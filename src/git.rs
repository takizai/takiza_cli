use std::path::Path;
use std::process::Command;

pub struct GitInfo {
    pub branch: Option<String>,
    pub is_dirty: bool,
}

impl GitInfo {
    pub fn get(workspace: &Path) -> Self {
        let branch = Command::new("git")
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

        let is_dirty = Command::new("git")
            .arg("status")
            .arg("--porcelain")
            .current_dir(workspace)
            .output()
            .ok()
            .map(|out| !out.stdout.is_empty())
            .unwrap_or(false);

        Self { branch, is_dirty }
    }

    pub fn diff(workspace: &Path) -> Option<String> {
        Command::new("git")
            .arg("diff")
            .arg("HEAD")
            .current_dir(workspace)
            .output()
            .ok()
            .and_then(|out| {
                let diff_str = String::from_utf8_lossy(&out.stdout).to_string();
                if diff_str.trim().is_empty() {
                    // Try unstaged
                    Command::new("git")
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
