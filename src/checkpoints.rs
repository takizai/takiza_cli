//! Workspace checkpoints: committed Git blobs plus local file copies, or a
//! standalone filesystem snapshot when Git is unavailable.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::{Read, Write}, path::{Component, Path, PathBuf}};

struct CaptureControl {
    cancel: tokio_util::sync::CancellationToken,
}

impl CaptureControl {
    fn check(&self) -> Result<()> {
        anyhow::ensure!(!self.cancel.is_cancelled(), "Checkpoint cancelled");
        Ok(())
    }
}

fn check_capture(control: Option<&CaptureControl>) -> Result<()> {
    if let Some(control) = control { control.check()?; }
    Ok(())
}

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
    #[serde(default)]
    pub prompt: Option<String>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<crate::session::Session>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stored_conversation: Option<crate::checkpoint_store::Conversation>,
    #[serde(skip)]
    stored_workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_commit: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    git_files: BTreeMap<PathBuf, String>,
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
    anyhow::ensure!(matches!(point.format_version, 1 | 2 | 3) && point.id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'_') && !point.id.is_empty(), "Invalid checkpoint metadata");
    if let Some(commit) = &point.git_commit {
        anyhow::ensure!(crate::checkpoint_git::valid_oid(commit), "Invalid Git checkpoint commit");
    }
    for (path, hash) in &point.git_files {
        anyhow::ensure!(point.git_commit.is_some() && matches!(point.entries.get(path), Some(Entry::File { .. }))
            && crate::checkpoint_git::valid_oid(hash), "Invalid Git checkpoint file");
    }
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
    scan_controlled(workspace, None)
}

// A terminal opened from the Windows Start menu commonly starts in USERPROFILE.
// Snapshotting that directory reaches protected AppData and registry files.
// Reject the whole snapshot rather than silently omitting unreadable files,
// which would make a later restore unsafe.
#[cfg(any(windows, test))]
fn ensure_project_directory(workspace: &Path, profile: Option<&Path>) -> Result<()> {
    if let Some(profile) = profile.and_then(|path| path.canonicalize().ok()) {
        anyhow::ensure!(workspace.canonicalize()? != profile, "{}", crate::i18n::tr(
            "Checkpoints are disabled in the Windows user profile root. Start Takiza from a project directory to enable file recovery."));
    }
    Ok(())
}

fn scan_controlled(workspace: &Path, control: Option<&CaptureControl>) -> Result<BTreeMap<PathBuf, Entry>> {
    #[cfg(windows)]
    ensure_project_directory(workspace, std::env::var_os("USERPROFILE").as_deref().map(Path::new))?;
    let mut entries = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(parent) = pending.pop() {
        check_capture(control)?;
        for item in fs::read_dir(workspace.join(&parent)).with_context(|| format!("Cannot read {}", parent.display()))? {
            check_capture(control)?;
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
    same_contents_controlled(a, b, None)
}

fn same_contents_controlled(a: &Path, b: &Path, control: Option<&CaptureControl>) -> Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() { return Ok(false); }
    let mut a = fs::File::open(a)?;
    let mut b = fs::File::open(b)?;
    let mut left = [0u8; 65536];
    let mut right = [0u8; 65536];
    loop {
        check_capture(control)?;
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

#[cfg(test)]
pub fn capture_session(workspace: &Path, session: &crate::session::Session, label: &str) -> Result<Checkpoint> {
    capture_controlled(workspace, &session.id, label, Some(session.clone()), Some(label.into()), None)
}

pub fn capture_session_cancellable(workspace: &Path, session: &crate::session::Session, label: &str,
    cancel: tokio_util::sync::CancellationToken) -> Result<Checkpoint> {
    let control = CaptureControl { cancel };
    capture_controlled(workspace, &session.id, label, Some(session.clone()), Some(label.into()), Some(&control))
}

fn copy_controlled(source: &Path, destination: &Path, control: Option<&CaptureControl>) -> Result<()> {
    if control.is_none() { fs::copy(source, destination)?; return Ok(()); }
    let mut input = fs::File::open(source)?;
    let permissions = input.metadata()?.permissions();
    let mut output = fs::File::create(destination)?;
    let mut bytes = [0u8; 65536];
    loop {
        check_capture(control)?;
        let count = input.read(&mut bytes)?;
        if count == 0 { break; }
        output.write_all(&bytes[..count])?;
    }
    output.set_permissions(permissions)?;
    Ok(())
}

pub fn conversation_at(point: &Checkpoint, current: &crate::session::Session) -> Result<crate::session::Session> {
    anyhow::ensure!(point.session_id == current.id, "This checkpoint belongs to another chat");
    if let Some(session) = &point.conversation { return Ok(session.clone()); }
    if let Some(stored) = &point.stored_conversation {
        return stored.load(point.stored_workspace.as_deref().context("Missing checkpoint workspace")?, &point.session_id);
    }
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

/// Older snapshots store only a shortened label. Use their chat boundary when
/// available, and recover a full prompt only when it can be identified safely.
pub fn prompt_at(point: &Checkpoint, current: &crate::session::Session) -> Option<String> {
    if let Some(prompt) = &point.prompt { return Some(prompt.clone()); }
    let matches_label = |text: &str| text.lines().next().unwrap_or("").chars().take(100).collect::<String>() == point.label;
    let history = current.build_history_from_messages();
    let stored = point.stored_conversation.as_ref().and_then(|stored|
        stored.load(point.stored_workspace.as_deref()?, &point.session_id).ok());
    if let Some(saved) = point.conversation.as_ref().or(stored.as_ref()) {
        let boundary = saved.build_history_from_messages().len();
        if serde_json::to_value(history.get(..boundary)?).ok()? != serde_json::to_value(saved.build_history_from_messages()).ok()? {
            return None;
        }
        return match history.get(boundary) {
            Some(crate::cli_ui::HistoryItem::UserPrompt(text)) if matches_label(text) => Some(text.clone()),
            _ => None,
        };
    }
    let mut prompts = history.iter().filter_map(|item| match item {
        crate::cli_ui::HistoryItem::UserPrompt(text) if matches_label(text) => Some(text.clone()),
        _ => None,
    });
    let prompt = prompts.next()?;
    prompts.next().is_none().then_some(prompt)
}

fn capture_conversation(workspace: &Path, session_id: &str, label: &str, conversation: Option<crate::session::Session>) -> Result<Checkpoint> {
    capture_controlled(workspace, session_id, label, conversation, None, None)
}

fn capture_controlled(workspace: &Path, session_id: &str, label: &str, conversation: Option<crate::session::Session>,
    prompt: Option<String>, control: Option<&CaptureControl>) -> Result<Checkpoint> {
    let conversation = conversation.map(|mut session| {
        session.history.retain(|item| !matches!(item, crate::cli_ui::HistoryItem::ToolLog(text) if text.starts_with("⏳ Queued #")));
        session
    });
    let workspace = workspace.canonicalize()?;
    let entries = scan_controlled(&workspace, control)?;
    let now = chrono::Local::now();
    let mut point = Checkpoint { format_version: 1, id: now.format("%Y%m%d_%H%M%S_%9f").to_string(),
        created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        label: label.lines().next().unwrap_or("").chars().take(100).collect(), prompt, session_id: session_id.into(), conversation, stored_conversation: None, stored_workspace: None,
        git_commit: None, git_files: BTreeMap::new(), entries };
    validate(&point)?;
    let mut existing = metadata(&workspace, None)?;
    // Migrate embedded transcripts atomically; old data stays readable until
    // every referenced object has been written successfully.
    for saved in existing.iter_mut().filter(|saved| saved.conversation.is_some()) {
        check_capture(control)?;
        write_metadata(&workspace, saved)?;
        saved.conversation = None;
    }
    // Files can be shared across chats while their restore menus stay isolated.
    let previous = existing.into_iter().next();
    let paths = point.entries.iter().filter_map(|(path, entry)| matches!(entry, Entry::File { .. }).then_some(path.clone()));
    let cancel = control.map(|control| &control.cancel);
    if let Ok(baseline) = crate::checkpoint_git::baseline(&workspace, paths, cancel) {
        if !baseline.files.is_empty() {
            if crate::checkpoint_git::pin(&workspace, &point.id, &baseline.commit, cancel).is_ok() {
                point.format_version = 3;
                point.git_commit = Some(baseline.commit);
                point.git_files = baseline.files;
            } else {
                // A cancelled update-ref may have already published its ref.
                let _ = crate::checkpoint_git::release(&workspace, &point.id, &baseline.commit);
            }
        }
    }
    let snapshot = files(&workspace, &point);
    let result = (|| -> Result<()> {
        check_capture(control)?;
        fs::create_dir_all(&snapshot)?;
        for (path, entry) in &point.entries {
            check_capture(control)?;
            if matches!(entry, Entry::File { .. }) {
                if point.git_files.contains_key(path) { continue; }
                let destination = snapshot.join(path);
                fs::create_dir_all(destination.parent().context("Missing snapshot parent")?)?;
                // Snapshot hardlinks only point at other immutable snapshots, never at live files.
                let linked = if let Some(previous) = &previous {
                    let old = files(&workspace, previous).join(path);
                    matches!(previous.entries.get(path), Some(Entry::File { .. }))
                        && fs::symlink_metadata(&old).is_ok_and(|metadata| metadata.is_file())
                        && same_contents_controlled(&workspace.join(path), &old, control)? && fs::hard_link(&old, &destination).is_ok()
                } else { false };
                if !linked { copy_controlled(&workspace.join(path), &destination, control)?; }
            }
        }
        check_capture(control)?;
        write_metadata(&workspace, &point)?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Some(commit) = &point.git_commit { let _ = crate::checkpoint_git::release(&workspace, &point.id, commit); }
        let _ = fs::remove_dir_all(directory(&workspace).join(&point.id));
        let _ = fs::remove_file(directory(&workspace).join(format!("{}.tmp", point.id)));
        return Err(error);
    }
    Ok(point)
}

fn write_metadata(workspace: &Path, point: &Checkpoint) -> Result<()> {
    let mut stored = point.clone();
    stored.format_version = if stored.git_commit.is_some() { 3 } else { 2 };
    if let Some(conversation) = stored.conversation.take() {
        stored.stored_conversation = Some(crate::checkpoint_store::Conversation::save(workspace, &conversation)?);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory(workspace))?;
    serde_json::to_writer(temporary.as_file_mut(), &stored)?;
    temporary.persist(directory(workspace).join(format!("{}.json", point.id)))?;
    Ok(())
}

fn metadata(workspace: &Path, session_id: Option<&str>) -> Result<Vec<Checkpoint>> {
    let mut points = Vec::new();
    if !directory(workspace).exists() { return Ok(points); }
    for item in fs::read_dir(directory(workspace))? {
        let path = item?.path();
        if path.extension().is_some_and(|extension| extension == "json") {
            let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
            // Legacy Git checkpoints are not filesystem snapshots.
            if value.get("format_version").is_none() || session_id.is_some_and(|id| value.get("session_id").and_then(|id| id.as_str()) != Some(id)) { continue; }
            let point: Checkpoint = serde_json::from_value(value)?;
            validate(&point)?;
            points.push(point);
        }
    }
    points.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(points)
}

pub fn list(workspace: &Path, session_id: &str) -> Result<Vec<Checkpoint>> {
    let mut points = metadata(workspace, Some(session_id))?;
    for point in &mut points {
        // Opening the menu must not expand every historical transcript in RAM.
        point.stored_workspace = Some(workspace.to_path_buf());
    }
    Ok(points)
}

pub fn remove_session(workspace: &Path, session_id: &str) -> Result<()> {
    for point in metadata(workspace, Some(session_id))? {
        if let Some(commit) = &point.git_commit { crate::checkpoint_git::release(workspace, &point.id, commit)?; }
        fs::remove_file(directory(workspace).join(format!("{}.json", point.id)))?;
        let path = directory(workspace).join(&point.id);
        if path.exists() { fs::remove_dir_all(path)?; }
    }
    crate::checkpoint_store::remove_session(workspace, session_id)
}

struct SnapshotFiles<'a> {
    workspace: &'a Path,
    point: &'a Checkpoint,
    git: Option<crate::checkpoint_git::Objects>,
    git_sizes: Option<BTreeMap<String, u64>>,
}

impl<'a> SnapshotFiles<'a> {
    fn new(workspace: &'a Path, point: &'a Checkpoint) -> Self { Self { workspace, point, git: None, git_sizes: None } }

    fn git(&mut self) -> Result<&mut crate::checkpoint_git::Objects> {
        if self.git.is_none() { self.git = Some(crate::checkpoint_git::Objects::open(self.workspace)?); }
        self.git.as_mut().context("Missing Git object reader")
    }

    fn same(&mut self, path: &Path) -> Result<bool> {
        if let Some(hash) = self.point.git_files.get(path).cloned() {
            let live = self.workspace.join(path);
            if self.git_sizes.is_none() {
                self.git_sizes = Some(crate::checkpoint_git::verify(self.workspace, self.point.git_files.values().cloned())?);
            }
            let size = self.git_sizes.as_ref().and_then(|sizes| sizes.get(&hash)).context("Missing Git blob size")?;
            if fs::metadata(&live)?.len() != *size { return Ok(false); }
            self.git()?.blob(&hash, |input, size| {
                if fs::metadata(&live)?.len() != size { return Ok(false); }
                let mut live = fs::File::open(live)?;
                let (mut left, mut right) = ([0u8; 65536], [0u8; 65536]);
                loop {
                    let count = input.read(&mut left)?;
                    if count == 0 { return Ok(live.read(&mut right[..1])? == 0); }
                    live.read_exact(&mut right[..count])?;
                    if left[..count] != right[..count] { return Ok(false); }
                }
            })
        } else { same_contents(&self.workspace.join(path), &files(self.workspace, self.point).join(path)) }
    }

    fn copy(&mut self, path: &Path, destination: &Path) -> Result<()> {
        if let Some(hash) = self.point.git_files.get(path).cloned() {
            self.git()?.blob(&hash, |input, size| {
                let mut output = fs::File::create(destination)?;
                anyhow::ensure!(std::io::copy(input, &mut output)? == size, "Truncated Git checkpoint data");
                Ok(())
            })
        } else { fs::copy(files(self.workspace, self.point).join(path), destination)?; Ok(()) }
    }
}

fn changed(point: &Checkpoint, current: &BTreeMap<PathBuf, Entry>, path: &Path, data: &mut SnapshotFiles<'_>) -> Result<bool> {
    if current.get(path) != point.entries.get(path) { return Ok(true); }
    if matches!(current.get(path), Some(Entry::File { .. })) {
        return Ok(!data.same(path)?);
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
    let mut data = SnapshotFiles::new(workspace, point);
    for path in paths {
        if changed(point, &current, &path, &mut data)? {
            let status = if !point.entries.contains_key(&path) { "Remove" } else if !current.contains_key(&path) { "Recover" } else { "Restore" };
            let status = crate::i18n::tr(status);
            let padding = " ".repeat(7usize.saturating_sub(crate::markdown::visible_width(status)));
            lines.push(format!("{status}{padding} {}", path.display()));
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
        if matches!(entry, Entry::File { .. }) && !point.git_files.contains_key(path) {
            anyhow::ensure!(fs::symlink_metadata(files(&workspace, point).join(path))?.is_file(), "Missing checkpoint data: {}", path.display());
        }
    }
    crate::checkpoint_git::verify(&workspace, point.git_files.values().cloned())
        .context("Cannot read Git data required by this checkpoint; workspace left unchanged")?;
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
    let mut data = SnapshotFiles::new(workspace, point);
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
                if !changed(point, current, path, &mut data)? { continue; }
                fs::create_dir_all(absolute.parent().context("Missing restore parent")?)?;
                // Copy through a fresh file rather than writing through a live hardlink.
                if fs::symlink_metadata(&absolute).is_ok() { fs::remove_file(&absolute)?; }
                data.copy(path, &absolute)?;
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

    #[test]
    fn profile_root_is_rejected_but_project_directories_are_allowed() {
        let profile = Workspace::new();
        fs::create_dir_all(profile.0.join("AppData/Local/Microsoft/Edge")).unwrap();
        fs::create_dir_all(profile.0.join("project")).unwrap();
        let error = ensure_project_directory(&profile.0, Some(&profile.0)).unwrap_err();
        assert!(error.to_string().contains("project"));
        assert!(!directory(&profile.0).exists());
        ensure_project_directory(&profile.0.join("."), Some(&profile.0)).unwrap_err();
        ensure_project_directory(&profile.0.join("project"), Some(&profile.0)).unwrap();
        ensure_project_directory(&profile.0, None).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_profile_is_rejected_before_scanning_even_when_cancelled() {
        let profile = PathBuf::from(std::env::var_os("USERPROFILE").expect("Windows user profile"));
        // Cancellation also prevents traversal if the profile guard regresses.
        let control = CaptureControl { cancel: tokio_util::sync::CancellationToken::new() };
        control.cancel.cancel();
        let error = scan_controlled(&profile, Some(&control)).unwrap_err();
        assert!(error.to_string().contains("project"));
    }

    #[test]
    fn capture_continues_after_ten_seconds_and_can_still_be_cancelled() {
        let workspace = Workspace::new();
        workspace.write("file", b"before");
        let session = crate::session::Session::new("test-model".into());
        let control = CaptureControl { cancel: tokio_util::sync::CancellationToken::new() };
        std::thread::sleep(std::time::Duration::from_millis(10_050));
        let point = capture_controlled(&workspace.0, &session.id, "prompt", Some(session.clone()),
            Some("prompt".into()), Some(&control)).unwrap();
        assert_eq!(fs::read(files(&workspace.0, &point).join("file")).unwrap(), b"before");
        control.cancel.cancel();
        assert!(capture_controlled(&workspace.0, &session.id, "next", Some(session.clone()),
            Some("next".into()), Some(&control)).is_err());
        assert_eq!(list(&workspace.0, &session.id).unwrap().len(), 1);
    }

    #[test]
    fn rewind_recovers_full_prompts_and_keeps_safety_snapshots_without_a_prompt() {
        let workspace = Workspace::new();
        let mut session = crate::session::Session::new("test-model".into());
        let prompt = format!("{}\nвторая строка 🦀", "длинный запрос ".repeat(12));
        capture_session(&workspace.0, &session, &prompt).unwrap();
        let point = list(&workspace.0, &session.id).unwrap().remove(0);
        assert_eq!(prompt_at(&point, &session).as_deref(), Some(prompt.as_str()));
        assert_eq!(point.label.chars().count(), 100);
        session.history = vec![crate::cli_ui::HistoryItem::UserPrompt(prompt.clone())];
        let mut old_json = serde_json::to_value(&point).unwrap();
        old_json.as_object_mut().unwrap().remove("prompt");
        old_json.as_object_mut().unwrap().remove("stored_conversation");
        old_json["conversation"] = serde_json::to_value(conversation_at(&point, &session).unwrap()).unwrap();
        let mut old: Checkpoint = serde_json::from_value(old_json).unwrap();
        assert_eq!(prompt_at(&old, &session).as_deref(), Some(prompt.as_str()));
        old.conversation = None;
        assert_eq!(prompt_at(&old, &session).as_deref(), Some(prompt.as_str()));
        session.history.push(crate::cli_ui::HistoryItem::UserPrompt(prompt.clone()));
        assert!(prompt_at(&old, &session).is_none());
        assert_eq!(prompt_at(&point, &session).as_deref(), Some(prompt.as_str()));
        let backup = restore_session(&workspace.0, &point, &session).unwrap();
        assert!(prompt_at(&backup, &session).is_none());
    }

    #[test]
    fn cancelled_capture_never_publishes_a_restore_point() {
        let workspace = Workspace::new();
        workspace.write("file", b"must stay unchanged");
        let session = crate::session::Session::new("test-model".into());
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        assert!(capture_session_cancellable(&workspace.0, &session, "prompt", cancel).is_err());
        assert!(list(&workspace.0, &session.id).unwrap().is_empty());
        assert!(!directory(&workspace.0).exists());
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"must stay unchanged");
    }

    #[test]
    fn cancellable_capture_preserves_files_and_can_be_restored() {
        let workspace = Workspace::new();
        workspace.write("file", b"before");
        let session = crate::session::Session::new("test-model".into());
        let point = capture_session_cancellable(&workspace.0, &session, "prompt",
            tokio_util::sync::CancellationToken::new()).unwrap();
        let next = capture_session_cancellable(&workspace.0, &session, "next prompt",
            tokio_util::sync::CancellationToken::new()).unwrap();
        workspace.write("file", b"after");
        assert_eq!(fs::read(files(&workspace.0, &next).join("file")).unwrap(), b"before");
        restore_session(&workspace.0, &point, &session).unwrap();
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"before");
    }
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
        ChatMessage { provider_data: None, role: role.into(), content: Some(text.into()), image_urls: Vec::new(), tool_calls: None, tool_call_id: None, name: None }
    }

    fn git(workspace: &Path, args: &[&str]) -> Vec<u8> {
        let result = crate::git::create_git_command().current_dir(workspace)
            .args(["-c", "user.name=Checkpoint Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args).output().unwrap();
        assert!(result.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&result.stderr));
        result.stdout
    }

    fn init_git(workspace: &Workspace) {
        git(&workspace.0, &["init", "-q"]);
        git(&workspace.0, &["config", "core.autocrlf", "false"]);
        workspace.write(".gitignore", b".takiza/\nignored.bin\n");
    }

    #[test]
    fn git_snapshots_reuse_committed_files_and_restore_dirty_staged_and_untracked_bytes() {
        let workspace = Workspace::new();
        init_git(&workspace);
        workspace.write("large.bin", &vec![17; 1024 * 1024]);
        for file in ["dirty", "staged", "deleted"] { workspace.write(file, b"committed"); }
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "baseline"]);
        workspace.write("dirty", b"dirty before prompt");
        workspace.write("staged", b"staged contents");
        git(&workspace.0, &["add", "staged"]);
        workspace.write("staged", b"working contents beyond index");
        fs::remove_file(workspace.0.join("deleted")).unwrap();
        workspace.write("new", b"untracked before prompt");
        workspace.write("ignored.bin", b"ignored before prompt");
        let head = git(&workspace.0, &["rev-parse", "HEAD"]);
        let index = git(&workspace.0, &["ls-files", "--stage", "-z"]);
        let first = capture(&workspace.0, "git-chat", "prompt").unwrap();
        assert_eq!(first.git_commit.as_deref(), Some(std::str::from_utf8(&head).unwrap().trim()));
        assert!(first.git_files.contains_key(Path::new("large.bin")));
        assert!(!files(&workspace.0, &first).join("large.bin").exists());
        for file in ["dirty", "staged", "new", "ignored.bin"] {
            assert!(!first.git_files.contains_key(Path::new(file)));
            assert_eq!(fs::read(files(&workspace.0, &first).join(file)).unwrap(), fs::read(workspace.0.join(file)).unwrap());
        }
        assert!(preview(&workspace.0, &first).unwrap().is_empty());
        assert_eq!(git(&workspace.0, &["ls-files", "--stage", "-z"]), index);
        workspace.write("large.bin", b"edited large file");
        workspace.write("staged", b"later staged working contents");
        fs::remove_file(workspace.0.join("dirty")).unwrap();
        workspace.write("new", b"later new file");
        workspace.write("ignored.bin", b"later ignored file");
        workspace.write("deleted", b"recreated later");
        workspace.write("added-later", b"must disappear");
        let point = list(&workspace.0, "git-chat").unwrap().remove(0);
        assert_eq!(point.format_version, 3);
        let backup = restore(&workspace.0, &point, "git-chat").unwrap();
        assert_eq!(fs::read(workspace.0.join("large.bin")).unwrap(), vec![17; 1024 * 1024]);
        assert_eq!(fs::read(workspace.0.join("dirty")).unwrap(), b"dirty before prompt");
        assert_eq!(fs::read(workspace.0.join("staged")).unwrap(), b"working contents beyond index");
        assert_eq!(fs::read(workspace.0.join("new")).unwrap(), b"untracked before prompt");
        assert_eq!(fs::read(workspace.0.join("ignored.bin")).unwrap(), b"ignored before prompt");
        assert!(!workspace.0.join("deleted").exists() && !workspace.0.join("added-later").exists());
        assert_eq!(git(&workspace.0, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&workspace.0, &["ls-files", "--stage", "-z"]), index);
        restore(&workspace.0, &backup, "git-chat").unwrap();
        assert_eq!(fs::read(workspace.0.join("large.bin")).unwrap(), b"edited large file");
        assert!(!workspace.0.join("dirty").exists());
        assert!(workspace.0.join("added-later").exists());
        assert_eq!(git(&workspace.0, &["ls-files", "--stage", "-z"]), index);
    }

    #[test]
    fn git_checkpoint_survives_branch_replacement_and_gc_without_resetting_head_or_index() {
        let workspace = Workspace::new();
        init_git(&workspace);
        workspace.write("file", b"original commit");
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "original"]);
        let branch = git(&workspace.0, &["symbolic-ref", "--short", "HEAD"]);
        let point = capture(&workspace.0, "chat", "before replacement").unwrap();
        assert!(!point.git_files.is_empty());
        git(&workspace.0, &["checkout", "--orphan", "replacement"]);
        workspace.write("file", b"replacement commit");
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "replacement"]);
        git(&workspace.0, &["branch", "-D", std::str::from_utf8(&branch).unwrap().trim()]);
        git(&workspace.0, &["reflog", "expire", "--expire=now", "--all"]);
        git(&workspace.0, &["gc", "--prune=now"]);
        let head = git(&workspace.0, &["rev-parse", "HEAD"]);
        let index = git(&workspace.0, &["ls-files", "--stage", "-z"]);
        let backup = restore(&workspace.0, &point, "chat").unwrap();
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"original commit");
        assert_eq!(git(&workspace.0, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&workspace.0, &["ls-files", "--stage", "-z"]), index);
        restore(&workspace.0, &backup, "chat").unwrap();
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"replacement commit");
        remove_session(&workspace.0, "chat").unwrap();
        assert!(git(&workspace.0, &["for-each-ref", "--format=%(refname)", "refs/takiza/checkpoints/"]).is_empty());
    }

    #[test]
    fn git_checkpoint_missing_repository_fails_before_changing_files() {
        let workspace = Workspace::new();
        init_git(&workspace);
        workspace.write("file", b"committed");
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "baseline"]);
        let point = capture(&workspace.0, "chat", "prompt").unwrap();
        assert!(!point.git_files.is_empty());
        workspace.write("file", b"must remain");
        workspace.write("new", b"must also remain");
        // Only an isolated test repository is removed.
        fs::remove_dir_all(workspace.0.join(".git")).unwrap();
        assert!(restore(&workspace.0, &point, "chat").unwrap_err().to_string().contains("workspace left unchanged"));
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"must remain");
        assert_eq!(fs::read(workspace.0.join("new")).unwrap(), b"must also remain");
        assert_eq!(list(&workspace.0, "chat").unwrap().len(), 1);
        remove_session(&workspace.0, "chat").unwrap();
    }

    #[test]
    fn git_without_commits_uses_standalone_snapshots_that_still_restore_without_git() {
        let workspace = Workspace::new();
        init_git(&workspace);
        workspace.write("file", b"before the first commit");
        let first = capture(&workspace.0, "chat", "prompt").unwrap();
        assert!(first.git_commit.is_none() && first.git_files.is_empty());
        assert_eq!(fs::read(files(&workspace.0, &first).join("file")).unwrap(), b"before the first commit");
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "first commit"]);
        let second = capture(&workspace.0, "chat", "next prompt").unwrap();
        assert!(second.git_commit.is_some());
        let first = list(&workspace.0, "chat").unwrap().into_iter().find(|p| p.id == first.id).unwrap();
        assert_eq!(first.format_version, 2);
        fs::remove_dir_all(workspace.0.join(".git")).unwrap();
        workspace.write("file", b"changed after commit");
        restore(&workspace.0, &first, "chat").unwrap();
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"before the first commit");
    }

    #[test]
    fn git_subdirectory_and_worktree_checkpoints_use_the_right_repository() {
        let workspace = Workspace::new();
        init_git(&workspace);
        fs::create_dir_all(workspace.0.join("project")).unwrap();
        workspace.write("outside", b"outside workspace");
        workspace.write("project/file", b"inside workspace");
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "baseline"]);
        let project = workspace.0.join("project");
        let point = capture(&project, "subdirectory", "prompt").unwrap();
        assert_eq!(point.git_files.len(), 1);
        assert!(point.git_files.contains_key(Path::new("file")));
        assert!(!point.entries.contains_key(Path::new("outside")));
        fs::write(project.join("file"), b"edited").unwrap();
        restore(&project, &point, "subdirectory").unwrap();
        assert_eq!(fs::read(project.join("file")).unwrap(), b"inside workspace");
        assert_eq!(fs::read(workspace.0.join("outside")).unwrap(), b"outside workspace");
        let worktree = Workspace::new();
        git(&workspace.0, &["worktree", "add", "--detach", worktree.0.to_str().unwrap(), "HEAD"]);
        let point = capture(&worktree.0, "worktree", "prompt").unwrap();
        assert!(point.git_files.contains_key(Path::new("project/file")));
        assert!(!files(&worktree.0, &point).join("project/file").exists());
        worktree.write("project/file", b"worktree edit");
        restore(&worktree.0, &point, "worktree").unwrap();
        assert_eq!(fs::read(worktree.0.join("project/file")).unwrap(), b"inside workspace");
    }

    #[cfg(unix)]
    #[test]
    fn git_uses_literal_paths_and_preserves_crlf_symlinks_and_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let workspace = Workspace::new();
        init_git(&workspace);
        git(&workspace.0, &["config", "core.autocrlf", "input"]);
        let path = "space \"quote\n界.txt";
        workspace.write(path, b"literal filename contents");
        workspace.write("crlf", b"one\r\ntwo\r\n");
        symlink(path, workspace.0.join("link")).unwrap();
        fs::set_permissions(workspace.0.join(path), fs::Permissions::from_mode(0o755)).unwrap();
        git(&workspace.0, &["add", "."]);
        git(&workspace.0, &["commit", "-qm", "baseline"]);
        let point = capture(&workspace.0, "chat", "prompt").unwrap();
        assert!(point.git_files.contains_key(Path::new(path)));
        assert!(!point.git_files.contains_key(Path::new("crlf")), "CRLF bytes differ from the LF blob in Git");
        assert_eq!(fs::read(files(&workspace.0, &point).join("crlf")).unwrap(), b"one\r\ntwo\r\n");
        workspace.write(path, b"edited");
        workspace.write("crlf", b"edited");
        fs::set_permissions(workspace.0.join(path), fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_file(workspace.0.join("link")).unwrap();
        restore(&workspace.0, &point, "chat").unwrap();
        assert_eq!(fs::read(workspace.0.join(path)).unwrap(), b"literal filename contents");
        assert_eq!(fs::read(workspace.0.join("crlf")).unwrap(), b"one\r\ntwo\r\n");
        assert_eq!(fs::read_link(workspace.0.join("link")).unwrap(), PathBuf::from(path));
        assert_eq!(fs::metadata(workspace.0.join(path)).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn legacy_transcripts_are_migrated_without_losing_branches_or_images() {
        let workspace = Workspace::new();
        let mut session = Session::new("test".into());
        session.messages = vec![message("user", "old prompt"), message("assistant", &"large answer ".repeat(10000))];
        session.messages[0].image_urls.push("data:image/png;base64,aGVsbG8=".into());
        session.history = vec![HistoryItem::UserPrompt("old prompt".into()), HistoryItem::FileDiff("diff\n".repeat(10000))];
        let first = capture_session(&workspace.0, &session, "next").unwrap();
        let path = directory(&workspace.0).join(format!("{}.json", first.id));
        // A real v1 checkpoint has an embedded transcript and no object refs.
        fs::write(&path, serde_json::to_vec_pretty(&first).unwrap()).unwrap();
        let old_size = fs::metadata(&path).unwrap().len();
        session.messages[1].content = Some("a different branch".into());
        capture_session(&workspace.0, &session, "branch prompt").unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["format_version"], 2);
        assert!(saved.get("conversation").is_none());
        assert!(fs::metadata(&path).unwrap().len() * 10 < old_size);
        let loaded = list(&workspace.0, &session.id).unwrap().into_iter().find(|p| p.id == first.id).unwrap();
        assert!(loaded.conversation.is_none(), "menus must load conversations lazily");
        let restored = conversation_at(&loaded, &session).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), serde_json::to_value(first.conversation.unwrap()).unwrap());
    }

    /// Read-only source; all writes and round-trip checks use a temporary copy.
    #[test]
    #[ignore = "set TAKIZA_CHECKPOINT_FIXTURE to a legacy workspace for a storage benchmark"]
    fn benchmark_legacy_workspace_copy() {
        let source = PathBuf::from(std::env::var_os("TAKIZA_CHECKPOINT_FIXTURE").expect("fixture workspace"));
        let workspace = Workspace::new();
        fs::create_dir_all(directory(&workspace.0)).unwrap();
        let points = metadata(&source, None).unwrap();
        let mut before = 0;
        for point in &points {
            assert!(point.conversation.is_some(), "fixture must have embedded conversations");
            before += fs::metadata(directory(&source).join(format!("{}.json", point.id))).unwrap().len();
            write_metadata(&workspace.0, point).unwrap();
        }
        for point in &points {
            let loaded = list(&workspace.0, &point.session_id).unwrap().into_iter().find(|p| p.id == point.id).unwrap();
            let original = point.conversation.as_ref().unwrap();
            assert_eq!(serde_json::to_value(conversation_at(&loaded, original).unwrap()).unwrap(),
                serde_json::to_value(original).unwrap());
        }
        let mut pending = vec![directory(&workspace.0)];
        let (mut after, mut allocated, mut count) = (0, 0, 0);
        while let Some(path) = pending.pop() {
            for item in fs::read_dir(path).unwrap() {
                let item = item.unwrap();
                let metadata = item.metadata().unwrap();
                if metadata.is_dir() { pending.push(item.path()); }
                else {
                    count += 1;
                    after += metadata.len();
                    #[cfg(unix)]
                    { use std::os::unix::fs::MetadataExt; allocated += metadata.blocks() * 512; }
                    #[cfg(not(unix))]
                    { allocated += metadata.len(); }
                }
            }
        }
        println!("{} conversations verified: before={before} bytes, after={after} bytes, allocated={allocated} bytes, files={count}", points.len());
    }

    #[test]
    fn deleting_a_chat_removes_its_rewinds_and_keeps_other_chats_restorable() {
        let workspace = Workspace::new();
        workspace.write("file", b"shared contents");
        let mut first = Session::new("test".into());
        first.messages = vec![message("user", "first")];
        first.save(&workspace.0).unwrap();
        let a = capture_session(&workspace.0, &first, "prompt").unwrap();
        let other = Session::new("test".into());
        let b = capture_session(&workspace.0, &other, "prompt").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(files(&workspace.0, &a).join("file")).unwrap().ino(),
                fs::metadata(files(&workspace.0, &b).join("file")).unwrap().ino());
        }
        Session::delete(&workspace.0, &first.id).unwrap();
        assert!(list(&workspace.0, &first.id).unwrap().is_empty());
        assert!(!directory(&workspace.0).join(&a.id).exists());
        assert_eq!(fs::read(files(&workspace.0, &b).join("file")).unwrap(), b"shared contents");
        workspace.write("file", b"changed");
        let b = list(&workspace.0, &other.id).unwrap().remove(0);
        restore_session(&workspace.0, &b, &other).unwrap();
        assert!(conversation_at(&b, &other).unwrap().messages.is_empty());
        assert_eq!(fs::read(workspace.0.join("file")).unwrap(), b"shared contents");
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
