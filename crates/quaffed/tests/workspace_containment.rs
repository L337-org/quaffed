//! Only the workspace module reaches git's own files.
//!
//! Everything quaff takes from a version control system goes through `src/workspace/`, so that
//! supporting another system means adding an implementation there, not finding git assumed
//! elsewhere.  This reads every other source file of the `quaff` binary and fails naming any
//! that reaches one of git's files or settings itself.  `discover.rs`'s list of every system's
//! metadata directories, skipped by name alike, is not git's and is not caught.  A test may build
//! a tree holding git's files, whether its module is inside a file or a file of its own.

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
    ".git_global(",
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

/// Whether `path` is a test module in a file of its own, as its parent module declares it:
/// `#[cfg(test)]` and then `mod <name>;`.  A test may read freely, as the test module inside a
/// file may, and is left out whole.  Known by its declaration rather than its name, so that no
/// product file is left out for being named like one.
fn is_test_module(path: &Path) -> bool {
    let (Some(name), Some(dir)) = (path.file_stem(), path.parent()) else {
        return false;
    };
    let declared = format!("#[cfg(test)]\nmod {};", name.to_string_lossy());
    [
        dir.with_extension("rs"),
        dir.join("mod.rs"),
        dir.join("main.rs"),
        dir.join("lib.rs"),
    ]
    .iter()
    .any(|parent| fs::read_to_string(parent).is_ok_and(|text| text.contains(&declared)))
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
            && !is_test_module(&path)
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

#[test]
fn a_test_module_in_a_file_of_its_own_is_known_by_its_parents_declaration() {
    let dir = std::env::temp_dir().join(format!("quaffed-test-module-{}", std::process::id()));
    fs::create_dir_all(dir.join("parent")).expect("creating the directory");
    fs::write(
        dir.join("parent.rs"),
        "mod tested;\n#[cfg(test)]\nmod tests;\n",
    )
    .expect("writing");
    let tests = is_test_module(&dir.join("parent/tests.rs"));
    let tested = is_test_module(&dir.join("parent/tested.rs"));
    let elsewhere = is_test_module(&dir.join("tests.rs"));
    let _ = fs::remove_dir_all(&dir);
    assert!(tests, "a file its parent declares a test module is one");
    assert!(
        !tested,
        "a file its parent declares a plain module is product code"
    );
    assert!(
        !elsewhere,
        "a file named tests.rs that nothing declares is product code"
    );
}
