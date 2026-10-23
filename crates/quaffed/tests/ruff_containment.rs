//! Only the Python adapter depends on a `ruff_*` crate, and it pins each one exactly.
//!
//! Ruff says of its crates that their API "will have frequent breaking changes", so they are
//! confined to `crates/python`, where an upgrade's damage stops, and each is pinned with `=` to
//! one version, the same for all of them, so that an upgrade is deliberate work and never a
//! lock-file refresh.  These tests read every member's `Cargo.toml` and fail naming any other
//! member that depends on one, under any section or renamed with `package =`, and any of the
//! adapter's that is not pinned so.  `architecture/python.md` says why.

use std::collections::BTreeSet;
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

/// Each `ruff_*` dependency the manifest text declares on a line of its own, with the version
/// it asks for: the string itself, or a table's `version`, or `None` for a table without one.
fn ruff_versions(manifest: &str) -> Vec<(String, Option<String>)> {
    let quoted = |text: &str| -> Option<String> {
        let start = text.find('"')? + 1;
        let end = start + text[start..].find('"')?;
        Some(text[start..end].to_owned())
    };
    manifest
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim().trim_matches(['"', '\'']);
            if !key.starts_with("ruff_") {
                return None;
            }
            let value = value.trim();
            let version = if value.starts_with('{') {
                value
                    .split_once("version")
                    .and_then(|(_, rest)| rest.trim_start().strip_prefix('='))
                    .and_then(quoted)
            } else {
                quoted(value)
            };
            Some((key.to_owned(), version))
        })
        .collect()
}

/// Why the adapter's `ruff_*` pins are not exact and alike, if they are not.
fn pin_problems(manifest: &str) -> Vec<String> {
    let versions = ruff_versions(manifest);
    let mut problems = Vec::new();
    if versions.is_empty() {
        problems.push("no ruff_* dependency found, so nothing was checked".to_owned());
    }
    for (name, version) in &versions {
        match version {
            Some(version) if version.starts_with('=') => {}
            Some(version) => problems.push(format!(
                "{name} asks for {version:?}, which is not an exact pin: write \"={}\"",
                version.trim_start_matches(['^', '~', '>', '<', '='])
            )),
            None => problems.push(format!("{name} names no version, so nothing pins it")),
        }
    }
    // Compared by the version alone, so that `0.0.18` beside `=0.0.18` is one loose pin rather
    // than two versions.
    let pinned: BTreeSet<&str> = versions
        .iter()
        .filter_map(|(_, version)| version.as_deref())
        .map(|version| version.trim_start_matches(['^', '~', '>', '<', '=']))
        .collect();
    if pinned.len() > 1 {
        problems.push(format!(
            "the ruff_* crates are pinned to different versions, {pinned:?}: upgrade them together"
        ));
    }
    problems
}

#[test]
fn the_adapter_pins_every_ruff_crate_exactly_and_alike() {
    let manifest = workspace_root()
        .join("crates")
        .join(ADAPTER)
        .join("Cargo.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap_or_else(|err| panic!("reading {}: {err}", manifest.display()));
    let problems = pin_problems(&text);
    assert!(
        problems.is_empty(),
        "{}: {}.  architecture/python.md has the upgrade checklist",
        manifest.display(),
        problems.join("; ")
    );
}

#[test]
fn a_loosened_or_split_pin_is_found_however_it_is_declared() {
    let exact = "[dependencies]\nruff_python_ast = \"=0.0.18\"\nruff_text_size = { version = \"=0.0.18\" }\n";
    assert_eq!(pin_problems(exact), Vec::<String>::new());
    for loose in [
        "ruff_python_ast = \"0.0.18\"",
        "ruff_python_ast = \"^0.0.18\"",
        "ruff_python_ast = \"~0.0.18\"",
        "ruff_python_ast = \">=0.0.18\"",
        "ruff_python_ast = { version = \"0.0.18\", default-features = false }",
    ] {
        let problems = pin_problems(&format!("[dependencies]\n{loose}\n"));
        assert_eq!(problems.len(), 1, "{loose}: {problems:?}");
        assert!(
            problems[0].contains("not an exact pin"),
            "{loose}: {problems:?}"
        );
    }
    let unversioned = pin_problems("[dependencies]\nruff_python_ast = { path = \"../ruff\" }\n");
    assert!(
        unversioned[0].contains("names no version"),
        "{unversioned:?}"
    );
    let split = pin_problems(
        "[dependencies]\nruff_python_ast = \"=0.0.18\"\nruff_python_parser = \"=0.0.19\"\n",
    );
    assert!(split[0].contains("different versions"), "{split:?}");
    assert!(pin_problems("[dependencies]\nmemchr = \"=2\"\n")[0].contains("nothing was checked"));
}
