use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub fn create_git_command() -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new("git");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[derive(Clone)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub is_dirty: bool,
}

impl GitInfo {
    pub fn get_cached(workspace: &Path) -> Self {
        use std::sync::{Mutex, OnceLock};
        use std::time::{Duration, Instant};
        type Cache = Option<(std::path::PathBuf, Instant, GitInfo)>;
        static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
        let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
        if let Some((path, timestamp, info)) = cache.as_ref() {
            if path == workspace && timestamp.elapsed() < Duration::from_millis(500) {
                return info.clone();
            }
        }
        let info = Self::get(workspace);
        *cache = Some((workspace.to_path_buf(), Instant::now(), info.clone()));
        info
    }

    pub fn get(workspace: &Path) -> Self {
        let branch = create_git_command()
            .arg("rev-parse")
            .arg("--abbrev-ref")
            .arg("HEAD")
            .current_dir(workspace)
            .output()
            .ok()
            .and_then(|out| {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !s.is_empty() {
                        Some(s)
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

        let is_dirty = if branch.is_some() {
            create_git_command()
                .arg("status")
                .arg("--porcelain")
                .current_dir(workspace)
                .output()
                .ok()
                .map(|out| out.status.success() && !out.stdout.is_empty())
                .unwrap_or(false)
        } else {
            false
        };

        Self { branch, is_dirty }
    }

    pub fn diff(workspace: &Path) -> Option<String> {
        create_git_command()
            .arg("diff")
            .arg("HEAD")
            .current_dir(workspace)
            .output()
            .ok()
            .and_then(|out| {
                let diff_str = String::from_utf8_lossy(&out.stdout).to_string();
                if diff_str.trim().is_empty() {
                    // Try unstaged
                    create_git_command()
                        .arg("diff")
                        .current_dir(workspace)
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                } else {
                    Some(diff_str)
                }
            })
    }
}
