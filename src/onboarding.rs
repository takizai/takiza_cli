use crate::theme::{Theme, UserPreferences};
use chrono::Local;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{stdout, Write};

pub struct Onboarding;

/// Computes visible width of a string in terminal columns, ignoring ANSI escape codes.
pub fn visible_width(s: &str) -> usize {
    let mut w = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        w += crate::prompt::char_width(c);
    }
    w
}

/// Truncates string to a max visible width, adding "…" if needed.
fn truncate_visible(s: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }
    let mut cur_w = 0;
    let mut res = String::new();
    for c in s.chars() {
        let cw = crate::prompt::char_width(c);
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

/// Word-wraps text into lines not exceeding `max_w` visible columns.
fn wrap_words(text: &str, max_w: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let max_w = max_w.max(10);
    let words = text.split_whitespace();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for word in words {
        let w_len = visible_width(word);
        if current.is_empty() {
            current.push_str(word);
            current_len = w_len;
        } else if current_len + 1 + w_len <= max_w {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + w_len;
        } else {
            lines.push(current);
            current = word.to_string();
            current_len = w_len;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn draw_box_top(
    out: &mut impl Write,
    row: &mut u16,
    prefix: &str,
    p_ansi: &str,
    title: &str,
    box_w: usize,
) -> std::io::Result<()> {
    let title_vis = visible_width(title);
    let dashes = box_w.saturating_sub(title_vis + 6);
    execute!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print(format!(
            "{}{}\x1b[1m╭─ \x1b[0m{}\x1b[0m {}{}\x1b[1m─╮\x1b[0m",
            prefix,
            p_ansi,
            title,
            p_ansi,
            "─".repeat(dashes)
        ))
    )?;
    *row += 1;
    Ok(())
}

fn draw_box_divider(
    out: &mut impl Write,
    row: &mut u16,
    prefix: &str,
    p_ansi: &str,
    box_w: usize,
) -> std::io::Result<()> {
    let dashes = box_w.saturating_sub(2);
    execute!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print(format!(
            "{}{}\x1b[1m├{}┤\x1b[0m",
            prefix,
            p_ansi,
            "─".repeat(dashes)
        ))
    )?;
    *row += 1;
    Ok(())
}

fn draw_box_divider_with_label(
    out: &mut impl Write,
    row: &mut u16,
    prefix: &str,
    p_ansi: &str,
    label: &str,
    box_w: usize,
) -> std::io::Result<()> {
    let label_vis = visible_width(label);
    let dashes = box_w.saturating_sub(label_vis + 6);
    execute!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print(format!(
            "{}{}\x1b[1m├─ \x1b[0m{}\x1b[0m {}{}\x1b[1m─┤\x1b[0m",
            prefix,
            p_ansi,
            label,
            p_ansi,
            "─".repeat(dashes)
        ))
    )?;
    *row += 1;
    Ok(())
}

fn draw_box_bottom(
    out: &mut impl Write,
    row: &mut u16,
    prefix: &str,
    p_ansi: &str,
    hint: &str,
    box_w: usize,
) -> std::io::Result<()> {
    let hint_vis = visible_width(hint);
    let dashes = box_w.saturating_sub(hint_vis + 6);
    execute!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print(format!(
            "{}{}\x1b[1m╰─ \x1b[0m{}\x1b[0m {}{}\x1b[1m─╯\x1b[0m",
            prefix,
            p_ansi,
            hint,
            p_ansi,
            "─".repeat(dashes)
        ))
    )?;
    *row += 1;
    Ok(())
}

fn draw_box_line(
    out: &mut impl Write,
    row: &mut u16,
    prefix: &str,
    p_ansi: &str,
    content: &str,
    inner_w: usize,
) -> std::io::Result<()> {
    let vis_w = visible_width(content);
    let pad = inner_w.saturating_sub(vis_w);
    execute!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print(format!(
            "{}{}│\x1b[0m {}{}{} {}{}│\x1b[0m",
            prefix,
            p_ansi,
            content,
            " ".repeat(pad),
            "",
            p_ansi,
            ""
        ))
    )?;
    *row += 1;
    Ok(())
}

fn render_banner(
    out: &mut impl Write,
    row: &mut u16,
    term_cols: u16,
    current_theme: Theme,
    step_num: usize,
    step_title: &str,
    step_info: &[(&str, &str)],
) -> std::io::Result<()> {
    let p_ansi = current_theme.primary_ansi();

    if term_cols >= 80 {
        let box_w = (term_cols as usize).saturating_sub(4).min(84);
        let info_w = box_w.saturating_sub(32).min(50);
        let div_w = info_w.min(48);
        let divider = "─".repeat(div_w);

        let mut info_lines: Vec<String> = vec![
            format!("{}TAKIZA \x1b[1;38;2;245;245;250mCODE\x1b[0m  \x1b[38;2;120;120;125mv{}\x1b[0m", p_ansi, env!("TAKIZA_VERSION")),
            format!("\x1b[38;2;160;160;165m{}\x1b[0m", truncate_visible("Autonomous AI Software Engineering Agent", info_w)),
            format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider),
            format!("{}Step {} of 2\x1b[0m   \x1b[38;2;70;70;75m│\x1b[0m \x1b[1;38;2;230;230;235m{}\x1b[0m", p_ansi, step_num, step_title),
        ];

        for (label, val) in step_info {
            let label_pad = 10usize.saturating_sub(label.len());
            info_lines.push(format!(
                "{}{}{}\x1b[38;2;70;70;75m│\x1b[0m \x1b[38;2;170;170;175m{}\x1b[0m",
                p_ansi,
                label,
                " ".repeat(label_pad),
                truncate_visible(val, info_w.saturating_sub(13))
            ));
        }

        info_lines.push(format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider));

        for i in 0..crate::logo::LOGO_HEIGHT {
            let logo_part = crate::logo::LOGO_LINES[i];
            let info_part = if i < info_lines.len() { &info_lines[i] } else { "" };
            execute!(
                out,
                cursor::MoveTo(0, *row),
                Clear(ClearType::UntilNewLine),
                Print(format!(" {}  {}", logo_part, info_part))
            )?;
            *row += 1;
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
            format!("  {}╭─ TAKIZA \x1b[38;2;245;245;250mCODE\x1b[0m \x1b[38;2;120;120;125mv{}{} {}╮\x1b[0m", p_ansi, env!("TAKIZA_VERSION"), p_ansi, top_div),
            format!("  {}│\x1b[0m {}Step {}/2:   \x1b[0m\x1b[38;2;230;230;235m{}\x1b[0m", p_ansi, p_ansi, step_num, truncate_visible(step_title, val_w)),
            format!("  {}│\x1b[0m {}Action:    \x1b[0m\x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, p_ansi, truncate_visible(if step_num == 1 { "Select Theme" } else { "Review Terms" }, val_w)),
            format!("  {}╰{}╯\x1b[0m", p_ansi, bot_div),
        ];

        for l in lines {
            execute!(
                out,
                cursor::MoveTo(0, *row),
                Clear(ClearType::UntilNewLine),
                Print(l)
            )?;
            *row += 1;
        }
    }

    Ok(())
}

impl Onboarding {
    pub fn run_always() -> anyhow::Result<(UserPreferences, bool)> {
        let mut prefs = UserPreferences::load();

        let mut out = stdout();
        let _ = execute!(
            out,
            Clear(ClearType::All),
            Clear(ClearType::Purge),
            cursor::MoveTo(0, 0),
            cursor::Hide
        );
        let _ = out.flush();

        enable_raw_mode()?;
        let res = Self::interactive_wizard(&mut prefs);
        disable_raw_mode()?;

        let mut out = stdout();
        let _ = execute!(out, cursor::Show);
        let _ = out.flush();

        if let Ok(true) = res {
            prefs.save()?;
            Ok((prefs, true))
        } else {
            println!("You must accept the user agreement to use Takiza Harness. Goodbye!");
            std::process::exit(0);
        }
    }

    pub fn run_if_needed() -> anyhow::Result<(UserPreferences, bool)> {
        let prefs = UserPreferences::load();
        if prefs.agreed_to_terms {
            return Ok((prefs, false));
        }
        Self::run_always()
    }

    fn interactive_wizard(prefs: &mut UserPreferences) -> std::io::Result<bool> {
        // Step 1: Theme Selection
        let selected_theme = Self::select_theme(prefs.theme)?;
        prefs.theme = selected_theme;

        // Clear screen before Step 2
        let mut out = stdout();
        let _ = execute!(out, Clear(ClearType::All), Clear(ClearType::Purge), cursor::MoveTo(0, 0));
        let _ = out.flush();

        // Step 2: Terms of Service review & acceptance
        let accepted = Self::show_terms(prefs.theme)?;
        if accepted {
            prefs.agreed_to_terms = true;
            prefs.accepted_at = Some(Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn select_theme(initial: Theme) -> std::io::Result<Theme> {
        let themes = Theme::all();
        let mut selected_idx = themes.iter().position(|&t| t == initial).unwrap_or(0);

        loop {
            Self::render_theme_selector(themes, selected_idx)?;

            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
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
                            return Ok(themes[selected_idx]);
                        }
                        KeyCode::Char('q') | KeyCode::Esc => {
                            return Ok(themes[selected_idx]);
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    let mut out = stdout();
                    let _ = execute!(out, Clear(ClearType::All), Clear(ClearType::Purge));
                }
                _ => {}
            }
        }
    }

    fn render_theme_selector(themes: &[Theme], selected_idx: usize) -> std::io::Result<()> {
        let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let mut out = stdout();

        execute!(
            out,
            cursor::Hide,
            cursor::MoveTo(0, 0)
        )?;

        let current_theme = themes[selected_idx];
        let p_ansi = current_theme.primary_ansi();
        let s_ansi = current_theme.secondary_ansi();

        let prefix = "  "; // left-aligned with 2 spaces margin
        let box_w = (term_cols as usize).saturating_sub(4).min(84);
        let inner_w = box_w.saturating_sub(4);

        let mut row: u16 = 0;
        let _ = execute!(out, cursor::MoveTo(0, row), Clear(ClearType::UntilNewLine));
        row += 1;

        // 1. Banner (logo on left, step info on right)
        let step_info = [
            ("Controls", "Use ↑/↓ or j/k to preview"),
            ("Action", "Press [Enter] to select"),
            ("Selected", current_theme.name()),
        ];
        render_banner(&mut out, &mut row, term_cols, current_theme, 1, "Select Visual Theme", &step_info)?;
        row += 1;

        // 2. Card Header
        draw_box_top(
            &mut out,
            &mut row,
            prefix,
            p_ansi,
            &format!("{}\x1b[1mSTEP 1 OF 2: AVAILABLE THEMES\x1b[0m", p_ansi),
            box_w,
        )?;
        let instructions = "Use [↑/↓] or [j/k] to preview theme, [Enter] to confirm";
        draw_box_line(&mut out, &mut row, prefix, p_ansi, &format!("\x1b[38;2;160;160;165m{}\x1b[0m", instructions), inner_w)?;
        draw_box_divider(&mut out, &mut row, prefix, p_ansi, box_w)?;

        // 3. Theme List
        let two_line_themes = term_rows >= 27;
        for (i, t) in themes.iter().enumerate() {
            let is_sel = i == selected_idx;
            let marker = if is_sel { "▶ " } else { "  " };
            let radio = if is_sel {
                format!("{}◉ \x1b[1m{}\x1b[0m", t.primary_ansi(), t.name())
            } else {
                format!("\x1b[38;2;100;100;105m○ \x1b[38;2;220;220;225m{}\x1b[0m", t.name())
            };
            let badge = if is_sel {
                format!("{}✔ SELECTED\x1b[0m", t.primary_ansi())
            } else {
                String::new()
            };

            if two_line_themes {
                let left_vis = 2 + 2 + t.name().len();
                let badge_vis = if is_sel { 10 } else { 0 };
                let space_w = inner_w.saturating_sub(left_vis + badge_vis);
                let line1 = format!("{}{}{}{}", marker, radio, " ".repeat(space_w), badge);
                draw_box_line(&mut out, &mut row, prefix, p_ansi, &line1, inner_w)?;

                let desc = format!("    \x1b[38;2;140;140;145m{}\x1b[0m", t.description());
                draw_box_line(&mut out, &mut row, prefix, p_ansi, &desc, inner_w)?;
            } else {
                let left_vis = 2 + 2 + t.name().len();
                let badge_vis = if is_sel { 10 } else { 0 };
                let desc_avail = inner_w.saturating_sub(left_vis + badge_vis + 3);
                let desc_short = truncate_visible(t.description(), desc_avail);
                let desc_vis = visible_width(&desc_short);
                let space_w = inner_w.saturating_sub(left_vis + 3 + desc_vis + badge_vis);
                let line = format!(
                    "{}{}\x1b[38;2;100;100;105m — \x1b[38;2;140;140;145m{}\x1b[0m{}{}",
                    marker, radio, desc_short, " ".repeat(space_w), badge
                );
                draw_box_line(&mut out, &mut row, prefix, p_ansi, &line, inner_w)?;
            }
        }

        // 4. Preview & Footer
        draw_box_divider(&mut out, &mut row, prefix, p_ansi, box_w)?;
        let preview_text = format!(
            "Preview: {}❯ TAKIZA AGENT READY\x1b[0m  {}• Tools: bash, git, edit, grep\x1b[0m",
            p_ansi, s_ansi
        );
        draw_box_line(&mut out, &mut row, prefix, p_ansi, &preview_text, inner_w)?;
        draw_box_bottom(&mut out, &mut row, prefix, p_ansi, "Press [ENTER] to Confirm Selection", box_w)?;

        execute!(out, Clear(ClearType::FromCursorDown))?;
        out.flush()?;
        Ok(())
    }

    fn show_terms(theme: Theme) -> std::io::Result<bool> {
        let mut scroll_offset: usize = 0;
        let mut user_choice = false;

        loop {
            let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let box_w = (term_cols as usize).saturating_sub(4).min(84);
            let inner_w = box_w.saturating_sub(4);

            let terms_lines = Self::get_wrapped_terms(inner_w, theme);

            let banner_rows = if term_cols >= 80 { 8 + 1 } else { 4 + 1 };
            let card_fixed = 6;
            let view_height = (term_rows as usize).saturating_sub(banner_rows + card_fixed + 2).max(4);
            let max_scroll = terms_lines.len().saturating_sub(view_height);
            if scroll_offset > max_scroll {
                scroll_offset = max_scroll;
            }

            Self::render_terms_screen(
                &terms_lines,
                scroll_offset,
                view_height,
                theme,
                user_choice,
                term_cols,
                term_rows,
            )?;

            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            scroll_offset = scroll_offset.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if scroll_offset < max_scroll {
                                scroll_offset += 1;
                            }
                        }
                        KeyCode::PageUp => {
                            scroll_offset = scroll_offset.saturating_sub(view_height);
                        }
                        KeyCode::PageDown => {
                            scroll_offset = (scroll_offset + view_height).min(max_scroll);
                        }
                        KeyCode::Home => {
                            scroll_offset = 0;
                        }
                        KeyCode::End => {
                            scroll_offset = max_scroll;
                        }
                        KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                            user_choice = !user_choice;
                        }
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            user_choice = true;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') => {
                            user_choice = false;
                        }
                        KeyCode::Enter => {
                            return Ok(user_choice);
                        }
                        KeyCode::Esc => {
                            return Ok(false);
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    let mut out = stdout();
                    let _ = execute!(out, Clear(ClearType::All), Clear(ClearType::Purge));
                }
                _ => {}
            }
        }
    }

    fn render_terms_screen(
        lines: &[String],
        scroll_offset: usize,
        view_height: usize,
        theme: Theme,
        user_choice: bool,
        term_cols: u16,
        _term_rows: u16,
    ) -> std::io::Result<()> {
        let mut out = stdout();
        execute!(
            out,
            cursor::Hide,
            cursor::MoveTo(0, 0)
        )?;

        let p_ansi = theme.primary_ansi();

        let prefix = "  "; // left margin matching main screen
        let box_w = (term_cols as usize).saturating_sub(4).min(84);
        let inner_w = box_w.saturating_sub(4);

        let mut row: u16 = 0;
        let _ = execute!(out, cursor::MoveTo(0, row), Clear(ClearType::UntilNewLine));
        row += 1;

        // 1. Banner
        let step_info = [
            ("Scroll", "Use [↑/↓, PgUp/PgDn] to read"),
            ("Decision", "Use [Tab] or [←/→] to choose"),
            ("Action", "[Enter] accept  •  [Esc] exit"),
        ];
        render_banner(&mut out, &mut row, term_cols, theme, 2, "User Agreement & Policies", &step_info)?;
        row += 1;

        // 2. Card Header
        draw_box_top(
            &mut out,
            &mut row,
            prefix,
            p_ansi,
            &format!("{}\x1b[1mSTEP 2 OF 2: USER AGREEMENT & SAFETY POLICIES\x1b[0m", p_ansi),
            box_w,
        )?;
        let subtitle = "Scroll with [↑/↓, PgUp/PgDn]. Please review carefully:";
        draw_box_line(&mut out, &mut row, prefix, p_ansi, &format!("\x1b[38;2;160;160;165m{}\x1b[0m", subtitle), inner_w)?;
        draw_box_divider(&mut out, &mut row, prefix, p_ansi, box_w)?;

        // 3. Scrollable terms content
        let end_idx = (scroll_offset + view_height).min(lines.len());
        for i in scroll_offset..end_idx {
            draw_box_line(&mut out, &mut row, prefix, p_ansi, &lines[i], inner_w)?;
        }
        for _ in (end_idx - scroll_offset)..view_height {
            draw_box_line(&mut out, &mut row, prefix, p_ansi, "", inner_w)?;
        }

        // 4. Scroll position divider
        let pct = if lines.len() <= view_height {
            100
        } else {
            ((scroll_offset * 100) / (lines.len() - view_height)).min(100)
        };
        let scroll_label = format!("\x1b[1m[Scroll: {:>2}%]\x1b[0m", pct);
        draw_box_divider_with_label(&mut out, &mut row, prefix, p_ansi, &scroll_label, box_w)?;

        // 5. Decision buttons
        let accept_btn = if user_choice {
            format!("\x1b[1;30;48;2;0;255;128m [✔] I ACCEPT THE TERMS \x1b[0m")
        } else {
            format!("\x1b[38;2;160;160;165;48;2;45;45;50m [ ] I ACCEPT THE TERMS \x1b[0m")
        };
        let decline_btn = if !user_choice {
            format!("\x1b[1;30;48;2;255;75;75m [✕] DECLINE & EXIT \x1b[0m")
        } else {
            format!("\x1b[38;2;160;160;165;48;2;45;45;50m [ ] DECLINE & EXIT \x1b[0m")
        };

        let btn_total_vis = 25 + 4 + 21; // 50
        let btn_pad = inner_w.saturating_sub(btn_total_vis) / 2;
        let buttons_content = format!("{}{}{}{}", " ".repeat(btn_pad), accept_btn, "    ", decline_btn);
        draw_box_line(&mut out, &mut row, prefix, p_ansi, &buttons_content, inner_w)?;

        // 6. Card Bottom border
        let footer_hint = "[Tab]/[←/→] to toggle, [Enter] to confirm, [Esc] to exit";
        draw_box_bottom(&mut out, &mut row, prefix, p_ansi, footer_hint, box_w)?;

        execute!(out, Clear(ClearType::FromCursorDown))?;
        out.flush()?;
        Ok(())
    }

    fn get_wrapped_terms(inner_w: usize, theme: Theme) -> Vec<String> {
        let p_ansi = theme.primary_ansi();
        let raw_sections: Vec<(&str, bool)> = vec![
            ("TAKIZA HARNESS - END USER LICENSE & SAFETY AGREEMENT", true),
            ("Version 1.0.0 • Effective Date: September 2026", false),
            ("", false),
            ("1. AUTONOMOUS AI AGENT CAPABILITIES", true),
            ("Takiza Harness equips advanced AI models with direct tool access to your local operating system, file system, network, and shell. The agent possesses autonomous planning, code reading, editing, file creation, and command execution capabilities in your workspace.", false),
            ("", false),
            ("2. USER RESPONSIBILITY & OVERSIGHT", true),
            ("You acknowledge and agree that:", false),
            ("  • You retain full supervisory authority over all tool actions.", false),
            ("  • You are responsible for inspecting commands before execution.", false),
            ("  • You will keep critical files version-controlled with Git.", false),
            ("  • You can press Ctrl+C at any time to abort running operations.", false),
            ("", false),
            ("3. SAFE COMPUTING & WORKSPACE ISOLATION", true),
            ("  • Do not run Takiza Harness in root or privileged system directories.", false),
            ("  • File modifications and shell tasks are scoped to your workspace.", false),
            ("  • Do not provide credentials, private keys, or confidential data you are not authorized to share with third-party LLMs.", false),
            ("", false),
            ("4. THIRD-PARTY PROVIDERS & NETWORK USAGE", true),
            ("Takiza Harness interfaces with user-specified LLM API endpoints (such as Groq, OpenAI, OpenRouter, DeepSeek, or local Ollama). Your prompt context and tool output snippets are transmitted to the selected endpoint pursuant to that provider's privacy policy.", false),
            ("", false),
            ("5. NO WARRANTY & LIMITATION OF LIABILITY", true),
            ("THE SOFTWARE IS PROVIDED 'AS IS', WITHOUT WARRANTY OF ANY KIND. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY DAMAGES, DATA LOSS, OR SYSTEM DAMAGE ARISING FROM THE USE OF AI AGENT AUTOMATION TOOLS.", false),
            ("", false),
            ("By choosing 'I ACCEPT', you consent to all terms above.", true),
        ];

        let mut lines = Vec::new();
        for (text, is_header) in raw_sections {
            if text.is_empty() {
                lines.push(String::new());
                continue;
            }
            if is_header {
                let wrapped = wrap_words(text, inner_w);
                for w_line in wrapped {
                    lines.push(format!("{}\x1b[1m{}\x1b[0m", p_ansi, w_line));
                }
            } else if let Some(bullet_content) = text.strip_prefix("  • ") {
                let wrapped = wrap_words(bullet_content, inner_w.saturating_sub(4));
                for (idx, w_line) in wrapped.into_iter().enumerate() {
                    if idx == 0 {
                        lines.push(format!("  \x1b[38;2;160;160;165m•\x1b[0m \x1b[38;2;230;230;235m{}\x1b[0m", w_line));
                    } else {
                        lines.push(format!("    \x1b[38;2;230;230;235m{}\x1b[0m", w_line));
                    }
                }
            } else {
                let wrapped = wrap_words(text, inner_w);
                for w_line in wrapped {
                    lines.push(format!("\x1b[38;2;200;200;205m{}\x1b[0m", w_line));
                }
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visible_width() {
        assert_eq!(visible_width("Hello world"), 11);
        assert_eq!(visible_width("\x1b[1;38;2;255;195;0mAmber\x1b[0m"), 5);
        assert_eq!(visible_width("▶ ◉ Amber"), 9);
    }

    #[test]
    fn test_box_line_widths() {
        let box_w = 76;
        let inner_w = box_w - 4;
        let line = "Test content inside box";
        let vis = visible_width(line);
        let pad = inner_w - vis;
        // Total inside should be vis + pad = inner_w
        assert_eq!(vis + pad, inner_w);
    }

    #[test]
    fn test_box_alignment() {
        let check_elem = |name: &str, expected: usize, f: &dyn Fn(&mut Vec<u8>)| {
            let mut buf = Vec::new();
            f(&mut buf);
            let s = String::from_utf8_lossy(&buf);
            let w = visible_width(&s);
            assert_eq!(w, expected, "{}: width {} != expected {}", name, w, expected);
        };

        let prefix = "  "; // 2 spaces
        let p_ansi = "\x1b[38;2;255;195;0m";

        for box_w in [44, 50, 60, 76, 80, 82, 100, 120] {
            let inner_w = box_w - 4;
            let exp = prefix.len() + box_w;
            check_elem("draw_box_top", exp, &|b| { let mut r = 0; draw_box_top(b, &mut r, prefix, p_ansi, "TITLE", box_w).unwrap(); });
            check_elem("draw_box_line", exp, &|b| { let mut r = 0; draw_box_line(b, &mut r, prefix, p_ansi, "Content Line", inner_w).unwrap(); });
            check_elem("draw_box_divider", exp, &|b| { let mut r = 0; draw_box_divider(b, &mut r, prefix, p_ansi, box_w).unwrap(); });
            check_elem("draw_box_divider_with_label", exp, &|b| { let mut r = 0; draw_box_divider_with_label(b, &mut r, prefix, p_ansi, "[Label 50%]", box_w).unwrap(); });
            check_elem("draw_box_bottom", exp, &|b| { let mut r = 0; draw_box_bottom(b, &mut r, prefix, p_ansi, "HINT", box_w).unwrap(); });
        }
    }

    #[test]
    fn test_get_wrapped_terms_no_panic() {
        for w in [30, 40, 50, 60, 72, 80, 100] {
            let terms = Onboarding::get_wrapped_terms(w, Theme::Amber);
            assert!(!terms.is_empty());
            for line in &terms {
                let vis = visible_width(line);
                assert!(vis <= w, "Line '{}' visible width {} exceeds max {}", line, vis, w);
            }
        }
    }
}
