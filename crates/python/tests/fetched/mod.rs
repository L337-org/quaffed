//! A source `scripts/fetch-corpus` fetched under `target/corpus`, as the real-world tests read
//! it: its directory, the files the oracle refused, and how many it read.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One fetched source under `target/corpus`: its directory, the paths the oracle refuses with
/// its reasons, and how many files the oracle read.
pub struct Fetched {
    pub dir: PathBuf,
    pub refused: BTreeMap<String, String>,
    pub read: usize,
}

/// Reads `refused.txt` in `dir`, as `scripts/fetch-corpus` writes it: a header line counting
/// the files read, then a path, a tab and the oracle's reason per refused file.
pub fn fetched(dir: &Path) -> Result<Fetched, String> {
    let list = dir.join("refused.txt");
    let text =
        fs::read_to_string(&list).map_err(|err| format!("reading {}: {err}", list.display()))?;
    let mut lines = text.lines();
    let read = lines
        .next()
        .and_then(|header| header.strip_prefix("# "))
        .and_then(|header| header.split(' ').next())
        .and_then(|count| count.parse().ok())
        .ok_or_else(|| {
            format!(
                "{} does not open with the count of files read",
                list.display()
            )
        })?;
    let refused = lines
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let (path, reason) = line
                .split_once('\t')
                .ok_or_else(|| format!("{}: {line:?} has no tab", list.display()))?;
            Ok((path.to_owned(), reason.to_owned()))
        })
        .collect::<Result<_, String>>()?;
    Ok(Fetched {
        dir: dir.to_path_buf(),
        refused,
        read,
    })
}

/// Every `.py` file under `dir`, as paths relative to it with `/` between parts, sorted.
pub fn python_files(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries =
            fs::read_dir(&next).unwrap_or_else(|err| panic!("listing {}: {err}", next.display()));
        for entry in entries {
            let path = entry.expect("reading a directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "py") {
                let relative = path.strip_prefix(dir).expect("under the source");
                let parts: Vec<_> = relative.iter().map(|p| p.to_string_lossy()).collect();
                found.push(parts.join("/"));
            }
        }
    }
    found.sort();
    found
}

/// Every fetched source under `root`, sorted: each directory holding a `refused.txt`.
///
/// # Panics
///
/// If `root` cannot be listed or holds no fetched source, saying to run `scripts/fetch-corpus`.
pub fn sources(root: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(root).unwrap_or_else(|err| {
        panic!(
            "listing {}: {err}.  Run scripts/fetch-corpus first",
            root.display()
        )
    });
    let mut sources: Vec<PathBuf> = entries
        .map(|entry| entry.expect("reading a directory entry").path())
        .filter(|dir| dir.join("refused.txt").is_file())
        .collect();
    sources.sort();
    assert!(
        !sources.is_empty(),
        "no fetched source under {}: run scripts/fetch-corpus first",
        root.display()
    );
    sources
}
