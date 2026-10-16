//! Only the workspace module reaches git's own files.
//!
//! Everything quaff takes from a version control system goes through `src/workspace/`, so that
//! supporting another system means adding an implementation there, not finding git assumed
//! elsewhere.  This reads every other source file of the `quaff` binary and fails naming any
//! that reaches one of git's files or settings itself.  `discover.rs`'s list of every system's
//! metadata directories, skipped by name alike, is not git's and is not caught.

use std::fs;
use std::path::{Path, PathBuf};

/// What reaching git directly looks like in source: its attributes and ignore files, its
/// pointer files, and the walker's git switches.
const GIT_ONLY: &[&str] = &[
    "gitattributes",
    "info/attributes",
    "gitdir:",
    ".git_ignore(",
    ".git_exclude(",
    "MARKER",
];

/// The lines of `source` that reach git directly, outside comments and tests: a test may build
/// a tree holding git's files.
fn reaches_git(source: &str) -> Vec<&str> {
    let product = source.split("#[cfg(test)]").next().unwrap_or_default();
    product
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| GIT_ONLY.iter().any(|needle| line.contains(needle)))
        .collect()
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {dir:?}: {err}")) {
        let path = entry.expect("reading a directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "workspace") {
                continue;
            }
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|name| name != "workspace.rs")
        {
            found.push(path);
        }
    }
}

#[test]
fn only_the_workspace_module_reaches_git() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    sources(&src, &mut found);
    // A walk that read nothing would pass having checked nothing.
    assert!(found.len() >= 5, "found only {found:?} under {src:?}");
    let mut offenders = Vec::new();
    for path in &found {
        let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
        for line in reaches_git(&text) {
            offenders.push(format!("{}: {}", path.display(), line.trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these reach git directly, which only src/workspace/ may; go through the Workspace \
         trait instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn reaching_git_is_found_however_it_is_done() {
    assert_eq!(
        reaches_git("    walk.git_ignore(true);"),
        ["    walk.git_ignore(true);"]
    );
    assert_eq!(
        reaches_git(r#"let f = root.join(".gitattributes");"#).len(),
        1
    );
    assert_eq!(
        reaches_git(r#"let x = text.strip_prefix("gitdir:");"#).len(),
        1
    );
    // A comment may name git's files: describing a rule is not reaching around it.
    assert_eq!(
        reaches_git("// .gitattributes is the workspace's"),
        Vec::<&str>::new()
    );
    assert_eq!(
        reaches_git("let name = \".git\"; // in a list of every system's"),
        Vec::<&str>::new()
    );
    assert_eq!(
        reaches_git("fn f() {}\n#[cfg(test)]\nmod tests { const A: &str = \".gitattributes\"; }"),
        Vec::<&str>::new()
    );
}
