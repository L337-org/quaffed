//! The Linux package's copyright file names every crate linked into its binary.
//!
//! The package ships a static binary, so every crate it links is distributed with it, and the
//! permissive licences most of them use require their notices to go along.  The list in
//! `packaging/deb/copyright` is written by hand, so this test reads the binary's real dependency
//! graph for each packaged target and fails naming any crate the file does not mention.  It also
//! checks the attribution of the codec tables, which are data in the source tree rather than a
//! crate, against the CPython release they were generated from.

use std::collections::BTreeMap;
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

/// Returns `(name, version)` for every crate linked into `quaff` built for `target`.
///
/// Normal dependencies only: build scripts' dependencies run at build time and are not shipped.
/// Not offline, because a host build never downloads the crates only a musl target uses.
fn linked_crates(target: &str) -> Vec<(String, String)> {
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
    let mut crates: Vec<(String, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // "mimalloc v0.1.52", or "quaffed-script v0.1.0 (/path)" for one of the workspace's
            // own crates, which the package's own copyright covers.
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let version = fields.next()?.strip_prefix('v')?;
            if fields.next().is_some_and(|source| source.starts_with("(/")) {
                return None;
            }
            Some((name.to_owned(), version.to_owned()))
        })
        .collect();
    crates.sort();
    crates.dedup();
    crates
}

/// Whether `text` contains `crate_` - "name version" - as whole words.
///
/// A substring match would find `mimalloc 0.1.5` inside `mimalloc 0.1.52`, or `sys 0.1.49`
/// inside `libmimalloc-sys 0.1.49`, and pass with the wrong crate or version named.
fn names(text: &str, crate_: &str) -> bool {
    let part_of_a_word = |c: char| c.is_ascii_alphanumeric() || "._-+".contains(c);
    text.match_indices(crate_).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + crate_.len()..].chars().next();
        // A full stop straight after is the end of a sentence, not more of a version.
        let after_ends_it = match after {
            Some('.') => text[start + crate_.len() + 1..]
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
fn a_crate_is_named_only_as_whole_words() {
    let text = "contains mimalloc 0.1.52 and libmimalloc-sys 0.1.49.";
    assert!(names(text, "mimalloc 0.1.52"));
    assert!(names(text, "libmimalloc-sys 0.1.49"));
    assert!(!names(text, "mimalloc 0.1.5"));
    assert!(!names(text, "sys 0.1.49"));
    assert!(!names(text, "mimalloc 0.1"));
}

#[test]
fn the_package_copyright_file_names_every_linked_crate() {
    let path = workspace_root().join("packaging/deb/copyright");
    let copyright = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    // The comment wraps, so compare with line breaks and continuation spaces folded away.
    let folded = copyright.split_whitespace().collect::<Vec<_>>().join(" ");
    // Each missing crate once, with every packaged target that links it.
    let mut missing: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut checked = 0;
    for target in PACKAGED_TARGETS {
        for (name, version) in linked_crates(target) {
            checked += 1;
            let crate_ = format!("{name} {version}");
            if !names(&folded, &crate_) {
                missing.entry(crate_).or_default().push(target);
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
        "{} does not name these crates, which the packaged binary links: {}.  Add each, with \
         its copyright and licence, to the Comment and to the Copyright and License fields",
        path.display(),
        missing.join(", ")
    );
}

/// The CPython release the generated codec tables say they were read from, by the first line of
/// their module comment, `` //! `CPython` 3.14.8's codecs, ... ``.
fn codec_tables_release() -> String {
    let path = workspace_root().join("crates/encoding/src/codecs.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    source
        .lines()
        .find_map(|line| line.strip_prefix("//! `CPython` "))
        .and_then(|rest| rest.split_once("'s codecs"))
        .map(|(release, _)| release.to_owned())
        .unwrap_or_else(|| {
            panic!(
                "{} has no line \"//! `CPython` <release>'s codecs\" naming the release its \
                 tables come from",
                path.display()
            )
        })
}

#[test]
fn the_copyright_file_attributes_the_codec_tables_to_their_cpython_release() {
    // The tables are data in the source tree, not a crate, so the dependency graph above cannot
    // see them; their attribution names the release they were generated from.
    let path = workspace_root().join("packaging/deb/copyright");
    let copyright = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    let folded = copyright.split_whitespace().collect::<Vec<_>>().join(" ");
    let release = codec_tables_release();
    assert!(
        names(&folded, &format!("Python {release}")),
        "{} does not attribute the codec tables to Python {release}, the release \
         crates/encoding/src/codecs.rs was generated from.  Name it in the paragraph on the \
         character-mapping tables",
        path.display()
    );
}
