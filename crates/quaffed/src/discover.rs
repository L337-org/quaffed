// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Which files a search looks at.
//!
//! A discovery filter governs where quaff looks, never what it may touch.  The ignore files read
//! are `.gitignore`, `.ignore` and `.git/info/exclude`, through the `ignore` crate - ripgrep's
//! walker - and nothing else of git's: no configuration, so no global excludes file.  Hidden
//! files are searched.  Version-control metadata is skipped by name.

use std::fmt;
use std::path::{Path, PathBuf};

/// Directory and file names that hold version-control metadata, never searched.
///
/// Skipped by name wherever they appear, so a submodule's `.git` file is skipped as well as the
/// top-level `.git` directory.
pub const VCS_METADATA: &[&str] = &[
    ".git", ".hg", ".svn", ".bzr", "_darcs", ".jj", ".pijul", "CVS",
];

/// The files found under a scope.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Every regular file, sorted by path, so that output is the same on every run.
    pub files: Vec<PathBuf>,
    /// Symbolic links met and not followed, so the run can say it did not look through them.
    pub links_not_followed: usize,
}

/// Discovery could not finish, and a search over part of the scope must not pass for a search
/// over all of it.
#[derive(Debug)]
pub struct Error {
    /// What the walker reported, verbatim.
    pub cause: ignore::Error,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "finding the files to search: {}", self.cause)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Finds every file under `scope`, which may be a directory or a single file.
///
/// A scope that names a file, or an ignored directory, is searched even so: naming it is
/// asking for it.  Ignore rules apply to what is found beneath it.
///
/// # Errors
///
/// Returns an [`Error`] for anything the walker cannot read - a directory, or an ignore file
/// it could not parse - rather than searching the rest and reporting a short answer as complete.
pub fn files(scope: &Path) -> Result<Found, Error> {
    let mut walker = ignore::WalkBuilder::new(scope);
    walker
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .follow_links(false)
        .sort_by_file_name(std::cmp::Ord::cmp)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !VCS_METADATA.contains(&name))
        });
    let mut found = Found::default();
    for entry in walker.build() {
        let entry = entry.map_err(|cause| Error { cause })?;
        // An ignore file that only partly parsed is reported here, on the entry it applied to.
        if let Some(cause) = entry.error() {
            return Err(Error {
                cause: cause.clone(),
            });
        }
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            found.links_not_followed += 1;
        } else if kind.is_file() {
            found.files.push(entry.into_path());
        }
    }
    found.files.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("quaffed-discover-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            for (path, content) in files {
                let path = dir.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
            fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        fn found(&self, scope: &str) -> Vec<String> {
            files(&self.0.join(scope))
                .unwrap()
                .files
                .iter()
                .map(|p| {
                    p.strip_prefix(&self.0)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn ignore_files_are_honoured_and_hidden_files_searched() {
        let tree = Tree::new(
            "honoured",
            &[
                (".git/HEAD", "ref: refs/heads/main\n"),
                (".git/info/exclude", "excluded.txt\n"),
                (".gitignore", "build/\n"),
                (".ignore", "*.log\n"),
                (".hidden", ""),
                ("a.py", ""),
                ("build/out.py", ""),
                ("debug.log", ""),
                ("excluded.txt", ""),
                ("src/b.py", ""),
            ],
        );
        assert_eq!(
            tree.found("."),
            [".gitignore", ".hidden", ".ignore", "a.py", "src/b.py"]
        );
    }

    #[test]
    fn gitignore_needs_a_repository_but_ignore_does_not() {
        let tree = Tree::new(
            "norepo",
            &[
                (".gitignore", "a.py\n"),
                (".ignore", "b.py\n"),
                ("a.py", ""),
                ("b.py", ""),
            ],
        );
        assert_eq!(tree.found("."), [".gitignore", ".ignore", "a.py"]);
    }

    #[test]
    fn version_control_metadata_is_skipped_by_name_at_any_depth() {
        let tree = Tree::new(
            "vcs",
            &[
                (".git/config", ""),
                (".hg/store", ""),
                ("sub/.git", "gitdir: ../.git/modules/sub\n"),
                ("sub/CVS/Entries", ""),
                ("sub/x.py", ""),
            ],
        );
        assert_eq!(tree.found("."), ["sub/x.py"]);
    }

    #[test]
    fn a_named_scope_is_searched_even_if_ignored() {
        let tree = Tree::new(
            "named",
            &[
                (".git/HEAD", ""),
                (".gitignore", "build/\n"),
                ("build/out.py", ""),
            ],
        );
        assert_eq!(tree.found("build"), ["build/out.py"]);
        assert_eq!(tree.found("build/out.py"), ["build/out.py"]);
    }

    #[test]
    fn symbolic_links_are_counted_not_followed() {
        let tree = Tree::new("links", &[("a.py", "")]);
        std::os::unix::fs::symlink("a.py", tree.0.join("link.py")).unwrap();
        let found = files(&tree.0).unwrap();
        assert_eq!(found.files.len(), 1);
        assert_eq!(found.links_not_followed, 1);
    }

    #[test]
    fn an_unreadable_directory_stops_discovery_naming_it() {
        use std::os::unix::fs::PermissionsExt;
        let tree = Tree::new("unreadable", &[("locked/a.py", "")]);
        let locked = tree.0.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = fs::read_dir(&locked).is_ok();
        let result = files(&tree.0);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable_anyway {
            // Root reads a mode-000 directory, so the failure cannot happen; say so rather than
            // pass having checked nothing.
            eprintln!("skipped: this user can read a mode-000 directory, so it cannot fail");
            return;
        }
        let message = result
            .expect_err("an unreadable directory must stop discovery")
            .to_string();
        assert!(
            message.starts_with("finding the files to search: "),
            "{message}"
        );
        assert!(message.contains("locked"), "{message}");
    }
}
