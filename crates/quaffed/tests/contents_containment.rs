// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Only `src/contents.rs` reads a project's files.
//!
//! A file can be read for its rules - an ignore file, a declaration file - and searched as well,
//! and is read once because every reader goes through the store in `src/contents.rs`.  A reader
//! that read a file itself would read it a second time, which nothing else would notice until
//! the budgets' file trace did.  This reads every other source file of the `quaff` binary and
//! fails naming any line that reads a file directly, except a script file given with `-f`,
//! which is never a project file.

use std::fs;
use std::path::{Path, PathBuf};

/// What reading a file directly looks like in source.
const DIRECT_READS: &[&str] = &[
    "fs::read(",
    "fs::read_to_string(",
    "File::open(",
    "OpenOptions",
    ".add(&",
];

/// Lines allowed to read a file directly, each with its reason.
const ALLOWED: &[&str] = &[
    // A script given with -f: quaff's input, never a file in the project it searches.
    "script_text(fs::read(&path), &source)",
];

/// The lines of `source` that read a file directly, outside comments, the test module and
/// `ALLOWED`.  Only the test module is left out: a `#[cfg(test)]` on any other item, such as a
/// counter a test reads, must not hide the product code after it.
fn reads_directly(source: &str) -> Vec<&str> {
    let product = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or_default();
    product
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| DIRECT_READS.iter().any(|needle| line.contains(needle)))
        .filter(|line| !ALLOWED.iter().any(|allowed| line.contains(allowed)))
        .collect()
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {dir:?}: {err}")) {
        let path = entry.expect("reading a directory entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|name| name != "contents.rs")
        {
            found.push(path);
        }
    }
}

#[test]
fn only_the_contents_store_reads_a_project_s_files() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    sources(&src, &mut found);
    // A walk that read nothing would pass having checked nothing.
    assert!(found.len() >= 5, "found only {found:?} under {src:?}");
    let mut offenders = Vec::new();
    for path in &found {
        let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
        for line in reads_directly(&text) {
            offenders.push(format!("{}: {}", path.display(), line.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these read a file directly, which would read a file also read for its rules a second \
         time; read through crate::contents::Contents instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn reading_directly_is_found_however_it_is_done() {
    assert_eq!(reads_directly("    let b = fs::read(path)?;").len(), 1);
    assert_eq!(
        reads_directly("    let t = std::fs::read_to_string(p);").len(),
        1
    );
    assert_eq!(reads_directly("    let f = File::open(p)?;").len(), 1);
    assert_eq!(reads_directly("    builder.add(&file);").len(), 1);
    // A comment may describe reading, and a test may read freely; the allowed line is allowed.
    assert_eq!(
        reads_directly("// fs::read( is what the store does"),
        Vec::<&str>::new()
    );
    assert_eq!(
        reads_directly("fn f() {}\n#[cfg(test)]\nmod tests { fn t() { fs::read(p); } }"),
        Vec::<&str>::new()
    );
    // A test-only item above product code does not hide that code.
    assert_eq!(
        reads_directly("#[cfg(test)]\nstatic READS: u8 = 0;\nfn read() { fs::read(p); }").len(),
        1
    );
    assert_eq!(
        reads_directly("    let text = script_text(fs::read(&path), &source)?;"),
        Vec::<&str>::new()
    );
}
