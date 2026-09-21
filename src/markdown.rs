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

    let _th = theme::current();

    while i < n {
        // 1. Inline code: `code`
        if chars[i] == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '`') {
                let code_end = i + 1 + end;
                let code_content: String = chars[i + 1..code_end].iter().collect();
                out.push_str("\x1b[48;2;38;38;44m\x1b[38;2;245;200;100m ");
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

                        out.push_str("\x1b[4;38;2;100;180;255m");
                        out.push_str(&render_inline(&link_text));
                        out.push_str("\x1b[0m \x1b[38;2;130;130;135m(");
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
                out.push_str("\x1b[1;3m");
                out.push_str(&render_inline(&content));
                out.push_str("\x1b[22;23m");
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
                out.push_str("\x1b[1m");
                out.push_str(&render_inline(&content));
                out.push_str("\x1b[22m");
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

    let tokens = tokenize_ansi(line);
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_vis_w = 0;

    for token in tokens {
        match token {
            Token::Escape(esc) => {
                current_line.push_str(&esc);
            }
            Token::Space(sp) => {
                let sp_w = sp.len();
                if current_vis_w + sp_w <= max_width {
                    current_line.push_str(&sp);
                    current_vis_w += sp_w;
                } else if !current_line.is_empty() {
                    lines.push(current_line);
                    current_line = String::new();
                    current_vis_w = 0;
                }
            }
            Token::Word(word, w) => {
                if current_vis_w + w <= max_width || current_vis_w == 0 {
                    current_line.push_str(&word);
                    current_vis_w += w;
                } else {
                    lines.push(current_line);
                    current_line = word;
                    current_vis_w = w;
                }
            }
        }
    }

    if !current_line.is_empty() || lines.is_empty() {
        lines.push(current_line);
    }

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

    let mut in_code_block = false;

    let code_box_w = max_width.min(84);
    let code_inner_w = code_box_w.saturating_sub(4);

    for raw in raw_lines {
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
                    "  \x1b[38;2;120;120;130m╭─\x1b[0m{}\x1b[1m{}\x1b[0m\x1b[38;2;120;120;130m{}╮\x1b[0m",
                    p_ansi, tag, "─".repeat(dashes)
                );
                result_lines.push(top_line);
            } else {
                in_code_block = false;
                // Draw code box bottom: ╰────────────────────────────────────────╯
                let dashes = code_box_w.saturating_sub(2);
                let bottom_line = format!("  \x1b[38;2;120;120;130m╰{}╯\x1b[0m", "─".repeat(dashes));
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
                "  \x1b[38;2;120;120;130m│\x1b[0m \x1b[38;2;225;230;240m{}\x1b[0m{} \x1b[38;2;120;120;130m│\x1b[0m",
                trunc, " ".repeat(pad)
            );
            result_lines.push(code_line);
            continue;
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
            result_lines.push(format!("\x1b[38;2;80;80;85m{}\x1b[0m", "─".repeat(hr_w)));
            continue;
        }

        // 3. Headers: #, ##, ###
        if let Some(h1) = trimmed.strip_prefix("# ") {
            let rendered = render_inline(h1);
            result_lines.push(format!("{}\x1b[1;4m{}\x1b[0m", p_ansi, rendered));
            continue;
        }
        if let Some(h2) = trimmed.strip_prefix("## ") {
            let rendered = render_inline(h2);
            result_lines.push(format!("{}\x1b[1m## {}\x1b[0m", p_ansi, rendered));
            continue;
        }
        if let Some(h3) = trimmed.strip_prefix("### ") {
            let rendered = render_inline(h3);
            result_lines.push(format!("\x1b[1;38;2;225;225;230m### {}\x1b[0m", rendered));
            continue;
        }

        // 4. Blockquotes: > quote
        if let Some(quote) = trimmed.strip_prefix("> ") {
            let rendered = render_inline(quote);
            let wrapped = wrap_rendered_line(&rendered, max_width.saturating_sub(4));
            for w_line in wrapped {
                result_lines.push(format!("  \x1b[38;2;100;100;105m│\x1b[0m \x1b[3;38;2;190;190;195m{}\x1b[0m", w_line));
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
        assert!(res.contains("\x1b[48;2;38;38;44m"));
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
