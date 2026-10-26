//! Immutable, compressed conversation items shared by checkpoints in one chat.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashMap}, fs, io::{Read, Write}, path::{Path, PathBuf}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    session: crate::session::Session,
    messages: Vec<String>,
    history: Vec<String>,
}

fn directory(workspace: &Path, session_id: &str) -> PathBuf {
    workspace.join(".takiza/checkpoints/conversations").join(format!("{:x}", Sha256::digest(session_id.as_bytes())))
}

// One immutable ZIP per batch avoids a filesystem block for every small item.
// The ZIP central directories are the index; concurrent writers can publish
// overlapping packs without losing references or mutating existing objects.
struct Packs {
    archives: Vec<PathBuf>,
    locations: HashMap<String, usize>,
    current: Option<(usize, zip::ZipArchive<fs::File>)>,
}

impl Packs {
    fn open(directory: &Path) -> Result<Self> {
        let mut packs = Self { archives: Vec::new(), locations: HashMap::new(), current: None };
        if directory.exists() {
            for item in fs::read_dir(directory)? {
                let path = item?.path();
                if path.extension().is_none_or(|extension| extension != "zip") { continue; }
                let archive = zip::ZipArchive::new(fs::File::open(&path)?)?;
                for name in archive.file_names() {
                    packs.locations.insert(name.to_owned(), packs.archives.len());
                }
                packs.archives.push(path);
            }
        }
        Ok(packs)
    }

    fn get<T: serde::de::DeserializeOwned>(&mut self, hash: &str) -> Result<T> {
        anyhow::ensure!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()), "Invalid conversation object reference");
        let index = *self.locations.get(hash).context("Missing checkpoint conversation data")?;
        if self.current.as_ref().is_none_or(|(current, _)| *current != index) {
            self.current = Some((index, zip::ZipArchive::new(fs::File::open(&self.archives[index])?)?));
        }
        let mut bytes = Vec::new();
        self.current.as_mut().context("Missing conversation archive")?.1.by_name(hash)?.read_to_end(&mut bytes)?;
        anyhow::ensure!(format!("{:x}", Sha256::digest(&bytes)) == hash, "Corrupt checkpoint conversation data");
        Ok(serde_json::from_slice(&bytes)?)
    }
}

fn put<T: Serialize>(packs: &Packs, pending: &mut BTreeMap<String, Vec<u8>>, value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if !packs.locations.contains_key(&hash) { pending.entry(hash.clone()).or_insert(bytes); }
    Ok(hash)
}

fn publish(directory: &Path, pending: BTreeMap<String, Vec<u8>>) -> Result<()> {
    if pending.is_empty() { return Ok(()); }
    let mut digest = Sha256::new();
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    {
        let mut archive = zip::ZipWriter::new(temporary.as_file_mut());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated).compression_level(Some(1));
        for (hash, bytes) in pending {
            digest.update(hash.as_bytes());
            archive.start_file(hash, options)?;
            archive.write_all(&bytes)?;
        }
        archive.finish()?;
    }
    let path = directory.join(format!("{:x}.zip", digest.finalize()));
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error.into()),
    }
}

impl Conversation {
    pub fn save(workspace: &Path, session: &crate::session::Session) -> Result<Self> {
        let directory = directory(workspace, &session.id);
        fs::create_dir_all(&directory)?;
        let packs = Packs::open(&directory)?;
        let mut pending = BTreeMap::new();
        let messages = session.messages.iter().map(|item| put(&packs, &mut pending, item)).collect::<Result<_>>()?;
        let history = session.history.iter().map(|item| put(&packs, &mut pending, item)).collect::<Result<_>>()?;
        publish(&directory, pending)?;
        let mut metadata = session.clone();
        metadata.messages.clear();
        metadata.history.clear();
        Ok(Self { session: metadata, messages, history })
    }

    pub fn load(&self, workspace: &Path, session_id: &str) -> Result<crate::session::Session> {
        anyhow::ensure!(self.session.id == session_id, "Checkpoint conversation belongs to another chat");
        let mut packs = Packs::open(&directory(workspace, session_id))?;
        let mut session = self.session.clone();
        session.messages = self.messages.iter().map(|hash| packs.get(hash)).collect::<Result<_>>()?;
        session.history = self.history.iter().map(|hash| packs.get(hash)).collect::<Result<_>>()?;
        Ok(session)
    }
}

pub fn remove_session(workspace: &Path, session_id: &str) -> Result<()> {
    let directory = directory(workspace, session_id);
    match fs::remove_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_snapshots_share_objects_and_missing_or_corrupt_data_is_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let mut session = crate::session::Session::new("test".into());
        session.history.push(crate::cli_ui::HistoryItem::FileDiff("large diff\n".repeat(10000)));
        let original = Conversation::save(workspace.path(), &session).unwrap();
        for _ in 0..10 { Conversation::save(workspace.path(), &session).unwrap(); }
        let objects = directory(workspace.path(), &session.id);
        assert_eq!(fs::read_dir(&objects).unwrap().count(), 1);
        let object = fs::read_dir(&objects).unwrap().next().unwrap().unwrap().path();
        assert!(fs::metadata(&object).unwrap().len() < 2000);
        session.history.push(crate::cli_ui::HistoryItem::UserPrompt("new branch".into()));
        let branch = Conversation::save(workspace.path(), &session).unwrap();
        assert_eq!(fs::read_dir(&objects).unwrap().count(), 2);
        assert_eq!(original.load(workspace.path(), &session.id).unwrap().history.len(), 1);
        assert_eq!(branch.load(workspace.path(), &session.id).unwrap().history.len(), 2);
        assert!(branch.load(workspace.path(), "other chat").is_err());
        fs::write(&object, b"corrupt archive").unwrap();
        assert!(original.load(workspace.path(), &session.id).is_err());
        fs::remove_file(&object).unwrap();
        assert!(original.load(workspace.path(), &session.id).is_err());
        remove_session(workspace.path(), &session.id).unwrap();
        assert!(!objects.exists());
    }
}
