mod agent;
mod attachments;
mod checkpoints;
mod context;
mod clipboard;
mod cli_ui;
mod config;
mod git;
mod interactive;
mod llm;
mod model_metadata;
mod providers;
mod grill_me;
mod reasoning;
mod logo;
mod custom_logo;
mod terminal_image;
mod execution_mode;
mod subagents;
pub mod logger;
mod markdown;
mod onboarding;
mod prompt;
mod session;
mod session_format;
mod snow;
mod theme;
mod i18n;
mod tools;
mod runtime;
mod lsp;
mod mcp;
mod web_search;
mod update;
mod moa_router;
pub mod skills;

use agent::{Agent, AgentEvent};
use anyhow::Context;
use config::Config;
use crossterm::{
    cursor,
    execute,
    queue,
    terminal::{Clear, ClearType},
};
use git::GitInfo;
use prompt::{LineEditor, PromptResult};
use session::Session;
use std::io::{stdout, Write};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tools::ToolOutputEvent;

type PendingTitle = Option<(String, tokio::task::JoinHandle<anyhow::Result<String>>)>;

fn save_active_transcript(session: &mut Session, workspace: &std::path::Path,
    history: &[cli_ui::HistoryItem], stream: &Option<String>) {
    let mut snapshot = history.to_vec();
    let mut partial = stream.clone();
    preserve_streamed_text(&mut snapshot, &mut partial);
    if session.history == snapshot { return; }
    session.history = snapshot;
    if let Err(error) = session.save(workspace) {
        logger::log_warn("Session", &format!("Active transcript could not be saved: {error}"));
    }
}

fn collect_chat_title(pending: &mut PendingTitle, session: &mut Session, workspace: &std::path::Path,
    history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>) -> bool {
    use futures_util::FutureExt;
    if !pending.as_ref().is_some_and(|(_, handle)| handle.is_finished()) { return false; }
    let (session_id, handle) = pending.take().unwrap();
    if session_id != session.id { return false; }
    match handle.now_or_never() {
        Some(Ok(Ok(title))) => {
            cli_ui::set_active_chat_title(&title);
            session.title = Some(title);
            let _ = session.save(workspace);
        }
        Some(Ok(Err(error))) => {
            history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(crate::i18n::tf!("Chat title unavailable: {error:#}", error = error)));
        }
        _ => return false,
    }
    true
}

fn clear_screen_and_banner(config: &Config) -> u16 {
    model_metadata::ensure(config);
    cli_ui::invalidate_content_frame();
    let git_info = GitInfo::get(&config.workspace_dir);
    let git_display = match &git_info.branch {
        Some(b) => {
            if git_info.is_dirty {
                crate::i18n::tf!("{} (dirty)", b)
            } else {
                crate::i18n::tf!("{} (clean)", b)
            }
        }
        None => crate::i18n::tr("(no git)").to_string(),
    };

    let mut out = stdout();
    let _ = queue!(out, cursor::Hide, Clear(ClearType::All), Clear(ClearType::Purge), cursor::MoveTo(0, 0));
    let row = cli_ui::print_banner_to(
        &mut out,
        &config.model,
        &config.base_url,
        &config.workspace_dir.display().to_string(),
        &git_display,
    );
    let _ = queue!(out, Clear(ClearType::FromCursorDown), cursor::Show);
    let _ = out.flush();
    let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    row.min(term_rows.saturating_sub(1))
}

fn set_execution_mode(config: &Config, prefs: &mut theme::UserPreferences,
    mode: execution_mode::ExecutionMode) -> std::io::Result<()> {
    let mut updated = prefs.clone();
    updated.execution_mode = Some(mode);
    updated.auto_approve = Some(mode == execution_mode::ExecutionMode::AllowAll);
    updated.save()?;
    *prefs = updated;
    config.execution_mode.set(mode);
    execution_mode::set_display(mode);
    Ok(())
}

fn redraw_full_screen(config: &Config, history: &[cli_ui::HistoryItem]) -> u16 {
    model_metadata::ensure(config);
    let current_git = GitInfo::get_cached(&config.workspace_dir);
    let git_display = match &current_git.branch {
        Some(b) => {
            if current_git.is_dirty {
                crate::i18n::tf!("{} (dirty)", b)
            } else {
                crate::i18n::tf!("{} (clean)", b)
            }
        }
        None => crate::i18n::tr("(no git)").to_string(),
    };
    let branch_tag = match &current_git.branch {
        Some(b) => {
            if current_git.is_dirty {
                format!("{}*", b)
            } else {
                b.clone()
            }
        }
        None => "".to_string(),
    };

    cli_ui::redraw_all(
        &config.model,
        &config.base_url,
        &config.workspace_dir.display().to_string(),
        &git_display,
        history,
        &branch_tag,
        "",
    )
}

fn preserve_streamed_text(history: &mut Vec<cli_ui::HistoryItem>, stream: &mut Option<String>) {
    if let Some(text) = stream.take().filter(|text| !text.trim().is_empty()) {
        let previous = history.iter().rev().find(|item| !matches!(item, cli_ui::HistoryItem::Thought(_)));
        if !matches!(previous, Some(cli_ui::HistoryItem::AssistantMessage(previous)) if previous == &text) {
            history.push(cli_ui::HistoryItem::AssistantMessage(text));
        }
    }
}

async fn redraw_active_view(
    config: &Config,
    history: &[cli_ui::HistoryItem],
    _branch_tag: &str,
    output_row: &Arc<StdMutex<u16>>,
    _spinner: &mut Option<cli_ui::Spinner>,
    stream_writer: &mut Option<String>,
) {
    // Streaming and browsing share the same Markdown layout and content viewport.
    // Never replay the response through terminal scrolling commands.
    let _update = cli_ui::begin_content_update();
    let mut snapshot = history.to_vec();
    if let Some(text) = stream_writer.as_ref() {
        if !text.is_empty() { snapshot.push(cli_ui::HistoryItem::AssistantMessage(text.clone())); }
    }
    *output_row.lock().unwrap() = redraw_full_screen(config, &snapshot);
}

async fn redraw_active_editor(
    config: &Config, history: &[cli_ui::HistoryItem], branch_tag: &str,
    output_row: &Arc<StdMutex<u16>>, spinner: &mut Option<cli_ui::Spinner>,
    stream_writer: &mut Option<String>,
) {
    // Paint content and editor in one frame while the activity indicator stays active.
    let _update = cli_ui::begin_content_update();
    redraw_active_view(config, history, branch_tag, output_row, spinner, stream_writer).await;
    let (x, y) = cli_ui::render_bottom_box("", branch_tag);
    cli_ui::position_input_cursor(x, y);
}

struct SigListener {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for SigListener {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
    }
}

impl SigListener {
    async fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

enum TerminalEvent {
    Resize,
    ToggleOutput,
    Scroll { delta: i32, row: u16 },
    Key(crossterm::event::KeyEvent),
    Mouse(crossterm::event::MouseEvent),
    Paste(String),
}

fn spawn_sig_listener(terminal_tx: mpsc::UnboundedSender<TerminalEvent>) -> SigListener {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener_stop = stop.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let mut pending_event = None;
        // Finish this reader before handing terminal input to a modal UI.
        // Aborting an async wrapper does not stop a blocking event::read.
        while !listener_stop.load(std::sync::atomic::Ordering::Acquire) {
            let has_event = pending_event.is_some() || crossterm::event::poll(std::time::Duration::from_millis(50))
                .unwrap_or(false);
            if listener_stop.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            if has_event {
                if let Ok(ev) = pending_event.take().map(Ok).unwrap_or_else(crossterm::event::read) {
                    match ev {
                        crossterm::event::Event::Key(k) => {
                            if k.kind == crossterm::event::KeyEventKind::Press {
                                if k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                    && k.code == crossterm::event::KeyCode::Char('o')
                                {
                                    let _ = terminal_tx.send(TerminalEvent::ToggleOutput);
                                } else if k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                    && k.code == crossterm::event::KeyCode::Char('v') {
                                    match clipboard::paste() {
                                        Ok(text) => { let _ = terminal_tx.send(TerminalEvent::Paste(text)); }
                                        Err(error) => logger::log_warn("Clipboard", &error),
                                    }
                                } else {
                                    let _ = terminal_tx.send(TerminalEvent::Key(k));
                                }
                            }
                        }
                        crossterm::event::Event::Mouse(mouse) => {
                            if let Some(delta) = cli_ui::history_scroll_mouse(mouse) {
                                let delta = cli_ui::coalesce_scroll(delta, &mut pending_event);
                                let _ = terminal_tx.send(TerminalEvent::Scroll { delta, row: mouse.row });
                            } else {
                                let _ = terminal_tx.send(TerminalEvent::Mouse(mouse));
                            }
                        }
                        crossterm::event::Event::Paste(text) => {
                            let _ = terminal_tx.send(TerminalEvent::Paste(text));
                        }
                        crossterm::event::Event::Resize(_, _) => {
                            let _ = terminal_tx.send(TerminalEvent::Resize);
                        }
                        _ => {}
                    }
                }
            }
        }
    });
    SigListener { stop, handle: Some(handle) }
}


#[derive(Default)]
struct PromptQueue {
    next_id: usize,
    pending: std::collections::VecDeque<(usize, String)>,
}

impl PromptQueue {
    fn marker(id: usize, text: &str) -> String { format!("⏳ Queued #{id}: {text}") }
    fn push(&mut self, text: String, history: &mut Vec<cli_ui::HistoryItem>) {
        self.next_id += 1;
        history.push(cli_ui::HistoryItem::ToolLog(Self::marker(self.next_id, &text)));
        self.pending.push_back((self.next_id, text));
    }
    fn pop(&mut self, history: &mut Vec<cli_ui::HistoryItem>) -> Option<String> {
        let (id, text) = self.pending.pop_front()?;
        let marker = Self::marker(id, &text);
        history.retain(|item| !matches!(item, cli_ui::HistoryItem::ToolLog(log) if log == &marker));
        Some(text)
    }
    fn clear(&mut self, history: &mut Vec<cli_ui::HistoryItem>) {
        while self.pop(history).is_some() {}
    }
}

#[derive(PartialEq)]
enum CommandResult { Handled, Exit, InsertSkill(skills::Skill), RestorePrompt(String) }

struct ActiveAgent {
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    events: mpsc::Receiver<AgentEvent>,
}

impl ActiveAgent {
    async fn stop(&mut self, history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>, usage: &mut Option<session::TokenUsage>, context: &mut Option<u64>) {
        self.cancel.cancel();
        if let Some(mut task) = self.task.take() {
            // Drain bounded output while cancellation releases the agent lock.
            loop {
                tokio::select! {
                    _ = &mut task => break,
                    event = self.events.recv() => match event {
                        Some(AgentEvent::AssistantMessage(message)) => history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(message)),
                        Some(AgentEvent::TokenUsage(tokens)) => session::record_tokens(usage, tokens),
                        Some(AgentEvent::ContextUsage(tokens)) => { *context = tokens; cli_ui::set_context_tokens(tokens); },
                        Some(AgentEvent::Interrupted) => session::record_tokens(usage, None),
                        Some(_) => {}, // Dropping modal responders unblocks tool requests.
                        None => { let _ = task.await; break; }
                    }
                }
            }
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                AgentEvent::AssistantMessage(message) => history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(message)),
                AgentEvent::TokenUsage(tokens) => session::record_tokens(usage, tokens),
                AgentEvent::ContextUsage(tokens) => { *context = tokens; cli_ui::set_context_tokens(tokens); },
                AgentEvent::Interrupted => session::record_tokens(usage, None),
                _ => {}
            }
        }
    }
}

async fn command_agent<'a>(
    agent: &'a Arc<Mutex<Agent>>, active: &mut Option<ActiveAgent>,
    session: &mut Session, history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>, config: &Config,
) -> tokio::sync::MutexGuard<'a, Agent> {
    if let Some(running) = active.as_mut() {
        if running.task.is_some() {
            running.stop(history, &mut session.token_usage, &mut session.context_tokens).await;
            let locked = agent.lock().await;
            session.messages = locked.get_messages().to_vec();
            session.history = history.lock().unwrap().clone();
            let _ = session.save(&config.workspace_dir);
            return locked;
        }
    }
    agent.lock().await
}

fn fresh_connection_chat(config: &Config, session: &mut Session, history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>) {
    *session = Session::new(config.model.clone());
    history.lock().unwrap().clear();
    cli_ui::set_active_chat_title("");
    cli_ui::set_context_tokens(None);
}

fn collect_context_updates(config: &Config, history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>) -> bool {
    let changed = model_metadata::take_changed();
    if let Some(warning) = model_metadata::take_warning(config) {
        history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(warning));
        return true;
    }
    changed
}

async fn handle_tui_command(
    line: &str, config: &mut Config, user_prefs: &mut theme::UserPreferences,
    agent: &Arc<Mutex<Agent>>, current_session: &mut Session,
    history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>, output_row: &Arc<StdMutex<u16>>,
    branch_tag: &str, active: &mut Option<ActiveAgent>, tools: &Arc<tools::ToolExecutor>,
) -> anyhow::Result<CommandResult> {
    let mut command_output = Vec::new();
    let result = Box::pin(handle_tui_command_inner(line, config, user_prefs, agent, current_session,
        history, output_row, branch_tag, active, tools, &mut command_output)).await;
    if !command_output.is_empty() {
        history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(format!("{}\n{}", line, command_output.join("\n").trim())));
        *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
    }
    result
}

async fn handle_tui_command_inner(
    line: &str, config: &mut Config, user_prefs: &mut theme::UserPreferences,
    agent: &Arc<Mutex<Agent>>, current_session: &mut Session,
    history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>, output_row: &Arc<StdMutex<u16>>,
    branch_tag: &str, active: &mut Option<ActiveAgent>, tools: &Arc<tools::ToolExecutor>, command_output: &mut Vec<String>,
) -> anyhow::Result<CommandResult> {
    let runtime = &tools.runtime;
    // Buffer textual responses so every command supports the shared Ctrl+O preview.
    macro_rules! command_message {
        () => { command_output.push(String::new()) };
        ($($arg:tt)*) => { command_output.push(format!($($arg)*)) };
    }
    cli_ui::invalidate_content_frame();
    if line == "/agents" || line.starts_with("/agents ") {
        if let Some(manager) = tools.subagents() {
            let args = line.strip_prefix("/agents").unwrap().trim();
            if let Some(id) = args.strip_prefix("stop ") {
                if id.trim() == "all" { manager.cancel_all(); command_message!("{}", crate::i18n::tr("Subagent cancellation requested.")); }
                else { match manager.cancel(id.trim()) {
                    Ok(()) => command_message!("{}", crate::i18n::tr("Subagent cancellation requested.")),
                    Err(error) => command_message!("{error}"),
                } }
            } else if args.is_empty() {
                let views = manager.views();
                if views.is_empty() { command_message!("{}", crate::i18n::tr("No subagents in this chat yet.")); }
                for view in views {
                    command_message!("{}", crate::i18n::tf!("{} | {} | {} | {} | {} tokens{}", view.id, crate::i18n::tr(&view.profile),
                        crate::i18n::tr(&view.status), view.model.chars().take(24).collect::<String>(), view.tokens,
                        if view.usage_complete { "" } else { crate::i18n::tr(" (partial)") }));
                }
            } else { match manager.view(args) {
                Ok(view) => command_message!("{} | {} | {}\n{}\n{}\n{}", view.id, crate::i18n::tr(&view.profile), crate::i18n::tr(&view.status),
                    view.task, view.report.as_deref().unwrap_or_else(|| crate::i18n::tr(&view.activity)), view.error.as_deref().unwrap_or("")),
                Err(error) => command_message!("{error}\n/agents | /agents <id> | /agents stop <id|all>"),
            } }
        }
        return Ok(CommandResult::Handled);
    }
    if line == "/config" {
        let mut selection = 0;
        loop {
            let ctx = interactive::ScreenContext::from_config(config);
            let Some(action) = interactive::configure_interactive(&ctx, config, user_prefs, selection)? else { break; };
            selection = action as usize;
            if matches!(action, interactive::ConfigAction::Language) {
                if let Some(language) = interactive::select_language_interactive(Some(&ctx), user_prefs.language)? {
                    user_prefs.language = language;
                    user_prefs.save()?;
                    i18n::set_current(language);
                }
            } else if matches!(action, interactive::ConfigAction::Approval) {
                set_execution_mode(config, user_prefs, config.execution_mode.current().next())?;
            } else {
                let command = match action {
                    interactive::ConfigAction::Theme => "/theme",
                    interactive::ConfigAction::Provider => "/provider",
                    interactive::ConfigAction::Model => "/model",
                    interactive::ConfigAction::Mode => "/mode",
                    interactive::ConfigAction::Effort => "/effort",
                    interactive::ConfigAction::Approval => "/approval",
                    interactive::ConfigAction::Language => unreachable!(),
                };
                Box::pin(handle_tui_command(command, config, user_prefs, agent, current_session,
                    history, output_row, branch_tag, active, tools)).await?;
            }
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
        }
        *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
        return Ok(CommandResult::Handled);
    }
        // Handle slash commands
        if line == "/exit" || line == "/quit" {
            return Ok(CommandResult::Exit);
        }

        if matches!(line.split_whitespace().next(), Some("/rewind" | "/restore")) {
            // Freeze tool execution before inspecting or replacing files.
            let mut locked = command_agent(agent, active, current_session, history, config).await;
            current_session.messages = locked.get_messages().to_vec();
            current_session.history = history.lock().unwrap().clone();
            let result = (|| -> anyhow::Result<Option<(Session, Option<String>)>> {
                let points = checkpoints::list(&config.workspace_dir, &current_session.id)?;
                let ctx = interactive::ScreenContext::from_config(config);
                let choices = points.iter().map(|point| format!("{}  {}", point.created_at, point.label)).collect::<Vec<_>>();
                let Some(selected) = interactive::rewind_menu(&ctx, crate::i18n::tr("Restore workspace checkpoint"), &[], &choices)? else { return Ok(None); };
                let point = &points[selected];
                let changes = checkpoints::preview(&config.workspace_dir, point)?;
                let conversation = checkpoints::conversation_at(point, current_session)?;
                let prompt = checkpoints::prompt_at(point, current_session);
                let mut changes = changes;
                changes.push(crate::i18n::tr("Restore chat to this checkpoint; later prompts, answers and tool results will be removed").into());
                let options = vec![crate::i18n::tr("Cancel").to_string(), crate::i18n::tr("Restore files and chat").to_string()];
                if interactive::rewind_menu(&ctx, crate::i18n::tr("Review restore · a safety checkpoint will be saved"), &changes, &options)? != Some(1) { return Ok(None); }
                checkpoints::restore_session(&config.workspace_dir, point, current_session)?;
                Ok(Some((conversation, prompt)))
            })();
            let mut outcome = CommandResult::Handled;
            match result {
                Ok(Some((conversation, prompt))) => {
                    current_session.history = conversation.build_history_from_messages();
                    current_session.messages = conversation.messages;
                    locked.runtime().reset_documents().await;
                    locked.set_messages(current_session.messages.clone());
                    *history.lock().unwrap() = current_session.history.clone();
                    cli_ui::scroll_history(i32::MIN);
                    current_session.save(&config.workspace_dir)?;
                    if let Some(prompt) = prompt { outcome = CommandResult::RestorePrompt(prompt); }
                }
                Ok(None) => {}
                Err(error) => history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(crate::i18n::tf!("Restore unavailable: {error:#}", error = error))),
            }
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            return Ok(outcome);
        }

        if line == "/help" {
            clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            command_message!("{}", cli_ui::help_text());
            return Ok(CommandResult::Handled);
        }

        if line == "/compact" {
            let mut locked = command_agent(agent, active, current_session, history, config).await;
            let cancel = CancellationToken::new();
            let (tx, mut rx) = mpsc::channel(100);
            let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
            crossterm::terminal::enable_raw_mode().ok();
            let mut reader = spawn_sig_listener(terminal_tx);
            let spinner = cli_ui::Spinner::start_with_box(crate::i18n::tr("Compacting context..."), "", branch_tag, output_row.clone());
            let result = {
                let task = locked.compact_context(&tx, &cancel);
                tokio::pin!(task);
                loop {
                    tokio::select! {
                        result = &mut task => break result,
                        Some(event) = rx.recv() => match event {
                            AgentEvent::CompactionProgress { percent, part, parts } => cli_ui::set_compaction_progress(percent, part, parts),
                            AgentEvent::TokenUsage(tokens) => session::record_tokens(&mut current_session.token_usage, tokens),
                            AgentEvent::ContextUsage(tokens) => {
                                current_session.context_tokens = tokens;
                                cli_ui::set_context_tokens(tokens);
                            },
                            AgentEvent::ToolLog(text) => history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(text)),
                            _ => {}
                        },
                        Some(event) = terminal_rx.recv() => match event {
                            TerminalEvent::Key(key) if key.code == crossterm::event::KeyCode::Esc
                                || key.code == crossterm::event::KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => cancel.cancel(),
                            TerminalEvent::Resize => { *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap()); },
                            _ => {}
                        }
                    }
                }
            };
            while let Ok(event) = rx.try_recv() {
                match event {
                    AgentEvent::TokenUsage(tokens) => session::record_tokens(&mut current_session.token_usage, tokens),
                    AgentEvent::ContextUsage(tokens) => {
                        current_session.context_tokens = tokens;
                        cli_ui::set_context_tokens(tokens);
                    },
                    AgentEvent::ToolLog(text) => history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(text)),
                    _ => {}
                }
            }
            reader.stop().await;
            spinner.stop().await;
            cli_ui::disable_raw_mode().ok();
            if let Err(error) = result {
                let text = if cancel.is_cancelled() { crate::i18n::tr("Context compaction cancelled.").into() }
                    else { crate::i18n::tf!("Context compaction failed: {error}", error = error) };
                history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(text));
            }
            current_session.messages = locked.get_messages().to_vec();
            current_session.history = history.lock().unwrap().clone();
            current_session.save(&config.workspace_dir)?;
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            return Ok(CommandResult::Handled);
        }

        if line == "/approval" || line == "/permissions" || line.starts_with("/permissions ") {
            let mode = if let Some(value) = line.strip_prefix("/permissions ") {
                execution_mode::ExecutionMode::parse(value.trim())
            } else { Some(if config.execution_mode.current() == execution_mode::ExecutionMode::AllowAll {
                execution_mode::ExecutionMode::Auto
            } else { execution_mode::ExecutionMode::AllowAll }) };
            if let Some(mode) = mode {
                set_execution_mode(config, user_prefs, mode)?;
                *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
                command_message!("{}: {}", crate::i18n::tr("Execution mode"), mode.name());
            } else { command_message!("{}", crate::i18n::tr("Usage: /permissions auto|plan|allow-all")); }
            return Ok(CommandResult::Handled);
        }

        if line == "/logo" || line.starts_with("/logo ") {
            let value = line.strip_prefix("/logo").unwrap().trim();
            if value.is_empty() {
                command_message!("{}", crate::i18n::tr("Usage: /logo <image path> | /logo reset"));
            } else {
                let result = if value == "reset" {
                    Ok(None)
                } else {
                    custom_logo::CustomLogo::from_path(&custom_logo::image_path(value)).map(Some)
                };
                match result {
                    Ok(logo) => {
                        let mut updated = user_prefs.clone();
                        updated.custom_logo = logo;
                        updated.save()?;
                        *user_prefs = updated;
                        custom_logo::set_current(user_prefs.custom_logo.as_ref());
                        *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
                    }
                    Err(error) => {
                        command_message!("{} {}", crate::i18n::tr("Could not load logo:"), error);
                    }
                }
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/theme") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 1 {
                if let Some(th) = theme::Theme::from_str_loose(parts[1]) {
                    user_prefs.theme = th;
                    let _ = user_prefs.save();
                    theme::set_current(th);
                    *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
                } else {
                    clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    command_message!("{}", crate::i18n::tf!("Unknown theme: '{}'. Available themes: amber, cyberpunk, emerald, nord, monochrome, dracula, rose, ocean, sakura, solarized\n", parts[1]));
                }
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                if let Ok(Some(new_th)) = interactive::select_theme_interactive(&ctx, user_prefs.theme) {
                    user_prefs.theme = new_th;
                    let _ = user_prefs.save();
                    theme::set_current(new_th);
                }
                *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            }
            return Ok(CommandResult::Handled);
        }

        if line == "/mode" || line.starts_with("/mode ") {
            *output_row.lock().unwrap() = clear_screen_and_banner(config);
            command_message!("{}", crate::i18n::tr("A third-party provider is active. Takiza mode selection is unavailable."));
            return Ok(CommandResult::Handled);
        }

        if line == "/usage" || line == "/quota" {
            let ctx = interactive::ScreenContext::from_config(&config);
            interactive::show_interactive_usage(&ctx, &config)?;
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            return Ok(CommandResult::Handled);
        }

        if line == "/skills" {
            let ctx = interactive::ScreenContext::from_config(config);
            let selected = interactive::show_interactive_skills(&ctx, &config.workspace_dir)?;
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            return Ok(selected.map(CommandResult::InsertSkill).unwrap_or(CommandResult::Handled));
        }

        if matches!(line.split_whitespace().next(), Some("/mcp" | "/lsp")) {
            let locked = command_agent(agent, active, current_session, history, config).await;
            let runtime = locked.runtime();
            drop(locked);
            let mut words = line.split_whitespace();
            let kind = words.next().unwrap().trim_start_matches('/');
            let action = words.next().unwrap_or("status");
            let server = words.next();
            let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
            crossterm::terminal::enable_raw_mode().ok();
            let mut reader = spawn_sig_listener(terminal_tx);
            let spinner = cli_ui::Spinner::start_with_box(crate::i18n::tr("Managing integration..."), "", branch_tag, output_row.clone());
            let result = {
                let command = tokio::time::timeout(std::time::Duration::from_secs(35), runtime.command(kind, action, server));
                tokio::pin!(command);
                loop {
                    tokio::select! {
                        result = &mut command => break match result {
                            Ok(result) => result.unwrap_or_else(|error| error),
                            Err(_) => crate::i18n::tr("Integration management timed out.").into(),
                        },
                        Some(event) = terminal_rx.recv() => match event {
                            TerminalEvent::Key(key) if key.code == crossterm::event::KeyCode::Esc
                                || key.code == crossterm::event::KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                                    break crate::i18n::tr("Integration management cancelled.").into();
                                },
                            TerminalEvent::Resize => { *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap()); },
                            _ => {},
                        },
                    }
                }
            };
            reader.stop().await;
            spinner.stop().await;
            cli_ui::disable_raw_mode().ok();
            history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(result));
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            return Ok(CommandResult::Handled);
        }

        if line == "/tools" {
            let ctx = interactive::ScreenContext::from_config(&config);
            interactive::show_interactive_tools(&ctx, &runtime.snapshot().definitions())?;
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            return Ok(CommandResult::Handled);
        }

        if line == "/diff" {
            if let Some(diff) = GitInfo::diff(&config.workspace_dir) {
                if diff.trim().is_empty() {
                    clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    command_message!("{}", crate::i18n::tf!("No changes (working tree clean).\n"));
                } else {
                    let ctx = interactive::ScreenContext::from_config(&config);
                    interactive::show_interactive_diff(&ctx, &diff, &config.workspace_dir)?;
                    *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
                }
            } else {
                clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                command_message!("{}", crate::i18n::tf!("No git repository found in workspace.\n"));
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/model") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 1 {
                config.model = parts[1].to_string();
                config.mode = theme::AppMode::Manual;
                user_prefs.mode = Some("manual".to_string());
                user_prefs.remember_model(config.model.clone());
                user_prefs.save()?;
                let mut locked = command_agent(agent, active, current_session, history, config).await;
                locked.reset(config.clone());
                fresh_connection_chat(config, current_session, history);
                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                command_message!("{}", crate::i18n::tf!("Switched model to: {}\n", config.model));
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                if let Some(chosen_model) = interactive::select_model(&ctx, config, user_prefs)? {
                    config.model = chosen_model.clone();
                    config.mode = theme::AppMode::Manual;
                    user_prefs.mode = Some("manual".to_string());
                    user_prefs.remember_model(chosen_model);
                    user_prefs.save()?;
                    let mut locked = command_agent(agent, active, current_session, history, config).await;
                    locked.reset(config.clone());
                    fresh_connection_chat(config, current_session, history);
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    command_message!("{}", crate::i18n::tf!("Switched model to: {}\n", config.model));
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/effort") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let levels = config.reasoning_levels();
            if levels.is_empty() {
                command_message!("{}", crate::i18n::tr("Model does not support adjustable effort"));
                return Ok(CommandResult::Handled);
            }
            let chosen_effort = if parts.len() > 1 {
                let eff = parts[1].to_lowercase();
                match eff.as_str() {
                    _ if eff == "auto" || levels.contains(&eff) => Some(eff),
                    _ => {
                        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                        cli_ui::print_user_cmd(&line);
                        command_message!("{}", crate::i18n::tf!("Unknown effort: {}. Available: {}\n", parts[1], format!("{}, auto", levels.join(", "))));
                        None
                    }
                }
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                let cur = config.effort.clone().unwrap_or_else(|| "auto".to_string());
                match interactive::select_effort_interactive(&ctx, &cur, &levels) {
                    Ok(Some(eff)) => Some(eff),
                    _ => {
                        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                        None
                    }
                }
            };

            if let Some(eff) = chosen_effort {
                config.effort = (eff != "auto").then(|| eff.clone());
                user_prefs.effort = Some(config.effort.clone().unwrap_or_default());
                let _ = user_prefs.save();
                let mut locked = command_agent(agent, active, current_session, history, config).await;
                locked.reset(config.clone());
                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                command_message!("{}", crate::i18n::tf!("Set reasoning effort to: {}\n", eff));
            }
            return Ok(CommandResult::Handled);
        }

        if line == "/provider" || line.starts_with("/provider ") {
            let ctx = interactive::ScreenContext::from_config(config);
            let requested = line.strip_prefix("/provider").unwrap().trim();
            let selected = if requested.is_empty() {
                interactive::manage_providers(&ctx, user_prefs, config)?
            } else {
                user_prefs.providers.iter().find(|(id,p)| id.as_str()==requested || p.name.eq_ignore_ascii_case(requested))
                    .map(|(id,_)|id.clone())
            };
            if let Some(id) = selected {
                user_prefs.activate_provider(&id, config)?;
                config.mode = theme::AppMode::Manual;
                user_prefs.mode = Some("manual".into());
                user_prefs.save()?;
                let mut locked = command_agent(agent, active, current_session, history, config).await;
                locked.reset(config.clone());
                fresh_connection_chat(config, current_session, history);
                *output_row.lock().unwrap() = clear_screen_and_banner(config);
                cli_ui::print_user_cmd(&line);
                command_message!("{}", crate::i18n::tf!("Switched provider: {}\n  Endpoint: {}\n  Model:    {}\n", user_prefs.providers[&id].name, config.base_url, config.model));
            } else {
                *output_row.lock().unwrap() = clear_screen_and_banner(config);
                if !requested.is_empty() { command_message!("{}", crate::i18n::tr("Saved provider not found. Open /provider to add a connection.")); }
            }
            return Ok(CommandResult::Handled);
        }

        if line == "/status" {
            let msg_count = agent.try_lock().map(|a| a.message_count()).unwrap_or_else(|_| current_session.message_count());
            let ctx = interactive::ScreenContext::from_config(&config);
            if let Ok(Some(action)) = interactive::show_interactive_status(&ctx, &config, &current_session, msg_count, &branch_tag) {
                match action {
                    interactive::StatusAction::ChangeProvider => {
                        let cur_ctx = interactive::ScreenContext::from_config(config);
                        if let Some(id) = interactive::manage_providers(&cur_ctx, user_prefs, config)? {
                            user_prefs.activate_provider(&id, config)?;
                            config.mode = theme::AppMode::Manual;
                            user_prefs.mode = Some("manual".into());
                            user_prefs.save()?;
                            let mut locked = command_agent(agent, active, current_session, history, config).await;
                            locked.reset(config.clone());
                            fresh_connection_chat(config, current_session, history);
                        }
                    }
                    interactive::StatusAction::ChangeModel => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        if let Some(chosen_model) = interactive::select_model(&cur_ctx, config, user_prefs)? {
                            config.model = chosen_model.clone();
                            user_prefs.remember_model(chosen_model);
                            let _ = user_prefs.save();
                            let mut locked = command_agent(agent, active, current_session, history, config).await;
                            locked.reset(config.clone());
                            fresh_connection_chat(config, current_session, history);
                        }
                    }
                    interactive::StatusAction::ChangeMode => {
                        let ctx = interactive::ScreenContext::from_config(config);
                        interactive::show_external_mode_notice(&ctx)?;
                    }
                    interactive::StatusAction::ShowUsage => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        let _ = interactive::show_interactive_usage(&cur_ctx, &config);
                    }
                    interactive::StatusAction::ChangeTheme => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        if let Ok(Some(new_th)) = interactive::select_theme_interactive(&cur_ctx, user_prefs.theme) {
                            user_prefs.theme = new_th;
                            let _ = user_prefs.save();
                            theme::set_current(new_th);
                        }
                    }
                    interactive::StatusAction::Reset => {
                        if !current_session.messages.is_empty() {
                            current_session.history = history.lock().unwrap().clone();
                            let _ = current_session.save(&config.workspace_dir);
                        }
                        let mut locked = command_agent(agent, active, current_session, history, config).await;
                        locked.reset(config.clone());
                        fresh_connection_chat(config, current_session, history);
                    }
                }
            }
            let hist = history.lock().unwrap().clone();
            *output_row.lock().unwrap() = redraw_full_screen(&config, &hist);
            return Ok(CommandResult::Handled);
        }

        if line == "/reset" || line == "/new" || line == "/clear" {
            if !current_session.messages.is_empty() {
                current_session.history = history.lock().unwrap().clone();
                let _ = current_session.save(&config.workspace_dir);
            }
            let mut locked = command_agent(agent, active, current_session, history, config).await;
            locked.reset(config.clone());
            fresh_connection_chat(config, current_session, history);
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            command_message!("{}", crate::i18n::tf!("Conversation reset. Started a new session.\n"));
            return Ok(CommandResult::Handled);
        }

        if line == "/history" {
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            let sess_dir = Session::sessions_dir(&config.workspace_dir);
            let md_path = sess_dir.join(format!("{}.md", current_session.id));
            command_message!("{}", crate::i18n::tf!("Current Session: {}", current_session.id));
            command_message!("{}", crate::i18n::tf!("Created:         {}", current_session.created_at));
            command_message!("{}", crate::i18n::tf!("Model:           {}", current_session.model));
            command_message!("{}", crate::i18n::tf!("Messages:        {}", current_session.message_count()));
            if md_path.exists() {
                command_message!("{}", crate::i18n::tf!("Markdown Log:    {}", md_path.display()));
            }
            let session_ids = Session::list(&config.workspace_dir);
            command_message!("{}", crate::i18n::tf!("\nSaved Sessions ({}):", session_ids.len()));
            for id in session_ids.iter().take(8) {
                let is_cur = if id == &current_session.id { crate::i18n::tr(" (current)") } else { "" };
                if let Some(s) = Session::load(&config.workspace_dir, id) {
                    command_message!("{}", crate::i18n::tf!("  • {} | {:2} msgs | \"{}\"{}", s.id, s.message_count(), s.title(), is_cur));
                } else {
                    command_message!("  • {}{}", id, is_cur);
                }
            }
            if session_ids.len() > 8 {
                command_message!("{}", crate::i18n::tf!("  ... and {} more (use /sessions to browse)", session_ids.len() - 8));
            }
            command_message!("{}", crate::i18n::tf!("\nTip: Use /sessions to interactively resume or delete past sessions.\n"));
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/resume") || line == "/sessions" {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if line.starts_with("/resume") && parts.len() > 1 {
                let id = parts[1];
                if let Some(loaded) = Session::load(&config.workspace_dir, id) {
                    if !current_session.messages.is_empty() {
                        current_session.history = history.lock().unwrap().clone();
                        let _ = current_session.save(&config.workspace_dir);
                    }
                    let mut locked = command_agent(agent, active, current_session, history, config).await;
                    locked.set_messages(loaded.messages.clone());
                    let restored_history = loaded.build_history_from_messages();
                    *current_session = loaded;
                    let _ = current_session.save(&config.workspace_dir);
                    cli_ui::set_active_chat_title(current_session.title.as_deref().unwrap_or(""));
                    *history.lock().unwrap() = restored_history.clone();

                    *output_row.lock().unwrap() = redraw_full_screen(&config, &restored_history);
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    command_message!("{}", crate::i18n::tf!("Session {} not found.\n", id));
                }
            } else {
                let sessions = Session::list(&config.workspace_dir);
                if sessions.is_empty() {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    command_message!("{}", crate::i18n::tf!("No saved chat sessions found in .takiza/sessions/\n"));
                } else {
                    let ctx = interactive::ScreenContext::from_config(&config);
                    match interactive::select_session(&ctx, &config.workspace_dir) {
                        Ok(Some(interactive::SessionAction::Resume(resumed))) => {
                            if !current_session.messages.is_empty() {
                                current_session.history = history.lock().unwrap().clone();
                                let _ = current_session.save(&config.workspace_dir);
                            }
                            let mut locked = command_agent(agent, active, current_session, history, config).await;
                            locked.set_messages(resumed.messages.clone());
                            let restored_history = resumed.build_history_from_messages();
                            *current_session = resumed;
                            let _ = current_session.save(&config.workspace_dir);
                            cli_ui::set_active_chat_title(current_session.title.as_deref().unwrap_or(""));
                            *history.lock().unwrap() = restored_history.clone();

                            *output_row.lock().unwrap() = redraw_full_screen(&config, &restored_history);
                        }
                        Ok(Some(interactive::SessionAction::Deleted(id))) => {
                            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                            cli_ui::print_user_cmd(&line);
                            command_message!("{}", crate::i18n::tf!("Deleted session {}.\n", id));
                        }
                        _ => {
                            let hist = history.lock().unwrap().clone();
                            *output_row.lock().unwrap() = redraw_full_screen(&config, &hist);
                        }
                    }
                }
            }
            return Ok(CommandResult::Handled);
        }


    cli_ui::print_user_cmd(line);
    command_message!("{}", crate::i18n::tf!("Unknown command: {line}. Use /help to see available commands.", line = line));
    Ok(CommandResult::Handled)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli_args = config::CliArgs::parse();
    let automatic = if !cli_args.update && !cli_args.show_help && !cli_args.show_version {
        update::Automatic::start()
    } else { None };
    let result = run(cli_args).await;
    if let Some(automatic) = automatic { automatic.finish(result.is_ok()).await; }
    result
}

async fn run(cli_args: config::CliArgs) -> anyhow::Result<()> {

    i18n::set_current(theme::UserPreferences::load().language);
    if cli_args.show_help {
        println!("{}", crate::i18n::tf!("Takiza Code v{} - Autonomous AI Software Engineering Agent\n", env!("TAKIZA_VERSION")));
        println!("{}", crate::i18n::tf!("USAGE:"));
        println!("    takiza [OPTIONS] [PROMPT]\n");
        println!("    takiza update");
        println!("{}\n", crate::i18n::tr("            Check for updates and install using the original installation channel"));
        println!("{}", crate::i18n::tf!("OPTIONS:"));
        println!("{}", crate::i18n::tf!("    -c, --continue, --resume"));
        println!("{}", crate::i18n::tf!("            Continue previous conversation session from where you left off\n"));
        println!("    -y, --yes, -a, --auto-approve, --dangerously-skip-permissions");
        println!("{}", crate::i18n::tf!("            Skip permission prompts for commands, external file writes and MCP calls"));
        println!("{}", crate::i18n::tf!("            (Default: confirm commands, external file writes and untrusted MCP calls)\n"));
        println!("    -h, --help");
        println!("{}", crate::i18n::tf!("            Print help information\n"));
        println!("    -v, -V, --version");
        println!("{}", crate::i18n::tf!("            Print version information\n"));
        println!("    --session <id>");
        println!("{}", crate::i18n::tr("            Resume a specific session from this directory"));
        println!("{}", crate::i18n::tf!("ARGS:"));
        println!("    [PROMPT]");
        println!("{}", crate::i18n::tf!("            Optional initial prompt to send to the agent\n"));
        return Ok(());
    }

    if cli_args.show_version {
        println!("takiza {}", env!("TAKIZA_VERSION"));
        return Ok(());
    }

    if cli_args.update { return update::manual().await; }

    if let Some(id) = &cli_args.session_id {
        anyhow::ensure!(!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'_'),
            "--session requires a valid session ID");
    }

    let run_started = std::time::Instant::now();
    let _terminal_screen = cli_ui::TerminalScreen::enter()?;

    // 1. Run onboarding wizard if user has not yet accepted terms
    let (mut user_prefs, onboarding_shown) = match onboarding::Onboarding::run_if_needed() {
        Ok(result) => result,
        Err(error) if error.is::<onboarding::OnboardingDeclined>() => {
            drop(_terminal_screen);
            println!("{}", crate::i18n::tf!("You must accept the user agreement to use Takiza Harness. Goodbye!"));
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    theme::set_current(user_prefs.theme);
    custom_logo::set_current(user_prefs.custom_logo.as_ref());

    // If onboarding was not shown, purge previous terminal output (e.g. `cargo run`).
    // If onboarding was shown, it already purged previous output on startup.
    if !onboarding_shown {
        let _ = execute!(stdout(), Clear(ClearType::All), Clear(ClearType::Purge), cursor::MoveTo(0, 0));
    }

    let mut config = Config::load_with_args(&cli_args);
    anyhow::ensure!(config.context_window.is_none_or(|limit|limit>llm::AGENT_MAX_OUTPUT_TOKENS),
        "{}", crate::i18n::tr("context_window must be an integer larger than the 8192-token response reserve, or null."));
    if user_prefs.providers.is_empty() {
        user_prefs.migrate_provider(&config);
        let id = user_prefs.active_provider.clone().unwrap();
        user_prefs.activate_provider(&id, &mut config)?;
        user_prefs.save()?;
    }
    anyhow::ensure!(user_prefs.active_provider.as_ref().is_some_and(|id|user_prefs.providers.contains_key(id)),
        "active_provider must identify a saved provider in config.json");
    user_prefs.providers[user_prefs.active_provider.as_ref().unwrap()].validate()?;
    execution_mode::set_display(config.execution_mode.current());

    if !user_prefs.is_workspace_trusted(&config.workspace_dir,
        config.execution_mode.current() == execution_mode::ExecutionMode::AllowAll) {
        let ctx = interactive::ScreenContext::from_config(&config);
        let allowed = interactive::workspace_trust_menu(&ctx, &config.workspace_dir)?;
        if allowed {
            user_prefs.trust_workspace(&config.workspace_dir)?;
        } else {
            drop(_terminal_screen);
            println!("{}", crate::i18n::tr("Directory access was declined. Goodbye!"));
            return Ok(());
        }
    }

    let mut initial_prompt = cli_args.prompt;
    let mut line_editor = LineEditor::new();
    let agent = Arc::new(Mutex::new(Agent::new(config.clone())));
    let tools_executor = agent.lock().await.tool_executor();

    // Spawn a fresh new chat session on startup or continue previous session
    let mut current_session = Session::new(config.model.clone());
    cli_ui::set_active_chat_title("");
    let history = Arc::new(StdMutex::new(Vec::new()));

    let output_row = Arc::new(StdMutex::new(0));

    if config.continue_session {
        let resumed = match cli_args.session_id.as_deref() {
            Some(id) => Session::load(&config.workspace_dir, id),
            None => Session::latest(&config.workspace_dir),
        };
        anyhow::ensure!(cli_args.session_id.is_none() || resumed.is_some(), "Requested session was not found in this directory");
        if let Some(loaded) = resumed {
            let mut locked = agent.lock().await;
            locked.set_messages(loaded.messages.clone());
            let restored_history = loaded.build_history_from_messages();
            current_session = loaded;
            let _ = current_session.save(&config.workspace_dir);
            cli_ui::set_active_chat_title(current_session.title.as_deref().unwrap_or(""));
            *history.lock().unwrap() = restored_history.clone();
            *output_row.lock().unwrap() = redraw_full_screen(&config, &restored_history);
        } else {
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            println!("{}", crate::i18n::tf!("  ℹ  No previous session found in .takiza/sessions. Started a new session.\n"));
        }
    } else {
        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
    }

    let mut prompt_queue = PromptQueue::default();
    let mut ready_commands = std::collections::VecDeque::new();
    let mut pending_title: PendingTitle = None;
    let mut title_attempted_sessions = std::collections::HashSet::new();
    'chat: loop {
        cli_ui::set_context_tokens(current_session.context_tokens);
        let current_git = GitInfo::get(&config.workspace_dir);
        let branch_tag = match &current_git.branch {
            Some(b) => {
                if current_git.is_dirty {
                    format!("{}*", b)
                } else {
                    b.clone()
                }
            }
            None => "".to_string(),
        };

        let command = ready_commands.pop_front();
        let queued = if command.is_none() { prompt_queue.pop(&mut history.lock().unwrap()) } else { None };
        cli_ui::set_pending_prompt_count(prompt_queue.pending.len());
        let line = if let Some(command) = command {
            command
        } else if let Some(p) = queued {
            *output_row.lock().unwrap() = redraw_full_screen(&config, &history.lock().unwrap());
            p
        } else if let Some(p) = initial_prompt.take() {
            p
        } else {
            let history_clone = history.clone();
            let config_clone = config.clone();
            let branch_tag_for_redraw = branch_tag.clone();
            let output_row_for_redraw = output_row.clone();

            let input_res = line_editor.read_line_with_updates(&branch_tag, move |current_buffer| {
                let git_info = GitInfo::get_cached(&config_clone.workspace_dir);
                let git_display = match &git_info.branch {
                    Some(b) => {
                        if git_info.is_dirty {
                            crate::i18n::tf!("{} (dirty)", b)
                        } else {
                            crate::i18n::tf!("{} (clean)", b)
                        }
                    }
                    None => crate::i18n::tr("(no git)").to_string(),
                };
                let hist = history_clone.lock().unwrap().clone();
                let new_row = cli_ui::redraw_all(
                    &config_clone.model,
                    &config_clone.base_url,
                    &config_clone.workspace_dir.display().to_string(),
                    &git_display,
                    &hist,
                    &branch_tag_for_redraw,
                    current_buffer,
                );
                *output_row_for_redraw.lock().unwrap() = new_row;
            }, || {
                let title_changed = collect_chat_title(&mut pending_title, &mut current_session, &config.workspace_dir, &history);
                title_changed | collect_context_updates(&config, &history)
            })?;

            match input_res {
                PromptResult::CycleMode => {
                    set_execution_mode(&config, &mut user_prefs, config.execution_mode.current().next())?;
                    continue 'chat;
                }
                PromptResult::Exit => {
                    break;
                }
                PromptResult::Interrupted => {
                    println!("^C");
                    continue;
                }
                PromptResult::Line(l) => l,
            }
        };

        if line.is_empty() {
            continue;
        }

        if line.starts_with('/') && !grill_me::is_command(&line) {
            let outcome = handle_tui_command(&line, &mut config, &mut user_prefs, &agent,
                &mut current_session, &history, &output_row, &branch_tag, &mut None, &tools_executor).await?;
            if pending_title.as_ref().is_some_and(|(id, _)| *id != current_session.id) {
                if let Some((_, task)) = pending_title.take() { task.abort(); }
            }
            match outcome {
                CommandResult::Exit => break,
                CommandResult::InsertSkill(skill) => line_editor.draft.insert_text(&skill.prompt_reference()),
                CommandResult::RestorePrompt(text) => {
                    line_editor.draft = prompt::Draft::default();
                    line_editor.draft.text = text;
                    line_editor.draft.cursor = line_editor.draft.text.chars().count();
                }
                CommandResult::Handled => {}
            }
            continue;
        }

        // Direct shell command (!<command>)
        if line.starts_with('!') {
            if config.execution_mode.current() == execution_mode::ExecutionMode::Plan {
                history.lock().unwrap().push(cli_ui::HistoryItem::Error(
                    crate::i18n::tr("Plan mode blocks shell commands. Switch modes with Shift+Tab.").into()));
                *output_row.lock().unwrap() = redraw_full_screen(&config, &history.lock().unwrap());
                continue;
            }
            let cmd = line[1..].trim().to_string();
            if cmd.is_empty() {
                continue;
            }

            history.lock().unwrap().push(cli_ui::HistoryItem::UserPrompt(line.clone()));
            {
                let mut row = output_row.lock().unwrap();
                *row = redraw_full_screen(&config, &history.lock().unwrap());
                cli_ui::print_tool_start_at("run_command", &cmd, &mut *row, "", &branch_tag);
            }
            history.lock().unwrap().push(cli_ui::HistoryItem::ToolStart {
                name: "run_command".to_string(),
                args: cmd.clone(),
            });

            let cancel_token = CancellationToken::new();
            let cancel_clone = cancel_token.clone();

            let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
            let mut sig_handle = spawn_sig_listener(terminal_tx);
            let cancel_handle = tokio::spawn(async move {
                while let Some(event) = terminal_rx.recv().await {
                    if let TerminalEvent::Key(key) = event {
                        if key.code == crossterm::event::KeyCode::Esc || key.code == crossterm::event::KeyCode::Char('c')
                            && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                            cancel_clone.cancel();
                        }
                    }
                }
            });

            let (sub_tx, mut sub_rx) = mpsc::channel::<ToolOutputEvent>(100);
            let output_row_clone = output_row.clone();
            let branch_tag_clone = branch_tag.clone();
            let history_clone = history.clone();

            let log_handle = tokio::spawn(async move {
                let mut log_count = 0usize;
                let mut hidden_count = 0usize;
                while let Some(evt) = sub_rx.recv().await {
                    if let ToolOutputEvent::Log(l) = evt {
                        if l.starts_with("$ ") { continue; }
                        log_count += 1;
                        history_clone.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(l.clone()));
                        if cli_ui::is_output_expanded() || log_count <= 3 {
                            let mut row = output_row_clone.lock().unwrap();
                            cli_ui::print_tool_log_at(&l, &mut *row, "", &branch_tag_clone);
                        } else {
                            hidden_count += 1;
                        }
                    }
                }
                if hidden_count > 0 {
                    let mut row = output_row_clone.lock().unwrap();
                    cli_ui::print_tool_collapsed_indicator_at(hidden_count, &mut *row, "", &branch_tag_clone);
                }
            });

            let res = tools_executor
                .execute(
                    "run_command",
                    &format!(r#"{{"command": {:?}}}"#, cmd),
                    Some(sub_tx),
                    Some(cancel_token.clone()),
                )
                .await;

            sig_handle.stop().await;
            cancel_handle.abort();
            let _ = log_handle.await;

            let (is_err, out_text) = match res {
                Ok(out) => (false, out),
                Err(err) => (true, err),
            };

            history.lock().unwrap().push(cli_ui::HistoryItem::ToolEnd {
                name: "run_command".to_string(),
                args: cmd,
                result: out_text,
                is_error: is_err,
            });
            *output_row.lock().unwrap() = redraw_full_screen(&config, &history.lock().unwrap());
            continue;
        }

        // Model prompts create checkpoints for this chat; commands do not.
        current_session.started = true;
        let workspace = config.workspace_dir.clone();
        let mut conversation = current_session.clone();
        conversation.messages = agent.lock().await.get_messages().to_vec();
        conversation.history = history.lock().unwrap().clone();
        let label = line.clone();

        cli_ui::scroll_history(i32::MIN);
        // Autonomous AI Agent Execution
        let response_started = std::time::Instant::now();
        let mut response_tokens: Option<u64> = None;
        let mut usage_complete = true;
        history.lock().unwrap().push(cli_ui::HistoryItem::UserPrompt(line.clone()));
        current_session.history = history.lock().unwrap().clone();
        let _ = current_session.save(&config.workspace_dir);
        {
            *output_row.lock().unwrap() = redraw_full_screen(&config, &history.lock().unwrap());
        }

        if config.mode == theme::AppMode::MoA {
            let mut decision = moa_router::resolve_moa_route(&line, None).await;
            let available = config.provider_id.as_ref().and_then(|id| user_prefs.providers.get(id));
            if !available.is_some_and(|provider| provider.models.iter().any(|model|model.id==decision.selected_model)) {
                decision.selected_model = config.model.clone();
            }
            if config.model != decision.selected_model {
                config.model = decision.selected_model.clone();
                current_session.model = config.model.clone();
                let mut locked = agent.lock().await;
                locked.reset(config.clone());
            }
            let src_tag = decision.source.as_deref().unwrap_or("Router");
            let info_log = format!("⚡ Takiza MoA [{}]: routed to {} ({} • {})", src_tag, decision.selected_model, decision.category, decision.complexity);
            history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(info_log.clone()));
            let mut row = output_row.lock().unwrap();
            cli_ui::print_tool_log_at(&info_log, &mut *row, "", &branch_tag);
        }

        // Keep raw mode enabled during preparation/execution so typed keys cannot corrupt display
        crossterm::terminal::enable_raw_mode().ok();

        let cancel_token = CancellationToken::new();
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let mut sig_handle = spawn_sig_listener(terminal_tx.clone());

        cli_ui::set_active_draft(Some(line_editor.draft.clone()));
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(100);
        let thinking_message = cli_ui::random_thinking_message();
        let mut spinner = Some(cli_ui::Spinner::start_with_box(
            thinking_message,
            "",
            &branch_tag,
            output_row.clone(),
        ));

        let agent_clone = agent.clone();
        let cancel_token_clone = cancel_token.clone();
        let line_clone = line.clone();
        if current_session.title.is_none() && pending_title.is_none()
            && title_attempted_sessions.insert(current_session.id.clone()) {
            let title_client = crate::llm::LlmClient::new(config.clone());
            let title_prompt = line.clone();
            pending_title = Some((current_session.id.clone(), tokio::spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(90), title_client.generate_title(&title_prompt))
                    .await.context("Title generation timed out after 90 seconds")?
            })));
        }
        let capture_checkpoint = config.execution_mode.current() != execution_mode::ExecutionMode::Plan;
        let agent_task = tokio::spawn(async move {
            // Keep the editor, spinner and cancellation alive while scanning and
            // copying the workspace. Previously Enter blocked the UI here.
            let checkpoint_cancel = cancel_token_clone.clone();
            let capture = if capture_checkpoint { Some(tokio::task::spawn_blocking(move ||
                checkpoints::capture_session_cancellable(&workspace, &conversation, &label, checkpoint_cancel)).await) } else { None };
            if cancel_token_clone.is_cancelled() {
                let _ = event_tx.send(AgentEvent::Interrupted).await;
                let _ = event_tx.send(AgentEvent::Finished).await;
                return;
            }
            let error = match capture { Some(Ok(Err(error))) => Some(format!("{error:#}")),
                Some(Err(error)) => Some(error.to_string()), _ => None };
            if let Some(error) = error {
                let _ = event_tx.send(AgentEvent::ToolLog(crate::i18n::tf!("Checkpoint unavailable: {error}", error = error))).await;
            }
            let mut locked = agent_clone.lock().await;
            locked
                .handle_user_input(line_clone, event_tx, cancel_token_clone)
                .await;
        });

        let turn_session_id = current_session.id.clone();
        let mut active = Some(ActiveAgent { cancel: cancel_token.clone(), task: Some(agent_task), events: event_rx });
        let mut interrupted_by_command = false;
        let mut exit_requested = false;
        let mut stream_writer: Option<String> = None;
        let mut thought_index: Option<usize> = None;
        let mut tool_log_count = 0usize;

        let mut title_tick = tokio::time::interval(std::time::Duration::from_millis(100));
        let mut transcript_tick = tokio::time::interval(std::time::Duration::from_secs(1));
        transcript_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut background_tick = tokio::time::interval(snow::FRAME_INTERVAL);
        background_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Event processing loop
        loop {
            let event = tokio::select! {
                _ = transcript_tick.tick() => {
                    if current_session.id == turn_session_id {
                        save_active_transcript(&mut current_session, &config.workspace_dir,
                            &history.lock().unwrap(), &stream_writer);
                    }
                    continue;
                }
                _ = background_tick.tick(), if snow::enabled() => {
                    cli_ui::animate_background();
                    continue;
                }
                _ = title_tick.tick() => {
                    let title_changed = collect_chat_title(&mut pending_title, &mut current_session, &config.workspace_dir, &history);
                    if title_changed | collect_context_updates(&config, &history) {
                        let snapshot = history.lock().unwrap().clone();
                        redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                    }
                    continue;
                }
                Some(terminal_event) = terminal_rx.recv() => {
                    match terminal_event {
                        TerminalEvent::Mouse(mouse) => {
                            let (cols, rows) = cli_ui::terminal_size();
                            if line_editor.draft.mouse_cursor(mouse, cols.saturating_sub(6) as usize, rows) {
                                cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                continue;
                            }
                            match cli_ui::mouse_action(mouse) {
                                Some(cli_ui::MouseAction::Copy(text)) => {
                                    if let Ok(Err(error)) = tokio::task::spawn_blocking(move || clipboard::copy(&text)).await {
                                        logger::log_warn("Clipboard", &error);
                                    }
                                    let snapshot = history.lock().unwrap().clone();
                                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                }
                                Some(cli_ui::MouseAction::Paste) => {
                                    match tokio::task::spawn_blocking(clipboard::paste).await {
                                        Ok(Ok(text)) => line_editor.draft.insert_text(&text),
                                        Ok(Err(error)) => logger::log_warn("Clipboard", &error),
                                        Err(error) => logger::log_warn("Clipboard", &error.to_string()),
                                    }
                                    cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                                    let snapshot = history.lock().unwrap().clone();
                                    redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                }
                                Some(cli_ui::MouseAction::Clear) => {
                                    let snapshot = history.lock().unwrap().clone();
                                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                }
                                None => {}
                            }
                        }
                        TerminalEvent::Paste(text) => {
                            cli_ui::clear_mouse_selection();
                            line_editor.draft.insert_text(&text);
                            cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                            let snapshot = history.lock().unwrap().clone();
                            redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                        }
                        TerminalEvent::Scroll { delta, row } => {
                            cli_ui::clear_mouse_selection();
                            let (cols, rows) = cli_ui::terminal_size();
                            if line_editor.draft.mouse_over_input(row, cols.saturating_sub(6) as usize, rows) {
                                line_editor.draft.scroll_input(delta, cols.saturating_sub(6) as usize, rows);
                                cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                continue;
                            }
                            cli_ui::scroll_history(delta);
                            let snapshot = history.lock().unwrap().clone();
                            redraw_active_view(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                        }
                        TerminalEvent::Key(key) => {
                            if execution_mode::is_cycle_key(key) {
                                set_execution_mode(&config, &mut user_prefs, config.execution_mode.current().next())?;
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                continue;
                            }
                            if line_editor.draft.clipboard_key(key) {
                                cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                continue;
                            }
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                && key.code == crossterm::event::KeyCode::Char('c') {
                                cancel_token.cancel();
                                continue;
                            }
                            if cli_ui::clear_mouse_selection() {
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_view(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                            }
                            if let Some(delta) = cli_ui::history_scroll_key(key) {
                                cli_ui::scroll_history(delta);
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_view(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                                continue;
                            }
                            if line_editor.draft.should_stop_response(key) {
                                line_editor.active_key(key);
                                cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                                cancel_token.cancel();
                                continue;
                            }
                            let (cols, rows) = cli_ui::terminal_size();
                            let width = cols.saturating_sub(6) as usize;
                            let previous_box_rows = line_editor.draft.menu_rows(rows) + line_editor.draft.input_rows(width, rows);
                            let submitted = line_editor.active_key(key);
                            cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                            if let Some(submitted) = submitted {
                                cli_ui::scroll_history(i32::MIN);
                                if submitted.starts_with('/') && !grill_me::is_command(&submitted) {
                                    sig_handle.stop().await;
                                    let status = spinner.as_ref().map(|s| s.message());
                                    if let Some(s) = spinner.take() { s.stop().await; }
                                    cli_ui::set_active_draft(None);
                                    cli_ui::disable_raw_mode().ok();
                                    let outcome = handle_tui_command(&submitted, &mut config, &mut user_prefs,
                                        &agent, &mut current_session, &history, &output_row, &branch_tag, &mut active, &tools_executor).await?;
                                    crossterm::terminal::enable_raw_mode().ok();
                                    exit_requested = outcome == CommandResult::Exit;
                                    match outcome {
                                        CommandResult::InsertSkill(skill) => line_editor.draft.insert_text(&skill.prompt_reference()),
                                        CommandResult::RestorePrompt(text) => {
                                            line_editor.draft = prompt::Draft::default();
                                            line_editor.draft.text = text;
                                            line_editor.draft.cursor = line_editor.draft.text.chars().count();
                                        }
                                        _ => {}
                                    }
                                    interrupted_by_command = active.as_ref().is_some_and(|a| a.task.is_none());
                                    if exit_requested || interrupted_by_command {
                                        if exit_requested { active.as_mut().unwrap().stop(&history, &mut current_session.token_usage, &mut current_session.context_tokens).await; }
                                        // Partial output belongs to the previous session, never replay it into a new one.
                                        stream_writer = None;
                                        if current_session.id != turn_session_id || exit_requested || matches!(submitted.split_whitespace().next(), Some("/rewind" | "/restore")) {
                                            prompt_queue.clear(&mut history.lock().unwrap());
                                        }
                                        cli_ui::set_pending_prompt_count(prompt_queue.pending.len());
                                        break;
                                    }
                                    cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                                    if let Some(message) = status {
                                        spinner = Some(cli_ui::Spinner::start_with_box(message, "", &branch_tag, output_row.clone()));
                                    }
                                    let snapshot = history.lock().unwrap().clone();
                                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row,
                                        &mut spinner, &mut stream_writer).await;
                                    sig_handle = spawn_sig_listener(terminal_tx.clone());
                                } else {
                                    prompt_queue.push(submitted, &mut history.lock().unwrap());
                                    cli_ui::set_pending_prompt_count(prompt_queue.pending.len());
                                    let snapshot = history.lock().unwrap().clone();
                                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row,
                                        &mut spinner, &mut stream_writer).await;
                                }
                            } else if previous_box_rows != line_editor.draft.menu_rows(rows) + line_editor.draft.input_rows(width, rows) {
                                // Content and editor must move together when a completion menu changes size.
                                let snapshot = history.lock().unwrap().clone();
                                redraw_active_editor(&config, &snapshot, &branch_tag, &output_row,
                                    &mut spinner, &mut stream_writer).await;
                                continue;
                            }
                            let (x, y) = cli_ui::render_bottom_box("", &branch_tag);
                            cli_ui::position_input_cursor(x, y);
                        }
                        TerminalEvent::ToggleOutput | TerminalEvent::Resize => {
                            cli_ui::clear_mouse_selection();
                            if matches!(terminal_event, TerminalEvent::ToggleOutput) { cli_ui::toggle_expanded_output(); }
                            let snapshot = history.lock().unwrap().clone();
                            redraw_active_view(&config, &snapshot, &branch_tag, &output_row,
                                &mut spinner, &mut stream_writer).await;
                            let (x, y) = cli_ui::render_bottom_box("", &branch_tag);
                            cli_ui::position_input_cursor(x, y);
                        }
                    }
                    continue;
                }
                event = active.as_mut().unwrap().events.recv() => match event {
                    Some(event) => event,
                    None => break,
                },
            };
            match event {
                AgentEvent::CompactionProgress { percent, part, parts } => {
                    if spinner.as_ref().is_none_or(|s| s.message() != crate::i18n::tr("Compacting context...")) {
                        if let Some(s) = spinner.take() { s.stop().await; }
                        spinner = Some(cli_ui::Spinner::start_with_box(crate::i18n::tr("Compacting context..."),
                            "", &branch_tag, output_row.clone()));
                    }
                    cli_ui::set_compaction_progress(percent, part, parts);
                }
                AgentEvent::StatusUpdate(status) => {
                    if status == "Thinking..." {
                        thought_index = None;
                        if spinner.as_ref().is_none_or(|s| s.message() != thinking_message) {
                            if let Some(s) = spinner.take() { s.stop().await; }
                            spinner = Some(cli_ui::Spinner::start_with_box(
                                thinking_message, "", &branch_tag, output_row.clone(),
                            ));
                        }
                    } else if status.starts_with("Connection interrupted;") && spinner.is_none() {
                        spinner = Some(cli_ui::Spinner::start_with_box(
                            "Reconnecting...", "", &branch_tag, output_row.clone(),
                        ));
                    } else if status.starts_with("Invalid tool call;") && spinner.is_none() {
                        spinner = Some(cli_ui::Spinner::start_with_box(
                            "Retrying tool call...", "", &branch_tag, output_row.clone(),
                        ));
                    }
                }
                AgentEvent::PermissionRequest { name, command, responder, .. } => {
                    if cancel_token.is_cancelled() {
                        if let Some(tx) = responder.lock().unwrap().take() { let _ = tx.send(agent::PermissionResponse::Deny); }
                        continue;
                    }
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);

                    sig_handle.stop().await;
                    cli_ui::invalidate_content_frame();
                    cli_ui::set_active_draft(None);

                    let choice = if matches!(name.as_str(), "write_file" | "edit_file") {
                        let ctx = interactive::ScreenContext::from_config(&config);
                        let width = cli_ui::get_box_width().saturating_sub(6);
                        let details = command.lines().flat_map(|line| cli_ui::wrap_text_line(line, width)).collect::<Vec<_>>();
                        let choices = vec![crate::i18n::tr("Allow once").into(), crate::i18n::tr("Deny").into()];
                        match interactive::file_permission_menu(&ctx, &details, &choices)? {
                            Some(0) => cli_ui::PermissionChoice::AllowOnce,
                            _ => cli_ui::PermissionChoice::Deny,
                        }
                    } else if name.starts_with("mcp__") {
                        let ctx = interactive::ScreenContext::from_config(&config);
                        let width = cli_ui::get_box_width().saturating_sub(6);
                        let details = command.lines().flat_map(|line| cli_ui::wrap_text_line(line, width)).collect::<Vec<_>>();
                        let choices = vec![crate::i18n::tr("Allow once").into(), crate::i18n::tr("Deny").into(), crate::i18n::tr("Trust this server for this run").into()];
                        match interactive::mcp_permission_menu(&ctx, &details, &choices)? {
                            Some(0) => cli_ui::PermissionChoice::AllowOnce,
                            Some(2) => cli_ui::PermissionChoice::AllowAlways,
                            _ => cli_ui::PermissionChoice::Deny,
                        }
                    } else {
                        let mut row = output_row.lock().unwrap();
                        let snapshot = history.lock().unwrap().clone();
                        cli_ui::ask_command_permission_with_redraw(&command, &mut *row, &branch_tag,
                            |row| *row = redraw_full_screen(&config, &snapshot))
                    };

                    let response = match choice {
                        cli_ui::PermissionChoice::AllowOnce => {
                            agent::PermissionResponse::AllowOnce
                        }
                        cli_ui::PermissionChoice::AllowAlways => {
                            if name == "run_command" {
                                set_execution_mode(&config, &mut user_prefs, execution_mode::ExecutionMode::AllowAll)?;
                            }
                            agent::PermissionResponse::AllowAlways
                        }
                        cli_ui::PermissionChoice::Deny => {
                            agent::PermissionResponse::Deny
                        }
                    };

                    if let Some(tx) = responder.lock().unwrap().take() {
                        let _ = tx.send(response);
                    }

                    cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                    sig_handle = spawn_sig_listener(terminal_tx.clone());

                    spinner = Some(cli_ui::Spinner::start_with_box(
                        thinking_message,
                        "",
                        &branch_tag,
                        output_row.clone(),
                    ));
                }
                AgentEvent::QuestionRequest { questions, responder, .. } => {
                    if cancel_token.is_cancelled() {
                        if let Some(tx) = responder.lock().unwrap().take() {
                            let _ = tx.send(agent::QuestionResponse { answers: Vec::new(), skipped: true });
                        }
                        continue;
                    }
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);

                    sig_handle.stop().await;
                    cli_ui::invalidate_content_frame();
                    cli_ui::set_active_draft(None);

                    let response = {
                        let mut row = output_row.lock().unwrap();
                        let snapshot = history.lock().unwrap().clone();
                        cli_ui::ask_interactive_question_with_redraw(&questions, &mut *row, &branch_tag,
                            |row| *row = redraw_full_screen(&config, &snapshot))
                    };

                    for ans in &response.answers {
                        let mut parts = Vec::new();
                        if !ans.selected_options.is_empty() {
                            parts.push(ans.selected_options.join(", "));
                        }
                        if let Some(ref c) = ans.custom_text {
                            if !c.trim().is_empty() {
                                parts.push(format!("\"{}\"", c.trim()));
                            }
                        }
                        let answer_str = if parts.is_empty() {
                            "[Skipped]".to_string()
                        } else {
                            parts.join(" | ")
                        };
                        history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(format!("  📋 Question: {}", ans.question)));
                        history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(format!("     Answer: {}", answer_str)));
                    }

                    if let Some(tx) = responder.lock().unwrap().take() {
                        let _ = tx.send(response);
                    }

                    cli_ui::set_active_draft(Some(line_editor.draft.clone()));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                    sig_handle = spawn_sig_listener(terminal_tx.clone());

                    spinner = Some(cli_ui::Spinner::start_with_box(
                        thinking_message,
                        "",
                        &branch_tag,
                        output_row.clone(),
                    ));
                }
                AgentEvent::StreamRetry { preserve_text } => {
                    if let Some(s) = spinner.take() { s.stop().await; }
                    if let Some(index) = thought_index.take() {
                        if !preserve_text { history.lock().unwrap().remove(index); }
                    }
                    if preserve_text {
                        preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);
                    } else { stream_writer.take(); }
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::TokenUsage(tokens) => {
                    session::record_tokens(&mut current_session.token_usage, tokens);
                    if let Some(tokens) = tokens {
                        response_tokens = Some(response_tokens.unwrap_or(0).saturating_add(tokens));
                    } else { usage_complete = false; }
                }
                AgentEvent::ContextUsage(tokens) => {
                    current_session.context_tokens = tokens;
                    cli_ui::set_context_tokens(tokens);
                    let snapshot = history.lock().unwrap().clone();
                    redraw_active_editor(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
                }
                AgentEvent::ThoughtToken(token) => {
                    cli_ui::update_streamed_thought(&mut history.lock().unwrap(), &mut thought_index, &token, false);
                    let snapshot = history.lock().unwrap().clone();
                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row,
                        &mut spinner, &mut stream_writer).await;
                    if spinner.is_none() {
                        spinner = Some(cli_ui::Spinner::start_with_box(
                            thinking_message, "", &branch_tag, output_row.clone(),
                        ));
                    }
                }
                AgentEvent::AssistantToken(token) => {
                    if spinner.as_ref().is_none_or(|s| s.message() != "Responding...") {
                        if let Some(s) = spinner.take() { s.stop().await; }
                        spinner = Some(cli_ui::Spinner::start_with_box(
                            "Responding...", "", &branch_tag, output_row.clone(),
                        ));
                    }
                    stream_writer.get_or_insert_with(String::new).push_str(&token);
                    let snapshot = history.lock().unwrap().clone();
                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row,
                        &mut spinner, &mut stream_writer).await;
                }
                AgentEvent::AssistantThought(thought) => {
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    // Content accompanying a tool call is ordinary assistant text,
                    // even when the provider has no separate reasoning field.
                    if thought_index.is_none() && stream_writer.as_ref().is_some_and(|text| text == &thought) {
                        preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);
                    } else {
                        cli_ui::update_streamed_thought(&mut history.lock().unwrap(), &mut thought_index, &thought, true);
                    }
                    let snapshot = history.lock().unwrap().clone();
                    redraw_active_view(&config, &snapshot, &branch_tag, &output_row,
                        &mut spinner, &mut stream_writer).await;
                    spinner = Some(cli_ui::Spinner::start_with_box(
                        "Executing tools...",
                        "",
                        &branch_tag,
                        output_row.clone(),
                    ));
                }
                AgentEvent::ToolStart { name, args, .. } => {
                    tool_log_count = 0;
                    preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);
                    thought_index = None;
                    if let Some(s) = spinner.take() { s.stop().await; }
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolStart { name, args });
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                    spinner = Some(cli_ui::Spinner::start_with_box(
                        "Executing tools...", "", &branch_tag, output_row.clone(),
                    ));
                }
                AgentEvent::FileDiff(diff) => {
                    history.lock().unwrap().push(cli_ui::HistoryItem::FileDiff(diff));
                }
                AgentEvent::ToolLog(log_item) => {
                    if log_item.starts_with("$ ") { continue; }
                    tool_log_count += 1;
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(log_item));
                    if cli_ui::is_output_expanded() || tool_log_count <= 4 {
                        if let Some(s) = spinner.take() { s.stop().await; }
                        let snapshot = history.lock().unwrap().clone();
                        *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                        spinner = Some(cli_ui::Spinner::start_with_box(
                            "Executing tools...", "", &branch_tag, output_row.clone(),
                        ));
                    }
                }
                AgentEvent::ToolEnd { name, args, result, is_error, .. } => {
                    thought_index = None;
                    if is_error && !cli_ui::tool_was_denied(&result) {
                        crate::logger::log_warn("Tool", &format!("{name} failed (args: {args}): {result}"));
                    }
                    if let Some(s) = spinner.take() { s.stop().await; }
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolEnd {
                        name,
                        args,
                        result,
                        is_error,
                    });
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                    spinner = Some(cli_ui::Spinner::start_with_box(
                        thinking_message,
                        "",
                        &branch_tag,
                        output_row.clone(),
                    ));
                }
                AgentEvent::AssistantMessage(msg) => {
                    stream_writer.take();
                    thought_index = None;
                    history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(msg));
                    history.lock().unwrap().push(cli_ui::HistoryItem::ResponseStats {
                        elapsed_ms: response_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                        tokens: response_tokens,
                        usage_complete,
                    });
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::Interrupted => {
                    session::record_tokens(&mut current_session.token_usage, None);
                    if let Some(s) = spinner.take() { s.stop().await; }
                    if let Some(sw) = stream_writer.take() {
                        let partial = sw;
                        if !partial.trim().is_empty() {
                            history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(partial));
                        }
                    }
                    let message = crate::i18n::tr("⏸ Stopped by user");
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(message.to_string()));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::Error(err) => {
                    session::record_tokens(&mut current_session.token_usage, None);
                    crate::logger::log_error("Agent", &err);
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);
                    history.lock().unwrap().push(cli_ui::HistoryItem::Error(err));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::Finished => {
                    preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    break;
                }
                AgentEvent::UserMessage(_) => {}
            }
            if cli_ui::history_scrolled() {
                let snapshot = history.lock().unwrap().clone();
                redraw_active_view(&config, &snapshot, &branch_tag, &output_row, &mut spinner, &mut stream_writer).await;
            }
        }

        preserve_streamed_text(&mut history.lock().unwrap(), &mut stream_writer);
        if let Some(s) = spinner.take() {
            s.stop().await;
        }

        sig_handle.stop().await;
        if let Some(task) = active.as_mut().unwrap().task.take() {
            if let Err(error) = task.await {
                let message = crate::i18n::tf!("Agent task failed unexpectedly: {error}", error = error);
                logger::log_error("Agent", &message);
                history.lock().unwrap().push(cli_ui::HistoryItem::Error(message));
                *output_row.lock().unwrap() = redraw_full_screen(&config, &history.lock().unwrap());
            }
        }
        // Preserve input already forwarded by the reader at the response boundary.
        while let Ok(event) = terminal_rx.try_recv() {
            if let TerminalEvent::Paste(text) = &event {
                line_editor.draft.insert_text(text);
                continue;
            }
            if let TerminalEvent::Mouse(mouse) = &event {
                match cli_ui::mouse_action(*mouse) {
                    Some(cli_ui::MouseAction::Copy(text)) => { let _ = clipboard::copy(&text); }
                    Some(cli_ui::MouseAction::Paste) => {
                        if let Ok(text) = clipboard::paste() { line_editor.draft.insert_text(&text); }
                    }
                    Some(cli_ui::MouseAction::Clear) | None => {}
                }
                continue;
            }
            if let TerminalEvent::Key(key) = event {
                if execution_mode::is_cycle_key(key) {
                    set_execution_mode(&config, &mut user_prefs, config.execution_mode.current().next())?;
                    continue;
                }
                if let Some(text) = line_editor.active_key(key) {
                    if text.starts_with('/') {
                        ready_commands.push_back(text);
                    } else if current_session.id == turn_session_id && !exit_requested {
                        prompt_queue.push(text, &mut history.lock().unwrap());
                    }
                }
            }
        }
        cli_ui::set_active_draft(None);
        cli_ui::disable_raw_mode().ok();
        if pending_title.as_ref().is_some_and(|(id, _)| *id != current_session.id) {
            if let Some((_, task)) = pending_title.take() { task.abort(); }
        }
        if interrupted_by_command || exit_requested {
            if exit_requested { break 'chat; }
            continue;
        }

        collect_chat_title(&mut pending_title, &mut current_session, &config.workspace_dir, &history);
        // Auto-save session transcript
        let locked = agent.lock().await;
        current_session.messages = locked.get_messages().to_vec();
        current_session.history = history.lock().unwrap().clone();
        let _ = current_session.save(&config.workspace_dir);
    }

    if let Some((_, task)) = pending_title.take() { task.abort(); }
    agent.lock().await.runtime().shutdown().await;
    current_session.messages = agent.lock().await.get_messages().to_vec();
    current_session.history = history.lock().unwrap().clone();
    let saved = current_session.is_started().then(|| current_session.save(&config.workspace_dir));
    drop(_terminal_screen);
    println!("\n{}", crate::i18n::tr("Goodbye!"));
    let prompts = current_session.history.iter().filter(|item| matches!(item, cli_ui::HistoryItem::UserPrompt(_))).count();
    let tools = current_session.messages.iter().filter_map(|message| message.tool_calls.as_ref()).map(Vec::len).sum::<usize>();
    let elapsed = cli_ui::duration_text(run_started.elapsed().as_millis().min(u64::MAX as u128) as u64);
    let tokens = match &current_session.token_usage {
        Some(usage) => cli_ui::token_usage_text(usage.total, usage.complete),
        None if prompts == 0 => cli_ui::token_usage_text(Some(0), true),
        None => cli_ui::token_usage_text(None, false),
    };
    if current_session.is_started() {
        println!("{}", crate::i18n::tf!("Session: {id} · {model}", id = current_session.id, model = current_session.model));
    }
    println!("{}", crate::i18n::tf!("{prompts} prompts · {tools} tool calls · {elapsed} in this run", prompts = prompts, tools = tools, elapsed = elapsed));
    println!("{}", crate::i18n::tf!("Reported model usage: {tokens}", tokens = tokens));
    match saved {
        Some(Ok(())) => {
            println!("{}", crate::i18n::tr("Resume this session from this directory:"));
            println!("  takiza --session {}", current_session.id);
        }
        Some(Err(error)) => println!("{}", crate::i18n::tf!("Session could not be saved: {error}", error = error)),
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod active_input_tests {
    use super::*;

    #[test]
    fn queued_prompts_keep_order_and_remove_only_their_pending_markers() {
        let mut queue = PromptQueue::default();
        let mut history = vec![cli_ui::HistoryItem::ToolLog("existing log".into())];
        queue.push("first".into(), &mut history);
        queue.push("second".into(), &mut history);
        assert_eq!(queue.pop(&mut history).as_deref(), Some("first"));
        assert!(history.iter().any(|item| matches!(item, cli_ui::HistoryItem::ToolLog(s) if s == "⏳ Queued #2: second")));
        assert!(!history.iter().any(|item| matches!(item, cli_ui::HistoryItem::ToolLog(s) if s == "⏳ Queued #1: first")));
        assert_eq!(queue.pop(&mut history).as_deref(), Some("second"));
        assert!(queue.pop(&mut history).is_none());
        assert_eq!(history.len(), 1);
        queue.push("third".into(), &mut history);
        queue.clear(&mut history);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn tool_transition_preserves_streamed_answer_once_and_does_not_clear_old_history() {
        let mut history = vec![cli_ui::HistoryItem::UserPrompt("Inspect project".into())];
        let mut stream = Some("Checking logic's pockets...".into());
        preserve_streamed_text(&mut history, &mut stream);
        preserve_streamed_text(&mut history, &mut stream);
        history.push(cli_ui::HistoryItem::Thought("Retrying the call".into()));
        stream = Some("Checking logic's pockets...".into());
        preserve_streamed_text(&mut history, &mut stream);
        assert!(stream.is_none());
        assert_eq!(history.len(), 3);
        assert!(matches!(&history[1], cli_ui::HistoryItem::AssistantMessage(text) if text == "Checking logic's pockets..."));
    }

    #[tokio::test]
    async fn stopping_active_agent_drains_full_channel_and_releases_modal_waiter() {
        let (tx, events) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            for _ in 0..10 { tx.send(AgentEvent::AssistantToken("chunk".into())).await.unwrap(); }
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            tx.send(AgentEvent::QuestionRequest {
                id: "question".into(), questions: Vec::new(),
                responder: Arc::new(StdMutex::new(Some(response_tx))),
            }).await.unwrap();
            assert!(response_rx.await.is_err());
            tx.send(AgentEvent::Finished).await.unwrap();
        });
        let cancel = CancellationToken::new();
        let mut active = ActiveAgent { task: Some(task), events, cancel: cancel.clone() };
        tokio::time::timeout(std::time::Duration::from_secs(2), active.stop(&Arc::new(StdMutex::new(Vec::new())), &mut None, &mut None)).await.unwrap();
        assert!(cancel.is_cancelled());
        assert!(active.task.is_none());
    }
}
