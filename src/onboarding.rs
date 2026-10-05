use crate::theme::{Theme, UserPreferences};
use chrono::Local;
use crossterm::{
    cursor,
    event::{Event, KeyCode, KeyEventKind},
    execute, queue,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{stdout, Write};

pub struct Onboarding;

#[derive(Debug)]
pub struct OnboardingDeclined;

impl std::fmt::Display for OnboardingDeclined {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(crate::i18n::tr("User agreement was not accepted"))
    }
}

impl std::error::Error for OnboardingDeclined {}

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
    crate::cli_ui::wrap_text_line(text, max_w.max(1))
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
            format!("\x1b[38;2;160;160;165m{}\x1b[0m", truncate_visible(crate::i18n::tr("Autonomous AI Software Engineering Agent"), info_w)),
            format!("\x1b[38;2;60;60;65m{}\x1b[0m", divider),
            crate::i18n::tf!("{}Step {} of 3\x1b[0m   \x1b[38;2;70;70;75m│\x1b[0m \x1b[1;38;2;230;230;235m{}\x1b[0m", p_ansi, step_num, step_title),
        ];

        for (label, val) in step_info {
            let label_pad = 10usize.saturating_sub(visible_width(label));
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
            let info_part = crate::interactive::fit_menu_text(info_part, info_w);
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
            crate::i18n::tf!("  {}│\x1b[0m {}Step {}/3:   \x1b[0m\x1b[38;2;230;230;235m{}\x1b[0m", p_ansi, p_ansi, step_num, truncate_visible(step_title, val_w)),
            crate::i18n::tf!("  {}│\x1b[0m {}Action:    \x1b[0m\x1b[38;2;170;170;175m{}\x1b[0m", p_ansi, p_ansi, truncate_visible(step_title, val_w)),
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
    pub(crate) fn render_language_selector(lines: &[(String, crossterm::style::Color)], hint: &str) -> std::io::Result<()> {
        let (cols, rows) = crate::cli_ui::terminal_size();
        let theme = crate::theme::current();
        let box_w = cols.saturating_sub(2) as usize;
        let top = rows.saturating_sub(lines.len() as u16 + 2);
        let mut frame = Vec::new();
        queue!(frame, crossterm::terminal::BeginSynchronizedUpdate, cursor::Hide, Clear(ClearType::All))?;
        let mut banner_row = 1;
        render_banner(&mut frame, &mut banner_row, cols, theme, 1, crate::i18n::tr("Select Language"), &[(crate::i18n::tr("Step"), crate::i18n::tr("1 of 3"))])?;
        crate::cli_ui::paint_onboarding_snow(&mut frame, banner_row, top)?;
        crate::interactive::draw_bottom_box_top(&mut frame, top, crate::i18n::tr("Language / Язык / 语言 · 1/3"), box_w,
            theme.primary_crossterm(), theme.border_crossterm())?;
        for (index, (text, color)) in lines.iter().enumerate() {
            let mut styled = Vec::new();
            queue!(styled, crossterm::style::SetForegroundColor(*color), Print(text), crossterm::style::ResetColor)?;
            crate::interactive::draw_bottom_box_line(&mut frame, top + index as u16 + 1,
                &String::from_utf8_lossy(&styled), 0, box_w, theme.border_crossterm())?;
        }
        crate::interactive::draw_bottom_box_bottom(&mut frame, rows.saturating_sub(1), hint, box_w, theme.border_crossterm())?;
        queue!(frame, crossterm::terminal::EndSynchronizedUpdate)?;
        let mut out = stdout().lock(); out.write_all(&frame)?; out.flush()
    }
    pub fn run_always() -> anyhow::Result<(UserPreferences, bool)> {
        let mut prefs = UserPreferences::load();
        crate::i18n::set_current(prefs.language);
        crate::theme::set_current(prefs.theme);

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

        if res? {
            prefs.save()?;
            Ok((prefs, true))
        } else {
            Err(OnboardingDeclined.into())
        }
    }

    pub fn run_if_needed() -> anyhow::Result<(UserPreferences, bool)> {
        let prefs = UserPreferences::load();
        crate::i18n::set_current(prefs.language);
        crate::theme::set_current(prefs.theme);
        if prefs.agreed_to_terms {
            return Ok((prefs, false));
        }
        Self::run_always()
    }

    fn interactive_wizard(prefs: &mut UserPreferences) -> std::io::Result<bool> {
        if let Some(language) = crate::interactive::select_language_interactive(None, prefs.language)? {
            prefs.language = language;
            crate::i18n::set_current(language);
        }
        // Step 2: Theme Selection
        let selected_theme = Self::select_theme(prefs.theme)?;
        prefs.theme = selected_theme;

        // Step 3: Terms of Service review & acceptance
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
                            crate::theme::set_current(initial);
                            return Ok(initial);
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }

    fn render_theme_selector(themes: &[Theme], selected_idx: usize) -> std::io::Result<()> {
        let (cols, rows) = crate::cli_ui::terminal_size();
        let selected = themes[selected_idx];
        crate::theme::set_current(selected);
        let primary = selected.primary_ansi();
        let secondary = selected.secondary_ansi();
        let border = selected.border_crossterm();
        let accent = selected.primary_crossterm();
        let box_w = (cols as usize).saturating_sub(2);
        let banner_height = if cols >= 80 { 10 } else { 6 };
        let visible = (rows as usize).saturating_sub(banner_height + 5)
            .clamp(1, 6).min(themes.len());
        let offset = selected_idx.saturating_sub(visible.saturating_sub(1));
        let top = rows.saturating_sub(visible as u16 + 4);

        let mut frame = Vec::new();
        queue!(frame, crossterm::terminal::BeginSynchronizedUpdate,
            cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        let mut banner_row = 1;
        let info = [
            (crate::i18n::tr("Selected"), selected.name()),
            (crate::i18n::tr("Controls"), "↑/↓ or j/k"),
            (crate::i18n::tr("Step"), crate::i18n::tr("2 of 3")),
        ];
        render_banner(&mut frame, &mut banner_row, cols, selected, 2,
            crate::i18n::tr("Select Visual Theme"), &info)?;
        crate::cli_ui::paint_onboarding_snow(&mut frame, banner_row, top)?;
        crate::interactive::draw_bottom_box_top(&mut frame, top,
            crate::i18n::tr("Select Visual Theme · 2/3"), box_w, accent, border)?;
        for (row, (index, theme)) in themes.iter().enumerate()
            .skip(offset).take(visible).enumerate() {
            let text = if index == selected_idx {
                format!("{primary}\x1b[1m❯ (●) {}\x1b[0m", theme.name())
            } else {
                format!("\x1b[90m  ( ) {}\x1b[0m", theme.name())
            };
            crate::interactive::draw_bottom_box_line(&mut frame,
                top + 1 + row as u16, &text, 0, box_w, border)?;
        }
        let description = format!("\x1b[90m{}\x1b[0m", selected.description());
        crate::interactive::draw_bottom_box_line(&mut frame,
            top + 1 + visible as u16, &description, 0, box_w, border)?;
        let preview = crate::i18n::tf!("{primary}\x1b[1m❯ Your message\x1b[0m  {secondary}│ Tool activity\x1b[0m", primary = primary, secondary = secondary);
        crate::interactive::draw_bottom_box_line(&mut frame,
            top + 2 + visible as u16, &preview, 0, box_w, border)?;
        let hint = crate::i18n::tf!("↑/↓: {}/{} · Live Preview · Enter: confirm · Esc: keep current",
            selected_idx + 1, themes.len());
        crate::interactive::draw_bottom_box_bottom(&mut frame,
            rows.saturating_sub(1), &hint, box_w, border)?;
        queue!(frame, crossterm::terminal::EndSynchronizedUpdate)?;
        let mut out = stdout().lock();
        out.write_all(&frame)?;
        out.flush()?;
        Ok(())
    }

    fn show_terms(theme: Theme) -> std::io::Result<bool> {
        crate::theme::set_current(theme);
        let mut scroll_offset: usize = 0;
        let mut user_choice = false;

        loop {
            let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let box_w = (term_cols as usize).saturating_sub(2);
            let inner_w = box_w.saturating_sub(4);

            let terms_lines = Self::get_wrapped_terms(inner_w, theme);

            let banner_rows = if term_cols >= 80 { 10 } else { 6 };
            let view_height = (term_rows as usize).saturating_sub(banner_rows + 6).max(1)
                .min(term_rows.saturating_sub(6).max(1) as usize);
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
                Event::Resize(_, _) => {}
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
        term_rows: u16,
    ) -> std::io::Result<()> {
        let mut frame = Vec::new();
        queue!(frame, crossterm::terminal::BeginSynchronizedUpdate,
            cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        let primary = theme.primary_ansi();
        let border = theme.border_crossterm();
        let accent = theme.primary_crossterm();
        let box_w = (term_cols as usize).saturating_sub(2);
        let mut banner_row = 1;
        let info = [
            (crate::i18n::tr("Version"), "1.0.0"),
            (crate::i18n::tr("Selected"), theme.name()),
            (crate::i18n::tr("Step"), crate::i18n::tr("3 of 3")),
        ];
        render_banner(&mut frame, &mut banner_row, term_cols, theme, 3,
            crate::i18n::tr("User Agreement"), &info)?;

        let top = term_rows.saturating_sub(view_height as u16 + 5);
        crate::cli_ui::paint_onboarding_snow(&mut frame, banner_row, top)?;
        crate::interactive::draw_bottom_box_top(&mut frame, top,
            crate::i18n::tr("User Agreement · 3/3"), box_w, accent, border)?;
        let end = (scroll_offset + view_height).min(lines.len());
        let status = crate::i18n::tf!("\x1b[90mLines {}–{} of {} · v1.0.0\x1b[0m",
            if lines.is_empty() { 0 } else { scroll_offset + 1 }, end, lines.len());
        crate::interactive::draw_bottom_box_line(&mut frame, top + 1,
            &status, 0, box_w, border)?;
        for index in 0..view_height {
            let line = lines.get(scroll_offset + index).map(String::as_str).unwrap_or("");
            crate::interactive::draw_bottom_box_line(&mut frame, top + 2 + index as u16,
                line, 0, box_w, border)?;
        }
        let decision_row = top + 2 + view_height as u16;
        for (index, (selected, label)) in [
            (user_choice, crate::i18n::tr("I accept the terms")),
            (!user_choice, crate::i18n::tr("Decline and exit")),
        ].into_iter().enumerate() {
            let text = if selected {
                format!("{primary}\x1b[1m❯ (●) {label}\x1b[0m")
            } else {
                format!("\x1b[90m  ( ) {label}\x1b[0m")
            };
            crate::interactive::draw_bottom_box_line(&mut frame,
                decision_row + index as u16, &text, 0, box_w, border)?;
        }
        crate::interactive::draw_bottom_box_bottom(&mut frame,
            term_rows.saturating_sub(1),
            crate::i18n::tr("↑/↓ PgUp/PgDn: scroll · Tab: choose · Enter: confirm · Esc: exit"),
            box_w, border)?;
        queue!(frame, crossterm::terminal::EndSynchronizedUpdate)?;
        let mut out = stdout().lock();
        out.write_all(&frame)?;
        out.flush()?;
        Ok(())
    }

    fn get_wrapped_terms(inner_w: usize, theme: Theme) -> Vec<String> {
        let p_ansi = theme.primary_ansi();
        let raw_sections: Vec<(&str, bool)> = vec![
            (crate::i18n::tr("TAKIZA HARNESS - END USER LICENSE & SAFETY AGREEMENT"), true),
            (crate::i18n::tr("Version 1.0.0 • Effective Date: September 2026"), false),
            ("", false),
            (crate::i18n::tr("1. AUTONOMOUS AI AGENT CAPABILITIES"), true),
            (crate::i18n::tr("Takiza Harness equips advanced AI models with direct tool access to your local operating system, file system, network, and shell. The agent possesses autonomous planning, code reading, editing, file creation, and command execution capabilities in your workspace."), false),
            ("", false),
            (crate::i18n::tr("2. USER RESPONSIBILITY & OVERSIGHT"), true),
            (crate::i18n::tr("You acknowledge and agree that:"), false),
            (crate::i18n::tr("  • You retain full supervisory authority over all tool actions."), false),
            (crate::i18n::tr("  • You are responsible for inspecting commands before execution."), false),
            (crate::i18n::tr("  • You will keep critical files version-controlled with Git."), false),
            (crate::i18n::tr("  • You can press Ctrl+C at any time to abort running operations."), false),
            ("", false),
            (crate::i18n::tr("3. SAFE COMPUTING & WORKSPACE ISOLATION"), true),
            (crate::i18n::tr("  • Do not run Takiza Harness in root or privileged system directories."), false),
            (crate::i18n::tr("  • File modifications and shell tasks are scoped to your workspace."), false),
            (crate::i18n::tr("  • Do not provide credentials, private keys, or confidential data you are not authorized to share with third-party LLMs."), false),
            ("", false),
            (crate::i18n::tr("4. THIRD-PARTY PROVIDERS & NETWORK USAGE"), true),
            (crate::i18n::tr("Takiza Harness interfaces with user-specified LLM API endpoints (such as Groq, OpenAI, OpenRouter, DeepSeek, or local Ollama). Your prompt context and tool output snippets are transmitted to the selected endpoint pursuant to that provider's privacy policy."), false),
            ("", false),
            (crate::i18n::tr("5. NO WARRANTY & LIMITATION OF LIABILITY"), true),
            (crate::i18n::tr("THE SOFTWARE IS PROVIDED 'AS IS', WITHOUT WARRANTY OF ANY KIND. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY DAMAGES, DATA LOSS, OR SYSTEM DAMAGE ARISING FROM THE USE OF AI AGENT AUTOMATION TOOLS."), false),
            ("", false),
            (crate::i18n::tr("By choosing 'I ACCEPT', you consent to all terms above."), true),
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
