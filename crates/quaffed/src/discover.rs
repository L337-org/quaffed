// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Which files a search looks at.
//!
//! A discovery filter governs where quaff looks, never what it may touch.  The ignore files read
//! are `.ignore`, and the version control system's own - for git, `.gitignore` and
//! `.git/info/exclude` - through the `ignore` crate, ripgrep's walker, and nothing else of the
//! system's: no configuration, so no global excludes file.  Hidden files are searched.
//! Version-control metadata is skipped by name.

use std::fmt;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::workspace::{self, Workspace};

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
/// asking for it.  Naming an ignored directory crosses only that one boundary: every ignore
/// rule from the boundary down still applies to what is beneath it, so a file or directory
/// inside that another rule ignores is not searched.  No ignore file above the boundary - the
/// project root, or `cwd` with no project - applies, so that nothing outside the project changes
/// which files are searched.
///
/// # Errors
///
/// Returns an [`Error`] for anything the walker cannot read - a directory, or an ignore file
/// it could not parse - rather than searching the rest and reporting a short answer as complete.
pub fn files(scope: &Path, cwd: &Path, workspace: Option<&dyn Workspace>) -> Result<Found, Error> {
    let boundary = workspace.map_or(cwd, |w| w.root());
    let mut found = Found::default();
    // The walker reads ignore files from every directory above where it starts, up to the
    // filesystem root, and has no way to stop at the project's.  So it starts at the boundary,
    // with no parents read, and is narrowed to the scope as it goes: the ignore files between
    // the boundary and the scope still apply, as they are met on the way down, and none above
    // the boundary is ever consulted.  A file scope is walked on its own, as naming it is
    // asking for it whatever ignores it.
    let saw_scope = if scope.is_dir() {
        walk(boundary, Some(scope), &[], cwd, workspace, &mut found)?
    } else {
        walk(scope, None, &[], cwd, workspace, &mut found)?
    };
    // An ignored scope directory is never reached from the boundary, so it is walked from
    // itself.  The walker would then apply only the rules from the scope down, so the rules
    // between the boundary and the scope are read here and applied beneath it too.
    if !saw_scope {
        let above = rules_above(scope, boundary, cwd, workspace, &mut found)?;
        walk(scope, None, &above, cwd, workspace, &mut found)?;
    }
    found.files.sort();
    Ok(found)
}

/// An error from reading ignore files, tolerated if it is only lines that could not be parsed:
/// each is noted in `found` and the rest of its file applies.  Anything else - a directory that
/// cannot be listed, an ignore file that cannot be read - stops the run rather than answering
/// short.
///
/// It takes the error by value: a clone of one loses the system's error inside it, which the
/// message quotes.
fn tolerate(cause: ignore::Error, cwd: &Path, found: &mut Found) -> Result<(), Error> {
    if !is_rule_error(&cause) {
        return Err(Error {
            cause,
            cwd: cwd.to_path_buf(),
        });
    }
    for each in each_error(&cause) {
        let described = describe(each, cwd);
        if !found.skipped_rules.contains(&described) {
            found.skipped_rules.push(described);
        }
    }
    Ok(())
}

/// The ignore files in each directory from the one holding `scope` up to `boundary`, read as
/// matchers, the one that takes precedence first: nearer directories before further ones, and
/// within a directory `.ignore` before the workspace's own.  A line that cannot be used is noted
/// in `found`, as the walker notes its own.
fn rules_above(
    scope: &Path,
    boundary: &Path,
    cwd: &Path,
    workspace: Option<&dyn Workspace>,
    found: &mut Found,
) -> Result<Vec<Gitignore>, Error> {
    let mut rules = Vec::new();
    for dir in scope
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(boundary))
    {
        let mut files = vec![dir.join(".ignore")];
        if let Some(workspace) = workspace {
            let declared = workspace.ignore_files(dir).map_err(|err| Error {
                cause: ignore::Error::WithPath {
                    path: err.path,
                    err: Box::new(ignore::Error::Io(err.cause)),
                },
                cwd: cwd.to_path_buf(),
            })?;
            files.extend(declared);
        }
        for file in files.into_iter().filter(|file| file.is_file()) {
            let mut builder = GitignoreBuilder::new(dir);
            if let Some(cause) = builder.add(&file) {
                tolerate(cause, cwd, found)?;
            }
            match builder.build() {
                Ok(matcher) => rules.push(matcher),
                Err(cause) => tolerate(cause, cwd, found)?,
            }
        }
    }
    Ok(rules)
}

/// Walks from `start`, adding what is found to `found`: everything, or with `within`, only
/// what is at or under it, and the directories on the way to it.  Below `start`, an entry the
/// first of `above` to match ignores is left out, with what is under it.  Returns whether
/// `within`, if given, was reached.
fn walk(
    start: &Path,
    within: Option<&Path>,
    above: &[Gitignore],
    cwd: &Path,
    workspace: Option<&dyn Workspace>,
    found: &mut Found,
) -> Result<bool, Error> {
    let mut walker = ignore::WalkBuilder::new(start);
    let narrow_to = within.map(Path::to_path_buf);
    let above = above.to_vec();
    walker
        .hidden(false)
        .parents(false)
        .ignore(true)
        .follow_links(false)
        .sort_by_file_name(std::cmp::Ord::cmp)
        .filter_entry(move |entry| {
            let name_ok = entry
                .file_name()
                .to_str()
                .is_none_or(|name| !VCS_METADATA.contains(&name));
            let path = entry.path();
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            // The first rule that says anything about the entry decides, as the walker decides
            // between its own; `start` itself is the boundary being crossed, so never left out.
            let kept_by_above = entry.depth() == 0
                || above
                    .iter()
                    .map(|rules| rules.matched(path, is_dir))
                    .find(|matched| !matched.is_none())
                    .is_none_or(|matched| !matched.is_ignore());
            name_ok
                && kept_by_above
                && narrow_to
                    .as_deref()
                    .is_none_or(|scope| path.starts_with(scope) || scope.starts_with(path))
        });
    match workspace {
        Some(workspace) => workspace.ignore_rules(&mut walker),
        None => workspace::no_system_ignore_rules(&mut walker),
    }
    let mut reached = within.is_none();
    for entry in walker.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(cause) => {
                tolerate(cause, cwd, found)?;
                continue;
            }
        };
        if let Some(cause) = entry.error() {
            tolerate(cause.clone(), cwd, found)?;
        }
        let Some(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if within.is_some_and(|scope| path == scope) {
            reached = true;
        }
        // On the way down to the scope, only what is inside it counts.
        if within.is_some_and(|scope| !path.starts_with(scope)) {
            continue;
        }
        if kind.is_symlink() {
            found.links_not_followed += 1;
        } else if kind.is_file() {
            found.files.push(entry.into_path());
        } else if !kind.is_dir() {
            found.special_files += 1;
        }
    }
    Ok(reached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Discovery as a run does it: with the workspace found from `cwd`, if there is one.
    fn files_in(scope: &Path, cwd: &Path) -> Result<Found, Error> {
        let workspace = workspace::find(cwd);
        files(scope, cwd, workspace.as_deref())
    }

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
            files_in(&self.0.join(scope), &self.0)
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

    /// The files found searching from `dir`, under the tree, with no scope.
    fn found_from(tree: &Tree, dir: &str) -> Vec<String> {
        let dir = tree.0.join(dir);
        files_in(&dir, &dir)
            .unwrap()
            .files
            .iter()
            .map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn no_ignore_file_above_the_project_root_applies() {
        let tree = Tree::new(
            "above-root",
            &[
                ("outer/.ignore", "*.py\n"),
                ("outer/.gitignore", "*.py\n"),
                ("outer/proj/.git/HEAD", ""),
                ("outer/proj/x.py", ""),
            ],
        );
        assert_eq!(found_from(&tree, "outer/proj"), ["x.py"]);
    }

    #[test]
    fn with_no_project_no_ignore_file_above_the_current_directory_applies() {
        let tree = Tree::new(
            "above-cwd",
            &[("outer/.ignore", "*.py\n"), ("outer/here/x.py", "")],
        );
        assert_eq!(found_from(&tree, "outer/here"), ["x.py"]);
    }

    #[test]
    fn the_ignore_files_between_the_root_and_a_scope_still_apply() {
        let tree = Tree::new(
            "between",
            &[
                (".git/HEAD", ""),
                (".gitignore", "*.log\n"),
                ("sub/.ignore", "*.tmp\n"),
                ("sub/deeper/a.log", ""),
                ("sub/deeper/b.tmp", ""),
                ("sub/deeper/c.py", ""),
                ("other/d.py", ""),
            ],
        );
        assert_eq!(tree.found("sub/deeper"), ["sub/deeper/c.py"]);
        assert_eq!(tree.found("sub"), ["sub/.ignore", "sub/deeper/c.py"]);
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
    fn naming_an_ignored_directory_crosses_only_that_boundary() {
        // build/ is named, so it is searched; every other rule from the root down still applies
        // inside it, so a .log file, a .tmp file and the cache/ directory stay out.
        let tree = Tree::new(
            "boundary",
            &[
                (".git/HEAD", ""),
                (".git/info/exclude", "*.tmp\n"),
                (".gitignore", "build/\n*.log\n"),
                (".ignore", "cache/\n"),
                ("build/a.log", ""),
                ("build/b.py", ""),
                ("build/c.tmp", ""),
                ("build/cache/d.py", ""),
                ("build/sub/e.py", ""),
                ("build/sub/f.log", ""),
            ],
        );
        assert_eq!(tree.found("build"), ["build/b.py", "build/sub/e.py"]);
        assert_eq!(tree.found("build/sub"), ["build/sub/e.py"]);
    }

    #[test]
    fn with_no_project_a_named_ignored_directory_keeps_the_other_ignore_rules() {
        let tree = Tree::new(
            "boundary-no-project",
            &[
                (".ignore", "build/\n*.log\n"),
                ("build/a.log", ""),
                ("build/b.py", ""),
            ],
        );
        assert_eq!(tree.found("build"), ["build/b.py"]);
    }

    #[test]
    fn a_bad_line_above_a_named_ignored_directory_is_skipped_with_a_note() {
        let tree = Tree::new(
            "boundary-bad-line",
            &[
                (".git/HEAD", ""),
                (".gitignore", "build/\na{b\n*.log\n"),
                ("build/a.log", ""),
                ("build/b.py", ""),
            ],
        );
        // Canonical, as a run passes them: the temporary directory may be reached through a
        // symbolic link, and the walker names files by the path it was given.
        let root = fs::canonicalize(&tree.0).unwrap();
        let found = files_in(&root.join("build"), &root).unwrap();
        let names: Vec<_> = found
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        // The good lines still apply beneath it; the bad one is noted once, though both the
        // walker and the rules read above the scope met it.
        assert_eq!(names, ["build/b.py"]);
        assert_eq!(found.skipped_rules.len(), 1, "{:?}", found.skipped_rules);
        assert!(
            found.skipped_rules[0].starts_with("\".gitignore\": line 2: "),
            "{:?}",
            found.skipped_rules
        );
    }

    /// Discovery from the tree's root with `scope` named, after making `locked` unreadable; `None`
    /// if this user can read it anyway, as root can, so that the failure cannot happen.
    fn found_with_unreadable(
        tree: &Tree,
        scope: &str,
        locked: &str,
    ) -> Option<Result<Found, Error>> {
        use std::os::unix::fs::PermissionsExt;
        let root = fs::canonicalize(&tree.0).unwrap();
        let locked = root.join(locked);
        let mode = fs::metadata(&locked).unwrap().permissions().mode();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = if locked.is_dir() {
            fs::read_dir(&locked).is_ok()
        } else {
            fs::read(&locked).is_ok()
        };
        let result = files_in(&root.join(scope), &root);
        fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();
        if readable_anyway {
            eprintln!("skipped: this user can read a mode-000 path, so it cannot fail");
            return None;
        }
        Some(result)
    }

    #[test]
    fn an_unreadable_ignore_file_above_a_named_ignored_scope_stops_discovery() {
        // a/ is ignored, so the walk from the root never reads a/.ignore; the rules read above
        // the scope a/b do, and must not pass over it.
        let tree = Tree::new(
            "above-unreadable",
            &[
                (".git/HEAD", ""),
                (".gitignore", "a/\n"),
                ("a/.ignore", "*.log\n"),
                ("a/b/x.py", ""),
            ],
        );
        if let Some(result) = found_with_unreadable(&tree, "a/b", "a/.ignore") {
            let message = result.expect_err("must stop").to_string();
            assert!(message.contains("a/.ignore"), "{message}");
            assert!(message.contains("Permission denied"), "{message}");
        }
    }

    #[test]
    fn a_workspace_that_cannot_say_where_its_ignore_files_are_stops_discovery() {
        // a/ is ignored and holds a nested repository whose .git file cannot be read, so where
        // its info/exclude is cannot be known.
        let tree = Tree::new(
            "above-unreadable-git",
            &[
                (".git/HEAD", ""),
                (".gitignore", "a/\n"),
                ("a/.git", "gitdir: elsewhere\n"),
                ("a/b/x.py", ""),
            ],
        );
        if let Some(result) = found_with_unreadable(&tree, "a/b", "a/.git") {
            let message = result.expect_err("must stop").to_string();
            assert!(message.contains("a/.git"), "{message}");
            assert!(message.contains("Permission denied"), "{message}");
        }
    }

    #[test]
    fn an_unreadable_directory_beneath_a_named_ignored_scope_stops_discovery() {
        let tree = Tree::new(
            "beneath-unreadable",
            &[
                (".git/HEAD", ""),
                (".gitignore", "build/\n"),
                ("build/locked/x.py", ""),
            ],
        );
        if let Some(result) = found_with_unreadable(&tree, "build", "build/locked") {
            let message = result.expect_err("must stop").to_string();
            assert!(message.contains("build/locked"), "{message}");
            assert!(message.contains("Permission denied"), "{message}");
        }
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
        let found = files_in(&tree.0, &tree.0).unwrap();
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
        let found = files_in(&tree.0, &tree.0).unwrap();
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
        let found = files_in(&tree.0.join("sub"), &tree.0).unwrap();
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
        let found = files_in(&tree.0, &tree.0).unwrap();
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
        let result = files_in(&tree.0, &tree.0);
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
