use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    // Generate format macros from the same catalog as ordinary UI labels.
    // Rust validates all translated placeholders at each call site.
    println!("cargo:rerun-if-changed=src/locales/ui.json");
    let catalog: std::collections::BTreeMap<String, [String; 2]> = serde_json::from_str(
        &std::fs::read_to_string("src/locales/ui.json").expect("read interface catalog"),
    )
    .expect("valid interface catalog");
    let mut macros = String::from("macro_rules! tf {\n");
    for (english, [russian, chinese]) in catalog {
        let english_literal = format!("{english:?}").replace("\\u{1b}", "\\x1b");
        macros.push_str(&format!(
            "({english_literal} $(, $($args:tt)*)?) => {{ match $crate::i18n::current() {{\n\
             $crate::i18n::Language::English => format!({english_literal} $(, $($args)*)?),\n\
             $crate::i18n::Language::Russian => format!({russian:?} $(, $($args)*)?),\n\
             $crate::i18n::Language::Chinese => format!({chinese:?} $(, $($args)*)?),\n\
             }} }};\n"
        ));
    }
    macros.push_str("}\npub(crate) use tf;\n");
    let output_dir =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("build output directory"));
    std::fs::write(output_dir.join("ui_formats.rs"), macros)
        .expect("write interface format macros");
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
