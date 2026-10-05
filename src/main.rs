mod agent;
mod attachments;
mod checkpoints;
mod clipboard;
mod cli_ui;
mod config;
mod git;
mod interactive;
mod llm;
mod logo;
pub mod logger;
mod markdown;
mod onboarding;
mod prompt;
mod session;
mod theme;
mod tools;
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
use tools::{ToolExecutor, ToolOutputEvent};

type PendingTitle = Option<(String, tokio::task::JoinHandle<anyhow::Result<String>>)>;

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
            history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(format!("Chat title unavailable: {error:#}")));
        }
        _ => return false,
    }
    true
}

fn clear_screen_and_banner(config: &Config) -> u16 {
    cli_ui::invalidate_content_frame();
    let git_info = GitInfo::get(&config.workspace_dir);
    let git_display = match &git_info.branch {
        Some(b) => {
            if git_info.is_dirty {
                format!("{} (dirty)", b)
            } else {
                format!("{} (clean)", b)
            }
        }
        None => "(no git)".to_string(),
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

fn redraw_full_screen(config: &Config, history: &[cli_ui::HistoryItem]) -> u16 {
    let current_git = GitInfo::get_cached(&config.workspace_dir);
    let git_display = match &current_git.branch {
        Some(b) => {
            if current_git.is_dirty {
                format!("{} (dirty)", b)
            } else {
                format!("{} (clean)", b)
            }
        }
        None => "(no git)".to_string(),
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

async fn redraw_active_view(
    config: &Config,
    history: &[cli_ui::HistoryItem],
    _branch_tag: &str,
    output_row: &Arc<StdMutex<u16>>,
    spinner: &mut Option<cli_ui::Spinner>,
    stream_writer: &mut Option<String>,
) {
    // Streaming and browsing share the same Markdown layout and content viewport.
    // Never replay the response through terminal scrolling commands.
    if stream_writer.is_some() {
        if let Some(active_spinner) = spinner.take() { active_spinner.stop().await; }
    }
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
    // Stop concurrent terminal writes before opening one frame for content and editor.
    if stream_writer.is_some() {
        if let Some(active_spinner) = spinner.take() { active_spinner.stop().await; }
    }
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
enum CommandResult { Handled, Exit, InsertSkill(skills::Skill) }

struct ActiveAgent {
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    events: mpsc::Receiver<AgentEvent>,
}

impl ActiveAgent {
    async fn stop(&mut self, history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>) {
        self.cancel.cancel();
        if let Some(mut task) = self.task.take() {
            // Drain bounded output while cancellation releases the agent lock.
            loop {
                tokio::select! {
                    _ = &mut task => break,
                    event = self.events.recv() => match event {
                        Some(AgentEvent::AssistantMessage(message)) => history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(message)),
                        Some(_) => {}, // Dropping modal responders unblocks tool requests.
                        None => { let _ = task.await; break; }
                    }
                }
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
            running.stop(history).await;
            let locked = agent.lock().await;
            session.messages = locked.get_messages().to_vec();
            session.history = history.lock().unwrap().clone();
            let _ = session.save(&config.workspace_dir);
            return locked;
        }
    }
    agent.lock().await
}

async fn handle_tui_command(
    line: &str, config: &mut Config, user_prefs: &mut theme::UserPreferences,
    agent: &Arc<Mutex<Agent>>, current_session: &mut Session,
    history: &Arc<StdMutex<Vec<cli_ui::HistoryItem>>>, output_row: &Arc<StdMutex<u16>>,
    branch_tag: &str, active: &mut Option<ActiveAgent>,
) -> anyhow::Result<CommandResult> {
    cli_ui::invalidate_content_frame();
        // Handle slash commands
        if line == "/exit" || line == "/quit" {
            println!("Goodbye!");
            return Ok(CommandResult::Exit);
        }

        if matches!(line.split_whitespace().next(), Some("/rewind" | "/restore")) {
            // Freeze tool execution before inspecting or replacing files.
            let mut locked = command_agent(agent, active, current_session, history, config).await;
            current_session.messages = locked.get_messages().to_vec();
            current_session.history = history.lock().unwrap().clone();
            let result = (|| -> anyhow::Result<Option<Session>> {
                let points = checkpoints::list(&config.workspace_dir, &current_session.id)?;
                let ctx = interactive::ScreenContext::from_config(config);
                let choices = points.iter().map(|point| format!("{}  {}", point.created_at, point.label)).collect::<Vec<_>>();
                let Some(selected) = interactive::rewind_menu(&ctx, "Restore workspace checkpoint", &[], &choices)? else { return Ok(None); };
                let point = &points[selected];
                let changes = checkpoints::preview(&config.workspace_dir, point)?;
                let conversation = checkpoints::conversation_at(point, current_session)?;
                let mut changes = changes;
                changes.push("Restore chat to this checkpoint; later prompts, answers and tool results will be removed".into());
                let options = vec!["Cancel".to_string(), "Restore files and chat".to_string()];
                if interactive::rewind_menu(&ctx, "Review restore · a safety checkpoint will be saved", &changes, &options)? != Some(1) { return Ok(None); }
                checkpoints::restore_session(&config.workspace_dir, point, current_session)?;
                Ok(Some(conversation))
            })();
            match result {
                Ok(Some(conversation)) => {
                    current_session.history = conversation.build_history_from_messages();
                    current_session.messages = conversation.messages;
                    locked.set_messages(current_session.messages.clone());
                    *history.lock().unwrap() = current_session.history.clone();
                    cli_ui::scroll_history(i32::MIN);
                    current_session.save(&config.workspace_dir)?;
                }
                Ok(None) => {}
                Err(error) => history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(format!("Restore unavailable: {error:#}"))),
            }
            *output_row.lock().unwrap() = redraw_full_screen(config, &history.lock().unwrap());
            return Ok(CommandResult::Handled);
        }

        if line == "/help" {
            clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            cli_ui::print_help();
            return Ok(CommandResult::Handled);
        }

        if line == "/approval" || line == "/permissions" {
            config.auto_approve = !config.auto_approve;
            let mut locked = command_agent(agent, active, current_session, history, config).await;
            locked.config.auto_approve = config.auto_approve;
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            if config.auto_approve {
                println!("Command execution approval: AUTO-APPROVE (commands run without asking)\n");
            } else {
                println!("Command execution approval: ASK (permission requested before each command)\n");
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
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched theme to: {}\n", th.name());
                } else {
                    clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Unknown theme: '{}'. Available themes: amber, cyberpunk, emerald, nord, monochrome\n", parts[1]);
                }
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                if let Ok(Some(new_th)) = interactive::select_theme_interactive(&ctx, user_prefs.theme) {
                    user_prefs.theme = new_th;
                    let _ = user_prefs.save();
                    theme::set_current(new_th);
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched theme to: {}\n", new_th.name());
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/mode") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let ctx = interactive::ScreenContext::from_config(&config);
            if parts.len() > 1 {
                let sub = parts[1];
                if let Some(mode) = theme::AppMode::from_str_loose(sub) {
                    match mode {
                        theme::AppMode::Manual => {
                            if parts.len() > 2 {
                                let chosen = parts[2].to_string();
                                config.mode = theme::AppMode::Manual;
                                config.model = chosen.clone();
                                user_prefs.mode = Some("manual".to_string());
                                user_prefs.model = Some(chosen.clone());
                                let _ = user_prefs.save();
                                let mut locked = command_agent(agent, active, current_session, history, config).await;
                                locked.reset(config.clone());
                                *current_session = Session::new(config.model.clone());
                                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                                cli_ui::print_user_cmd(&line);
                                println!("Mode switched to: \x1b[1mTakiza Manual\x1b[0m\nActive model: \x1b[1;38;2;0;220;255m{}\x1b[0m", config.model);
                                if let Some(curated) = interactive::CURATED_MODELS.iter().find(|m| interactive::is_curated_model_match(m.id, &chosen)) {
                                    if curated.is_expensive {
                                        println!("  \x1b[1;38;2;255;95;80m⚠ WARNING:\x1b[0m High-tier model ($10/1M in, $50/1M out). Quota and token limits will be consumed significantly faster!");
                                    }
                                }
                                println!();
                            } else if let Ok(Some(curated)) = interactive::select_curated_model_interactive(&ctx, &config.model) {
                                config.mode = theme::AppMode::Manual;
                                config.model = curated.id.to_string();
                                user_prefs.mode = Some("manual".to_string());
                                user_prefs.model = Some(curated.id.to_string());
                                let _ = user_prefs.save();
                                let mut locked = command_agent(agent, active, current_session, history, config).await;
                                locked.reset(config.clone());
                                *current_session = Session::new(config.model.clone());
                                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                                cli_ui::print_user_cmd(&line);
                                println!("Mode switched to: \x1b[1mTakiza Manual\x1b[0m\nSelected Model: \x1b[1;38;2;0;220;255m{} ({})\x1b[0m • {}", curated.name, curated.provider, curated.description);
                                if curated.is_expensive {
                                    println!("  \x1b[1;38;2;255;95;80m⚠ WARNING:\x1b[0m High-tier model ($10/1M in, $50/1M out). Quota and token limits will be consumed significantly faster!");
                                }
                                println!();
                            } else {
                                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                            }
                        }
                        theme::AppMode::MoA => {
                            config.mode = theme::AppMode::MoA;
                            user_prefs.mode = Some("moa".to_string());
                            let _ = user_prefs.save();
                            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                            cli_ui::print_user_cmd(&line);
                            println!("Mode switched to: \x1b[1mTakiza MoA\x1b[0m\n  \x1b[1;38;2;40;220;120m⚡ ~45% Cheaper:\x1b[0m Smart task orchestration — routine search and file inspection run on fast models, while complex code routes to flagships without token waste!\n");
                        }
                    }
                } else {
                    clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Unknown mode: '{}'. Available modes: manual, moa\n", sub);
                }
            } else if let Ok(Some(chosen_mode)) = interactive::select_mode_interactive(&ctx, config.mode) {
                match chosen_mode {
                    theme::AppMode::Manual => {
                        if let Ok(Some(curated)) = interactive::select_curated_model_interactive(&ctx, &config.model) {
                            config.mode = theme::AppMode::Manual;
                            config.model = curated.id.to_string();
                            user_prefs.mode = Some("manual".to_string());
                            user_prefs.model = Some(curated.id.to_string());
                            let _ = user_prefs.save();
                            let mut locked = command_agent(agent, active, current_session, history, config).await;
                            locked.reset(config.clone());
                            *current_session = Session::new(config.model.clone());
                            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                            cli_ui::print_user_cmd(&line);
                            println!("Mode switched to: \x1b[1mTakiza Manual\x1b[0m\nSelected Model: \x1b[1;38;2;0;220;255m{} ({})\x1b[0m • {}", curated.name, curated.provider, curated.description);
                            if curated.is_expensive {
                                println!("  \x1b[1;38;2;255;95;80m⚠ WARNING:\x1b[0m High-tier model ($10/1M in, $50/1M out). Quota and token limits will be consumed significantly faster!");
                            }
                            println!();
                        } else {
                            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                        }
                    }
                    theme::AppMode::MoA => {
                        config.mode = theme::AppMode::MoA;
                        user_prefs.mode = Some("moa".to_string());
                        let _ = user_prefs.save();
                        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                        cli_ui::print_user_cmd(&line);
                        println!("Mode switched to: \x1b[1mTakiza MoA\x1b[0m\n  \x1b[1;38;2;40;220;120m⚡ ~45% Cheaper:\x1b[0m Smart task orchestration — routine search and file inspection run on fast models, while complex code routes to flagships without token waste!\n");
                    }
                }
            } else {
                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            }
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

        if line == "/tools" {
            let ctx = interactive::ScreenContext::from_config(&config);
            interactive::show_interactive_tools(&ctx)?;
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            return Ok(CommandResult::Handled);
        }

        if line == "/diff" {
            if let Some(diff) = GitInfo::diff(&config.workspace_dir) {
                if diff.trim().is_empty() {
                    clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("No changes (working tree clean).\n");
                } else {
                    let ctx = interactive::ScreenContext::from_config(&config);
                    interactive::show_interactive_diff(&ctx, &diff, &config.workspace_dir)?;
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            } else {
                clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                println!("No git repository found in workspace.\n");
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/model") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 1 {
                config.model = parts[1].to_string();
                config.mode = theme::AppMode::Manual;
                user_prefs.mode = Some("manual".to_string());
                user_prefs.model = Some(config.model.clone());
                let _ = user_prefs.save();
                let mut locked = command_agent(agent, active, current_session, history, config).await;
                locked.reset(config.clone());
                *current_session = Session::new(config.model.clone());
                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                println!("Switched model to: {}\n", config.model);
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                if let Ok(Some(chosen_model)) = interactive::select_model(&ctx, &config.base_url, &config.model) {
                    config.model = chosen_model.clone();
                    config.mode = theme::AppMode::Manual;
                    user_prefs.mode = Some("manual".to_string());
                    user_prefs.model = Some(chosen_model);
                    let _ = user_prefs.save();
                    let mut locked = command_agent(agent, active, current_session, history, config).await;
                    locked.reset(config.clone());
                    *current_session = Session::new(config.model.clone());
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched model to: {}\n", config.model);
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/effort") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let chosen_effort = if parts.len() > 1 {
                let eff = parts[1].to_lowercase();
                match eff.as_str() {
                    "low" | "medium" | "high" => Some(eff),
                    _ => {
                        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                        cli_ui::print_user_cmd(&line);
                        println!("Unknown effort: {}. Available: low, medium, high\n", parts[1]);
                        None
                    }
                }
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                let cur = config.effort.clone().unwrap_or_else(|| "medium".to_string());
                match interactive::select_effort_interactive(&ctx, &cur) {
                    Ok(Some(eff)) => Some(eff),
                    _ => {
                        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                        None
                    }
                }
            };

            if let Some(eff) = chosen_effort {
                config.effort = Some(eff.clone());
                user_prefs.effort = Some(eff.clone());
                let _ = user_prefs.save();
                let mut locked = command_agent(agent, active, current_session, history, config).await;
                locked.reset(config.clone());
                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                let is_supported = llm::supports_reasoning_effort(&config.model);
                if is_supported {
                    println!("Set reasoning effort to: {}\n", eff);
                } else {
                    println!("Set reasoning effort to: {} (Note: current model '{}' may not use reasoning effort; setting will apply when switching to o1, o3-mini, claude-3-7, deepseek-r1, etc.)\n", eff, config.model);
                }
            }
            return Ok(CommandResult::Handled);
        }

        if line.starts_with("/provider") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 1 {
                let p = parts[1];
                if let Some(preset) = interactive::find_provider(p) {
                    config.base_url = preset.base_url.to_string();
                    config.model = preset.default_model.to_string();
                    user_prefs.provider = Some(preset.id.to_string());
                    user_prefs.base_url = Some(config.base_url.clone());
                    user_prefs.model = Some(config.model.clone());
                    let _ = user_prefs.save();

                    let mut locked = command_agent(agent, active, current_session, history, config).await;
                    locked.reset(config.clone());
                    *current_session = Session::new(config.model.clone());
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched provider: {}\n  Endpoint: {}\n  Model:    {}\n", preset.name, config.base_url, config.model);
                } else {
                    clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Unknown provider: {}. Available: groq, openrouter, openai, deepseek, ollama\n", p);
                }
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                if let Ok(Some(selected_provider)) = interactive::select_provider(&ctx, &config.base_url) {
                    config.base_url = selected_provider.base_url.clone();
                    config.model = selected_provider.default_model.clone();
                    if let Some(key) = selected_provider.api_key {
                        config.api_key = key.clone();
                        user_prefs.api_key = Some(key);
                    }
                    user_prefs.provider = Some(selected_provider.id.clone());
                    user_prefs.base_url = Some(config.base_url.clone());
                    user_prefs.model = Some(config.model.clone());
                    let _ = user_prefs.save();

                    let mut locked = command_agent(agent, active, current_session, history, config).await;
                    locked.reset(config.clone());
                    *current_session = Session::new(config.model.clone());
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched provider: {}\n  Endpoint: {}\n  Model:    {}\n", selected_provider.name, config.base_url, config.model);
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            }
            return Ok(CommandResult::Handled);
        }

        if line == "/status" {
            let msg_count = agent.try_lock().map(|a| a.message_count()).unwrap_or_else(|_| current_session.message_count());
            let ctx = interactive::ScreenContext::from_config(&config);
            if let Ok(Some(action)) = interactive::show_interactive_status(&ctx, &config, &current_session, msg_count, &branch_tag) {
                match action {
                    interactive::StatusAction::ChangeProvider => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        if let Ok(Some(selected_provider)) = interactive::select_provider(&cur_ctx, &config.base_url) {
                            config.base_url = selected_provider.base_url.clone();
                            config.model = selected_provider.default_model.clone();
                            if let Some(key) = selected_provider.api_key {
                                config.api_key = key.clone();
                                user_prefs.api_key = Some(key);
                            }
                            user_prefs.provider = Some(selected_provider.id.clone());
                            user_prefs.base_url = Some(config.base_url.clone());
                            user_prefs.model = Some(config.model.clone());
                            let _ = user_prefs.save();

                            let mut locked = command_agent(agent, active, current_session, history, config).await;
                            locked.reset(config.clone());
                            *current_session = Session::new(config.model.clone());
                        }
                    }
                    interactive::StatusAction::ChangeModel => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        if let Ok(Some(chosen_model)) = interactive::select_model(&cur_ctx, &config.base_url, &config.model) {
                            config.model = chosen_model.clone();
                            user_prefs.model = Some(chosen_model);
                            let _ = user_prefs.save();
                            let mut locked = command_agent(agent, active, current_session, history, config).await;
                            locked.reset(config.clone());
                            *current_session = Session::new(config.model.clone());
                        }
                    }
                    interactive::StatusAction::ChangeMode => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        if let Ok(Some(chosen_mode)) = interactive::select_mode_interactive(&cur_ctx, config.mode) {
                            match chosen_mode {
                                theme::AppMode::Manual => {
                                    if let Ok(Some(curated)) = interactive::select_curated_model_interactive(&cur_ctx, &config.model) {
                                        config.mode = theme::AppMode::Manual;
                                        config.model = curated.id.to_string();
                                        user_prefs.mode = Some("manual".to_string());
                                        user_prefs.model = Some(curated.id.to_string());
                                        let _ = user_prefs.save();
                                        let mut locked = command_agent(agent, active, current_session, history, config).await;
                                        locked.reset(config.clone());
                                        *current_session = Session::new(config.model.clone());
                                    }
                                }
                                theme::AppMode::MoA => {
                                    config.mode = theme::AppMode::MoA;
                                    user_prefs.mode = Some("moa".to_string());
                                    let _ = user_prefs.save();
                                }
                            }
                        }
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
                        *current_session = Session::new(config.model.clone());
                        cli_ui::set_active_chat_title("");
                        history.lock().unwrap().clear();
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
            *current_session = Session::new(config.model.clone());
            cli_ui::set_active_chat_title("");
            history.lock().unwrap().clear();
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            println!("Conversation reset. Started a new session.\n");
            return Ok(CommandResult::Handled);
        }

        if line == "/history" {
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            let sess_dir = Session::sessions_dir(&config.workspace_dir);
            let md_path = sess_dir.join(format!("{}.md", current_session.id));
            println!("Current Session: {}", current_session.id);
            println!("Created:         {}", current_session.created_at);
            println!("Model:           {}", current_session.model);
            println!("Messages:        {}", current_session.message_count());
            if md_path.exists() {
                println!("Markdown Log:    {}", md_path.display());
            }
            let session_ids = Session::list(&config.workspace_dir);
            println!("\nSaved Sessions ({}):", session_ids.len());
            for id in session_ids.iter().take(8) {
                let is_cur = if id == &current_session.id { " (current)" } else { "" };
                if let Some(s) = Session::load(&config.workspace_dir, id) {
                    println!("  • {} | {:2} msgs | \"{}\"{}", s.id, s.message_count(), s.title(), is_cur);
                } else {
                    println!("  • {}{}", id, is_cur);
                }
            }
            if session_ids.len() > 8 {
                println!("  ... and {} more (use /sessions to browse)", session_ids.len() - 8);
            }
            println!("\nTip: Use /sessions to interactively resume or delete past sessions.\n");
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
                    println!("Session {} not found.\n", id);
                }
            } else {
                let sessions = Session::list(&config.workspace_dir);
                if sessions.is_empty() {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("No saved chat sessions found in .takiza/sessions/\n");
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
                            println!("Deleted session {}.\n", id);
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
    println!("Unknown command: {line}. Use /help to see available commands.");
    Ok(CommandResult::Handled)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli_args = config::CliArgs::parse();

    if cli_args.show_help {
        println!("Takiza Code v{} - Autonomous AI Software Engineering Agent\n", env!("TAKIZA_VERSION"));
        println!("USAGE:");
        println!("    takiza [OPTIONS] [PROMPT]\n");
        println!("OPTIONS:");
        println!("    -c, --continue, --resume");
        println!("            Continue previous conversation session from where you left off\n");
        println!("    -y, --yes, -a, --auto-approve, --dangerously-skip-permissions");
        println!("            Skip asking for permission before executing commands (auto-approve)");
        println!("            (Default: ask user for permission before running each command)\n");
        println!("    -h, --help");
        println!("            Print help information\n");
        println!("    -v, -V, --version");
        println!("            Print version information\n");
        println!("ARGS:");
        println!("    [PROMPT]");
        println!("            Optional initial prompt to send to the agent\n");
        return Ok(());
    }

    if cli_args.show_version {
        println!("takiza {}", env!("TAKIZA_VERSION"));
        return Ok(());
    }

    let _terminal_screen = cli_ui::TerminalScreen::enter()?;

    // 1. Run onboarding wizard if user has not yet accepted terms
    let (mut user_prefs, onboarding_shown) = onboarding::Onboarding::run_if_needed()?;
    theme::set_current(user_prefs.theme);

    // If onboarding was not shown, purge previous terminal output (e.g. `cargo run`).
    // If onboarding was shown, it already purged previous output on startup.
    if !onboarding_shown {
        let _ = execute!(stdout(), Clear(ClearType::All), Clear(ClearType::Purge), cursor::MoveTo(0, 0));
    }

    let mut config = Config::load_with_args(&cli_args);
    let mut initial_prompt = cli_args.prompt;
    let mut line_editor = LineEditor::new();
    let agent = Arc::new(Mutex::new(Agent::new(config.clone())));
    let tools_executor = Arc::new(ToolExecutor::new(config.workspace_dir.clone()));

    // Spawn a fresh new chat session on startup or continue previous session
    let mut current_session = Session::new(config.model.clone());
    cli_ui::set_active_chat_title("");
    let history = Arc::new(StdMutex::new(Vec::new()));

    let output_row = Arc::new(StdMutex::new(0));

    if config.continue_session {
        if let Some(loaded) = Session::latest(&config.workspace_dir) {
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
            println!("  ℹ  No previous session found in .takiza/sessions. Started a new session.\n");
        }
    } else {
        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
    }

    let mut prompt_queue = PromptQueue::default();
    let mut ready_commands = std::collections::VecDeque::new();
    let mut pending_title: PendingTitle = None;
    let mut title_attempted_sessions = std::collections::HashSet::new();
    'chat: loop {
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
                            format!("{} (dirty)", b)
                        } else {
                            format!("{} (clean)", b)
                        }
                    }
                    None => "(no git)".to_string(),
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
            }, || collect_chat_title(&mut pending_title, &mut current_session, &config.workspace_dir, &history))?;

            match input_res {
                PromptResult::Exit => {
                    println!("Goodbye!");
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

        if line.starts_with('/') {
            let outcome = handle_tui_command(&line, &mut config, &mut user_prefs, &agent,
                &mut current_session, &history, &output_row, &branch_tag, &mut None).await?;
            if pending_title.as_ref().is_some_and(|(id, _)| *id != current_session.id) {
                if let Some((_, task)) = pending_title.take() { task.abort(); }
            }
            match outcome {
                CommandResult::Exit => break,
                CommandResult::InsertSkill(skill) => line_editor.draft.insert_text(&skill.prompt_reference()),
                CommandResult::Handled => {}
            }
            continue;
        }

        // Direct shell command (!<command>)
        if line.starts_with('!') {
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

            let sig_handle = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_clone.cancel();
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

            sig_handle.abort();
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
        let workspace = config.workspace_dir.clone();
        let mut conversation = current_session.clone();
        conversation.messages = agent.lock().await.get_messages().to_vec();
        conversation.history = history.lock().unwrap().clone();
        let label = line.clone();

        cli_ui::scroll_history(i32::MIN);
        // Autonomous AI Agent Execution
        history.lock().unwrap().push(cli_ui::HistoryItem::UserPrompt(line.clone()));
        {
            *output_row.lock().unwrap() = redraw_full_screen(&config, &history.lock().unwrap());
        }

        if config.mode == theme::AppMode::MoA {
            let decision = moa_router::resolve_moa_route(&line, None).await;
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
        let mut thinking_message = cli_ui::random_thinking_message();
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
        let agent_task = tokio::spawn(async move {
            // Keep the editor, spinner and cancellation alive while scanning and
            // copying the workspace. Previously Enter blocked the UI here.
            let checkpoint_cancel = cancel_token_clone.clone();
            let capture = tokio::task::spawn_blocking(move ||
                checkpoints::capture_session_cancellable(&workspace, &conversation, &label, checkpoint_cancel)).await;
            if cancel_token_clone.is_cancelled() {
                let _ = event_tx.send(AgentEvent::Interrupted).await;
                let _ = event_tx.send(AgentEvent::Finished).await;
                return;
            }
            let error = match capture { Ok(Ok(_)) => None, Ok(Err(error)) => Some(format!("{error:#}")), Err(error) => Some(error.to_string()) };
            if let Some(error) = error {
                let _ = event_tx.send(AgentEvent::ToolLog(format!("Checkpoint unavailable: {error}"))).await;
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
        // Event processing loop
        loop {
            let event = tokio::select! {
                _ = title_tick.tick(), if pending_title.is_some() => {
                    if collect_chat_title(&mut pending_title, &mut current_session, &config.workspace_dir, &history) {
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
                                if submitted.starts_with('/') {
                                    sig_handle.stop().await;
                                    let status = spinner.as_ref().map(|s| s.message());
                                    if let Some(s) = spinner.take() { s.stop().await; }
                                    cli_ui::set_active_draft(None);
                                    crossterm::terminal::disable_raw_mode().ok();
                                    let outcome = handle_tui_command(&submitted, &mut config, &mut user_prefs,
                                        &agent, &mut current_session, &history, &output_row, &branch_tag, &mut active).await?;
                                    crossterm::terminal::enable_raw_mode().ok();
                                    exit_requested = outcome == CommandResult::Exit;
                                    if let CommandResult::InsertSkill(skill) = outcome {
                                        line_editor.draft.insert_text(&skill.prompt_reference());
                                    }
                                    interrupted_by_command = active.as_ref().is_some_and(|a| a.task.is_none());
                                    if exit_requested || interrupted_by_command {
                                        if exit_requested { active.as_mut().unwrap().stop(&history).await; }
                                        // Partial output belongs to the previous session, never replay it into a new one.
                                        stream_writer = None;
                                        if current_session.id != turn_session_id || exit_requested || matches!(submitted.split_whitespace().next(), Some("/rewind" | "/restore")) {
                                            prompt_queue.clear(&mut history.lock().unwrap());
                                        }
                                        cli_ui::set_pending_prompt_count(prompt_queue.pending.len());
                                        break;
                                    }
                                    // Plain command output remains readable until the user returns to the chat.
                                    let name = submitted.split_whitespace().next().unwrap_or("");
                                    if matches!(name, "/help" | "/history") || !prompt::SLASH_COMMANDS.iter().any(|c| c.name == name) {
                                        println!("Press any key to return to the response.");
                                        loop {
                                            if matches!(crossterm::event::read()?, crossterm::event::Event::Key(_)) { break; }
                                        }
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
                AgentEvent::StatusUpdate(status) => {
                    if status == "Thinking..." {
                        thought_index = None;
                        thinking_message = cli_ui::random_thinking_message();
                        if let Some(s) = spinner.take() { s.stop().await; }
                        spinner = Some(cli_ui::Spinner::start_with_box(
                            thinking_message, "", &branch_tag, output_row.clone(),
                        ));
                    }
                }
                AgentEvent::PermissionRequest { command, responder, .. } => {
                    if cancel_token.is_cancelled() {
                        if let Some(tx) = responder.lock().unwrap().take() { let _ = tx.send(agent::PermissionResponse::Deny); }
                        continue;
                    }
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    stream_writer.take();

                    sig_handle.stop().await;
                    cli_ui::invalidate_content_frame();
                    cli_ui::set_active_draft(None);

                    let choice = {
                        let mut row = output_row.lock().unwrap();
                        let snapshot = history.lock().unwrap().clone();
                        cli_ui::ask_command_permission_with_redraw(&command, &mut *row, &branch_tag,
                            |row| *row = redraw_full_screen(&config, &snapshot))
                    };

                    let status_log = match choice {
                        cli_ui::PermissionChoice::AllowOnce => "approval allowed",
                        cli_ui::PermissionChoice::AllowAlways => "approval allowed for session",
                        cli_ui::PermissionChoice::Deny => "approval denied",
                    };
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(status_log.to_string()));

                    let response = match choice {
                        cli_ui::PermissionChoice::AllowOnce => {
                            agent::PermissionResponse::AllowOnce
                        }
                        cli_ui::PermissionChoice::AllowAlways => {
                            config.auto_approve = true;
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
                    stream_writer.take();

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
                    if let Some(s) = spinner.take() {
                        s.stop().await;
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
                    cli_ui::update_streamed_thought(&mut history.lock().unwrap(), &mut thought_index, &thought, true);
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
                    stream_writer.take();
                    if let Some(s) = spinner.take() { s.stop().await; }
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolStart { name, args });
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                    spinner = Some(cli_ui::Spinner::start_with_box(
                        "Executing tools...", "", &branch_tag, output_row.clone(),
                    ));
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
                    if is_error {
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
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    stream_writer.take();
                    history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(msg));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::Interrupted => {
                    if let Some(s) = spinner.take() { s.stop().await; }
                    if let Some(sw) = stream_writer.take() {
                        let partial = sw;
                        if !partial.trim().is_empty() {
                            history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(partial));
                        }
                    }
                    let message = "⏸ Stopped by user";
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(message.to_string()));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::Error(err) => {
                    crate::logger::log_error("Agent", &err);
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    stream_writer.take();
                    history.lock().unwrap().push(cli_ui::HistoryItem::Error(err));
                    let snapshot = history.lock().unwrap().clone();
                    *output_row.lock().unwrap() = redraw_full_screen(&config, &snapshot);
                }
                AgentEvent::Finished => {
                    stream_writer.take();
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

        stream_writer.take();
        if let Some(s) = spinner.take() {
            s.stop().await;
        }

        sig_handle.stop().await;
        if let Some(task) = active.as_mut().unwrap().task.take() { let _ = task.await; }
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
        crossterm::terminal::disable_raw_mode().ok();
        if pending_title.as_ref().is_some_and(|(id, _)| *id != current_session.id) {
            if let Some((_, task)) = pending_title.take() { task.abort(); }
        }
        if interrupted_by_command || exit_requested {
            if exit_requested { println!("Goodbye!"); break 'chat; }
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
        tokio::time::timeout(std::time::Duration::from_secs(2), active.stop(&Arc::new(StdMutex::new(Vec::new())))).await.unwrap();
        assert!(cancel.is_cancelled());
        assert!(active.task.is_none());
    }
}
