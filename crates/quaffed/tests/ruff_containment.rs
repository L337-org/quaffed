//! Only the Python adapter depends on a `ruff_*` crate.
//!
//! Ruff says of its crates that their API "will have frequent breaking changes", so they are
//! confined to `crates/python`, where an upgrade's damage stops.  This test reads every member's
//! `Cargo.toml` and fails naming any other member that depends on one, under any section or
//! renamed with `package =`.  `architecture/python.md` says why.

use std::fs;
use std::path::{Path, PathBuf};

/// The one member allowed to depend on Ruff, by its directory under `crates/`.
const ADAPTER: &str = "python";

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

/// Whether the manifest text declares a dependency on a `ruff_*` crate, by its own name, by a
/// `[...dependencies.ruff_*]` table, or renamed through `package = "ruff_*"`.
fn depends_on_ruff(manifest: &str) -> bool {
    manifest.lines().map(str::trim).any(|line| {
        // Cargo accepts a key in quotes, so the quotes are not part of the name.
        let key = line
            .split(['=', ' '])
            .next()
            .unwrap_or_default()
            .trim_matches(['"', '\'']);
        key.starts_with("ruff_")
            || (line.starts_with('[') && line.contains("dependencies.ruff_"))
            || line.replace(' ', "").contains("package=\"ruff_")
    })
}

#[test]
fn no_member_but_the_adapter_depends_on_ruff() {
    let crates = workspace_root().join("crates");
    let mut read = 0;
    let mut offenders = Vec::new();
    let members =
        fs::read_dir(&crates).unwrap_or_else(|err| panic!("listing {}: {err}", crates.display()));
    for member in members {
        let dir = member.expect("reading crates/").path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        read += 1;
        let text = fs::read_to_string(&manifest)
            .unwrap_or_else(|err| panic!("reading {}: {err}", manifest.display()));
        let name = dir
            .file_name()
            .expect("a member directory")
            .to_string_lossy()
            .into_owned();
        if name != ADAPTER && depends_on_ruff(&text) {
            offenders.push(name);
        }
    }
    // A walk that read nothing would pass having checked nothing.
    assert!(
        read > 0,
        "found no member manifests under {}",
        crates.display()
    );
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "these members depend on a ruff_* crate, which only crates/{ADAPTER} may: {}.  Go \
         through quaffed-python instead",
        offenders.join(", ")
    );
}

#[test]
fn a_ruff_dependency_is_found_however_it_is_declared() {
    assert!(depends_on_ruff(
        "[dependencies]\nruff_python_ast = \"=0.0.16\"\n"
    ));
    assert!(depends_on_ruff(
        "[dev-dependencies]\nruff_text_size = { version = \"1\" }\n"
    ));
    assert!(depends_on_ruff(
        "[dependencies.ruff_python_parser]\nversion = \"1\"\n"
    ));
    assert!(depends_on_ruff(
        "[dependencies]\nparser = { package = \"ruff_python_parser\" }\n"
    ));
    assert!(depends_on_ruff(
        "[dependencies]\n\"ruff_python_ast\" = \"1\"\n"
    ));
    assert!(depends_on_ruff(
        "[dependencies]\n'ruff_python_ast' = \"1\"\n"
    ));
    assert!(!depends_on_ruff(
        "[package]\nname = \"quaffed\"\n# ruff_ in a comment\n"
    ));
    assert!(!depends_on_ruff(
        "[dependencies]\nquaffed-python = { path = \"../python\" }\n"
    ));
}
