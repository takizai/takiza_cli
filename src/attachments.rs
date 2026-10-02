use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::{Path, PathBuf};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};

const IMAGE_MARKER: &str = "[Image: ";
static NEXT_IMAGE: AtomicU64 = AtomicU64::new(0);

pub fn image_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some(("image/png", "png")) }
    else if bytes.starts_with(b"\xff\xd8\xff") { Some(("image/jpeg", "jpg")) }
    else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { Some(("image/gif", "gif")) }
    else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { Some(("image/webp", "webp")) }
    else { None }
}

pub fn cache_image(bytes: &[u8]) -> Result<String, String> {
    let (_, extension) = image_type(bytes).ok_or("Unsupported clipboard image; use PNG, JPEG, GIF or WebP.")?;
    let directory = std::env::current_dir().map_err(|e| e.to_string())?.join(".takiza/attachments");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let path = directory.join(format!("image-{stamp}-{}.{}", NEXT_IMAGE.fetch_add(1, Ordering::Relaxed), extension));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(format!("{IMAGE_MARKER}{}] ", serde_json::to_string(&path.to_string_lossy()).map_err(|e| e.to_string())?))
}

/// Parse terminal-quoted paths without invoking a shell or expanding commands.
fn path_tokens(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => token.push(c),
            (_, '\\') => token.push(chars.next()?),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !token.is_empty() { tokens.push(std::mem::take(&mut token)); }
            }
            _ => token.push(c),
        }
    }
    if quote.is_some() { return None; }
    if !token.is_empty() { tokens.push(token); }
    Some(tokens)
}

fn local_path(token: &str) -> Option<PathBuf> {
    if token.starts_with("file:") {
        return reqwest::Url::parse(token).ok()?.to_file_path().ok();
    }
    let path = if let Some(rest) = token.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else { PathBuf::from(token) };
    Some(path)
}

fn quoted_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.chars().any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\')) {
        format!("'{}'", text.replace('\'', "'\\''"))
    } else { text.into_owned() }
}

fn read_image(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.by_ref().take(12).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if image_type(&bytes).is_none() { return Ok(None); }
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(Some(bytes))
}

/// Only a paste consisting entirely of existing local paths is a file drop.
/// Ordinary prose, commands and URLs remain plain text.
pub fn pasted_files(text: &str) -> Result<Option<String>, String> {
    let Some(tokens) = path_tokens(text) else { return Ok(None); };
    if tokens.is_empty() { return Ok(None); }
    let paths: Option<Vec<_>> = tokens.iter().map(|token| {
        let path = local_path(token)?;
        path.exists().then_some(path)
    }).collect();
    let Some(paths) = paths else { return Ok(None); };
    let mut parts = Vec::new();
    for path in paths {
        let image = if path.is_file() { read_image(&path)? } else { None };
        if let Some(bytes) = image { parts.push(cache_image(&bytes)?); }
        else { parts.push(format!("{} ", quoted_path(&path))); }
    }
    Ok(Some(parts.concat()))
}

pub fn pasted_text(text: &str) -> Result<String, String> {
    Ok(pasted_files(text)?.unwrap_or_else(|| text.to_string()))
}

pub fn file_list(text: &str) -> Result<String, String> {
    let paths = text.lines().filter(|line| !line.is_empty() && !line.starts_with('#') && *line != "copy" && *line != "cut")
        .filter_map(local_path).map(|path| quoted_path(&path)).collect::<Vec<_>>().join(" ");
    pasted_files(&paths)?.ok_or_else(|| "Clipboard file paths are unavailable.".into())
}

/// Read only explicitly attached images. Document paths remain text for tools.
pub fn prompt_images(text: &str) -> Result<Vec<String>, String> {
    let mut urls = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find(IMAGE_MARKER) {
        remaining = &remaining[start + IMAGE_MARKER.len()..];
        let mut stream = serde_json::Deserializer::from_str(remaining).into_iter::<String>();
        let Some(Ok(path)) = stream.next() else { continue; };
        let offset = stream.byte_offset();
        if !remaining[offset..].starts_with(']') { continue; }
        let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read attached image {path}: {e}"))?;
        let (mime, _) = image_type(&bytes).ok_or_else(|| format!("Unsupported attached image: {path}"))?;
        urls.push(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
        remaining = &remaining[offset + 1..];
    }
    // Unbracketed file drops arrive as normal typed paths in some terminals.
    {
        if let Some(tokens) = path_tokens(text) {
            for token in tokens {
                let Some(path) = local_path(&token).filter(|path| path.is_file()) else { continue; };
                let Ok(Some(bytes)) = read_image(&path) else { continue; };
                if let Some((mime, _)) = image_type(&bytes) {
                    let url = format!("data:{mime};base64,{}", STANDARD.encode(bytes));
                    if !urls.contains(&url) { urls.push(url); }
                }
            }
        }
    }
    Ok(urls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_parsed_without_shell_expansion() {
        assert_eq!(path_tokens("'/tmp/a b.png' /tmp/c\\ d.pdf"), Some(vec!["/tmp/a b.png".into(), "/tmp/c d.pdf".into()]));
        assert_eq!(path_tokens("'a'\\''b.txt'"), Some(vec!["a'b.txt".into()]));
        assert!(path_tokens("'unfinished").is_none());
        assert_eq!(pasted_text("please inspect $(pwd) and https://example.com").unwrap(), "please inspect $(pwd) and https://example.com");
        assert_eq!(image_type(b"%PDF-1.7"), None);
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n"), Some(("image/png", "png")));
        assert_eq!(image_type(b"RIFFxxxxWEBP"), Some(("image/webp", "webp")));
    }

    #[test]
    fn image_markers_attach_bytes_but_documents_remain_paths() {
        let dir = std::env::temp_dir().join(format!("takiza-attachment-test-{}-{}", std::process::id(), NEXT_IMAGE.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image with spaces.png");
        let document = dir.join("document.pdf");
        let bytes = b"\x89PNG\r\n\x1a\nfixture";
        std::fs::write(&image, bytes).unwrap();
        std::fs::write(&document, b"%PDF document body").unwrap();
        let text = format!("look {IMAGE_MARKER}{}] {}", serde_json::to_string(&image.to_string_lossy()).unwrap(), quoted_path(&document));
        assert_eq!(prompt_images(&text).unwrap(), [format!("data:image/png;base64,{}", STANDARD.encode(bytes))]);
        assert_eq!(prompt_images(&format!("look {}", quoted_path(&image))).unwrap(), prompt_images(&text).unwrap());
        assert!(prompt_images(&quoted_path(&document)).unwrap().is_empty());
        assert_eq!(pasted_files(&quoted_path(&document)).unwrap(), Some(format!("{} ", quoted_path(&document))));
        assert!(pasted_text("ordinary text").unwrap() == "ordinary text");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
