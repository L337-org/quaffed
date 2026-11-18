// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What quaff says about structural search and editing names no language it parses.
//!
//! The help, the messages and the errors are written for every language a structural pattern
//! applies to, and the help lists those languages once, from the list that decides a file's
//! language.  A sentence that names one of them instead goes stale, unnoticed, when another is
//! added.  This fails on a language's name in any line of code - not a comment, which may say
//! why the code is the way it is for that language - in the binary and the language-neutral
//! crates.  A language's own adapter is not scanned: what it says is about that language.

use std::fs;
use std::path::{Path, PathBuf};

/// The directories scanned, relative to the workspace root.
const SCANNED: &[&str] = &[
    "crates/quaffed/src",
    "crates/representation/src",
    "crates/script/src",
    "crates/syntax/src",
];

/// A language's name allowed on a line holding `fragment` in `file`, and why.
struct Allowed {
    file: &'static str,
    fragment: &'static str,
    why: &'static str,
}

const ALLOWED: &[Allowed] = &[Allowed {
    file: "crates/quaffed/src/resolve.rs",
    fragment: "Python's",
    why: "a codec quaff does not read is named as CPython names it, since the codec tables are \
          CPython's, whatever declared the encoding",
}];

/// The languages quaff parses, by name.
fn languages() -> Vec<&'static str> {
    vec![quaffed_python::PYTHON.name()]
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels below the workspace root")
        .to_path_buf()
}

/// Every `.rs` file under `dir`, in a stable order.
fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("reading the directory {}: {err}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_language_is_named_in_what_quaff_says() {
    let root = workspace_root();
    let languages = languages();
    let mut files = Vec::new();
    for dir in SCANNED {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(!files.is_empty(), "no source files found under {SCANNED:?}");
    let mut used = vec![false; ALLOWED.len()];
    let mut named = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .expect("under the root")
            .to_string_lossy()
            .replace('\\', "/");
        let text =
            fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {relative}: {err}"));
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for language in &languages {
                if !line.contains(language) {
                    continue;
                }
                let allowed = ALLOWED
                    .iter()
                    .position(|a| a.file == relative && line.contains(a.fragment));
                match allowed {
                    Some(index) => used[index] = true,
                    None => named.push(format!("{relative}:{}: {}", number + 1, line.trim())),
                }
            }
        }
    }
    assert!(
        named.is_empty(),
        "these lines name a language structural patterns apply to; write them for every \
         language, and list languages only through resolve::LANGUAGES:\n{}",
        named.join("\n")
    );
    let stale: Vec<String> = ALLOWED
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(a, _)| format!("{}: {:?} ({})", a.file, a.fragment, a.why))
        .collect();
    assert!(
        stale.is_empty(),
        "these allowances match nothing any more, so remove them:\n{}",
        stale.join("\n")
    );
}
