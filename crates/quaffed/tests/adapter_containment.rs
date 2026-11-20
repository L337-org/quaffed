// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Only `src/languages.rs` names a language's adapter.
//!
//! The binary reaches every language through its front end, the `FrontEnd` trait in
//! `quaffed-syntax`, and finds it in the list in `src/languages.rs`, so that a language is added
//! there and nothing else in the binary changes.  A line anywhere else that named an adapter
//! crate would reach one language directly, and the next language would have to change it.  This
//! reads every other source and test file of the binary, test modules included, and fails naming
//! each line that does, comments aside.  `architecture/syntax.md` says why.

use std::fs;
use std::path::{Path, PathBuf};

/// Every language adapter crate, as Rust code names it.
const ADAPTERS: &[&str] = &["quaffed_python"];

/// The one file allowed to name them, under `src/`.
const REGISTRY: &str = "languages.rs";

/// The lines of `source` that name an adapter crate, comments aside.
fn naming_an_adapter(source: &str) -> Vec<&str> {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| ADAPTERS.iter().any(|adapter| line.contains(adapter)))
        .collect()
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {dir:?}: {err}")) {
        let path = entry.expect("reading a directory entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn only_the_registry_names_an_adapter() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let registry = root.join("src").join(REGISTRY);
    let mut found = Vec::new();
    sources(&root.join("src"), &mut found);
    sources(&root.join("tests"), &mut found);
    // A walk that missed the registry, or found nothing else, would pass having checked nothing.
    assert!(
        found.contains(&registry),
        "did not find {}",
        registry.display()
    );
    assert!(found.len() > 10, "found only {found:?}");
    let this = Path::new(file!())
        .file_name()
        .expect("this test has a file name");
    let mut offenders = Vec::new();
    for path in found
        .iter()
        .filter(|path| **path != registry && path.file_name() != Some(this))
    {
        let source = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        for line in naming_an_adapter(&source) {
            offenders.push(format!("{}: {}", path.display(), line.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these lines name a language's adapter, which only src/{REGISTRY} may: reach the \
         language through its front end instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_registry_names_every_adapter_the_binary_depends_on() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("reading Cargo.toml");
    let registry =
        fs::read_to_string(root.join("src").join(REGISTRY)).expect("reading the registry");
    for adapter in ADAPTERS {
        let package = adapter.replace('_', "-");
        assert!(
            manifest.contains(&format!("{package} =")),
            "Cargo.toml does not depend on {package}: ADAPTERS in this test is out of date"
        );
        assert!(
            naming_an_adapter(&registry)
                .iter()
                .any(|line| line.contains(adapter)),
            "src/{REGISTRY} does not name {adapter}"
        );
    }
}

#[test]
fn a_named_adapter_is_found_and_a_comment_is_not() {
    assert_eq!(
        naming_an_adapter("use quaffed_python::Python;\n"),
        ["use quaffed_python::Python;"]
    );
    assert_eq!(
        naming_an_adapter("    let p = quaffed_python::parse_module(t);\n"),
        ["    let p = quaffed_python::parse_module(t);"]
    );
    assert_eq!(
        naming_an_adapter("use quaffed_python as py;\n"),
        ["use quaffed_python as py;"]
    );
    assert_eq!(
        naming_an_adapter("// as `quaffed_python` does\n/// quaffed_python\n"),
        Vec::<&str>::new()
    );
    assert_eq!(
        naming_an_adapter("use quaffed_syntax::FrontEnd;\n"),
        Vec::<&str>::new()
    );
}
