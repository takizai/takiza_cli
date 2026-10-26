use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 64 * 1024;
static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn append_log(path: &std::path::Path, entry: &str) -> std::io::Result<()> {
    let entry = if entry.len() > MAX_ENTRY_BYTES {
        let mut boundary = MAX_ENTRY_BYTES;
        while !entry.is_char_boundary(boundary) { boundary -= 1; }
        format!("{} [truncated]\n", &entry[..boundary])
    } else { entry.to_owned() };
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len().saturating_add(entry.len() as u64) > MAX_LOG_BYTES) {
        let backup = path.with_extension("log.1");
        match std::fs::remove_file(&backup) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
        std::fs::rename(path, backup)?;
    }
    OpenOptions::new().create(true).append(true).open(path)?.write_all(entry.as_bytes())
}

pub fn log_error(module: &str, msg: &str) {
    write_log_entry("ERROR", module, msg);
}

pub fn log_warn(module: &str, msg: &str) {
    write_log_entry("WARN", module, msg);
}

#[allow(dead_code)]
pub fn log_info(module: &str, msg: &str) {
    write_log_entry("INFO", module, msg);
}

fn write_log_entry(level: &str, module: &str, msg: &str) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let entry = format!("[{}] [{}] [{}] {}\n", now, level, module, msg);

    // 1. Log to workspace .takiza/takiza.log
    if let Ok(cwd) = std::env::current_dir() {
        let takiza_dir = cwd.join(".takiza");
        let _ = std::fs::create_dir_all(&takiza_dir);
        let log_file = takiza_dir.join("takiza.log");
        let _ = append_log(&log_file, &entry);
    }

    // 2. Log to global ~/.config/takiza/takiza.log
    if let Some(home) = dirs_home() {
        let cfg_dir = home.join(".config").join("takiza");
        let _ = std::fs::create_dir_all(&cfg_dir);
        let log_file = cfg_dir.join("takiza.log");
        let _ = append_log(&log_file, &entry);
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_rotate_and_large_unicode_entries_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("takiza.log");
        std::fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        append_log(&path, "next\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next\n");
        assert_eq!(std::fs::metadata(path.with_extension("log.1")).unwrap().len(), MAX_LOG_BYTES);
        append_log(&path, &"🦀".repeat(MAX_ENTRY_BYTES)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with(" [truncated]\n"));
        assert!(text.len() < MAX_ENTRY_BYTES + 100);
        // Replacing the backup must also work on Windows.
        std::fs::write(&path, vec![b'y'; MAX_LOG_BYTES as usize]).unwrap();
        append_log(&path, "last\n").unwrap();
        assert_eq!(std::fs::read(path.with_extension("log.1")).unwrap()[0], b'y');
    }
}
