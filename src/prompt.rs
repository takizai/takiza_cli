use crate::cli_ui;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{stdin, stdout, IsTerminal, Write};

pub enum PromptResult {
    Line(String),
    Interrupted,
    Exit,
}

pub fn char_width(c: char) -> usize {
    match c {
        '\0'..='\x1f' | '\x7f'..='\u{9f}' => 0,
        '\u{200B}'..='\u{200F}' | '\u{FEFF}' => 0,
        '\u{1100}'..='\u{115F}'
        | '\u{2329}'..='\u{232A}'
        | '\u{2E80}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE10}'..='\u{FE19}'
        | '\u{FE30}'..='\u{FE6F}'
        | '\u{FF00}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}'
        | '\u{1F300}'..='\u{1F64F}'
        | '\u{1F680}'..='\u{1F6FF}' => 2,
        _ => 1,
    }
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}


pub fn truncate_visible(s: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }
    let mut cur_w = 0;
    let mut res = String::new();
    for c in s.chars() {
        let cw = char_width(c);
        if cur_w + cw > max_w.saturating_sub(1) && max_w > 2 {
            res.push('…');
            return res;
        } else if cur_w + cw > max_w {
            return res;
        }
        res.push(c);
        cur_w += cw;
    }
    res
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub has_args: bool,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/help",
        description: "Show help summary and all slash commands",
        has_args: false,
    },
    SlashCommand {
        name: "/clear",
        description: "Clear terminal screen and reset history view",
        has_args: false,
    },
    SlashCommand {
        name: "/theme",
        description: "Switch visual theme (amber, cyberpunk, nord...)",
        has_args: true,
    },
    SlashCommand {
        name: "/mode",
        description: "Switch execution mode (Takiza Manual or Takiza MoA)",
        has_args: true,
    },
    SlashCommand {
        name: "/model",
        description: "Switch active LLM model",
        has_args: true,
    },
    SlashCommand {
        name: "/effort",
        description: "Set reasoning effort (low, medium, high) for reasoning models",
        has_args: true,
    },
    SlashCommand {
        name: "/provider",
        description: "Switch preset provider (openai, groq, ollama...)",
        has_args: true,
    },
    SlashCommand {
        name: "/approval",
        description: "Toggle auto-approving shell commands vs asking confirmation",
        has_args: false,
    },
    SlashCommand {
        name: "/diff",
        description: "Show git diff of workspace modifications",
        has_args: false,
    },
    SlashCommand {
        name: "/status",
        description: "Show session status, token usage, and git branch",
        has_args: false,
    },
    SlashCommand {
        name: "/usage",
        description: "Show daily quotas and token usage breakdown (Takiza Manual / MoA)",
        has_args: false,
    },
    SlashCommand {
        name: "/tools",
        description: "List active agent tools and descriptions",
        has_args: false,
    },
    SlashCommand {
        name: "/history",
        description: "View recent conversation and saved session history",
        has_args: false,
    },
    SlashCommand {
        name: "/sessions",
        description: "List saved conversation sessions",
        has_args: false,
    },
    SlashCommand {
        name: "/resume",
        description: "List saved chats or resume a session by ID (/resume [id])",
        has_args: false,
    },
    SlashCommand {
        name: "/new",
        description: "Start a new conversation session (alias for /reset)",
        has_args: false,
    },
    SlashCommand {
        name: "/reset",
        description: "Reset conversation history and start fresh",
        has_args: false,
    },
    SlashCommand {
        name: "/exit",
        description: "Exit Takiza Harness",
        has_args: false,
    },
    SlashCommand {
        name: "/quit",
        description: "Exit Takiza Harness (alias for /exit)",
        has_args: false,
    },
];

pub fn get_matching_commands(buffer: &str) -> Vec<SlashCommand> {
    if !buffer.starts_with('/') {
        return Vec::new();
    }
    if buffer.contains(' ') {
        return Vec::new();
    }
    let query = buffer.to_lowercase();
    let query_no_slash = query.trim_start_matches('/');

    let mut matches = Vec::new();
    for cmd in SLASH_COMMANDS {
        let name_lower = cmd.name.to_lowercase();
        let name_no_slash = name_lower.trim_start_matches('/');
        if query == "/" || name_lower.starts_with(&query) || name_no_slash.starts_with(query_no_slash) {
            matches.push(*cmd);
        }
    }
    if matches.is_empty() && !query_no_slash.is_empty() {
        for cmd in SLASH_COMMANDS {
            let name_lower = cmd.name.to_lowercase();
            if name_lower.contains(query_no_slash) || cmd.description.to_lowercase().contains(query_no_slash) {
                matches.push(*cmd);
            }
        }
    }
    matches
}

pub struct LineEditor {
    history: Vec<String>,
    history_index: Option<usize>,
    prev_total_rows: usize,
}

impl LineEditor {
    fn history_file_path() -> std::path::PathBuf {
        let dir = std::path::Path::new(".takiza");
        let _ = std::fs::create_dir_all(dir);
        dir.join("history")
    }

    fn load_history() -> Vec<String> {
        let path = Self::history_file_path();
        if let Ok(content) = std::fs::read_to_string(path) {
            content
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        } else {
            Vec::new()
        }
    }

    fn append_history(&self, line: &str) {
        use std::io::Write;
        let path = Self::history_file_path();
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", line);
        }
    }

    pub fn new() -> Self {
        let history = Self::load_history();
        Self {
            history,
            history_index: None,
            prev_total_rows: 3,
        }
    }

    #[allow(dead_code)]
    pub fn read_line(&mut self, branch_tag: &str) -> std::io::Result<PromptResult> {
        self.read_line_with_redraw(branch_tag, |_| {})
    }

    pub fn read_line_with_redraw<F>(&mut self, branch_tag: &str, on_redraw: F) -> std::io::Result<PromptResult>
    where
        F: FnMut(&str),
    {
        if !stdout().is_terminal() {
            let mut line = String::new();
            let bytes_read = stdin().read_line(&mut line)?;
            if bytes_read == 0 {
                return Ok(PromptResult::Exit);
            }
            let trimmed = line.trim().to_string();
            return Ok(PromptResult::Line(trimmed));
        }

        enable_raw_mode()?;
        let res = self.read_line_box(branch_tag, on_redraw);
        disable_raw_mode()?;
        res
    }

    fn read_line_box<F>(&mut self, branch_tag: &str, mut on_redraw: F) -> std::io::Result<PromptResult>
    where
        F: FnMut(&str),
    {
        let mut buffer = String::new();
        let mut cursor_char_idx = 0;
        self.history_index = None;
        let mut prev_cursor_row = 0;
        let mut prev_rendered = false;
        let mut menu_selected_idx = 0;
        let mut menu_dismissed = false;
        self.prev_total_rows = 0;

        let matching_cmds = if !menu_dismissed {
            get_matching_commands(&buffer)
        } else {
            Vec::new()
        };
        self.render_box(
            branch_tag,
            &buffer,
            cursor_char_idx,
            &mut prev_cursor_row,
            &mut prev_rendered,
            &matching_cmds,
            menu_selected_idx,
            &mut on_redraw,
        )?;

        loop {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // Handle Ctrl+C
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                        if buffer.is_empty() {
                            let mut out = stdout();
                            execute!(out, cursor::MoveToColumn(0), Print("\r\n"))?;
                            out.flush()?;
                            return Ok(PromptResult::Exit);
                        } else {
                            self.clear_box()?;
                            buffer.clear();
                            return Ok(PromptResult::Interrupted);
                        }
                    }

                    // Handle Ctrl+D (EOF)
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
                        if buffer.is_empty() {
                            let mut out = stdout();
                            execute!(out, cursor::MoveToColumn(0), Print("\r\n"))?;
                            out.flush()?;
                            return Ok(PromptResult::Exit);
                        }
                    }

                    // Handle Ctrl+L (Clear screen)
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
                        on_redraw(&buffer);
                        prev_rendered = false;
                        prev_cursor_row = 0;
                        let matching = if !menu_dismissed {
                            get_matching_commands(&buffer)
                        } else {
                            Vec::new()
                        };
                        self.render_box(
                            branch_tag,
                            &buffer,
                            cursor_char_idx,
                            &mut prev_cursor_row,
                            &mut prev_rendered,
                            &matching,
                            menu_selected_idx,
                            &mut on_redraw,
                        )?;
                        continue;
                    }

                    // Handle Ctrl+O (Toggle expanded command output)
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
                        crate::cli_ui::toggle_expanded_output();
                        on_redraw(&buffer);
                        prev_rendered = false;
                        prev_cursor_row = 0;
                        let matching = if !menu_dismissed {
                            get_matching_commands(&buffer)
                        } else {
                            Vec::new()
                        };
                        self.render_box(
                            branch_tag,
                            &buffer,
                            cursor_char_idx,
                            &mut prev_cursor_row,
                            &mut prev_rendered,
                            &matching,
                            menu_selected_idx,
                            &mut on_redraw,
                        )?;
                        continue;
                    }

                    let matching = if !menu_dismissed {
                        get_matching_commands(&buffer)
                    } else {
                        Vec::new()
                    };
                    let menu_active = !matching.is_empty();

                    match key.code {
                        KeyCode::Esc => {
                            if menu_active {
                                menu_dismissed = true;
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &[],
                                    0,
                                    &mut on_redraw,
                                )?;
                            }
                        }
                        KeyCode::Tab => {
                            if menu_active {
                                let sel = matching[menu_selected_idx];
                                if sel.has_args {
                                    buffer = format!("{} ", sel.name);
                                } else {
                                    buffer = sel.name.to_string();
                                }
                                cursor_char_idx = buffer.chars().count();
                                menu_dismissed = true;
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &[],
                                    0,
                                    &mut on_redraw,
                                )?;
                            }
                        }
                        KeyCode::Enter => {
                            let trimmed = if menu_active {
                                let sel = matching[menu_selected_idx];
                                sel.name.to_string()
                            } else {
                                buffer.trim().to_string()
                            };
                            if !trimmed.is_empty() {
                                if self.history.last().map(|s| s.as_str()) != Some(&trimmed) {
                                    self.append_history(&trimmed);
                                    self.history.push(trimmed.clone());
                                }
                            }
                            self.finish_input(branch_tag)?;
                            return Ok(PromptResult::Line(trimmed));
                        }
                        KeyCode::Char(c) => {
                            let mut chars: Vec<char> = buffer.chars().collect();
                            chars.insert(cursor_char_idx, c);
                            buffer = chars.into_iter().collect();
                            cursor_char_idx += 1;
                            menu_dismissed = false;
                            menu_selected_idx = 0;
                            let new_matching = get_matching_commands(&buffer);
                            self.render_box(
                                branch_tag,
                                &buffer,
                                cursor_char_idx,
                                &mut prev_cursor_row,
                                &mut prev_rendered,
                                &new_matching,
                                menu_selected_idx,
                                &mut on_redraw,
                            )?;
                        }
                        KeyCode::Backspace => {
                            if cursor_char_idx > 0 {
                                let mut chars: Vec<char> = buffer.chars().collect();
                                cursor_char_idx -= 1;
                                chars.remove(cursor_char_idx);
                                buffer = chars.into_iter().collect();
                                menu_dismissed = false;
                                menu_selected_idx = 0;
                                let new_matching = get_matching_commands(&buffer);
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &new_matching,
                                    menu_selected_idx,
                                    &mut on_redraw,
                                )?;
                            }
                        }
                        KeyCode::Delete => {
                            let mut chars: Vec<char> = buffer.chars().collect();
                            if cursor_char_idx < chars.len() {
                                chars.remove(cursor_char_idx);
                                buffer = chars.into_iter().collect();
                                menu_dismissed = false;
                                menu_selected_idx = 0;
                                let new_matching = get_matching_commands(&buffer);
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &new_matching,
                                    menu_selected_idx,
                                    &mut on_redraw,
                                )?;
                            }
                        }
                        KeyCode::Left => {
                            if cursor_char_idx > 0 {
                                cursor_char_idx -= 1;
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &matching,
                                    menu_selected_idx,
                                    &mut on_redraw,
                                )?;
                            }
                        }
                        KeyCode::Right => {
                            let char_count = buffer.chars().count();
                            if cursor_char_idx < char_count {
                                cursor_char_idx += 1;
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &matching,
                                    menu_selected_idx,
                                    &mut on_redraw,
                                )?;
                            }
                        }
                        KeyCode::Home => {
                            cursor_char_idx = 0;
                            self.render_box(
                                branch_tag,
                                &buffer,
                                cursor_char_idx,
                                &mut prev_cursor_row,
                                &mut prev_rendered,
                                &matching,
                                menu_selected_idx,
                                &mut on_redraw,
                            )?;
                        }
                        KeyCode::End => {
                            cursor_char_idx = buffer.chars().count();
                            self.render_box(
                                branch_tag,
                                &buffer,
                                cursor_char_idx,
                                &mut prev_cursor_row,
                                &mut prev_rendered,
                                &matching,
                                menu_selected_idx,
                                &mut on_redraw,
                            )?;
                        }
                        KeyCode::Up => {
                            if menu_active {
                                if menu_selected_idx > 0 {
                                    menu_selected_idx -= 1;
                                } else {
                                    menu_selected_idx = matching.len() - 1;
                                }
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &matching,
                                    menu_selected_idx,
                                    &mut on_redraw,
                                )?;
                            } else if !self.history.is_empty() {
                                let next_idx = match self.history_index {
                                    None => self.history.len().saturating_sub(1),
                                    Some(idx) => idx.saturating_sub(1),
                                };
                                self.history_index = Some(next_idx);
                                if let Some(item) = self.history.get(next_idx) {
                                    buffer = item.clone();
                                    cursor_char_idx = buffer.chars().count();
                                    self.render_box(
                                        branch_tag,
                                        &buffer,
                                        cursor_char_idx,
                                        &mut prev_cursor_row,
                                        &mut prev_rendered,
                                        &[],
                                        0,
                                        &mut on_redraw,
                                    )?;
                                }
                            }
                        }
                        KeyCode::Down => {
                            if menu_active {
                                if menu_selected_idx + 1 < matching.len() {
                                    menu_selected_idx += 1;
                                } else {
                                    menu_selected_idx = 0;
                                }
                                self.render_box(
                                    branch_tag,
                                    &buffer,
                                    cursor_char_idx,
                                    &mut prev_cursor_row,
                                    &mut prev_rendered,
                                    &matching,
                                    menu_selected_idx,
                                    &mut on_redraw,
                                )?;
                            } else if let Some(idx) = self.history_index {
                                if idx + 1 < self.history.len() {
                                    let next_idx = idx + 1;
                                    self.history_index = Some(next_idx);
                                    if let Some(item) = self.history.get(next_idx) {
                                        buffer = item.clone();
                                        cursor_char_idx = buffer.chars().count();
                                        self.render_box(
                                            branch_tag,
                                            &buffer,
                                            cursor_char_idx,
                                            &mut prev_cursor_row,
                                            &mut prev_rendered,
                                            &[],
                                            0,
                                            &mut on_redraw,
                                        )?;
                                    }
                                } else {
                                    self.history_index = None;
                                    buffer.clear();
                                    cursor_char_idx = 0;
                                    self.render_box(
                                        branch_tag,
                                        &buffer,
                                        cursor_char_idx,
                                        &mut prev_cursor_row,
                                        &mut prev_rendered,
                                        &[],
                                        0,
                                        &mut on_redraw,
                                    )?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    on_redraw(&buffer);
                    let matching = if !menu_dismissed {
                        get_matching_commands(&buffer)
                    } else {
                        Vec::new()
                    };
                    self.render_box(
                        branch_tag,
                        &buffer,
                        cursor_char_idx,
                        &mut prev_cursor_row,
                        &mut prev_rendered,
                        &matching,
                        menu_selected_idx,
                        &mut on_redraw,
                    )?;
                }
                _ => {}
            }
        }
    }

    fn finish_input(&self, branch_tag: &str) -> std::io::Result<()> {
        let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let start_row = term_rows.saturating_sub(self.prev_total_rows as u16);

        let mut out = stdout();
        execute!(out, cursor::MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
        cli_ui::render_bottom_box("", branch_tag);
        let _ = execute!(out, cursor::Hide);
        out.flush()?;
        Ok(())
    }

    fn clear_box(&self) -> std::io::Result<()> {
        let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let start_row = term_rows.saturating_sub(self.prev_total_rows as u16);

        let mut out = stdout();
        execute!(out, cursor::MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
        out.flush()?;
        Ok(())
    }

    fn render_box<F>(
        &mut self,
        branch_tag: &str,
        buffer: &str,
        cursor_char_idx: usize,
        prev_cursor_row: &mut usize,
        prev_rendered: &mut bool,
        menu_items: &[SlashCommand],
        menu_selected_idx: usize,
        _on_redraw: &mut F,
    ) -> std::io::Result<()>
    where
        F: FnMut(&str),
    {
        let box_width = cli_ui::get_box_width();
        let max_content_chars = box_width.saturating_sub(6);

        // Compute rows of content
        let chars: Vec<char> = buffer.chars().collect();
        let mut content_lines: Vec<String> = Vec::new();
        let mut cursor_row = 0;
        let mut cursor_col = 4; // default after "│ > "

        if chars.is_empty() {
            content_lines.push(String::new());
            cursor_row = 0;
            cursor_col = 4;
        } else {
            let mut start = 0;
            while start < chars.len() {
                let mut cur_w = 0;
                let mut end = start;
                while end < chars.len() {
                    let cw = char_width(chars[end]);
                    if cur_w + cw > max_content_chars && end > start {
                        break;
                    }
                    cur_w += cw;
                    end += 1;
                }

                let line_chunk: String = chars[start..end].iter().collect();

                if cursor_char_idx >= start && (cursor_char_idx < end || (cursor_char_idx == end && end == chars.len())) {
                    cursor_row = content_lines.len();
                    let pre_cursor: String = chars[start..cursor_char_idx].iter().collect();
                    cursor_col = 4 + str_width(&pre_cursor);
                }

                content_lines.push(line_chunk);
                start = end;
            }

            if cursor_char_idx == chars.len() && cursor_row < content_lines.len().saturating_sub(1) {
                cursor_row = content_lines.len() - 1;
                cursor_col = 4 + str_width(content_lines.last().unwrap());
            }
        }

        let (_, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let max_visible_items = 5usize;
        let menu_active = !menu_items.is_empty();
        let visible_slots = if menu_active {
            (term_rows as usize).saturating_sub(6).clamp(2, max_visible_items)
        } else {
            0
        };
        let menu_rows = if menu_active {
            1 + visible_slots + 1 + 1 // divider + visible slots + status/overflow + hints
        } else {
            0
        };

        let total_rows = 1 + content_lines.len() + menu_rows + 1;
        let start_row = term_rows.saturating_sub(total_rows as u16);

        let mut out = stdout();
        queue!(out, cursor::Hide)?;

        let shrank = *prev_rendered && total_rows < self.prev_total_rows;
        if shrank {
            let old_start_row = term_rows.saturating_sub(self.prev_total_rows as u16);
            let new_start_row = term_rows.saturating_sub(total_rows as u16);
            for r in old_start_row..new_start_row {
                queue!(out, cursor::MoveTo(0, r), Clear(ClearType::UntilNewLine))?;
            }
            out.flush()?;
        }
        self.prev_total_rows = total_rows;

        // 1. Draw Top border: ╭─ You [branch] ────────────────────────────────────────╮
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

        let th = crate::theme::current();
        let border_color = th.border_crossterm();
        let primary_color = th.primary_crossterm();

        queue!(
            out,
            cursor::MoveTo(0, start_row),
            Clear(ClearType::FromCursorDown),
            SetForegroundColor(border_color),
            Print("╭─"),
            SetForegroundColor(primary_color),
            Print(&safe_title),
            SetForegroundColor(border_color),
            Print("─".repeat(dashes_top)),
            Print("╮"),
            ResetColor
        )?;

        // 2. Draw Content lines
        for (i, line) in content_lines.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let prefix = if i == 0 { "> " } else { "  " };
            let line_w = str_width(line);
            let pad = max_content_chars.saturating_sub(line_w);

            queue!(
                out,
                cursor::MoveTo(0, row),
                SetForegroundColor(border_color),
                Print("│ "),
                SetForegroundColor(primary_color),
                Print(prefix),
                ResetColor
            )?;

            if buffer.is_empty() {
                let placeholder = "Type your prompt, /help for commands, !cmd for shell";
                let mut ph_chars: Vec<char> = Vec::new();
                let mut ph_w = 0;
                for c in placeholder.chars() {
                    let cw = char_width(c);
                    if ph_w + cw > max_content_chars {
                        break;
                    }
                    ph_chars.push(c);
                    ph_w += cw;
                }
                let ph_str: String = ph_chars.into_iter().collect();
                let ph_pad = max_content_chars.saturating_sub(ph_w);

                queue!(
                    out,
                    SetForegroundColor(Color::DarkGrey),
                    Print(&ph_str),
                    Print(" ".repeat(ph_pad)),
                    ResetColor
                )?;
            } else {
                queue!(
                    out,
                    Print(line),
                    Print(" ".repeat(pad))
                )?;
            }

            queue!(
                out,
                SetForegroundColor(border_color),
                Print(" │"),
                ResetColor
            )?;
        }

        let mut current_row = start_row + 1 + content_lines.len() as u16;

        // 3. If command dropdown is active, render commands menu
        if menu_active {
            let inner_w = box_width.saturating_sub(4);
            let dashes_div = box_width.saturating_sub(2);

            // Divider: ├────────────────────────────────────────┤
            queue!(
                out,
                cursor::MoveTo(0, current_row),
                SetForegroundColor(border_color),
                Print("├"),
                Print("─".repeat(dashes_div)),
                Print("┤"),
                ResetColor
            )?;
            current_row += 1;

            // Compute visible window
            let scroll_offset = if menu_selected_idx < visible_slots {
                0
            } else {
                menu_selected_idx + 1 - visible_slots
            };

            for slot in 0..visible_slots {
                let row = current_row;
                current_row += 1;
                let idx = scroll_offset + slot;

                if idx < menu_items.len() {
                    let item = &menu_items[idx];
                    let is_selected = idx == menu_selected_idx;

                    let prefix = if is_selected { "> " } else { "  " };
                    let cmd_name = item.name;
                    let cmd_w = str_width(cmd_name);
                    let col_w = 14usize;
                    let cmd_pad = col_w.saturating_sub(cmd_w);

                    let desc_avail = inner_w.saturating_sub(2 + col_w + 2);
                    let desc_trunc = if desc_avail > 5 {
                        truncate_visible(item.description, desc_avail)
                    } else {
                        String::new()
                    };
                    let desc_w = str_width(&desc_trunc);
                    let right_pad = inner_w.saturating_sub(2 + col_w + 2 + desc_w);

                    queue!(
                        out,
                        cursor::MoveTo(0, row),
                        SetForegroundColor(border_color),
                        Print("│ "),
                        ResetColor
                    )?;

                    if is_selected {
                        queue!(
                            out,
                            SetForegroundColor(primary_color),
                            Print(prefix),
                            Print(cmd_name),
                            Print(" ".repeat(cmd_pad)),
                            Print("  "),
                            SetForegroundColor(Color::White),
                            Print(&desc_trunc),
                            Print(" ".repeat(right_pad)),
                            ResetColor
                        )?;
                    } else {
                        queue!(
                            out,
                            Print(prefix),
                            SetForegroundColor(Color::AnsiValue(250)),
                            Print(cmd_name),
                            Print(" ".repeat(cmd_pad)),
                            Print("  "),
                            SetForegroundColor(Color::DarkGrey),
                            Print(&desc_trunc),
                            Print(" ".repeat(right_pad)),
                            ResetColor
                        )?;
                    }

                    queue!(
                        out,
                        SetForegroundColor(border_color),
                        Print(" │"),
                        ResetColor
                    )?;
                } else {
                    queue!(
                        out,
                        cursor::MoveTo(0, row),
                        SetForegroundColor(border_color),
                        Print("│ "),
                        ResetColor,
                        Print(" ".repeat(inner_w)),
                        SetForegroundColor(border_color),
                        Print(" │"),
                        ResetColor
                    )?;
                }
            }

            // Status / Overflow line
            let row = current_row;
            current_row += 1;

            let end_idx = (scroll_offset + visible_slots).min(menu_items.len());
            let status_text = if menu_items.len() > visible_slots {
                let remaining = menu_items.len().saturating_sub(end_idx);
                if remaining > 0 {
                    format!("↓ {} more  ({}/{} commands)", remaining, menu_selected_idx + 1, menu_items.len())
                } else {
                    format!("↑ {} above  ({}/{} commands)", scroll_offset, menu_selected_idx + 1, menu_items.len())
                }
            } else {
                format!("{}/{} commands", menu_selected_idx + 1, menu_items.len())
            };
            let status_w = str_width(&status_text);
            let safe_status = if 2 + status_w > inner_w {
                truncate_visible(&status_text, inner_w.saturating_sub(2))
            } else {
                status_text
            };
            let safe_status_w = str_width(&safe_status);
            let right_pad = inner_w.saturating_sub(2 + safe_status_w);

            queue!(
                out,
                cursor::MoveTo(0, row),
                SetForegroundColor(border_color),
                Print("│ "),
                SetForegroundColor(Color::DarkGrey),
                Print("  "),
                Print(&safe_status),
                Print(" ".repeat(right_pad)),
                SetForegroundColor(border_color),
                Print(" │"),
                ResetColor
            )?;

            // Hints line: ↑/↓ Navigate · enter Select · tab Complete · esc to cancel
            let row = current_row;
            current_row += 1;

            queue!(
                out,
                cursor::MoveTo(0, row),
                SetForegroundColor(border_color),
                Print("│ "),
                ResetColor
            )?;

            if inner_w >= 62 {
                let vis_len = 60usize;
                let right_pad = inner_w.saturating_sub(vis_len);
                queue!(
                    out,
                    Print("  "),
                    SetForegroundColor(primary_color),
                    Print("↑/↓"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Navigate · "),
                    SetForegroundColor(primary_color),
                    Print("enter"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Select · "),
                    SetForegroundColor(primary_color),
                    Print("tab"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Complete · "),
                    SetForegroundColor(primary_color),
                    Print("esc"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" to cancel"),
                    Print(" ".repeat(right_pad)),
                    ResetColor
                )?;
            } else if inner_w >= 44 {
                let vis_len = 42usize;
                let right_pad = inner_w.saturating_sub(vis_len);
                queue!(
                    out,
                    Print("  "),
                    SetForegroundColor(primary_color),
                    Print("↑/↓"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Nav · "),
                    SetForegroundColor(primary_color),
                    Print("↵"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Select · "),
                    SetForegroundColor(primary_color),
                    Print("Tab"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Fill · "),
                    SetForegroundColor(primary_color),
                    Print("Esc"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Quit"),
                    Print(" ".repeat(right_pad)),
                    ResetColor
                )?;
            } else if inner_w >= 31 {
                let vis_len = 31usize;
                let right_pad = inner_w.saturating_sub(vis_len);
                queue!(
                    out,
                    Print("  "),
                    SetForegroundColor(primary_color),
                    Print("↑/↓"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Nav · "),
                    SetForegroundColor(primary_color),
                    Print("↵"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Select · "),
                    SetForegroundColor(primary_color),
                    Print("Esc"),
                    SetForegroundColor(Color::DarkGrey),
                    Print(" Quit"),
                    Print(" ".repeat(right_pad)),
                    ResetColor
                )?;
            } else {
                queue!(out, Print(" ".repeat(inner_w)))?;
            }

            queue!(
                out,
                SetForegroundColor(border_color),
                Print(" │"),
                ResetColor
            )?;

            // Bottom border with menu active: ╰────────────────────────╯
            let bottom_row = current_row;
            let dashes_bottom = box_width.saturating_sub(2);
            queue!(
                out,
                cursor::MoveTo(0, bottom_row),
                SetForegroundColor(border_color),
                Print("╰"),
                Print("─".repeat(dashes_bottom)),
                Print("╯"),
                ResetColor
            )?;
        } else {
            // Normal Bottom border without menu: ╰────── 💬 Title • Ctrl+O: Expand • Ctrl+C: Cancel ───╯
            let hint = if box_width >= 62 {
                if crate::cli_ui::is_output_expanded() {
                    " Ctrl+O: Collapse • Ctrl+C: Cancel "
                } else {
                    " Ctrl+O: Expand • Ctrl+C: Cancel "
                }
            } else if box_width >= 50 {
                " Enter: ↵  Ctrl+C: ✕ "
            } else {
                " ↵ Enter "
            };
            let hint_w = str_width(hint);

            let chat_title = crate::cli_ui::get_active_chat_title();
            let (title_part, title_vis) = if !chat_title.trim().is_empty() && box_width >= 55 {
                let max_title_w = box_width.saturating_sub(hint_w + 14).min(35);
                let safe_title = truncate_visible(chat_title.trim(), max_title_w);
                let vis = str_width(" 💬 ") + str_width(&safe_title) + str_width("  •  ");
                (Some(safe_title), vis)
            } else {
                (None, 0)
            };

            let total_text_w = title_vis + hint_w;
            let dashes_bottom = box_width.saturating_sub(total_text_w + 3);
            let bottom_row = current_row;

            queue!(
                out,
                cursor::MoveTo(0, bottom_row),
                SetForegroundColor(border_color),
                Print("╰"),
                Print("─".repeat(dashes_bottom)),
            )?;

            if let Some(ref t) = title_part {
                queue!(
                    out,
                    SetForegroundColor(Color::DarkGrey),
                    Print(" 💬 "),
                    SetForegroundColor(primary_color),
                    Print(t),
                    SetForegroundColor(Color::DarkGrey),
                    Print("  •  "),
                )?;
            }

            queue!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(hint),
                SetForegroundColor(border_color),
                Print("─╯"),
                ResetColor
            )?;
        }

        // Position terminal cursor strictly inside the prompt line
        let target_cursor_y = start_row + 1 + cursor_row as u16;
        queue!(out, cursor::MoveTo(cursor_col as u16, target_cursor_y), cursor::Show)?;
        out.flush()?;

        *prev_cursor_row = cursor_row;
        *prev_rendered = true;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_str_width() {
        assert_eq!(str_width("hello"), 5);
        assert_eq!(str_width("привет"), 6);
        assert_eq!(str_width("╭─"), 2);
        assert_eq!(str_width("↵"), 1);
        assert_eq!(str_width("✕"), 1);
    }

    #[test]
    fn test_border_widths() {
        for box_width in [36usize, 40, 50, 60, 70, 72, 80, 100, 120, 160, 200, 250] {
            // Top border
            let title = " You ";
            let title_w = str_width(title);
            let dashes_top = box_width.saturating_sub(title_w + 3);
            let top_len = 2 + title_w + dashes_top + 1;
            assert_eq!(top_len, box_width, "Top border width mismatch at {}", box_width);

            // Bottom border
            let hint = if box_width >= 50 {
                " Enter: ↵  Ctrl+C: ✕ "
            } else {
                " ↵ Enter "
            };
            let hint_w = str_width(hint);
            let dashes_bottom = box_width.saturating_sub(hint_w + 3);
            let bottom_len = 1 + dashes_bottom + hint_w + 2;
            assert_eq!(bottom_len, box_width, "Bottom border width mismatch at {}", box_width);

            // Bottom border with chat title
            crate::cli_ui::set_active_chat_title("Приветствие на русском");
            let chat_title = crate::cli_ui::get_active_chat_title();
            let (title_part, title_vis) = if !chat_title.trim().is_empty() && box_width >= 55 {
                let max_title_w = box_width.saturating_sub(hint_w + 14).min(35);
                let safe_title = truncate_visible(chat_title.trim(), max_title_w);
                let vis = str_width(" 💬 ") + str_width(&safe_title) + str_width("  •  ");
                (Some(safe_title), vis)
            } else {
                (None, 0)
            };
            let total_text_w = title_vis + hint_w;
            let dashes_with_title = box_width.saturating_sub(total_text_w + 3);
            let printed_title_len = if let Some(ref t) = title_part {
                str_width(" 💬 ") + str_width(t) + str_width("  •  ")
            } else {
                0
            };
            let actual_bottom_len = 1 + dashes_with_title + printed_title_len + hint_w + 2;
            assert_eq!(actual_bottom_len, box_width, "Bottom border with title mismatch at {}", box_width);
            crate::cli_ui::set_active_chat_title("");

            // Content line
            let max_content = box_width.saturating_sub(6);
            let content_len = 2 + 2 + max_content + 2;
            assert_eq!(content_len, box_width, "Content line width mismatch at {}", box_width);
        }
    }

    #[test]
    fn test_get_matching_commands() {
        let all = get_matching_commands("/");
        assert_eq!(all.len(), SLASH_COMMANDS.len());

        let t_cmds = get_matching_commands("/t");
        let names: Vec<&str> = t_cmds.iter().map(|c| c.name).collect();
        assert!(names.contains(&"/theme"));
        assert!(names.contains(&"/tools"));

        let cl_cmds = get_matching_commands("/cl");
        assert_eq!(cl_cmds.len(), 1);
        assert_eq!(cl_cmds[0].name, "/clear");

        // Spaces terminate command matching (arguments mode)
        assert!(get_matching_commands("/theme ").is_empty());
        assert!(get_matching_commands("/theme amber").is_empty());

        // Regular prompts do not match
        assert!(get_matching_commands("hello world").is_empty());
    }

    #[test]
    fn test_menu_box_widths() {
        for box_width in [36usize, 40, 50, 60, 70, 72, 80, 100, 120, 160, 200, 250] {
            let inner_w = box_width.saturating_sub(4);

            // Divider: ├────────────────┤
            let dashes_div = box_width.saturating_sub(2);
            let div_len = 1 + dashes_div + 1;
            assert_eq!(div_len, box_width);

            // Menu item line: │ > /theme        Switch visual theme... │
            let col_w = 14usize;
            let cmd_name = "/theme";
            let cmd_w = str_width(cmd_name);
            let cmd_pad = col_w.saturating_sub(cmd_w);
            let desc_avail = inner_w.saturating_sub(2 + col_w + 2);
            let desc = "Switch visual theme (amber, cyberpunk, nord...)";
            let desc_trunc = if desc_avail > 5 {
                truncate_visible(desc, desc_avail)
            } else {
                String::new()
            };
            let desc_w = str_width(&desc_trunc);
            let right_pad = inner_w.saturating_sub(2 + col_w + 2 + desc_w);
            let item_len = 2 + 2 + cmd_w + cmd_pad + 2 + desc_w + right_pad + 2;
            assert_eq!(item_len, box_width, "Item line width mismatch at {}", box_width);

            // Overflow indicator: │   ↓ 7 more                           │
            let more_text = "↓ 7 more";
            let more_w = str_width(more_text);
            let overflow_right_pad = inner_w.saturating_sub(2 + more_w);
            let overflow_len = 2 + 2 + more_w + overflow_right_pad + 2;
            assert_eq!(overflow_len, box_width, "Overflow line width mismatch at {}", box_width);

            // Hints line
            let vis_len = if inner_w >= 62 {
                60usize
            } else if inner_w >= 44 {
                42usize
            } else if inner_w >= 31 {
                31usize
            } else {
                0
            };
            let hints_pad = inner_w.saturating_sub(vis_len);
            let hints_len = 2 + vis_len + hints_pad + 2;
            assert_eq!(hints_len, box_width, "Hints line width mismatch at {}", box_width);

            // Bottom border with menu active: ╰────────────────────────╯
            let dashes_bottom = box_width.saturating_sub(2);
            let bottom_len = 1 + dashes_bottom + 1;
            assert_eq!(bottom_len, box_width);
        }
    }
}
