//! The Linux package's copyright file has a paragraph for every crate linked into its binary.
//!
//! The package ships a static binary, so every crate it links is distributed with it, and the
//! permissive licences most of them use require their notices to go along.  The paragraphs in
//! `packaging/deb/copyright` are written by hand, so this test reads the binary's real dependency
//! graph for each packaged target and fails naming any crate no paragraph covers.  A crate is
//! matched by name alone, so a new version of a crate already there needs no change to the file,
//! and a new crate does.  It also checks the attribution of the codec tables, which are data in
//! the source tree rather than a crate, against the `CPython` release they were generated from.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The targets the Linux package is built for, as `scripts/build-deb` names them.
const PACKAGED_TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

/// Returns the name of every crate linked into `quaff` built for `target`.
///
/// Normal dependencies only: build scripts' dependencies run inside the compiler and none of
/// their code is in the binary.  Procedural macros and the crates they are built from are
/// included, because the code a derive generates is compiled into the binary, and the graph
/// cannot tell which macros' output ships.
/// Not offline, because a host build never downloads the crates only a musl target uses.
fn linked_crates(target: &str) -> Vec<String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree", "--locked", "-e", "normal", "-p", "quaffed", "--target", target, "--prefix",
            "none", "--format", "{p}",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("running cargo tree");
    assert!(
        output.status.success(),
        "cargo tree for {target} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut crates: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // "mimalloc v0.1.52", or "quaffed-script v0.1.0 (/path)" for one of the workspace's
            // own crates, which the package's own copyright covers.
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            fields.next()?.strip_prefix('v')?;
            if fields.next().is_some_and(|source| source.starts_with("(/")) {
                return None;
            }
            Some(name.to_owned())
        })
        .collect();
    crates.sort();
    crates.dedup();
    crates
}

/// Whether `text` contains `phrase` as whole words.
///
/// A substring match would find `mimalloc 0.1.5` inside `mimalloc 0.1.52`, or `sys 0.1.49`
/// inside `libmimalloc-sys 0.1.49`, and pass with the wrong name or version.
fn names(text: &str, phrase: &str) -> bool {
    let part_of_a_word = |c: char| c.is_ascii_alphanumeric() || "._-+".contains(c);
    text.match_indices(phrase).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + phrase.len()..].chars().next();
        // A full stop straight after is the end of a sentence, not more of a version.
        let after_ends_it = match after {
            Some('.') => text[start + phrase.len() + 1..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric()),
            Some(c) => !part_of_a_word(c),
            None => true,
        };
        !before.is_some_and(part_of_a_word) && after_ends_it
    })
}

#[test]
fn a_phrase_is_found_only_as_whole_words() {
    let text = "contains mimalloc 0.1.52 and libmimalloc-sys 0.1.49.";
    assert!(names(text, "mimalloc 0.1.52"));
    assert!(names(text, "libmimalloc-sys 0.1.49"));
    assert!(!names(text, "mimalloc 0.1.5"));
    assert!(!names(text, "sys 0.1.49"));
    assert!(!names(text, "mimalloc 0.1"));
}

/// The copyright file's paragraphs, each a map from field name to its value, with continuation
/// lines joined by line breaks and their leading space removed.
fn paragraphs(copyright: &str) -> Vec<BTreeMap<String, String>> {
    let mut paragraphs = Vec::new();
    let mut paragraph: BTreeMap<String, String> = BTreeMap::new();
    let mut field: Option<String> = None;
    for line in copyright.lines() {
        if line.trim().is_empty() {
            if !paragraph.is_empty() {
                paragraphs.push(std::mem::take(&mut paragraph));
            }
            field = None;
        } else if let Some(continuation) = line.strip_prefix(' ') {
            let name = field
                .as_ref()
                .unwrap_or_else(|| panic!("continuation line {line:?} follows no field"));
            let value = paragraph.get_mut(name).expect("the field was inserted");
            value.push('\n');
            value.push_str(continuation);
        } else {
            let (name, value) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("line {line:?} is neither a field nor a continuation"));
            paragraph.insert(name.to_owned(), value.trim().to_owned());
            field = Some(name.to_owned());
        }
    }
    if !paragraph.is_empty() {
        paragraphs.push(paragraph);
    }
    paragraphs
}

/// The copyright file's paragraphs, read from the source tree.
fn copyright_paragraphs() -> (PathBuf, Vec<BTreeMap<String, String>>) {
    let path = workspace_root().join("packaging/deb/copyright");
    let copyright = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    let paragraphs = paragraphs(&copyright);
    (path, paragraphs)
}

/// The crate a `Files` pattern names, `NAME` for `crates.io/NAME/*` or a path under it.
fn crate_of(pattern: &str) -> Option<&str> {
    pattern.strip_prefix("crates.io/")?.split('/').next()
}

#[test]
fn a_crate_is_named_by_its_files_pattern() {
    assert_eq!(crate_of("crates.io/mimalloc/*"), Some("mimalloc"));
    assert_eq!(
        crate_of("crates.io/libmimalloc-sys/c_src/mimalloc/*"),
        Some("libmimalloc-sys")
    );
    assert_eq!(crate_of("toolchain/musl/*"), None);
    assert_eq!(crate_of("*"), None);
}

#[test]
fn the_package_copyright_file_has_a_paragraph_for_every_linked_crate() {
    let (path, paragraphs) = copyright_paragraphs();
    let covered: BTreeSet<&str> = paragraphs
        .iter()
        .filter_map(|paragraph| paragraph.get("Files"))
        .flat_map(|files| files.split_whitespace())
        .filter_map(crate_of)
        .collect();
    // Each missing crate once, with every packaged target that links it.
    let mut missing: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut checked = 0;
    for target in PACKAGED_TARGETS {
        for name in linked_crates(target) {
            checked += 1;
            if !covered.contains(name.as_str()) {
                missing.entry(name).or_default().push(target);
            }
        }
    }
    // mimalloc is linked into both packaged targets, so a graph with nothing in it means the
    // query is broken rather than the binary dependency-free.
    assert!(
        checked > 0,
        "cargo tree listed no linked crate for any packaged target, so nothing was checked"
    );
    let missing: Vec<String> = missing
        .into_iter()
        .map(|(crate_, targets)| format!("{crate_} (for {})", targets.join(", ")))
        .collect();
    assert!(
        missing.is_empty(),
        "{} has no paragraph for these crates, which the packaged binary links: {}.  Add each \
         as crates.io/NAME/* to the Files field of a paragraph whose Copyright and License \
         fields are that crate's own, or a new paragraph for it",
        path.display(),
        missing.join(", ")
    );
}

#[test]
fn every_files_paragraph_has_a_copyright_and_a_licence_whose_text_is_carried() {
    let (path, paragraphs) = copyright_paragraphs();
    // A paragraph with a License field and no Files field carries that licence's text.
    let carried: BTreeSet<&str> = paragraphs
        .iter()
        .filter(|paragraph| !paragraph.contains_key("Files"))
        .filter_map(|paragraph| paragraph.get("License"))
        .filter_map(|license| license.lines().next())
        .collect();
    let mut faults = Vec::new();
    for paragraph in paragraphs
        .iter()
        .filter(|paragraph| paragraph.contains_key("Files"))
    {
        let files = paragraph["Files"]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if paragraph
            .get("Copyright")
            .is_none_or(|copyright| copyright.trim().is_empty())
        {
            faults.push(format!("{files}: no Copyright field"));
        }
        let Some(license) = paragraph.get("License") else {
            faults.push(format!("{files}: no License field"));
            continue;
        };
        // The first line is an expression of short names joined by "and" and "or".
        for name in license
            .lines()
            .next()
            .unwrap_or_default()
            .split(|c: char| c.is_whitespace() || "()".contains(c))
            .filter(|word| !word.is_empty() && *word != "and" && *word != "or")
        {
            if !carried.contains(name) {
                faults.push(format!(
                    "{files}: licence {name:?} has no paragraph with its text"
                ));
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{}: {}",
        path.display(),
        faults.join("; ")
    );
}

/// The `CPython` release the generated codec tables say they were read from, by the first line of
/// their module comment, `` //! `CPython` 3.14.8's codecs, ... ``.
fn codec_tables_release() -> String {
    let path = workspace_root().join("crates/encoding/src/codecs.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    source
        .lines()
        .find_map(|line| line.strip_prefix("//! `CPython` "))
        .and_then(|rest| rest.split_once("'s codecs"))
        .map_or_else(
            || {
                panic!(
                    "{} has no line \"//! `CPython` <release>'s codecs\" naming the release its \
                 tables come from",
                    path.display()
                )
            },
            |(release, _)| release.to_owned(),
        )
}

#[test]
fn the_copyright_file_attributes_the_codec_tables_to_their_cpython_release() {
    // The tables are data in the source tree, not a crate, so the dependency graph above cannot
    // see them; their paragraph names the release they were generated from.
    let (path, paragraphs) = copyright_paragraphs();
    let tables = "crates/encoding/src/codecs.rs";
    let paragraph = paragraphs
        .iter()
        .find(|paragraph| paragraph.get("Files").is_some_and(|files| files == tables))
        .unwrap_or_else(|| {
            panic!(
                "{} has no paragraph whose Files is {tables}",
                path.display()
            )
        });
    // The comment wraps, so compare with line breaks folded away.
    let comment = paragraph
        .get("Comment")
        .map_or_else(String::new, |comment| {
            comment.split_whitespace().collect::<Vec<_>>().join(" ")
        });
    let release = codec_tables_release();
    assert!(
        names(&comment, &format!("Python {release}")),
        "{}'s paragraph for {tables} does not attribute the codec tables to Python {release}, \
         the release they were generated from.  Name it in that paragraph's Comment",
        path.display()
    );
}
