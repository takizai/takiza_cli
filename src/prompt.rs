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
        '\u{0300}'..='\u{036F}' | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}'
        | '\u{FE00}'..='\u{FE0F}' | '\u{FE20}'..='\u{FE2F}' => 0,
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
        description: "Start a new chat and clear conversation history",
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
    SlashCommand { name: "/rewind", description: "Restore files and chat to a checkpoint before a prompt", has_args: false },
    SlashCommand { name: "/restore", description: "Restore files and chat (alias of /rewind)", has_args: false },
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
        name: "/skills",
        description: "Choose a local or global skill to insert into the prompt",
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

#[derive(Clone, Debug)]
struct Completion {
    name: String,
    description: String,
    has_args: bool,
    is_skill: bool,
}

fn skill_token(text: &str, cursor: usize) -> Option<(std::ops::Range<usize>, &str)> {
    let caret = text.char_indices().nth(cursor).map(|(index, _)| index).unwrap_or(text.len());
    let start = text[..caret].rfind('$')?;
    let name_char = |ch: char| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.');
    if text[..start].chars().next_back().is_some_and(|ch| name_char(ch) || ch == '$') { return None; }
    let query = &text[start + 1..caret];
    if !query.chars().all(name_char) { return None; }
    let mut end = caret;
    for ch in text[caret..].chars().take_while(|ch| name_char(*ch)) { end += ch.len_utf8(); }
    Some((start..end, query))
}

fn skill_matches(text: &str, cursor: usize, skills: &[crate::skills::Skill]) -> Vec<Completion> {
    let Some((_, query)) = skill_token(text, cursor) else { return Vec::new(); };
    let query = query.to_lowercase();
    skills.iter().filter(|skill| skill.name.to_lowercase().contains(&query))
        .map(|skill| Completion { name: format!("${}", skill.name),
            description: format!("{} · {}", if skill.is_workspace { "local" } else { "global" }, skill.description),
            has_args: false, is_skill: true }).collect()
}

fn get_completions(text: &str, cursor: usize) -> Vec<Completion> {
    if skill_token(text, cursor).is_some() {
        let workspace = std::env::current_dir().unwrap_or_default();
        return skill_matches(text, cursor, &crate::skills::completion_skills(&workspace));
    }
    get_matching_commands(text).into_iter().map(|command| Completion {
        name: command.name.into(), description: command.description.into(), has_args: command.has_args, is_skill: false,
    }).collect()
}

fn apply_completion(text: &mut String, cursor: &mut usize, item: &Completion) {
    if item.is_skill {
        if let Some((range, _)) = skill_token(text, *cursor) {
            let start = range.start;
            let has_space = text[range.end..].starts_with(char::is_whitespace);
            let replacement = format!("{}{}", item.name, if has_space { "" } else { " " });
            text.replace_range(range, &replacement);
            *cursor = text[..start].chars().count() + replacement.chars().count() + usize::from(has_space);
        }
    } else {
        *text = format!("{}{}", item.name, if item.has_args { " " } else { "" });
        *cursor = text.chars().count();
    }
}

/// Editable draft shared between active responses and the idle prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub text: String,
    pub cursor: usize,
    pub menu_selected: usize,
    pub menu_dismissed: bool,
    view_top: Option<usize>,
    last_escape: Option<std::time::Instant>,
    preferred_column: Option<usize>,
    selection_anchor: Option<usize>,
    mouse_anchor: Option<usize>,
}

const MAX_INPUT_LINES: usize = 3;
const DOUBLE_ESCAPE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

fn input_layout(buffer: &str, cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    let chars: Vec<char> = buffer.chars().collect();
    let cursor = cursor.min(chars.len());
    let ranges = input_ranges(buffer, width);
    let mut caret = (0, 0);
    let mut lines = Vec::new();
    for (row, range) in ranges.iter().enumerate() {
        if cursor >= range.start && (cursor < range.end || cursor == range.end
            && ranges.get(row + 1).is_none_or(|next| next.start > range.end)) {
            caret = (row, chars[range.start..cursor].iter().copied().map(char_width).sum());
        }
        lines.push(chars[range.clone()].iter().collect());
    }
    (lines, caret.0, caret.1)
}

fn input_ranges(buffer: &str, width: usize) -> Vec<std::ops::Range<usize>> {
    let chars: Vec<char> = buffer.chars().collect();
    let mut ranges = Vec::new();
    let mut start = 0;
    loop {
        let mut end = start;
        let mut used = 0;
        while end < chars.len() && chars[end] != '\n' {
            let next = char_width(chars[end]);
            if used + next > width.max(1) && end > start { break; }
            used += next;
            end += 1;
        }
        ranges.push(start..end);
        if end == chars.len() { break; }
        start = end + usize::from(chars[end] == '\n');
    }
    ranges
}

impl Draft {
    fn byte_index(&self) -> usize {
        self.text.char_indices().nth(self.cursor).map(|(i, _)| i).unwrap_or(self.text.len())
    }

    pub fn insert_text(&mut self, text: &str) {
        self.delete_selection();
        self.preferred_column = None;
        self.view_top = None;
        self.last_escape = None;
        let text = crate::attachments::pasted_text(text).unwrap_or_else(|error| {
            crate::logger::log_warn("Attachments", &error);
            text.to_string()
        });
        let text: String = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ")
            .chars().filter(|&c| c == '\n' || !c.is_control()).collect();
        self.text.insert_str(self.byte_index(), &text);
        self.cursor += text.chars().count();
        self.menu_dismissed = true;
        self.menu_selected = 0;
    }

    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> Option<String> {
        if key.code == KeyCode::Esc {
            let now = std::time::Instant::now();
            if self.last_escape.is_some_and(|last| now.duration_since(last) <= DOUBLE_ESCAPE_DELAY) {
                *self = Self::default();
                return None;
            }
            self.last_escape = Some(now);
        } else {
            self.last_escape = None;
        }
        self.view_top = None;
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(key.code, KeyCode::Enter) && key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) {
            self.insert_text("\n");
            return None;
        }
        let commands = self.completions();
        if !commands.is_empty() {
            self.menu_selected = self.menu_selected.min(commands.len() - 1);
            match key.code {
                KeyCode::Up => { self.menu_selected = self.menu_selected.saturating_sub(1); return None; }
                KeyCode::Down => { self.menu_selected = (self.menu_selected + 1).min(commands.len() - 1); return None; }
                KeyCode::Esc => { self.menu_dismissed = true; return None; }
                KeyCode::Tab => {
                    apply_completion(&mut self.text, &mut self.cursor, &commands[self.menu_selected]);
                    self.menu_dismissed = true;
                    return None;
                }
                KeyCode::Enter => {
                    let item = &commands[self.menu_selected];
                    if item.is_skill {
                        apply_completion(&mut self.text, &mut self.cursor, item);
                        self.menu_dismissed = true;
                        return None;
                    }
                    let command = item.name.clone();
                    *self = Self::default();
                    return Some(command);
                }
                _ => {}
            }
        }
        let previous_text = self.text.clone();
        let (cols, _) = cli_ui::terminal_size();
        let width = cols.saturating_sub(6).max(1) as usize;
        let movement = matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End);
        if matches!(key.code, KeyCode::Left | KeyCode::Right) && !key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) {
            if let Some(range) = self.selection_range() {
                self.cursor = if key.code == KeyCode::Left { range.start } else { range.end };
                self.selection_anchor = None;
                self.preferred_column = None;
                return None;
            }
        }
        if movement {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                if self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor); }
            } else { self.selection_anchor = None; }
        }
        if !matches!(key.code, KeyCode::Up | KeyCode::Down) { self.preferred_column = None; }
        match key.code {
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.text).trim().to_string();
                *self = Self::default();
                return (!text.is_empty()).then_some(text);
            }
            KeyCode::Char('a') if control => { self.selection_anchor = Some(0); self.cursor = self.text.chars().count(); }
            KeyCode::Char('u') if control => { self.text.clear(); self.cursor = 0; self.selection_anchor = None; }
            KeyCode::Char('w') if control => {
                if self.delete_selection() { return None; }
                while self.cursor > 0 && self.text.chars().nth(self.cursor - 1).is_some_and(char::is_whitespace) {
                    self.backspace();
                }
                while self.cursor > 0 && self.text.chars().nth(self.cursor - 1).is_some_and(|c| !c.is_whitespace()) {
                    self.backspace();
                }
            }
            KeyCode::Char(c) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.delete_selection();
                self.text.insert(self.byte_index(), c);
                self.cursor += 1;
            }
            KeyCode::Backspace => { if !self.delete_selection() { self.backspace(); } }
            KeyCode::Delete => { if !self.delete_selection() && self.cursor < self.text.chars().count() { self.text.remove(self.byte_index()); } }
            KeyCode::Left if control => self.move_word(false),
            KeyCode::Right if control => self.move_word(true),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.chars().count()),
            KeyCode::Up | KeyCode::Down => {
                let (_, row, col) = input_layout(&self.text, self.cursor, width);
                let ranges = input_ranges(&self.text, width);
                let desired = *self.preferred_column.get_or_insert(col);
                let next = if key.code == KeyCode::Up { row.saturating_sub(1) } else { (row + 1).min(ranges.len() - 1) };
                self.cursor = self.cursor_at(&ranges[next], desired);
            }
            KeyCode::Home | KeyCode::End => {
                let (_, row, _) = input_layout(&self.text, self.cursor, width);
                let ranges = input_ranges(&self.text, width);
                self.cursor = if key.code == KeyCode::Home { ranges[row].start } else { ranges[row].end };
            }
            _ => {}
        }
        if self.text != previous_text || movement {
            self.menu_selected = 0;
            self.menu_dismissed = false;
        }
        None
    }

    fn cursor_at(&self, range: &std::ops::Range<usize>, column: usize) -> usize {
        let mut used = 0;
        let mut index = range.start;
        for c in self.text.chars().skip(range.start).take(range.len()) {
            if used + char_width(c) > column { break; }
            used += char_width(c);
            index += 1;
        }
        index
    }

    fn move_word(&mut self, right: bool) {
        let chars: Vec<_> = self.text.chars().collect();
        if right {
            while self.cursor < chars.len() && !chars[self.cursor].is_whitespace() { self.cursor += 1; }
            while self.cursor < chars.len() && chars[self.cursor].is_whitespace() { self.cursor += 1; }
        } else {
            while self.cursor > 0 && chars[self.cursor - 1].is_whitespace() { self.cursor -= 1; }
            while self.cursor > 0 && !chars[self.cursor - 1].is_whitespace() { self.cursor -= 1; }
        }
    }

    fn selection_range(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor).then_some(anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection_range().map(|range| self.text.chars().skip(range.start).take(range.len()).collect())
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection_range() else { self.selection_anchor = None; return false; };
        let start = self.text.char_indices().nth(range.start).map_or(self.text.len(), |(byte, _)| byte);
        let end = self.text.char_indices().nth(range.end).map_or(self.text.len(), |(byte, _)| byte);
        self.text.replace_range(start..end, "");
        self.cursor = range.start;
        self.selection_anchor = None;
        self.view_top = None;
        true
    }

    pub fn clipboard_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if !key.modifiers.contains(KeyModifiers::CONTROL) || !matches!(key.code, KeyCode::Char('c' | 'x')) { return false; }
        let Some(text) = self.selected_text() else { return false; };
        match crate::clipboard::copy(&text) {
            Ok(()) if key.code == KeyCode::Char('x') => { self.delete_selection(); }
            Ok(()) => {}
            Err(error) => crate::logger::log_warn("Clipboard", &error),
        }
        true
    }

    pub fn mouse_cursor(&mut self, mouse: crossterm::event::MouseEvent, width: usize, rows: u16) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)) { return false; }
        let (lines, caret, _) = input_layout(&self.text, self.cursor, width);
        let limit = self.visible_limit(rows);
        let top = self.view_top.unwrap_or(caret.saturating_add(1).saturating_sub(limit)).min(lines.len().saturating_sub(limit));
        let start = rows.saturating_sub((lines.len().min(limit) + self.menu_rows(rows) + 2) as u16) + 1;
        if mouse.row < start || mouse.row >= start + lines.len().min(limit) as u16 {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) { self.mouse_anchor = None; self.selection_anchor = None; }
            if mouse.kind == MouseEventKind::Up(MouseButton::Left) { self.mouse_anchor = None; }
            return false;
        }
        let range = &input_ranges(&self.text, width)[top + (mouse.row - start) as usize];
        let next = self.cursor_at(range, mouse.column.saturating_sub(4) as usize);
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.selection_anchor = None;
            self.mouse_anchor = Some(next);
        } else if let Some(anchor) = self.mouse_anchor { self.selection_anchor = Some(anchor); }
        self.cursor = next;
        self.preferred_column = None;
        self.last_escape = None;
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            self.mouse_anchor = None;
            if let Some(text) = self.selected_text() {
                if let Err(error) = crate::clipboard::copy(&text) { crate::logger::log_warn("Clipboard", &error); }
            }
        }
        true
    }

    pub fn should_stop_response(&self, key: crossterm::event::KeyEvent) -> bool {
        key.code == KeyCode::Esc && self.completions().is_empty()
            && !self.last_escape.is_some_and(|last| last.elapsed() <= DOUBLE_ESCAPE_DELAY)
    }

    pub fn input_rows(&self, width: usize, rows: u16) -> usize {
        input_layout(&self.text, self.cursor, width.max(1)).0.len().min(self.visible_limit(rows))
    }

    fn visible_limit(&self, rows: u16) -> usize {
        MAX_INPUT_LINES.min((rows as usize).saturating_sub(self.menu_rows(rows) + 3).max(1))
    }

    pub fn scroll_input(&mut self, delta: i32, width: usize, rows: u16) -> bool {
        let (lines, caret, _) = input_layout(&self.text, self.cursor, width.max(1));
        let limit = self.visible_limit(rows);
        if lines.len() <= limit { return false; }
        let top = self.view_top.unwrap_or(caret.saturating_add(1).saturating_sub(limit))
            .min(lines.len() - limit);
        self.view_top = Some(if delta > 0 { top.saturating_sub(delta as usize) }
            else { top.saturating_add(delta.unsigned_abs() as usize).min(lines.len() - limit) });
        true
    }

    pub fn mouse_over_input(&self, row: u16, width: usize, rows: u16) -> bool {
        let start = rows.saturating_sub((self.input_rows(width, rows) + self.menu_rows(rows) + 2) as u16);
        row >= start && row < rows.saturating_sub(self.menu_rows(rows) as u16 + 1)
    }

    pub fn caret_visible(&self, width: usize, rows: u16) -> bool {
        let (lines, caret, _) = input_layout(&self.text, self.cursor, width.max(1));
        let limit = self.visible_limit(rows);
        let top = self.view_top.unwrap_or(caret.saturating_add(1).saturating_sub(limit))
            .min(lines.len().saturating_sub(limit));
        caret >= top && caret < top + limit
    }

    #[cfg(test)]
    pub fn commands(&self) -> Vec<SlashCommand> {
        if self.menu_dismissed { Vec::new() } else { get_matching_commands(&self.text) }
    }

    fn completions(&self) -> Vec<Completion> {
        if self.menu_dismissed { Vec::new() } else { get_completions(&self.text, self.cursor) }
    }

    pub fn menu_rows(&self, rows: u16) -> usize {
        let count = self.completions().len();
        if count == 0 { 0 } else { command_menu_slots(count, rows) + 3 }
    }

    fn backspace(&mut self) {
        if self.cursor > 0 { self.cursor -= 1; self.text.remove(self.byte_index()); }
    }

    /// Keep a single row while streaming, scrolling horizontally around the caret.
    pub fn viewport(&self, width: usize) -> (String, usize) {
        let chars: Vec<char> = self.text.chars().map(|c| if c == '\n' { '↵' } else { c }).collect();
        let cursor = self.cursor.min(chars.len());
        let mut start = cursor;
        let mut caret = 0;
        while start > 0 && caret + char_width(chars[start - 1]) < width {
            start -= 1;
            caret += char_width(chars[start]);
        }
        let mut text = String::new();
        let mut used = 0;
        for &c in &chars[start..] {
            if used + char_width(c) > width { break; }
            text.push(c);
            used += char_width(c);
        }
        (text, caret)
    }
}

fn command_menu_slots(count: usize, rows: u16) -> usize {
    count.min(5).min((rows as usize).saturating_sub(9).max(1))
}

/// Use the same command dropdown as the idle editor while reserving space for output.
pub fn render_active_command_box(branch: &str, draft: &Draft) -> std::io::Result<(u16, u16)> {
    let width = cli_ui::get_box_width();
    let (_, rows) = cli_ui::terminal_size();
    let mut editor = LineEditor { history: Vec::new(), history_index: None, prev_total_rows: 0, draft: Draft::default() };
    let mut previous_cursor_row = 0;
    let mut previously_rendered = false;
    editor.render_box(branch, &draft.text, draft.cursor, draft.view_top, draft.selection_range(), &mut previous_cursor_row, &mut previously_rendered,
        &draft.completions(), draft.menu_selected, &mut |_| {})?;
    let (lines, caret_row, caret_col) = input_layout(&draft.text, draft.cursor, width.saturating_sub(6));
    let limit = draft.visible_limit(rows);
    let top = draft.view_top.unwrap_or(caret_row.saturating_add(1).saturating_sub(limit))
        .min(lines.len().saturating_sub(limit));
    let start = rows.saturating_sub(editor.prev_total_rows as u16);
    Ok((4 + caret_col as u16, start + 1 + caret_row.saturating_sub(top).min(limit - 1) as u16))
}

pub struct LineEditor {
    history: Vec<String>,
    history_index: Option<usize>,
    prev_total_rows: usize,
    pub draft: Draft,
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
            draft: Draft::default(),
        }
    }

    pub fn remember(&mut self, line: &str) {
        self.history.push(line.to_string());
        self.append_history(line);
    }

    pub fn active_key(&mut self, key: crossterm::event::KeyEvent) -> Option<String> {
        if let Some(line) = self.draft.key(key) {
            self.remember(&line);
            return Some(line);
        }
        None
    }

    #[allow(dead_code)]
    pub fn read_line(&mut self, branch_tag: &str) -> std::io::Result<PromptResult> {
        self.read_line_with_redraw(branch_tag, |_| {})
    }

    pub fn read_line_with_redraw<F>(&mut self, branch_tag: &str, on_redraw: F) -> std::io::Result<PromptResult>
    where
        F: FnMut(&str),
    {
        self.read_line_with_updates(branch_tag, on_redraw, || false)
    }

    pub fn read_line_with_updates<F, U>(&mut self, branch_tag: &str, on_redraw: F, on_update: U) -> std::io::Result<PromptResult>
    where F: FnMut(&str), U: FnMut() -> bool,
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
        let res = self.read_line_box(branch_tag, on_redraw, on_update);
        disable_raw_mode()?;
        res
    }

    fn read_line_box<F, U>(&mut self, branch_tag: &str, mut on_redraw: F, mut on_update: U) -> std::io::Result<PromptResult>
    where F: FnMut(&str), U: FnMut() -> bool,
    {
        let mut draft = std::mem::take(&mut self.draft);
        draft.cursor = draft.cursor.min(draft.text.chars().count());
        self.history_index = None;
        self.prev_total_rows = 0;
        let mut previous_cursor_row = 0;
        let mut previously_rendered = false;
        let mut pending_event = None;
        loop {
            let matching = draft.completions();
            draft.menu_selected = draft.menu_selected.min(matching.len().saturating_sub(1));
            self.render_box(branch_tag, &draft.text, draft.cursor, draft.view_top, draft.selection_range(), &mut previous_cursor_row,
                &mut previously_rendered, &matching, draft.menu_selected, &mut on_redraw)?;
            // Scroll only content; the editor and completion menu stay fixed.
            let event = loop {
                if pending_event.is_none() && !event::poll(std::time::Duration::from_millis(100))? {
                    if on_update() { break Event::Resize(cli_ui::terminal_size().0, cli_ui::terminal_size().1); }
                    continue;
                }
                let event = pending_event.take().map(Ok).unwrap_or_else(event::read)?;
                let delta = match event {
                    Event::Mouse(mouse) => {
                        let wheel = cli_ui::history_scroll_mouse(mouse)
                            .map(|delta| cli_ui::coalesce_scroll(delta, &mut pending_event));
                        let (cols, rows) = cli_ui::terminal_size();
                        if let Some(delta) = wheel {
                            if draft.mouse_over_input(mouse.row, cols.saturating_sub(6) as usize, rows) {
                                draft.scroll_input(delta, cols.saturating_sub(6) as usize, rows);
                                break Event::Mouse(mouse);
                            }
                        }
                        wheel
                    },
                    Event::Key(key) => cli_ui::history_scroll_key(key),
                    _ => None,
                };
                if let Some(delta) = delta {
                    cli_ui::clear_mouse_selection();
                    cli_ui::scroll_history(delta);
                    on_redraw(&draft.text);
                } else { break event; }
            };
            match event {
                Event::Mouse(mouse) => {
                    let (cols, rows) = cli_ui::terminal_size();
                    if draft.mouse_cursor(mouse, cols.saturating_sub(6) as usize, rows) {
                        self.history_index = None;
                        continue;
                    }
                    match cli_ui::mouse_action(mouse) {
                        Some(cli_ui::MouseAction::Copy(text)) => {
                            if let Err(error) = crate::clipboard::copy(&text) { crate::logger::log_warn("Clipboard", &error); }
                            on_redraw(&draft.text);
                        }
                        Some(cli_ui::MouseAction::Paste) => {
                            self.history_index = None;
                            match crate::clipboard::paste() {
                                Ok(text) => draft.insert_text(&text),
                                Err(error) => crate::logger::log_warn("Clipboard", &error),
                            }
                            on_redraw(&draft.text);
                        }
                        Some(cli_ui::MouseAction::Clear) => on_redraw(&draft.text),
                        None => {}
                    }
                }
                Event::Paste(text) => {
                    self.history_index = None;
                    if cli_ui::clear_mouse_selection() { on_redraw(&draft.text); }
                    draft.insert_text(&text);
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if !matches!(key.code, KeyCode::Up | KeyCode::Down) { self.history_index = None; }
                    if cli_ui::clear_mouse_selection() { on_redraw(&draft.text); }
                    let control = key.modifiers.contains(KeyModifiers::CONTROL);
                    if draft.clipboard_key(key) { continue; }
                    if control && key.code == KeyCode::Char('v') {
                        match crate::clipboard::paste() {
                            Ok(text) => draft.insert_text(&text),
                            Err(error) => crate::logger::log_warn("Clipboard", &error),
                        }
                        continue;
                    }
                    if control && key.code == KeyCode::Char('c') {
                        if draft.text.is_empty() { return Ok(PromptResult::Exit); }
                        self.clear_box()?;
                        return Ok(PromptResult::Interrupted);
                    }
                    if control && key.code == KeyCode::Char('d') && draft.text.is_empty() {
                        return Ok(PromptResult::Exit);
                    }
                    if control && matches!(key.code, KeyCode::Char('l' | 'o')) {
                        if key.code == KeyCode::Char('o') { cli_ui::toggle_expanded_output(); }
                        on_redraw(&draft.text);
                        previously_rendered = false;
                        continue;
                    }
                    if matching.is_empty() && matches!(key.code, KeyCode::Up | KeyCode::Down)
                        && !key.modifiers.contains(KeyModifiers::SHIFT)
                        && (key.modifiers.contains(KeyModifiers::ALT)
                            || draft.text.is_empty() || self.history_index.is_some()) {
                        let next = match key.code {
                            KeyCode::Up => Some(self.history_index.map(|index| index.saturating_sub(1))
                                .unwrap_or(self.history.len().saturating_sub(1))),
                            _ => self.history_index.and_then(|index| (index + 1 < self.history.len()).then_some(index + 1)),
                        };
                        self.history_index = next;
                        draft.text = next.and_then(|index| self.history.get(index)).cloned().unwrap_or_default();
                        draft.cursor = draft.text.chars().count();
                        draft.menu_dismissed = true;
                    } else if let Some(line) = draft.key(key) {
                        if self.history.last() != Some(&line) {
                            self.append_history(&line);
                            self.history.push(line.clone());
                        }
                        let was_scrolled = cli_ui::history_scrolled();
                        cli_ui::scroll_history(i32::MIN);
                        if was_scrolled { on_redraw(&line); }
                        self.finish_input(branch_tag)?;
                        return Ok(PromptResult::Line(line));
                    }
                }
                Event::Resize(_, _) => {
                    cli_ui::clear_mouse_selection();
                    previously_rendered = false;
                    self.prev_total_rows = 0;
                    on_redraw(&draft.text);
                }
                _ => {}
            }
        }
    }

    fn finish_input(&self, branch_tag: &str) -> std::io::Result<()> {
        let (_, term_rows) = cli_ui::terminal_size();
        let start_row = term_rows.saturating_sub(self.prev_total_rows as u16);

        let mut out = stdout().lock();
        execute!(out, cursor::MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
        cli_ui::render_bottom_box("", branch_tag);
        let _ = execute!(out, cursor::Hide);
        out.flush()?;
        Ok(())
    }

    fn clear_box(&self) -> std::io::Result<()> {
        let (_, term_rows) = cli_ui::terminal_size();
        let start_row = term_rows.saturating_sub(self.prev_total_rows as u16);

        let mut out = stdout().lock();
        execute!(out, cursor::MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
        out.flush()?;
        Ok(())
    }

    fn render_box<F>(
        &mut self,
        branch_tag: &str,
        buffer: &str,
        cursor_char_idx: usize,
        view_top: Option<usize>,
        selection: Option<std::ops::Range<usize>>,
        prev_cursor_row: &mut usize,
        prev_rendered: &mut bool,
        menu_items: &[Completion],
        menu_selected_idx: usize,
        on_redraw: &mut F,
    ) -> std::io::Result<()>
    where
        F: FnMut(&str),
    {
        let _geometry = cli_ui::begin_terminal_frame();
        if cli_ui::terminal_is_small() {
            cli_ui::render_small_terminal();
            self.prev_total_rows = 0;
            *prev_rendered = false;
            return Ok(());
        }
        let box_width = cli_ui::get_box_width();
        let max_content_chars = box_width.saturating_sub(6);

        let (mut content_lines, mut cursor_row, caret_col) = input_layout(buffer, cursor_char_idx, max_content_chars);
        let cursor_col = 4 + caret_col;

        let (_, term_rows) = cli_ui::terminal_size();
        let menu_active = !menu_items.is_empty();
        let visible_slots = if menu_active {
            command_menu_slots(menu_items.len(), term_rows)
        } else {
            0
        };
        let menu_rows = if menu_active {
            1 + visible_slots + 1 + 1 // divider + visible slots + status/overflow + hints
        } else {
            0
        };

        let visible_content = MAX_INPUT_LINES.min((term_rows as usize).saturating_sub(menu_rows + 3).max(1));
        let first = view_top.unwrap_or(cursor_row.saturating_add(1).saturating_sub(visible_content))
            .min(content_lines.len().saturating_sub(visible_content));
        let cursor_visible = cursor_row >= first && cursor_row < first + visible_content;
        let total_content = content_lines.len();
        content_lines = content_lines[first..(first + visible_content).min(total_content)].to_vec();
        cursor_row = cursor_row.saturating_sub(first).min(content_lines.len() - 1);
        let total_rows = 1 + content_lines.len() + menu_rows + 1;
        let start_row = term_rows.saturating_sub(total_rows as u16);

        let _update = cli_ui::begin_content_update();
        let changed_height = total_rows != self.prev_total_rows;
        cli_ui::set_input_rows(total_rows as u16);
        if changed_height { on_redraw(buffer); }
        let mut out = stdout().lock();
        queue!(out, cursor::Hide)?;

        self.prev_total_rows = total_rows;
        cli_ui::set_input_rows(total_rows as u16);

        // 1. Draw Top border: ╭─ You [branch] ────────────────────────────────────────╮
        let queued = cli_ui::pending_prompt_count();
        let mut title_tag = if queued > 0 {
            format!(" You [{queued} queued] ")
        } else if branch_tag.is_empty() {
            " You ".to_string()
        } else {
            format!(" You [{}] ", branch_tag.trim())
        };
        if total_content > visible_content {
            title_tag = format!("{} · {}-{}/{} ", title_tag.trim_end(), first + 1,
                first + content_lines.len(), total_content);
        }
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
                let source_start = input_ranges(buffer, max_content_chars)[first + i].start;
                let display = if let Some(range) = selection.as_ref() {
                    let chars: Vec<_> = line.chars().collect();
                    let begin = range.start.saturating_sub(source_start).min(chars.len());
                    let end = range.end.saturating_sub(source_start).min(chars.len());
                    format!("{}\x1b[7m{}\x1b[27m{}", chars[..begin].iter().collect::<String>(),
                        chars[begin..end].iter().collect::<String>(), chars[end..].iter().collect::<String>())
                } else { line.clone() };
                queue!(
                    out,
                    SetForegroundColor(primary_color),
                    Print(display),
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
                    let name_width = if item.is_skill { 24 } else { 14 };
                    let cmd_name = truncate_visible(&item.name, inner_w.saturating_sub(4).min(name_width));
                    let cmd_w = str_width(&cmd_name);
                    let col_w = name_width.min(inner_w.saturating_sub(4));
                    let cmd_pad = col_w.saturating_sub(cmd_w);

                    let desc_avail = inner_w.saturating_sub(2 + col_w + 2);
                    let desc_trunc = if desc_avail > 5 {
                        truncate_visible(&item.description, desc_avail)
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
                            Print(&cmd_name),
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
                            Print(&cmd_name),
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
            let kind = if menu_items[0].is_skill { "skills" } else { "commands" };
            let status_text = if menu_items.len() > visible_slots {
                let remaining = menu_items.len().saturating_sub(end_idx);
                if remaining > 0 {
                    format!("↓ {} more  ({}/{} {kind})", remaining, menu_selected_idx + 1, menu_items.len())
                } else {
                    format!("↑ {} above  ({}/{} {kind})", scroll_offset, menu_selected_idx + 1, menu_items.len())
                }
            } else {
                format!("{}/{} {kind}", menu_selected_idx + 1, menu_items.len())
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
        queue!(out, cursor::MoveTo(cursor_col as u16, target_cursor_y))?;
        if cursor_visible { queue!(out, cursor::Show)?; } else { queue!(out, cursor::Hide)?; }
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
    fn skill_completions_filter_names_and_preserve_surrounding_unicode_text() {
        let skills = ["design", "database"].map(|name| crate::skills::Skill {
            name: name.into(), description: "instructions".into(), path: format!("/tmp/{name}/SKILL.md"),
            directory: format!("/tmp/{name}"), is_workspace: name == "design",
        });
        assert_eq!(skill_matches("$", 1, &skills).len(), 2);
        let mut text = "Привет $de-old друг".to_string();
        let mut cursor = 10;
        let matching = skill_matches(&text, cursor, &skills);
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].name, "$design");
        apply_completion(&mut text, &mut cursor, &matching[0]);
        assert_eq!(text, "Привет $design друг");
        assert_eq!(cursor, 15);
        assert!(skill_matches("price$de", 8, &skills).is_empty());
        assert!(skill_matches("$design text", 12, &skills).is_empty());
        assert!(skill_matches("$unknown", 8, &skills).is_empty());
    }

    #[test]
    fn skill_insertion_preserves_unicode_draft_and_caret() {
        let mut draft = Draft { text: "Привет мир".into(), cursor: 7, ..Draft::default() };
        draft.insert_text("$design ");
        assert_eq!(draft.text, "Привет $design мир");
        assert_eq!(draft.cursor, 15);
        assert!(draft.menu_dismissed);
    }

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

#[cfg(test)]
mod draft_tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }

    #[test]
    fn editing_and_submitting_unicode_does_not_split_characters() {
        let mut draft = Draft::default();
        for c in "я🙂界".chars() { draft.key(key(KeyCode::Char(c))); }
        draft.key(key(KeyCode::Left));
        draft.key(key(KeyCode::Backspace));
        assert_eq!(draft.text, "я界");
        draft.key(key(KeyCode::Char('ю')));
        draft.key(key(KeyCode::Delete));
        assert_eq!(draft.key(key(KeyCode::Enter)).as_deref(), Some("яю"));
        assert_eq!(draft, Draft::default());
    }

    #[test]
    fn streaming_viewport_keeps_caret_inside_field_without_losing_draft() {
        let mut draft = Draft { text: "а界🙂бвгдежз".into(), cursor: 10, ..Draft::default() };
        let original = draft.text.clone();
        for width in [1, 2, 4, 8, 30] {
            let (text, caret) = draft.viewport(width);
            assert!(str_width(&text) <= width);
            assert!(caret < width);
        }
        draft.key(key(KeyCode::Home));
        let (text, caret) = draft.viewport(4);
        assert_eq!(caret, 0);
        assert!(text.starts_with('а'));
        assert_eq!(draft.text, original);
    }

    #[test]
    fn busy_menu_filters_navigates_completes_and_submits_selected_command() {
        let mut draft = Draft::default();
        draft.key(key(KeyCode::Char('/')));
        assert_eq!(draft.commands().len(), SLASH_COMMANDS.len());
        draft.key(key(KeyCode::Char('t')));
        assert_eq!(draft.commands().iter().map(|c| c.name).collect::<Vec<_>>(), vec!["/theme", "/tools"]);
        draft.key(key(KeyCode::Down));
        assert_eq!(draft.menu_selected, 1);
        assert_eq!(draft.key(key(KeyCode::Enter)).as_deref(), Some("/tools"));
        assert_eq!(draft, Draft::default());
        for c in "/th".chars() { draft.key(key(KeyCode::Char(c))); }
        draft.key(key(KeyCode::Tab));
        assert_eq!(draft.text, "/theme ");
        assert!(draft.commands().is_empty());
        draft.key(key(KeyCode::Char('a')));
        assert_eq!(draft.key(key(KeyCode::Enter)).as_deref(), Some("/theme a"));
    }

    #[test]
    fn busy_menu_escape_and_geometry_leave_room_for_response() {
        let mut draft = Draft { text: "/".into(), cursor: 1, ..Draft::default() };
        for rows in [8, 10, 16, 26] {
            assert!(draft.menu_rows(rows) > 0);
            assert!(3 + draft.menu_rows(rows) < rows as usize);
        }
        draft.key(key(KeyCode::Esc));
        assert!(draft.commands().is_empty());
        assert_eq!(draft.menu_rows(26), 0);
        assert_eq!(draft.text, "/");
        draft.key(key(KeyCode::Char('t')));
        assert!(!draft.commands().is_empty());
        draft.key(key(KeyCode::Down));
        draft.key(key(KeyCode::Up));
        assert_eq!(draft.menu_selected, 0);
    }

    #[test]
    fn active_command_completion_and_word_delete() {
        let mut draft = Draft { text: "/usa".into(), cursor: 4, ..Draft::default() };
        draft.key(key(KeyCode::Tab));
        assert_eq!(draft.key(key(KeyCode::Enter)).as_deref(), Some("/usage"));
        draft = Draft { text: "первое второе ".into(), cursor: 14, ..Draft::default() };
        draft.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(draft.text, "первое ");
    }
}

#[cfg(test)]
mod stop_key_tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn escape_dismisses_open_menu_before_stopping_response_and_keeps_draft() {
        let escape = crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut draft = Draft { text: "/t".into(), cursor: 2, ..Draft::default() };
        assert!(!draft.should_stop_response(escape));
        draft.key(escape);
        assert!(!draft.should_stop_response(escape));
        assert_eq!(draft.text, "/t");
        draft.key(escape);
        assert!(draft.text.is_empty());
        assert_eq!(draft.cursor, 0);
        let plain = Draft { text: "unfinished".into(), cursor: 10, ..Draft::default() };
        assert!(plain.should_stop_response(escape));
        assert_eq!(plain.text, "unfinished");
    }

    #[test]
    fn delayed_escape_and_typing_break_the_double_escape_sequence() {
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let mut draft = Draft { text: "long draft".into(), cursor: 10, ..Draft::default() };
        draft.key(escape);
        draft.last_escape = Some(std::time::Instant::now() - DOUBLE_ESCAPE_DELAY - std::time::Duration::from_millis(1));
        assert!(draft.should_stop_response(escape));
        draft.key(escape);
        assert_eq!(draft.text, "long draft");
        draft.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        draft.key(escape);
        assert_eq!(draft.text, "long draft!");
        draft.key(escape);
        assert!(draft.text.is_empty());
    }

    #[test]
    fn vertical_cursor_keeps_its_column_across_short_lines() {
        let mut draft = Draft { text: "alpha beta\nz\nalpha beta".into(), cursor: 23, ..Draft::default() };
        draft.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(draft.cursor, 12);
        draft.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(draft.cursor, 10);
        draft.key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(draft.cursor, 6);
        draft.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(draft.text, "alpha !beta\nz\nalpha beta");
        draft.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(draft.cursor, 0);
    }

    #[test]
    fn multiline_layout_and_selected_text_replacement_preserve_unicode() {
        assert_eq!(input_layout("first\n\nlast\n", 12, 80).0, ["first", "", "last", ""]);
        let mut draft = Draft { text: "Привет мир".into(), cursor: 7, ..Draft::default() };
        for _ in 0..3 { draft.key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)); }
        assert_eq!(draft.selected_text().as_deref(), Some("мир"));
        draft.insert_text("свет");
        assert_eq!(draft.text, "Привет свет");
        draft.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(draft.text, "Привет свет\n");
        assert_eq!(input_layout(&draft.text, draft.cursor, 80).1, 1);
    }
}
