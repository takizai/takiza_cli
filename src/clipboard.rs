use std::io::Write;
use std::process::{Command, Stdio};

fn commands(write: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        return vec![(if write { "pbcopy" } else { "pbpaste" }, vec![])];
    }
    if cfg!(windows) {
        return vec![("powershell.exe", vec!["-NoProfile", "-Command", if write {
            "$input | Set-Clipboard"
        } else { "[Console]::Out.Write((Get-Clipboard -Raw))" }])];
    }
    vec![
        (if write { "wl-copy" } else { "wl-paste" }, if write { vec![] } else { vec!["--no-newline"] }),
        ("xclip", vec!["-selection", "clipboard", if write { "-in" } else { "-out" }]),
        ("xsel", vec!["--clipboard", if write { "--input" } else { "--output" }]),
    ]
}

pub fn copy(text: &str) -> Result<(), String> {
    for (program, args) in commands(true) {
        let Ok(mut child) = Command::new(program).args(args).stdin(Stdio::piped())
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { continue; };
        let written = child.stdin.take().is_some_and(|mut input| input.write_all(text.as_bytes()).is_ok());
        if child.wait().is_ok_and(|status| status.success()) && written { return Ok(()); }
    }
    Err("Clipboard unavailable: install wl-clipboard, xclip or xsel.".into())
}

pub fn paste() -> Result<String, String> {
    // File lists take precedence over image previews supplied by file managers.
    if let Some(result) = formatted_paste("wl-paste", true) { return result; }
    if let Some(result) = formatted_paste("xclip", false) { return result; }
    if let Some(files) = native_files() { return files; }
    if let Some(image) = native_image() { return image; }
    for (program, args) in commands(false) {
        let Ok(output) = Command::new(program).args(args).stdin(Stdio::null())
            .stderr(Stdio::null()).output() else { continue; };
        if output.status.success() {
            return crate::attachments::pasted_text(&String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n").replace('\r', "\n"));
        }
    }
    Err("Clipboard unavailable: install wl-clipboard, xclip or xsel.".into())
}

fn formatted_paste(program: &str, wayland: bool) -> Option<Result<String, String>> {
    let list_args = if wayland { vec!["--list-types"] }
        else { vec!["-selection", "clipboard", "-out", "-target", "TARGETS"] };
    let types = Command::new(program).args(list_args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !types.status.success() { return None; }
    let types = String::from_utf8_lossy(&types.stdout);
    let mime = ["text/uri-list", "x-special/gnome-copied-files", "image/png", "image/jpeg", "image/webp", "image/gif"]
        .into_iter().find(|mime| types.split_whitespace().any(|value| value == *mime))?;
    let args = if wayland { vec!["--no-newline", "--type", mime] }
        else { vec!["-selection", "clipboard", "-out", "-target", mime] };
    let output = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !output.status.success() { return None; }
    Some(if mime.starts_with("image/") {
        crate::attachments::cache_image(&output.stdout)
    } else { crate::attachments::file_list(&String::from_utf8_lossy(&output.stdout)) })
}

fn native_image() -> Option<Result<String, String>> {
    if !cfg!(target_os = "macos") && !cfg!(windows) { return None; }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let path = std::env::temp_dir().join(format!("takiza-clipboard-{}-{stamp}.png", std::process::id()));
    let output = if cfg!(target_os = "macos") {
        let quoted = serde_json::to_string(&path.to_string_lossy()).ok()?;
        let script = format!("set imageData to the clipboard as «class PNGf»\nset targetFile to open for access POSIX file {quoted} with write permission\nwrite imageData to targetFile\nclose access targetFile");
        Command::new("osascript").args(["-e", &script]).stdin(Stdio::null()).stderr(Stdio::null()).output()
    } else {
        let quoted = path.to_string_lossy().replace('\'', "''");
        let script = format!("Add-Type -AssemblyName System.Windows.Forms; $image = [Windows.Forms.Clipboard]::GetImage(); if ($null -eq $image) {{ exit 1 }}; $image.Save('{quoted}', [Drawing.Imaging.ImageFormat]::Png); $image.Dispose()");
        Command::new("powershell.exe").args(["-NoProfile", "-STA", "-Command", &script]).stdin(Stdio::null()).stderr(Stdio::null()).output()
    };
    let result = if output.is_ok_and(|output| output.status.success()) {
        std::fs::read(&path).ok().map(|bytes| crate::attachments::cache_image(&bytes))
    } else { None };
    let _ = std::fs::remove_file(path);
    result
}

fn native_files() -> Option<Result<String, String>> {
    let output = if cfg!(target_os = "macos") {
        let script = "set fileList to the clipboard as list\nset output to \"\"\nrepeat with itemFile in fileList\nset output to output & POSIX path of (itemFile as alias) & linefeed\nend repeat\nreturn output";
        Command::new("osascript").args(["-e", script]).stdin(Stdio::null()).stderr(Stdio::null()).output()
    } else if cfg!(windows) {
        let script = "Add-Type -AssemblyName System.Windows.Forms; if (![Windows.Forms.Clipboard]::ContainsFileDropList()) { exit 1 }; [Windows.Forms.Clipboard]::GetFileDropList() | ForEach-Object { [Console]::Out.WriteLine($_) }";
        Command::new("powershell.exe").args(["-NoProfile", "-STA", "-Command", script]).stdin(Stdio::null()).stderr(Stdio::null()).output()
    } else { return None; };
    let output = output.ok().filter(|output| output.status.success() && !output.stdout.is_empty())?;
    Some(crate::attachments::file_list(&String::from_utf8_lossy(&output.stdout)))
}
