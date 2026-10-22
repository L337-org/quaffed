//! What the adapter's corpus tests share: where the synthetic corpus is, its three groups of
//! files, and the floor under which a group means the corpus was not found.

use std::fs;
use std::path::{Path, PathBuf};

/// One group of the synthetic corpus: a directory, the extension its files carry, the least
/// number of files that means it was found, and how its files are parsed.
pub struct Group {
    pub dir: &'static str,
    pub extension: &'static str,
    pub floor: usize,
    pub mode: Mode,
}

/// How a file is parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Module,
    Expression,
    Ipython,
}

/// The synthetic corpus's groups.  Fewer files than a floor means the corpus was not found or
/// was emptied, not that it is complete.
pub const GROUPS: [Group; 3] = [
    Group {
        dir: "modules",
        extension: "py",
        floor: 8,
        mode: Mode::Module,
    },
    Group {
        dir: "expressions",
        extension: "pyexpr",
        floor: 2,
        mode: Mode::Expression,
    },
    Group {
        dir: "ipython",
        extension: "ipy",
        floor: 1,
        mode: Mode::Ipython,
    },
];

/// The workspace root.
pub fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/python; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/python has a grandparent")
        .to_path_buf()
}

/// The synthetic corpus.
pub fn synthetic() -> PathBuf {
    workspace_root().join("corpus/synthetic")
}

/// The files of `group`, sorted so failures read the same on every run, failing if there are
/// fewer than its floor.
pub fn group_files(group: &Group) -> Vec<PathBuf> {
    let dir = synthetic().join(group.dir);
    let found = files(&dir, group.extension);
    assert!(
        found.len() >= group.floor,
        "found {} {} files under {}, fewer than the floor of {}",
        found.len(),
        group.dir,
        dir.display(),
        group.floor
    );
    found
}

/// The files directly under `dir` with `extension`, sorted.
pub fn files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()))
        .map(|entry| entry.expect("reading a directory entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .collect();
    found.sort();
    found
}
