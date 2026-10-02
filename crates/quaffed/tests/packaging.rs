//! The Linux package's copyright file names every crate linked into its binary.
//!
//! The package ships a static binary, so every crate it links is distributed with it, and the
//! permissive licences most of them use require their notices to go along.  The list in
//! `packaging/deb/copyright` is written by hand, so this test reads the binary's real dependency
//! graph for each packaged target and fails naming any crate the file does not mention.

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
            // "mimalloc v0.1.52", or "quaffed v0.1.0 (/path)" for the workspace's own crate.
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let version = fields.next()?.strip_prefix('v')?;
            Some((name.to_owned(), version.to_owned()))
        })
        .filter(|(name, _)| name != "quaffed")
        .collect();
    crates.sort();
    crates.dedup();
    crates
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
            if !folded.contains(&crate_) {
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
