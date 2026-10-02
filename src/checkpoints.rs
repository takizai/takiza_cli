//! Local filesystem checkpoints. Git is neither invoked nor required.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::{Read, Write}, path::{Component, Path, PathBuf}};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Entry {
    Directory,
    File { mode: u32, readonly: bool },
    Symlink { target: PathBuf },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    format_version: u32,
    pub id: String,
    pub created_at: String,
    pub label: String,
    pub session_id: String,
    #[serde(default)]
    pub conversation: Option<crate::session::Session>,
    entries: BTreeMap<PathBuf, Entry>,
}

fn directory(workspace: &Path) -> PathBuf { workspace.join(".takiza/checkpoints") }
fn files(workspace: &Path, point: &Checkpoint) -> PathBuf { directory(workspace).join(&point.id).join("files") }
fn excluded(path: &Path) -> bool {
    path.components().any(|part| matches!(part.as_os_str().to_str(),
        Some(".git" | ".takiza" | "node_modules" | "target" | ".venv" | "venv" | "__pycache__" | ".next" | ".cache")))
}
fn valid_path(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|part| matches!(part, Component::Normal(_))) && !excluded(path)
}
fn validate(point: &Checkpoint) -> Result<()> {
    anyhow::ensure!(point.format_version == 1 && point.id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'_') && !point.id.is_empty(), "Invalid checkpoint metadata");
    if let Some(session) = &point.conversation {
        anyhow::ensure!(session.id == point.session_id, "Checkpoint conversation belongs to another chat");
    }
    for (path, entry) in &point.entries {
        anyhow::ensure!(valid_path(path), "Invalid checkpoint path: {}", path.display());
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            anyhow::ensure!(matches!(point.entries.get(parent), Some(Entry::Directory)), "Invalid checkpoint parent: {}", parent.display());
        }
        if let Entry::File { mode, .. } = entry { anyhow::ensure!(*mode <= 0o7777, "Invalid checkpoint permissions"); }
    }
    Ok(())
}
fn scan(workspace: &Path) -> Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(parent) = pending.pop() {
        for item in fs::read_dir(workspace.join(&parent)).with_context(|| format!("Cannot read {}", parent.display()))? {
            let item = item?;
            let path = parent.join(item.file_name());
            if excluded(&path) { continue; }
            let metadata = fs::symlink_metadata(item.path())?;
            let entry = if metadata.file_type().is_symlink() {
                Entry::Symlink { target: fs::read_link(item.path())? }
            } else if metadata.is_dir() {
                pending.push(path.clone()); Entry::Directory
            } else if metadata.is_file() {
                #[cfg(unix)]
                let mode = { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() & 0o7777 };
                #[cfg(not(unix))]
                let mode = 0;
                Entry::File { mode, readonly: metadata.permissions().readonly() }
            } else { anyhow::bail!("Unsupported file type: {}", path.display()); };
            entries.insert(path, entry);
        }
    }
    Ok(entries)
}
fn same_contents(a: &Path, b: &Path) -> Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() { return Ok(false); }
    let mut a = fs::File::open(a)?;
    let mut b = fs::File::open(b)?;
    let mut left = [0u8; 65536];
    let mut right = [0u8; 65536];
    loop {
        let count = a.read(&mut left)?;
        if count == 0 { return Ok(true); }
        b.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] { return Ok(false); }
    }
}

#[cfg(test)]
fn capture(workspace: &Path, session_id: &str, label: &str) -> Result<Checkpoint> {
    capture_conversation(workspace, session_id, label, None)
}

pub fn capture_session(workspace: &Path, session: &crate::session::Session, label: &str) -> Result<Checkpoint> {
    capture_conversation(workspace, &session.id, label, Some(session.clone()))
}

pub fn conversation_at(point: &Checkpoint, current: &crate::session::Session) -> Result<crate::session::Session> {
    anyhow::ensure!(point.session_id == current.id, "This checkpoint belongs to another chat");
    if let Some(session) = &point.conversation { return Ok(session.clone()); }
    // Older checkpoints stored only files. Recover the boundary when their
    // prompt label identifies a unique turn; never guess with repeated prompts.
    let label = |text: &str| text.lines().next().unwrap_or("").chars().take(100).collect::<String>();
    let history = current.build_history_from_messages();
    let turns = history.iter().enumerate().filter_map(|(i, item)| match item {
        crate::cli_ui::HistoryItem::UserPrompt(text) if label(text) == point.label => Some(i), _ => None,
    }).collect::<Vec<_>>();
    let messages = current.messages.iter().enumerate().filter_map(|(i, message)| {
        (message.role == "user" && message.content.as_deref().is_some_and(|text| label(text) == point.label)).then_some(i)
    }).collect::<Vec<_>>();
    anyhow::ensure!(turns.len() == 1 && messages.len() == 1,
        "This older checkpoint has no chat snapshot and its prompt cannot be identified uniquely");
    let mut session = current.clone();
    session.history = history[..turns[0]].to_vec();
    session.messages.truncate(messages[0]);
    Ok(session)
}

fn capture_conversation(workspace: &Path, session_id: &str, label: &str, conversation: Option<crate::session::Session>) -> Result<Checkpoint> {
    let conversation = conversation.map(|mut session| {
        session.history.retain(|item| !matches!(item, crate::cli_ui::HistoryItem::ToolLog(text) if text.starts_with("⏳ Queued #")));
        session
    });
    let workspace = workspace.canonicalize()?;
    let entries = scan(&workspace)?;
    let now = chrono::Local::now();
    let point = Checkpoint { format_version: 1, id: now.format("%Y%m%d_%H%M%S_%9f").to_string(),
        created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        label: label.lines().next().unwrap_or("").chars().take(100).collect(), session_id: session_id.into(), conversation, entries };
    validate(&point)?;
    let previous = list(&workspace, session_id)?.into_iter().next();
    let snapshot = files(&workspace, &point);
    fs::create_dir_all(&snapshot)?;
    let result = (|| -> Result<()> {
        for (path, entry) in &point.entries {
            if matches!(entry, Entry::File { .. }) {
                let destination = snapshot.join(path);
                fs::create_dir_all(destination.parent().context("Missing snapshot parent")?)?;
                // Snapshot hardlinks only point at other immutable snapshots, never at live files.
                let linked = if let Some(previous) = &previous {
                    let old = files(&workspace, previous).join(path);
                    matches!(previous.entries.get(path), Some(Entry::File { .. }))
                        && fs::symlink_metadata(&old).is_ok_and(|metadata| metadata.is_file())
                        && same_contents(&workspace.join(path), &old).unwrap_or(false) && fs::hard_link(&old, &destination).is_ok()
                } else { false };
                if !linked { fs::copy(workspace.join(path), &destination)?; }
            }
        }
        let metadata = directory(&workspace).join(format!("{}.json", point.id));
        let temporary = metadata.with_extension("tmp");
        fs::File::create(&temporary)?.write_all(&serde_json::to_vec_pretty(&point)?)?;
        fs::rename(temporary, metadata)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(directory(&workspace).join(&point.id));
        return Err(error);
    }
    Ok(point)
}

pub fn list(workspace: &Path, session_id: &str) -> Result<Vec<Checkpoint>> {
    let mut points = Vec::new();
    if !directory(workspace).exists() { return Ok(points); }
    for item in fs::read_dir(directory(workspace))? {
        let path = item?.path();
        if path.extension().is_some_and(|extension| extension == "json") {
            let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
            // Legacy Git checkpoints are not filesystem snapshots.
            if value.get("format_version").is_none() || value.get("session_id").and_then(|id| id.as_str()) != Some(session_id) { continue; }
            let point: Checkpoint = serde_json::from_value(value)?;
            validate(&point)?;
            points.push(point);
        }
    }
    points.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(points)
}

fn changed(workspace: &Path, point: &Checkpoint, current: &BTreeMap<PathBuf, Entry>, path: &Path) -> Result<bool> {
    if current.get(path) != point.entries.get(path) { return Ok(true); }
    if matches!(current.get(path), Some(Entry::File { .. })) {
        return Ok(!same_contents(&workspace.join(path), &files(workspace, point).join(path))?);
    }
    Ok(false)
}
fn protect_excluded(workspace: &Path, point: &Checkpoint) -> Result<()> {
    for (path, entry) in &point.entries {
        if matches!(entry, Entry::Directory) { continue; }
        let absolute = workspace.join(path);
        if fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.is_dir()) {
            let mut pending = vec![path.clone()];
            while let Some(parent) = pending.pop() {
                for item in fs::read_dir(workspace.join(parent))? {
                    let item = item?;
                    let relative = item.path().strip_prefix(workspace)?.to_path_buf();
                    anyhow::ensure!(!excluded(&relative), "Restore would replace an excluded directory: {}. Move it aside first", relative.display());
                    if item.file_type()?.is_dir() { pending.push(relative); }
                }
            }
        }
    }
    Ok(())
}

pub fn preview(workspace: &Path, point: &Checkpoint) -> Result<Vec<String>> {
    validate(point)?;
    protect_excluded(workspace, point)?;
    let current = scan(workspace)?;
    let paths = current.keys().chain(point.entries.keys()).cloned().collect::<std::collections::BTreeSet<_>>();
    let mut lines = Vec::new();
    for path in paths {
        if changed(workspace, point, &current, &path)? {
            let status = if !point.entries.contains_key(&path) { "Remove" } else if !current.contains_key(&path) { "Recover" } else { "Restore" };
            lines.push(format!("{status:<7} {}", path.display()));
        }
    }
    Ok(lines)
}

#[cfg(test)]
fn restore(workspace: &Path, point: &Checkpoint, session_id: &str) -> Result<Checkpoint> {
    restore_conversation(workspace, point, session_id, None)
}

pub fn restore_session(workspace: &Path, point: &Checkpoint, session: &crate::session::Session) -> Result<Checkpoint> {
    restore_conversation(workspace, point, &session.id, Some(session.clone()))
}

fn restore_conversation(workspace: &Path, point: &Checkpoint, session_id: &str, conversation: Option<crate::session::Session>) -> Result<Checkpoint> {
    let workspace = workspace.canonicalize()?;
    anyhow::ensure!(point.session_id == session_id, "This checkpoint belongs to another chat");
    validate(point)?;
    protect_excluded(&workspace, point)?;
    // Check every snapshot file before changing anything in the workspace.
    for (path, entry) in &point.entries {
        if matches!(entry, Entry::File { .. }) {
            anyhow::ensure!(fs::symlink_metadata(files(&workspace, point).join(path))?.is_file(), "Missing checkpoint data: {}", path.display());
        }
    }
    let backup = capture_conversation(&workspace, session_id, &format!("Before restore {}", point.id), conversation)?;
    let result = restore_entries(&workspace, point, &backup.entries);
    if let Err(error) = result {
        // Keep the previous workspace intact when a filesystem write fails partway through.
        let rollback = scan(&workspace).and_then(|current| restore_entries(&workspace, &backup, &current));
        anyhow::bail!("Restore failed: {error:#}; {} (safety checkpoint {})",
            if rollback.is_ok() { "previous files recovered" } else { "automatic recovery failed" }, backup.id);
    }
    Ok(backup)
}
fn restore_entries(workspace: &Path, point: &Checkpoint, current: &BTreeMap<PathBuf, Entry>) -> Result<()> {
    let mut paths = current.keys().collect::<Vec<_>>();
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in paths {
        if current.get(path) == point.entries.get(path) { continue; }
        let absolute = workspace.join(path);
        if matches!(current.get(path), Some(Entry::Directory)) {
            // Added folders containing excluded build outputs are left in place.
            if let Err(error) = fs::remove_dir(&absolute) {
                if point.entries.contains_key(path) { return Err(error.into()); }
            }
        } else { fs::remove_file(absolute)?; }
    }
    // BTree ordering ensures parents exist before files are restored.
    for (path, entry) in &point.entries {
        let absolute = workspace.join(path);
        match entry {
            Entry::Directory => fs::create_dir_all(absolute)?,
            Entry::File { mode, readonly } => {
                if !changed(workspace, point, current, path)? { continue; }
                fs::create_dir_all(absolute.parent().context("Missing restore parent")?)?;
                // Copy through a fresh file rather than writing through a live hardlink.
                if fs::symlink_metadata(&absolute).is_ok() { fs::remove_file(&absolute)?; }
                fs::copy(files(workspace, point).join(path), &absolute)?;
                #[cfg(unix)]
                { use std::os::unix::fs::PermissionsExt; fs::set_permissions(absolute, fs::Permissions::from_mode(*mode))?; let _ = readonly; }
                #[cfg(not(unix))]
                { let mut permissions = fs::metadata(&absolute)?.permissions(); permissions.set_readonly(*readonly); fs::set_permissions(absolute, permissions)?; let _ = mode; }
            }
            Entry::Symlink { target } => {
                if current.get(path) == Some(entry) { continue; }
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, absolute)?;
                #[cfg(windows)]
                if workspace.join(path.parent().unwrap_or(Path::new(""))).join(target).is_dir() {
                    std::os::windows::fs::symlink_dir(target, absolute)?;
                } else { std::os::windows::fs::symlink_file(target, absolute)?; }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cli_ui::HistoryItem, llm::ChatMessage, session::Session};
    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("takiza-files-checkpoint-{}-{}", std::process::id(), chrono::Local::now().timestamp_nanos_opt().unwrap()));
            fs::create_dir_all(&path).unwrap(); Self(path)
        }
        fn write(&self, path: &str, data: &[u8]) { fs::write(self.0.join(path), data).unwrap(); }
    }
    impl Drop for Workspace { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    fn message(role: &str, text: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: Some(text.into()), image_urls: Vec::new(), tool_calls: None, tool_call_id: None, name: None }
    }

    #[test]
    fn checkpoints_restore_chat_context_and_keep_an_undo_snapshot_after_reload() {
        let workspace = Workspace::new();
        workspace.write("file", b"before");
        let mut session = Session::new("test-model".into());
        session.messages = vec![message("user", "earlier prompt"), message("assistant", "earlier answer")];
        session.history = vec![HistoryItem::UserPrompt("earlier prompt".into()), HistoryItem::AssistantMessage("earlier answer".into())];
        let point = capture_session(&workspace.0, &session, "later prompt").unwrap();
        session.messages.extend([message("user", "later prompt"), message("assistant", "removed answer")]);
        session.history.extend([HistoryItem::UserPrompt("later prompt".into()), HistoryItem::Thought("removed reasoning".into()),
            HistoryItem::ToolLog("removed tool output".into()), HistoryItem::AssistantMessage("removed answer".into()),
            HistoryItem::ToolLog("⏳ Queued #1: stale prompt".into())]);
        workspace.write("file", b"after");
        let point = list(&workspace.0, &session.id).unwrap().into_iter().find(|p| p.id == point.id).unwrap();
        let restored = conversation_at(&point, &session).unwrap();
        let backup = restore_session(&workspace.0, &point, &session).unwrap();
        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.history.len(), 2);
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"before");
        restored.save(&workspace.0).unwrap();
        let reloaded = Session::load(&workspace.0, &session.id).unwrap();
        let encoded = serde_json::to_string(&reloaded).unwrap();
        assert!(!encoded.contains("removed") && !encoded.contains("later prompt"));
        let backup = list(&workspace.0, &session.id).unwrap().into_iter().find(|p| p.id == backup.id).unwrap();
        let undone = conversation_at(&backup, &restored).unwrap();
        restore_session(&workspace.0, &backup, &restored).unwrap();
        assert_eq!(undone.messages.len(), 4);
        assert!(serde_json::to_string(&undone.history).unwrap().contains("removed answer"));
        assert!(!serde_json::to_string(&undone.history).unwrap().contains("Queued"));
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"after");
    }

    #[test]
    fn chat_can_be_restored_when_files_have_not_changed_and_legacy_prompts_must_be_unique() {
        let workspace = Workspace::new();
        workspace.write("file", b"unchanged");
        let mut session = Session::new("test-model".into());
        let point = capture_session(&workspace.0, &session, "prompt").unwrap();
        session.messages = vec![message("user", "prompt"), message("assistant", "answer")];
        session.history = vec![HistoryItem::UserPrompt("prompt".into()), HistoryItem::AssistantMessage("answer".into())];
        assert!(preview(&workspace.0, &point).unwrap().is_empty());
        restore_session(&workspace.0, &point, &session).unwrap();
        assert!(conversation_at(&point, &session).unwrap().history.is_empty());
        let mut legacy = point.clone();
        legacy.conversation = None;
        assert!(conversation_at(&legacy, &session).unwrap().messages.is_empty());
        session.history.push(HistoryItem::UserPrompt("prompt".into()));
        session.messages.push(message("user", "prompt"));
        assert!(conversation_at(&legacy, &session).is_err());
    }
    #[test]
    fn restores_dirty_binary_deleted_and_added_files_without_git_and_can_undo() {
        let workspace = Workspace::new(); workspace.write("existing", b"user changes"); workspace.write("deleted", &[0, 1, 255]);
        fs::create_dir(workspace.0.join("empty")).unwrap();
        let point = capture(&workspace.0, "session", "first prompt").unwrap();
        workspace.write("existing", b"agent edit"); fs::remove_file(workspace.0.join("deleted")).unwrap();
        fs::remove_dir(workspace.0.join("empty")).unwrap(); workspace.write("added", b"new");
        let changes = preview(&workspace.0, &point).unwrap().join("\n");
        assert!(changes.contains("Remove  added") && changes.contains("Recover deleted"));
        let backup = restore(&workspace.0, &point, "session").unwrap();
        assert_eq!(fs::read(workspace.0.join("existing")).unwrap(), b"user changes");
        assert_eq!(fs::read(workspace.0.join("deleted")).unwrap(), [0, 1, 255]);
        assert!(!workspace.0.join("added").exists() && workspace.0.join("empty").is_dir());
        restore(&workspace.0, &backup, "session").unwrap();
        assert_eq!(fs::read(workspace.0.join("existing")).unwrap(), b"agent edit");
        assert!(workspace.0.join("added").exists() && !workspace.0.join("deleted").exists());
        assert!(list(&workspace.0, "session").unwrap().iter().any(|saved| saved.id == point.id));
    }
    #[test]
    fn checkpoints_are_isolated_by_chat_and_empty_chat_has_none() {
        let workspace = Workspace::new(); workspace.write("file", b"first state");
        let first = capture(&workspace.0, "chat-a", "first prompt").unwrap();
        workspace.write("file", b"second state");
        let second = capture(&workspace.0, "chat-a", "second prompt").unwrap();
        let other = capture(&workspace.0, "chat-b", "another chat prompt").unwrap();
        assert_eq!(list(&workspace.0, "chat-a").unwrap().iter().map(|point| point.id.clone()).collect::<Vec<_>>(), vec![second.id, first.id.clone()]);
        assert_eq!(list(&workspace.0, "chat-b").unwrap()[0].id, other.id);
        assert!(list(&workspace.0, "new-empty-chat").unwrap().is_empty());
        assert!(restore(&workspace.0, &first, "chat-b").is_err());
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"second state");
    }
    #[test]
    fn keeps_metadata_build_outputs_and_git_untouched() {
        let workspace = Workspace::new(); workspace.write("file", b"before");
        let point = capture(&workspace.0, "session", "prompt").unwrap();
        for directory in ["target", ".git", ".takiza"] {
            fs::create_dir_all(workspace.0.join(directory)).unwrap();
            workspace.write(&format!("{directory}/keep"), b"keep");
        }
        workspace.write("file", b"after"); restore(&workspace.0, &point, "session").unwrap();
        for directory in ["target", ".git", ".takiza"] { assert_eq!(fs::read(workspace.0.join(directory).join("keep")).unwrap(), b"keep"); }
    }
    #[test]
    fn snapshot_cannot_escape_workspace_or_replace_excluded_directory() {
        let workspace = Workspace::new(); workspace.write("file", b"before");
        let mut point = capture(&workspace.0, "session", "prompt").unwrap();
        point.entries.insert(PathBuf::from("../escape"), Entry::Directory);
        assert!(restore(&workspace.0, &point, "session").is_err());
        point.entries.remove(Path::new("../escape"));
        fs::remove_file(workspace.0.join("file")).unwrap(); fs::create_dir(workspace.0.join("file")).unwrap();
        fs::create_dir(workspace.0.join("file/target")).unwrap(); workspace.write("file/target/keep", b"keep");
        assert!(restore(&workspace.0, &point, "session").is_err());
        assert_eq!(fs::read(workspace.0.join("file/target/keep")).unwrap(), b"keep");
    }
    #[test]
    fn restores_file_directory_replacements_and_never_hardlinks_live_data() {
        let workspace = Workspace::new(); workspace.write("file", b"original");
        let point = capture(&workspace.0, "session", "prompt").unwrap();
        let other = capture(&workspace.0, "session", "next prompt").unwrap();
        workspace.write("file", b"modified");
        assert_eq!(fs::read(files(&workspace.0, &other).join("file")).unwrap(), b"original");
        fs::remove_file(workspace.0.join("file")).unwrap(); fs::create_dir(workspace.0.join("file")).unwrap();
        workspace.write("file/child", b"new child");
        let backup = restore(&workspace.0, &point, "session").unwrap();
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"original");
        restore(&workspace.0, &backup, "session").unwrap();
        assert_eq!(fs::read(workspace.0.join("file/child")).unwrap(), b"new child");
    }
    #[test]
    fn missing_checkpoint_file_leaves_workspace_untouched() {
        let workspace = Workspace::new(); workspace.write("file", b"original");
        let point = capture(&workspace.0, "session", "prompt").unwrap();
        workspace.write("file", b"current");
        fs::remove_file(files(&workspace.0, &point).join("file")).unwrap();
        assert!(restore(&workspace.0, &point, "session").is_err());
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"current");
    }
    #[cfg(unix)]
    #[test]
    fn restores_symlinks_and_permissions_without_following_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let workspace = Workspace::new(); workspace.write("file", b"before");
        symlink("file", workspace.0.join("link")).unwrap();
        fs::set_permissions(workspace.0.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
        let point = capture(&workspace.0, "session", "prompt").unwrap();
        fs::remove_file(workspace.0.join("link")).unwrap(); symlink("other", workspace.0.join("link")).unwrap();
        fs::set_permissions(workspace.0.join("file"), fs::Permissions::from_mode(0o644)).unwrap();
        restore(&workspace.0, &point, "session").unwrap();
        assert_eq!(fs::read_link(workspace.0.join("link")).unwrap(), PathBuf::from("file"));
        assert_ne!(fs::metadata(workspace.0.join("file")).unwrap().permissions().mode() & 0o111, 0);
    }
}
