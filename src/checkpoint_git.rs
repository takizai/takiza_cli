//! Reuse committed blobs without checking out files or changing HEAD/index.
use anyhow::{Context, Result};
use std::{collections::BTreeMap, fs, io::{BufRead, BufReader, Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, process::{Child, ChildStdin, ChildStdout, Command, Stdio}};
use tokio_util::sync::CancellationToken;

pub struct Baseline {
    pub commit: String,
    pub files: BTreeMap<PathBuf, String>,
}

pub fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit())
}

fn command(workspace: &Path) -> Command {
    let mut command = crate::git::create_git_command();
    command.current_dir(workspace).env("GIT_NO_LAZY_FETCH", "1").env("GIT_NO_REPLACE_OBJECTS", "1");
    command
}

// File-backed stdio avoids pipe deadlocks for repositories with many paths.
// Poll the process so Esc can stop hashing a large repository.
fn output(mut command: Command, input: Option<fs::File>, cancel: Option<&CancellationToken>) -> Result<Vec<u8>> {
    let mut result = tempfile::tempfile()?;
    command.stdin(input.map(Stdio::from).unwrap_or(Stdio::null()))
        .stdout(result.try_clone()?).stderr(Stdio::null());
    let mut child = command.spawn()?;
    loop {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Checkpoint cancelled");
        }
        match child.try_wait() {
            Ok(Some(status)) => { anyhow::ensure!(status.success(), "Git checkpoint data unavailable"); break; },
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error.into()); },
        }
    }
    result.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    result.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}
#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> { path.to_string_lossy().as_bytes().to_vec() }

fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    { use std::os::unix::ffi::OsStringExt; Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into()) }
    #[cfg(not(unix))]
    { Ok(String::from_utf8(bytes.to_vec())?.into()) }
}

pub fn baseline(workspace: &Path, paths: impl Iterator<Item = PathBuf>, cancel: Option<&CancellationToken>) -> Result<Baseline> {
    let mut head = command(workspace);
    head.args(["rev-parse", "--verify", "HEAD^{commit}"]);
    let commit = String::from_utf8(output(head, None, cancel)?)?.trim().to_owned();
    anyhow::ensure!(valid_oid(&commit), "Invalid Git checkpoint commit");
    let mut root = command(workspace);
    root.args(["rev-parse", "--show-toplevel"]);
    let root = output(root, None, cancel)?;
    // Remove just the record terminator; spaces in repository names are literal.
    let root = root.strip_suffix(b"\n").unwrap_or(&root);
    #[cfg(windows)]
    let root = root.strip_suffix(b"\r").unwrap_or(root);
    let root = path_from_bytes(root)?.canonicalize()?;
    let prefix = workspace.strip_prefix(root)?;
    let mut tree = command(workspace);
    tree.args(["ls-tree", "-r", "-z", "-l", "--full-tree", &commit]);
    let tree = output(tree, None, cancel)?;
    let mut tracked = BTreeMap::new();
    for record in tree.split(|&b| b == 0).filter(|record| !record.is_empty()) {
        let tab = record.iter().position(|&b| b == b'\t').context("Invalid Git tree")?;
        let header = std::str::from_utf8(&record[..tab])?.split_whitespace().collect::<Vec<_>>();
        if header.len() != 4 || header[1] != "blob" || !matches!(header[0], "100644" | "100755") { continue; }
        let path = path_from_bytes(&record[tab + 1..])?;
        if let Ok(relative) = path.strip_prefix(prefix) {
            if valid_oid(header[2]) { tracked.insert(relative.to_path_buf(), (header[2].to_owned(), header[3].parse::<u64>()?)); }
        }
    }
    let candidates = paths.filter(|path| tracked.get(path).is_some_and(|(_, size)|
        fs::metadata(workspace.join(path)).is_ok_and(|metadata| metadata.len() == *size))).collect::<Vec<_>>();
    let mut input = tempfile::tempfile()?;
    for path in &candidates {
        if cancel.is_some_and(CancellationToken::is_cancelled) { anyhow::bail!("Checkpoint cancelled"); }
        // Git accepts C-quoted paths, including tabs/newlines and non-UTF8 bytes.
        input.write_all(b"\"")?;
        for byte in path_bytes(&workspace.join(path)) { write!(input, "\\{byte:03o}")?; }
        input.write_all(b"\"\n")?;
    }
    input.seek(SeekFrom::Start(0))?;
    let mut hash = command(workspace);
    // Compare actual bytes, bypassing CRLF conversion, LFS and clean filters.
    hash.args(["hash-object", "--no-filters", "--stdin-paths"]);
    let hashes = String::from_utf8(output(hash, Some(input), cancel)?)?;
    let hashes = hashes.lines().collect::<Vec<_>>();
    anyhow::ensure!(hashes.len() == candidates.len(), "Incomplete Git file hashes");
    let mut files = BTreeMap::new();
    for (path, hash) in candidates.into_iter().zip(hashes) {
        if tracked.get(&path).is_some_and(|(oid, _)| oid == hash) { files.insert(path, hash.to_owned()); }
    }
    Ok(Baseline { commit, files })
}

fn reference(id: &str) -> String { format!("refs/takiza/checkpoints/{id}") }

pub fn pin(workspace: &Path, id: &str, commit: &str, cancel: Option<&CancellationToken>) -> Result<()> {
    let mut pin = command(workspace);
    pin.args(["update-ref", "--no-deref", &reference(id), commit, ""]);
    output(pin, None, cancel)?;
    Ok(())
}

pub fn release(workspace: &Path, id: &str, commit: &str) -> Result<()> {
    let mut probe = command(workspace);
    probe.args(["rev-parse", "--verify", &reference(id)]);
    let Ok(current) = output(probe, None, None) else { return Ok(()); };
    anyhow::ensure!(std::str::from_utf8(&current)?.trim() == commit, "Git checkpoint reference was changed externally");
    let mut release = command(workspace);
    release.args(["update-ref", "--no-deref", "-d", &reference(id), commit]);
    output(release, None, None)?;
    Ok(())
}

pub fn verify(workspace: &Path, hashes: impl Iterator<Item = String>) -> Result<BTreeMap<String, u64>> {
    let hashes = hashes.collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
    if hashes.is_empty() { return Ok(BTreeMap::new()); }
    let mut input = tempfile::tempfile()?;
    for hash in &hashes { anyhow::ensure!(valid_oid(hash), "Invalid Git blob reference"); writeln!(input, "{hash}")?; }
    input.seek(SeekFrom::Start(0))?;
    let mut verify = command(workspace);
    verify.args(["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"]);
    let result = String::from_utf8(output(verify, Some(input), None)?)?;
    let lines = result.lines().collect::<Vec<_>>();
    anyhow::ensure!(lines.len() == hashes.len(), "Incomplete Git checkpoint object metadata");
    let mut sizes = BTreeMap::new();
    for (hash, line) in hashes.into_iter().zip(lines) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        anyhow::ensure!(fields.len() == 3 && fields[0] == hash && fields[1] == "blob",
            "Git data needed for this checkpoint is missing; restore has not changed the workspace");
        sizes.insert(hash, fields[2].parse()?);
    }
    Ok(sizes)
}

pub struct Objects {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Objects {
    pub fn open(workspace: &Path) -> Result<Self> {
        let mut child = command(workspace).args(["cat-file", "--batch"]).stdin(Stdio::piped())
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let input = child.stdin.take().context("Missing Git input")?;
        let output = BufReader::new(child.stdout.take().context("Missing Git output")?);
        Ok(Self { child, input, output })
    }

    pub fn blob<T>(&mut self, hash: &str, consume: impl FnOnce(&mut dyn Read, u64) -> Result<T>) -> Result<T> {
        anyhow::ensure!(valid_oid(hash), "Invalid Git blob reference");
        writeln!(self.input, "{hash}")?;
        self.input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        anyhow::ensure!(fields.len() == 3 && fields[0] == hash && fields[1] == "blob", "Missing Git checkpoint blob {hash}");
        let size: u64 = fields[2].parse()?;
        let mut body = self.output.by_ref().take(size);
        let value = consume(&mut body, size)?;
        std::io::copy(&mut body, &mut std::io::sink())?;
        anyhow::ensure!(body.limit() == 0, "Truncated Git checkpoint blob");
        let mut newline = [0u8];
        self.output.read_exact(&mut newline)?;
        anyhow::ensure!(newline == [b'\n'], "Invalid Git object response");
        Ok(value)
    }
}

impl Drop for Objects {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}
