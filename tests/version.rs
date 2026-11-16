// SPDX-License-Identifier: MIT

//! The version in `Cargo.toml` is the one the binary reports, and it has release notes.

use std::path::Path;
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn the_release_version_is_documented() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let page = format!("v{VERSION}.md");
    let notes = std::fs::read_to_string(root.join("docs/releases").join(&page))
        .unwrap_or_else(|e| panic!("docs/releases/{page} is missing: {e}"));
    assert!(
        notes.starts_with(&format!("# lake-polars v{VERSION}\n")),
        "docs/releases/{page} must start with its version"
    );
    let index = std::fs::read_to_string(root.join("docs/releases/README.md")).unwrap();
    assert!(
        index.contains(&format!("]({page})")),
        "docs/releases/README.md must link {page}"
    );
}

#[test]
fn version_flag_reports_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_lake-polars"))
        .arg("--version")
        .env_remove("LAKE_POLARS_TOKEN")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("lake-polars {VERSION}\n")
    );
}
