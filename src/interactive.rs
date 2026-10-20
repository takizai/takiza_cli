use crate::cli_ui::disable_raw_mode;
use crate::config::Config;
use crate::prompt::{str_width, truncate_visible};
use crate::session::Session;
use crate::theme::{self, Theme};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{enable_raw_mode, Clear, ClearType},
};
use std::io::{stdout, IsTerminal, Write};
use std::path::Path;

/// Buffer menu painting so clearing, transcript and controls appear together.
#[derive(Default)]
struct MenuFrame {
    bytes: Vec<u8>,
}

impl Write for MenuFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.bytes.is_empty() { return Ok(()); }
        let mut frame = Vec::new();
        queue!(frame, crossterm::terminal::BeginSynchronizedUpdate)?;
        frame.extend_from_slice(&self.bytes);
        queue!(frame, crossterm::terminal::EndSynchronizedUpdate)?;
        let mut out = stdout().lock();
        out.write_all(&frame)?;
        out.flush()?;
        self.bytes.clear();
        Ok(())
    }
}

pub(crate) fn fit_menu_text(text: &str, width: usize) -> String {
    if crate::markdown::visible_width(text) <= width { return text.to_string(); }
    if width == 0 { return String::new(); }
    let mut result = String::new();
    let mut used = 0;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            result.push(ch);
            result.push(chars.next().unwrap());
            for code in chars.by_ref() {
                result.push(code);
                if ('@'..='~').contains(&code) { break; }
            }
            continue;
        }
        let columns = crate::prompt::char_width(ch);
        if used + columns > width.saturating_sub(1) {
            result.push_str("…\x1b[0m");
            break;
        }
        result.push(ch);
        used += columns;
    }
    result
}

#[derive(Clone, Debug)]
pub struct ScreenContext {
    pub model: String,
    pub base_url: String,
    pub workspace: String,
    pub git_info: String,
    pub branch_tag: String,
}

impl ScreenContext {
    pub fn from_config(config: &Config) -> Self {
        let git = crate::git::GitInfo::get(&config.workspace_dir);
        let git_display = match &git.branch {
            Some(b) => {
                if git.is_dirty {
                    crate::i18n::tf!("{} (dirty)", b)
                } else {
                    crate::i18n::tf!("{} (clean)", b)
                }
            }
            None => crate::i18n::tr("(no git)").to_string(),
        };
        let branch_tag = match &git.branch {
            Some(b) => {
                if git.is_dirty {
                    format!("{}*", b)
                } else {
                    b.clone()
                }
            }
            None => "".to_string(),
        };

        Self {
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            workspace: config.workspace_dir.display().to_string(),
            git_info: git_display,
            branch_tag,
        }
    }
}

/// Language selection preserves the surrounding terminal mode and transcript.
pub fn select_language_interactive(ctx: Option<&ScreenContext>, initial: crate::i18n::Language) -> std::io::Result<Option<crate::i18n::Language>> {
    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw { enable_raw_mode()?; }
    let result = (|| {
        let mut selected = crate::i18n::Language::ALL.iter().position(|language| *language == initial).unwrap_or(0);
        let mut panel = BottomSelectionPanel::default();
        loop {
            let th = theme::current();
            let lines = crate::i18n::Language::ALL.iter().enumerate().map(|(index, language)| (
                format!("{} {}", if index == selected { "❯ (●)" } else { "  ( )" }, language.name()),
                if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() },
            )).collect::<Vec<_>>();
            let hint = crate::i18n::tr("↑/↓ · Enter: Confirm · Esc: Cancel");
            if let Some(ctx) = ctx {
                panel.draw(ctx, crate::i18n::tr("Select Language"), &lines, hint)?;
            } else {
                crate::onboarding::Onboarding::render_language_selector(&lines, hint)?;
            }
            match crate::cli_ui::read_event_with_background()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => selected = (selected + 2) % 3,
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % 3,
                    KeyCode::Enter => return Ok(Some(crate::i18n::Language::ALL[selected])),
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(None),
                    _ => {},
                },
                _ => {},
            }
        }
    })();
    if !was_raw { disable_raw_mode()?; }
    result
}

#[derive(Clone, Copy)]
pub enum ConfigAction { Language, Theme, Provider, Model, Mode, Effort, Approval }

pub fn configure_interactive(ctx: &ScreenContext, config: &Config, prefs: &theme::UserPreferences, initial: usize) -> std::io::Result<Option<ConfigAction>> {
    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw { enable_raw_mode()?; }
    let result = (|| {
        let mut selected = initial.min(6);
        let mut panel = BottomSelectionPanel::default();
        loop {
            let effort_levels = config.reasoning_levels();
            let entries = [
                (ConfigAction::Language, crate::i18n::tr("Language"), prefs.language.name().to_string()),
                (ConfigAction::Theme, crate::i18n::tr("Visual Theme"), prefs.theme.name().to_string()),
                (ConfigAction::Provider, crate::i18n::tr("Provider"), config.base_url.clone()),
                (ConfigAction::Model, crate::i18n::tr("Model"), config.model.clone()),
                (ConfigAction::Mode, crate::i18n::tr("Mode"), crate::i18n::tr("Third-party provider").to_string()),
                (ConfigAction::Effort, crate::i18n::tr("Reasoning Effort"), if effort_levels.is_empty() { crate::i18n::tr("Model does not support adjustable effort").into() } else { config.effort.as_deref().filter(|e| effort_levels.iter().any(|l| l == e)).unwrap_or("default").into() }),
                (ConfigAction::Approval, crate::i18n::tr("Execution mode"), config.execution_mode.current().name().to_string()),
            ];
            let (_, rows) = crate::cli_ui::terminal_size();
            let visible = rows.saturating_sub(13).clamp(1, entries.len() as u16) as usize;
            let offset = selected.saturating_sub(visible.saturating_sub(1));
            let th = theme::current();
            let lines = entries.iter().enumerate().skip(offset).take(visible).map(|(index, (_, label, value))| {
                let label = crate::i18n::tr(label);
                let padding = " ".repeat(22usize.saturating_sub(str_width(label)));
                (format!("{} {label}{padding} {value}", if index == selected { "❯" } else { " " }),
                    if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() })
            }).collect::<Vec<_>>();
            panel.draw(ctx, crate::i18n::tr("Settings"), &lines, crate::i18n::tr("↑/↓ · Enter: Select · Esc: Back"))?;
            match crate::cli_ui::read_event_with_background()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => selected = (selected + entries.len() - 1) % entries.len(),
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % entries.len(),
                    KeyCode::Enter => return Ok(Some(entries[selected].0)),
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(None),
                    _ => {},
                },
                _ => {},
            }
        }
    })();
    if !was_raw { disable_raw_mode()?; }
    result
}

pub(crate) fn draw_bottom_box_top(
    out: &mut impl Write,
    row: u16,
    title: &str,
    box_w: usize,
    primary: Color,
    border: Color,
) -> std::io::Result<()> {
    crate::cli_ui::paint_chat_background(out, row)?;
    let title_tag = format!(" {} ", title);
    let title_w = str_width(&title_tag);
    let (safe_title, safe_title_w) = if title_w + 4 >= box_w {
        { let title = format!(" {} ", truncate_visible(title, box_w.saturating_sub(6)));
          let width = str_width(&title); (title, width) }
    } else {
        (title_tag, title_w)
    };
    let dashes = box_w.saturating_sub(safe_title_w + 3);

    queue!(
        out,
        cursor::MoveTo(0, row),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(border),
        Print("╭─"),
        SetForegroundColor(primary),
        Print(&safe_title),
        SetForegroundColor(border),
        Print("─".repeat(dashes)),
        Print("╮"),
        ResetColor
    )?;
    Ok(())
}

fn draw_bottom_box_divider(
    out: &mut impl Write,
    row: u16,
    box_w: usize,
    border: Color,
) -> std::io::Result<()> {
    let dashes = box_w.saturating_sub(2);
    queue!(
        out,
        cursor::MoveTo(0, row),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(border),
        Print("├"),
        Print("─".repeat(dashes)),
        Print("┤"),
        ResetColor
    )?;
    Ok(())
}

pub(crate) fn draw_bottom_box_line(
    out: &mut impl Write,
    row: u16,
    content: &str,
    _visible_width: usize,
    box_w: usize,
    border: Color,
) -> std::io::Result<()> {
    let inner_w = box_w.saturating_sub(4);
    let content = fit_menu_text(content, inner_w);
    let pad = inner_w.saturating_sub(crate::markdown::visible_width(&content));

    queue!(
        out,
        cursor::MoveTo(0, row),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(border),
        Print("│ "),
        ResetColor,
        Print(&content),
        Print(" ".repeat(pad)),
        SetForegroundColor(border),
        Print(" │"),
        ResetColor
    )?;
    Ok(())
}

pub(crate) fn draw_bottom_box_bottom(
    out: &mut impl Write,
    row: u16,
    hint: &str,
    box_w: usize,
    border: Color,
) -> std::io::Result<()> {
    let hint_tag = format!(" {} ", truncate_visible(hint, box_w.saturating_sub(5)));
    let hint_w = str_width(&hint_tag);
    let dashes = box_w.saturating_sub(hint_w + 3);

    queue!(
        out,
        cursor::MoveTo(0, row),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(border),
        Print("╰"),
        Print("─".repeat(dashes)),
        SetForegroundColor(Color::DarkGrey),
        Print(&hint_tag),
        SetForegroundColor(border),
        Print("─╯"),
        ResetColor
    )?;
    Ok(())
}

/// Prompts user to input a single text string with visual box.
pub fn prompt_string_input(ctx: &ScreenContext, title: &str, prompt_label: &str, default_val: &str) -> std::io::Result<Option<String>> {
    prompt_string_input_inner(ctx, title, prompt_label, default_val, false)
}

fn prompt_string_input_inner(ctx: &ScreenContext, title: &str, prompt_label: &str, default_val: &str, secret: bool) -> std::io::Result<Option<String>> {
    let mut buffer = default_val.to_string();
    let mut cursor_idx = buffer.chars().count();
    let th = theme::current();
    let b_color = th.border_crossterm();
    let p_color = th.primary_crossterm();
    let mut needs_clear = true;

    loop {
        let (_, term_rows) = crate::cli_ui::terminal_size();
        let box_w = crate::cli_ui::get_box_width();
        let mut out = MenuFrame::default();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let start_row = term_rows.saturating_sub(3);

        let full_title = if ctx.branch_tag.is_empty() {
            title.to_string()
        } else {
            format!("{} [{}]", title, ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &full_title, box_w, p_color, b_color)?;
        let inner_w = box_w.saturating_sub(4);
        let label = truncate_visible(prompt_label, inner_w / 2);
        let available = inner_w.saturating_sub(str_width(&label) + 1);
        let (displayed, caret) = connection_input_window(&buffer, cursor_idx, secret, available);
        let display_line = format!("{label} {displayed}");
        let display_w = str_width(&display_line);
        draw_bottom_box_line(&mut out, start_row + 1, &display_line, display_w, box_w, b_color)?;
        draw_bottom_box_bottom(&mut out, start_row + 2, crate::i18n::tr("Enter: confirm  •  Esc: cancel"), box_w, b_color)?;
        
        let cursor_x = (2 + str_width(&label) + 1 + caret).min(box_w.saturating_sub(3));
        queue!(out, cursor::MoveTo(cursor_x as u16, start_row + 1), cursor::Show)?;
        out.flush()?;

        match crate::cli_ui::read_event_with_background()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Enter => {
                        return Ok(Some(buffer.trim().to_string()));
                    }
                    KeyCode::Esc => {
                        return Ok(None);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {buffer.clear(); cursor_idx=0;},
                    KeyCode::Home => cursor_idx=0,
                    KeyCode::End => cursor_idx=buffer.chars().count(),
                    KeyCode::Char(c) if !key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        let mut chars: Vec<char> = buffer.chars().collect();
                        chars.insert(cursor_idx, c);
                        buffer = chars.into_iter().collect();
                        cursor_idx += 1;
                    }
                    KeyCode::Backspace => {
                        if cursor_idx > 0 {
                            let mut chars: Vec<char> = buffer.chars().collect();
                            cursor_idx -= 1;
                            chars.remove(cursor_idx);
                            buffer = chars.into_iter().collect();
                        }
                    }
                    KeyCode::Delete => {
                        let mut chars: Vec<char> = buffer.chars().collect();
                        if cursor_idx < chars.len() {
                            chars.remove(cursor_idx);
                            buffer = chars.into_iter().collect();
                        }
                    }
                    KeyCode::Left => {
                        if cursor_idx > 0 {
                            cursor_idx -= 1;
                        }
                    }
                    KeyCode::Right => {
                        if cursor_idx < buffer.chars().count() {
                            cursor_idx += 1;
                        }
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                let mut chars=buffer.chars().collect::<Vec<_>>();
                let pasted=text.chars().filter(|ch|!ch.is_control()).collect::<Vec<_>>();
                let added=pasted.len(); chars.splice(cursor_idx..cursor_idx,pasted); buffer=chars.into_iter().collect(); cursor_idx+=added;
            }
            Event::Resize(_, _) => {
                needs_clear = true;
            }
            _ => {}
        }
    }
}

include!("provider_ui.rs");

fn advance_theme_secret(progress: &mut usize, key: crossterm::event::KeyEvent) -> bool {
    const SEQUENCE: [KeyCode; 10] = [
        KeyCode::Up, KeyCode::Up, KeyCode::Down, KeyCode::Down,
        KeyCode::Left, KeyCode::Right, KeyCode::Left, KeyCode::Right,
        KeyCode::Char('b'), KeyCode::Char('a'),
    ];
    if key.modifiers.intersects(crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT) {
        *progress = 0;
        return false;
    }
    let code = match key.code {
        KeyCode::Char(ch) => KeyCode::Char(ch.to_ascii_lowercase()),
        code => code,
    };
    *progress = if code == SEQUENCE[*progress] { *progress + 1 }
        else if code == SEQUENCE[0] { 1 } else { 0 };
    if *progress == SEQUENCE.len() { *progress = 0; true } else { false }
}

pub fn select_theme_interactive(ctx: &ScreenContext, initial: Theme) -> std::io::Result<Option<Theme>> {
    enable_raw_mode()?;
    let themes = Theme::all();
    let mut selected_idx = themes.iter().position(|&t| t == initial).unwrap_or(0);
    let mut needs_clear = true;
    let mut secret_progress = 0;

    loop {
        let current_theme = themes[selected_idx];
        theme::set_current(current_theme);

        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let b_color = current_theme.border_crossterm();
        let p_color = current_theme.primary_crossterm();

        let mut out = MenuFrame::default();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let visible_count = (term_rows as usize).saturating_sub(8).clamp(1, 6).min(themes.len());
        let scroll_offset = selected_idx.saturating_sub(visible_count.saturating_sub(1));
        let total_box_height = 1 + visible_count + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            crate::i18n::tr("Select Visual Theme").to_string()
        } else {
            crate::i18n::tf!("Select Visual Theme [{}]", ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, &t) in themes.iter().enumerate().skip(scroll_offset).take(visible_count) {
            let is_sel = i == selected_idx;
            let is_init = t == initial;

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_init {
                (crate::i18n::tr("\x1b[1;38;2;40;220;120m[active]\x1b[0m"), crate::markdown::visible_width(crate::i18n::tr("\x1b[1;38;2;40;220;120m[active]\x1b[0m")))
            } else {
                ("        ", 8)
            };
            let line = if is_sel {
                format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{:<12}\x1b[0m \x1b[38;2;160;160;165m{}\x1b[0m  {}", prefix, t.primary_ansi(), t.name(), t.description(), mark_str)
            } else {
                format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{:<12}\x1b[0m \x1b[38;2;120;120;125m{}\x1b[0m  {}", prefix, t.name(), t.description(), mark_str)
            };
            let vis_len = 2 + 12 + 1 + str_width(t.description()) + 2 + mark_vis;
            draw_bottom_box_line(&mut out, cur_row, &line, vis_len, box_w, b_color)?;
            cur_row += 1;
        }

        let hint = crate::i18n::tf!("↑/↓: {}/{}  •  Live Preview  •  Enter: Confirm  Esc: Cancel", selected_idx + 1, themes.len());
        draw_bottom_box_bottom(&mut out, cur_row, &hint, box_w, b_color)?;
        out.flush()?;

        match crate::cli_ui::read_event_with_background()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if advance_theme_secret(&mut secret_progress, key) {
                    theme::set_current(Theme::EveningIrkutsk);
                    disable_raw_mode()?;
                    return Ok(Some(Theme::EveningIrkutsk));
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        } else {
                            selected_idx = themes.len() - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < themes.len() {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        return Ok(Some(themes[selected_idx]));
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        theme::set_current(initial);
                        disable_raw_mode()?;
                        return Ok(None);
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                needs_clear = true;
            }
            _ => {}
        }
    }
}

// All current connections use provider credentials. Takiza account login will
// supply its own mode and quota UI when that authentication flow is implemented.
pub fn show_external_mode_notice(ctx: &ScreenContext) -> std::io::Result<()> {
    show_provider_notice(ctx, &[crate::i18n::tr("A third-party provider is active. Takiza mode selection is unavailable.")])
}

fn show_provider_notice(ctx: &ScreenContext, details: &[&str]) -> std::io::Result<()> {
    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw { enable_raw_mode()?; }
    let result = (|| {
        let mut panel = BottomSelectionPanel::default();
        loop {
            let width = crate::cli_ui::get_box_width().saturating_sub(4).max(1);
            let lines = details.iter().flat_map(|line| crate::markdown::wrap_rendered_line(line, width))
                // The wrapper appends an ANSI reset even to plain text. This
                // panel accepts plain text and supplies its own row colors.
                .map(|line| (line.trim_end_matches("\x1b[0m").to_string(), theme::current().secondary_crossterm())).collect::<Vec<_>>();
            panel.draw(ctx, crate::i18n::tr("Third-party provider"), &lines,
                crate::i18n::tr("Press [Enter] or [Esc] to return to chat"))?;
            if let Event::Key(key) = crate::cli_ui::read_event_with_background()? {
                if key.kind == KeyEventKind::Press && (matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                    || key.code == KeyCode::Char('c') && key.modifiers.contains(event::KeyModifiers::CONTROL)) {
                    return Ok(());
                }
            }
        }
    })();
    if !was_raw { disable_raw_mode()?; }
    result
}

pub fn select_effort_interactive(ctx: &ScreenContext, current_effort: &str, levels: &[String]) -> std::io::Result<Option<String>> {
    if levels.is_empty() { return Ok(None); }
    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw { enable_raw_mode()?; }
    let result = (|| {
        let mut efforts = vec!["auto".to_string()];
        efforts.extend_from_slice(levels);
        let mut selected = efforts.iter().position(|id| id == current_effort).unwrap_or(0);
        let mut panel = BottomSelectionPanel::default();
        loop {
            let (_, rows) = crate::cli_ui::terminal_size();
            let visible = rows.saturating_sub(13).clamp(1, efforts.len() as u16) as usize;
            let offset = selected.saturating_sub(visible.saturating_sub(1));
            let th = theme::current();
            let lines = efforts.iter().enumerate().skip(offset).take(visible).map(|(index, value)| {
                let label = if value == "auto" { crate::i18n::tr("Automatic") } else { value.as_str() };
                (format!("{} {}", if index == selected { "❯" } else { " " }, label),
                    if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() })
            }).collect::<Vec<_>>();
            panel.draw(ctx, crate::i18n::tr("Select Reasoning Effort"), &lines, crate::i18n::tr("↑/↓ · Enter: Select · Esc: Back"))?;
            match crate::cli_ui::read_event_with_background()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => selected = (selected + efforts.len() - 1) % efforts.len(),
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % efforts.len(),
                    KeyCode::Enter => return Ok(Some(efforts[selected].clone())),
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(None),
                    _ => {},
                },
                _ => {},
            }
        }
    })();
    if !was_raw { disable_raw_mode()?; }
    result
}

pub enum SessionAction {
    Resume(Session),
    Deleted(String),
}

/// Interactive session selector menu, rendered at the bottom of the screen instead of the input box.
pub fn select_session(ctx: &ScreenContext, workspace: &Path) -> std::io::Result<Option<SessionAction>> {
    enable_raw_mode()?;
    let res = select_session_inner(ctx, workspace);
    disable_raw_mode()?;
    res
}

fn select_session_inner(ctx: &ScreenContext, workspace: &Path) -> std::io::Result<Option<SessionAction>> {
    let mut sessions = Session::list(workspace);
    let mut selected_idx = 0;
    let mut needs_clear = true;

    loop {
        if sessions.is_empty() {
            return Ok(None);
        }
        if selected_idx >= sessions.len() {
            selected_idx = sessions.len() - 1;
        }

        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let inner_w = box_w.saturating_sub(4);
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = MenuFrame::default();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let two_lines = term_rows >= 28;
        let item_lines = if two_lines { 2 } else { 1 };
        let max_visible = if term_rows >= 34 {
            5usize
        } else if term_rows >= 24 {
            3usize
        } else {
            2usize
        };
        let visible_count = sessions.len().min(max_visible);
        let content_rows = visible_count * item_lines;
        let total_box_height = 1 + content_rows + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            crate::i18n::tr("Resume Saved Chat").to_string()
        } else {
            crate::i18n::tf!("Resume Saved Chat [{}]", ctx.branch_tag.trim())
        };

        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let scroll_offset = if selected_idx < max_visible {
            0
        } else {
            selected_idx + 1 - max_visible
        };
        let end_idx = (scroll_offset + max_visible).min(sessions.len());

        let mut cur_row = start_row + 1;
        for i in scroll_offset..end_idx {
            let id = &sessions[i];
            let is_sel = i == selected_idx;
            let loaded = Session::load(workspace, id);

            let prefix = if is_sel { "> " } else { "  " };
            let (created, msgs_count, model_name, first_msg) = match &loaded {
                Some(s) => {
                    (s.created_at.clone(), s.message_count(), s.model.clone(), s.title())
                }
                None => (String::new(), 0, String::new(), String::new()),
            };

            if two_lines {
                let id_styled = if is_sel {
                    crate::i18n::tf!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;210;210;215m{:2} msgs\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;160;160;165m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;120;120;125m{}\x1b[0m",
                        prefix, p_ansi, id, msgs_count, created, model_name)
                } else {
                    crate::i18n::tf!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;140;140;145m{:2} msgs\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;100;100;105m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;90;90;95m{}\x1b[0m",
                        prefix, id, msgs_count, created, model_name)
                };
                let id_vis = 2 + str_width(id) + 5 + 7 + 5 + str_width(&created) + 5 + str_width(&model_name);
                draw_bottom_box_line(&mut out, cur_row, &id_styled, id_vis, box_w, b_color)?;
                cur_row += 1;

                let preview_trunc = truncate_visible(&first_msg, inner_w.saturating_sub(8));
                let preview_line = format!("      \x1b[38;2;130;130;135m\"{}\"\x1b[0m", preview_trunc);
                let preview_vis = 6 + 1 + str_width(&preview_trunc) + 1;
                draw_bottom_box_line(&mut out, cur_row, &preview_line, preview_vis, box_w, b_color)?;
                cur_row += 1;
            } else {
                let preview_trunc = truncate_visible(&first_msg, inner_w.saturating_sub(42));
                let line_styled = if is_sel {
                    crate::i18n::tf!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;210;210;215m{:2} msgs\x1b[0m  \x1b[38;2;130;130;135m(\"{}\")\x1b[0m",
                        prefix, p_ansi, id, msgs_count, preview_trunc)
                } else {
                    crate::i18n::tf!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;140;140;145m{:2} msgs\x1b[0m  \x1b[38;2;100;100;105m(\"{}\")\x1b[0m",
                        prefix, id, msgs_count, preview_trunc)
                };
                let line_vis = 2 + str_width(id) + 5 + 7 + 4 + str_width(&preview_trunc);
                draw_bottom_box_line(&mut out, cur_row, &line_styled, line_vis, box_w, b_color)?;
                cur_row += 1;
            }
        }

        let hint = if sessions.len() > visible_count {
            crate::i18n::tf!("↑/↓: navigate ({}/{})  •  Enter: resume  •  d: delete  •  Esc: cancel", selected_idx + 1, sessions.len())
        } else {
            crate::i18n::tr("Enter: resume  •  d: delete  •  Esc: cancel").to_string()
        };
        draw_bottom_box_bottom(&mut out, cur_row, &hint, box_w, b_color)?;
        out.flush()?;

        match crate::cli_ui::read_event_with_background()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        } else {
                            selected_idx = sessions.len() - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < sessions.len() {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Enter => {
                        let id = &sessions[selected_idx];
                        if let Some(loaded) = Session::load(workspace, id) {
                            return Ok(Some(SessionAction::Resume(loaded)));
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Delete => {
                        let id = sessions.remove(selected_idx);
                        let _ = Session::delete(workspace, &id);
                        needs_clear = true;
                        if sessions.is_empty() {
                            return Ok(Some(SessionAction::Deleted(id)));
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        return Ok(None);
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                needs_clear = true;
            }
            _ => {}
        }
    }
}

/// Scrollable workspace diff in the shared bottom panel.
pub fn show_interactive_diff(ctx: &ScreenContext, diff: &str, workspace: &Path) -> std::io::Result<()> {
    enable_raw_mode()?;
    let lines: Vec<&str> = diff.lines().collect();
    let mut scroll_offset = 0usize;
    let mut panel = BottomSelectionPanel::default();

    loop {
        let (_, term_rows) = crate::cli_ui::terminal_size();
        let view_h = (term_rows as usize / 3).clamp(3, 12)
            .min(term_rows.saturating_sub(4).max(1) as usize);
        scroll_offset = scroll_offset.min(lines.len().saturating_sub(view_h));
        let th = theme::current();
        let end_idx = (scroll_offset + view_h).min(lines.len());
        let mut content = vec![(crate::i18n::tf!("Lines {}–{} of {}", if lines.is_empty() { 0 } else { scroll_offset + 1 }, end_idx, lines.len()), Color::DarkGrey)];
        for i in scroll_offset..end_idx {
            let line = lines[i];
            let color = if line.starts_with('+') && !line.starts_with("+++") {
                Color::Rgb { r: 80, g: 230, b: 120 }
            } else if line.starts_with('-') && !line.starts_with("---") {
                Color::Rgb { r: 255, g: 90, b: 90 }
            } else if line.starts_with("@@") {
                th.primary_crossterm()
            } else if line.starts_with("diff --git") {
                Color::White
            } else {
                Color::Grey
            };
            content.push((line.to_string(), color));
        }
        for _ in (end_idx - scroll_offset)..view_h {
            content.push((String::new(), Color::Grey));
        }
        panel.draw(ctx, crate::i18n::tr("Git Workspace Diff"), &content, crate::i18n::tr("↑/↓ PgUp/PgDn: scroll • c: commit • r: revert • Esc: back"))?;

        match crate::cli_ui::read_event_with_background()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        scroll_offset = scroll_offset.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if scroll_offset + view_h < lines.len() {
                            scroll_offset += 1;
                        }
                    }
                    KeyCode::PageUp => {
                        scroll_offset = scroll_offset.saturating_sub(view_h);
                    }
                    KeyCode::PageDown => {
                        if scroll_offset + view_h < lines.len() {
                            scroll_offset = (scroll_offset + view_h).min(lines.len().saturating_sub(view_h));
                        }
                    }
                    KeyCode::Char('c') => {
                        // Commit action
                        if let Some(msg) = prompt_string_input(ctx, crate::i18n::tr("Git Commit"), crate::i18n::tr("Commit message:"), "")? {
                            if !msg.trim().is_empty() {
                                let _ = crate::git::create_git_command()
                                    .args(["add", "-A"])
                                    .current_dir(workspace)
                                    .output();
                                let res = crate::git::create_git_command()
                                    .args(["commit", "-m", &msg])
                                    .current_dir(workspace)
                                    .output();
                                disable_raw_mode()?;
                                if let Ok(out) = res {
                                    println!("{}", crate::i18n::tf!("\nGit commit result:\n{}", String::from_utf8_lossy(&out.stdout)));
                                }
                                return Ok(());
                            }
                        }
                    }
                    KeyCode::Char('r') => {
                        // Revert action
                        if let Some(confirm) = prompt_string_input(ctx, crate::i18n::tr("Revert Workspace Changes"), crate::i18n::tr("Revert all unstaged files? (yes/no):"), "no")? {
                            if confirm.eq_ignore_ascii_case("yes") || confirm.eq_ignore_ascii_case("y") {
                                let _ = crate::git::create_git_command()
                                    .args(["checkout", "--", "."])
                                    .current_dir(workspace)
                                    .output();
                                disable_raw_mode()?;
                                println!("{}", crate::i18n::tf!("\nReverted all modifications back to git HEAD.\n"));
                                return Ok(());
                            }
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                        disable_raw_mode()?;
                        return Ok(());
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

pub enum StatusAction {
    ChangeProvider,
    ChangeModel,
    ChangeMode,
    ShowUsage,
    ChangeTheme,
    Reset,
}

/// Interactive Status Dashboard with quick shortcuts rendered in bottom box replacing the input field.
pub fn show_interactive_status(
    ctx: &ScreenContext,
    config: &Config,
    session: &Session,
    msg_count: usize,
    branch: &str,
) -> std::io::Result<Option<StatusAction>> {
    enable_raw_mode()?;
    let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let box_w = crate::cli_ui::get_box_width();
    let inner_w = box_w.saturating_sub(4);
    let th = theme::current();
    let b_color = th.border_crossterm();
    let p_color = th.primary_crossterm();

    let mut out = MenuFrame::default();
    queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

    let masked_key = if config.api_key.is_empty() {crate::i18n::tr("(not set)")} else {crate::i18n::tr("(configured)")};

    let approval_str = config.execution_mode.current().name();

    let items = [
        (crate::i18n::tr("AI Model"), config.model.as_str()),
        (crate::i18n::tr("Mode"), crate::i18n::tr("Third-party provider")),
        (crate::i18n::tr("Endpoint"), config.base_url.as_str()),
        (crate::i18n::tr("API type"), config.api_type.label()),
        (crate::i18n::tr("API Key"), masked_key),
        (crate::i18n::tr("Approval"), approval_str),
        (crate::i18n::tr("Visual Theme"), th.name()),
        (crate::i18n::tr("Workspace"), config.workspace_dir.to_str().unwrap_or(".")),
        (crate::i18n::tr("Git Branch"), if branch.is_empty() { crate::i18n::tr("(no git)") } else { branch }),
        (crate::i18n::tr("Session ID"), session.id.as_str()),
        (crate::i18n::tr("Messages"), &msg_count.to_string()),
    ];

    let total_box_height = 1 + items.len() + 1 + 1 + 1; // 13 lines
    let start_row = term_rows.saturating_sub(total_box_height as u16);

    let title = if branch.is_empty() {
        crate::i18n::tr("Takiza Code Session & System Status").to_string()
    } else {
        crate::i18n::tf!("Takiza Code Session & System Status [{}]", branch.trim())
    };

    draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

    let mut cur_row = start_row + 1;
    for (k, v) in items {
        let max_v_w = inner_w.saturating_sub(16);
        let v_trunc = truncate_visible(v, max_v_w);
        let label = truncate_visible(k, 14);
        let padding = " ".repeat(14usize.saturating_sub(str_width(&label)));
        let line = format!("\x1b[1;38;2;0;220;255m{label}{padding}\x1b[0m \x1b[38;2;220;220;225m{v_trunc}\x1b[0m");
        let vis_len = 14 + 1 + str_width(&v_trunc);
        draw_bottom_box_line(&mut out, cur_row, &line, vis_len, box_w, b_color)?;
        cur_row += 1;
    }

    draw_bottom_box_divider(&mut out, cur_row, box_w, b_color)?;
    cur_row += 1;

    let actions_hint = crate::i18n::tr("Actions: [p] Provider  •  [m] Model  •  [o] Mode  •  [u] Usage  •  [t] Theme  •  [r] Reset  •  [Esc] Back");
    let actions_trunc = truncate_visible(actions_hint, inner_w);
    let actions_vis = str_width(&actions_trunc);
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[1;38;2;255;255;255m{}\x1b[0m", actions_trunc), actions_vis, box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_bottom(&mut out, cur_row, crate::i18n::tr("Press shortcut key or Esc to exit"), box_w, b_color)?;
    out.flush()?;

    loop {
        if let Event::Key(key) = crate::cli_ui::read_event_with_background()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            disable_raw_mode()?;
            match key.code {
                KeyCode::Char('p') | KeyCode::Char('P') => return Ok(Some(StatusAction::ChangeProvider)),
                KeyCode::Char('m') | KeyCode::Char('M') => return Ok(Some(StatusAction::ChangeModel)),
                KeyCode::Char('o') | KeyCode::Char('O') => return Ok(Some(StatusAction::ChangeMode)),
                KeyCode::Char('u') | KeyCode::Char('U') => return Ok(Some(StatusAction::ShowUsage)),
                KeyCode::Char('t') | KeyCode::Char('T') => return Ok(Some(StatusAction::ChangeTheme)),
                KeyCode::Char('r') | KeyCode::Char('R') => return Ok(Some(StatusAction::Reset)),
                _ => return Ok(None),
            }
        }
    }
}

/// Shared bottom panel matching the saved-chat selector.
#[derive(Default)]
struct BottomSelectionPanel {
    geometry: Option<(u16, u16)>,
    top: u16,
}
impl BottomSelectionPanel {
    fn visible_items(rows: u16) -> usize {
        if rows >= 34 { 5 } else if rows >= 24 { 3 } else { 2 }
    }
    fn draw(&mut self, ctx: &ScreenContext, title: &str, lines: &[(String, Color)], hint: &str) -> std::io::Result<()> {
        let (cols, rows) = crate::cli_ui::terminal_size();
        let box_w = crate::cli_ui::get_box_width();
        let inner_w = box_w.saturating_sub(4);
        let th = theme::current();
        let top = rows.saturating_sub(lines.len() as u16 + 2);
        let mut frame = Vec::new();
        queue!(frame, crossterm::terminal::BeginSynchronizedUpdate, cursor::Hide)?;
        if self.geometry != Some((cols, rows)) {
            queue!(frame, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut frame, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);
        let clear_top = if self.geometry == Some((cols, rows)) { self.top.min(top) } else { top };
        for row in clear_top..rows { queue!(frame, cursor::MoveTo(0, row), Clear(ClearType::UntilNewLine))?; }
        let title = if ctx.branch_tag.is_empty() { title.to_string() } else { format!("{title} [{}]", ctx.branch_tag) };
        draw_bottom_box_top(&mut frame, top, &title, box_w, th.primary_crossterm(), th.border_crossterm())?;
        for (index, (line, color)) in lines.iter().enumerate() {
            let clean = line.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect::<String>();
            let clean = truncate_visible(&clean, inner_w);
            let mut styled = Vec::new();
            queue!(styled, SetForegroundColor(*color), Print(&clean), ResetColor)?;
            draw_bottom_box_line(&mut frame, top + index as u16 + 1, &String::from_utf8_lossy(&styled), str_width(&clean), box_w, th.border_crossterm())?;
        }
        let hint = truncate_visible(hint, box_w.saturating_sub(5));
        draw_bottom_box_bottom(&mut frame, rows.saturating_sub(1), &hint, box_w, th.border_crossterm())?;
        queue!(frame, crossterm::terminal::EndSynchronizedUpdate)?;
        let mut out = stdout().lock(); out.write_all(&frame)?; out.flush()?;
        self.geometry = Some((cols, rows)); self.top = top;
        Ok(())
    }
}

/// Browse discovered skills without changing the active agent/session.
pub fn show_interactive_skills(ctx: &ScreenContext, workspace: &Path) -> std::io::Result<Option<crate::skills::Skill>> {
    enable_raw_mode()?;
    let result = (|| {
        let mut skills = crate::skills::discover_skills(workspace);
        let mut query = String::new();
        let mut selected = 0usize;
        let mut pending_event = None;
        let mut panel = BottomSelectionPanel::default();
        loop {
            let _geometry = crate::cli_ui::begin_terminal_frame();
            let (_, rows) = crate::cli_ui::terminal_size();
            let theme = theme::current();
            let matching = skills.iter().filter(|skill| {
                let needle = query.to_lowercase();
                skill.name.to_lowercase().contains(&needle)
                    || skill.description.to_lowercase().contains(&needle)
                    || skill.path.to_lowercase().contains(&needle)
            }).collect::<Vec<_>>();
            selected = selected.min(matching.len().saturating_sub(1));
            if crate::cli_ui::terminal_is_small() {
                crate::cli_ui::render_small_terminal();
            } else {
                let local = skills.iter().filter(|skill| skill.is_workspace).count();
                let slots = BottomSelectionPanel::visible_items(rows);
                let start = selected.saturating_sub(slots - 1);
                let mut lines = vec![(crate::i18n::tf!("Search: {}", query), theme.primary_crossterm())];
                for slot in 0..slots {
                    let index = start + slot;
                    if let Some(skill) = matching.get(index) {
                        lines.push((format!("{} {:<6} {}", if index == selected { ">" } else { " " },
                            if skill.is_workspace { crate::i18n::tr("local") } else { crate::i18n::tr("global") }, skill.name),
                            if index == selected { theme.primary_crossterm() } else { theme.secondary_crossterm() }));
                    } else {
                        lines.push((if slot == 0 { crate::i18n::tr("No skills found.").into() } else { String::new() }, Color::DarkGrey));
                    }
                }
                let current = matching.get(selected);
                lines.push((current.map(|skill| skill.description.clone()).unwrap_or_else(||
                    crate::i18n::tr("Place SKILL.md inside a skill folder under .takiza/skills or .agents/skills.").into()), Color::DarkGrey));
                lines.push((current.map(|skill| skill.path.clone()).unwrap_or_default(), theme.secondary_crossterm()));
                let title = crate::i18n::tf!("Skills · {} local · {} global", local, skills.len() - local);
                let hint = crate::i18n::tf!("{}/{} · ↑↓ · Enter Insert · F5 Rescan · Esc Back", if matching.is_empty() { 0 } else { selected + 1 }, matching.len());
                panel.draw(ctx, &title, &lines, &hint)?;
            }
            match pending_event.take().map(Ok).unwrap_or_else(crate::cli_ui::read_event_with_background)? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => {
                        if let Some(skill) = matching.get(selected) { return Ok(Some((*skill).clone())); }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = selected.saturating_add(1),
                    KeyCode::PageUp => selected = selected.saturating_sub(BottomSelectionPanel::visible_items(rows)),
                    KeyCode::PageDown => selected = selected.saturating_add(BottomSelectionPanel::visible_items(rows)),
                    KeyCode::F(5) => skills = crate::skills::discover_skills(workspace),
                    KeyCode::Backspace => { query.pop(); selected = 0; }
                    KeyCode::Char(ch) if !key.modifiers.intersects(event::KeyModifiers::CONTROL | event::KeyModifiers::ALT) => {
                        query.push(ch); selected = 0;
                    }
                    _ => {}
                },
                Event::Mouse(mouse) => {
                    if let Some(delta) = crate::cli_ui::history_scroll_mouse(mouse) {
                        let delta = crate::cli_ui::coalesce_scroll(delta, &mut pending_event);
                        selected = if delta > 0 { selected.saturating_sub(delta as usize) }
                            else { selected.saturating_add(delta.unsigned_abs() as usize) };
                    }
                }
                _ => {}
            }
        }
    })();
    let _ = disable_raw_mode();
    result
}

/// Interactive Tools Explorer view rendered in bottom box replacing the input field.
pub fn show_interactive_tools(ctx: &ScreenContext, definitions: &serde_json::Value) -> std::io::Result<()> {
    let details = definitions.as_array().into_iter().flatten().map(|tool| {
        let function = &tool["function"];
        format!("{} · {}", function["name"].as_str().unwrap_or("?"), function["description"].as_str().unwrap_or(""))
    }).collect::<Vec<_>>();
    rewind_menu(ctx, crate::i18n::tr("Autonomous Agent Tools & Capabilities"), &[], &details)?;
    Ok(())
}

/// Provider billing and quotas are managed by the provider, not Takiza.
pub fn show_interactive_usage(ctx: &ScreenContext, _config: &Config) -> std::io::Result<()> {
    show_provider_notice(ctx, &[
        crate::i18n::tr("A third-party provider is active."),
        crate::i18n::tr("Check your provider dashboard for current usage, balance and limits."),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_connection_inputs_keep_the_caret_in_view_and_keys_masked() {
        for width in [5,10,30] {
            let (shown,caret)=connection_input_window("连接Очень-длинный-ключ",20,false,width);
            assert!(str_width(&shown)<=width);assert!(caret<width);
            let (masked,caret)=connection_input_window("secret-key-that-must-not-appear",30,true,width);
            assert!(masked.chars().all(|ch|ch=='*'));assert!(caret<width);
        }
    }
    #[test]
    fn model_names_and_provider_ids_fit_unicode_columns() {
        let model=crate::providers::Model { id:"vendor/deployment-long-name".into(), name:"模型 · Модель".into(), context_window:None, metadata:Default::default() };
        for width in [20,40,60,100] {
            let row=model_columns(&model,width);
            assert!(str_width(&row)<=width, "{row}");
            if width>=60 {assert!(row.starts_with("模型 · Модель")); assert!(row.ends_with(&model.id));}
        }
    }
}

/// Checkpoint selection and confirmation use the same keyboard controls as command menus.
pub fn rewind_menu(ctx: &ScreenContext, title: &str, details: &[String], choices: &[String]) -> std::io::Result<Option<usize>> {
    selection_menu(ctx, title, details, choices, false, false)
}

pub fn mcp_permission_menu(ctx: &ScreenContext, details: &[String], choices: &[String]) -> std::io::Result<Option<usize>> {
    selection_menu(ctx, crate::i18n::tr("MCP tool permission"), details, choices, true, false)
}

pub fn file_permission_menu(ctx: &ScreenContext, details: &[String], choices: &[String]) -> std::io::Result<Option<usize>> {
    selection_menu(ctx, crate::i18n::tr("File write permission"), details, choices, true, true)
}

fn selection_menu(ctx: &ScreenContext, title: &str, details: &[String], choices: &[String], tool_permission: bool, file_permission: bool) -> std::io::Result<Option<usize>> {
    enable_raw_mode()?;
    let result = (|| {
        let mut selected = 0usize;
        let mut scroll = 0usize;
        let mut panel = BottomSelectionPanel::default();
        loop {
            let _geometry = crate::cli_ui::begin_terminal_frame();
            let (_, rows) = crate::cli_ui::terminal_size();
            let th = theme::current();
            let slots = BottomSelectionPanel::visible_items(rows);
            let start = if details.is_empty() { selected.saturating_sub(slots - 1) } else { scroll };
            let mut lines = Vec::new();
            if choices.is_empty() {
                lines.push((crate::i18n::tr("No checkpoints in this chat.").into(), th.secondary_crossterm()));
                lines.push((crate::i18n::tr("Checkpoints are saved before prompts.").into(), Color::DarkGrey));
            } else if details.is_empty() {
                for (index, choice) in choices.iter().enumerate().skip(start).take(slots) {
                    lines.push((format!("{} {choice}", if index == selected { ">" } else { " " }),
                        if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() }));
                }
            } else {
                lines.push((if file_permission { crate::i18n::tf!("Changes · {}–{} / {}", start + 1, (start + slots).min(details.len()), details.len()) }
                    else if tool_permission { crate::i18n::tf!("Arguments · {}–{} / {}", start + 1, (start + slots).min(details.len()), details.len()) }
                    else { crate::i18n::tf!("{} changes · files {}–{}", details.len(), start + 1, (start + slots).min(details.len())) }, Color::DarkGrey));
                for detail in details.iter().skip(start).take(slots) { lines.push((detail.clone(), th.secondary_crossterm())); }
                lines.push((String::new(), Color::DarkGrey));
                for (index, choice) in choices.iter().enumerate() {
                    lines.push((format!("{} {choice}", if index == selected { ">" } else { " " }),
                        if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() }));
                }
            }
            if crate::cli_ui::terminal_is_small() { crate::cli_ui::render_small_terminal(); }
            else {
                let hint = if choices.is_empty() { crate::i18n::tr("Esc Back").to_string() }
                    else if details.is_empty() { crate::i18n::tf!("{}/{} · ↑↓ · Enter Review · Esc Back", selected + 1, choices.len()) }
                    else if file_permission { crate::i18n::tr("↑↓ Select · Enter · PgUp/PgDn Changes · Esc Deny").to_string() }
                    else if tool_permission { crate::i18n::tr("↑↓ Select · Enter · PgUp/PgDn Arguments · Esc Deny").to_string() }
                    else { crate::i18n::tr("↑↓ · Enter Confirm · PgUp/PgDn Files · Esc Back").to_string() };
                panel.draw(ctx, title, &lines, &hint)?;
            }
            match crate::cli_ui::read_event_with_background()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Enter if !choices.is_empty() => return Ok(Some(selected)),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(choices.len().saturating_sub(1)),
                    KeyCode::PageUp if details.is_empty() => selected = selected.saturating_sub(slots),
                    KeyCode::PageDown if details.is_empty() => selected = (selected + slots).min(choices.len().saturating_sub(1)),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(slots),
                    KeyCode::PageDown => scroll = (scroll + slots).min(details.len().saturating_sub(slots)),
                    _ => {}
                },
                _ => {}
            }
        }
    })();
    let _ = disable_raw_mode();
    result
}

pub fn workspace_trust_menu(ctx: &ScreenContext, workspace: &Path) -> std::io::Result<bool> {
    if !stdout().is_terminal() {
        return Ok(false);
    }
    enable_raw_mode()?;
    let result = (|| {
        let mut selected = 0usize;
        let mut panel = BottomSelectionPanel::default();
        let choices = [
            crate::i18n::tr("Allow access to this directory"),
            crate::i18n::tr("Deny and exit"),
        ];
        let title = crate::i18n::tr("Directory access permission");
        let question = crate::i18n::tr("Allow Takiza Code access to files in this directory?");
        let dir_display = workspace.display().to_string();
        let hint = crate::i18n::tr("↑↓: Select  Enter: Confirm  Esc: Deny");

        loop {
            let _geometry = crate::cli_ui::begin_terminal_frame();
            let th = theme::current();
            let mut lines = Vec::new();
            lines.push((question.to_string(), Color::White));
            lines.push((dir_display.clone(), th.primary_crossterm()));
            lines.push((String::new(), Color::DarkGrey));
            for (index, choice) in choices.iter().enumerate() {
                lines.push((
                    format!("{} {choice}", if index == selected { ">" } else { " " }),
                    if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() },
                ));
            }

            if crate::cli_ui::terminal_is_small() {
                crate::cli_ui::render_small_terminal();
            } else {
                panel.draw(ctx, title, &lines, hint)?;
            }

            match crate::cli_ui::read_event_with_background()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => return Ok(false),
                    KeyCode::Enter => return Ok(selected == 0),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(choices.len().saturating_sub(1)),
                    _ => {}
                },
                _ => {}
            }
        }
    })();
    let _ = disable_raw_mode();
    result
}
