mod agent;
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

use agent::{Agent, AgentEvent};
use config::Config;
use crossterm::{
    cursor,
    execute,
    queue,
    style::Color,
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

fn clear_screen_and_banner(config: &Config) -> u16 {
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
    let _ = queue!(out, cursor::Hide, cursor::MoveTo(0, 0));
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
    let current_git = GitInfo::get(&config.workspace_dir);
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Run onboarding wizard if user has not yet accepted terms
    let (mut user_prefs, onboarding_shown) = onboarding::Onboarding::run_if_needed()?;
    theme::set_current(user_prefs.theme);

    // If onboarding was not shown, purge previous terminal output (e.g. `cargo run`).
    // If onboarding was shown, it already purged previous output on startup.
    if !onboarding_shown {
        let _ = execute!(stdout(), Clear(ClearType::Purge));
    }

    let mut config = Config::load();
    let mut line_editor = LineEditor::new();
    let agent = Arc::new(Mutex::new(Agent::new(config.clone())));
    let tools_executor = Arc::new(ToolExecutor::new(config.workspace_dir.clone()));

    // Spawn a fresh new chat session on startup
    let mut current_session = Session::new(config.model.clone());
    cli_ui::set_active_chat_title("");
    let history = Arc::new(StdMutex::new(Vec::new()));

    let output_row = Arc::new(StdMutex::new(0));
    {
        *output_row.lock().unwrap() = clear_screen_and_banner(&config);
    }

    loop {
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

        let history_clone = history.clone();
        let config_clone = config.clone();
        let branch_tag_for_redraw = branch_tag.clone();
        let output_row_for_redraw = output_row.clone();

        let input_res = line_editor.read_line_with_redraw(&branch_tag, move |current_buffer| {
            let git_info = GitInfo::get(&config_clone.workspace_dir);
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
        })?;

        let line = match input_res {
            PromptResult::Exit => {
                println!("Goodbye!");
                break;
            }
            PromptResult::Interrupted => {
                println!("^C");
                continue;
            }
            PromptResult::Line(l) => l,
        };

        if line.is_empty() {
            continue;
        }

        // Handle slash commands
        if line == "/exit" || line == "/quit" {
            println!("Goodbye!");
            break;
        }

        if line == "/clear" {
            history.lock().unwrap().clear();
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            continue;
        }

        if line == "/help" {
            clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            cli_ui::print_help();
            continue;
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
            continue;
        }

        if line == "/tools" {
            let ctx = interactive::ScreenContext::from_config(&config);
            interactive::show_interactive_tools(&ctx)?;
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            continue;
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
            continue;
        }

        if line.starts_with("/model") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 1 {
                config.model = parts[1].to_string();
                user_prefs.model = Some(config.model.clone());
                let _ = user_prefs.save();
                let mut locked = agent.lock().await;
                locked.reset(config.clone());
                current_session = Session::new(config.model.clone());
                *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                cli_ui::print_user_cmd(&line);
                println!("Switched model to: {}\n", config.model);
            } else {
                let ctx = interactive::ScreenContext::from_config(&config);
                if let Ok(Some(chosen_model)) = interactive::select_model(&ctx, &config.base_url, &config.model) {
                    config.model = chosen_model.clone();
                    user_prefs.model = Some(chosen_model);
                    let _ = user_prefs.save();
                    let mut locked = agent.lock().await;
                    locked.reset(config.clone());
                    current_session = Session::new(config.model.clone());
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched model to: {}\n", config.model);
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            }
            continue;
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

                    let mut locked = agent.lock().await;
                    locked.reset(config.clone());
                    current_session = Session::new(config.model.clone());
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

                    let mut locked = agent.lock().await;
                    locked.reset(config.clone());
                    current_session = Session::new(config.model.clone());
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                    cli_ui::print_user_cmd(&line);
                    println!("Switched provider: {}\n  Endpoint: {}\n  Model:    {}\n", selected_provider.name, config.base_url, config.model);
                } else {
                    *output_row.lock().unwrap() = clear_screen_and_banner(&config);
                }
            }
            continue;
        }

        if line == "/status" {
            let msg_count = {
                let locked = agent.lock().await;
                locked.message_count()
            };
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

                            let mut locked = agent.lock().await;
                            locked.reset(config.clone());
                            current_session = Session::new(config.model.clone());
                        }
                    }
                    interactive::StatusAction::ChangeModel => {
                        let cur_ctx = interactive::ScreenContext::from_config(&config);
                        if let Ok(Some(chosen_model)) = interactive::select_model(&cur_ctx, &config.base_url, &config.model) {
                            config.model = chosen_model.clone();
                            user_prefs.model = Some(chosen_model);
                            let _ = user_prefs.save();
                            let mut locked = agent.lock().await;
                            locked.reset(config.clone());
                            current_session = Session::new(config.model.clone());
                        }
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
                        let mut locked = agent.lock().await;
                        locked.reset(config.clone());
                        current_session = Session::new(config.model.clone());
                        cli_ui::set_active_chat_title("");
                        history.lock().unwrap().clear();
                    }
                }
            }
            let hist = history.lock().unwrap().clone();
            *output_row.lock().unwrap() = redraw_full_screen(&config, &hist);
            continue;
        }

        if line == "/reset" || line == "/new" {
            if !current_session.messages.is_empty() {
                current_session.history = history.lock().unwrap().clone();
                let _ = current_session.save(&config.workspace_dir);
            }
            let mut locked = agent.lock().await;
            locked.reset(config.clone());
            current_session = Session::new(config.model.clone());
            cli_ui::set_active_chat_title("");
            history.lock().unwrap().clear();
            *output_row.lock().unwrap() = clear_screen_and_banner(&config);
            cli_ui::print_user_cmd(&line);
            println!("Conversation reset. Started a new session.\n");
            continue;
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
            continue;
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
                    let mut locked = agent.lock().await;
                    locked.set_messages(loaded.messages.clone());
                    let restored_history = loaded.build_history_from_messages();
                    current_session = loaded;
                    let _ = current_session.save(&config.workspace_dir);
                    cli_ui::set_active_chat_title(&current_session.title());
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
                            let mut locked = agent.lock().await;
                            locked.set_messages(resumed.messages.clone());
                            let restored_history = resumed.build_history_from_messages();
                            current_session = resumed;
                            let _ = current_session.save(&config.workspace_dir);
                            cli_ui::set_active_chat_title(&current_session.title());
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
                cli_ui::print_user_prompt_at(&line, &mut *row, &branch_tag);
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
                        log_count += 1;
                        history_clone.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(l.clone()));
                        if cli_ui::is_output_expanded() || log_count <= 5 {
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

            {
                let mut row = output_row.lock().unwrap();
                cli_ui::print_tool_end_at("run_command", &cmd, &out_text, is_err, &mut *row, "", &branch_tag);
            }
            history.lock().unwrap().push(cli_ui::HistoryItem::ToolEnd {
                name: "run_command".to_string(),
                args: cmd,
                result: out_text,
                is_error: is_err,
            });
            continue;
        }

        // If this session does not have an AI-generated title yet, spawn background task to generate one
        let title_task = if current_session.title.is_none() {
            let config_clone = config.clone();
            let prompt_for_title = line.clone();
            Some(tokio::spawn(async move {
                let client = crate::llm::LlmClient::new(config_clone);
                client.generate_title(&prompt_for_title).await
            }))
        } else {
            None
        };

        // Autonomous AI Agent Execution
        history.lock().unwrap().push(cli_ui::HistoryItem::UserPrompt(line.clone()));
        {
            let mut row = output_row.lock().unwrap();
            cli_ui::print_user_prompt_at(&line, &mut *row, &branch_tag);
        }

        // Keep raw mode enabled during preparation/execution so typed keys cannot corrupt display
        crossterm::terminal::enable_raw_mode().ok();

        let cancel_token = CancellationToken::new();
        let cancel_clone = cancel_token.clone();
        let branch_tag_clone = branch_tag.clone();

        let sig_handle = tokio::spawn(async move {
            loop {
                let has_event = tokio::task::spawn_blocking(|| {
                    crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap_or(false)
                })
                .await
                .unwrap_or(false);

                if has_event {
                    if let Ok(Ok(ev)) = tokio::task::spawn_blocking(crossterm::event::read).await {
                        match ev {
                            crossterm::event::Event::Key(k) => {
                                if k.kind == crossterm::event::KeyEventKind::Press {
                                    if k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                        && k.code == crossterm::event::KeyCode::Char('c')
                                    {
                                        cancel_clone.cancel();
                                        break;
                                    }
                                    if k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                        && k.code == crossterm::event::KeyCode::Char('o')
                                    {
                                        cli_ui::toggle_expanded_output();
                                        let _ = cli_ui::render_bottom_box("", &branch_tag_clone);
                                    }
                                    // All other keys are silently discarded so user cannot type into field
                                }
                            }
                            crossterm::event::Event::Resize(_, _) => {
                                let _ = cli_ui::render_bottom_box("", &branch_tag_clone);
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(100);
        let mut spinner = Some(cli_ui::Spinner::start_with_box(
            "Thinking...",
            "",
            &branch_tag,
            output_row.clone(),
        ));

        let agent_clone = agent.clone();
        let cancel_token_clone = cancel_token.clone();
        let line_clone = line.clone();
        let agent_task = tokio::spawn(async move {
            let mut locked = agent_clone.lock().await;
            locked
                .handle_user_input(line_clone, event_tx, cancel_token_clone)
                .await;
        });

        let mut stream_writer: Option<cli_ui::StreamWriter> = None;
        let mut accumulated_thought = String::new();
        let mut thought_printed = false;
        let mut tool_log_count = 0usize;
        let mut tool_hidden_count = 0usize;

        // Event processing loop
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::StatusUpdate(status) => {
                    let _ = status;
                }
                AgentEvent::ThoughtToken(token) => {
                    accumulated_thought.push_str(&token);
                }
                AgentEvent::AssistantToken(token) => {
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    if !thought_printed && !accumulated_thought.trim().is_empty() {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_thought_at(&accumulated_thought, &mut *row, "", &branch_tag);
                        history.lock().unwrap().push(cli_ui::HistoryItem::Thought(accumulated_thought.clone()));
                        thought_printed = true;
                    }
                    accumulated_thought.clear();
                    if stream_writer.is_none() {
                        stream_writer = Some(cli_ui::StreamWriter::new(
                            output_row.clone(),
                            "🤖 ",
                            Color::Green,
                            "   ",
                            "",
                            &branch_tag,
                        ));
                    }
                    if let Some(sw) = stream_writer.as_mut() {
                        sw.write_token(&token);
                    }
                }
                AgentEvent::AssistantThought(thought) => {
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    if !thought_printed {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_thought_at(&thought, &mut *row, "", &branch_tag);
                        history.lock().unwrap().push(cli_ui::HistoryItem::Thought(thought));
                        thought_printed = true;
                    }
                    accumulated_thought.clear();
                    spinner = Some(cli_ui::Spinner::start_with_box(
                        "Executing tools...",
                        "",
                        &branch_tag,
                        output_row.clone(),
                    ));
                }
                AgentEvent::ToolStart { name, args, .. } => {
                    tool_log_count = 0;
                    tool_hidden_count = 0;
                    if let Some(sw) = stream_writer.take() {
                        sw.finish();
                    }
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    if !thought_printed && !accumulated_thought.trim().is_empty() {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_thought_at(&accumulated_thought, &mut *row, "", &branch_tag);
                        history.lock().unwrap().push(cli_ui::HistoryItem::Thought(accumulated_thought.clone()));
                        thought_printed = true;
                    }
                    accumulated_thought.clear();
                    {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_tool_start_at(&name, &args, &mut *row, "", &branch_tag);
                    }
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolStart {
                        name,
                        args,
                    });
                }
                AgentEvent::ToolLog(log_item) => {
                    tool_log_count += 1;
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolLog(log_item.clone()));
                    if cli_ui::is_output_expanded() || tool_log_count <= 5 {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_tool_log_at(&log_item, &mut *row, "", &branch_tag);
                    } else {
                        tool_hidden_count += 1;
                    }
                }
                AgentEvent::ToolEnd { name, args, result, is_error, .. } => {
                    thought_printed = false;
                    if is_error {
                        crate::logger::log_warn("Tool", &format!("{name} failed (args: {args}): {result}"));
                    }
                    {
                        let mut row = output_row.lock().unwrap();
                        if tool_hidden_count > 0 {
                            cli_ui::print_tool_collapsed_indicator_at(tool_hidden_count, &mut *row, "", &branch_tag);
                        }
                        cli_ui::print_tool_end_at(&name, &args, &result, is_error, &mut *row, "", &branch_tag);
                    }
                    history.lock().unwrap().push(cli_ui::HistoryItem::ToolEnd {
                        name,
                        args,
                        result,
                        is_error,
                    });
                    spinner = Some(cli_ui::Spinner::start_with_box(
                        "Thinking...",
                        "",
                        &branch_tag,
                        output_row.clone(),
                    ));
                }
                AgentEvent::AssistantMessage(msg) => {
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    if let Some(sw) = stream_writer.take() {
                        sw.finish_and_replace(&msg);
                    } else {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_assistant_message_at(&msg, &mut *row, "", &branch_tag);
                    }
                    history.lock().unwrap().push(cli_ui::HistoryItem::AssistantMessage(msg));
                }
                AgentEvent::Error(err) => {
                    crate::logger::log_error("Agent", &err);
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    if let Some(sw) = stream_writer.take() {
                        sw.finish();
                    }
                    {
                        let mut row = output_row.lock().unwrap();
                        cli_ui::print_error_at(&err, &mut *row, "", &branch_tag);
                    }
                    history.lock().unwrap().push(cli_ui::HistoryItem::Error(err));
                }
                AgentEvent::Finished => {
                    if let Some(sw) = stream_writer.take() {
                        sw.finish();
                    }
                    if let Some(s) = spinner.take() {
                        s.stop().await;
                    }
                    break;
                }
                AgentEvent::UserMessage(_) => {}
            }
        }

        if let Some(sw) = stream_writer.take() {
            sw.finish();
        }
        if let Some(s) = spinner.take() {
            s.stop().await;
        }

        sig_handle.abort();
        let _ = agent_task.await;

        // Drain any stray key events so they don't leak into the next prompt
        while crossterm::event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
            let _ = crossterm::event::read();
        }
        crossterm::terminal::disable_raw_mode().ok();

        // If AI title was generated in background, save it into the session
        if let Some(handle) = title_task {
            if let Ok(Some(generated_title)) = handle.await {
                if !generated_title.trim().is_empty() {
                    let t = generated_title.trim().to_string();
                    cli_ui::set_active_chat_title(&t);
                    current_session.title = Some(t);
                }
            }
        }

        // Auto-save session transcript
        let locked = agent.lock().await;
        current_session.messages = locked.get_messages().to_vec();
        current_session.history = history.lock().unwrap().clone();
        let _ = current_session.save(&config.workspace_dir);

        // Turn is finished: redraw full screen cleanly with completed turn in history
        let hist = history.lock().unwrap().clone();
        *output_row.lock().unwrap() = redraw_full_screen(&config, &hist);
    }

    Ok(())
}
