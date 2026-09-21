use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

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
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let entry = format!("[{}] [{}] [{}] {}\n", now, level, module, msg);

    // 1. Log to workspace .takiza/takiza.log
    if let Ok(cwd) = std::env::current_dir() {
        let takiza_dir = cwd.join(".takiza");
        let _ = std::fs::create_dir_all(&takiza_dir);
        let log_file = takiza_dir.join("takiza.log");
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_file) {
            let _ = f.write_all(entry.as_bytes());
        }
    }

    // 2. Log to global ~/.config/takiza/takiza.log
    if let Some(home) = dirs_home() {
        let cfg_dir = home.join(".config").join("takiza");
        let _ = std::fs::create_dir_all(&cfg_dir);
        let log_file = cfg_dir.join("takiza.log");
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_file) {
            let _ = f.write_all(entry.as_bytes());
        }
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}
