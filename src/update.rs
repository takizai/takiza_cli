//! Updates never touch configuration or credentials. npm-managed binaries are
//! updated by the Node launcher, after the Rust process has exited (Windows).
use anyhow::{bail, Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const API: &str = "https://api.github.com/repos/takizai/takiza_cli/releases/latest";
const MAX_DOWNLOAD: usize = 128 * 1024 * 1024;
const CURRENT: &str = env!("TAKIZA_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
enum Installation {
    Npm,
    Cargo { root: PathBuf, source: String },
    Development,
    Standalone,
}

fn detect(exe: &Path) -> Installation {
    if std::env::var_os("TAKIZA_NPM_BINARY")
        .and_then(|path| std::fs::canonicalize(path).ok())
        .is_some_and(|path| std::fs::canonicalize(exe).is_ok_and(|exe| exe == path))
        || exe
            .components()
            .any(|part| part.as_os_str() == "node_modules")
    {
        return Installation::Npm;
    }
    if let Some(bin) = exe.parent() {
        if bin.file_name().is_some_and(|name| name == "bin") {
            if let Some(root) = bin.parent() {
                if let Ok(text) = std::fs::read_to_string(root.join(".crates2.json")) {
                    if let Some(installs) = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|data| data.get("installs").and_then(|v| v.as_object()).cloned())
                    {
                        for (key, record) in installs {
                            if record["bins"].as_array().is_some_and(|bins| {
                                bins.iter()
                                    .any(|bin| bin == "takiza" || bin == "takiza.exe")
                            }) {
                                if let Some(source) = key
                                    .split_once('(')
                                    .and_then(|(_, source)| source.strip_suffix(')'))
                                {
                                    return Installation::Cargo {
                                        root: root.to_owned(),
                                        source: source.to_owned(),
                                    };
                                }
                            }
                        }
                    }
                }
            }
        }
        // Never overwrite cargo run/build output. Native release archives contain
        // a single executable; no package manager is involved in that channel.
        if matches!(
            bin.file_name().and_then(|v| v.to_str()),
            Some("debug" | "release")
        ) && bin
            .ancestors()
            .any(|parent| parent.file_name().is_some_and(|name| name == "target"))
        {
            return Installation::Development;
        }
    }
    Installation::Standalone
}

pub fn automatic_enabled() -> bool {
    if std::env::var_os("CI").is_some() || std::env::var("TAKIZA_NO_UPDATE").as_deref() == Ok("1") {
        return false;
    }
    let mut paths = vec![crate::theme::UserPreferences::config_path()];
    paths.extend(crate::theme::UserPreferences::global_config_path());
    paths
        .iter()
        .all(|path| match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|value| {
                value.get("auto_update") != Some(&serde_json::Value::Bool(false))
            }),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

fn release_version(release: &Release) -> Result<Version> {
    let version = Version::parse(release.tag_name.trim_start_matches('v'))?;
    anyhow::ensure!(
        !release.draft && !release.prerelease && version.pre.is_empty(),
        "GitHub returned an unstable release"
    );
    Ok(version)
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("takiza-updater/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(120))
        .build()?)
}

async fn bounded_get(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "Update download is too large"
    );
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            data.len() + chunk.len() <= limit,
            "Update download is too large"
        );
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

async fn latest() -> Result<Option<Release>> {
    let data = tokio::time::timeout(
        Duration::from_secs(12),
        bounded_get(&client()?, API, 2 * 1024 * 1024),
    )
    .await??;
    let release: Release = serde_json::from_slice(&data)?;
    Ok((release_version(&release)? > Version::parse(CURRENT)?).then_some(release))
}

fn asset_for(release: &Release, target: &str) -> Result<Asset> {
    let extension = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    let name = format!("takiza-{}-{target}.{extension}", release.tag_name);
    let asset = release.assets.iter().find(|asset| asset.name == name).context("This release has no binary for your platform yet. Try again after the release build finishes.")?;
    let url = reqwest::Url::parse(&asset.browser_download_url)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url
                .path()
                .starts_with("/takizai/takiza_cli/releases/download/"),
        "Unexpected release download URL"
    );
    Ok(asset.clone())
}

fn verify(data: &[u8], digest: Option<&str>) -> Result<()> {
    let expected = digest
        .and_then(|value| value.strip_prefix("sha256:"))
        .context("Release has no SHA-256 digest; refusing an unverified update")?;
    anyhow::ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid release checksum"
    );
    anyhow::ensure!(
        format!("{:x}", Sha256::digest(data)).eq_ignore_ascii_case(expected),
        "Release checksum mismatch; installed executable was not changed"
    );
    Ok(())
}

fn extract(data: &[u8], windows: bool, output: &Path) -> Result<()> {
    let expected = if windows { "takiza.exe" } else { "takiza" };
    let mut binary = None;
    if windows {
        let mut archive = zip::ZipArchive::new(Cursor::new(data))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.name() == expected && entry.is_file() {
                anyhow::ensure!(binary.is_none(), "Duplicate executable in release archive");
                let mut bytes = Vec::new();
                entry
                    .by_ref()
                    .take((MAX_DOWNLOAD + 1) as u64)
                    .read_to_end(&mut bytes)?;
                anyhow::ensure!(
                    bytes.len() <= MAX_DOWNLOAD,
                    "Release executable is too large"
                );
                binary = Some(bytes);
            }
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data));
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path()?.as_ref() == Path::new(expected)
                && entry.header().entry_type().is_file()
            {
                anyhow::ensure!(binary.is_none(), "Duplicate executable in release archive");
                let mut bytes = Vec::new();
                entry
                    .by_ref()
                    .take((MAX_DOWNLOAD + 1) as u64)
                    .read_to_end(&mut bytes)?;
                anyhow::ensure!(
                    bytes.len() <= MAX_DOWNLOAD,
                    "Release executable is too large"
                );
                binary = Some(bytes);
            }
        }
    }
    let binary = binary.context("Executable is missing from release archive")?;
    anyhow::ensure!(!binary.is_empty(), "Empty executable in release archive");
    std::fs::write(output, binary)?;
    Ok(())
}

fn cargo_args(root: &Path, source: &str, tag: &str) -> Result<Vec<String>> {
    let mut args = vec![
        "install".into(),
        "--force".into(),
        "--locked".into(),
        "--root".into(),
        root.to_string_lossy().into_owned(),
    ];
    if let Some(source) = source.strip_prefix("path+") {
        let url = reqwest::Url::parse(source)?;
        let path = url
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("Invalid Cargo source path"))?;
        anyhow::ensure!(
            path.join("Cargo.toml").exists(),
            "Original Cargo source checkout is missing: {}",
            path.display()
        );
        args.extend(["--path".into(), path.to_string_lossy().into_owned()]);
    } else if let Some(source) = source.strip_prefix("git+") {
        let mut url = reqwest::Url::parse(source)?;
        url.set_fragment(None);
        url.set_query(None);
        args.extend([
            "--git".into(),
            url.to_string(),
            "--tag".into(),
            tag.into(),
            "takiza-harness".into(),
        ]);
    } else if source == "registry+https://github.com/rust-lang/crates.io-index"
        || source == "sparse+https://index.crates.io/"
    {
        args.extend([
            "takiza-harness".into(),
            "--version".into(),
            tag.trim_start_matches('v').into(),
        ]);
    } else {
        bail!("Unsupported Cargo installation source; use your original cargo install command");
    }
    Ok(args)
}

async fn apply(release: Release, origin: Installation) -> Result<()> {
    let version = release_version(&release)?;
    anyhow::ensure!(
        version > Version::parse(CURRENT)?,
        "Update is not newer than the installed version"
    );
    println!("Updating Takiza {CURRENT} → {version}…");
    match origin {
        Installation::Standalone => {
            let asset = asset_for(&release, env!("TAKIZA_TARGET"))?;
            let data = bounded_get(&client()?, &asset.browser_download_url, MAX_DOWNLOAD).await?;
            verify(&data, asset.digest.as_deref())?;
            let directory = tempfile::tempdir()?;
            let binary = directory.path().join(if cfg!(windows) {
                "takiza.exe"
            } else {
                "takiza"
            });
            extract(&data, cfg!(windows), &binary)?;
            self_replace::self_replace(&binary)
                .context("Cannot replace Takiza; check installation directory permissions")?;
        }
        Installation::Cargo { root, source } => {
            let args = cargo_args(&root, &source, &release.tag_name)?;
            // On Linux current_exe() can acquire a " (deleted)" suffix after
            // Cargo replaces the running inode, so capture the path first.
            let installed_executable = std::env::current_exe()?;
            let status = tokio::process::Command::new("cargo")
                .args(args)
                .env("TAKIZA_NO_UPDATE", "1")
                .stdin(std::process::Stdio::null())
                .status()
                .await?;
            anyhow::ensure!(status.success(), "cargo install failed: {status}");
            // Local sources may still contain the old version: never claim that
            // fetching release metadata also updated the user's source checkout.
            let installed = tokio::process::Command::new(installed_executable)
                .arg("--version")
                .output()
                .await?;
            anyhow::ensure!(installed.status.success() && String::from_utf8_lossy(&installed.stdout).trim() == format!("takiza {version}"),
                "Cargo installation finished, but the source did not build version {version}. Update the original checkout and run takiza update again.");
        }
        _ => bail!("This installation must be updated through its original launcher"),
    }
    println!("Takiza {version} installed. It will be used on the next launch.");
    Ok(())
}

pub async fn manual() -> Result<()> {
    let origin = detect(&std::env::current_exe()?);
    match origin {
        Installation::Npm => bail!("Run takiza update through the npm launcher, or npm install -g takiza@latest"),
        Installation::Development => bail!("This is a Cargo build directory. Update the source checkout and rebuild; it will not be replaced with a release binary."),
        _ => {}
    }
    println!("Checking GitHub Releases for updates (installed: {CURRENT})…");
    if let Some(release) = latest().await? {
        apply(release, origin).await?;
    } else {
        println!("Takiza {CURRENT} is up to date.");
    }
    Ok(())
}

pub struct Automatic {
    task: tokio::task::JoinHandle<Result<Option<Release>>>,
    origin: Installation,
}

impl Automatic {
    pub fn start() -> Option<Self> {
        if !automatic_enabled() {
            return None;
        }
        let origin = detect(&std::env::current_exe().ok()?);
        // Source installations need an explicit update: cargo install --path
        // cannot fetch changes, and rebuilding arbitrary local code in the
        // background would not constitute a release update.
        if origin != Installation::Standalone {
            return None;
        }
        let home = crate::theme::dirs_next_or_home()?;
        let cache = home.join(".config/takiza/updates/github.json");
        let task = tokio::spawn(async move {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            if let Ok(data) = std::fs::read(&cache) {
                if let Ok((checked, current, release)) =
                    serde_json::from_slice::<(u64, String, Option<serde_json::Value>)>(&data)
                {
                    if current == CURRENT && now.saturating_sub(checked) < 86400 {
                        return release
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(Into::into);
                    }
                }
            }
            let release = latest().await?;
            if let Some(release) = &release {
                // Releases are created before CI uploads every platform asset.
                // Do not cache incomplete metadata for an entire day.
                let asset = asset_for(release, env!("TAKIZA_TARGET"))?;
                anyhow::ensure!(
                    asset
                        .digest
                        .as_deref()
                        .is_some_and(|digest| digest.starts_with("sha256:")),
                    "Release checksum is not available yet"
                );
            }
            // Cache successes only; failed checks can be retried next launch.
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(data) = serde_json::to_vec(&(now, CURRENT, &release)) {
                let _ = std::fs::write(&cache, data);
            }
            Ok(release)
        });
        Some(Self { task, origin })
    }

    pub async fn finish(self, success: bool) {
        if !success || !self.task.is_finished() || !automatic_enabled() {
            self.task.abort();
            return;
        }
        if let Ok(Ok(Some(release))) = self.task.await {
            if let Err(error) = apply(release, self.origin).await {
                eprintln!("Takiza auto-update failed: {error:#}\nRun takiza update to retry.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn replacement_child() {
        if let Some(payload) = std::env::var_os("TAKIZA_TEST_REPLACEMENT") {
            self_replace::self_replace(payload).unwrap();
        }
    }

    #[test]
    fn replacement_changes_only_an_isolated_running_copy() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join(if cfg!(windows) {
            "test-copy.exe"
        } else {
            "test-copy"
        });
        let payload = directory.path().join("replacement");
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        std::fs::write(&payload, b"verified replacement payload").unwrap();
        let status = std::process::Command::new(&executable)
            .args(["--exact", "update::tests::replacement_child"])
            .env("TAKIZA_TEST_REPLACEMENT", &payload)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read(&executable).unwrap(),
            b"verified replacement payload"
        );
    }

    #[test]
    fn release_selection_requires_new_stable_version_and_correct_platform() {
        let release = Release {
            tag_name: "v999.1.0".into(),
            draft: false,
            prerelease: false,
            assets: vec![Asset {
                name: "takiza-v999.1.0-x86_64-pc-windows-msvc.zip".into(),
                browser_download_url:
                    "https://github.com/takizai/takiza_cli/releases/download/v999.1.0/windows.zip"
                        .into(),
                digest: None,
            }],
        };
        assert!(release_version(&release).unwrap() > Version::parse(CURRENT).unwrap());
        assert!(asset_for(&release, "x86_64-pc-windows-msvc").is_ok());
        assert!(asset_for(&release, "aarch64-apple-darwin").is_err());
        let mut bad = release.clone();
        bad.prerelease = true;
        assert!(release_version(&bad).is_err());
        bad = release.clone();
        bad.assets[0].browser_download_url = "https://example.com/untrusted.zip".into();
        assert!(asset_for(&bad, "x86_64-pc-windows-msvc").is_err());
    }

    #[test]
    fn checksum_is_required_and_mismatch_is_rejected() {
        let digest = format!("sha256:{:x}", Sha256::digest(b"archive"));
        assert!(verify(b"archive", Some(&digest)).is_ok());
        assert!(verify(b"changed", Some(&digest)).is_err());
        assert!(verify(b"archive", None).is_err());
        assert!(verify(b"archive", Some("sha256:bad")).is_err());
    }

    #[test]
    fn archive_extraction_selects_only_the_root_executable() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("executable");
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        for (name, contents) in [
            ("other", "ignored"),
            ("nested/takiza", "wrong"),
            ("takiza", "binary"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, name, contents.as_bytes())
                .unwrap();
        }
        let data = archive.into_inner().unwrap().finish().unwrap();
        extract(&data, false, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"binary");
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("../takiza.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"wrong").unwrap();
        let missing = zip.finish().unwrap().into_inner();
        assert!(extract(&missing, true, &output).is_err());
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("takiza.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"windows binary").unwrap();
        extract(&zip.finish().unwrap().into_inner(), true, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"windows binary");
    }

    #[test]
    fn cargo_receipt_keeps_original_root_and_local_source() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cargo prefix");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let source = directory.path().join("source checkout");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Cargo.toml"), "[package]").unwrap();
        let origin = format!(
            "path+{}",
            reqwest::Url::from_directory_path(&source).unwrap()
        );
        let key = format!("takiza-harness 0.1.11 ({origin})");
        std::fs::write(
            root.join(".crates2.json"),
            serde_json::to_vec(&serde_json::json!({"installs": { key: {"bins": ["takiza"]} }}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            detect(&root.join("bin/takiza")),
            Installation::Cargo {
                root: root.clone(),
                source: origin.clone()
            }
        );
        let args = cargo_args(&root, &origin, "v0.1.12").unwrap();
        assert_eq!(args[4], root.to_string_lossy());
        assert_eq!(Path::new(&args[6]), source);
        assert!(cargo_args(&root, "unknown+source", "v0.1.12").is_err());
        assert_eq!(
            detect(&directory.path().join("target/debug/takiza")),
            Installation::Development
        );
    }
}
