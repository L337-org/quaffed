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
    /// FIFOs, sockets and device files met and not searched: reading a FIFO would wait for a
    /// writer that may never come, so they are counted rather than read.
    pub special_files: usize,
    /// Each line of an ignore file that could not be parsed, described for the user.  The line
    /// is skipped and the rest of its file applies, as git applies the lines it understands.
    pub skipped_rules: Vec<String>,
}

/// Discovery could not finish, and a search over part of the scope must not pass for a search
/// over all of it.
#[derive(Debug)]
pub struct Error {
    /// What the walker reported, verbatim.
    pub cause: ignore::Error,
    /// The directory paths in the message are made relative to, as every other message's are.
    pub cwd: PathBuf,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "finding the files to search: {}",
            describe(&self.cause, &self.cwd)
        )
    }
}

/// Whether `err` is only ignore-file lines that could not be parsed, which cost one rule each,
/// rather than something that stops the walk.
fn is_rule_error(err: &ignore::Error) -> bool {
    match err {
        ignore::Error::Glob { .. } => true,
        ignore::Error::WithPath { err, .. }
        | ignore::Error::WithLineNumber { err, .. }
        | ignore::Error::WithDepth { err, .. } => is_rule_error(err),
        ignore::Error::Partial(errs) => errs.iter().all(is_rule_error),
        _ => false,
    }
}

/// The separate errors inside `err`: one per unparseable line, however the walker grouped them.
fn each_error(err: &ignore::Error) -> Vec<&ignore::Error> {
    match err {
        ignore::Error::Partial(errs) => errs.iter().flat_map(each_error).collect(),
        ignore::Error::WithDepth { err, .. } => each_error(err),
        other => vec![other],
    }
}

/// The walker's error in quaff's form: a path relative to `cwd` and quoted, a line number
/// where there is one, and the underlying error's own words.
fn describe(err: &ignore::Error, cwd: &Path) -> String {
    match err {
        ignore::Error::WithPath { path, err } => {
            format!(
                "{:?}: {}",
                crate::output::relative(path, cwd),
                describe(err, cwd)
            )
        }
        ignore::Error::WithLineNumber { line, err } => {
            format!("line {line}: {}", describe(err, cwd))
        }
        ignore::Error::WithDepth { err, .. } => describe(err, cwd),
        ignore::Error::Partial(errs) => errs
            .iter()
            .map(|err| describe(err, cwd))
            .collect::<Vec<_>>()
            .join("; "),
        // walkdir wraps the system's error in one of its own, whose message repeats the
        // absolute path already given above; the system's error is the one to quote.
        ignore::Error::Io(io) => io
            .get_ref()
            .and_then(std::error::Error::source)
            .map_or_else(|| io.to_string(), ToString::to_string),
        other => other.to_string(),
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
pub fn files(scope: &Path, cwd: &Path) -> Result<Found, Error> {
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
        // An ignore file line that could not be parsed costs that one rule: say so, and carry
        // on with the rest.  Anything else - a directory that cannot be listed, an ignore file
        // that cannot be read - stops the run rather than answering short.
        let mut skipped = |cause: &ignore::Error| {
            for each in each_error(cause) {
                let described = describe(each, cwd);
                if !found.skipped_rules.contains(&described) {
                    found.skipped_rules.push(described);
                }
            }
        };
        let entry = match entry {
            Ok(entry) => entry,
            Err(cause) if is_rule_error(&cause) => {
                skipped(&cause);
                continue;
            }
            Err(cause) => {
                return Err(Error {
                    cause,
                    cwd: cwd.to_path_buf(),
                });
            }
        };
        if let Some(cause) = entry.error() {
            if !is_rule_error(cause) {
                return Err(Error {
                    cause: cause.clone(),
                    cwd: cwd.to_path_buf(),
                });
            }
            skipped(cause);
        }
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            found.links_not_followed += 1;
        } else if kind.is_file() {
            found.files.push(entry.into_path());
        } else if !kind.is_dir() {
            found.special_files += 1;
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
            files(&self.0.join(scope), &self.0)
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
    fn special_files_are_counted_not_read() {
        let tree = Tree::new("special", &[("a.py", "")]);
        let fifo = tree.0.join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("running mkfifo");
        assert!(made.success(), "mkfifo {} failed", fifo.display());
        let found = files(&tree.0, &tree.0).unwrap();
        assert_eq!(found.files.len(), 1);
        assert_eq!(found.special_files, 1);
    }

    #[test]
    fn an_unparseable_ignore_line_is_skipped_and_the_rest_of_the_file_applies() {
        let tree = Tree::new(
            "badglob",
            &[
                (".git/HEAD", ""),
                (".gitignore", "ok.log\na{b\n[z-a]\n"),
                ("ok.log", ""),
                ("a{b", ""),
                ("kept.py", ""),
            ],
        );
        let found = files(&tree.0, &tree.0).unwrap();
        let names: Vec<_> = found
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(&tree.0)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        // The good line still ignores ok.log; the skipped one ignores nothing.
        assert_eq!(names, [".gitignore", "a{b", "kept.py"]);
        assert_eq!(found.skipped_rules.len(), 2, "{:?}", found.skipped_rules);
        assert!(
            found.skipped_rules[0].starts_with("\".gitignore\": line 2: "),
            "{:?}",
            found.skipped_rules
        );
        assert!(
            found.skipped_rules[1].starts_with("\".gitignore\": line 3: "),
            "{:?}",
            found.skipped_rules
        );
    }

    #[test]
    fn an_unparseable_line_in_a_parent_ignore_file_is_skipped_too() {
        let tree = Tree::new(
            "badparent",
            &[(".git/HEAD", ""), (".gitignore", "a{b\n"), ("sub/x.py", "")],
        );
        let found = files(&tree.0.join("sub"), &tree.0).unwrap();
        assert_eq!(found.files.len(), 1);
        assert_eq!(found.skipped_rules.len(), 1, "{:?}", found.skipped_rules);
    }

    #[test]
    fn the_walker_error_stays_on_the_chain() {
        use std::error::Error as _;
        let err = Error {
            cause: ignore::Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            cwd: PathBuf::from("/"),
        };
        let source = err
            .source()
            .expect("the walker's error is the source")
            .to_string();
        assert_eq!(
            source,
            std::io::Error::from(std::io::ErrorKind::PermissionDenied).to_string()
        );
    }

    #[test]
    fn symbolic_links_are_counted_not_followed() {
        let tree = Tree::new("links", &[("a.py", "")]);
        std::os::unix::fs::symlink("a.py", tree.0.join("link.py")).unwrap();
        let found = files(&tree.0, &tree.0).unwrap();
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
        let result = files(&tree.0, &tree.0);
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
            message.starts_with("finding the files to search: \"locked\": Permission denied"),
            "{message}"
        );
    }
}
