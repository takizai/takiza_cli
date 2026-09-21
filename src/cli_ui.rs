use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::{cursor, execute, queue};
use serde::{Deserialize, Serialize};
use std::io::{stdout, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use crate::prompt::{char_width, str_width};

static ACTIVE_CHAT_TITLE: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());
static EXPAND_COMMAND_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn is_output_expanded() -> bool {
    EXPAND_COMMAND_OUTPUT.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn toggle_expanded_output() -> bool {
    let current = EXPAND_COMMAND_OUTPUT.load(std::sync::atomic::Ordering::Relaxed);
    EXPAND_COMMAND_OUTPUT.store(!current, std::sync::atomic::Ordering::Relaxed);
    !current
}

#[allow(dead_code)]
pub fn set_output_expanded(val: bool) {
    EXPAND_COMMAND_OUTPUT.store(val, std::sync::atomic::Ordering::Relaxed);
}

pub fn set_active_chat_title(title: &str) {
    if let Ok(mut lock) = ACTIVE_CHAT_TITLE.write() {
        *lock = title.to_string();
    }
}

pub fn get_active_chat_title() -> String {
    ACTIVE_CHAT_TITLE.read().map(|g| g.clone()).unwrap_or_default()
}

pub fn get_box_width() -> usize {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    (term_cols as usize).max(36)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else if max_len > 1 {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{}…", truncated)
    } else {
        s.chars().take(max_len).collect()
    }
}

pub fn print_banner_to(out: &mut impl Write, model: &str, base_url: &str, workspace: &str, git_info: &str) -> u16 {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut row: u16 = 0;

    let _ = queue!(out, cursor::MoveTo(0, row), crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine));
    row += 1;

    let th = crate::theme::current();
    let p_ansi = th.primary_ansi();

    if term_cols >= 80 {
        let info_w = (term_cols as usize).saturating_sub(32).min(50);
        let div_w = info_w.min(48);
        let divider = "─".repeat(div_w);

        let info_lines = [
            format!("{}TAKIZA \x1b[1;38;2;245;245;250mCODE\x1b[0m  \x1b[38;2;120;120;125mv{}\x1b[0m", p_ansi, env!("CARGO_PKG_VERSION")),
            format!("\x1b[38;2;160;160;165m{}\x1b[0m", truncate_str("Autonomous AI Software Engineering Agent", info_w)),
            format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider),
            format!("{}Model     \x1b[38;2;70;70;75m│\x1b[0m \x1b[38;2;230;230;235m{}\x1b[0m", p_ansi, truncate_str(model, info_w.saturating_sub(12))),
            format!("{}Endpoint  \x1b[38;2;70;70;75m│\x1b[0m \x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, truncate_str(base_url, info_w.saturating_sub(12))),
            format!("{}Workspace \x1b[38;2;70;70;75m│\x1b[0m \x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, truncate_str(workspace, info_w.saturating_sub(12))),
            format!("{}Git       \x1b[38;2;70;70;75m│\x1b[0m \x1b[38;2;130;215;145m{}\x1b[0m", p_ansi, truncate_str(git_info, info_w.saturating_sub(12))),
            format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider),
        ];

        for i in 0..crate::logo::LOGO_HEIGHT {
            let logo_part = crate::logo::LOGO_LINES[i];
            let info_part = if i < info_lines.len() { &info_lines[i] } else { "" };
            let _ = queue!(
                out,
                cursor::MoveTo(0, row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(format!(" {}  {}", logo_part, info_part))
            );
            row += 1;
        }
    } else {
        // Compact banner for narrower terminals (< 80 columns)
        let box_w = (term_cols as usize).saturating_sub(4).max(30);
        let inner_w = box_w.saturating_sub(4);
        let val_w = inner_w.saturating_sub(11);
        let top_div_len = box_w.saturating_sub(23);
        let top_div = "─".repeat(top_div_len);
        let bot_div = "─".repeat(box_w.saturating_sub(2));

        let lines = [
            format!("  {}╭─ TAKIZA \x1b[38;2;245;245;250mCODE\x1b[0m \x1b[38;2;120;120;125mv{}{} {}╮\x1b[0m", p_ansi, env!("CARGO_PKG_VERSION"), p_ansi, top_div),
            format!("  {}│\x1b[0m {}Model:     \x1b[0m\x1b[38;2;230;230;235m{}\x1b[0m", p_ansi, p_ansi, truncate_str(model, val_w)),
            format!("  {}│\x1b[0m {}Endpoint:  \x1b[0m\x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, p_ansi, truncate_str(base_url, val_w)),
            format!("  {}│\x1b[0m {}Workspace: \x1b[0m\x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, p_ansi, truncate_str(workspace, val_w)),
            format!("  {}│\x1b[0m {}Git:       \x1b[0m\x1b[38;2;130;215;145m{}\x1b[0m", p_ansi, p_ansi, truncate_str(git_info, val_w)),
            format!("  {}╰{}╯\x1b[0m", p_ansi, bot_div),
        ];

        for l in lines {
            let _ = queue!(
                out,
                cursor::MoveTo(0, row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(l)
            );
            row += 1;
        }
    }

    let _ = queue!(out, cursor::MoveTo(0, row), crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine));
    row += 1;

    let max_cmd_w = (term_cols as usize).saturating_sub(6);
    let cmd_str = "Commands: /help, /theme, /model, /provider, /diff, /reset, /exit";
    if term_cols >= 75 {
        let _ = queue!(
            out,
            cursor::MoveTo(0, row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(format!("  \x1b[38;2;160;160;165mCommands: \x1b[38;2;240;240;240m{}\x1b[0m", truncate_str(&cmd_str[10..], max_cmd_w.saturating_sub(10))))
        );
        row += 1;
        let _ = queue!(
            out,
            cursor::MoveTo(0, row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(format!("  \x1b[38;2;160;160;165mDirect:   {}!<command>\x1b[0m \x1b[38;2;100;100;105m(e.g. !ls)\x1b[0m   \x1b[38;2;70;70;75m•\x1b[0m   \x1b[38;2;160;160;165mExpand: \x1b[38;2;130;215;145mCtrl+O\x1b[0m   \x1b[38;2;70;70;75m•\x1b[0m   \x1b[38;2;160;160;165mCancel: \x1b[38;2;255;100;100mCtrl+C\x1b[0m", p_ansi))
        );
        row += 1;
    } else if term_cols >= 62 {
        let _ = queue!(
            out,
            cursor::MoveTo(0, row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(format!("  \x1b[38;2;160;160;165mCommands: \x1b[38;2;240;240;240m{}\x1b[0m", truncate_str(&cmd_str[10..], max_cmd_w.saturating_sub(10))))
        );
        row += 1;
        let _ = queue!(
            out,
            cursor::MoveTo(0, row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(format!("  \x1b[38;2;160;160;165mDirect: {}!<command>\x1b[0m \x1b[38;2;100;100;105m(!ls)\x1b[0m  •  \x1b[38;2;160;160;165mExpand: \x1b[38;2;130;215;145mCtrl+O\x1b[0m  •  \x1b[38;2;160;160;165mCancel: \x1b[38;2;255;100;100mCtrl+C\x1b[0m", p_ansi))
        );
        row += 1;
    } else {
        let _ = queue!(
            out,
            cursor::MoveTo(0, row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print("  \x1b[38;2;160;160;165m/help, /theme, /model, /reset  •  !<cmd> (e.g. !ls)  •  Ctrl+O expand\x1b[0m")
        );
        row += 1;
    }

    let _ = queue!(out, cursor::MoveTo(0, row), crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine));
    row += 1;

    row
}

pub fn print_banner(model: &str, base_url: &str, workspace: &str, git_info: &str) -> u16 {
    let mut out = stdout();
    let row = print_banner_to(&mut out, model, base_url, workspace, git_info);
    let _ = out.flush();
    row
}


pub fn print_user_cmd(cmd: &str) {
    let th = crate::theme::current();
    let mut out = stdout();
    execute!(out, SetForegroundColor(th.primary_crossterm())).ok();
    print!("❯ ");
    execute!(out, ResetColor).ok();
    println!("{}\n", cmd);
}

pub fn print_help() {
    let th = crate::theme::current();
    println!();
    execute!(stdout(), SetForegroundColor(th.primary_crossterm())).ok();
    println!("Available Slash Commands:");
    println!("  /help               - Show this help summary");
    println!("  /clear              - Clear the terminal screen");
    println!("  /theme [name]       - View or switch visual theme (amber, cyberpunk, emerald, nord, monochrome)");
    println!("  /model <name>       - Switch model (e.g. /model gpt-4o, /model claude-3-5-sonnet)");
    println!("  /provider <name>    - Switch preset provider (openai, openrouter, deepseek, ollama, groq)");
    println!("  /diff               - View git diff of changes made in the workspace");
    println!("  /status             - View current session info, git status, and token usage");
    println!("  /sessions           - List saved conversation sessions");
    println!("  /reset              - Reset conversation history to start fresh");
    println!("  /tools              - List available agent tools");
    println!("  /exit or /quit      - Exit Takiza Code");
    println!("  !<shell command>    - Execute bash command directly (e.g. !git status, !cargo test)");
    println!("  Ctrl+O              - Toggle expansion of command/search output (max 5 lines in standard mode)");
    execute!(stdout(), ResetColor).ok();
    println!();
}

#[allow(dead_code)]
pub fn print_themes() {
    let th = crate::theme::current();
    println!();
    execute!(stdout(), SetForegroundColor(th.primary_crossterm())).ok();
    println!("Visual Themes (use `/theme <name>`):");
    for t in crate::theme::Theme::all() {
        let is_curr = *t == th;
        let prefix = if is_curr { "  • \x1b[1m" } else { "    " };
        let suffix = if is_curr { " (active)\x1b[0m" } else { "" };
        println!("{}{} - {}{}", prefix, t.name(), t.description(), suffix);
    }
    execute!(stdout(), ResetColor).ok();
    println!();
}

#[allow(dead_code)]
pub fn print_tools() {
    println!();
    execute!(stdout(), SetForegroundColor(Color::Cyan)).ok();
    println!("Active Agent Tools:");
    println!("  • read_file(path, start_line, end_line)  - Read workspace file with line numbers");
    println!("  • write_file(path, content)             - Create or overwrite a file");
    println!("  • edit_file(path, target, replacement)  - Replace exact string block in file");
    println!("  • list_dir(path)                        - List directory entries & sizes");
    println!("  • find_files(pattern, path)             - Recursively search files by filename");
    println!("  • grep_search(query, path)              - Recursively search text inside files");
    println!("  • run_command(command)                  - Execute bash shell command in workspace");
    execute!(stdout(), ResetColor).ok();
    println!();
}

#[allow(dead_code)]
pub fn print_providers() {
    println!();
    execute!(stdout(), SetForegroundColor(Color::Cyan)).ok();
    println!("Preset Providers (use `/provider <name>`):");
    println!("  • openai      -> https://api.openai.com/v1          (model: gpt-4o)");
    println!("  • openrouter  -> https://openrouter.ai/api/v1       (model: anthropic/claude-3.5-sonnet)");
    println!("  • deepseek    -> https://api.deepseek.com           (model: deepseek-chat)");
    println!("  • ollama      -> http://localhost:11434/v1          (model: qwen2.5-coder:latest)");
    println!("  • groq        -> https://api.groq.com/openai/v1     (model: llama-3.3-70b-versatile)");
    execute!(stdout(), ResetColor).ok();
    println!();
}

#[allow(dead_code)]
pub fn print_diff(diff: &str) {
    println!();
    let mut out = stdout();
    if diff.trim().is_empty() {
        execute!(out, SetForegroundColor(Color::Green)).ok();
        println!("No changes (working tree clean).");
        execute!(out, ResetColor).ok();
        println!();
        return;
    }

    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            execute!(out, SetForegroundColor(Color::Green)).ok();
            println!("{}", line);
        } else if line.starts_with('-') && !line.starts_with("---") {
            execute!(out, SetForegroundColor(Color::Red)).ok();
            println!("{}", line);
        } else if line.starts_with("@@") {
            execute!(out, SetForegroundColor(Color::Cyan)).ok();
            println!("{}", line);
        } else if line.starts_with("diff --git") || line.starts_with("index ") {
            execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
            println!("{}", line);
        } else {
            execute!(out, ResetColor).ok();
            println!("{}", line);
        }
    }
    execute!(out, ResetColor).ok();
    println!();
}

pub fn render_bottom_box(prompt: &str, branch_tag: &str) -> (u16, u16) {
    let mut out = stdout();
    let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let box_width = get_box_width();
    let max_content = box_width.saturating_sub(6);

    let title_tag = if branch_tag.is_empty() {
        " You ".to_string()
    } else {
        format!(" You [{}] ", branch_tag.trim())
    };
    let title_w = str_width(&title_tag);
    let (safe_title, safe_title_w) = if title_w + 4 >= box_width {
        (" You ".to_string(), 5)
    } else {
        (title_tag, title_w)
    };
    let dashes_top = box_width.saturating_sub(safe_title_w + 3);

    // Split prompt into lines
    let chars: Vec<char> = prompt.chars().collect();
    let mut lines = Vec::new();
    if chars.is_empty() {
        lines.push(String::new());
    } else {
        let mut start = 0;
        while start < chars.len() {
            let mut cur_w = 0;
            let mut end = start;
            while end < chars.len() {
                let cw = char_width(chars[end]);
                if cur_w + cw > max_content && end > start {
                    break;
                }
                cur_w += cw;
                end += 1;
            }
            lines.push(chars[start..end].iter().collect::<String>());
            start = end;
        }
    }

    let box_rows = 1 + lines.len() + 1;
    let start_row = term_rows.saturating_sub(box_rows as u16);

    // Position strictly at bottom of the terminal window and clear downwards
    let _ = execute!(
        out,
        cursor::MoveTo(0, start_row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
    );

    let th = crate::theme::current();
    let border_color = th.border_crossterm();
    let primary_color = th.primary_crossterm();

    // 1. Top border
    let _ = execute!(
        out,
        cursor::MoveTo(0, start_row),
        SetForegroundColor(border_color),
        crossterm::style::Print("╭─"),
        SetForegroundColor(primary_color),
        crossterm::style::Print(&safe_title),
        SetForegroundColor(border_color),
        crossterm::style::Print("─".repeat(dashes_top)),
        crossterm::style::Print("╮"),
        ResetColor
    );

    // 2. Content lines
    for (i, line) in lines.iter().enumerate() {
        let prefix = if i == 0 { "> " } else { "  " };
        let line_w = str_width(line);
        let pad = max_content.saturating_sub(line_w);
        let row = start_row + 1 + i as u16;
        let _ = execute!(
            out,
            cursor::MoveTo(0, row),
            SetForegroundColor(border_color),
            crossterm::style::Print("│ "),
            SetForegroundColor(primary_color),
            crossterm::style::Print(prefix),
            ResetColor,
            crossterm::style::Print(line),
            crossterm::style::Print(" ".repeat(pad)),
            SetForegroundColor(border_color),
            crossterm::style::Print(" │"),
            ResetColor
        );
    }

    // 3. Bottom border - strictly no \r\n so it stays on the very bottom line
    let hint = if box_width >= 62 {
        if is_output_expanded() {
            " Ctrl+O: Collapse • Ctrl+C: Cancel "
        } else {
            " Ctrl+O: Expand • Ctrl+C: Cancel "
        }
    } else if box_width >= 50 {
        " Ctrl+C: Cancel "
    } else {
        " Ctrl+C "
    };
    let hint_w = str_width(hint);

    let chat_title = get_active_chat_title();
    let (title_part, title_vis) = if !chat_title.trim().is_empty() && box_width >= 55 {
        let max_title_w = box_width.saturating_sub(hint_w + 14).min(35);
        let safe_title = truncate_str(chat_title.trim(), max_title_w);
        let vis = str_width(" 💬 ") + str_width(&safe_title) + str_width("  •  ");
        (Some(safe_title), vis)
    } else {
        (None, 0)
    };

    let total_text_w = title_vis + hint_w;
    let dashes_bottom = box_width.saturating_sub(total_text_w + 3);
    let bottom_row = start_row + 1 + lines.len() as u16;
    let _ = execute!(
        out,
        cursor::MoveTo(0, bottom_row),
        SetForegroundColor(border_color),
        crossterm::style::Print("╰"),
        crossterm::style::Print("─".repeat(dashes_bottom)),
    );

    if let Some(ref t) = title_part {
        let _ = execute!(
            out,
            SetForegroundColor(Color::DarkGrey),
            crossterm::style::Print(" 💬 "),
            SetForegroundColor(primary_color),
            crossterm::style::Print(t),
            SetForegroundColor(Color::DarkGrey),
            crossterm::style::Print("  •  "),
        );
    }

    let _ = execute!(
        out,
        SetForegroundColor(Color::DarkGrey),
        crossterm::style::Print(hint),
        SetForegroundColor(border_color),
        crossterm::style::Print("─╯"),
        ResetColor
    );
    let _ = out.flush();

    let last_len = lines.last().map(|l| str_width(l)).unwrap_or(0);
    let cursor_x = 4 + last_len as u16;
    let cursor_y = start_row + lines.len() as u16;

    (cursor_x, cursor_y)
}

fn wrap_text_line(line: &str, max_width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = line.chars().collect();
    let mut result = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut cur_w = 0;
        let mut end = start;
        while end < chars.len() {
            let cw = char_width(chars[end]);
            if cur_w + cw > max_width && end > start {
                break;
            }
            cur_w += cw;
            end += 1;
        }
        result.push(chars[start..end].iter().collect());
        start = end;
    }
    result
}

pub fn prepare_output_line(row: &mut u16, needed: u16, prompt: &str, branch_tag: &str) -> u16 {
    prepare_output_line_internal(row, needed, prompt, branch_tag, true)
}

pub fn prepare_output_line_internal(row: &mut u16, needed: u16, prompt: &str, branch_tag: &str, render_bottom: bool) -> u16 {
    let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let box_width = get_box_width();
    let max_content = box_width.saturating_sub(6);
    let chars: Vec<char> = prompt.chars().collect();
    let lines_count = if chars.is_empty() {
        1
    } else {
        let mut count = 0;
        let mut start = 0;
        while start < chars.len() {
            let mut cur_w = 0;
            let mut end = start;
            while end < chars.len() {
                let cw = char_width(chars[end]);
                if cur_w + cw > max_content && end > start {
                    break;
                }
                cur_w += cw;
                end += 1;
            }
            count += 1;
            start = end;
        }
        count.max(1)
    };
    let box_rows = 1 + lines_count as u16 + 1;
    let limit = term_rows.saturating_sub(box_rows + 1);
    if *row + needed >= limit {
        let scroll = (*row + needed).saturating_sub(limit) + 1;
        let mut out = stdout();
        let start_row = term_rows.saturating_sub(box_rows as u16);
        let _ = execute!(
            out,
            cursor::MoveTo(0, start_row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown),
            crossterm::terminal::ScrollUp(scroll)
        );
        *row = row.saturating_sub(scroll);
        if render_bottom {
            render_bottom_box(prompt, branch_tag);
        }
        scroll
    } else {
        0
    }
}

pub struct Spinner {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
    output_row: Arc<Mutex<u16>>,
    cursor_x: u16,
    cursor_y: u16,
}

impl Spinner {
    pub fn start_with_box(
        message: &'static str,
        prompt: &str,
        branch_tag: &str,
        output_row: Arc<Mutex<u16>>,
    ) -> Self {
        {
            let mut row = output_row.lock().unwrap();
            prepare_output_line(&mut *row, 1, prompt, branch_tag);
        }
        let (cursor_x, cursor_y) = render_bottom_box(prompt, branch_tag);

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let row_arc = output_row.clone();
        let handle = tokio::spawn(async move {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            let mut out = stdout();
            loop {
                let frame = frames[i % frames.len()];
                let spinner_row = *row_arc.lock().unwrap();
                let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
                let cx = 4;
                let cy = term_rows.saturating_sub(2);
                execute!(
                    out,
                    cursor::Hide,
                    cursor::MoveTo(0, spinner_row),
                    SetForegroundColor(Color::Cyan),
                    crossterm::style::Print(format!("{frame} {message}")),
                    ResetColor,
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                    cursor::MoveTo(cx, cy),
                    cursor::Show
                ).ok();
                let _ = out.flush();

                tokio::select! {
                    _ = stop_rx.recv() => {
                        let spinner_row = *row_arc.lock().unwrap();
                        let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
                        let cx = 4;
                        let cy = term_rows.saturating_sub(2);
                        execute!(
                            out,
                            cursor::Hide,
                            cursor::MoveTo(0, spinner_row),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                            cursor::MoveTo(cx, cy),
                            cursor::Show
                        ).ok();
                        let _ = out.flush();
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(80)) => {
                        i += 1;
                    }
                }
            }
        });

        Self {
            stop_tx: Some(stop_tx),
            handle: Some(handle),
            output_row,
            cursor_x,
            cursor_y,
        }
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(()).await;
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }

        let spinner_row = *self.output_row.lock().unwrap();
        let mut out = stdout();
        let _ = execute!(
            out,
            cursor::Hide,
            cursor::MoveTo(0, spinner_row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            cursor::MoveTo(self.cursor_x, self.cursor_y),
            cursor::Show
        );
        let _ = out.flush();
    }
}

pub struct StreamWriter {
    output_row: Arc<Mutex<u16>>,
    start_row: u16,
    scrolls_occurred: u16,
    branch_tag: String,
    prompt: String,
    prefix: &'static str,
    prefix_color: Color,
    indent: &'static str,
    is_first_line: bool,
    at_line_start: bool,
    current_col: usize,
    wrap_cols: usize,
    in_bold: bool,
    in_code: bool,
    pending_chars: String,
}

impl StreamWriter {
    pub fn new(
        output_row: Arc<Mutex<u16>>,
        prefix: &'static str,
        prefix_color: Color,
        indent: &'static str,
        prompt: &str,
        branch_tag: &str,
    ) -> Self {
        let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let wrap_cols = (term_cols as usize).saturating_sub(6).max(20);
        let start_row = *output_row.lock().unwrap();

        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let mut out = stdout();
        let _ = execute!(out, cursor::MoveTo(cx, cy));
        let _ = out.flush();

        Self {
            output_row,
            start_row,
            scrolls_occurred: 0,
            branch_tag: branch_tag.to_string(),
            prompt: prompt.to_string(),
            prefix,
            prefix_color,
            indent,
            is_first_line: true,
            at_line_start: true,
            current_col: 0,
            wrap_cols,
            in_bold: false,
            in_code: false,
            pending_chars: String::new(),
        }
    }

    pub fn write_token(&mut self, token: &str) {
        if token.is_empty() {
            return;
        }

        let mut out = stdout();
        let _ = execute!(out, cursor::Hide);

        let mut row_guard = self.output_row.lock().unwrap();

        if self.is_first_line {
            self.scrolls_occurred += prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
            let _ = execute!(
                out,
                cursor::MoveTo(0, *row_guard),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                SetForegroundColor(self.prefix_color),
                crossterm::style::Print(self.prefix),
                ResetColor
            );
            if self.prefix == "💭 " {
                let _ = execute!(out, crossterm::style::Print("\x1b[3;38;2;190;150;210m"));
            }
            self.current_col = str_width(self.prefix);
            self.is_first_line = false;
            self.at_line_start = true;
        }

        self.pending_chars.push_str(token);

        let indent_w = str_width(self.indent);
        let th = crate::theme::current();
        let p_ansi = th.primary_ansi();

        while !self.pending_chars.is_empty() {
            // 1. Line-start bullet points
            if self.at_line_start {
                if self.pending_chars.starts_with("- ") || self.pending_chars.starts_with("* ") {
                    let _ = execute!(
                        out,
                        crossterm::style::Print(format!("{}•\x1b[0m ", p_ansi))
                    );
                    self.current_col += 2;
                    self.pending_chars.drain(..2);
                    self.at_line_start = false;
                    continue;
                } else if self.pending_chars == "-" || self.pending_chars == "*" {
                    break;
                }
            }

            // 2. Newline
            if self.pending_chars.starts_with('\r') {
                self.pending_chars.remove(0);
                continue;
            }

            if self.pending_chars.starts_with('\n') {
                self.pending_chars.remove(0);
                *row_guard += 1;
                self.scrolls_occurred += prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
                let _ = execute!(
                    out,
                    cursor::MoveTo(0, *row_guard),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                    crossterm::style::Print(self.indent)
                );
                if self.prefix == "💭 " {
                    let _ = execute!(out, crossterm::style::Print("\x1b[3;38;2;190;150;210m"));
                }
                if self.in_bold {
                    let _ = execute!(out, crossterm::style::Print("\x1b[1m"));
                }
                if self.in_code {
                    let _ = execute!(out, crossterm::style::Print("\x1b[48;2;38;38;44m\x1b[38;2;245;200;100m"));
                }
                self.current_col = indent_w;
                self.at_line_start = true;
                continue;
            }

            // 3. Bold delimiter **
            if self.pending_chars.starts_with("**") {
                self.pending_chars.drain(..2);
                if !self.in_bold {
                    self.in_bold = true;
                    let _ = execute!(out, crossterm::style::Print("\x1b[1m"));
                } else {
                    self.in_bold = false;
                    let _ = execute!(out, crossterm::style::Print("\x1b[22m"));
                }
                self.at_line_start = false;
                continue;
            }

            // 4. Inline code delimiter `
            if self.pending_chars.starts_with('`') && !self.pending_chars.starts_with("```") {
                self.pending_chars.remove(0);
                if !self.in_code {
                    self.in_code = true;
                    let _ = execute!(out, crossterm::style::Print("\x1b[48;2;38;38;44m\x1b[38;2;245;200;100m "));
                    self.current_col += 1;
                } else {
                    self.in_code = false;
                    let _ = execute!(out, crossterm::style::Print(" \x1b[0m"));
                    self.current_col += 1;
                }
                self.at_line_start = false;
                continue;
            }

            // 5. Whitespace (spaces, tabs)
            let first_char = self.pending_chars.chars().next().unwrap();
            if first_char.is_whitespace() && first_char != '\n' {
                let cw = char_width(first_char);
                self.pending_chars.remove(0);
                if self.current_col + cw <= self.wrap_cols {
                    let _ = execute!(out, crossterm::style::Print(first_char));
                    self.current_col += cw;
                } else {
                    *row_guard += 1;
                    self.scrolls_occurred += prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
                    let _ = execute!(
                        out,
                        cursor::MoveTo(0, *row_guard),
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                        crossterm::style::Print(self.indent)
                    );
                    self.current_col = indent_w;
                }
                self.at_line_start = false;
                continue;
            }

            // 6. Word: collect until whitespace, newline, or markdown delimiter
            let chars: Vec<char> = self.pending_chars.chars().collect();
            let mut word_end = 0;
            while word_end < chars.len() {
                let c = chars[word_end];
                if c.is_whitespace() || c == '*' || c == '`' {
                    break;
                }
                word_end += 1;
            }

            if word_end == 0 {
                let c = self.pending_chars.remove(0);
                let cw = char_width(c);
                let _ = execute!(out, crossterm::style::Print(c));
                self.current_col += cw;
                self.at_line_start = false;
                continue;
            }

            let is_word_complete = word_end < chars.len();
            let word_str: String = chars[..word_end].iter().collect();
            let word_w = str_width(&word_str);

            if is_word_complete {
                if self.current_col + word_w > self.wrap_cols && self.current_col > indent_w {
                    *row_guard += 1;
                    self.scrolls_occurred += prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
                    let _ = execute!(
                        out,
                        cursor::MoveTo(0, *row_guard),
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                        crossterm::style::Print(self.indent)
                    );
                    if self.in_bold {
                        let _ = execute!(out, crossterm::style::Print("\x1b[1m"));
                    }
                    if self.in_code {
                        let _ = execute!(out, crossterm::style::Print("\x1b[48;2;38;38;44m\x1b[38;2;245;200;100m"));
                    }
                    self.current_col = indent_w;
                }

                let _ = execute!(out, crossterm::style::Print(&word_str));
                self.current_col += word_w;
                self.pending_chars.drain(..word_str.len());
                self.at_line_start = false;
            } else {
                if self.current_col + word_w > self.wrap_cols {
                    if self.current_col > indent_w {
                        *row_guard += 1;
                        self.scrolls_occurred += prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
                        let _ = execute!(
                            out,
                            cursor::MoveTo(0, *row_guard),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                            crossterm::style::Print(self.indent)
                        );
                        self.current_col = indent_w;
                    } else {
                        let _ = execute!(out, crossterm::style::Print(&word_str));
                        self.current_col += word_w;
                        self.pending_chars.drain(..word_str.len());
                        self.at_line_start = false;
                    }
                }
                break;
            }
        }

        let _ = execute!(out, cursor::MoveTo(self.current_col as u16, *row_guard), cursor::Show);
        let _ = out.flush();
    }

    pub fn finish(self) {
        let mut row_guard = self.output_row.lock().unwrap();
        let mut out = stdout();
        let _ = execute!(out, ResetColor);
        if !self.is_first_line {
            *row_guard += 1;
            prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
            *row_guard += 1;
        }
        let (cx, cy) = render_bottom_box(&self.prompt, &self.branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }

    pub fn finish_and_replace(self, full_msg: &str) {
        let mut out = stdout();
        let _ = execute!(out, ResetColor);

        if self.is_first_line {
            if self.prefix == "💭 " {
                print_thought_at(full_msg, &mut *self.output_row.lock().unwrap(), &self.prompt, &self.branch_tag);
            } else {
                print_assistant_message_at(full_msg, &mut *self.output_row.lock().unwrap(), &self.prompt, &self.branch_tag);
            }
            return;
        }

        let can_replace = self.start_row >= self.scrolls_occurred;
        if can_replace {
            let actual_start_row = self.start_row - self.scrolls_occurred;
            let _ = execute!(
                out,
                cursor::MoveTo(0, actual_start_row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
            );
            *self.output_row.lock().unwrap() = actual_start_row;

            if self.prefix == "💭 " {
                print_thought_at(full_msg, &mut *self.output_row.lock().unwrap(), &self.prompt, &self.branch_tag);
            } else {
                print_assistant_message_at(full_msg, &mut *self.output_row.lock().unwrap(), &self.prompt, &self.branch_tag);
            }
        } else {
            let mut row_guard = self.output_row.lock().unwrap();
            *row_guard += 1;
            prepare_output_line(&mut *row_guard, 1, &self.prompt, &self.branch_tag);
            *row_guard += 1;
            let (cx, cy) = render_bottom_box(&self.prompt, &self.branch_tag);
            let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
            let _ = out.flush();
        }
    }
}

pub fn print_user_prompt_at(user_input: &str, row: &mut u16, branch_tag: &str) {
    print_user_prompt_internal(user_input, row, branch_tag, true);
}

pub fn print_user_prompt_internal(user_input: &str, row: &mut u16, branch_tag: &str, render_bottom: bool) {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let wrap_cols = (term_cols as usize).saturating_sub(6).max(20);
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    let mut first = true;
    for raw_line in user_input.lines() {
        let wrapped = wrap_text_line(raw_line, wrap_cols);
        for line in wrapped {
            prepare_output_line_internal(row, 1, "", branch_tag, render_bottom);
            let _ = execute!(
                out,
                cursor::MoveTo(0, *row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine)
            );
            if first {
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Yellow),
                    crossterm::style::Print("❯ "),
                    ResetColor,
                    SetForegroundColor(Color::White),
                    crossterm::style::Print(&line),
                    ResetColor
                );
                first = false;
            } else {
                let _ = execute!(
                    out,
                    crossterm::style::Print("  "),
                    SetForegroundColor(Color::White),
                    crossterm::style::Print(&line),
                    ResetColor
                );
            }
            *row += 1;
        }
    }
    if first {
        prepare_output_line_internal(row, 1, "", branch_tag, render_bottom);
        let _ = execute!(
            out,
            cursor::MoveTo(0, *row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            SetForegroundColor(Color::Yellow),
            crossterm::style::Print("❯ "),
            ResetColor
        );
        *row += 1;
    }
    prepare_output_line_internal(row, 1, "", branch_tag, render_bottom);
    *row += 1;

    if render_bottom {
        let (cx, cy) = render_bottom_box("", branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub fn print_thought_at(thought: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_thought_internal(thought, row, prompt, branch_tag, true);
}

pub fn print_thought_internal(thought: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let max_w = (term_cols as usize).saturating_sub(6).max(20);
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    let first_line = thought
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("Thinking");

    let avail_w = max_w.saturating_sub(4).saturating_sub(3); // 4 for "💭 ", 3 for "..."
    let truncated = truncate_str(first_line, avail_w);
    let line_text = if str_width(first_line) > avail_w || thought.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        format!("{truncated}...")
    } else {
        truncated
    };

    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    let _ = execute!(
        out,
        cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        SetForegroundColor(Color::Magenta),
        crossterm::style::Print("💭 "),
        SetForegroundColor(Color::DarkGrey),
        crossterm::style::Print(&line_text),
        ResetColor
    );
    *row += 1;

    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

fn extract_compact_arg(_name: &str, args: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        if let Some(obj) = v.as_object() {
            if let Some(path) = obj.get("path").and_then(|p| p.as_str()) {
                return path.to_string();
            }
            if let Some(cmd) = obj.get("command").and_then(|c| c.as_str()) {
                return cmd.to_string();
            }
            if let Some(q) = obj.get("query").and_then(|q| q.as_str()) {
                return format!("\"{}\"", q);
            }
            if let Some(p) = obj.get("pattern").and_then(|p| p.as_str()) {
                return format!("\"{}\"", p);
            }
        }
    }
    let trimmed = args.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        truncate_str(trimmed, 35)
    }
}

fn get_tool_icon(name: &str) -> &'static str {
    match name {
        "run_command" => "⚙  Bash",
        "write_file" => "📝 Write",
        "edit_file" => "✏️  Edit",
        "read_file" => "🔍 Read",
        "find_files" => "🔎 Find",
        "grep_search" => "🔎 Grep",
        "list_dir" => "📂 List",
        _ => "🔧 Tool",
    }
}

pub fn print_tool_start_at(name: &str, args: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_tool_start_internal(name, args, row, prompt, branch_tag, true);
}

pub fn print_tool_start_internal(name: &str, args: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    let icon = get_tool_icon(name);
    let target = extract_compact_arg(name, args);
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let max_w = (term_cols as usize).saturating_sub(6).max(20);
    let target_disp = if target.is_empty() { String::new() } else { format!(": {}", target) };
    let line_text = truncate_str(&format!("  ⏳ {} {}", icon, target_disp), max_w);

    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    let mut out = stdout();
    let _ = execute!(
        out,
        cursor::Hide,
        cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        SetForegroundColor(Color::Yellow),
        crossterm::style::Print(&line_text),
        ResetColor
    );
    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub fn print_tool_log_at(line: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_tool_log_internal(line, row, prompt, branch_tag, true);
}

pub fn print_tool_log_internal(line: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let max_log_w = (term_cols as usize).saturating_sub(6).max(20);
    let truncated_log = truncate_str(line, max_log_w);

    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    let mut out = stdout();
    let _ = execute!(
        out,
        cursor::Hide,
        cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        SetForegroundColor(Color::DarkGrey),
        crossterm::style::Print("    "),
        crossterm::style::Print(&truncated_log),
        ResetColor
    );
    *row += 1;
    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub fn print_tool_collapsed_indicator_at(hidden_count: usize, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_tool_collapsed_indicator_internal(hidden_count, row, prompt, branch_tag, true);
}

pub fn print_tool_collapsed_indicator_internal(hidden_count: usize, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    let mut out = stdout();
    let _ = execute!(
        out,
        cursor::Hide,
        cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        SetForegroundColor(Color::DarkGrey),
        crossterm::style::Print(format!("    ... (+{hidden_count} lines hidden, Ctrl+O to expand)")),
        ResetColor
    );
    *row += 1;
    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub fn print_tool_end_at(name: &str, args: &str, result: &str, is_error: bool, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_tool_end_internal(name, args, result, is_error, row, prompt, branch_tag, true);
}

pub fn print_tool_end_internal(name: &str, args: &str, result: &str, is_error: bool, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    let icon = get_tool_icon(name);
    let target = extract_compact_arg(name, args);
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let max_w = (term_cols as usize).saturating_sub(6).max(20);
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    let _ = execute!(
        out,
        cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine)
    );

    let target_disp = if target.is_empty() { String::new() } else { format!(": {}", target) };

    if is_error {
        let err_snippet = truncate_str(result.trim().lines().next().unwrap_or("error"), 35);
        let text = truncate_str(&format!("  ✖ {} {} — Error: {}", icon, target_disp, err_snippet), max_w);
        let _ = execute!(
            out,
            SetForegroundColor(Color::Red),
            crossterm::style::Print(&text),
            ResetColor
        );
    } else {
        let detail = match name {
            "list_dir" => {
                let count = result.lines().filter(|l| !l.trim().is_empty()).count();
                format!("({} items)", count)
            }
            "read_file" => {
                let count = result.lines().count();
                format!("({} lines)", count)
            }
            "write_file" => {
                let count = result.lines().count();
                format!("({} lines)", count)
            }
            "edit_file" => "(done)".to_string(),
            "run_command" => "(done)".to_string(),
            _ => {
                let count = result.lines().count();
                if count <= 1 { "(done)".to_string() } else { format!("({} lines)", count) }
            }
        };
        let main_part = format!("  ✔ {} {} ", icon, target_disp);
        let rem_w = max_w.saturating_sub(str_width(&main_part));
        let detail_trunc = truncate_str(&detail, rem_w);

        let _ = execute!(
            out,
            SetForegroundColor(Color::Green),
            crossterm::style::Print("  ✔ "),
            SetForegroundColor(Color::White),
            crossterm::style::Print(format!("{} {} ", icon, target_disp)),
            SetForegroundColor(Color::DarkGrey),
            crossterm::style::Print(&detail_trunc),
            ResetColor
        );
    }
    *row += 1;

    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub fn print_assistant_message_at(msg: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_assistant_message_internal(msg, row, prompt, branch_tag, true);
}

pub fn print_assistant_message_internal(msg: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let wrap_cols = (term_cols as usize).saturating_sub(6).max(20);
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    let rendered_lines = crate::markdown::render_markdown(msg, wrap_cols);

    let mut first = true;
    for line in rendered_lines {
        prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
        let _ = execute!(
            out,
            cursor::MoveTo(0, *row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine)
        );
        if first {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Green),
                crossterm::style::Print("🤖 "),
                ResetColor,
                crossterm::style::Print(&line),
                ResetColor
            );
            first = false;
        } else {
            let _ = execute!(
                out,
                crossterm::style::Print("   "),
                crossterm::style::Print(&line),
                ResetColor
            );
        }
        *row += 1;
    }
    if first {
        prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
        let _ = execute!(
            out,
            cursor::MoveTo(0, *row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            SetForegroundColor(Color::Green),
            crossterm::style::Print("🤖 "),
            ResetColor
        );
        *row += 1;
    }
    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    *row += 1;

    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub fn print_error_at(err: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_error_internal(err, row, prompt, branch_tag, true);
}

pub fn print_error_internal(err: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    let (term_cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let wrap_cols = (term_cols as usize).saturating_sub(12).max(20);
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    let mut first = true;
    for raw_line in err.lines() {
        let wrapped = wrap_text_line(raw_line, wrap_cols);
        for line in wrapped {
            prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
            let _ = execute!(
                out,
                cursor::MoveTo(0, *row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine)
            );
            if first {
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Red),
                    crossterm::style::Print("✖ Error: "),
                    crossterm::style::Print(&line),
                    ResetColor
                );
                first = false;
            } else {
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Red),
                    crossterm::style::Print("         "),
                    crossterm::style::Print(&line),
                    ResetColor
                );
            }
            *row += 1;
        }
    }
    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    *row += 1;

    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

#[allow(dead_code)]
pub fn print_tool_start(name: &str, args: &str) {
    let icon = get_tool_icon(name);
    let target = extract_compact_arg(name, args);
    let target_disp = if target.is_empty() { String::new() } else { format!(": {}", target) };
    let mut out = stdout();
    execute!(out, SetForegroundColor(Color::Yellow)).ok();
    print!("  ⏳ {} {}", icon, target_disp);
    execute!(out, ResetColor).ok();
    println!();
}

#[allow(dead_code)]
pub fn print_tool_log(line: &str) {
    let mut out = stdout();
    execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
    print!("    ");
    execute!(out, ResetColor).ok();
    println!("{}", line);
}

#[allow(dead_code)]
pub fn print_tool_collapsed_indicator(hidden_count: usize) {
    let mut out = stdout();
    let _ = execute!(
        out,
        SetForegroundColor(Color::DarkGrey),
        crossterm::style::Print(format!("    ... (+{hidden_count} lines hidden, Ctrl+O to expand)\r\n")),
        ResetColor
    );
    let _ = out.flush();
}

#[allow(dead_code)]
pub fn print_tool_end(name: &str, args: &str, result: &str, is_error: bool) {
    let icon = get_tool_icon(name);
    let target = extract_compact_arg(name, args);
    let target_disp = if target.is_empty() { String::new() } else { format!(": {}", target) };
    let mut out = stdout();
    if is_error {
        let err_snippet = truncate_str(result.trim().lines().next().unwrap_or("error"), 40);
        execute!(out, SetForegroundColor(Color::Red)).ok();
        println!("  ✖ {} {} — Error: {}", icon, target_disp, err_snippet);
        execute!(out, ResetColor).ok();
    } else {
        execute!(out, SetForegroundColor(Color::Green)).ok();
        print!("  ✔ ");
        execute!(out, SetForegroundColor(Color::White)).ok();
        print!("{} {} ", icon, target_disp);
        execute!(out, SetForegroundColor(Color::DarkGrey)).ok();
        println!("(done)");
        execute!(out, ResetColor).ok();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HistoryItem {
    UserPrompt(String),
    Thought(String),
    ToolStart {
        name: String,
        args: String,
    },
    ToolLog(String),
    ToolEnd {
        name: String,
        #[serde(default)]
        args: String,
        result: String,
        is_error: bool,
    },
    AssistantMessage(String),
    Error(String),
}

pub fn redraw_all(
    model: &str,
    base_url: &str,
    workspace: &str,
    git_info: &str,
    history: &[HistoryItem],
    branch_tag: &str,
    current_prompt: &str,
) -> u16 {
    let mut out = stdout();
    let _ = execute!(
        out,
        cursor::Hide,
        cursor::MoveTo(0, 0)
    );
    let mut row = print_banner(model, base_url, workspace, git_info);
    let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if row >= term_rows {
        row = term_rows.saturating_sub(1);
    }

    let mut i = 0;
    while i < history.len() {
        match &history[i] {
            HistoryItem::UserPrompt(text) => {
                print_user_prompt_internal(text, &mut row, branch_tag, false);
                i += 1;
            }
            HistoryItem::Thought(text) => {
                print_thought_internal(text, &mut row, current_prompt, branch_tag, false);
                i += 1;
            }
            HistoryItem::ToolStart { name, args } => {
                // Collect following ToolLog items and possible ToolEnd
                let mut logs: Vec<&str> = Vec::new();
                let mut j = i + 1;
                while j < history.len() {
                    if let HistoryItem::ToolLog(line) = &history[j] {
                        logs.push(line);
                        j += 1;
                    } else {
                        break;
                    }
                }
                let tool_end = if j < history.len() {
                    if let HistoryItem::ToolEnd { name: end_name, args: end_args, result, is_error } = &history[j] {
                        Some((end_name.as_str(), end_args.as_str(), result.as_str(), *is_error))
                    } else {
                        None
                    }
                } else {
                    None
                };

                if logs.is_empty() {
                    if let Some((end_name, end_args, result, is_error)) = tool_end {
                        print_tool_end_internal(end_name, end_args, result, is_error, &mut row, current_prompt, branch_tag, false);
                        i = j + 1;
                    } else {
                        print_tool_start_internal(name, args, &mut row, current_prompt, branch_tag, false);
                        i += 1;
                    }
                } else {
                    print_tool_start_internal(name, args, &mut row, current_prompt, branch_tag, false);
                    let is_expanded = is_output_expanded();
                    let visible_limit = if is_expanded { logs.len() } else { logs.len().min(5) };

                    for line in &logs[..visible_limit] {
                        print_tool_log_internal(line, &mut row, current_prompt, branch_tag, false);
                    }

                    if !is_expanded && logs.len() > 5 {
                        let hidden = logs.len() - 5;
                        print_tool_collapsed_indicator_internal(hidden, &mut row, current_prompt, branch_tag, false);
                    }

                    if let Some((end_name, end_args, result, is_error)) = tool_end {
                        print_tool_end_internal(end_name, end_args, result, is_error, &mut row, current_prompt, branch_tag, false);
                        i = j + 1;
                    } else {
                        i = j;
                    }
                }
            }
            HistoryItem::ToolLog(line) => {
                print_tool_log_internal(line, &mut row, current_prompt, branch_tag, false);
                i += 1;
            }
            HistoryItem::ToolEnd { name, args, result, is_error } => {
                print_tool_end_internal(name, args, result, *is_error, &mut row, current_prompt, branch_tag, false);
                i += 1;
            }
            HistoryItem::AssistantMessage(text) => {
                print_assistant_message_internal(text, &mut row, current_prompt, branch_tag, false);
                i += 1;
            }
            HistoryItem::Error(text) => {
                print_error_internal(text, &mut row, current_prompt, branch_tag, false);
                i += 1;
            }
        }
    }

    // Clear empty space between the output row and the bottom box
    let box_width = get_box_width();
    let max_content = box_width.saturating_sub(6);
    let chars: Vec<char> = current_prompt.chars().collect();
    let lines_count = if chars.is_empty() {
        1
    } else {
        let mut count = 0;
        let mut start = 0;
        while start < chars.len() {
            let mut cur_w = 0;
            let mut end = start;
            while end < chars.len() {
                let cw = char_width(chars[end]);
                if cur_w + cw > max_content && end > start {
                    break;
                }
                cur_w += cw;
                end += 1;
            }
            count += 1;
            start = end;
        }
        count.max(1)
    };
    let box_rows = 1 + lines_count as u16 + 1;
    let box_start = term_rows.saturating_sub(box_rows);
    for r in row..box_start {
        let _ = execute!(out, cursor::MoveTo(0, r), crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine));
    }

    let (cx, cy) = render_bottom_box(current_prompt, branch_tag);
    let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
    let _ = out.flush();
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_writer_basic() {
        let row = Arc::new(Mutex::new(5));
        let mut sw = StreamWriter::new(row.clone(), "🤖 ", Color::Green, "   ", "", "main");
        sw.write_token("Hello");
        sw.write_token(" world!");
        sw.write_token("\nSecond line");
        assert_eq!(*row.lock().unwrap(), 6);
    }

    #[test]
    fn test_stream_writer_finish_and_replace() {
        let row = Arc::new(Mutex::new(5));
        let mut sw = StreamWriter::new(row.clone(), "🤖 ", Color::Green, "   ", "", "main");
        sw.write_token("**Hello** `world`");
        sw.finish_and_replace("**Takiza Code** — AI agent:\n- `read_file` tool");
        // Replaced row should be updated properly
        assert!(*row.lock().unwrap() > 5);
    }
}


