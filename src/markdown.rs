use crate::prompt::{char_width, str_width, truncate_visible};
use crate::theme;

/// Computes visible width of a string in terminal columns, ignoring ANSI escape sequences.
#[allow(dead_code)]
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
        w += char_width(c);
    }
    w
}

/// Helper to find single closing delimiter (e.g. `*` or `_`).
fn find_single_delim(chars: &[char], delim: char) -> Option<usize> {
    if chars.is_empty() || chars[0].is_whitespace() {
        return None;
    }
    for i in 0..chars.len() {
        if chars[i] == delim && !chars[i.saturating_sub(1)].is_whitespace() {
            return Some(i);
        }
    }
    None
}

/// Helper to find double closing delimiter (e.g. `**`, `__`, `~~`).
fn find_double_delim(chars: &[char], delim: char) -> Option<usize> {
    if chars.len() < 2 || chars[0].is_whitespace() {
        return None;
    }
    for i in 0..chars.len().saturating_sub(1) {
        if chars[i] == delim && chars[i + 1] == delim && !chars[i.saturating_sub(1)].is_whitespace() {
            return Some(i);
        }
    }
    None
}

/// Helper to find triple closing delimiter (e.g. `***` or `___`).
fn find_triple_delim(chars: &[char], delim: char) -> Option<usize> {
    if chars.len() < 3 || chars[0].is_whitespace() {
        return None;
    }
    for i in 0..chars.len().saturating_sub(2) {
        if chars[i] == delim && chars[i + 1] == delim && chars[i + 2] == delim && !chars[i.saturating_sub(1)].is_whitespace() {
            return Some(i);
        }
    }
    None
}

/// Renders inline Markdown elements (bold, italic, strikethrough, inline code, links)
/// into terminal ANSI escape codes.
pub fn render_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;

    let th = theme::current();

    while i < n {
        // 1. Inline code: `code`
        if chars[i] == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '`') {
                let code_end = i + 1 + end;
                let code_content: String = chars[i + 1..code_end].iter().collect();
                out.push_str(th.code_background_ansi());
                out.push_str(th.primary_ansi());
                out.push(' ');
                out.push_str(&code_content);
                out.push_str(" \x1b[0m");
                i = code_end + 1;
                continue;
            }
        }

        // 2. Links: [text](url)
        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']') {
                let bracket_end = i + 1 + close_bracket;
                if bracket_end + 1 < n && chars[bracket_end + 1] == '(' {
                    if let Some(close_paren) = chars[bracket_end + 2..].iter().position(|&c| c == ')') {
                        let paren_end = bracket_end + 2 + close_paren;
                        let link_text: String = chars[i + 1..bracket_end].iter().collect();
                        let link_url: String = chars[bracket_end + 2..paren_end].iter().collect();

                        out.push_str(th.secondary_ansi());
                        out.push_str("\x1b[4m");
                        out.push_str(&render_inline(&link_text));
                        out.push_str("\x1b[0m ");
                        out.push_str(th.secondary_ansi());
                        out.push('(');
                        out.push_str(&link_url);
                        out.push_str(")\x1b[0m");

                        i = paren_end + 1;
                        continue;
                    }
                }
            }
        }

        // 3. Bold + Italic: ***text*** or ___text___
        if (chars[i] == '*' && i + 2 < n && chars[i + 1] == '*' && chars[i + 2] == '*')
            || (chars[i] == '_' && i + 2 < n && chars[i + 1] == '_' && chars[i + 2] == '_')
        {
            let delim = chars[i];
            let rest = &chars[i + 3..];
            if let Some(end) = find_triple_delim(rest, delim) {
                let content: String = rest[..end].iter().collect();
                out.push_str(th.primary_ansi());
                out.push_str("\x1b[1;3m");
                out.push_str(&render_inline(&content));
                out.push_str("\x1b[22;23m\x1b[39m");
                i += 3 + end + 3;
                continue;
            }
        }

        // 4. Bold: **text** or __text__
        if (chars[i] == '*' && i + 1 < n && chars[i + 1] == '*')
            || (chars[i] == '_' && i + 1 < n && chars[i + 1] == '_')
        {
            let delim = chars[i];
            let rest = &chars[i + 2..];
            if let Some(end) = find_double_delim(rest, delim) {
                let content: String = rest[..end].iter().collect();
                out.push_str(th.primary_ansi());
                out.push_str("\x1b[1m");
                out.push_str(&render_inline(&content));
                out.push_str("\x1b[22m\x1b[39m");
                i += 2 + end + 2;
                continue;
            }
        }

        // 5. Strikethrough: ~~text~~
        if chars[i] == '~' && i + 1 < n && chars[i + 1] == '~' {
            let rest = &chars[i + 2..];
            if let Some(end) = find_double_delim(rest, '~') {
                let content: String = rest[..end].iter().collect();
                out.push_str("\x1b[9m");
                out.push_str(&render_inline(&content));
                out.push_str("\x1b[29m");
                i += 2 + end + 2;
                continue;
            }
        }

        // 6. Italic: *text* or _text_
        if chars[i] == '*' || (chars[i] == '_' && (i == 0 || !chars[i - 1].is_alphanumeric())) {
            let delim = chars[i];
            let rest = &chars[i + 1..];
            if let Some(end) = find_single_delim(rest, delim) {
                let is_valid = if delim == '_' {
                    let next_pos = i + 1 + end + 1;
                    next_pos >= n || !chars[next_pos].is_alphanumeric()
                } else {
                    true
                };

                if is_valid {
                    let content: String = rest[..end].iter().collect();
                    out.push_str("\x1b[3m");
                    out.push_str(&render_inline(&content));
                    out.push_str("\x1b[23m");
                    i += 1 + end + 1;
                    continue;
                }
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

#[derive(Debug, Clone)]
enum Token {
    Escape(String),
    Space(String),
    Word(String, usize),
}

/// Tokenizes a string containing ANSI escape codes into Escape, Space, and Word tokens.
fn tokenize_ansi(s: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c == '\x1b' {
            let mut esc = String::new();
            esc.push(chars.next().unwrap());
            if chars.peek() == Some(&'[') {
                esc.push(chars.next().unwrap());
                while let Some(&next) = chars.peek() {
                    esc.push(chars.next().unwrap());
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            tokens.push(Token::Escape(esc));
        } else if c.is_whitespace() {
            let mut sp = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    sp.push(chars.next().unwrap());
                } else {
                    break;
                }
            }
            tokens.push(Token::Space(sp));
        } else {
            let mut word = String::new();
            let mut w = 0;
            while let Some(&next) = chars.peek() {
                if next == '\x1b' || next.is_whitespace() {
                    break;
                }
                let ch = chars.next().unwrap();
                w += char_width(ch);
                word.push(ch);
            }
            tokens.push(Token::Word(word, w));
        }
    }

    tokens
}

/// Wraps an inline-rendered string into lines where visible columns <= max_width.
/// Respects word boundaries and ANSI escape codes.
pub fn wrap_rendered_line(line: &str, max_width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    let max_width = max_width.max(1);
    let tokens = tokenize_ansi(line);
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_vis_w = 0;
    // Each viewport row is painted independently with a color reset. Replay
    // the inline style on continuation rows, including partial reset codes.
    let mut style = String::new();

    for token in tokens {
        match token {
            Token::Escape(esc) => {
                if esc == "\x1b[0m" || esc == "\x1b[m" { style.clear(); }
                else if esc.ends_with('m') { style.push_str(&esc); }
                current_line.push_str(&esc);
            }
            Token::Space(sp) => {
                let sp_w = str_width(&sp);
                if current_vis_w + sp_w <= max_width {
                    current_line.push_str(&sp);
                    current_vis_w += sp_w;
                } else if current_vis_w > 0 {
                    lines.push(format!("{current_line}\x1b[0m"));
                    current_line = style.clone();
                    current_vis_w = 0;
                }
            }
            Token::Word(word, w) => {
                if current_vis_w > 0 && current_vis_w + w > max_width {
                    lines.push(format!("{current_line}\x1b[0m"));
                    current_line = style.clone();
                    current_vis_w = 0;
                }
                for ch in word.chars() {
                    let width = char_width(ch);
                    if current_vis_w > 0 && current_vis_w + width > max_width {
                        lines.push(format!("{current_line}\x1b[0m"));
                        current_line = style.clone();
                        current_vis_w = 0;
                    }
                    current_line.push(ch);
                    current_vis_w += width;
                }
            }
        }
    }

    if current_vis_w > 0 || lines.is_empty() {
        lines.push(format!("{current_line}\x1b[0m"));
    }

    lines
}

#[derive(Clone, Copy)]
enum Alignment { Left, Center, Right }

fn table_cells(line: &str) -> Option<Vec<String>> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut code = false;
    let mut chars = line.trim().chars().peekable();
    let mut pipes = 0;
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => { cell.push(chars.next().unwrap()); }
            '`' => { code = !code; cell.push(c); }
            '|' if !code => { cells.push(cell.trim().to_string()); cell.clear(); pipes += 1; }
            _ => cell.push(c),
        }
    }
    if pipes == 0 { return None; }
    cells.push(cell.trim().to_string());
    if cells.first().is_some_and(String::is_empty) { cells.remove(0); }
    if cells.last().is_some_and(String::is_empty) { cells.pop(); }
    (!cells.is_empty()).then_some(cells)
}

fn table_alignment(line: &str, columns: usize) -> Option<Vec<Alignment>> {
    let cells = table_cells(line)?;
    if cells.len() != columns { return None; }
    cells.into_iter().map(|cell| {
        let dashes = cell.trim_matches(':');
        if dashes.len() < 3 || !dashes.chars().all(|c| c == '-') { return None; }
        Some(match (cell.starts_with(':'), cell.ends_with(':')) {
            (true, true) => Alignment::Center,
            (_, true) => Alignment::Right,
            _ => Alignment::Left,
        })
    }).collect()
}

fn render_table(rows: &[Vec<String>], alignment: &[Alignment], max_width: usize) -> Vec<String> {
    let columns = alignment.len();
    let th = theme::current();
    let border = th.secondary_ansi();
    let primary = th.primary_ansi();
    let minimums: Vec<usize> = (0..columns).map(|col| rows.iter()
        .flat_map(|row| row[col].chars()).map(char_width).max().unwrap_or(1).max(1)).collect();
    // A grid needs one character per cell plus borders and padding. On very
    // narrow screens use labeled records instead of losing or clipping data.
    if max_width < minimums.iter().sum::<usize>() + columns * 3 + 1 {
        let mut lines = Vec::new();
        for row in rows.iter().skip(1) {
            for (header, cell) in rows[0].iter().zip(row) {
                lines.extend(wrap_rendered_line(&format!("{primary}{}:\x1b[0m {}",
                    render_inline(header), render_inline(cell)), max_width));
            }
            lines.push(String::new());
        }
        if rows.len() == 1 {
            lines.extend(wrap_rendered_line(&render_inline(&rows[0].join(" · ")), max_width));
        }
        return lines;
    }
    let budget = max_width - columns * 3 - 1;
    let mut widths: Vec<usize> = (0..columns).map(|col| rows.iter()
        .map(|row| visible_width(&render_inline(&row[col])))
        .max().unwrap_or(1).max(1).min(budget)).collect();
    while widths.iter().sum::<usize>() > budget {
        let widest = widths.iter().enumerate().filter(|(col, width)| **width > minimums[*col])
            .max_by_key(|(_, width)| **width).unwrap().0;
        widths[widest] -= 1;
    }
    let rule = |left: char, middle: char, right: char| {
        let segments: Vec<_> = widths.iter().map(|width| "─".repeat(width + 2)).collect();
        format!("{border}{left}{}{right}\x1b[0m", segments.join(&middle.to_string()))
    };
    let mut lines = vec![rule('╭', '┬', '╮')];
    for (row_index, row) in rows.iter().enumerate() {
        let cells: Vec<_> = row.iter().zip(&widths)
            .map(|(cell, width)| wrap_rendered_line(&render_inline(cell), *width)).collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..height {
            let mut line = format!("{border}│\x1b[0m");
            for (col, cell_lines) in cells.iter().enumerate() {
                let cell = cell_lines.get(line_index).map(String::as_str).unwrap_or("");
                let padding = widths[col].saturating_sub(visible_width(cell));
                let left = match alignment[col] { Alignment::Left => 0, Alignment::Center => padding / 2, Alignment::Right => padding };
                line.push(' ');
                line.push_str(&" ".repeat(left));
                if row_index == 0 { line.push_str(primary); line.push_str("\x1b[1m"); }
                line.push_str(cell);
                line.push_str("\x1b[0m");
                line.push_str(&" ".repeat(padding - left));
                line.push_str(&format!(" {border}│\x1b[0m"));
            }
            lines.push(line);
        }
        if row_index == 0 { lines.push(rule('├', '┼', '┤')); }
    }
    lines.push(rule('╰', '┴', '╯'));
    lines
}

/// Full Markdown document renderer:
/// Processes headers, bullet lists, numbered lists, blockquotes, horizontal rules,
/// code blocks, and standard paragraphs with inline styling and word wrapping.
pub fn render_markdown(text: &str, max_width: usize) -> Vec<String> {
    let mut result_lines = Vec::new();
    let raw_lines: Vec<&str> = text.lines().collect();

    let th = theme::current();
    let p_ansi = th.primary_ansi();
    let s_ansi = th.secondary_ansi();
    let code_background = th.code_background_ansi();

    let mut in_code_block = false;

    let code_box_w = max_width.min(84);
    let code_inner_w = code_box_w.saturating_sub(4);

    let mut raw_lines = raw_lines.into_iter().peekable();
    while let Some(raw) = raw_lines.next() {
        let trimmed = raw.trim();

        // 1. Fenced code block toggle
        if trimmed.starts_with("```") {
            if !in_code_block {
                in_code_block = true;
                let lang = trimmed.trim_start_matches('`').trim();
                let code_lang = if lang.is_empty() { "code" } else { lang };

                // Draw code box top: ╭─ [rust] ──────────────────────────────────╮
                let tag = format!(" [{}] ", code_lang);
                let tag_w = str_width(&tag);
                let dashes = code_box_w.saturating_sub(tag_w + 3);
                let top_line = format!(
                    "  {s_ansi}╭─\x1b[0m{}\x1b[1m{}\x1b[0m{s_ansi}{}╮\x1b[0m",
                    p_ansi, tag, "─".repeat(dashes)
                );
                result_lines.push(top_line);
            } else {
                in_code_block = false;
                // Draw code box bottom: ╰────────────────────────────────────────╯
                let dashes = code_box_w.saturating_sub(2);
                let bottom_line = format!("  {s_ansi}╰{}╯\x1b[0m", "─".repeat(dashes));
                result_lines.push(bottom_line);
            }
            continue;
        }

        // Inside code block: preserve literal content inside bordered box
        if in_code_block {
            let trunc = if str_width(raw) > code_inner_w {
                truncate_visible(raw, code_inner_w)
            } else {
                raw.to_string()
            };
            let vis_w = str_width(&trunc);
            let pad = code_inner_w.saturating_sub(vis_w);
            let code_line = format!(
                "  {s_ansi}│\x1b[0m {code_background}{p_ansi}{}\x1b[0m{} {s_ansi}│\x1b[0m",
                trunc, " ".repeat(pad)
            );
            result_lines.push(code_line);
            continue;
        }

        if let Some(header) = table_cells(raw) {
            if let Some(alignment) = raw_lines.peek().and_then(|line| table_alignment(line, header.len())) {
                raw_lines.next();
                let mut rows = vec![header];
                while let Some(cells) = raw_lines.peek().and_then(|line| table_cells(line)) {
                    if cells.len() != alignment.len() { break; }
                    raw_lines.next();
                    rows.push(cells);
                }
                result_lines.extend(render_table(&rows, &alignment, max_width));
                continue;
            }
        }

        // Empty line
        if trimmed.is_empty() {
            result_lines.push(String::new());
            continue;
        }

        // 2. Horizontal rule: --- or *** or ___
        if (trimmed.starts_with("---") || trimmed.starts_with("***") || trimmed.starts_with("___"))
            && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_' || c.is_whitespace())
            && trimmed.len() >= 3
        {
            let hr_w = max_width.min(48);
            result_lines.push(format!("{s_ansi}{}\x1b[0m", "─".repeat(hr_w)));
            continue;
        }

        // 3. Headers: #, ##, ###
        if let Some(h1) = trimmed.strip_prefix("# ") {
            let rendered = render_inline(h1);
            result_lines.extend(wrap_rendered_line(&format!("{}\x1b[1;4m{}\x1b[0m", p_ansi, rendered), max_width));
            continue;
        }
        if let Some(h2) = trimmed.strip_prefix("## ") {
            let rendered = render_inline(h2);
            result_lines.extend(wrap_rendered_line(&format!("{}\x1b[1m## {}\x1b[0m", p_ansi, rendered), max_width));
            continue;
        }
        if let Some(h3) = trimmed.strip_prefix("### ") {
            let rendered = render_inline(h3);
            result_lines.extend(wrap_rendered_line(&format!("{p_ansi}\x1b[1m### {}\x1b[0m", rendered), max_width));
            continue;
        }

        // 4. Blockquotes: > quote
        if let Some(quote) = trimmed.strip_prefix("> ") {
            let rendered = render_inline(quote);
            let wrapped = wrap_rendered_line(&rendered, max_width.saturating_sub(4));
            for w_line in wrapped {
                result_lines.push(format!("  {s_ansi}│\x1b[0m {s_ansi}\x1b[3m{}\x1b[0m", w_line));
            }
            continue;
        }

        // 5. Unordered List Items: - item or * item
        let is_bullet = trimmed.starts_with("- ") || trimmed.starts_with("* ");
        if is_bullet {
            let indent_spaces = raw.len() - raw.trim_start().len();
            let content = &trimmed[2..];
            let rendered = render_inline(content);
            let bullet_indent = " ".repeat(indent_spaces);
            let wrap_avail = max_width.saturating_sub(indent_spaces + 3).max(15);
            let wrapped = wrap_rendered_line(&rendered, wrap_avail);

            for (idx, w_line) in wrapped.into_iter().enumerate() {
                if idx == 0 {
                    result_lines.push(format!("{}{}•\x1b[0m {}", bullet_indent, p_ansi, w_line));
                } else {
                    result_lines.push(format!("{}  {}", bullet_indent, w_line));
                }
            }
            continue;
        }

        // 6. Ordered List Items: 1. item, 2. item, etc.
        let mut is_num_list = false;
        let mut num_prefix = String::new();
        let mut num_rest = "";
        if let Some(dot_idx) = trimmed.find(". ") {
            let prefix_cand = &trimmed[..dot_idx];
            if !prefix_cand.is_empty() && prefix_cand.chars().all(|c| c.is_ascii_digit()) {
                is_num_list = true;
                num_prefix = format!("{}. ", prefix_cand);
                num_rest = &trimmed[dot_idx + 2..];
            }
        }

        if is_num_list {
            let indent_spaces = raw.len() - raw.trim_start().len();
            let rendered = render_inline(num_rest);
            let base_indent = " ".repeat(indent_spaces);
            let prefix_w = str_width(&num_prefix);
            let wrap_avail = max_width.saturating_sub(indent_spaces + prefix_w).max(15);
            let wrapped = wrap_rendered_line(&rendered, wrap_avail);

            for (idx, w_line) in wrapped.into_iter().enumerate() {
                if idx == 0 {
                    result_lines.push(format!("{}{}{}\x1b[0m{}", base_indent, p_ansi, num_prefix, w_line));
                } else {
                    result_lines.push(format!("{}{}{}", base_indent, " ".repeat(prefix_w), w_line));
                }
            }
            continue;
        }

        // 7. Regular paragraph: apply inline formatting and word-wrap
        let rendered = render_inline(raw);
        let wrapped = wrap_rendered_line(&rendered, max_width);
        for w_line in wrapped {
            result_lines.push(w_line);
        }
    }

    result_lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_align_columns_and_style_headers() {
        let lines = render_markdown("| Пакет | Обычная цена | Через MoA |\n|---|---:|---:|\n|10M|490 ₽|270 ₽|\n|1B|24 900 ₽|13 695 ₽|", 80);
        assert!(lines.first().unwrap().contains('╭'));
        assert!(lines.last().unwrap().contains('╯'));
        assert!(lines[1].contains("\x1b[1m"));
        assert!(lines[3].contains("       490 ₽"));
        let widths: Vec<_> = lines.iter().map(|line| visible_width(line)).collect();
        assert!(widths.iter().all(|width| *width == widths[0]));
        assert!(!lines.iter().any(|line| line.contains("---")));
    }

    #[test]
    fn tables_wrap_cells_without_losing_unicode_or_breaking_borders() {
        let text = "Имя | Описание\n:---: | ---\n界界 | **Длинное** описание с эмодзи 🤖 и словами\nТест | `code`";
        for width in [11, 15, 24, 40, 80] {
            let lines = render_markdown(text, width);
            assert!(lines.iter().all(|line| visible_width(line) <= width), "{width}: {lines:?}");
            let joined = lines.join("\n");
            assert!(joined.contains('界') && joined.contains('🤖'));
            assert!(!joined.contains("**") && !joined.contains('`'));
            assert!(lines.first().unwrap().contains('╭'));
            assert!(lines.last().unwrap().contains('╯'));
        }
    }

    #[test]
    fn narrow_tables_fall_back_to_labeled_records() {
        let lines = render_markdown("| A | B | C |\n|---|---|---|\n|one|two|three|", 8);
        assert!(lines.iter().all(|line| visible_width(line) <= 8));
        let joined = lines.join("\n");
        assert!(joined.contains("one") && joined.contains("two") && joined.contains("three"));
        assert!(joined.contains("A:") && joined.contains("B:") && joined.contains("C:"));
    }

    #[test]
    fn table_detection_respects_code_escaped_pipes_and_partial_streams() {
        assert_eq!(table_cells(r"| a\|b | `c|d` |"), Some(vec!["a|b".into(), "`c|d`".into()]));
        for text in ["ordinary | text", "A | B\n--- | invalid", "A | B\n--- | --"] {
            assert!(!render_markdown(text, 60).join("\n").contains('┬'));
        }
        let lines = render_markdown("```\n| A | B |\n|---|---|\n```", 60);
        assert!(!lines.join("\n").contains('┬'));
        let lines = render_markdown("| A | B |\n|---|---|\n|unfinished", 60);
        assert!(lines.join("\n").contains("unfinished"));
    }

    #[test]
    fn test_inline_bold() {
        let res = render_inline("This is **bold** text");
        assert!(res.contains("\x1b[1mbold\x1b[22m"));
        assert!(!res.contains("**"));
    }

    #[test]
    fn test_inline_italic() {
        let res = render_inline("This is *italic* text");
        assert!(res.contains("\x1b[3mitalic\x1b[23m"));
        assert!(!res.contains("*italic*"));
    }

    #[test]
    fn test_inline_bold_italic() {
        let res = render_inline("This is ***both*** text");
        assert!(res.contains("\x1b[1;3mboth\x1b[22;23m"));
        assert!(!res.contains("***"));
    }

    #[test]
    fn test_inline_code() {
        let res = render_inline("Run `cargo test` now");
        assert!(res.contains("cargo test"));
        assert!(res.contains("\x1b[48;2;"));
        assert!(!res.contains("`cargo test`"));
    }

    #[test]
    fn test_inline_strikethrough() {
        let res = render_inline("This is ~~deleted~~ text");
        assert!(res.contains("\x1b[9mdeleted\x1b[29m"));
        assert!(!res.contains("~~"));
    }

    #[test]
    fn test_inline_links() {
        let res = render_inline("Check [Rust](https://rust-lang.org) here");
        assert!(res.contains("Rust"));
        assert!(res.contains("(https://rust-lang.org)"));
        assert!(!res.contains("[Rust]("));
    }

    #[test]
    fn test_wrap_rendered_line_with_ansi() {
        let line = "\x1b[1mHello\x1b[22m wonderful world of terminal AI";
        let wrapped = wrap_rendered_line(line, 20);
        assert!(wrapped.len() >= 2);
        for l in &wrapped {
            assert!(visible_width(l) <= 20);
        }
    }

    #[test]
    fn wrapping_keeps_styles_on_independently_painted_rows() {
        let lines = wrap_rendered_line("\x1b[31m\x1b[1mfirst second third\x1b[0m", 7);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.starts_with("\x1b[31m\x1b[1m")));
        assert!(lines.iter().all(|line| line.ends_with("\x1b[0m")));
    }

    #[test]
    fn long_words_links_and_headings_cannot_wrap_the_terminal() {
        for text in ["abcdefghijklmnopqrstuvwxyz", "## abcdefghijklmnopqrstuvwxyz", "[link](https://example.com/abcdefghijklmnopqrstuvwxyz)"] {
            let lines = render_markdown(text, 10);
            assert!(lines.iter().all(|line| visible_width(line) <= 10), "{lines:?}");
        }
    }

    #[test]
    fn test_code_block_rendering() {
        let md = "```rust\nfn main() {\n    println!(\"hi\");\n}\n```";
        let lines = render_markdown(md, 60);
        assert!(lines.len() >= 4);
        assert!(lines[0].contains("rust"));
        assert!(lines[1].contains("fn main()"));
        assert!(lines.last().unwrap().contains("╰"));
    }

    #[test]
    fn test_list_and_headers() {
        let md = "# Header 1\n- Item 1\n- Item 2\n1. Numbered";
        let lines = render_markdown(md, 60);
        assert!(lines[0].contains("Header 1"));
        assert!(lines[1].contains("•"));
        assert!(lines[3].contains("1. "));
    }
}
