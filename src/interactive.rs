use crate::config::Config;
use crate::prompt::{str_width, truncate_visible};
use crate::session::Session;
use crate::theme::{self, AppMode, Theme};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{stdout, Write};
use std::path::Path;

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
                    format!("{} (dirty)", b)
                } else {
                    format!("{} (clean)", b)
                }
            }
            None => "(no git)".to_string(),
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

#[derive(Clone, Debug)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub popular_models: &'static [&'static str],
    pub env_key_name: &'static str,
    pub description: &'static str,
}

pub const PRESET_PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "groq",
        name: "Groq Cloud",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        popular_models: &[
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
            "deepseek-r1-distill-llama-70b",
            "mixtral-8x7b-32768",
            "openai/gpt-oss-120b",
        ],
        env_key_name: "GROQ_API_KEY",
        description: "Ultra-fast LPU inference (Llama 3.3, DeepSeek R1, Mixtral)",
    },
    ProviderPreset {
        id: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-3.5-sonnet",
        popular_models: &[
            "anthropic/claude-3.5-sonnet",
            "openai/gpt-4o",
            "deepseek/deepseek-r1",
            "google/gemini-2.0-flash-exp:free",
            "meta-llama/llama-3.3-70b-instruct",
            "qwen/qwen-2.5-coder-32b-instruct",
        ],
        env_key_name: "OPENROUTER_API_KEY",
        description: "Unified router accessing Claude 3.5, GPT-4o, DeepSeek, Gemini",
    },
    ProviderPreset {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
        popular_models: &[
            "gpt-4o",
            "gpt-4o-mini",
            "o1",
            "o3-mini",
            "gpt-4-turbo",
        ],
        env_key_name: "OPENAI_API_KEY",
        description: "Official OpenAI endpoints (GPT-4o, o1, o3-mini)",
    },
    ProviderPreset {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        default_model: "deepseek-chat",
        popular_models: &[
            "deepseek-chat",
            "deepseek-reasoner",
        ],
        env_key_name: "DEEPSEEK_API_KEY",
        description: "DeepSeek V3 Chat & R1 Reasoner official API",
    },
    ProviderPreset {
        id: "ollama",
        name: "Ollama (Local)",
        base_url: "http://localhost:11434/v1",
        default_model: "qwen2.5-coder:latest",
        popular_models: &[
            "qwen2.5-coder:latest",
            "deepseek-r1:latest",
            "llama3.2:latest",
            "codellama:latest",
            "mistral:latest",
        ],
        env_key_name: "",
        description: "Offline privacy-first local LLM runner (no API key needed)",
    },
];

pub fn find_provider(name_or_id: &str) -> Option<ProviderPreset> {
    let clean = name_or_id.trim().to_lowercase();
    PRESET_PROVIDERS.iter().find(|p| p.id == clean || p.name.to_lowercase().contains(&clean)).cloned()
}

pub struct ProviderChoice {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub default_model: String,
    pub api_key: Option<String>,
}

fn draw_picker_top(out: &mut impl Write, row: &mut u16, title: &str, box_w: usize, p_color: Color, b_color: Color) -> std::io::Result<()> {
    let title_w = str_width(title);
    let dashes = box_w.saturating_sub(title_w + 5);
    queue!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print("  "),
        SetForegroundColor(b_color),
        Print("╭─ "),
        SetForegroundColor(p_color),
        Print(title),
        SetForegroundColor(b_color),
        Print(" "),
        Print("─".repeat(dashes)),
        Print("╮"),
        ResetColor
    )?;
    *row += 1;
    Ok(())
}

fn draw_picker_divider(out: &mut impl Write, row: &mut u16, box_w: usize, b_color: Color) -> std::io::Result<()> {
    let dashes = box_w.saturating_sub(2);
    queue!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print("  "),
        SetForegroundColor(b_color),
        Print("├"),
        Print("─".repeat(dashes)),
        Print("┤"),
        ResetColor
    )?;
    *row += 1;
    Ok(())
}

fn draw_picker_line(out: &mut impl Write, row: &mut u16, content: &str, content_w: usize, box_w: usize, b_color: Color) -> std::io::Result<()> {
    let inner_w = box_w.saturating_sub(4);
    let pad = inner_w.saturating_sub(content_w);
    queue!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print("  "),
        SetForegroundColor(b_color),
        Print("│ "),
        ResetColor,
        Print(content),
        Print(" ".repeat(pad)),
        SetForegroundColor(b_color),
        Print(" │"),
        ResetColor
    )?;
    *row += 1;
    Ok(())
}

fn draw_picker_bottom(out: &mut impl Write, row: &mut u16, hint: &str, box_w: usize, b_color: Color) -> std::io::Result<()> {
    let hint_w = str_width(hint);
    let dashes = box_w.saturating_sub(hint_w + 5);
    queue!(
        out,
        cursor::MoveTo(0, *row),
        Clear(ClearType::UntilNewLine),
        Print("  "),
        SetForegroundColor(b_color),
        Print("╰─ "),
        SetForegroundColor(Color::DarkGrey),
        Print(hint),
        SetForegroundColor(b_color),
        Print(" "),
        Print("─".repeat(dashes)),
        Print("╯"),
        ResetColor
    )?;
    *row += 1;
    Ok(())
}

fn draw_bottom_box_top(
    out: &mut impl Write,
    row: u16,
    title: &str,
    box_w: usize,
    primary: Color,
    border: Color,
) -> std::io::Result<()> {
    let title_tag = format!(" {} ", title);
    let title_w = str_width(&title_tag);
    let (safe_title, safe_title_w) = if title_w + 4 >= box_w {
        (format!(" {} ", truncate_visible(title, box_w.saturating_sub(6))), box_w.saturating_sub(4))
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

fn draw_bottom_box_line(
    out: &mut impl Write,
    row: u16,
    content: &str,
    visible_width: usize,
    box_w: usize,
    border: Color,
) -> std::io::Result<()> {
    let inner_w = box_w.saturating_sub(4);
    let pad = inner_w.saturating_sub(visible_width);

    queue!(
        out,
        cursor::MoveTo(0, row),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(border),
        Print("│ "),
        ResetColor,
        Print(content),
        Print(" ".repeat(pad)),
        SetForegroundColor(border),
        Print(" │"),
        ResetColor
    )?;
    Ok(())
}

fn draw_bottom_box_bottom(
    out: &mut impl Write,
    row: u16,
    hint: &str,
    box_w: usize,
    border: Color,
) -> std::io::Result<()> {
    let hint_tag = format!(" {} ", hint);
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
    let mut buffer = default_val.to_string();
    let mut cursor_idx = buffer.chars().count();
    let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let box_w = crate::cli_ui::get_box_width();
    let th = theme::current();
    let b_color = th.border_crossterm();
    let p_color = th.primary_crossterm();
    let mut needs_clear = true;

    loop {
        let mut out = stdout();
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
        let display_line = format!("{} {}", prompt_label, buffer);
        let display_w = str_width(&display_line);
        draw_bottom_box_line(&mut out, start_row + 1, &display_line, display_w, box_w, b_color)?;
        draw_bottom_box_bottom(&mut out, start_row + 2, "Enter: confirm  •  Esc: cancel", box_w, b_color)?;
        
        let pre_cursor: String = buffer.chars().take(cursor_idx).collect();
        let cursor_x = 2 + str_width(prompt_label) + 1 + str_width(&pre_cursor);
        queue!(out, cursor::MoveTo(cursor_x as u16, start_row + 1), cursor::Show)?;
        out.flush()?;

        match event::read()? {
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
                    KeyCode::Char(c) => {
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
            Event::Resize(_, _) => {
                needs_clear = true;
            }
            _ => {}
        }
    }
}

/// Prompts user to configure a Custom AI Provider endpoint (Base URL, Model, API Key) in a single bottom box.
pub fn prompt_custom_provider(ctx: &ScreenContext) -> std::io::Result<Option<ProviderChoice>> {
    let mut base_url = if ctx.base_url.contains("localhost") || ctx.base_url.contains("127.0.0.1") {
        ctx.base_url.clone()
    } else {
        "http://localhost:8000/v1".to_string()
    };
    let mut model = if !ctx.model.is_empty() {
        ctx.model.clone()
    } else {
        "default-model".to_string()
    };
    let mut api_key = String::new();

    let mut active_field = 0usize; // 0: base_url, 1: model, 2: api_key
    let mut cursor_indices = [base_url.chars().count(), model.chars().count(), 0];
    let mut needs_clear = true;

    loop {
        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let total_box_height = 5; // top, 3 fields, bottom
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            "Custom AI Provider Endpoint".to_string()
        } else {
            format!("Custom AI Provider Endpoint [{}]", ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let fields = [
            ("Base URL: ", &base_url, "http://localhost:8000/v1"),
            ("Model ID: ", &model, "model-name"),
            ("API Key:  ", &api_key, "(optional)"),
        ];

        for (i, (label, val, placeholder)) in fields.iter().enumerate() {
            let is_active = i == active_field;
            let prefix = if is_active { "> " } else { "  " };
            let (val_styled, val_vis) = if val.is_empty() {
                (format!("\x1b[38;2;110;110;115m{}\x1b[0m", placeholder), str_width(placeholder))
            } else if is_active {
                (format!("\x1b[1;38;2;255;255;255m{}\x1b[0m", val), str_width(val))
            } else {
                (format!("\x1b[38;2;220;220;225m{}\x1b[0m", val), str_width(val))
            };

            let line = if is_active {
                format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{}\x1b[0m{}", prefix, p_ansi, label, val_styled)
            } else {
                format!("\x1b[38;2;160;160;165m{}{}\x1b[0m{}", prefix, label, val_styled)
            };
            let vis_len = 2 + str_width(label) + val_vis;
            draw_bottom_box_line(&mut out, start_row + 1 + i as u16, &line, vis_len, box_w, b_color)?;
        }

        draw_bottom_box_bottom(
            &mut out,
            start_row + 4,
            "Tab/Enter: Next  •  Enter on Key: Save  •  Esc: Cancel",
            box_w,
            b_color,
        )?;

        // Position cursor at active field value
        let active_val = match active_field {
            0 => &base_url,
            1 => &model,
            _ => &api_key,
        };
        let c_idx = cursor_indices[active_field].min(active_val.chars().count());
        let pre_cursor: String = active_val.chars().take(c_idx).collect();
        let cursor_x = 2 + 2 + str_width(fields[active_field].0) + str_width(&pre_cursor);
        let cursor_y = start_row + 1 + active_field as u16;
        queue!(out, cursor::MoveTo(cursor_x as u16, cursor_y), cursor::Show)?;
        out.flush()?;

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        active_field = (active_field + 1) % 3;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        active_field = if active_field == 0 { 2 } else { active_field - 1 };
                    }
                    KeyCode::Esc => {
                        return Ok(None);
                    }
                    KeyCode::Enter => {
                        if active_field < 2 && (active_field == 0 && !base_url.trim().is_empty() || active_field == 1 && !model.trim().is_empty()) {
                            active_field += 1;
                        } else if !base_url.trim().is_empty() && !model.trim().is_empty() {
                            let final_key = if api_key.trim().is_empty() {
                                None
                            } else {
                                Some(api_key.trim().to_string())
                            };
                            return Ok(Some(ProviderChoice {
                                id: "custom".to_string(),
                                name: "Custom Provider".to_string(),
                                base_url: base_url.trim().to_string(),
                                default_model: model.trim().to_string(),
                                api_key: final_key,
                            }));
                        }
                    }
                    KeyCode::Char(c) => {
                        let buf = match active_field {
                            0 => &mut base_url,
                            1 => &mut model,
                            _ => &mut api_key,
                        };
                        let mut chars: Vec<char> = buf.chars().collect();
                        let ci = cursor_indices[active_field].min(chars.len());
                        chars.insert(ci, c);
                        *buf = chars.into_iter().collect();
                        cursor_indices[active_field] += 1;
                    }
                    KeyCode::Backspace => {
                        if cursor_indices[active_field] > 0 {
                            let buf = match active_field {
                                0 => &mut base_url,
                                1 => &mut model,
                                _ => &mut api_key,
                            };
                            let mut chars: Vec<char> = buf.chars().collect();
                            let ci = cursor_indices[active_field] - 1;
                            chars.remove(ci);
                            *buf = chars.into_iter().collect();
                            cursor_indices[active_field] -= 1;
                        }
                    }
                    KeyCode::Delete => {
                        let buf = match active_field {
                            0 => &mut base_url,
                            1 => &mut model,
                            _ => &mut api_key,
                        };
                        let mut chars: Vec<char> = buf.chars().collect();
                        let ci = cursor_indices[active_field];
                        if ci < chars.len() {
                            chars.remove(ci);
                            *buf = chars.into_iter().collect();
                        }
                    }
                    KeyCode::Left => {
                        if cursor_indices[active_field] > 0 {
                            cursor_indices[active_field] -= 1;
                        }
                    }
                    KeyCode::Right => {
                        let max_c = match active_field {
                            0 => base_url.chars().count(),
                            1 => model.chars().count(),
                            _ => api_key.chars().count(),
                        };
                        if cursor_indices[active_field] < max_c {
                            cursor_indices[active_field] += 1;
                        }
                    }
                    KeyCode::Home => {
                        cursor_indices[active_field] = 0;
                    }
                    KeyCode::End => {
                        let max_c = match active_field {
                            0 => base_url.chars().count(),
                            1 => model.chars().count(),
                            _ => api_key.chars().count(),
                        };
                        cursor_indices[active_field] = max_c;
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

/// Interactive AI Provider selector menu.
pub fn select_provider(ctx: &ScreenContext, current_base_url: &str) -> std::io::Result<Option<ProviderChoice>> {
    enable_raw_mode()?;
    let res = select_provider_inner(ctx, current_base_url);
    disable_raw_mode()?;
    res
}

fn select_provider_inner(ctx: &ScreenContext, current_base_url: &str) -> std::io::Result<Option<ProviderChoice>> {
    let mut selected_idx = PRESET_PROVIDERS.iter().position(|p| p.base_url == current_base_url).unwrap_or(0);
    // Extra item for "Custom Provider Endpoint..."
    let total_options = PRESET_PROVIDERS.len() + 1;
    let mut needs_clear = true;

    loop {
        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let inner_w = box_w.saturating_sub(4);
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let two_lines = term_rows >= 30;
        let item_lines = if two_lines { 2 } else { 1 };
        let content_rows = total_options * item_lines;
        let total_box_height = 1 + content_rows + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            "Select AI Provider".to_string()
        } else {
            format!("Select AI Provider [{}]", ctx.branch_tag.trim())
        };

        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, p) in PRESET_PROVIDERS.iter().enumerate() {
            let is_sel = i == selected_idx;
            let is_curr = p.base_url == current_base_url;

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_curr {
                ("\x1b[1;38;2;40;220;120m[✔]\x1b[0m", 3)
            } else {
                ("\x1b[38;2;100;100;105m[ ]\x1b[0m", 3)
            };

            if two_lines {
                let name_styled = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;140;140;145mDefault: \x1b[38;2;210;210;215m{}\x1b[0m", prefix, mark_str, p_ansi, p.name, p.default_model)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;120;120;125mDefault: \x1b[38;2;170;170;175m{}\x1b[0m", prefix, mark_str, p.name, p.default_model)
                };
                let line1_vis = 2 + mark_vis + 1 + str_width(p.name) + 5 + 9 + str_width(p.default_model);
                draw_bottom_box_line(&mut out, cur_row, &name_styled, line1_vis, box_w, b_color)?;
                cur_row += 1;

                let desc_max = inner_w.saturating_sub(6);
                let desc_truncated = truncate_visible(p.description, desc_max);
                let line2 = format!("      \x1b[38;2;130;130;135m{}\x1b[0m", desc_truncated);
                let line2_vis = 6 + str_width(&desc_truncated);
                draw_bottom_box_line(&mut out, cur_row, &line2, line2_vis, box_w, b_color)?;
                cur_row += 1;
            } else {
                let fixed_w = 2 + mark_vis + 1 + str_width(p.name) + 5 + str_width(p.default_model);
                let desc_avail = inner_w.saturating_sub(fixed_w + 3);
                let desc_part = if desc_avail >= 12 {
                    let desc_trunc = truncate_visible(p.description, desc_avail.saturating_sub(2));
                    let vis = 3 + str_width(&desc_trunc);
                    (format!("  \x1b[38;2;110;110;115m({})\x1b[0m", desc_trunc), vis)
                } else {
                    (String::new(), 0)
                };

                let name_styled = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;200;200;205m{}\x1b[0m{}", prefix, mark_str, p_ansi, p.name, p.default_model, desc_part.0)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;160;160;165m{}\x1b[0m{}", prefix, mark_str, p.name, p.default_model, desc_part.0)
                };
                let line_vis = fixed_w + desc_part.1;
                draw_bottom_box_line(&mut out, cur_row, &name_styled, line_vis, box_w, b_color)?;
                cur_row += 1;
            }
        }

        // Custom Option
        let is_custom_sel = selected_idx == PRESET_PROVIDERS.len();
        let prefix = if is_custom_sel { "> " } else { "  " };
        let mark_str = "\x1b[38;2;100;100;105m[ ]\x1b[0m";
        let mark_vis = 3;

        if two_lines {
            let custom_title = if is_custom_sel {
                format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1mCustom Provider Endpoint...\x1b[0m", prefix, mark_str, p_ansi)
            } else {
                format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225mCustom Provider Endpoint...\x1b[0m", prefix, mark_str)
            };
            let custom_vis = 2 + mark_vis + 1 + 28;
            draw_bottom_box_line(&mut out, cur_row, &custom_title, custom_vis, box_w, b_color)?;
            cur_row += 1;

            let custom_desc = "      \x1b[38;2;130;130;135mEnter custom Base URL and Model manually\x1b[0m";
            draw_bottom_box_line(&mut out, cur_row, custom_desc, 6 + 40, box_w, b_color)?;
            cur_row += 1;
        } else {
            let custom_line = if is_custom_sel {
                format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1mCustom Provider Endpoint...\x1b[0m  \x1b[38;2;110;110;115m(Specify custom URL and Model)\x1b[0m", prefix, mark_str, p_ansi)
            } else {
                format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225mCustom Provider Endpoint...\x1b[0m  \x1b[38;2;110;110;115m(Specify custom URL and Model)\x1b[0m", prefix, mark_str)
            };
            let custom_vis = 2 + mark_vis + 1 + 28 + 2 + 30;
            draw_bottom_box_line(&mut out, cur_row, &custom_line, custom_vis, box_w, b_color)?;
            cur_row += 1;
        }

        draw_bottom_box_bottom(&mut out, cur_row, "↑/↓: Navigate  Enter: Select  Esc: Cancel", box_w, b_color)?;
        out.flush()?;

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
                            selected_idx = total_options - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < total_options {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        return Ok(None);
                    }
                    KeyCode::Enter => {
                        if selected_idx < PRESET_PROVIDERS.len() {
                            let p = &PRESET_PROVIDERS[selected_idx];
                            // Check if API key is required
                            let mut api_key = None;
                            if !p.env_key_name.is_empty() && std::env::var(p.env_key_name).is_err() {
                                let key_prompt = format!("Enter API Key for {} (or press Enter to skip):", p.name);
                                if let Some(entered) = prompt_string_input(ctx, &format!("API Key for {}", p.name), &key_prompt, "")? {
                                    if !entered.is_empty() {
                                        api_key = Some(entered);
                                    }
                                }
                            }
                            return Ok(Some(ProviderChoice {
                                id: p.id.to_string(),
                                name: p.name.to_string(),
                                base_url: p.base_url.to_string(),
                                default_model: p.default_model.to_string(),
                                api_key,
                            }));
                        } else {
                            // Custom endpoint: show ONLY the custom provider info box
                            if let Some(custom_choice) = prompt_custom_provider(ctx)? {
                                return Ok(Some(custom_choice));
                            }
                        }
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

/// Interactive Model selector menu.
pub fn select_model(ctx: &ScreenContext, current_base_url: &str, current_model: &str) -> std::io::Result<Option<String>> {
    enable_raw_mode()?;
    let res = select_model_inner(ctx, current_base_url, current_model);
    disable_raw_mode()?;
    res
}

fn select_model_inner(ctx: &ScreenContext, current_base_url: &str, current_model: &str) -> std::io::Result<Option<String>> {
    let preset = PRESET_PROVIDERS.iter().find(|p| p.base_url == current_base_url);
    let default_models: &[&str] = match preset {
        Some(p) => p.popular_models,
        None => &[
            "gpt-4o",
            "anthropic/claude-3.5-sonnet",
            "deepseek-chat",
            "llama-3.3-70b-versatile",
            "qwen2.5-coder:latest",
        ],
    };

    let mut models_list: Vec<String> = default_models.iter().map(|&s| s.to_string()).collect();
    if !models_list.contains(&current_model.to_string()) && !current_model.is_empty() {
        models_list.insert(0, current_model.to_string());
    }

    let mut selected_idx = models_list.iter().position(|m| m == current_model).unwrap_or(0);
    let total_options = models_list.len() + 1; // +1 for "Custom Model (type manually)..."
    let mut needs_clear = true;

    loop {
        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let total_box_height = 1 + total_options + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title_base = match preset {
            Some(p) => format!("Select AI Model ({})", p.name),
            None => "Select AI Model".to_string(),
        };
        let title = if ctx.branch_tag.is_empty() {
            title_base
        } else {
            format!("{} [{}]", title_base, ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, m) in models_list.iter().enumerate() {
            let is_sel = i == selected_idx;
            let is_curr = m == current_model;

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_curr {
                ("\x1b[1;38;2;40;220;120m[✔]\x1b[0m", 3)
            } else {
                ("\x1b[38;2;100;100;105m[ ]\x1b[0m", 3)
            };
            let line = if is_sel {
                format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1m{}\x1b[0m", prefix, mark_str, p_ansi, m)
            } else {
                format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225m{}\x1b[0m", prefix, mark_str, m)
            };
            let vis_len = 2 + mark_vis + 1 + str_width(m);
            draw_bottom_box_line(&mut out, cur_row, &line, vis_len, box_w, b_color)?;
            cur_row += 1;
        }

        // Custom model option
        let is_custom_sel = selected_idx == models_list.len();
        let prefix = if is_custom_sel { "> " } else { "  " };
        let mark_str = "\x1b[38;2;100;100;105m[ ]\x1b[0m";
        let mark_vis = 3;
        let custom_line = if is_custom_sel {
            format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1mCustom Model (type manually)...\x1b[0m", prefix, mark_str, p_ansi)
        } else {
            format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225mCustom Model (type manually)...\x1b[0m", prefix, mark_str)
        };
        draw_bottom_box_line(&mut out, cur_row, &custom_line, 2 + mark_vis + 1 + 32, box_w, b_color)?;
        cur_row += 1;

        draw_bottom_box_bottom(&mut out, cur_row, "↑/↓: Navigate  Enter: Select  Esc: Cancel", box_w, b_color)?;
        out.flush()?;

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
                            selected_idx = total_options - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < total_options {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        return Ok(None);
                    }
                    KeyCode::Enter => {
                        if selected_idx < models_list.len() {
                            return Ok(Some(models_list[selected_idx].clone()));
                        } else {
                            // Prompt for custom model string
                            if let Some(custom) = prompt_string_input(ctx, "Custom AI Model", "Enter Model Name/ID:", "")? {
                                if !custom.is_empty() {
                                    return Ok(Some(custom));
                                }
                            }
                            return Ok(None);
                        }
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

/// Interactive Theme selector menu with live preview.
pub fn select_theme_interactive(ctx: &ScreenContext, initial: Theme) -> std::io::Result<Option<Theme>> {
    enable_raw_mode()?;
    let themes = Theme::all();
    let mut selected_idx = themes.iter().position(|&t| t == initial).unwrap_or(0);
    let mut needs_clear = true;

    loop {
        let current_theme = themes[selected_idx];
        theme::set_current(current_theme);

        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let b_color = current_theme.border_crossterm();
        let p_color = current_theme.primary_crossterm();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let total_box_height = 1 + themes.len() + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            "Select Visual Theme".to_string()
        } else {
            format!("Select Visual Theme [{}]", ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, &t) in themes.iter().enumerate() {
            let is_sel = i == selected_idx;
            let is_init = t == initial;

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_init {
                ("\x1b[1;38;2;40;220;120m[active]\x1b[0m", 8)
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

        draw_bottom_box_bottom(&mut out, cur_row, "Live Preview  •  Enter: Confirm  Esc: Cancel", box_w, b_color)?;
        out.flush()?;

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

#[derive(Clone, Debug)]
pub struct CuratedModel {
    pub provider: &'static str,
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub is_expensive: bool,
}

pub const CURATED_MODELS: &[CuratedModel] = &[
    CuratedModel {
        provider: "OpenAI",
        id: "cx/gpt-6-astra",
        name: "GPT-6 Astra",
        description: "OpenAI flagship for demanding end-to-end work, advanced analysis & coding",
        is_expensive: true,
    },
    CuratedModel {
        provider: "OpenAI",
        id: "cx/gpt-5.6-sol",
        name: "GPT-5.6 Sol",
        description: "High-level reasoning & complex STEM architecture with long context",
        is_expensive: false,
    },
    CuratedModel {
        provider: "Anthropic",
        id: "cc/claude-opus-5",
        name: "Claude Opus 5",
        description: "Anthropic flagship for long-horizon agentic workflows and full-stack coding",
        is_expensive: false,
    },
    CuratedModel {
        provider: "Anthropic",
        id: "cc/claude-sonnet-5",
        name: "Claude Sonnet 5",
        description: "Frontier performance across autonomous engineering, coding and reasoning",
        is_expensive: false,
    },
    CuratedModel {
        provider: "Google",
        id: "ag/gemini-3.7-flash-high",
        name: "Gemini 3.7 Flash High",
        description: "Google high-velocity multimodal model with advanced reasoning & tool orchestration",
        is_expensive: false,
    },
    CuratedModel {
        provider: "Google",
        id: "ag/gemini-3.1-pro-low",
        name: "Gemini 3.1 Pro Low",
        description: "Balanced reasoning & complex codebase analysis at cost-effective inference",
        is_expensive: false,
    },
    CuratedModel {
        provider: "Moonshot AI",
        id: "kmc/k3",
        name: "Kimi K3",
        description: "Frontier multimodal reasoning with 256k context and full tool execution",
        is_expensive: false,
    },
    CuratedModel {
        provider: "Moonshot AI",
        id: "kmc/kimi-for-coding",
        name: "Kimi for Coding",
        description: "Specialized code generation & bug localization champion with vision capabilities",
        is_expensive: false,
    },
];

pub fn is_curated_model_match(model_a: &str, model_b: &str) -> bool {
    let a = model_a.trim();
    let b = model_b.trim();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.ends_with(b) || b.ends_with(a) {
        return true;
    }
    let base_a = a.split('/').last().unwrap_or(a);
    let base_b = b.split('/').last().unwrap_or(b);
    !base_a.is_empty() && base_a.eq_ignore_ascii_case(base_b)
}

pub fn select_mode_interactive(ctx: &ScreenContext, current_mode: AppMode) -> std::io::Result<Option<AppMode>> {
    enable_raw_mode()?;
    let modes = [AppMode::Manual, AppMode::MoA];
    let mut selected_idx = modes.iter().position(|&m| m == current_mode).unwrap_or(0);
    let mut needs_clear = true;

    loop {
        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let inner_w = box_w.saturating_sub(4);
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let two_lines = term_rows >= 22;
        let item_lines = if two_lines { 2 } else { 1 };
        let content_rows = modes.len() * item_lines;
        let total_box_height = 1 + content_rows + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            "Select Execution Mode".to_string()
        } else {
            format!("Select Execution Mode [{}]", ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, &m) in modes.iter().enumerate() {
            let is_sel = i == selected_idx;
            let is_curr = m == current_mode;

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_curr {
                ("\x1b[1;38;2;40;220;120m[active]\x1b[0m", 8)
            } else {
                ("        ", 8)
            };

            let (badge_str, badge_vis) = match m {
                AppMode::MoA => (" \x1b[1;38;2;40;220;120m[⚡ ~45% Cheaper]\x1b[0m", str_width(" [⚡ ~45% Cheaper]")),
                AppMode::Manual => ("", 0),
            };

            if two_lines {
                let line1 = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{:<14}\x1b[0m{}  {}", prefix, p_ansi, m.name(), badge_str, mark_str)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{:<14}\x1b[0m{}  {}", prefix, m.name(), badge_str, mark_str)
                };
                let line1_vis = 2 + 14 + badge_vis + 2 + mark_vis;
                draw_bottom_box_line(&mut out, cur_row, &line1, line1_vis, box_w, b_color)?;
                cur_row += 1;

                let desc_max = inner_w.saturating_sub(6);
                let desc_trunc = truncate_visible(m.description(), desc_max);
                let line2 = match m {
                    AppMode::MoA => format!("      \x1b[38;2;40;200;120m{}\x1b[0m", desc_trunc),
                    AppMode::Manual => format!("      \x1b[38;2;130;130;135m{}\x1b[0m", desc_trunc),
                };
                let line2_vis = 6 + str_width(&desc_trunc);
                draw_bottom_box_line(&mut out, cur_row, &line2, line2_vis, box_w, b_color)?;
                cur_row += 1;
            } else {
                let fixed_w = 2 + 14 + badge_vis + 2 + mark_vis;
                let desc_avail = inner_w.saturating_sub(fixed_w + 3);
                let desc_part = if desc_avail >= 10 {
                    let desc_trunc = truncate_visible(m.description(), desc_avail);
                    (format!("  \x1b[38;2;120;120;125m{}\x1b[0m", desc_trunc), 2 + str_width(&desc_trunc))
                } else {
                    (String::new(), 0)
                };

                let line = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{:<14}\x1b[0m{}  {}{}", prefix, p_ansi, m.name(), badge_str, mark_str, desc_part.0)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{:<14}\x1b[0m{}  {}{}", prefix, m.name(), badge_str, mark_str, desc_part.0)
                };
                let line_vis = fixed_w + desc_part.1;
                draw_bottom_box_line(&mut out, cur_row, &line, line_vis, box_w, b_color)?;
                cur_row += 1;
            }
        }

        draw_bottom_box_bottom(&mut out, cur_row, "Enter: Select  •  Esc: Cancel", box_w, b_color)?;
        out.flush()?;

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
                            selected_idx = modes.len() - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < modes.len() {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        return Ok(Some(modes[selected_idx]));
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
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

pub fn select_effort_interactive(ctx: &ScreenContext, current_effort: &str) -> std::io::Result<Option<String>> {
    enable_raw_mode()?;
    let efforts = [
        ("low", "Low", "Fast response, minimal reasoning tokens"),
        ("medium", "Medium", "Balanced reasoning depth (default)"),
        ("high", "High", "Deep analysis, exhaustive reasoning"),
    ];
    let cur = current_effort.to_lowercase();
    let mut selected_idx = efforts.iter().position(|&(id, _, _)| id == cur).unwrap_or(1);
    let mut needs_clear = true;

    loop {
        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let inner_w = box_w.saturating_sub(4);
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let two_lines = term_rows >= 22;
        let item_lines = if two_lines { 2 } else { 1 };
        let content_rows = efforts.len() * item_lines;
        let total_box_height = 1 + content_rows + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            "Select Reasoning Effort".to_string()
        } else {
            format!("Select Reasoning Effort [{}]", ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, &(id, name, desc)) in efforts.iter().enumerate() {
            let is_sel = i == selected_idx;
            let is_curr = id == cur;

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_curr {
                ("\x1b[1;38;2;40;220;120m[active]\x1b[0m", 8)
            } else {
                ("        ", 8)
            };

            if two_lines {
                let line1 = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{:<12}\x1b[0m  {}", prefix, p_ansi, name, mark_str)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{:<12}\x1b[0m  {}", prefix, name, mark_str)
                };
                let line1_vis = 2 + 12 + 2 + mark_vis;
                draw_bottom_box_line(&mut out, cur_row, &line1, line1_vis, box_w, b_color)?;
                cur_row += 1;

                let desc_max = inner_w.saturating_sub(6);
                let desc_trunc = truncate_visible(desc, desc_max);
                let line2 = format!("      \x1b[38;2;130;130;135m{}\x1b[0m", desc_trunc);
                let line2_vis = 6 + str_width(&desc_trunc);
                draw_bottom_box_line(&mut out, cur_row, &line2, line2_vis, box_w, b_color)?;
                cur_row += 1;
            } else {
                let fixed_w = 2 + 12 + 2 + mark_vis;
                let desc_avail = inner_w.saturating_sub(fixed_w + 3);
                let desc_part = if desc_avail >= 10 {
                    let desc_trunc = truncate_visible(desc, desc_avail);
                    (format!("  \x1b[38;2;120;120;125m{}\x1b[0m", desc_trunc), 2 + str_width(&desc_trunc))
                } else {
                    (String::new(), 0)
                };

                let line = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{:<12}\x1b[0m  {}{}", prefix, p_ansi, name, mark_str, desc_part.0)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{:<12}\x1b[0m  {}{}", prefix, name, mark_str, desc_part.0)
                };
                let line_vis = fixed_w + desc_part.1;
                draw_bottom_box_line(&mut out, cur_row, &line, line_vis, box_w, b_color)?;
                cur_row += 1;
            }
        }

        draw_bottom_box_bottom(&mut out, cur_row, "Enter: Select  •  Esc: Cancel", box_w, b_color)?;
        out.flush()?;

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
                            selected_idx = efforts.len() - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < efforts.len() {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        return Ok(Some(efforts[selected_idx].0.to_string()));
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
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

pub fn select_curated_model_interactive(ctx: &ScreenContext, current_model: &str) -> std::io::Result<Option<&'static CuratedModel>> {
    enable_raw_mode()?;
    let mut selected_idx = CURATED_MODELS
        .iter()
        .position(|m| is_curated_model_match(m.id, current_model))
        .unwrap_or(0);
    let mut needs_clear = true;

    loop {
        let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = crate::cli_ui::get_box_width();
        let inner_w = box_w.saturating_sub(4);
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();
        let p_ansi = th.primary_ansi();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        let two_lines = term_rows >= 32;
        let item_lines = if two_lines { 2 } else { 1 };
        let content_rows = CURATED_MODELS.len() * item_lines;
        let total_box_height = 1 + content_rows + 1;
        let start_row = term_rows.saturating_sub(total_box_height as u16);

        let title = if ctx.branch_tag.is_empty() {
            "Takiza Manual - Select Model".to_string()
        } else {
            format!("Takiza Manual - Select Model [{}]", ctx.branch_tag.trim())
        };
        draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

        let mut cur_row = start_row + 1;
        for (i, m) in CURATED_MODELS.iter().enumerate() {
            let is_sel = i == selected_idx;
            let is_curr = is_curated_model_match(m.id, current_model);

            let prefix = if is_sel { "> " } else { "  " };
            let (mark_str, mark_vis) = if is_curr {
                ("\x1b[1;38;2;40;220;120m[active]\x1b[0m", 8)
            } else {
                ("        ", 8)
            };

            let prov_styled = match m.provider {
                "OpenAI" => "\x1b[38;2;16;163;127m[OpenAI]\x1b[0m",
                "Anthropic" => "\x1b[38;2;217;119;87m[Anthropic]\x1b[0m",
                "Google" => "\x1b[38;2;66;133;244m[Google]\x1b[0m",
                "Moonshot AI" => "\x1b[38;2;147;112;219m[Moonshot]\x1b[0m",
                _ => "\x1b[38;2;160;160;165m[Model]\x1b[0m",
            };
            let prov_vis = match m.provider {
                "OpenAI" => 8,
                "Anthropic" => 11,
                "Google" => 8,
                "Moonshot AI" => 10,
                _ => 7,
            };

            let (cost_tag, cost_vis) = if m.is_expensive {
                (" \x1b[1;38;2;255;95;80m[⚠ $$$ / High Cost]\x1b[0m", str_width(" [⚠ $$$ / High Cost]"))
            } else {
                ("", 0)
            };

            if two_lines {
                let name_line = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1m{:<22}\x1b[0m{}  {}", prefix, prov_styled, p_ansi, m.name, cost_tag, mark_str)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225m{:<22}\x1b[0m{}  {}", prefix, prov_styled, m.name, cost_tag, mark_str)
                };
                let name_vis = 2 + prov_vis + 1 + 22 + cost_vis + 2 + mark_vis;
                draw_bottom_box_line(&mut out, cur_row, &name_line, name_vis, box_w, b_color)?;
                cur_row += 1;

                let desc_max = inner_w.saturating_sub(6);
                let desc_text = if m.is_expensive {
                    format!("⚠ High Cost ($10/1M in, $50/1M out)! {}", m.description)
                } else {
                    m.description.to_string()
                };
                let desc_trunc = truncate_visible(&desc_text, desc_max);
                let line2 = if m.is_expensive {
                    format!("      \x1b[38;2;255;130;100m{}\x1b[0m", desc_trunc)
                } else {
                    format!("      \x1b[38;2;130;130;135m{}\x1b[0m", desc_trunc)
                };
                let line2_vis = 6 + str_width(&desc_trunc);
                draw_bottom_box_line(&mut out, cur_row, &line2, line2_vis, box_w, b_color)?;
                cur_row += 1;
            } else {
                let fixed_vis = 2 + prov_vis + 1 + str_width(m.name) + cost_vis + 2 + mark_vis;
                let desc_avail = inner_w.saturating_sub(fixed_vis + 3);
                let desc_part = if desc_avail >= 12 {
                    let desc_trunc = truncate_visible(m.description, desc_avail);
                    (format!("  \x1b[38;2;120;120;125m• {}\x1b[0m", desc_trunc), 4 + str_width(&desc_trunc))
                } else {
                    (String::new(), 0)
                };

                let line = if is_sel {
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{} {}\x1b[1m{}\x1b[0m{}  {}{}", prefix, prov_styled, p_ansi, m.name, cost_tag, mark_str, desc_part.0)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m{} \x1b[38;2;220;220;225m{}\x1b[0m{}  {}{}", prefix, prov_styled, m.name, cost_tag, mark_str, desc_part.0)
                };
                let line_vis = fixed_vis + desc_part.1;
                draw_bottom_box_line(&mut out, cur_row, &line, line_vis, box_w, b_color)?;
                cur_row += 1;
            }
        }

        draw_bottom_box_bottom(&mut out, cur_row, "Enter: Select Model  •  Esc: Cancel", box_w, b_color)?;
        out.flush()?;

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
                            selected_idx = CURATED_MODELS.len() - 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected_idx + 1 < CURATED_MODELS.len() {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        return Ok(Some(&CURATED_MODELS[selected_idx]));
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
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

        let mut out = stdout();
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
            "Resume Saved Chat".to_string()
        } else {
            format!("Resume Saved Chat [{}]", ctx.branch_tag.trim())
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
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;210;210;215m{:2} msgs\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;160;160;165m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;120;120;125m{}\x1b[0m",
                        prefix, p_ansi, id, msgs_count, created, model_name)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;140;140;145m{:2} msgs\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;100;100;105m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;90;90;95m{}\x1b[0m",
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
                    format!("\x1b[1;38;2;255;255;255m{}\x1b[0m{}\x1b[1m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;210;210;215m{:2} msgs\x1b[0m  \x1b[38;2;130;130;135m(\"{}\")\x1b[0m",
                        prefix, p_ansi, id, msgs_count, preview_trunc)
                } else {
                    format!("\x1b[38;2;160;160;165m{}\x1b[0m\x1b[38;2;220;220;225m{}\x1b[0m  \x1b[38;2;70;70;75m•\x1b[0m  \x1b[38;2;140;140;145m{:2} msgs\x1b[0m  \x1b[38;2;100;100;105m(\"{}\")\x1b[0m",
                        prefix, id, msgs_count, preview_trunc)
                };
                let line_vis = 2 + str_width(id) + 5 + 7 + 4 + str_width(&preview_trunc);
                draw_bottom_box_line(&mut out, cur_row, &line_styled, line_vis, box_w, b_color)?;
                cur_row += 1;
            }
        }

        let hint = if sessions.len() > visible_count {
            format!("↑/↓: navigate ({}/{})  •  Enter: resume  •  d: delete  •  Esc: cancel", selected_idx + 1, sessions.len())
        } else {
            "Enter: resume  •  d: delete  •  Esc: cancel".to_string()
        };
        draw_bottom_box_bottom(&mut out, cur_row, &hint, box_w, b_color)?;
        out.flush()?;

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

/// Interactive scrollable Git Diff viewer with Commit and Revert actions.
pub fn show_interactive_diff(ctx: &ScreenContext, diff: &str, workspace: &Path) -> std::io::Result<()> {
    enable_raw_mode()?;
    let lines: Vec<&str> = diff.lines().collect();
    let mut scroll_offset = 0;
    let mut needs_clear = true;

    loop {
        let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let box_w = (term_cols as usize).saturating_sub(4).min(90);
        let inner_w = box_w.saturating_sub(4);
        let view_h = (term_rows as usize).saturating_sub(18).max(5);
        let th = theme::current();
        let b_color = th.border_crossterm();
        let p_color = th.primary_crossterm();

        let mut out = stdout();
        if needs_clear {
            queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
            needs_clear = false;
        } else {
            queue!(out, cursor::Hide, cursor::MoveTo(0, 0))?;
        }
        let mut row = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

        draw_picker_top(&mut out, &mut row, "Git Workspace Diff", box_w, p_color, b_color)?;
        let status_line = format!("Lines: {} • [↑/↓, PgUp/PgDn] scroll • [c] commit • [r] revert • [Esc] back", lines.len());
        draw_picker_line(&mut out, &mut row, &format!("\x1b[38;2;160;160;165m{}\x1b[0m", status_line), str_width(&status_line), box_w, b_color)?;
        draw_picker_divider(&mut out, &mut row, box_w, b_color)?;

        let end_idx = (scroll_offset + view_h).min(lines.len());
        for i in scroll_offset..end_idx {
            let line = lines[i];
            let trunc = truncate_visible(line, inner_w);
            let trunc_w = str_width(&trunc);
            let colored = if line.starts_with('+') && !line.starts_with("+++") {
                format!("\x1b[38;2;80;230;120m{}\x1b[0m", trunc)
            } else if line.starts_with('-') && !line.starts_with("---") {
                format!("\x1b[38;2;255;90;90m{}\x1b[0m", trunc)
            } else if line.starts_with("@@") {
                format!("\x1b[1;38;2;0;220;255m{}\x1b[0m", trunc)
            } else if line.starts_with("diff --git") {
                format!("\x1b[1;38;2;255;255;255m{}\x1b[0m", trunc)
            } else {
                format!("\x1b[38;2;180;180;185m{}\x1b[0m", trunc)
            };
            draw_picker_line(&mut out, &mut row, &colored, trunc_w, box_w, b_color)?;
        }

        for _ in (end_idx - scroll_offset)..view_h {
            draw_picker_line(&mut out, &mut row, "", 0, box_w, b_color)?;
        }

        let hint = format!("[Scroll: {:>2}%]  •  c: commit  •  r: revert  •  Esc: back", if lines.len() <= view_h { 100 } else { (scroll_offset * 100) / (lines.len() - view_h) });
        draw_picker_bottom(&mut out, &mut row, &hint, box_w, b_color)?;
        out.flush()?;

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
                        if let Some(msg) = prompt_string_input(ctx, "Git Commit", "Commit message:", "")? {
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
                                    println!("\nGit commit result:\n{}", String::from_utf8_lossy(&out.stdout));
                                }
                                return Ok(());
                            }
                        }
                    }
                    KeyCode::Char('r') => {
                        // Revert action
                        if let Some(confirm) = prompt_string_input(ctx, "Revert Workspace Changes", "Revert all unstaged files? (yes/no):", "no")? {
                            if confirm.eq_ignore_ascii_case("yes") || confirm.eq_ignore_ascii_case("y") {
                                let _ = crate::git::create_git_command()
                                    .args(["checkout", "--", "."])
                                    .current_dir(workspace)
                                    .output();
                                disable_raw_mode()?;
                                println!("\nReverted all modifications back to git HEAD.\n");
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
            Event::Resize(_, _) => {
                needs_clear = true;
            }
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

    let mut out = stdout();
    queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

    let masked_key = if config.api_key.len() > 8 {
        format!("{}...{}", &config.api_key[..4], &config.api_key[config.api_key.len() - 4..])
    } else if !config.api_key.is_empty() {
        "(configured)".to_string()
    } else {
        "(not set)".to_string()
    };

    let approval_str = if config.auto_approve {
        "Auto-approve (skip permissions)"
    } else {
        "Ask on command (default)"
    };

    let items = [
        ("AI Model", config.model.as_str()),
        ("Mode", config.mode.name()),
        ("Endpoint", config.base_url.as_str()),
        ("API Key", masked_key.as_str()),
        ("Approval", approval_str),
        ("Visual Theme", th.name()),
        ("Workspace", config.workspace_dir.to_str().unwrap_or(".")),
        ("Git Branch", if branch.is_empty() { "(no git)" } else { branch }),
        ("Session ID", session.id.as_str()),
        ("Messages", &msg_count.to_string()),
    ];

    let total_box_height = 1 + items.len() + 1 + 1 + 1; // 13 lines
    let start_row = term_rows.saturating_sub(total_box_height as u16);

    let title = if branch.is_empty() {
        "Takiza Code Session & System Status".to_string()
    } else {
        format!("Takiza Code Session & System Status [{}]", branch.trim())
    };

    draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

    let mut cur_row = start_row + 1;
    for (k, v) in items {
        let max_v_w = inner_w.saturating_sub(16);
        let v_trunc = truncate_visible(v, max_v_w);
        let line = format!("\x1b[1;38;2;0;220;255m{:<14}\x1b[0m \x1b[38;2;220;220;225m{}\x1b[0m", k, v_trunc);
        let vis_len = 14 + 1 + str_width(&v_trunc);
        draw_bottom_box_line(&mut out, cur_row, &line, vis_len, box_w, b_color)?;
        cur_row += 1;
    }

    draw_bottom_box_divider(&mut out, cur_row, box_w, b_color)?;
    cur_row += 1;

    let actions_hint = "Actions: [p] Provider  •  [m] Model  •  [o] Mode  •  [u] Usage  •  [t] Theme  •  [r] Reset  •  [Esc] Back";
    let actions_trunc = truncate_visible(actions_hint, inner_w);
    let actions_vis = str_width(&actions_trunc);
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[1;38;2;255;255;255m{}\x1b[0m", actions_trunc), actions_vis, box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_bottom(&mut out, cur_row, "Press shortcut key or Esc to exit", box_w, b_color)?;
    out.flush()?;

    loop {
        if let Event::Key(key) = event::read()? {
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
                let mut lines = vec![(format!("Search: {}", query), theme.primary_crossterm())];
                for slot in 0..slots {
                    let index = start + slot;
                    if let Some(skill) = matching.get(index) {
                        lines.push((format!("{} {:<6} {}", if index == selected { ">" } else { " " },
                            if skill.is_workspace { "local" } else { "global" }, skill.name),
                            if index == selected { theme.primary_crossterm() } else { theme.secondary_crossterm() }));
                    } else {
                        lines.push((if slot == 0 { "No skills found.".into() } else { String::new() }, Color::DarkGrey));
                    }
                }
                let current = matching.get(selected);
                lines.push((current.map(|skill| skill.description.clone()).unwrap_or_else(||
                    "Place SKILL.md inside a skill folder under .takiza/skills or .agents/skills.".into()), Color::DarkGrey));
                lines.push((current.map(|skill| skill.path.clone()).unwrap_or_default(), theme.secondary_crossterm()));
                let title = format!("Skills · {} local · {} global", local, skills.len() - local);
                let hint = format!("{}/{} · ↑↓ · Enter Insert · F5 Rescan · Esc Back", if matching.is_empty() { 0 } else { selected + 1 }, matching.len());
                panel.draw(ctx, &title, &lines, &hint)?;
            }
            match pending_event.take().map(Ok).unwrap_or_else(event::read)? {
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
pub fn show_interactive_tools(ctx: &ScreenContext) -> std::io::Result<()> {
    enable_raw_mode()?;
    let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let box_w = crate::cli_ui::get_box_width();
    let th = theme::current();
    let b_color = th.border_crossterm();
    let p_color = th.primary_crossterm();

    let mut out = stdout();
    queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

    let tools = [
        ("read_file", "(path, start, end)", "Reads workspace file with 1-indexed lines"),
        ("write_file", "(path, content)", "Creates or overwrites files in workspace"),
        ("edit_file", "(path, target, repl)", "Replaces exact substring chunk in file"),
        ("list_dir", "(path)", "Lists directory entries with file sizes"),
        ("find_files", "(pattern, path)", "Recursively searches files by glob/pattern"),
        ("grep_search", "(query, path)", "Recursively searches text contents inside files"),
        ("run_command", "(command)", "Executes bash shell commands in workspace sandbox"),
    ];

    let total_box_height = 1 + 1 + 1 + tools.len() + 1; // 11 lines
    let start_row = term_rows.saturating_sub(total_box_height as u16);

    let title = if ctx.branch_tag.is_empty() {
        "Autonomous Agent Tools & Capabilities".to_string()
    } else {
        format!("Autonomous Agent Tools & Capabilities [{}]", ctx.branch_tag.trim())
    };

    draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

    let mut cur_row = start_row + 1;
    let header_desc = "Takiza Harness provides 7 autonomous tools to inspect and modify code:";
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[38;2;160;160;165m{}\x1b[0m", header_desc), str_width(header_desc), box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_divider(&mut out, cur_row, box_w, b_color)?;
    cur_row += 1;

    for (name, sig, desc) in tools {
        let line = format!("\x1b[1;38;2;0;220;255m• {:<12}\x1b[0m \x1b[38;2;140;140;145m{:<22}\x1b[0m \x1b[38;2;210;210;215m{}\x1b[0m", name, sig, desc);
        let vis_len = 2 + 12 + 1 + 22 + 1 + str_width(desc);
        draw_bottom_box_line(&mut out, cur_row, &line, vis_len, box_w, b_color)?;
        cur_row += 1;
    }

    draw_bottom_box_bottom(&mut out, cur_row, "Press [Enter] or [Esc] to return to chat", box_w, b_color)?;
    out.flush()?;

    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            disable_raw_mode()?;
            return Ok(());
        }
    }
}

/// Interactive Quota & Token Usage dashboard rendered in bottom box replacing the input field.
pub fn show_interactive_usage(ctx: &ScreenContext, config: &Config) -> std::io::Result<()> {
    enable_raw_mode()?;
    let (_term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let box_w = crate::cli_ui::get_box_width();
    let inner_w = box_w.saturating_sub(4);
    let th = theme::current();
    let b_color = th.border_crossterm();
    let p_color = th.primary_crossterm();

    let mut out = stdout();
    queue!(out, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let _ = crate::cli_ui::print_banner_to(&mut out, &ctx.model, &ctx.base_url, &ctx.workspace, &ctx.git_info);

    let total_box_height = 13;
    let start_row = term_rows.saturating_sub(total_box_height as u16);

    let title = if ctx.branch_tag.is_empty() {
        "Token & Quota Usage [Mock]".to_string()
    } else {
        format!("Token & Quota Usage [Mock] [{}]", ctx.branch_tag.trim())
    };
    draw_bottom_box_top(&mut out, start_row, &title, box_w, p_color, b_color)?;

    let mut cur_row = start_row + 1;

    // Header intro
    let intro = "Daily API quotas and token limits breakdown for active modes:";
    let intro_trunc = truncate_visible(intro, inner_w);
    let intro_vis = str_width(&intro_trunc);
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[38;2;160;160;165m{}\x1b[0m", intro_trunc), intro_vis, box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_divider(&mut out, cur_row, box_w, b_color)?;
    cur_row += 1;

    // 1. Takiza Manual Quota
    let m_active = if config.mode == crate::theme::AppMode::Manual { " \x1b[1;38;2;40;220;120m[active]\x1b[0m" } else { "" };
    let m_header_plain = if config.mode == crate::theme::AppMode::Manual { "• Takiza Manual Quota [active]" } else { "• Takiza Manual Quota" };
    let m_header = format!("\x1b[1;38;2;0;220;255m• Takiza Manual Quota\x1b[0m{}", m_active);
    draw_bottom_box_line(&mut out, cur_row, &m_header, str_width(m_header_plain), box_w, b_color)?;
    cur_row += 1;

    let m_bar_plain = "    [██████████░░░░░░░░░░]  520,000 / 1,000,000 tokens (52% used)";
    let m_bar = format!("    \x1b[38;2;255;195;0m[██████████░░░░░░░░░░]\x1b[0m  \x1b[1;38;2;240;240;245m520,000\x1b[0m \x1b[38;2;140;140;145m/ 1,000,000 tokens (52% used)\x1b[0m");
    draw_bottom_box_line(&mut out, cur_row, &m_bar, str_width(m_bar_plain), box_w, b_color)?;
    cur_row += 1;

    let m_det = "    Limit: 1.0M tokens/day  •  Remaining: 480k (48%)  •  Resets in: 4h 18m";
    let m_det_trunc = truncate_visible(m_det, inner_w);
    let m_det_vis = str_width(&m_det_trunc);
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[38;2;130;130;135m{}\x1b[0m", m_det_trunc), m_det_vis, box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_divider(&mut out, cur_row, box_w, b_color)?;
    cur_row += 1;

    // 2. Takiza MoA Quota
    let moa_active = if config.mode == crate::theme::AppMode::MoA { " \x1b[1;38;2;40;220;120m[active]\x1b[0m" } else { "" };
    let moa_header_plain = if config.mode == crate::theme::AppMode::MoA {
        "• Takiza MoA Quota [⚡ ~45% Cheaper] [active]"
    } else {
        "• Takiza MoA Quota [⚡ ~45% Cheaper]"
    };
    let moa_header = format!("\x1b[1;38;2;40;220;120m• Takiza MoA Quota\x1b[0m \x1b[1;38;2;40;220;120m[⚡ ~45% Cheaper]\x1b[0m{}", moa_active);
    draw_bottom_box_line(&mut out, cur_row, &moa_header, str_width(moa_header_plain), box_w, b_color)?;
    cur_row += 1;

    let moa_bar_plain = "    [████░░░░░░░░░░░░░░░░]  210,000 / 1,000,000 tokens (21% used)";
    let moa_bar = format!("    \x1b[38;2;40;220;120m[████░░░░░░░░░░░░░░░░]\x1b[0m  \x1b[1;38;2;240;240;245m210,000\x1b[0m \x1b[38;2;140;140;145m/ 1,000,000 tokens (21% used)\x1b[0m");
    draw_bottom_box_line(&mut out, cur_row, &moa_bar, str_width(moa_bar_plain), box_w, b_color)?;
    cur_row += 1;

    let moa_det = "    Limit: 1.0M tokens/day  •  Remaining: 790k (79%)  •  Saved via MoA: ~172k tokens";
    let moa_det_trunc = truncate_visible(moa_det, inner_w);
    let moa_det_vis = str_width(&moa_det_trunc);
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[38;2;130;130;135m{}\x1b[0m", moa_det_trunc), moa_det_vis, box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_divider(&mut out, cur_row, box_w, b_color)?;
    cur_row += 1;

    let footer = format!("  Active mode: {}  •  Daily quotas reset at 00:00 UTC (mock data)", config.mode.name());
    let footer_trunc = truncate_visible(&footer, inner_w);
    let footer_vis = str_width(&footer_trunc);
    draw_bottom_box_line(&mut out, cur_row, &format!("\x1b[38;2;160;160;165m{}\x1b[0m", footer_trunc), footer_vis, box_w, b_color)?;
    cur_row += 1;

    draw_bottom_box_bottom(&mut out, cur_row, "Press [Enter] or [Esc] to return to chat", box_w, b_color)?;
    out.flush()?;

    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            disable_raw_mode()?;
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_curated_models_constraints() {
        assert!(CURATED_MODELS.len() <= 10, "Curated models count must not exceed 10");

        let openai_count = CURATED_MODELS.iter().filter(|m| m.provider == "OpenAI").count();
        let anthropic_count = CURATED_MODELS.iter().filter(|m| m.provider == "Anthropic").count();
        let google_count = CURATED_MODELS.iter().filter(|m| m.provider == "Google").count();
        let moonshot_count = CURATED_MODELS.iter().filter(|m| m.provider == "Moonshot AI").count();

        assert!(openai_count >= 2, "Must have at least 2 OpenAI models, found {}", openai_count);
        assert!(anthropic_count >= 2, "Must have at least 2 Anthropic models, found {}", anthropic_count);
        assert!(google_count >= 2, "Must have at least 2 Google models, found {}", google_count);
        assert!(moonshot_count >= 2, "Must have at least 2 Moonshot AI models, found {}", moonshot_count);

        let expensive_models: Vec<&str> = CURATED_MODELS.iter().filter(|m| m.is_expensive).map(|m| m.id).collect();
        assert_eq!(expensive_models, vec!["cx/gpt-6-astra"], "Only cx/gpt-6-astra should have expensive badge");

        for m in CURATED_MODELS {
            assert!(!m.id.is_empty());
            assert!(!m.name.is_empty());
            assert!(!m.description.is_empty());
        }

        assert!(is_curated_model_match("cx/gpt-6-astra", "cx/gpt-6-astra"));
        assert!(is_curated_model_match("cx/gpt-6-astra", "gpt-6-astra"));
        assert!(is_curated_model_match("cx/gpt-6-astra", "openai/gpt-6-astra"));
        assert!(is_curated_model_match("kmc/k3", "k3"));
        assert!(!is_curated_model_match("kmc/k3", "kmc/kimi-for-coding"));
    }
}

/// Checkpoint selection and confirmation use the same keyboard controls as command menus.
pub fn rewind_menu(ctx: &ScreenContext, title: &str, details: &[String], choices: &[String]) -> std::io::Result<Option<usize>> {
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
                lines.push(("No checkpoints in this chat.".into(), th.secondary_crossterm()));
                lines.push(("Checkpoints are saved before prompts.".into(), Color::DarkGrey));
            } else if details.is_empty() {
                for (index, choice) in choices.iter().enumerate().skip(start).take(slots) {
                    lines.push((format!("{} {choice}", if index == selected { ">" } else { " " }),
                        if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() }));
                }
            } else {
                lines.push((format!("{} changes · files {}–{}", details.len(), start + 1, (start + slots).min(details.len())), Color::DarkGrey));
                for detail in details.iter().skip(start).take(slots) { lines.push((detail.clone(), th.secondary_crossterm())); }
                lines.push((String::new(), Color::DarkGrey));
                for (index, choice) in choices.iter().enumerate() {
                    lines.push((format!("{} {choice}", if index == selected { ">" } else { " " }),
                        if index == selected { th.primary_crossterm() } else { th.secondary_crossterm() }));
                }
            }
            if crate::cli_ui::terminal_is_small() { crate::cli_ui::render_small_terminal(); }
            else {
                let hint = if choices.is_empty() { "Esc Back".to_string() }
                    else if details.is_empty() { format!("{}/{} · ↑↓ · Enter Review · Esc Back", selected + 1, choices.len()) }
                    else { "↑↓ · Enter Confirm · PgUp/PgDn Files · Esc Back".to_string() };
                panel.draw(ctx, title, &lines, &hint)?;
            }
            match event::read()? {
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
