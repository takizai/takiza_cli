use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::{cursor, execute, queue};
use serde::{Deserialize, Serialize};
use std::io::{stdout, IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use crate::prompt::{char_width, str_width};

/// Own one terminal screen for the lifetime of the application.
/// Redrawing and scrolling must never append application frames to shell scrollback.
pub struct TerminalScreen {
    active: bool,
}

impl TerminalScreen {
    pub fn enter() -> std::io::Result<Self> {
        let active = stdout().is_terminal();
        let screen = Self { active };
        if active {
            execute!(stdout(), crossterm::terminal::EndSynchronizedUpdate)?;
            // Windows can restore mouse modes only after EnableMouseCapture has
            // saved the original console mode. ANSI terminals can reset stale
            // mouse capture immediately, including after an interrupted session.
            #[cfg(not(windows))]
            execute!(stdout(), crossterm::event::DisableMouseCapture)?;
            execute!(stdout(), crossterm::event::DisableBracketedPaste,
                ResetColor, crossterm::terminal::EnterAlternateScreen,
                crossterm::event::EnableMouseCapture, crossterm::event::EnableBracketedPaste)?;
        }
        Ok(screen)
    }
}

impl Drop for TerminalScreen {
    fn drop(&mut self) {
        if self.active {
            let mut out = stdout();
            let _ = execute!(out, crossterm::terminal::EndSynchronizedUpdate,
                ResetColor, cursor::Show, crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableMouseCapture, crossterm::terminal::LeaveAlternateScreen);
            let _ = out.flush();
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[derive(Default)]
struct HistoryViewport { offset: usize, total: usize }

impl HistoryViewport {
    fn scroll(&mut self, delta: i32) {
        if delta == i32::MIN { self.offset = 0; self.total = 0; }
        else if delta > 0 {
            if self.offset == 0 { self.total = 0; }
            self.offset = self.offset.saturating_add(delta as usize);
        }
        else { self.offset = self.offset.saturating_sub(delta.unsigned_abs() as usize); }
    }

    fn range(&mut self, total: usize, height: usize) -> std::ops::Range<usize> {
        if self.offset > 0 && self.total > 0 && total > self.total {
            self.offset = self.offset.saturating_add(total - self.total);
        }
        self.total = total;
        self.offset = self.offset.min(total.saturating_sub(height));
        let end = total.saturating_sub(self.offset);
        end.saturating_sub(height)..end
    }
}

static HISTORY_VIEW: Mutex<HistoryViewport> = Mutex::new(HistoryViewport { offset: 0, total: 0 });

pub fn history_scrolled() -> bool { HISTORY_VIEW.lock().unwrap().offset > 0 }
pub fn scroll_history(delta: i32) { HISTORY_VIEW.lock().unwrap().scroll(delta); }

pub fn history_scroll_key(key: crossterm::event::KeyEvent) -> Option<i32> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let page = (terminal_size().1 / 2).max(1) as i32;
    match key.code {
        KeyCode::PageUp => Some(page), KeyCode::PageDown => Some(-page),
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => Some(i32::MAX),
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => Some(i32::MIN),
        _ => None,
    }
}

pub fn history_scroll_mouse(mouse: crossterm::event::MouseEvent) -> Option<i32> {
    use crossterm::event::MouseEventKind;
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(3), MouseEventKind::ScrollDown => Some(-3), _ => None,
    }
}

/// Combine a queued wheel burst into one frame, preserving the next key/resize
/// and direction changes so cancellation cannot be swallowed by scrolling.
pub fn coalesce_scroll(mut delta: i32, pending: &mut Option<crossterm::event::Event>) -> i32 {
    for _ in 0..255 {
        if !crossterm::event::poll(Duration::ZERO).unwrap_or(false) { break; }
        let Ok(event) = crossterm::event::read() else { break; };
        if let crossterm::event::Event::Mouse(mouse) = event {
            if let Some(next) = history_scroll_mouse(mouse) {
                if next.signum() == delta.signum() {
                    delta = delta.saturating_add(next);
                    continue;
                }
            }
        }
        *pending = Some(event);
        break;
    }
    delta
}

static INPUT_ROWS: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(3);
static CONTENT_FRAME: Mutex<Option<(u16, u16, Vec<String>)>> = Mutex::new(None);
// Preserve the undecorated transcript for mouse selection and clipboard copying.
static TEXT_FRAME: Mutex<Option<(u16, u16, Vec<String>)>> = Mutex::new(None);

struct SnowSurface {
    top: u16,
    size: (u16, u16),
    text: Vec<String>,
    painted: Vec<String>,
}
static SNOW_SURFACE: Mutex<Option<SnowSurface>> = Mutex::new(None);

fn remember_snow_surface(top: u16, cols: u16, rows: u16, text: Vec<String>, painted: Vec<String>) {
    *SNOW_SURFACE.lock().unwrap() = crate::snow::enabled().then_some(SnowSurface { top, size: (cols, rows), text, painted });
}

/// Animate the existing background without touching input, menus or the caret.
pub fn animate_background() {
    if !crate::snow::due() || terminal_is_small() || !stdout().is_terminal()
        || MOUSE_SELECTION.lock().unwrap().is_some() { return; }
    let mut surface = SNOW_SURFACE.lock().unwrap();
    let Some(surface) = surface.as_mut() else { return; };
    if surface.size != terminal_size() { return; }
    let snow = crate::snow::Frame::new(surface.size.0.saturating_sub(1) as usize, surface.text.len());
    let painted = surface.text.iter().enumerate().map(|(row, text)| snow.line(text, row)).collect::<Vec<_>>();
    let mut frame = Vec::new();
    let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate, cursor::SavePosition, ResetColor);
    for (row, line) in painted.iter().enumerate() {
        if surface.painted.get(row) == Some(line) { continue; }
        let _ = queue!(frame, cursor::MoveTo(0, surface.top + row as u16),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(line), ResetColor);
    }
    let _ = queue!(frame, cursor::RestorePosition, crossterm::terminal::EndSynchronizedUpdate);
    {
        let mut out = stdout().lock();
        let _ = out.write_all(&frame); let _ = out.flush();
    }
    // Keep the main renderer's diff cache synchronized with decorative frames.
    if surface.top == 0 {
        if let Some((cols, rows, lines)) = CONTENT_FRAME.lock().unwrap().as_mut() {
            if (*cols, *rows) == surface.size {
                for (row, line) in painted.iter().enumerate() {
                    if let Some(cached) = lines.get_mut(row) { *cached = line.clone(); }
                }
            }
        }
    }
    surface.painted = painted;
}

pub fn read_event_with_background() -> std::io::Result<crossterm::event::Event> {
    if !crate::snow::enabled() { return crossterm::event::read(); }
    loop {
        if crossterm::event::poll(crate::snow::FRAME_INTERVAL)? { return crossterm::event::read(); }
        animate_background();
    }
}

/// Onboarding owns its banner; only the gap above its panel is animated.
pub(crate) fn paint_onboarding_snow(out: &mut impl Write, top: u16, bottom: u16) -> std::io::Result<()> {
    let (cols, rows) = terminal_size();
    let text = vec![String::new(); bottom.saturating_sub(top) as usize];
    let snow = crate::snow::Frame::new(cols.saturating_sub(1) as usize, text.len());
    let painted = text.iter().enumerate().map(|(row, text)| snow.line(text, row)).collect::<Vec<_>>();
    for (row, line) in painted.iter().enumerate() {
        queue!(out, cursor::MoveTo(0, top + row as u16), crossterm::style::Print(line), ResetColor)?;
    }
    remember_snow_surface(top, cols, rows, text, painted);
    Ok(())
}

#[derive(Clone)]
struct MouseSelection {
    start: (u16, u16),
    end: (u16, u16),
    size: (u16, u16),
    lines: Vec<String>,
}

static MOUSE_SELECTION: Mutex<Option<MouseSelection>> = Mutex::new(None);

pub enum MouseAction { Copy(String), Paste, Clear }

pub fn clear_mouse_selection() -> bool {
    MOUSE_SELECTION.lock().unwrap().take().is_some()
}

fn column_slice(text: &str, start: usize, end: usize) -> String {
    let mut column = 0;
    let mut previous_selected = false;
    text.chars().filter(|&c| {
        let width = char_width(c);
        if width == 0 { return previous_selected; }
        let selected = column >= start && column < end;
        column += width;
        previous_selected = selected;
        selected
    }).collect()
}

impl MouseSelection {
    fn bounds(&self) -> ((u16, u16), (u16, u16)) {
        if self.start <= self.end { (self.start, self.end) } else { (self.end, self.start) }
    }

    fn columns(&self, row: u16) -> Option<(usize, usize)> {
        let ((first_row, first_col), (last_row, last_col)) = self.bounds();
        if row < first_row || row > last_row { return None; }
        Some((if row == first_row { first_col as usize } else { 0 },
            if row == last_row { last_col as usize + 1 } else { self.size.0 as usize }))
    }

    fn text(&self) -> String {
        self.lines.iter().enumerate().filter_map(|(row, line)| {
            self.columns(row as u16).map(|(start, end)| column_slice(&plain_terminal_text(line), start, end).trim_end().to_string())
        }).collect::<Vec<_>>().join("\n")
    }

    fn highlighted(&self) -> Vec<String> {
        self.lines.iter().enumerate().map(|(row, original)| {
            let Some((start, end)) = self.columns(row as u16) else { return original.clone(); };
            let line = plain_terminal_text(original);
            format!("{}\x1b[7m{}\x1b[0m{}", column_slice(&line, 0, start),
                column_slice(&line, start, end), column_slice(&line, end, usize::MAX))
        }).collect()
    }
}

pub fn mouse_action(mouse: crossterm::event::MouseEvent) -> Option<MouseAction> {
    use crossterm::event::{MouseButton, MouseEventKind};
    if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
        clear_mouse_selection();
        return Some(MouseAction::Paste);
    }
    let (cols, rows) = terminal_size();
    let content_rows = output_limit(rows);
    let point = (mouse.row.min(content_rows.saturating_sub(1)), mouse.column.min(cols.saturating_sub(1)));
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if mouse.row < content_rows => {
            let snapshot = TEXT_FRAME.lock().unwrap().clone().or_else(|| CONTENT_FRAME.lock().unwrap().clone());
            if let Some((w, h, mut lines)) = snapshot {
                lines.truncate(content_rows as usize);
                *MOUSE_SELECTION.lock().unwrap() = Some(MouseSelection {
                    start: point, end: point, size: (w, h), lines,
                });
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(selection) = MOUSE_SELECTION.lock().unwrap().as_mut() { selection.end = point; }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(mut selection) = MOUSE_SELECTION.lock().unwrap().take() {
                selection.end = point;
                // A single click is not a text selection.
                if selection.end != selection.start { return Some(MouseAction::Copy(selection.text())); }
                return Some(MouseAction::Clear);
            }
        }
        _ => return None,
    }
    let selection = MOUSE_SELECTION.lock().unwrap().clone();
    if let Some(selection) = selection {
        let highlighted = selection.highlighted();
        let mut cached = CONTENT_FRAME.lock().unwrap();
        let mut frame = Vec::new();
        let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate, cursor::SavePosition);
        for (row, line) in highlighted.iter().enumerate() {
            let _ = queue!(frame, cursor::MoveTo(0, row as u16), ResetColor,
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(line), ResetColor);
        }
        let _ = queue!(frame, cursor::RestorePosition, crossterm::terminal::EndSynchronizedUpdate);
        let mut out = stdout().lock();
        let _ = out.write_all(&frame);
        let _ = out.flush();
        if let Some((_, _, lines)) = cached.as_mut() {
            for (row, line) in highlighted.into_iter().enumerate() { if row < lines.len() { lines[row] = line; } }
        }
    }
    None
}

pub fn set_input_rows(rows: u16) {
    INPUT_ROWS.store(rows, std::sync::atomic::Ordering::Relaxed);
}

pub fn invalidate_content_frame() {
    *CONTENT_FRAME.lock().unwrap() = None;
    *TEXT_FRAME.lock().unwrap() = None;
}

static PENDING_PROMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn pending_prompt_count() -> usize {
    PENDING_PROMPTS.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_pending_prompt_count(count: usize) {
    PENDING_PROMPTS.store(count, std::sync::atomic::Ordering::Relaxed);
}

static ACTIVE_DRAFT: std::sync::RwLock<Option<crate::prompt::Draft>> = std::sync::RwLock::new(None);

pub fn set_active_draft(draft: Option<crate::prompt::Draft>) {
    *ACTIVE_DRAFT.write().unwrap() = draft;
}

pub fn position_input_cursor(x: u16, y: u16) {
    let (cols, rows) = terminal_size();
    let visible = active_draft().is_none_or(|draft| draft.caret_visible(cols.saturating_sub(6) as usize, rows));
    let mut out = stdout().lock();
    let _ = execute!(out, cursor::MoveTo(x, y));
    if visible { let _ = execute!(out, cursor::Show); } else { let _ = execute!(out, cursor::Hide); }
}

fn active_draft() -> Option<crate::prompt::Draft> {
    ACTIVE_DRAFT.read().unwrap().clone()
}

fn active_box_rows(rows: u16) -> Option<u16> {
    active_draft().map(|draft| 2 + draft.input_rows(terminal_size().0.saturating_sub(6) as usize, rows) as u16 + draft.menu_rows(rows) as u16)
}

fn output_limit(rows: u16) -> u16 {
    rows.saturating_sub(active_box_rows(rows).unwrap_or(3) + 1)
}

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

pub fn set_active_chat_title(title: &str) {
    if let Ok(mut lock) = ACTIVE_CHAT_TITLE.write() {
        *lock = title.to_string();
    }
}

pub fn get_active_chat_title() -> String {
    ACTIVE_CHAT_TITLE.read().map(|g| g.clone()).unwrap_or_default()
}

pub struct ContentUpdate { outer: bool }
thread_local! { static CONTENT_UPDATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

pub fn begin_content_update() -> ContentUpdate {
    let outer = CONTENT_UPDATE.with(|active| !active.replace(true));
    if outer { let _ = execute!(stdout(), crossterm::terminal::BeginSynchronizedUpdate); }
    ContentUpdate { outer }
}

impl Drop for ContentUpdate {
    fn drop(&mut self) {
        if self.outer {
            CONTENT_UPDATE.with(|active| active.set(false));
            let _ = execute!(stdout(), crossterm::terminal::EndSynchronizedUpdate);
        }
    }
}

thread_local! {
    static FRAME_SIZE: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
}

pub fn terminal_size() -> (u16, u16) {
    FRAME_SIZE.with(|size| size.get()).unwrap_or_else(|| {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        (cols.max(1), rows.max(1))
    })
}

pub struct TerminalFrame {
    previous: Option<(u16, u16)>,
}

impl Drop for TerminalFrame {
    fn drop(&mut self) {
        FRAME_SIZE.with(|size| size.set(self.previous));
    }
}

pub fn begin_terminal_frame() -> TerminalFrame {
    let dimensions = terminal_size();
    let previous = FRAME_SIZE.with(|size| size.replace(Some(dimensions)));
    TerminalFrame { previous }
}

pub fn terminal_is_small() -> bool {
    let (cols, rows) = terminal_size();
    cols < 20 || rows < 8
}

pub fn render_small_terminal() {
    let (cols, _) = terminal_size();
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide, crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
        cursor::MoveTo(0, 0), crossterm::style::Print(crate::prompt::truncate_visible(crate::i18n::tr("Resize terminal"), cols.saturating_sub(1) as usize)));
    let _ = out.flush();
}

pub fn get_box_width() -> usize {
    let (term_cols, _) = terminal_size();
    (term_cols as usize).max(1)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    crate::prompt::truncate_visible(s, max_len)
}

pub fn print_banner_to(out: &mut impl Write, model: &str, base_url: &str, workspace: &str, git_info: &str) -> u16 {
    if terminal_is_small() {
        render_small_terminal();
        return 0;
    }
    let (term_cols, term_rows) = terminal_size();
    if term_cols < 65 || term_rows < 18 {
        let lines = ["Takiza Code".to_string(), crate::i18n::tf!("Model: {}", model),
            format!("Git: {}", git_info)];
        for (row, line) in lines.iter().enumerate() {
            let _ = queue!(out, cursor::MoveTo(0, row as u16),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(crate::prompt::truncate_visible(line, term_cols.saturating_sub(1) as usize)));
        }
        return lines.len() as u16;
    }
    let mut row: u16 = 0;

    let _ = queue!(out, cursor::MoveTo(0, row), crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine));
    row += 1;

    let th = crate::theme::current();
    let p_ansi = th.primary_ansi();

    if term_cols >= 65 {
        let info_w = (term_cols as usize).saturating_sub(32).min(50);
        let div_w = info_w.min(48);
        let divider = "─".repeat(div_w);

        let mut info_lines = vec![
            format!("{}TAKIZA \x1b[1;38;2;245;245;250mCODE\x1b[0m  \x1b[38;2;120;120;125mv{}\x1b[0m", p_ansi, env!("TAKIZA_VERSION")),
            format!("\x1b[38;2;160;160;165m{}\x1b[0m", truncate_str(crate::i18n::tr("Autonomous AI Software Engineering Agent"), info_w)),
            format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider),
        ];
        for (label, value, color) in [
            ("Model", model, "\x1b[38;2;230;230;235m"),
            ("Endpoint", base_url, "\x1b[38;2;170;170;175m"),
            ("Workspace", workspace, "\x1b[38;2;170;170;175m"),
            ("Git", git_info, "\x1b[38;2;130;215;145m"),
        ] {
            let label = crate::i18n::tr(label);
            let padding = " ".repeat(10usize.saturating_sub(str_width(label)));
            let value = truncate_str(value, info_w.saturating_sub(13));
            info_lines.push(format!("{p_ansi}{label}{padding}\x1b[38;2;70;70;75m│\x1b[0m {color}{value}\x1b[0m"));
        }
        info_lines.push(format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider));

        for i in 0..crate::logo::LOGO_HEIGHT {
            let logo_part = crate::logo::LOGO_LINES[i];
            let info_part = if i < info_lines.len() { &info_lines[i] } else { "" };
            let info_part = crate::interactive::fit_menu_text(info_part, info_w);
            let _ = queue!(
                out,
                cursor::MoveTo(0, row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(format!(" {}  {}", logo_part, info_part))
            );
            row += 1;
        }
    } else {
        // Compact layout for very narrow terminals (< 65 columns): show logo on top, then box below
        let pad_left = (term_cols as usize).saturating_sub(crate::logo::LOGO_WIDTH) / 2;
        let pad_str = " ".repeat(pad_left);
        for i in 0..crate::logo::LOGO_HEIGHT {
            let logo_part = crate::logo::LOGO_LINES[i];
            let _ = queue!(
                out,
                cursor::MoveTo(0, row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(format!("{}{}", pad_str, logo_part))
            );
            row += 1;
        }

        let box_w = (term_cols as usize).saturating_sub(4).max(30);
        let inner_w = box_w.saturating_sub(4);
        let val_w = inner_w.saturating_sub(11);
        let top_div_len = box_w.saturating_sub(23);
        let top_div = "─".repeat(top_div_len);
        let bot_div = "─".repeat(box_w.saturating_sub(2));

        let lines = [
            format!("  {}╭─ TAKIZA \x1b[38;2;245;245;250mCODE\x1b[0m \x1b[38;2;120;120;125mv{}{} {}╮\x1b[0m", p_ansi, env!("TAKIZA_VERSION"), p_ansi, top_div),
            crate::i18n::tf!("  {}│\x1b[0m {}Model:     \x1b[0m\x1b[38;2;230;230;235m{}\x1b[0m", p_ansi, p_ansi, truncate_str(model, val_w)),
            crate::i18n::tf!("  {}│\x1b[0m {}Endpoint:  \x1b[0m\x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, p_ansi, truncate_str(base_url, val_w)),
            crate::i18n::tf!("  {}│\x1b[0m {}Workspace: \x1b[0m\x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, p_ansi, truncate_str(workspace, val_w)),
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

    row
}

pub fn print_user_cmd(cmd: &str) {
    let color = crate::theme::current().primary_ansi();
    println!("\x1b[1m{color}❯ {}\x1b[0m\n", plain_terminal_text(cmd));
}

pub fn print_help() {
    let th = crate::theme::current();
    println!();
    execute!(stdout(), SetForegroundColor(th.primary_crossterm())).ok();
    println!("{}", crate::i18n::tf!("Type while the agent responds; submitted prompts wait in FIFO order."));
    println!("{}", crate::i18n::tf!("Slash commands run immediately. Changing session/model interrupts the active response.\n"));
    println!("{}", crate::i18n::tf!("Available Slash Commands:"));
    println!("  /config             - {}", crate::i18n::tr("Configure language, theme, provider, model and permissions"));
    println!("{}", crate::i18n::tf!("  /help               - Show this help summary"));
    println!("{}", crate::i18n::tf!("  /clear              - Start a new chat and clear conversation history"));
    println!("{}", crate::i18n::tf!("  /theme [name]       - View or switch visual theme (amber, cyberpunk, emerald, nord, monochrome)"));
    println!("{}", crate::i18n::tf!("  /mode [manual|moa]  - Switch execution mode (Takiza Manual: pick model, Takiza MoA: auto)"));
    println!("{}", crate::i18n::tf!("  /model <name>       - Switch model (e.g. /model gpt-4o, /model claude-3-5-sonnet)"));
    println!("{}", crate::i18n::tf!("  /provider <name>    - Switch preset provider (openai, openrouter, deepseek, ollama, groq)"));
    println!("{}", crate::i18n::tf!("  /rewind, /restore   - Restore files and chat from a pre-prompt checkpoint"));
    println!("  /compact            - {}", crate::i18n::tr("Compress model context while keeping the visible conversation"));
    println!("{}", crate::i18n::tf!("  /diff               - View git diff of changes made in the workspace"));
    println!("{}", crate::i18n::tf!("  /status             - View current session info, git status, and token usage"));
    println!("{}", crate::i18n::tf!("  /usage              - View daily quotas and token usage breakdown (Manual / MoA)"));
    println!("{}", crate::i18n::tf!("  /sessions           - List saved conversation sessions"));
    println!("{}", crate::i18n::tf!("  /reset              - Reset conversation history to start fresh"));
    println!("{}", crate::i18n::tf!("  /approval           - Toggle auto-approving commands vs asking permissions"));
    println!("{}", crate::i18n::tf!("  /tools              - List available agent tools"));
    println!("{}", crate::i18n::tf!("  /skills             - Choose a skill (type to search, Enter to insert, F5 to rescan)"));
    println!("{}", crate::i18n::tf!("  /exit or /quit      - Exit Takiza Code"));
    println!("{}", crate::i18n::tf!("  !<shell command>    - Execute bash command directly (e.g. !git status, !cargo test)"));
    println!("{}", crate::i18n::tf!("  Ctrl+O              - Toggle expansion of command/search output (max 5 lines in standard mode)"));
    println!("{}", crate::i18n::tf!("\nCLI Flags (on startup):"));
    println!("{}", crate::i18n::tf!("  -c, --continue      - Continue previous session"));
    println!("{}", crate::i18n::tf!("  -y, --yes, -a       - Skip command permissions (default: ask permission before running commands)"));
    execute!(stdout(), ResetColor).ok();
    println!();
}

pub fn render_bottom_box(prompt: &str, branch_tag: &str) -> (u16, u16) {
    let _frame = begin_terminal_frame();
    if terminal_is_small() {
        render_small_terminal();
        return (0, 0);
    }
    let mut out = stdout().lock();
    let (_term_cols, term_rows) = terminal_size();
    let box_width = get_box_width();
    let max_content = box_width.saturating_sub(6);
    let draft = active_draft();
    if let Some(draft) = draft.as_ref() {
        return crate::prompt::render_active_command_box(branch_tag, draft).unwrap_or((0, 0));
    }
    let viewport = draft.as_ref().map(|d| d.viewport(max_content));
    let prompt = viewport.as_ref().map(|(text, _)| text.as_str()).unwrap_or(prompt);

    let queued = PENDING_PROMPTS.load(std::sync::atomic::Ordering::Relaxed);
    let title_tag = if queued > 0 {
        crate::i18n::tf!(" You [{queued} queued] ", queued = queued)
    } else if branch_tag.is_empty() {
        crate::i18n::tr(" You ").to_string()
    } else {
        crate::i18n::tf!(" You [{}] ", branch_tag.trim())
    };
    let title_w = str_width(&title_tag);
    let (safe_title, safe_title_w) = if title_w + 4 >= box_width {
        (crate::i18n::tr(" You ").to_string(), 5)
    } else {
        (title_tag, title_w)
    };
    let dashes_top = box_width.saturating_sub(safe_title_w + 3);

    let (mut lines, caret_row, caret_col) = crate::prompt::input_layout(
        prompt, prompt.chars().count(), max_content);
    let mut first = 0;

    let visible = 3.min((term_rows as usize).saturating_sub(3).max(1));
    if lines.len() > visible {
        first = lines.len() - visible;
        lines.drain(..first);
    }
    let box_rows = 1 + lines.len() + 1;
    set_input_rows(box_rows as u16);
    let start_row = term_rows.saturating_sub(box_rows as u16);

    // Position strictly at bottom of the terminal window.
    // Clear separator line above and the box area downwards to erase any leftover borders.
    let _ = execute!(
        out,
        cursor::MoveTo(0, start_row.saturating_sub(1)),
        crossterm::style::Print(activity_line(box_width.saturating_sub(2))),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
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
        let prefix = if first + i == 0 { "> " } else { "" };
        let line_w = str_width(line);
        let row_width = max_content + if first + i == 0 { 0 } else { 2 };
        let pad = row_width.saturating_sub(line_w);
        let row = start_row + 1 + i as u16;
        let _ = execute!(
            out,
            cursor::MoveTo(0, row),
            SetForegroundColor(border_color),
            crossterm::style::Print("│ "),
            SetForegroundColor(primary_color),
            crossterm::style::Print(prefix),
            SetForegroundColor(primary_color),
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
            crate::i18n::tr(" Ctrl+O: Collapse • Esc/Ctrl+C: Stop ")
        } else {
            crate::i18n::tr(" Ctrl+O: Expand • Esc/Ctrl+C: Stop ")
        }
    } else if box_width >= 50 {
        crate::i18n::tr(" Esc/Ctrl+C: Stop ")
    } else {
        crate::i18n::tr(" Esc: Stop ")
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

    let cursor_x = (crate::prompt::input_column(caret_row) + caret_col) as u16;
    let cursor_y = start_row + 1 + caret_row.saturating_sub(first) as u16;

    (cursor_x, cursor_y)
}

#[cfg(test)]
fn bottom_prompt_cursor(prompt: &str, width: usize, rows: u16) -> (u16, u16) {
    if let Some(draft) = active_draft() {
        return (4 + draft.viewport(width.saturating_sub(6).max(1)).1 as u16, rows.saturating_sub(2 + draft.menu_rows(rows) as u16));
    }
    let lines = wrap_text_line(prompt, width.saturating_sub(6).max(1));
    let last_width = lines.last().map(|line| str_width(line)).unwrap_or(0);
    (4 + last_width as u16, rows.saturating_sub(2))
}

#[cfg(test)]
fn restore_prompt_cursor_to(prompt: &str, out: &mut impl Write) {
    let _geometry = begin_terminal_frame();
    if terminal_is_small() {
        let _ = execute!(out, cursor::MoveTo(0, 0), cursor::Hide);
        return;
    }
    let (_, rows) = terminal_size();
    let (x, y) = bottom_prompt_cursor(prompt, get_box_width(), rows);
    let _ = execute!(out, cursor::MoveTo(x, y), cursor::Show);
    let _ = out.flush();
}

pub(crate) fn wrap_text_line(line: &str, max_width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = line.chars().collect();
    let max_width = max_width.max(1);
    let mut result = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut cur_w = 0;
        let mut end = start;
        while end < chars.len() {
            if end > start && !chars[end].is_whitespace() && chars[end - 1].is_whitespace() {
                let word_width: usize = chars[end..].iter().copied()
                    .take_while(|c| !c.is_whitespace()).map(char_width).sum();
                if cur_w + word_width > max_width { break; }
            }
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

#[cfg(test)]
pub fn prepare_output_line(row: &mut u16, needed: u16, prompt: &str, branch_tag: &str) -> u16 {
    prepare_output_line_internal(row, needed, prompt, branch_tag, true)
}

pub fn prepare_output_line_internal(row: &mut u16, needed: u16, prompt: &str, branch_tag: &str, render_bottom: bool) -> u16 {
    if history_scrolled() { return 0; }
    invalidate_content_frame();
    let (_, term_rows) = terminal_size();
    let box_width = get_box_width();
    let max_content = box_width.saturating_sub(6);
    let lines_count = crate::prompt::input_layout(prompt, prompt.chars().count(), max_content).0.len();
    let box_rows = active_box_rows(term_rows).unwrap_or(2 + lines_count.min(term_rows.saturating_sub(2) as usize) as u16);
    let limit = term_rows.saturating_sub(box_rows + 1);
    let start_row = term_rows.saturating_sub(box_rows as u16);
    if *row + needed >= limit {
        let scroll = (*row + needed).saturating_sub(limit) + 1;
        let mut out = stdout();
        // Clear bottom box area BEFORE scrolling so its borders/content
        // do not scroll up into the active transcript area
        let _ = execute!(
            out,
            cursor::MoveTo(0, start_row.saturating_sub(1)),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown),
            crossterm::terminal::ScrollUp(scroll)
        );
        // Wipe any rows in the transition zone above start_row
        for r in start_row.saturating_sub(scroll + 1)..=start_row {
            let _ = execute!(
                out,
                cursor::MoveTo(0, r),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine)
            );
        }
        *row = row.saturating_sub(scroll);
        if render_bottom {
            render_bottom_box(prompt, branch_tag);
        }
        scroll
    } else {
        0
    }
}

const THINKING_MESSAGES: &[&str] = &[
    "Thinking...",
    "Herding neurons...",
    "Brewing a thought...",
    "Negotiating with bits...",
    "Scratching the CPU's head...",
    "Chasing a stray idea...",
    "Feeding the brain hamster...",
    "Puzzling without the box...",
    "Untangling ideas...",
    "Checking logic's pockets...",
    "Warming up hypotheses...",
    "Consulting the rubber duck...",
    "Neurons in a meeting...",
];

pub fn random_thinking_message() -> &'static str {
    use std::hash::BuildHasher;
    let random = std::collections::hash_map::RandomState::new().hash_one("takiza-thinking");
    THINKING_MESSAGES[(random as usize) % THINKING_MESSAGES.len()]
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
static ACTIVE_ACTIVITY: Mutex<Option<(&'static str, usize)>> = Mutex::new(None);

fn activity_line(width: usize) -> String {
    let activity = *ACTIVE_ACTIVITY.lock().unwrap();
    let view = HISTORY_VIEW.lock().unwrap();
    let scroll = if view.offset > 0 {
        crate::i18n::tf!("{}/{} · Ctrl+End: latest", view.total.saturating_sub(view.offset), view.total)
    } else { String::new() };
    match activity {
        Some((message, frame)) => {
            let message = crate::i18n::tr(message);
            let status = format!("{} {message}", SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]);
            let status = crate::prompt::truncate_visible(&status, width);
            let spare = width.saturating_sub(str_width(&status));
            let hint = if !scroll.is_empty() && spare >= str_width(&scroll) + 2 {
                format!("{}\x1b[90m{scroll}", " ".repeat(spare - str_width(&scroll)))
            } else { String::new() };
            format!("\x1b[36m{status}{hint}\x1b[0m")
        }
        None if !scroll.is_empty() => format!("\x1b[90m{}\x1b[0m", crate::prompt::truncate_visible(&format!("  {scroll}"), width)),
        None => String::new(),
    }
}

fn paint_activity() {
    let _geometry = begin_terminal_frame();
    if terminal_is_small() { return; }
    let (cols, rows) = terminal_size();
    let row = output_limit(rows);
    let line = activity_line(cols.saturating_sub(2) as usize);
    // Share the content cache and write one complete frame so scrolling cannot
    // erase the status between animation ticks or repaint the whole transcript.
    let mut previous = CONTENT_FRAME.lock().unwrap();
    let mut frame = Vec::new();
    let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate,
        cursor::SavePosition, cursor::MoveTo(0, row), ResetColor,
        crossterm::style::Print(&line), ResetColor,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        cursor::RestorePosition, crossterm::terminal::EndSynchronizedUpdate);
    let mut out = stdout().lock();
    let _ = out.write_all(&frame);
    let _ = out.flush();
    if let Some((w, h, lines)) = previous.as_mut() {
        if *w == cols && *h == rows && lines.len() == row as usize + 1 {
            lines[row as usize] = line;
        }
    }
}

pub struct Spinner {
    message: &'static str,
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn start_with_box(
        message: &'static str,
        prompt: &str,
        branch_tag: &str,
        _output_row: Arc<Mutex<u16>>,
    ) -> Self {
        *ACTIVE_ACTIVITY.lock().unwrap() = Some((message, 0));
        let (x, y) = render_bottom_box(prompt, branch_tag);
        position_input_cursor(x, y);
        paint_activity();
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let handle = tokio::spawn(async move {
            let mut i = 0;
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => break,
                    _ = tokio::time::sleep(Duration::from_millis(80)) => {
                        i += 1;
                        *ACTIVE_ACTIVITY.lock().unwrap() = Some((message, i));
                        paint_activity();
                    }
                }
            }
        });
        Self { message, stop_tx: Some(stop_tx), handle: Some(handle) }
    }

    pub fn message(&self) -> &'static str { self.message }

    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() { let _ = tx.send(()).await; }
        if let Some(h) = self.handle.take() { let _ = h.await; }
        *ACTIVE_ACTIVITY.lock().unwrap() = None;
        paint_activity();
    }
}

#[cfg(test)]
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
    raw_text: String,
}

#[cfg(test)]
impl StreamWriter {
    pub fn new(
        output_row: Arc<Mutex<u16>>,
        prefix: &'static str,
        prefix_color: Color,
        indent: &'static str,
        prompt: &str,
        branch_tag: &str,
    ) -> Self {
        let (term_cols, _) = terminal_size();
        let wrap_cols = (term_cols as usize).saturating_sub(6).max(1);
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
            raw_text: String::new(),
        }
    }

    pub fn raw_text(&self) -> &str {
        &self.raw_text
    }

    pub fn write_token(&mut self, token: &str) {
        self.write_token_to(token, &mut stdout());
    }

    fn write_token_to(&mut self, token: &str, out: &mut impl Write) {
        let _geometry = begin_terminal_frame();
        self.raw_text.push_str(token);
        if history_scrolled() { return; }
        invalidate_content_frame();
        if terminal_is_small() {
            return;
        }
        if token.is_empty() {
            return;
        }

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

        // Each token starts at the saved answer position; the visible cursor rests in the prompt.
        let _ = execute!(out, cursor::MoveTo(self.current_col as u16, *row_guard));
        self.pending_chars.push_str(token);

        let indent_w = str_width(self.indent);
        let th = crate::theme::current();
        let p_ansi = th.primary_ansi();
        let _ = execute!(out, ResetColor);
        if self.in_bold { let _ = execute!(out, crossterm::style::Print(format!("{p_ansi}\x1b[1m"))); }
        if self.in_code { let _ = execute!(out, crossterm::style::Print(format!("{}{}", th.code_background_ansi(), p_ansi))); }

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
                    let _ = execute!(out, crossterm::style::Print(format!("{p_ansi}\x1b[1m")));
                }
                if self.in_code {
                    let _ = execute!(out, crossterm::style::Print(format!("{}{}", th.code_background_ansi(), p_ansi)));
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
                    let _ = execute!(out, crossterm::style::Print(format!("{p_ansi}\x1b[1m")));
                } else {
                    self.in_bold = false;
                    let _ = execute!(out, crossterm::style::Print("\x1b[22m\x1b[39m"));
                }
                self.at_line_start = false;
                continue;
            }

            // 4. Inline code delimiter `
            if self.pending_chars.starts_with('`') && !self.pending_chars.starts_with("```") {
                self.pending_chars.remove(0);
                if !self.in_code {
                    self.in_code = true;
                    let _ = execute!(out, crossterm::style::Print(format!("{}{} ", th.code_background_ansi(), p_ansi)));
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
                        let _ = execute!(out, crossterm::style::Print(format!("{p_ansi}\x1b[1m")));
                    }
                    if self.in_code {
                        let _ = execute!(out, crossterm::style::Print(format!("{}{}", th.code_background_ansi(), p_ansi)));
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

        restore_prompt_cursor_to(&self.prompt, out);
        let _ = out.flush();
    }

    pub fn finish_and_replace(self, full_msg: &str) {
        if history_scrolled() { return; }
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

#[cfg(test)]
fn print_thought_at(thought: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_thought_internal(thought, row, prompt, branch_tag, true);
}

#[cfg(test)]
fn print_thought_internal(thought: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    if history_scrolled() { return; }
    if terminal_is_small() { *row = 0; return; }
    let (term_cols, _) = terminal_size();
    let max_w = (term_cols as usize).saturating_sub(6).max(1);
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    for line in thought_lines(thought, max_w, is_output_expanded()) {
        prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
        let _ = execute!(out, cursor::MoveTo(0, *row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(line), ResetColor);
        *row += 1;
    }

    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

fn extract_compact_arg(name: &str, args: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        if let Some(obj) = v.as_object() {
            if name == "ask_question" {
                if let Some(questions) = obj.get("questions").and_then(|value| value.as_array()) {
                    return format!("{} {}", questions.len(), if questions.len() == 1 { crate::i18n::tr("question") } else { "questions" });
                }
            }
            if name == "read_skill" {
                if let Some(name) = obj.get("name").and_then(|value| value.as_str()) { return name.to_string(); }
            }
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
        "run_command" => crate::i18n::tr("command"),
        "web_search" => "websearch",
        "write_file" => crate::i18n::tr("write"),
        "edit_file" => crate::i18n::tr("edit"),
        "read_file" => crate::i18n::tr("read"),
        "find_files" => crate::i18n::tr("find"),
        "grep_search" => crate::i18n::tr("search"),
        "list_dir" => crate::i18n::tr("list"),
        "ask_question" => crate::i18n::tr("question"),
        "read_skill" => crate::i18n::tr("skill"),
        _ => crate::i18n::tr("tool"),
    }
}

fn tool_detail(name: &str, result: Option<&str>, is_error: bool) -> String {
    if is_error { return crate::i18n::tr("failed").into(); }
    let Some(result) = result else { return crate::i18n::tr("running").into(); };
    match name {
        "list_dir" => crate::i18n::tf!("{} items", result.lines().filter(|line| !line.trim().is_empty()).count()),
        "read_file" => crate::i18n::tf!("{} lines", result.lines().count()),
        _ => String::new(),
    }
}

fn tool_error_lines(result: &str, width: usize, expanded: bool) -> Vec<String> {
    let wrapped = wrap_history_text(result.trim(), width.max(1));
    let visible = if expanded { wrapped.len() } else { wrapped.len().min(3) };
    let mut lines: Vec<_> = wrapped[..visible].iter()
        .map(|line| format!("    \x1b[31m{line}\x1b[0m")).collect();
    if wrapped.len() > visible {
        lines.push(crate::i18n::tf!("    \x1b[90m+{} lines  · Ctrl+O to expand\x1b[0m", wrapped.len() - visible));
    }
    lines
}

fn tool_output_lines(logs: &[&str], width: usize, expanded: bool) -> Vec<String> {
    let wrapped: Vec<_> = logs.iter()
        .flat_map(|text| wrap_history_text(text.trim(), width.max(1))).collect();
    let visible = if expanded { wrapped.len() } else { wrapped.len().min(3) };
    let mut lines: Vec<_> = wrapped[..visible].iter()
        .map(|line| format!("    \x1b[90m{line}\x1b[0m")).collect();
    if wrapped.len() > visible {
        lines.push(crate::i18n::tf!("    \x1b[90m+{} lines  · Ctrl+O to expand\x1b[0m", wrapped.len() - visible));
    }
    lines
}

fn tool_header_parts(name: &str, args: &str, detail: &str, width: usize) -> (String, String, String) {
    let label = if width >= 32 { format!("  {}{}", get_tool_icon(name), " ".repeat(8usize.saturating_sub(str_width(get_tool_icon(name))))) } else { format!("  {} ", get_tool_icon(name)) };
    let detail = if detail.is_empty() || width < 32 { String::new() } else { format!("  · {detail}") };
    let target = plain_terminal_text(&extract_compact_arg(name, args)).replace(['\n', '\r'], " ");
    let target = truncate_str(&target, width.saturating_sub(str_width(&label) + str_width(&detail)));
    (label, target, detail)
}

fn print_tool_header(name: &str, args: &str, result: Option<&str>, is_error: bool,
    row: &mut u16, prompt: &str, branch: &str, render_bottom: bool) {
    if history_scrolled() { return; }
    if terminal_is_small() { *row = 0; return; }
    let (cols, _) = terminal_size();
    let detail = tool_detail(name, result, is_error);
    let (label, target, detail) = tool_header_parts(name, args, &detail, cols.saturating_sub(2) as usize);
    let mut out = stdout();
    prepare_output_line_internal(row, 1, prompt, branch, render_bottom);
    let _ = execute!(out, cursor::Hide, cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        SetForegroundColor(if is_error { Color::Red } else { crate::theme::current().secondary_crossterm() }),
        crossterm::style::Print(label), ResetColor, crossterm::style::Print(target),
        SetForegroundColor(if is_error { Color::Red } else { Color::DarkGrey }),
        crossterm::style::Print(detail), ResetColor);
    *row += 1;
    if is_error {
        for line in tool_error_lines(result.unwrap_or(crate::i18n::tr("Unknown error")), cols.saturating_sub(6).max(1) as usize, is_output_expanded()) {
            prepare_output_line_internal(row, 1, prompt, branch, render_bottom);
            let _ = execute!(out, cursor::MoveTo(0, *row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                crossterm::style::Print(line), ResetColor);
            *row += 1;
        }
    }
    if render_bottom {
        let (x, y) = render_bottom_box(prompt, branch);
        let _ = execute!(out, cursor::MoveTo(x, y), cursor::Show);
    }
    let _ = out.flush();
}

pub fn print_tool_start_at(name: &str, args: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_tool_start_internal(name, args, row, prompt, branch_tag, true);
}

pub fn print_tool_start_internal(name: &str, args: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    print_tool_header(name, args, None, false, row, prompt, branch_tag, render_bottom);
}

pub fn print_tool_log_at(line: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_tool_log_internal(line, row, prompt, branch_tag, true);
}

pub fn print_tool_log_internal(line: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    if history_scrolled() { return; }
    if terminal_is_small() { *row = 0; return; }
    let plain = plain_terminal_text(line);
    let line = plain.as_str();
    for (prefix, prefix_color, text_color) in [
        ("  📋 Question: ", crate::theme::current().secondary_crossterm(), Color::White),
        ("     Answer: ", Color::DarkGrey, crate::theme::current().primary_crossterm()),
    ] {
        if let Some(text) = line.strip_prefix(prefix) {
            let display_prefix = if prefix.contains("Question") { crate::i18n::tr("    question  ") } else { crate::i18n::tr("    answer    ") };
            print_question_transcript_line(display_prefix, text, prefix_color, text_color,
                row, prompt, branch_tag, render_bottom);
            return;
        }
    }
    let (term_cols, _) = terminal_size();
    let max_log_w = (term_cols as usize).saturating_sub(6).max(1);
    let trimmed = line.trim();
    let normalized = if trimmed.starts_with("⚠") && trimmed.contains("Permission: $") {
        crate::i18n::tr("approval requested")
    } else if trimmed.contains("Command approved") {
        if trimmed.contains("Always") { crate::i18n::tr("approval allowed for session") } else { crate::i18n::tr("approval allowed") }
    } else if trimmed.contains("Command denied") { crate::i18n::tr("approval denied") }
    else { trimmed };
    let truncated_log = truncate_str(normalized, max_log_w);

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
    if history_scrolled() { return; }
    if terminal_is_small() { *row = 0; return; }
    prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
    let mut out = stdout();
    let _ = execute!(
        out,
        cursor::Hide,
        cursor::MoveTo(0, *row),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
        SetForegroundColor(Color::DarkGrey),
        crossterm::style::Print(crate::i18n::tf!("    +{hidden_count} lines  · Ctrl+O to expand", hidden_count = hidden_count)),
        ResetColor
    );
    *row += 1;
    if render_bottom {
        let (cx, cy) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(cx, cy), cursor::Show);
        let _ = out.flush();
    }
}

pub const ASSISTANT_INDENT: &str = "  ";

#[cfg(test)]
pub fn print_assistant_message_at(msg: &str, row: &mut u16, prompt: &str, branch_tag: &str) {
    print_assistant_message_internal(msg, row, prompt, branch_tag, true);
}

#[cfg(test)]
pub fn print_assistant_message_internal(msg: &str, row: &mut u16, prompt: &str, branch_tag: &str, render_bottom: bool) {
    if history_scrolled() { return; }
    if terminal_is_small() { *row = 0; return; }
    let (term_cols, _) = terminal_size();
    let wrap_cols = (term_cols as usize).saturating_sub(6 + str_width(ASSISTANT_INDENT)).max(1);
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
        let _ = execute!(out, crossterm::style::Print(ASSISTANT_INDENT),
            crossterm::style::Print(&line), ResetColor);
        first = false;
        *row += 1;
    }
    if first {
        prepare_output_line_internal(row, 1, prompt, branch_tag, render_bottom);
        let _ = execute!(
            out,
            cursor::MoveTo(0, *row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionChoice {
    AllowOnce,
    AllowAlways,
    Deny,
}

fn render_permission_bottom_box(command: &str, branch_tag: &str, selected: usize) -> (u16, u16) {
    let _geometry = begin_terminal_frame();
    if terminal_is_small() {
        render_small_terminal();
        return (0, 0);
    }
    let (cols, rows) = terminal_size();
    let width = cols as usize;
    let inner = width - 4;
    let start = rows - 6;
    let theme = crate::theme::current();
    let border = theme.border_crossterm();
    let title = crate::prompt::truncate_visible(&crate::i18n::tf!(" Permission [{}] ", branch_tag), width - 3);
    let mut frame = Vec::new();
    let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate, cursor::Hide,
        cursor::MoveTo(0, start), crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown),
        SetForegroundColor(border), crossterm::style::Print("╭─"),
        SetForegroundColor(theme.primary_crossterm()), crossterm::style::Print(&title),
        SetForegroundColor(border), crossterm::style::Print("─".repeat(width - str_width(&title) - 3)),
        crossterm::style::Print("╮"));
    let choices = [(crate::i18n::tr("Allow once"), 'y'), (crate::i18n::tr("Deny"), 'n'), (crate::i18n::tr("Allow for session"), 'a')];
    let label_width = choices.iter().map(|(label, _)| str_width(label)).max().unwrap_or(0)
        .min(inner.saturating_sub(6));
    for (index, text) in std::iter::once(format!("$ {}", command))
        .chain(choices.iter().enumerate().map(|(index, (label, key))| {
            format!("{} {} [{}]", if index == selected { ">" } else { " " },
                fit_to_width(label, label_width), key)
        })).enumerate() {
        let color = if index == selected + 1 {
            theme.primary_crossterm()
        } else if index == 0 {
            theme.secondary_crossterm()
        } else {
            Color::DarkGrey
        };
        let _ = queue!(frame, cursor::MoveTo(0, start + index as u16 + 1),
            SetForegroundColor(border), crossterm::style::Print("│ "),
            SetForegroundColor(color), crossterm::style::Print(fit_to_width(&text, inner)),
            SetForegroundColor(border), crossterm::style::Print(" │"));
    }
    let hint = if inner >= 32 { crate::i18n::tr(" ↑↓ Select · Enter · Esc Deny ") } else { " ↑↓ · Enter · Esc " };
    let _ = queue!(frame, cursor::MoveTo(0, rows - 1), SetForegroundColor(border),
        crossterm::style::Print(format!("╰{}{}─╯", "─".repeat(width - str_width(hint) - 3), hint)),
        ResetColor, crossterm::terminal::EndSynchronizedUpdate);
    let mut out = stdout();
    let _ = out.write_all(&frame);
    let _ = out.flush();
    (0, rows - 1)
}

pub fn ask_command_permission_with_redraw(
    command: &str, row: &mut u16, branch_tag: &str, mut on_redraw: impl FnMut(&mut u16),
) -> PermissionChoice {
    let mut out = stdout();
    let mut selected = 0;
    set_input_rows(6);
    on_redraw(row);
    let choice = loop {
        let (_, rows) = terminal_size();
        let start = rows.saturating_sub(6);
        if !terminal_is_small() && *row >= start {
            let scroll = row.saturating_sub(start.saturating_sub(1));
            let _ = execute!(out, cursor::MoveTo(0, (*row).min(rows - 1)),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown),
                crossterm::terminal::ScrollUp(scroll));
            *row = row.saturating_sub(scroll);
        }
        render_permission_bottom_box(command, branch_tag, selected);
        if let Ok(event) = crossterm::event::read() {
            match event {
                crossterm::event::Event::Resize(_, _) => {
                    on_redraw(row);
                }
                crossterm::event::Event::Key(key) => {
                    if key.kind != crossterm::event::KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        crossterm::event::KeyCode::Up => selected = (selected + 2) % 3,
                        crossterm::event::KeyCode::Down
                        | crossterm::event::KeyCode::Tab => selected = (selected + 1) % 3,
                        crossterm::event::KeyCode::BackTab => selected = (selected + 2) % 3,
                        crossterm::event::KeyCode::Enter => {
                            break [PermissionChoice::AllowOnce, PermissionChoice::Deny,
                                PermissionChoice::AllowAlways][selected];
                        }
                        crossterm::event::KeyCode::Char('y')
                        | crossterm::event::KeyCode::Char('Y') => {
                            break PermissionChoice::AllowOnce;
                        }
                        crossterm::event::KeyCode::Char('a')
                        | crossterm::event::KeyCode::Char('A') => {
                            break PermissionChoice::AllowAlways;
                        }
                        crossterm::event::KeyCode::Char('n')
                        | crossterm::event::KeyCode::Char('N')
                        | crossterm::event::KeyCode::Esc
                        | crossterm::event::KeyCode::Char('q') => {
                            break PermissionChoice::Deny;
                        }
                        crossterm::event::KeyCode::Char('c')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            break PermissionChoice::Deny;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    };

    let (_, term_rows) = terminal_size();
    let start_row = term_rows.saturating_sub(6);
    // 1. Clear permission bottom box from screen
    let _ = execute!(
        out,
        cursor::MoveTo(0, start_row.saturating_sub(1)),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
    );

    let status = match choice {
        PermissionChoice::AllowOnce => crate::i18n::tr("approval allowed"),
        PermissionChoice::AllowAlways => crate::i18n::tr("approval allowed for session"),
        PermissionChoice::Deny => crate::i18n::tr("approval denied"),
    };
    print_tool_log_internal(status, row, "", branch_tag, true);

    choice
}

fn fit_to_width(s: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }
    let cur_w = str_width(s);
    if cur_w <= max_w {
        format!("{}{}", s, " ".repeat(max_w - cur_w))
    } else if max_w == 1 {
        "…".to_string()
    } else {
        let mut res = String::new();
        let mut w = 0;
        for c in s.chars() {
            let cw = char_width(c);
            if w + cw + 1 > max_w {
                break;
            }
            res.push(c);
            w += cw;
        }
        res.push('…');
        w += 1;
        if w < max_w {
            res.push_str(&" ".repeat(max_w - w));
        }
        res
    }
}

struct QuestionLine {
    prefix: String,
    text: String,
    prefix_color: Color,
    text_color: Color,
}

fn question_body_lines(
    item: &crate::agent::QuestionItem,
    selected: &std::collections::HashSet<usize>,
    focused: usize,
    input: &str,
    editing: bool,
    width: usize,
) -> (Vec<QuestionLine>, usize) {
    let mut lines = Vec::new();
    let mut focus_row = 0;
    let mut append = |prefix: &str, text: &str, prefix_color, text_color| {
        let start = lines.len();
        for (label, text) in question_transcript_lines(prefix, text, width + 1) {
            lines.push(QuestionLine { prefix: label, text, prefix_color, text_color });
        }
        start..lines.len()
    };
    append("", &item.question, Color::White, Color::White);
    for (index, option) in item.options.iter().enumerate() {
        let active = focused == index && !editing;
        let chosen = selected.contains(&index);
        let mark = if item.is_multi_select {
            if chosen { "[✔]" } else { "[ ]" }
        } else if chosen { "(●)" } else { "( )" };
        let prefix = format!("{} [{}] {} ", if active { "▶" } else { " " }, (index + 1) % 10, mark);
        let range = append(&prefix, option,
            if active { Color::Cyan } else { Color::DarkGrey },
            if chosen { Color::Green } else if active { Color::White } else { Color::Grey });
        if focused == index { focus_row = range.start; }
    }
    if item.options.is_empty() || item.allow_custom {
        let active = item.options.is_empty() || focused == item.options.len() || editing;
        let prefix = if item.options.is_empty() { crate::i18n::tr("Your answer: ").to_string() }
            else { crate::i18n::tf!("{} [0] Custom: ", if active { "▶" } else { " " }) };
        let text = if editing { format!("{input}█") }
            else if input.is_empty() { crate::i18n::tr("(write custom response...)").into() }
            else { input.to_string() };
        let range = append(&prefix, &text,
            if active { Color::Cyan } else { Color::DarkGrey },
            if editing { Color::Yellow } else if input.is_empty() { Color::DarkGrey } else { Color::Green });
        if active { focus_row = if editing { range.end - 1 } else { range.start }; }
    }
    (lines, focus_row)
}

fn question_transcript_lines(prefix: &str, text: &str, width: usize) -> Vec<(String, String)> {
    let prefix = crate::prompt::truncate_visible(prefix, width.saturating_sub(2));
    let indent = " ".repeat(str_width(&prefix));
    let content_width = width.saturating_sub(str_width(&prefix) + 1).max(1);
    let text = text.replace('\t', "    ").replace('\r', "");
    let mut lines = Vec::new();
    for line in text.split('\n') {
        for wrapped in wrap_text_line(line, content_width) {
            let label = if lines.is_empty() { prefix.clone() } else { indent.clone() };
            lines.push((label, wrapped));
        }
    }
    lines
}

fn print_question_transcript_line(
    prefix: &str,
    text: &str,
    prefix_color: Color,
    text_color: Color,
    row: &mut u16,
    prompt: &str,
    branch_tag: &str,
    render_bottom: bool,
) {
    if terminal_is_small() { *row = 0; return; }
    let (cols, _) = terminal_size();
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);
    for (label, line) in question_transcript_lines(prefix, text, cols as usize) {
        prepare_output_line_internal(row, 1, prompt, branch_tag, false);
        let _ = queue!(out, cursor::MoveTo(0, *row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            SetForegroundColor(prefix_color), crossterm::style::Print(label),
            SetForegroundColor(text_color), crossterm::style::Print(line), ResetColor);
        // Flush before a subsequent row can scroll the terminal.
        let _ = out.flush();
        *row += 1;
    }
    if render_bottom {
        let (x, y) = render_bottom_box(prompt, branch_tag);
        let _ = execute!(out, cursor::MoveTo(x, y), cursor::Show);
    }
    let _ = out.flush();
}

fn focus_question_option(
    selected: &mut std::collections::HashSet<usize>,
    focused: usize,
    num_options: usize,
    is_multi: bool,
) {
    if !is_multi {
        selected.clear();
        if focused < num_options {
            selected.insert(focused);
        }
    }
}

struct QuestionLayout {
    start_row: u16,
    scroll: u16,
    visible_options: std::ops::Range<usize>,
}

fn question_layout(row: u16, term_rows: u16, option_rows: usize, focused: usize) -> QuestionLayout {
    // Reserve the borders, question and hint; every remaining row can show an option.
    let visible_count = option_rows.min(term_rows.saturating_sub(4) as usize);
    let first = focused.saturating_add(1).saturating_sub(visible_count)
        .min(option_rows.saturating_sub(visible_count));
    let box_rows = visible_count as u16 + 4;
    let start_row = term_rows.saturating_sub(box_rows);
    QuestionLayout {
        start_row,
        scroll: row.saturating_sub(start_row),
        visible_options: first..first + visible_count,
    }
}

fn save_question_answer(
    answers: &mut Vec<crate::agent::QuestionAnswer>,
    answer: crate::agent::QuestionAnswer,
) {
    if let Some(existing) = answers
        .iter_mut()
        .find(|existing| existing.question_index == answer.question_index)
    {
        *existing = answer;
    } else {
        answers.push(answer);
        answers.sort_by_key(|answer| answer.question_index);
    }
}

fn question_answer_text(answer: Option<&crate::agent::QuestionAnswer>) -> String {
    let mut parts = Vec::new();
    if let Some(answer) = answer {
        parts.extend(answer.selected_options.iter().cloned());
        if let Some(text) = answer
            .custom_text
            .as_ref()
            .filter(|text| !text.trim().is_empty())
        {
            parts.push(text.clone());
        }
    }
    if parts.is_empty() {
        crate::i18n::tr("[Skipped / No answer]").into()
    } else {
        parts.join(" | ")
    }
}

fn question_review_lines(
    questions: &[crate::agent::QuestionItem],
    answers: &[crate::agent::QuestionAnswer],
    width: usize,
    focused: usize,
) -> (Vec<(String, Color)>, Vec<std::ops::Range<usize>>) {
    let mut lines = Vec::new();
    let mut ranges = Vec::new();
    for (index, question) in questions.iter().enumerate() {
        let first = lines.len();
        let marker = if index == focused { "▶" } else { " " };
        let title = format!("{} {}. {}", marker, index + 1, question.question);
        let color = if index == focused { Color::Cyan } else { Color::White };
        for line in title.lines() {
            lines.extend(wrap_text_line(line, width).into_iter().map(|line| (line, color)));
        }
        let answer = answers.iter().find(|answer| answer.question_index == index);
        let text = crate::i18n::tf!("   Answer: {}", question_answer_text(answer));
        for line in text.lines() {
            lines.extend(wrap_text_line(line, width).into_iter().map(|line| (line, Color::White)));
        }
        ranges.push(first..lines.len());
        lines.push((String::new(), Color::White));
    }
    (lines, ranges)
}

enum QuestionReviewAction {
    Confirm,
    Edit(usize),
    Cancel,
}

fn review_question_answers(
    questions: &[crate::agent::QuestionItem],
    answers: &[crate::agent::QuestionAnswer],
    row: &mut u16,
    on_redraw: &mut dyn FnMut(&mut u16),
) -> QuestionReviewAction {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    let mut out = stdout();
    let mut focused = 0;
    let mut offset = 0;
    let mut follow_focus = true;
    let mut last_size = None;
    loop {
        let geometry = begin_terminal_frame();
        let (cols, rows) = terminal_size();
        if terminal_is_small() {
            render_small_terminal();
            drop(geometry);
            let event = crossterm::event::read();
            if matches!(event, Ok(Event::Resize(_, _))) {
                on_redraw(row);
            }
            if let Ok(Event::Key(key)) = event {
                if key.kind == KeyEventKind::Press
                    && (key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)))
                {
                    return QuestionReviewAction::Cancel;
                }
            }
            continue;
        }
        let width = cols as usize;
        let inner_width = width - 4;
        let (lines, ranges) = question_review_lines(questions, answers, inner_width, focused);
        let capacity = (rows - 4) as usize;
        if follow_focus || last_size != Some((cols, rows)) {
            let range = &ranges[focused];
            if range.start < offset || range.len() > capacity {
                offset = range.start;
            } else if range.end > offset + capacity {
                offset = range.end.saturating_sub(capacity);
            }
            follow_focus = false;
        }
        offset = offset.min(lines.len().saturating_sub(capacity));
        let layout = question_layout(*row, rows, lines.len(), offset + capacity - 1);
        offset = layout.visible_options.start;
        let mut frame = Vec::new();
        let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate);
        if layout.scroll > 0 {
            let _ = queue!(
                frame,
                cursor::MoveTo(0, (*row).min(rows - 1)),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown),
                crossterm::terminal::ScrollUp(layout.scroll)
            );
        }
        *row = layout.start_row;
        if last_size != Some((cols, rows)) {
            let _ = queue!(
                frame,
                cursor::MoveTo(0, *row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
            );
        }
        last_size = Some((cols, rows));
        let border = Color::Rgb {
            r: 60,
            g: 60,
            b: 65,
        };
        let title = crate::i18n::tr(" Review answers ");
        let _ = queue!(
            frame,
            cursor::MoveTo(0, *row),
            SetForegroundColor(border),
            crossterm::style::Print("╭─"),
            SetForegroundColor(Color::Cyan),
            crossterm::style::Print(title),
            SetForegroundColor(border),
            crossterm::style::Print("─".repeat(width - str_width(title) - 3)),
            crossterm::style::Print("╮")
        );
        let mut display_lines =
            vec![(crate::i18n::tr("Check your answers. Select a question to change it.").to_string(), Color::White)];
        display_lines.extend(lines[layout.visible_options.clone()].iter().cloned());
        display_lines.push((if inner_width < 64 {
            crate::i18n::tr("[y] Send  [↵] Edit").into()
        } else {
            crate::i18n::tr("[y/Ctrl+Enter] Send  [↑↓/Enter] Edit  [PgUp/PgDn] Scroll  [Esc] Cancel").into()
        }, Color::White));
        for (index, (line, color)) in display_lines.iter().enumerate() {
            let _ = queue!(
                frame,
                cursor::MoveTo(0, *row + index as u16 + 1),
                SetForegroundColor(border),
                crossterm::style::Print("│ "),
                SetForegroundColor(*color),
                crossterm::style::Print(fit_to_width(line, inner_width)),
                SetForegroundColor(border),
                crossterm::style::Print(" │")
            );
        }
        let _ = queue!(
            frame,
            cursor::MoveTo(0, *row + display_lines.len() as u16 + 1),
            SetForegroundColor(border),
            crossterm::style::Print(format!("╰{}╯", "─".repeat(width - 2))),
            ResetColor,
            crossterm::terminal::EndSynchronizedUpdate
        );
        let _ = out.write_all(&frame);
        let _ = out.flush();
        drop(geometry);
        let action = match crossterm::event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(QuestionReviewAction::Confirm),
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(QuestionReviewAction::Confirm)
                }
                KeyCode::Enter => Some(QuestionReviewAction::Edit(focused)),
                KeyCode::Char('1'..='9') => {
                    let KeyCode::Char(digit) = key.code else {
                        unreachable!()
                    };
                    let index = digit as usize - '1' as usize;
                    (index < questions.len()).then_some(QuestionReviewAction::Edit(index))
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    focused = focused.saturating_sub(1);
                    follow_focus = true;
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    focused = (focused + 1).min(questions.len() - 1);
                    follow_focus = true;
                    None
                }
                KeyCode::PageUp => {
                    offset = offset.saturating_sub(capacity);
                    None
                }
                KeyCode::PageDown => {
                    offset = offset.saturating_add(capacity);
                    None
                }
                KeyCode::Esc => Some(QuestionReviewAction::Cancel),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(QuestionReviewAction::Cancel)
                }
                _ => None,
            },
            Ok(Event::Resize(_, _)) => {
                on_redraw(row);
                last_size = None;
                None
            }
            _ => None,
        };
        if let Some(action) = action {
            let _ = execute!(
                out,
                cursor::MoveTo(0, *row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
            );
            return action;
        }
    }
}

pub fn ask_interactive_question_with_redraw(
    questions: &[crate::agent::QuestionItem],
    row: &mut u16,
    branch_tag: &str,
    mut on_redraw: impl FnMut(&mut u16),
) -> crate::agent::QuestionResponse {
    if questions.is_empty() {
        return crate::agent::QuestionResponse {
            answers: Vec::new(),
            skipped: false,
        };
    }

    let mut answers: Vec<crate::agent::QuestionAnswer> = Vec::new();
    let mut out = stdout();
    let _ = execute!(out, cursor::Hide);

    let total_q = questions.len();

    let mut q_idx = 0;
    let mut editing_from_review = false;
    loop {
        if q_idx == total_q {
            match review_question_answers(questions, &answers, row, &mut on_redraw) {
                QuestionReviewAction::Confirm => break,
                QuestionReviewAction::Edit(index) => {
                    q_idx = index;
                    editing_from_review = true;
                }
                QuestionReviewAction::Cancel => {
                    return crate::agent::QuestionResponse { answers, skipped: true };
                }
            }
        }
        let item = &questions[q_idx];
        let is_multi = item.is_multi_select;
        let allow_custom = item.allow_custom;
        let num_options = item.options.len();

        let mut selected: std::collections::HashSet<usize> = std::collections::HashSet::new();
        focus_question_option(&mut selected, 0, num_options, is_multi);
        let mut custom_input = String::new();
        let mut in_custom_edit_mode = num_options == 0;
        let mut cursor_option: usize = 0;

        if let Some(answer) = answers.iter().find(|answer| answer.question_index == q_idx) {
            selected = item.options.iter().enumerate()
                .filter_map(|(index, label)| answer.selected_options.contains(label).then_some(index))
                .collect();
            custom_input = answer.custom_text.clone().unwrap_or_default();
            cursor_option = selected.iter().copied().min().unwrap_or_else(|| {
                if allow_custom && !custom_input.is_empty() { num_options } else { 0 }
            });
            in_custom_edit_mode = num_options == 0 || (cursor_option == num_options && allow_custom);
        }

        let total_selectable = num_options + if allow_custom { 1 } else { 0 };
        let mut current_start_row;
        let mut last_size = None;
        let mut body_offset = 0usize;
        let mut follow_focus = true;

        loop {
            let geometry = begin_terminal_frame();
            let (term_cols, term_rows) = terminal_size();
            if terminal_is_small() {
                let _ = execute!(
                    out,
                    cursor::MoveTo(0, 0),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                    crossterm::style::Print(fit_to_width(crate::i18n::tr("Resize terminal"), term_cols as usize))
                );
                drop(geometry);
                let event = crossterm::event::read();
                if matches!(event, Ok(crossterm::event::Event::Resize(_, _))) {
                    on_redraw(row);
                }
                if let Ok(crossterm::event::Event::Key(key)) = event {
                    if key.kind == crossterm::event::KeyEventKind::Press
                        && (key.code == crossterm::event::KeyCode::Esc
                            || (key.code == crossterm::event::KeyCode::Char('c')
                                && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)))
                    {
                        return crate::agent::QuestionResponse { answers, skipped: true };
                    }
                }
                continue;
            }
            let mut frame = Vec::new();
            let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate);
            let box_width = term_cols as usize;
            let inner_w = box_width.saturating_sub(4);

            let (body, focus_row) = question_body_lines(item, &selected, cursor_option,
                &custom_input, in_custom_edit_mode, inner_w);
            let capacity = term_rows.saturating_sub(3).max(1) as usize;
            if follow_focus || last_size != Some((term_cols, term_rows)) {
                if focus_row < body_offset { body_offset = focus_row; }
                if focus_row >= body_offset + capacity { body_offset = focus_row + 1 - capacity; }
            }
            body_offset = body_offset.min(body.len().saturating_sub(capacity));
            let visible_body = body_offset..(body_offset + capacity).min(body.len());
            let start_row = term_rows.saturating_sub((visible_body.len() + 3) as u16);
            let layout = QuestionLayout {
                start_row, scroll: row.saturating_sub(start_row), visible_options: visible_body,
            };

            // Clear the old box, preserving the transcript before scrolling it up.
            if layout.scroll > 0 {
                let _ = queue!(
                    frame,
                    cursor::MoveTo(0, (*row).min(term_rows - 1)),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown),
                    crossterm::terminal::ScrollUp(layout.scroll)
                );
            }
            // Erase rows left behind when the answer shrinks or editing ends.
            let _ = queue!(frame,
                cursor::MoveTo(0, row.saturating_sub(layout.scroll).min(layout.start_row)),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown));
            *row = layout.start_row;
            let start_row = layout.start_row;
            current_start_row = start_row;

            if last_size != Some((term_cols, term_rows)) {
                let _ = queue!(
                    frame,
                    cursor::MoveTo(0, start_row),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
                );
            }
            last_size = Some((term_cols, term_rows));

            let border_color = Color::Rgb { r: 60, g: 60, b: 65 };
            let primary_color = Color::Cyan;

            let mut cur_r = start_row;

            // 1. Top border with category integrated cleanly
            let title = if let Some(ref hdr) = item.header {
                crate::i18n::tf!(" Question [{}/{}] • {} ", q_idx + 1, total_q, hdr)
            } else {
                crate::i18n::tf!(" Question [{}/{}] ", q_idx + 1, total_q)
            };
            let title = if body.len() > capacity {
                format!("{} • {}-{}/{} · PgUp/PgDn ", title.trim_end(),
                    body_offset + 1, layout.visible_options.end, body.len())
            } else { title };
            let title = crate::prompt::truncate_visible(&title, box_width.saturating_sub(3));
            let title_w = str_width(&title);
            let dashes_top = box_width.saturating_sub(title_w + 3);
            let _ = queue!(
                frame,
                cursor::MoveTo(0, cur_r),
                SetForegroundColor(border_color),
                crossterm::style::Print("╭─"),
                SetForegroundColor(primary_color),
                crossterm::style::Print(&title),
                SetForegroundColor(border_color),
                crossterm::style::Print("─".repeat(dashes_top)),
                crossterm::style::Print("╮"),
                ResetColor
            );
            cur_r += 1;

            // Question, options and custom answer share a wrapping, scrollable body.
            for line in &body[layout.visible_options.clone()] {
                let padding = inner_w.saturating_sub(str_width(&line.prefix) + str_width(&line.text));
                let _ = queue!(
                    frame,
                    cursor::MoveTo(0, cur_r),
                    SetForegroundColor(border_color),
                    crossterm::style::Print("│ "),
                    SetForegroundColor(line.prefix_color),
                    crossterm::style::Print(&line.prefix),
                    SetForegroundColor(line.text_color),
                    crossterm::style::Print(&line.text),
                    crossterm::style::Print(" ".repeat(padding)),
                    SetForegroundColor(border_color),
                    crossterm::style::Print(" │"),
                    ResetColor
                );
                cur_r += 1;
            }

            // 4. Hint row
            let hint_str = if in_custom_edit_mode {
                if num_options == 0 {
                    crate::i18n::tr("[Enter] Submit  [Esc] Cancel")
                } else {
                    crate::i18n::tr("[Enter] Done editing  [Esc] Back to options")
                }
            } else {
                if is_multi {
                    crate::i18n::tr("[Space/1-9] Toggle  [Enter] Confirm  [Arrows] Move  [0/c] Custom  [s] Skip  [Esc] Cancel")
                } else {
                    crate::i18n::tr("[Enter] Confirm  [1-9/Arrows] Select  [0/c] Custom  [s] Skip  [Esc] Cancel")
                }
            };
            let hint_line = fit_to_width(hint_str, inner_w);
            let _ = queue!(
                frame,
                cursor::MoveTo(0, cur_r),
                SetForegroundColor(border_color),
                crossterm::style::Print("│ "),
                SetForegroundColor(Color::DarkGrey),
                crossterm::style::Print(hint_line),
                SetForegroundColor(border_color),
                crossterm::style::Print(" │"),
                ResetColor
            );
            cur_r += 1;

            // 8. Bottom border
            let dashes_bottom = box_width.saturating_sub(2);
            let _ = queue!(
                frame,
                cursor::MoveTo(0, cur_r),
                SetForegroundColor(border_color),
                crossterm::style::Print("╰"),
                crossterm::style::Print("─".repeat(dashes_bottom)),
                crossterm::style::Print("╯"),
                ResetColor
            );
            let _ = queue!(frame, crossterm::terminal::EndSynchronizedUpdate);
            let _ = out.write_all(&frame);
            let _ = out.flush();

            drop(geometry);
            // Wait for key event
            if let Ok(event) = crossterm::event::read() {
                match event {
                    crossterm::event::Event::Resize(_, _) => {
                        on_redraw(row);
                        last_size = None;
                        continue;
                    }
                    crossterm::event::Event::Key(key) => {
                        if key.kind != crossterm::event::KeyEventKind::Press {
                            continue;
                        }

                        match key.code {
                            crossterm::event::KeyCode::PageUp | crossterm::event::KeyCode::PageDown => {
                                body_offset = if key.code == crossterm::event::KeyCode::PageUp {
                                    body_offset.saturating_sub(capacity)
                                } else { (body_offset + capacity).min(body.len().saturating_sub(capacity)) };
                                follow_focus = false;
                                continue;
                            }
                            _ => follow_focus = true,
                        }

                        if in_custom_edit_mode {
                            match key.code {
                                crossterm::event::KeyCode::Char(c) => {
                                    if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) && c == 'c' {
                                        let _ = execute!(
                                            out,
                                            cursor::MoveTo(0, start_row),
                                            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
                                        );
                                        return crate::agent::QuestionResponse { answers, skipped: true };
                                    }
                                    custom_input.push(c);
                                }
                                crossterm::event::KeyCode::Backspace => {
                                    custom_input.pop();
                                }
                                crossterm::event::KeyCode::Enter => {
                                    if num_options == 0 {
                                        let ans_custom = if custom_input.trim().is_empty() { None } else { Some(custom_input.trim().to_string()) };
                                        save_question_answer(&mut answers, crate::agent::QuestionAnswer {
                                            question_index: q_idx,
                                            question: item.question.clone(),
                                            selected_options: Vec::new(),
                                            custom_text: ans_custom,
                                        });
                                        break;
                                    } else {
                                        if !is_multi && !custom_input.trim().is_empty() {
                                            selected.clear();
                                        }
                                        in_custom_edit_mode = false;
                                    }
                                }
                                crossterm::event::KeyCode::Esc => {
                                    if num_options == 0 {
                                        let _ = execute!(
                                            out,
                                            cursor::MoveTo(0, start_row),
                                            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
                                        );
                                        return crate::agent::QuestionResponse { answers, skipped: true };
                                    } else {
                                        in_custom_edit_mode = false;
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            match key.code {
                                crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                                    if total_selectable > 0 {
                                        cursor_option = if cursor_option == 0 { total_selectable - 1 } else { cursor_option - 1 };
                                        focus_question_option(&mut selected, cursor_option, num_options, is_multi);
                                    }
                                }
                                crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                                    if total_selectable > 0 {
                                        cursor_option = (cursor_option + 1) % total_selectable;
                                        focus_question_option(&mut selected, cursor_option, num_options, is_multi);
                                    }
                                }
                                crossterm::event::KeyCode::Char(c @ '1'..='9') => {
                                    let digit = (c as u8 - b'1') as usize;
                                    if digit < num_options {
                                        if is_multi {
                                            if selected.contains(&digit) {
                                                selected.remove(&digit);
                                            } else {
                                                selected.insert(digit);
                                            }
                                        } else {
                                            selected.clear();
                                            selected.insert(digit);
                                        }
                                        cursor_option = digit;
                                    }
                                }
                                crossterm::event::KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                                    let _ = execute!(
                                        out,
                                        cursor::MoveTo(0, start_row),
                                        crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
                                    );
                                    return crate::agent::QuestionResponse { answers, skipped: true };
                                }
                                crossterm::event::KeyCode::Char('0') | crossterm::event::KeyCode::Char('c') | crossterm::event::KeyCode::Char('C') => {
                                    if allow_custom {
                                        cursor_option = num_options;
                                        in_custom_edit_mode = true;
                                    }
                                }
                                crossterm::event::KeyCode::Char(' ') => {
                                    if cursor_option < num_options {
                                        if is_multi {
                                            if selected.contains(&cursor_option) {
                                                selected.remove(&cursor_option);
                                            } else {
                                                selected.insert(cursor_option);
                                            }
                                        } else {
                                            selected.clear();
                                            selected.insert(cursor_option);
                                        }
                                    } else if allow_custom && cursor_option == num_options {
                                        in_custom_edit_mode = true;
                                    }
                                }
                                crossterm::event::KeyCode::Char('s') | crossterm::event::KeyCode::Char('S') => {
                                    save_question_answer(&mut answers, crate::agent::QuestionAnswer {
                                        question_index: q_idx,
                                        question: item.question.clone(),
                                        selected_options: Vec::new(),
                                        custom_text: None,
                                    });
                                    break;
                                }
                                crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('q') => {
                                    let _ = execute!(
                                        out,
                                        cursor::MoveTo(0, current_start_row),
                                        crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
                                    );
                                    prepare_output_line_internal(row, 1, "", branch_tag, false);
                                    let _ = execute!(
                                        out,
                                        cursor::MoveTo(0, *row),
                                        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                                        SetForegroundColor(Color::DarkGrey),
                                        crossterm::style::Print(crate::i18n::tr("  ℹ Questionnaire cancelled by user.")),
                                        ResetColor
                                    );
                                    *row += 1;
                                    return crate::agent::QuestionResponse { answers, skipped: true };
                                }
                                crossterm::event::KeyCode::Enter => {
                                    if cursor_option == num_options && allow_custom && custom_input.trim().is_empty() {
                                        in_custom_edit_mode = true;
                                        continue;
                                    }
                                    if selected.is_empty() && custom_input.trim().is_empty() {
                                        // Skipping is explicit (s); Enter must not silently lose an answer.
                                        continue;
                                    }
                                    let mut sel_vec: Vec<usize> = selected.iter().copied().collect();
                                    sel_vec.sort();
                                    let selected_labels: Vec<String> = sel_vec.iter().map(|&i| item.options[i].clone()).collect();
                                    let ans_custom = if custom_input.trim().is_empty() { None } else { Some(custom_input.trim().to_string()) };

                                    save_question_answer(&mut answers, crate::agent::QuestionAnswer {
                                        question_index: q_idx,
                                        question: item.question.clone(),
                                        selected_options: selected_labels,
                                        custom_text: ans_custom,
                                    });
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // 1. Wipe the question box that was just answered
        let _ = execute!(
            out,
            cursor::MoveTo(0, current_start_row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
        );

        q_idx = if editing_from_review { total_q } else { q_idx + 1 };
    }

    // Restore the transcript position after dismissing the bottom-anchored panel.
    on_redraw(row);
    // Print confirmed questions and answers using the same labels as history.
    for ans in &answers {
        let mut parts = Vec::new();
        if !ans.selected_options.is_empty() {
            parts.push(ans.selected_options.join(", "));
        }
        if let Some(ref c) = ans.custom_text {
            if !c.trim().is_empty() {
                parts.push(format!("\"{}\"", c.trim()));
            }
        }
        let (ans_str, ans_color) = if parts.is_empty() {
            (crate::i18n::tr("[Skipped / No response]").to_string(), Color::DarkGrey)
        } else {
            (parts.join(" | "), Color::Green)
        };

        print_question_transcript_line(crate::i18n::tr("    question  "), &ans.question, crate::theme::current().secondary_crossterm(),
            Color::White, row, "", branch_tag, false);
        print_question_transcript_line(crate::i18n::tr("    answer    "), &ans_str, Color::DarkGrey,
            ans_color, row, "", branch_tag, false);
    }

    crate::agent::QuestionResponse {
        answers,
        skipped: false,
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
    FileDiff(String),
    ToolEnd {
        name: String,
        #[serde(default)]
        args: String,
        result: String,
        is_error: bool,
    },
    AssistantMessage(String),
    ResponseStats { elapsed_ms: u64, tokens: Option<u64>, usage_complete: bool },
    Error(String),
}

pub fn duration_text(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1000;
    if seconds >= 3600 {
        crate::i18n::tf!("{}h {:02}m {:02}s", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else if seconds >= 60 {
        crate::i18n::tf!("{}m {:02}s", seconds / 60, seconds % 60)
    } else { crate::i18n::tf!("{:.1}s", elapsed_ms as f64 / 1000.0) }
}

pub fn token_usage_text(tokens: Option<u64>, usage_complete: bool) -> String {
    match tokens {
        Some(tokens) if usage_complete => crate::i18n::tf!("{tokens} tokens", tokens = tokens),
        Some(tokens) => crate::i18n::tf!("≥ {tokens} tokens", tokens = tokens),
        None => crate::i18n::tr("tokens unavailable").to_string(),
    }
}

pub fn response_stats_text(elapsed_ms: u64, tokens: Option<u64>, usage_complete: bool) -> String {
    let duration = duration_text(elapsed_ms);
    let usage = token_usage_text(tokens, usage_complete);
    crate::i18n::tf!("Completed in {duration} · {usage}", duration = duration, usage = usage)
}

pub fn update_streamed_thought(history: &mut Vec<HistoryItem>, index: &mut Option<usize>, text: &str, complete: bool) {
    if let Some(HistoryItem::Thought(thought)) = index.and_then(|i| history.get_mut(i)) {
        if complete { *thought = text.to_string(); }
        else { thought.push_str(text); }
    } else if !text.is_empty() {
        *index = Some(history.len());
        history.push(HistoryItem::Thought(text.to_string()));
    }
}

/// Command output is transcript text, not a second terminal renderer. Strip
/// control sequences before wrapping, otherwise wrapping can split an escape
/// and replay cursor movement, screen clearing or terminal mode changes.
fn plain_terminal_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let escape = match c {
            '\x1b' => chars.next(),
            '\u{009b}' => Some('['),
            '\u{009d}' => Some(']'),
            '\u{0090}' => Some('P'),
            '\u{009e}' => Some('^'),
            '\u{009f}' => Some('_'),
            _ => None,
        };
        if let Some(code) = escape {
            match code {
                '[' => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) { break; }
                    }
                }
                ']' | 'P' | 'X' | '^' | '_' => {
                    while let Some(next) = chars.next() {
                        if next == '\x07' || next == '\u{009c}' { break; }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                '\x20'..='\x2f' => {
                    for next in chars.by_ref() {
                        if ('\x30'..='\x7e').contains(&next) { break; }
                    }
                }
                _ => {}
            }
            continue;
        }
        match c {
            '\n' => plain.push('\n'),
            '\r' if chars.peek() != Some(&'\n') => plain.push('\n'),
            '\t' => plain.push_str("    "),
            '\x08' => { if !plain.ends_with('\n') { plain.pop(); } }
            _ if !c.is_control() => plain.push(c),
            _ => {}
        }
    }
    plain
}

fn wrap_history_text(text: &str, width: usize) -> Vec<String> {
    plain_terminal_text(text).split('\n').flat_map(|line| wrap_text_line(line, width)).collect()
}

fn thought_lines(text: &str, width: usize, expanded: bool) -> Vec<String> {
    let secondary = crate::theme::current().secondary_ansi();
    let wrapped = plain_terminal_text(text).lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| {
            let rendered = crate::markdown::render_inline(line);
            crate::markdown::wrap_rendered_line(
                &format!("{secondary}\x1b[2;3m{rendered}\x1b[0m"),
                width.saturating_sub(4).max(1),
            )
        }).collect::<Vec<_>>();
    if wrapped.is_empty() { return Vec::new(); }
    let count = if expanded { wrapped.len() } else { wrapped.len().min(3) };
    let mut lines = vec![crate::i18n::tf!("  {secondary}Reasoning\x1b[0m", secondary = secondary)];
    lines.extend(wrapped[..count].iter().map(|line| format!("  {secondary}│ \x1b[2;3m{line}\x1b[0m")));
    if count < wrapped.len() {
        let hint = truncate_str(&crate::i18n::tf!("+{} reasoning lines · Ctrl+O to expand", wrapped.len() - count), width.saturating_sub(4));
        lines.push(format!("  {secondary}│ \x1b[2m{hint}\x1b[0m"));
    }
    lines.push(String::new());
    lines
}

fn question_log_lines(text: &str, width: usize) -> Option<Vec<String>> {
    let text_is_question = text.starts_with("  📋 Question: ");
    let theme = crate::theme::current();
    let (label, text, color) = if let Some(text) = text.strip_prefix("  📋 Question: ") {
        (crate::i18n::tr("    question  "), text, "\x1b[37m")
    } else if let Some(text) = text.strip_prefix("     Answer: ") {
        (crate::i18n::tr("    answer    "), text, theme.primary_ansi())
    } else { return None; };
    let label_color = if text_is_question { theme.secondary_ansi() } else { "\x1b[90m" };
    Some(question_transcript_lines(label, &plain_terminal_text(text), width + 6).into_iter()
        .map(|(label, line)| format!("{label_color}{label}{color}{line}\x1b[0m")).collect())
}

fn file_diff_lines(diff: &str, width: usize, expanded: bool) -> Vec<String> {
    let theme = crate::theme::current();
    let (added, removed) = theme.diff_colors_ansi();
    let (added_background, removed_background) = theme.diff_backgrounds_ansi();
    let secondary = theme.secondary_ansi();
    let inner_width = width.saturating_sub(4).max(1);
    let mut rows = Vec::new();
    for line in plain_terminal_text(diff).lines() {
        if !expanded && line.starts_with(' ') { continue; }
        let (color, background) = if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            (secondary, "")
        } else if line.starts_with('+') { (added, added_background) }
        else if line.starts_with('-') { (removed, removed_background) }
        else { ("\x1b[90m", "") };
        for wrapped in wrap_text_line(line, inner_width) {
            let padding = if background.is_empty() { 0 } else { inner_width.saturating_sub(str_width(&wrapped)) };
            rows.push(format!("    {background}{color}{wrapped}{}\x1b[0m", " ".repeat(padding)));
        }
    }
    let count = if expanded { rows.len() } else { rows.len().min(10) };
    let hidden = rows.len() - count;
    rows.truncate(count);
    if hidden > 0 {
        let hint = truncate_str(&crate::i18n::tf!("+{hidden} diff lines · Ctrl+O to expand", hidden = hidden), inner_width);
        rows.push(format!("    \x1b[90m{hint}\x1b[0m"));
    }
    rows
}

fn history_lines(history: &[HistoryItem], width: usize) -> Vec<String> {
    let theme = crate::theme::current();
    let primary = theme.primary_ansi();
    let secondary = theme.secondary_ansi();
    let mut lines = Vec::new();
    let mut index = 0;
    while index < history.len() {
        match &history[index] {
            HistoryItem::UserPrompt(text) => {
                for (i, line) in wrap_history_text(text, width).into_iter().enumerate() {
                    lines.push(format!("\x1b[1m{primary}{}{line}\x1b[0m", if i == 0 { "❯ " } else { "  " }));
                }
                lines.push(String::new());
            }
            HistoryItem::AssistantMessage(text) => {
                lines.extend(crate::markdown::render_markdown(&plain_terminal_text(text), width.saturating_sub(2).max(1))
                    .into_iter().map(|line| format!("{ASSISTANT_INDENT}{line}")));
                lines.push(String::new());
            }
            HistoryItem::ResponseStats { elapsed_ms, tokens, usage_complete } => {
                let stats = response_stats_text(*elapsed_ms, *tokens, *usage_complete);
                lines.extend(wrap_text_line(&stats, width.saturating_sub(2).max(1)).into_iter()
                    .map(|line| format!("{ASSISTANT_INDENT}\x1b[90m{line}\x1b[0m")));
                lines.push(String::new());
            }
            HistoryItem::Thought(text) => {
                lines.extend(thought_lines(text, width, is_output_expanded()));
            }
            HistoryItem::ToolStart { name, args } => {
                let start = index + 1;
                let mut end = start;
                while end < history.len() && matches!(history[end], HistoryItem::ToolLog(_) | HistoryItem::FileDiff(_)) { end += 1; }
                let completed = match history.get(end) {
                    Some(HistoryItem::ToolEnd { result, is_error, .. }) => Some((result.as_str(), *is_error)), _ => None,
                };
                let detail = tool_detail(name, completed.map(|c| c.0), completed.is_some_and(|c| c.1));
                let (label, target, detail) = tool_header_parts(name, args, &detail, width + 4);
                lines.push(format!("{secondary}{label}\x1b[0m{target}\x1b[90m{detail}\x1b[0m"));
                if let Some((result, true)) = completed {
                    lines.extend(tool_error_lines(result, width, is_output_expanded()));
                }
                let logs: Vec<_> = history[start..end].iter().filter_map(|item| match item {
                    HistoryItem::ToolLog(text) if !text.starts_with("$ ") && question_log_lines(text, width).is_none()
                        && !completed.is_some_and(|(result, error)| error && name == "run_command"
                            && plain_terminal_text(result).contains(plain_terminal_text(text).trim())) => Some(text.as_str()), _ => None,
                }).collect();
                lines.extend(tool_output_lines(&logs, width, is_output_expanded()));
                if completed.is_some_and(|(_, error)| !error) {
                    for item in &history[start..end] {
                        if let HistoryItem::FileDiff(diff) = item {
                            lines.extend(file_diff_lines(diff, width, is_output_expanded()));
                        }
                    }
                }
                // Answers are conversation content, so keep them visible even when tool output is collapsed.
                for item in &history[start..end] {
                    if let HistoryItem::ToolLog(text) = item {
                        if let Some(answer_lines) = question_log_lines(text, width) { lines.extend(answer_lines); }
                    }
                }
                index = if completed.is_some() { end + 1 } else { end };
                continue;
            }
            HistoryItem::FileDiff(diff) => {
                lines.extend(file_diff_lines(diff, width, is_output_expanded()));
            }
            HistoryItem::ToolLog(text) => {
                if let Some(answer_lines) = question_log_lines(text, width) { lines.extend(answer_lines); }
                else { lines.extend(wrap_history_text(text.trim(), width.max(1)).into_iter()
                    .map(|line| format!("    \x1b[90m{line}\x1b[0m")));
                }
            }
            HistoryItem::ToolEnd { name, args, result, is_error } => {
                let (label, target, detail) = tool_header_parts(name, args, &tool_detail(name, Some(result), *is_error), width);
                lines.push(format!("{secondary}{label}\x1b[0m{target}  {detail}"));
                if *is_error { lines.extend(tool_error_lines(result, width, is_output_expanded())); }
            }
            HistoryItem::Error(text) => {
                let display = text.strip_prefix("LLM error: ").map(|detail| crate::i18n::tf!("LLM error: {}", detail));
                lines.extend(tool_error_lines(display.as_deref().unwrap_or(text), width, is_output_expanded()));
            }
        }
        index += 1;
    }
    lines
}

fn banner_lines(model: &str, base_url: &str, workspace: &str, git_info: &str) -> Vec<String> {
    let mut bytes = Vec::new();
    let rows = print_banner_to(&mut bytes, model, base_url, workspace, git_info) as usize;
    let output = String::from_utf8_lossy(&bytes);
    let mut lines = vec![String::new(); rows];
    let mut row = 0;
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut parameters = String::new();
            while let Some(code) = chars.next() {
                if ('@'..='~').contains(&code) {
                    if code == 'H' {
                        row = parameters.split(';').next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).saturating_sub(1);
                    } else if code == 'm' && row < rows {
                        lines[row].push_str(&format!("\x1b[{parameters}m"));
                    }
                    break;
                }
                parameters.push(code);
            }
        } else if row < rows { lines[row].push(c); }
    }
    lines
}

struct ChatBackground {
    model: String,
    base_url: String,
    workspace: String,
    git_info: String,
    history: Vec<HistoryItem>,
}

static CHAT_BACKGROUND: Mutex<Option<ChatBackground>> = Mutex::new(None);

/// Reflow the current transcript above a menu, including live theme previews.
pub fn paint_chat_background(out: &mut impl Write, panel_top: u16) -> std::io::Result<()> {
    let background = CHAT_BACKGROUND.lock().unwrap();
    let Some(chat) = background.as_ref() else { return Ok(()); };
    let (cols, term_rows) = terminal_size();
    let mut lines = banner_lines(&chat.model, &chat.base_url, &chat.workspace, &chat.git_info);
    lines.extend(history_lines(&chat.history, cols.saturating_sub(6).max(1) as usize));
    let offset = HISTORY_VIEW.lock().unwrap().offset.min(lines.len().saturating_sub(panel_top as usize));
    let end = lines.len().saturating_sub(offset);
    let start = end.saturating_sub(panel_top as usize);
    let text = (0..panel_top).map(|row| lines.get(start + row as usize)
        .filter(|_| start + (row as usize) < end).cloned().unwrap_or_default()).collect::<Vec<_>>();
    let snow = crate::snow::Frame::new(cols.saturating_sub(1) as usize, text.len());
    let painted = text.iter().enumerate().map(|(row, line)| snow.line(line, row)).collect::<Vec<_>>();
    for (row, line) in painted.iter().enumerate() {
        queue!(out, cursor::MoveTo(0, row as u16), ResetColor,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(line), ResetColor)?;
    }
    remember_snow_surface(0, cols, term_rows, text, painted);
    invalidate_content_frame();
    Ok(())
}

pub fn redraw_all(
    model: &str, base_url: &str, workspace: &str, git_info: &str,
    history: &[HistoryItem], _branch_tag: &str, _current_prompt: &str,
) -> u16 {
    *CHAT_BACKGROUND.lock().unwrap() = Some(ChatBackground {
        model: model.to_string(), base_url: base_url.to_string(), workspace: workspace.to_string(),
        git_info: git_info.to_string(), history: history.to_vec(),
    });
    let _geometry = begin_terminal_frame();
    if terminal_is_small() { render_small_terminal(); return 0; }
    let (cols, rows) = terminal_size();
    let width = cols.saturating_sub(2).max(1) as usize;
    let mut lines = banner_lines(model, base_url, workspace, git_info);
    lines.extend(history_lines(history, cols.saturating_sub(6).max(1) as usize));
    let reserved = active_box_rows(rows).unwrap_or_else(|| INPUT_ROWS.load(std::sync::atomic::Ordering::Relaxed));
    let height = rows.saturating_sub(reserved.min(rows.saturating_sub(2)) + 1).max(1) as usize;
    let range = HISTORY_VIEW.lock().unwrap().range(lines.len(), height);
    let mut visible = lines[range.clone()].to_vec();
    visible.resize(height, String::new());
    let text = visible.clone();
    // Keep the text under the mouse fixed while a streamed response grows.
    let selection = MOUSE_SELECTION.lock().unwrap().clone();
    if let Some(selection) = selection.filter(|selection| selection.size == (cols, rows)) {
        visible = selection.highlighted();
        visible.resize(height, String::new());
        visible.truncate(height);
    } else {
        let snow = crate::snow::Frame::new(cols.saturating_sub(1) as usize, height);
        visible = visible.iter().enumerate().map(|(row, line)| snow.line(line, row)).collect();
    }
    let painted = visible.clone();
    visible.push(activity_line(width));

    // Only changed content rows are painted. The editor and dropdown own the bottom rows.
    let mut previous = CONTENT_FRAME.lock().unwrap();
    let same_size = previous.as_ref().is_some_and(|(w, h, _)| *w == cols && *h == rows);
    let mut frame = Vec::new();
    let nested = CONTENT_UPDATE.with(|active| active.get());
    if !nested { let _ = queue!(frame, crossterm::terminal::BeginSynchronizedUpdate); }
    let _ = queue!(frame, cursor::SavePosition, ResetColor);
    for (row, line) in visible.iter().enumerate() {
        if same_size && previous.as_ref().and_then(|(_, _, lines)| lines.get(row)) == Some(line) { continue; }
        let _ = queue!(frame, cursor::MoveTo(0, row as u16),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
            crossterm::style::Print(line), ResetColor);
    }
    let _ = queue!(frame, cursor::RestorePosition);
    if !nested { let _ = queue!(frame, crossterm::terminal::EndSynchronizedUpdate); }
    let mut out = stdout().lock();
    let _ = out.write_all(&frame);
    let _ = out.flush();
    drop(out);
    *previous = Some((cols, rows, visible));
    drop(previous);
    let mut clean = text.clone();
    clean.push(activity_line(width));
    *TEXT_FRAME.lock().unwrap() = Some((cols, rows, clean));
    remember_snow_surface(0, cols, rows, text, painted);
    range.len().min(height.saturating_sub(1)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_renders_with_its_own_label() {
        let (label, target, detail) = tool_header_parts("web_search", r#"{"query":"Rust documentation"}"#, "", 80);
        assert!(label.contains("websearch"));
        assert!(target.contains("Rust documentation"));
        assert!(detail.is_empty());
    }

    #[test]
    fn successive_stream_chunks_never_print_inside_the_prompt() {
        let previous = FRAME_SIZE.with(|size| size.replace(Some((80, 24))));
        let restore = TerminalFrame { previous };
        let row = Arc::new(Mutex::new(5));
        let mut writer = StreamWriter::new(row, "🤖 ", Color::Green, "   ", "", "main");
        let mut output = Vec::new();
        for chunk in ["Hello ", "world ", "\nAnother ", "line "] {
            writer.write_token_to(chunk, &mut output);
        }
        let output = String::from_utf8(output).unwrap();
        let mut characters = output.chars().peekable();
        let mut current_row = 0;
        let mut text = String::new();
        while let Some(character) = characters.next() {
            if character == '\x1b' {
                assert_eq!(characters.next(), Some('['));
                let mut arguments = String::new();
                while let Some(command) = characters.next() {
                    if command.is_ascii_alphabetic() {
                        if command == 'H' {
                            current_row = arguments.split(';').next().unwrap().parse::<u16>().unwrap();
                        }
                        break;
                    }
                    arguments.push(command);
                }
            } else {
                assert!(current_row < 22, "Printed {character:?} inside the prompt at row {current_row}");
                text.push(character);
            }
        }
        assert!(text.contains("Hello world"));
        assert!(text.contains("Another line"));
        assert_eq!(current_row, 23, "Visible cursor should finish in the prompt");
        drop(restore);
    }

    #[test]
    fn terminal_frame_uses_one_geometry_and_restores_the_previous_geometry() {
        let previous = FRAME_SIZE.with(|size| size.replace(Some((19, 4))));
        let restore = TerminalFrame { previous };
        let frame = begin_terminal_frame();
        assert_eq!(terminal_size(), (19, 4));
        assert_eq!(get_box_width(), 19);
        assert!(terminal_is_small());
        drop(frame);
        assert_eq!(terminal_size(), (19, 4));
        drop(restore);
    }

    #[test]
    fn resize_replay_retains_every_stream_token_including_pending_text() {
        let previous = FRAME_SIZE.with(|size| size.replace(Some((12, 4))));
        let restore = TerminalFrame { previous };
        let row = Arc::new(Mutex::new(0));
        let mut writer = StreamWriter::new(row, "🤖 ", Color::Green, "   ", "", "main");
        writer.write_token("First unfinished");
        writer.write_token(" word");
        assert_eq!(writer.raw_text(), "First unfinished word");
        drop(restore);
    }

    #[test]
    fn response_stats_follow_answer_and_survive_serialization() {
        assert_eq!(response_stats_text(12345, Some(1832), true), "Completed in 12.3s · 1832 tokens");
        assert_eq!(response_stats_text(125000, None, false), "Completed in 2m 05s · tokens unavailable");
        assert_eq!(response_stats_text(3601000, Some(42), false), "Completed in 1h 00m 01s · ≥ 42 tokens");
        let history = vec![HistoryItem::AssistantMessage("FINAL_ANSWER".into()),
            HistoryItem::ResponseStats { elapsed_ms: 12345, tokens: Some(1832), usage_complete: true }];
        let restored: Vec<HistoryItem> = serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();
        let lines = history_lines(&restored, 80);
        let answer_row = lines.iter().position(|line| line.contains("FINAL_ANSWER")).unwrap();
        assert!(lines[answer_row + 1].is_empty());
        assert!(lines[answer_row + 2].contains("Completed in 12.3s · 1832 tokens"));
        let narrow = history_lines(&restored, 20);
        assert!(narrow.iter().all(|line| str_width(&plain_terminal_text(line)) <= 20));
    }

    #[test]
    fn collapsed_tool_output_counts_wrapped_screen_rows() {
        let long_line = "x".repeat(310);
        let logs = [long_line.as_str(), "TAIL"];
        let collapsed = tool_output_lines(&logs, 20, false);
        assert_eq!(collapsed.len(), 4);
        assert!(collapsed[..3].iter().all(|line| plain_terminal_text(line) == format!("    {}", "x".repeat(20))));
        assert!(collapsed[3].contains("+14 lines  · Ctrl+O to expand"));
        assert!(!collapsed.iter().any(|line| line.contains("TAIL")));
        let expanded = tool_output_lines(&logs, 20, true);
        assert_eq!(expanded.len(), 17);
        assert!(expanded.last().unwrap().contains("TAIL"));
        assert!(!expanded.iter().any(|line| line.contains("Ctrl+O")));
        assert_eq!(tool_output_lines(&["short"], 20, false).len(), 1);
    }

    #[test]
    fn user_prompt_history_wraps_whole_words_and_skill_names() {
        let text = "используй $claude-design и $frontend-excellence для проверки результата";
        let history = vec![HistoryItem::UserPrompt(text.into())];
        let lines = history_lines(&history, 24);
        let content: Vec<_> = lines.iter().filter(|line| !line.is_empty())
            .map(|line| plain_terminal_text(line).chars().skip(2).collect::<String>()).collect();
        assert_eq!(content.concat(), text);
        assert!(content.iter().all(|line| str_width(line) <= 24));
        for word in text.split_whitespace() {
            assert!(content.iter().any(|line| line.contains(word)), "Split word: {word}");
        }
        assert_eq!(wrap_text_line("проверки результата", 16), ["проверки ", "результата"]);
        assert_eq!(wrap_text_line("a abcdefghijk", 5), ["a ", "abcde", "fghij", "k"]);
    }

    #[test]
    fn question_body_preserves_full_question_and_custom_answer() {
        let question = "Какой точный путь к папке сайта? В репозитории вижу каталог npm — подтверди расположение.";
        let answer = "Письменный ответ с иероглифами 界界 и эмодзи 🤖🤖. ".repeat(5);
        for options in [vec![], vec!["Первый вариант".into(), "Второй вариант".into()]] {
            let item = crate::agent::QuestionItem {
                question: question.into(), header: None, options, is_multi_select: false,
                allow_custom: true, placeholder: None,
            };
            for width in [16, 24, 80] {
                let (lines, focus) = question_body_lines(&item, &Default::default(),
                    item.options.len(), &answer, true, width);
                assert!(lines.iter().all(|line| str_width(&line.prefix) + str_width(&line.text) <= width));
                let text = lines.iter().map(|line| line.text.as_str()).collect::<String>();
                assert!(text.starts_with(question));
                assert!(text.ends_with(&format!("{answer}█")));
                assert!(lines[focus].text.ends_with('█'));
                let (confirmed, _) = question_body_lines(&item, &Default::default(),
                    item.options.len(), &answer, false, width);
                let text = confirmed.iter().map(|line| line.text.as_str()).collect::<String>();
                assert!(text.ends_with(&answer));
            }
        }
    }

    #[test]
    fn question_transcript_preserves_long_text_without_terminal_autowrap() {
        let text = "Мы хотим нанимать ML специалистов на роль фронтендера. ".repeat(8);
        for width in [20, 40, 80] {
            for prefix in ["  📋 Question: ", "     Answer: "] {
                let lines = question_transcript_lines(prefix, &text, width);
                assert!(lines.len() > 1);
                assert!(lines.iter().all(|(label, line)| str_width(label) + str_width(line) < width));
                assert_eq!(lines.iter().map(|(_, line)| line.as_str()).collect::<String>(), text);
            }
        }
    }

    #[test]
    fn streaming_cursor_stays_in_the_bottom_prompt() {
        assert_eq!(bottom_prompt_cursor("", 80, 24), (4, 22));
        assert_eq!(bottom_prompt_cursor("test", 80, 40), (8, 38));
        assert_eq!(bottom_prompt_cursor(&"я".repeat(80), 80, 24), (10, 22));
    }

    #[test]
    fn editing_an_answer_preserves_other_answers_without_duplicates() {
        let answer = |index, text: &str| crate::agent::QuestionAnswer {
            question_index: index,
            question: format!("Question {}", index),
            selected_options: Vec::new(),
            custom_text: Some(text.into()),
        };
        let mut answers = Vec::new();
        save_question_answer(&mut answers, answer(0, "First"));
        save_question_answer(&mut answers, answer(1, "Second"));
        save_question_answer(&mut answers, answer(0, "Changed"));
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].custom_text.as_deref(), Some("Changed"));
        assert_eq!(answers[1].custom_text.as_deref(), Some("Second"));
    }

    #[test]
    fn review_wraps_full_answers_and_marks_unanswered_questions() {
        let questions = vec![crate::agent::QuestionItem {
            question: "A long question that must remain readable".into(),
            header: None, options: vec![], is_multi_select: false,
            allow_custom: true, placeholder: None,
        }; 2];
        let answers = vec![crate::agent::QuestionAnswer {
            question_index: 0, question: questions[0].question.clone(),
            selected_options: vec!["First choice".into(), "Second choice".into()],
            custom_text: Some("A long custom answer that must remain readable".into()),
        }];
        let (lines, ranges) = question_review_lines(&questions, &answers, 20, 0);
        assert!(lines.iter().all(|(line, _)| str_width(line) <= 20));
        let first = lines[ranges[0].clone()].iter().map(|(line, _)| line.as_str()).collect::<String>();
        assert!(first.contains(&questions[0].question));
        assert!(first.contains("First choice | Second choice"));
        assert!(first.contains(answers[0].custom_text.as_ref().unwrap()));
        assert!(lines[ranges[1].clone()].iter().map(|(line, _)| line.as_str()).collect::<String>()
            .contains("[Skipped / No answer]"));
        let cyan_rows: Vec<_> = lines.iter().filter(|(_, color)| *color == Color::Cyan).collect();
        assert!(cyan_rows.len() > 1);
        assert_eq!(cyan_rows.iter().map(|(line, _)| line.as_str()).collect::<String>(),
            format!("▶ 1. {}", questions[0].question));
        assert!(lines[ranges[1].clone()].iter().all(|(_, color)| *color == Color::White));
        let (refocused, _) = question_review_lines(&questions, &answers, 20, 1);
        assert_eq!(refocused.iter().filter(|(_, color)| *color == Color::Cyan)
            .map(|(line, _)| line.as_str()).collect::<String>(),
            format!("▶ 2. {}", questions[1].question));
    }

    #[test]
    fn radio_focus_is_the_selected_answer() {
        let mut selected = std::collections::HashSet::new();
        focus_question_option(&mut selected, 0, 3, false);
        assert_eq!(selected, std::collections::HashSet::from([0]));
        focus_question_option(&mut selected, 2, 3, false);
        assert_eq!(selected, std::collections::HashSet::from([2]));
        focus_question_option(&mut selected, 3, 3, false);
        assert!(selected.is_empty());
    }

    #[test]
    fn checkbox_focus_preserves_explicit_selections() {
        let mut selected = std::collections::HashSet::from([0, 2]);
        focus_question_option(&mut selected, 1, 3, true);
        assert_eq!(selected, std::collections::HashSet::from([0, 2]));
    }

    #[test]
    fn question_is_anchored_to_terminal_bottom() {
        for option_rows in [1, 3, 8] {
            let layout = question_layout(9, 26, option_rows, 0);
            assert_eq!(layout.start_row + option_rows as u16 + 4, 26);
            assert_eq!(layout.scroll, 0);
            assert_eq!(layout.visible_options, 0..option_rows);
        }
    }

    #[test]
    fn questionnaire_history_is_readable_and_never_collapses_answers() {
        let mut history = vec![HistoryItem::ToolStart {
            name: "ask_question".into(), args: r#"{"questions":[{}, {}, {}, {}, {}]}"#.into(),
        }];
        for index in 0..5 {
            history.push(HistoryItem::ToolLog(format!("  📋 Question: Prompt {index}")));
            history.push(HistoryItem::ToolLog(format!("     Answer: Choice {index}")));
        }
        history.push(HistoryItem::ToolEnd { name: "ask_question".into(), args: String::new(),
            result: "completed".into(), is_error: false });
        let rendered = history_lines(&history, 74).join("\n");
        assert!(rendered.contains("5 questions"));
        for index in 0..5 {
            assert!(rendered.contains(&format!("Prompt {index}")));
            assert!(rendered.contains(&format!("Choice {index}")));
        }
        for unwanted in ["{", "📋", "Question:", "Answer:", "to expand"] {
            assert!(!rendered.contains(unwanted), "{rendered}");
        }
    }

    #[test]
    fn question_scroll_preserves_end_of_transcript() {
        let layout = question_layout(20, 24, 8, 0);
        assert_eq!(layout.start_row, 12);
        assert_eq!(20 - layout.scroll, layout.start_row);
        assert_eq!(layout.start_row + layout.visible_options.len() as u16 + 4, 24);
    }

    #[test]
    fn question_keeps_focused_option_visible_on_short_screens() {
        for term_rows in [5, 6, 10, 24] {
            // The last selectable row is the custom answer.
            for focused in 0..31 {
                let layout = question_layout(9, term_rows, 31, focused);
                assert!(layout.visible_options.contains(&focused));
                assert!(layout.visible_options.end <= 31);
                assert!(layout.start_row + layout.visible_options.len() as u16 + 4 <= term_rows);
            }
        }
    }

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

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn reasoning_tokens_are_visible_before_completion_and_final_text_does_not_duplicate_them() {
        let mut history = Vec::new();
        let mut index = None;
        update_streamed_thought(&mut history, &mut index, "First", false);
        assert!(history_lines(&history, 80).join("\n").contains("First"));
        update_streamed_thought(&mut history, &mut index, " second", false);
        assert!(history_lines(&history, 80).join("\n").contains("First second"));
        update_streamed_thought(&mut history, &mut index, "First second complete", true);
        assert_eq!(history.len(), 1);
        assert!(matches!(&history[0], HistoryItem::Thought(text) if text == "First second complete"));
        index = None;
        update_streamed_thought(&mut history, &mut index, "Next request", false);
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn reasoning_is_collapsed_until_explicitly_expanded() {
        let text = "first\n\nsecond\nthird\nfourth\nfifth";
        let compact = thought_lines(text, 80, false).join("\n");
        assert!(compact.contains("third"));
        assert!(!compact.contains("fourth"));
        assert!(compact.contains("+2 reasoning lines"));
        let full = thought_lines(text, 80, true).join("\n");
        assert!(full.contains("fourth") && full.contains("fifth"));
        assert!(!full.contains("Ctrl+O"));
    }

    #[test]
    fn viewport_keeps_visible_lines_when_new_content_arrives_and_returns_to_latest() {
        let mut view = HistoryViewport::default();
        assert_eq!(view.range(100, 20), 80..100);
        view.scroll(10);
        assert_eq!(view.range(100, 20), 70..90);
        assert_eq!(view.range(115, 20), 70..90);
        view.scroll(i32::MAX);
        assert_eq!(view.range(115, 20), 0..20);
        view.scroll(i32::MIN);
        assert_eq!(view.range(115, 20), 95..115);
        view.scroll(3);
        assert_eq!(view.range(5, 20), 0..5);
        assert_eq!(view.offset, 0);
    }

    #[test]
    fn scrollback_contains_full_answers_and_one_header_per_tool() {
        let history = vec![
            HistoryItem::UserPrompt("question".into()),
            HistoryItem::ToolStart { name: "read_file".into(), args: r#"{"path":"file.txt"}"#.into() },
            HistoryItem::ToolEnd { name: "read_file".into(), args: r#"{"path":"file.txt"}"#.into(), result: "first\nsecond".into(), is_error: false },
            HistoryItem::AssistantMessage("line one\nline two\nline three".into()),
        ];
        let text = history_lines(&history, 60).join("\n");
        assert_eq!(text.matches("file.txt").count(), 1);
        assert!(text.contains("line one") && text.contains("line three"));
        assert!(text.contains("2 lines"));
    }

    #[test]
    fn command_controls_are_removed_before_wrapping() {
        let output = "\x1b[2J\x1b[24;1H\x1b[38;2;255;195;0mHello world!\x1b[0m\x1b[?1049l\x1b[?2026h";
        assert_eq!(plain_terminal_text(output), "Hello world!");
        assert_eq!(wrap_history_text(output, 6), ["Hello ", "world!"]);
        assert_eq!(plain_terminal_text("\x1b]0;title\x07text\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\"), "textlink");
        assert_eq!(plain_terminal_text("\x1bPpayload\x1b\\\u{009b}2Jok\x1b[38;2;"), "ok");
        assert_eq!(plain_terminal_text("abc\x08d\rnext\r\n\tend\x07"), "abd\nnext\n    end");
    }

    #[test]
    fn tool_history_never_replays_command_terminal_controls() {
        let raw = "\x1b[2J\x1b[23;5H\x1b[38;2;255;195;0mHello world!\x1b[0m";
        let history = vec![
            HistoryItem::ToolStart { name: "run_command".into(), args: r#"{"command":"cargo test"}"#.into() },
            HistoryItem::ToolLog(raw.into()),
            HistoryItem::ToolEnd { name: "run_command".into(), args: String::new(), result: raw.into(), is_error: true },
            HistoryItem::ToolLog(raw.into()),
            HistoryItem::AssistantMessage(raw.into()),
        ];
        let rendered = history_lines(&history, 30).join("\n");
        assert!(rendered.contains("Hello world!"));
        assert!(!rendered.contains("\x1b[2J") && !rendered.contains("\x1b[23;5H"));
        assert!(!rendered.contains("38;2;255;195;0mHello"));
    }

    #[test]
    fn long_errors_require_expansion_including_wrapped_lines() {
        let result = "Command failed\nshort\nthird\nHIDDEN_ERROR_DETAIL\nfinal";
        let collapsed = tool_error_lines(result, 40, false).join("\n");
        assert!(collapsed.contains("Command failed") && collapsed.contains("+2 lines"));
        assert!(!collapsed.contains("HIDDEN_ERROR_DETAIL"));
        let expanded = tool_error_lines(result, 40, true).join("\n");
        assert!(expanded.contains("HIDDEN_ERROR_DETAIL") && expanded.contains("final"));
        assert!(!expanded.contains("to expand"));
        let wrapped = tool_error_lines(&"x".repeat(100), 10, false);
        assert_eq!(wrapped.len(), 4);
        assert!(wrapped.last().unwrap().contains("+7 lines"));
    }

    #[test]
    fn selection_copies_plain_unicode_text_in_both_directions() {
        let selection = MouseSelection {
            start: (0, 2), end: (1, 3), size: (80, 24),
            lines: vec!["  \x1b[31mПривет  \x1b[0m".into(), "  мир!".into()],
        };
        assert_eq!(selection.text(), "Привет\n  ми");
        let reverse = MouseSelection { start: selection.end, end: selection.start, ..selection.clone() };
        assert_eq!(reverse.text(), selection.text());
        assert!(selection.highlighted()[0].contains("\x1b[7mПривет"));
        assert_eq!(column_slice("界a\u{301}b", 2, 3), "a\u{301}");
    }
}
