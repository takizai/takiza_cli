use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    // Watch Git metadata as well as sources: moving HEAD or adding a tag must
    // update the version even when no Rust file changed (including worktrees).
    for path in ["HEAD", "refs", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    let version = git(&[
        "describe",
        "--tags",
        "--abbrev=0",
        "--match",
        "v[0-9]*",
        "--match",
        "[0-9]*",
    ])
    .map(|tag| tag.strip_prefix('v').unwrap_or(&tag).to_owned())
    .unwrap_or_else(|| {
        println!("cargo:warning=No reachable version tag; using Cargo.toml version");
        std::env::var("CARGO_PKG_VERSION").unwrap()
    });
    println!("cargo:rustc-env=TAKIZA_VERSION={version}");
}
