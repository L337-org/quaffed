//! Every product source file opens with the licence header.
//!
//! Product code means every `.rs` file under a member's `src/`.  Tests, benches and build
//! scripts are exempt, because the header convention is "product code, not test code" and the
//! failure it guards against is the inconsistent middle where nobody can tell which files
//! meant to carry one.

use std::fs;
use std::path::{Path, PathBuf};

const HEADER: [&str; 2] = [
    "// SPDX-License-Identifier: GPL-3.0-or-later",
    "// SPDX-FileCopyrightText: 2026 Gavin Lucas",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

fn rust_files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()))
            .path();
        if path.is_dir() {
            rust_files_under(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn every_product_source_file_carries_the_licence_header() {
    let root = workspace_root();
    let crates = root.join("crates");
    let mut sources = Vec::new();
    let members =
        fs::read_dir(&crates).unwrap_or_else(|err| panic!("listing {}: {err}", crates.display()));
    for member in members {
        let src = member.expect("reading crates/").path().join("src");
        if src.is_dir() {
            rust_files_under(&src, &mut sources);
        }
    }

    // A scan that found nothing would pass having checked nothing.  The binary's
    // own main.rs always exists, so zero means the walk is broken, not that the tree is clean.
    assert!(
        !sources.is_empty(),
        "found no .rs files under {}/*/src, so the header check verified nothing",
        crates.display()
    );

    let missing: Vec<String> = sources
        .iter()
        .filter(|path| {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
            !text.lines().take(HEADER.len()).eq(HEADER)
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();

    assert!(
        missing.is_empty(),
        "{} of {} product source files do not open with the licence header:\n  {}\nexpected the first lines to be:\n  {}",
        missing.len(),
        sources.len(),
        missing.join("\n  "),
        HEADER.join("\n  ")
    );
}
