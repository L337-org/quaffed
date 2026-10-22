//! `quaffed-syntax` depends on nothing, so that no parser reaches it.
//!
//! The arena is the one shape every language's adapter builds, and the reason it has a crate of
//! its own is that a second language's adapter can depend on it without depending on another
//! language's parser.  `ruff_containment.rs` keeps Ruff out; this keeps every other parser out
//! too, by failing on any dependency at all.  A dependency that is not a parser would be added
//! here deliberately, by name.  `architecture/syntax.md` says why.

use std::fs;
use std::path::Path;

/// Dependencies the crate may have.  None, so far.
const ALLOWED: &[&str] = &[];

/// The dependencies the manifest text declares, by name, in `[dependencies]`,
/// `[build-dependencies]` or a `[dependencies.<name>]` table, target-specific ones included.
fn dependencies(manifest: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_section = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            let header = line.trim_matches(['[', ']']);
            let section = header.rsplit('.').next().unwrap_or_default();
            if let Some(name) = header
                .strip_suffix(section)
                .and_then(|rest| rest.strip_suffix('.'))
                .filter(|rest| {
                    rest.ends_with("dependencies") && !rest.ends_with("dev-dependencies")
                })
                .map(|_| section)
            {
                found.push(name.trim_matches('"').to_owned());
                in_section = false;
            } else {
                in_section =
                    header.ends_with("dependencies") && !header.ends_with("dev-dependencies");
            }
        } else if in_section && !line.is_empty() && !line.starts_with('#') {
            let key = line.split(['=', ' ', '.']).next().unwrap_or_default();
            found.push(key.trim_matches(['"', '\'']).to_owned());
        }
    }
    found
}

#[test]
fn the_crate_depends_on_nothing_but_what_is_allowed() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    assert!(
        manifest.contains("name = \"quaffed-syntax\""),
        "read the wrong manifest: {}",
        path.display()
    );
    let extra: Vec<_> = dependencies(&manifest)
        .into_iter()
        .filter(|name| !ALLOWED.contains(&name.as_str()))
        .collect();
    assert!(
        extra.is_empty(),
        "quaffed-syntax depends on {}, but must depend on no parser; a dependency that is not one \
         goes in ALLOWED in this test, by name",
        extra.join(", ")
    );
}

#[test]
fn a_dependency_is_found_however_it_is_declared() {
    assert_eq!(
        dependencies("[dependencies]\ntree-sitter = \"0.25\"\n"),
        ["tree-sitter"]
    );
    assert_eq!(
        dependencies("[dependencies]\nfoo.workspace = true\n"),
        ["foo"]
    );
    assert_eq!(
        dependencies("[dependencies.bar]\nversion = \"1\"\n"),
        ["bar"]
    );
    assert_eq!(
        dependencies("[target.'cfg(unix)'.dependencies]\nbaz = \"1\"\n"),
        ["baz"]
    );
    assert_eq!(dependencies("[build-dependencies]\ncc = \"1\"\n"), ["cc"]);
    assert_eq!(
        dependencies(
            "[package]\nname = \"x\"\n[dev-dependencies]\nqux = \"1\"\n[lints]\nworkspace = true\n"
        ),
        Vec::<String>::new()
    );
}
