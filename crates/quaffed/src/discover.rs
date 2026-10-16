// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Which files a search looks at.
//!
//! A discovery filter governs where quaff looks, never what it may touch.  The ignore files read
//! are `.ignore`, and the version control system's own as the workspace names them - for git,
//! `.gitignore` and `.git/info/exclude` - and nothing else of the system's: no configuration, so
//! no global excludes file.  They are read and matched here rather than by the `ignore` crate's
//! walker, which would read them up to the filesystem root; this reads none above the project.
//! The walk is quaff's own as well, so that a directory is listed only once it is to be entered.
//! Hidden files are searched.  Version-control metadata is skipped by name.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::contents::Contents;
use crate::workspace::{IgnoreFiles, Workspace};

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

impl Found {
    /// Counts an entry that is not a directory: a regular file to search, or a symbolic link or
    /// a special file, which are not read.
    fn count(&mut self, path: &Path, kind: fs::FileType) {
        if kind.is_symlink() {
            self.links_not_followed += 1;
        } else if kind.is_file() {
            self.files.push(path.to_path_buf());
        } else {
            self.special_files += 1;
        }
    }
}

/// Discovery could not finish, and a search over part of the scope must not pass for a search
/// over all of it.
#[derive(Debug)]
pub struct Error {
    /// What could not be read, with the system's own error, verbatim.
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

/// Whether `err` is an ignore-file line that could not be parsed, which costs that one rule,
/// rather than something that stops the walk.  Each is reported on its own, tagged with its
/// file and line, as the rules are read.
fn is_rule_error(err: &ignore::Error) -> bool {
    match err {
        ignore::Error::Glob { .. } => true,
        ignore::Error::WithPath { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            is_rule_error(err)
        }
        _ => false,
    }
}

/// A discovery error in quaff's form: a path relative to `cwd` and quoted, a line number
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
/// project root, or `cwd` with no project - is read, so that nothing outside the project changes
/// which files are searched.
///
/// # Errors
///
/// Returns an [`Error`] for anything that cannot be read - a directory, or an ignore file - rather
/// than searching the rest and reporting a short answer as complete.
pub fn files(
    scope: &Path,
    cwd: &Path,
    workspace: Option<&dyn Workspace>,
    contents: &Contents,
) -> Result<Found, Error> {
    let boundary = workspace.map_or(cwd, |w| w.root());
    let mut found = Found::default();
    // The rules are read and matched here, as each directory is met, ranked as the ignore
    // crate's walker ranks its own, and none above the boundary is read.  The walk is quaff's
    // own too: a directory is listed only once it is to be entered, so an ignored one - or one
    // beside the way to the scope - is never opened, where a walker that filters what it has
    // already listed would read every name in it.
    let rules = Rules::new(boundary, workspace.map(Workspace::ignore_files), contents);
    if scope.is_dir() {
        walk(boundary, scope, &rules, cwd, &mut found)?;
    } else {
        // A file scope is searched on its own, as naming it is asking for it whatever ignores
        // it.
        let kind = fs::symlink_metadata(scope)
            .map_err(|cause| io_error(scope, cause, cwd))?
            .file_type();
        found.count(scope, kind);
    }
    for cause in rules.take_errors() {
        tolerate(cause, cwd, &mut found)?;
    }
    found.files.sort();
    Ok(found)
}

/// Walks from `start`, the boundary, down to and through `scope`, adding what is found under
/// `scope` to `found`, in name order.  The directories on the way to the scope, and the scope,
/// are entered whatever ignores them: those are the boundaries naming the scope crosses.
/// Nothing beside the way is entered, and nothing under the scope that the rules ignore.
fn walk(
    start: &Path,
    scope: &Path,
    rules: &Rules,
    cwd: &Path,
    found: &mut Found,
) -> Result<(), Error> {
    // Each directory to list, and whether it is in the scope: the scope and everything under it
    // are, so its entries are matched against the rules without comparing each with the scope.
    let mut dirs = vec![(start.to_path_buf(), start == scope)];
    while let Some((dir, inside)) = dirs.pop() {
        #[cfg(test)]
        LISTED.with(|listed| listed.borrow_mut().push(dir.clone()));
        let mut entries = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|cause| io_error(&dir, cause, cwd))? {
            let entry = entry.map_err(|cause| io_error(&dir, cause, cwd))?;
            let path = entry.path();
            // The entry's own type: a symbolic link is a link, never what it points to.
            let kind = entry
                .file_type()
                .map_err(|cause| io_error(&path, cause, cwd))?;
            entries.push((entry.file_name(), path, kind));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let chain = inside.then(|| rules.chain(&dir));
        let mut deeper = Vec::new();
        for (name, path, kind) in entries {
            if name
                .to_str()
                .is_some_and(|name| VCS_METADATA.contains(&name))
            {
                continue;
            }
            let child_inside = if let Some(chain) = &chain {
                if Rules::ignored_in(chain, &path, kind.is_dir()) {
                    continue;
                }
                true
            } else if scope.starts_with(&path) {
                // On the way to the scope, or the scope itself: entered whatever ignores it.
                path == scope
            } else {
                // Beside the way to the scope.
                continue;
            };
            if kind.is_dir() {
                deeper.push((path, child_inside));
            } else if child_inside {
                found.count(&path, kind);
            }
        }
        // Last pushed is walked first, so the directories go in reverse to keep name order.
        dirs.extend(deeper.into_iter().rev());
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Every directory the walk listed on this thread, so a test can see what it opened.
    static LISTED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// An error reading `path`, which stops the run: a directory that cannot be listed or an entry
/// whose type cannot be read, named with the system's own words.
fn io_error(path: &Path, cause: io::Error, cwd: &Path) -> Error {
    Error {
        cause: ignore::Error::WithPath {
            path: path.to_path_buf(),
            err: Box::new(ignore::Error::Io(cause)),
        },
        cwd: cwd.to_path_buf(),
    }
}

/// The rules in `bytes`, the contents of the ignore file `file`, matched relative to `dir`, and
/// what could not be used, each error tagged with the file and its line.
///
/// Parsed as `GitignoreBuilder::add` parses a file it opens itself, which a test holds it to:
/// by line, a line's CR and LF dropped, a UTF-8 byte-order mark dropped from the first line, and
/// a line that is not UTF-8 ending the file, as it ends `add`'s reading.  quaff parses the bytes
/// rather than letting `add` open the file, so that a file also searched is read once.
fn parse_ignore_file(dir: &Path, file: &Path, bytes: &[u8]) -> (Gitignore, Vec<ignore::Error>) {
    let mut builder = GitignoreBuilder::new(dir);
    let mut errors = Vec::new();
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    for (index, raw) in lines.into_iter().enumerate() {
        let tagged = |err: ignore::Error| ignore::Error::WithPath {
            path: file.to_path_buf(),
            err: Box::new(ignore::Error::WithLineNumber {
                line: index as u64 + 1,
                err: Box::new(err),
            }),
        };
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let Ok(line) = std::str::from_utf8(raw) else {
            errors.push(tagged(ignore::Error::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            ))));
            break;
        };
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            line
        };
        if let Err(err) = builder.add_line(Some(file.to_path_buf()), line) {
            errors.push(tagged(err));
        }
    }
    match builder.build() {
        Ok(matcher) => (matcher, errors),
        Err(cause) => {
            errors.push(cause);
            (Gitignore::empty(), errors)
        }
    }
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
    let described = describe(&cause, cwd);
    if !found.skipped_rules.contains(&described) {
        found.skipped_rules.push(described);
    }
    Ok(())
}

/// The ignore rules of the directories from the boundary down, read once each as the walk
/// meets them.
struct Rules {
    boundary: PathBuf,
    system: Option<Box<dyn IgnoreFiles>>,
    /// For each directory met, its rules and those of every directory above it up to the
    /// boundary, nearest first: built once per directory, from its parent's, so each ignore
    /// file is read once and an entry costs one lookup, of its parent.
    chains: Mutex<HashMap<PathBuf, Arc<Vec<Arc<DirRules>>>>>,
    /// What reading them reported, in the order met, for the run to judge once the walk is done.
    errors: Mutex<Vec<ignore::Error>>,
    /// Where they are read through, so one also searched is read once.
    contents: Contents,
}

/// One directory's ignore rules, each file as a matcher, empty where there is no such file.
struct DirRules {
    dot_ignore: Gitignore,
    system: Gitignore,
    exclude: Gitignore,
    repository_root: bool,
}

impl Rules {
    fn new(boundary: &Path, system: Option<Box<dyn IgnoreFiles>>, contents: &Contents) -> Rules {
        contents.keep_named(".ignore");
        Rules {
            boundary: boundary.to_path_buf(),
            system,
            chains: Mutex::new(HashMap::new()),
            errors: Mutex::new(Vec::new()),
            contents: contents.clone(),
        }
    }

    /// Whether `path`, in the directory whose chain of rules is `dirs`, is ignored, decided as
    /// the `ignore` crate's walker decides: in each kind of ignore file, the nearest directory
    /// with a rule that matches decides that kind, and the kinds then rank `.ignore` above the
    /// system's own files above its exclude file.  The system's files stop at the nearest
    /// repository root, so a nested repository's own rules alone apply in it; `.ignore` files
    /// apply up to the boundary.  None above the boundary is ever read.
    ///
    /// The walk fetches a directory's chain once, for everything the directory holds.
    fn ignored_in(dirs: &[Arc<DirRules>], path: &Path, is_dir: bool) -> bool {
        let decide = |matched: ignore::Match<&ignore::gitignore::Glob>| {
            if matched.is_ignore() {
                Some(true)
            } else if matched.is_whitelist() {
                Some(false)
            } else {
                None
            }
        };
        let (mut dot_ignore, mut system, mut exclude) = (None, None, None);
        let mut past_repository = false;
        for rules in dirs {
            if dot_ignore.is_none() {
                dot_ignore = decide(rules.dot_ignore.matched(path, is_dir));
            }
            if !past_repository {
                if system.is_none() {
                    system = decide(rules.system.matched(path, is_dir));
                }
                if exclude.is_none() {
                    exclude = decide(rules.exclude.matched(path, is_dir));
                }
            }
            past_repository = past_repository || rules.repository_root;
        }
        dot_ignore.or(system).or(exclude).unwrap_or(false)
    }

    /// The rules of `dir`, at or below the boundary, and of each directory above it up to the
    /// boundary, nearest first.
    fn chain(&self, dir: &Path) -> Arc<Vec<Arc<DirRules>>> {
        if let Some(chain) = self
            .chains
            .lock()
            .expect("no thread panics holding it")
            .get(dir)
        {
            return Arc::clone(chain);
        }
        // Every directory asked about is at or below the boundary, so the parents end there.
        let mut chain = vec![Arc::new(self.read(dir))];
        if dir != self.boundary
            && let Some(parent) = dir.parent()
        {
            chain.extend(self.chain(parent).iter().cloned());
        }
        let chain = Arc::new(chain);
        self.chains
            .lock()
            .expect("no thread panics holding it")
            .insert(dir.to_path_buf(), Arc::clone(&chain));
        chain
    }

    fn read(&self, dir: &Path) -> DirRules {
        let declared = match self.system.as_ref().map(|system| system.in_dir(dir)) {
            Some(Ok(declared)) => Some(declared),
            Some(Err(err)) => {
                self.report(ignore::Error::WithPath {
                    path: err.path,
                    err: Box::new(ignore::Error::Io(err.cause)),
                });
                None
            }
            None => None,
        };
        DirRules {
            dot_ignore: self.matcher(dir, Some(dir.join(".ignore"))),
            system: self.matcher(dir, declared.as_ref().map(|d| d.file.clone())),
            exclude: self.matcher(dir, declared.as_ref().and_then(|d| d.exclude.clone())),
            repository_root: declared.is_some_and(|d| d.repository_root),
        }
    }

    /// The rules in `file`, matched relative to `dir`; empty if there is no such file.
    fn matcher(&self, dir: &Path, file: Option<PathBuf>) -> Gitignore {
        let Some(file) = file.filter(|file| file.is_file()) else {
            return Gitignore::empty();
        };
        let bytes = match self.contents.read(&file) {
            Ok(bytes) => bytes,
            Err(cause) => {
                self.report(ignore::Error::WithPath {
                    path: file,
                    err: Box::new(ignore::Error::Io(cause)),
                });
                return Gitignore::empty();
            }
        };
        let (matcher, errors) = parse_ignore_file(dir, &file, &bytes);
        for cause in errors {
            self.report(cause);
        }
        matcher
    }

    fn report(&self, cause: ignore::Error) {
        self.errors
            .lock()
            .expect("no thread panics holding it")
            .push(cause);
    }

    fn take_errors(&self) -> Vec<ignore::Error> {
        std::mem::take(&mut *self.errors.lock().expect("no thread panics holding it"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Discovery as a run does it: with the workspace found from `cwd`, if there is one.
    fn files_in(scope: &Path, cwd: &Path) -> Result<Found, Error> {
        let contents = Contents::default();
        let workspace = crate::workspace::find(cwd, &contents);
        files(scope, cwd, workspace.as_deref(), &contents)
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
        // The good lines still apply beneath it; the bad one is noted once, though the root's
        // .gitignore is consulted for every entry beneath the root.
        assert_eq!(names, ["build/b.py"]);
        assert_eq!(found.skipped_rules.len(), 1, "{:?}", found.skipped_rules);
        assert!(
            found.skipped_rules[0].starts_with("\".gitignore\": line 2: "),
            "{:?}",
            found.skipped_rules
        );
    }

    /// Says that a test of an unreadable path could not check anything, because this user can
    /// read a mode-000 path, as root can.  In CI, where the runner is not root, that is a
    /// failure, so a run that checked nothing is never reported as one that passed.
    fn cannot_check() {
        assert!(
            std::env::var_os("CI").is_none(),
            "this user can read a mode-000 path, so the unreadable-path tests check nothing; \
             run them as a user other than root"
        );
        eprintln!("skipped: this user can read a mode-000 path, so it cannot fail");
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
            cannot_check();
            return None;
        }
        Some(result)
    }

    #[test]
    fn an_unreadable_ignore_file_above_a_named_ignored_scope_stops_discovery() {
        // The walker reads no ignore file, so a/.ignore is read only when an entry under a/ is
        // asked about, and that read must stop the run rather than pass over it.
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
    fn a_dot_ignore_rule_outranks_a_gitignore_rule_at_any_depth() {
        // As the walker ranks them: the nearest match of each kind, then .ignore before
        // .gitignore, however deep each is.
        let tree = Tree::new(
            "kind-order",
            &[
                (".git/HEAD", ""),
                (".ignore", "*.log\n!wanted.tmp\n"),
                ("sub/.gitignore", "build/\n!keep.log\n*.tmp\n"),
                ("sub/build/keep.log", ""),
                ("sub/build/wanted.tmp", ""),
                ("sub/build/x.py", ""),
            ],
        );
        assert_eq!(
            tree.found("sub/build"),
            ["sub/build/wanted.tmp", "sub/build/x.py"]
        );
    }

    #[test]
    fn a_nested_repository_s_gitignore_rules_stop_at_its_root() {
        // The root's .gitignore and info/exclude do not reach into a nested repository, whether
        // the search covers the project or names an ignored directory inside it.
        let tree = Tree::new(
            "nested-repo",
            &[
                (".git/HEAD", ""),
                (".git/info/exclude", "*.tmp\n"),
                (".gitignore", "*.log\n"),
                ("vendor/lib/.git", "gitdir: ../../.git/modules/lib\n"),
                ("vendor/lib/.gitignore", "out/\n"),
                ("vendor/lib/x.log", ""),
                ("vendor/lib/y.tmp", ""),
                ("vendor/lib/out/a.log", ""),
            ],
        );
        assert_eq!(
            tree.found("vendor"),
            [
                "vendor/lib/.gitignore",
                "vendor/lib/x.log",
                "vendor/lib/y.tmp"
            ]
        );
        assert_eq!(tree.found("vendor/lib/out"), ["vendor/lib/out/a.log"]);
    }

    #[test]
    fn beneath_a_named_ignored_directory_its_own_rules_can_bring_a_file_back() {
        let tree = Tree::new(
            "boundary-whitelist",
            &[
                (".git/HEAD", ""),
                (".gitignore", "build/\n*.log\n"),
                ("build/.gitignore", "!keep.log\n"),
                ("build/keep.log", ""),
                ("build/drop.log", ""),
            ],
        );
        assert_eq!(tree.found("build"), ["build/.gitignore", "build/keep.log"]);
    }

    #[test]
    fn a_bad_line_in_an_ignore_file_above_the_project_is_not_noted() {
        // None of that file applies, so saying the rest of it does would be untrue.
        let tree = Tree::new(
            "above-bad-line",
            &[
                ("outer/.ignore", "a{b\n"),
                ("outer/proj/.git/HEAD", ""),
                ("outer/proj/x.py", ""),
            ],
        );
        let dir = tree.0.join("outer/proj");
        let found = files_in(&dir, &dir).unwrap();
        assert_eq!(found.files.len(), 1);
        assert!(found.skipped_rules.is_empty(), "{:?}", found.skipped_rules);
    }

    #[test]
    fn an_ignore_file_that_is_not_utf8_stops_discovery_naming_its_line() {
        // As the ignore crate reads one: the lines before the bad one apply, and the run stops
        // rather than guess what the rest of the file says.
        let tree = Tree::new("not-utf8", &[(".git/HEAD", ""), ("a.py", "")]);
        fs::write(tree.0.join(".ignore"), b"*.log\nbad\xff\n").unwrap();
        let message = files_in(&tree.0, &tree.0)
            .expect_err("an ignore file that is not UTF-8 must stop discovery")
            .to_string();
        assert!(
            message.starts_with("finding the files to search: \".ignore\": line 2: "),
            "{message}"
        );
        assert!(message.contains("valid UTF-8"), "{message}");
    }

    /// Each error in `err`, as `GitignoreBuilder::add` groups several into one.
    fn flattened(err: ignore::Error) -> Vec<String> {
        match err {
            ignore::Error::Partial(errs) => errs.into_iter().flat_map(flattened).collect(),
            other => vec![other.to_string()],
        }
    }

    #[test]
    fn ignore_files_are_parsed_from_their_bytes_as_the_ignore_crate_parses_them() {
        // GitignoreBuilder::add is the reference: for each file, the same paths ignored, the
        // same whitelisted, and the same errors, as the crate's own reading of the same file.
        let dir = std::env::temp_dir().join(format!("quaffed-parse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cases: &[&[u8]] = &[
            b"\xef\xbb\xbf*.log\nbuild/\n",
            b"*.log\r\n!keep.log\r\nbuild/\r\n",
            b"*.tmp\nno-final-newline.c",
            b"*.log\na{b\n[z-a]\n*.bak\n",
            b"*.log\nbad\xff\n*.bak\n",
            b"# a comment\n\n  \n\\#literal\ntrailing\\ \n",
            b"",
        ];
        let paths = [
            "a.log",
            "keep.log",
            "build",
            "x.tmp",
            "no-final-newline.c",
            "y.bak",
            "#literal",
            "trailing ",
            "other.py",
        ];
        for (number, bytes) in cases.iter().enumerate() {
            let file = dir.join(format!("case{number}.ignore"));
            fs::write(&file, bytes).unwrap();
            let mut reference = GitignoreBuilder::new(&dir);
            let reference_errors = reference.add(&file).map(flattened).unwrap_or_default();
            let reference = reference.build().unwrap();
            let (ours, our_errors) = parse_ignore_file(&dir, &file, bytes);
            let our_errors: Vec<String> = our_errors.into_iter().flat_map(flattened).collect();
            assert_eq!(our_errors, reference_errors, "case {number}: {bytes:?}");
            for path in paths {
                for is_dir in [false, true] {
                    let path = dir.join(path);
                    let (got, want) = (
                        ours.matched(&path, is_dir),
                        reference.matched(&path, is_dir),
                    );
                    assert_eq!(
                        (got.is_ignore(), got.is_whitelist()),
                        (want.is_ignore(), want.is_whitelist()),
                        "case {number} {bytes:?}: {} as a {}",
                        path.display(),
                        if is_dir { "directory" } else { "file" }
                    );
                }
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_byte_order_marked_ignore_file_still_applies_its_first_line() {
        let tree = Tree::new("bom", &[(".git/HEAD", ""), ("a.log", ""), ("b.py", "")]);
        fs::write(tree.0.join(".ignore"), b"\xef\xbb\xbf*.log\n").unwrap();
        assert_eq!(tree.found("."), [".ignore", "b.py"]);
    }

    /// Every answer `scripts/generate-ignore-cases` recorded from git, for the same trees
    /// rebuilt here: discovery must list exactly the files git lists, for each scope asked.
    #[test]
    fn discovery_agrees_with_git_on_which_files_it_lists() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ignores.txt");
        let data = fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {path}: {err}"));
        let base = std::env::temp_dir().join(format!("quaffed-ignores-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let mut root: Option<PathBuf> = None;
        // The scope being collected, and git's answer for it so far.
        let mut pending: Option<(String, Vec<String>)> = None;
        let mut checked = 0;
        let check = |root: &Path, scope: &str, want: &[String]| {
            let root = fs::canonicalize(root).unwrap();
            let target = if scope == "." {
                root.clone()
            } else {
                root.join(scope)
            };
            let found = files_in(&target, &root).unwrap();
            let got: Vec<String> = found
                .files
                .iter()
                .map(|p| {
                    p.strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(got, want, "{} scope {scope}", root.display());
        };
        for (number, line) in data.lines().enumerate() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let at = format!("{path}:{}", number + 1);
            match fields.as_slice() {
                ["case", name] => {
                    if let (Some(root), Some((scope, want))) = (&root, pending.take()) {
                        check(root, &scope, &want);
                        checked += 1;
                    }
                    let dir = base.join(name);
                    fs::create_dir_all(dir.join(".git")).unwrap();
                    root = Some(dir);
                }
                ["repository", dir] => {
                    let root = root.as_ref().expect("a case first");
                    fs::create_dir_all(root.join(dir).join(".git")).unwrap();
                }
                ["file", file, hex] => {
                    let bytes: Vec<u8> = (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
                        .collect();
                    let target = root.as_ref().expect("a case first").join(file);
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    fs::write(target, bytes).unwrap();
                }
                ["scope", scope] => {
                    if let (Some(root), Some((scope, want))) = (&root, pending.take()) {
                        check(root, &scope, &want);
                        checked += 1;
                    }
                    pending = Some(((*scope).to_owned(), Vec::new()));
                }
                ["listed", file] => {
                    pending
                        .as_mut()
                        .expect("a scope first")
                        .1
                        .push((*file).to_owned());
                }
                _ => panic!("{at}: cannot read {line:?}"),
            }
        }
        if let (Some(root), Some((scope, want))) = (&root, pending.take()) {
            check(root, &scope, &want);
            checked += 1;
        }
        let _ = fs::remove_dir_all(&base);
        // A file that held no answers would pass having checked nothing.
        assert!(checked >= 10, "only {checked} scopes in {path}");
    }

    #[test]
    fn an_ignored_directory_and_one_beside_the_scope_are_never_listed() {
        // Listing an ignored node_modules/ would read every name in it to throw them away.
        let tree = Tree::new(
            "never-listed",
            &[
                (".git/HEAD", ""),
                (".gitignore", "node_modules/\n"),
                ("node_modules/pkg/index.js", ""),
                ("beside/x.py", ""),
                ("src/deeper/a.py", ""),
            ],
        );
        let root = fs::canonicalize(&tree.0).unwrap();
        for (scope, expected) in [
            (".", vec![".", "beside", "src", "src/deeper"]),
            ("src", vec![".", "src", "src/deeper"]),
        ] {
            LISTED.with(|listed| listed.borrow_mut().clear());
            files_in(&root.join(scope), &root).unwrap();
            let listed: Vec<String> = LISTED.with(|listed| {
                listed
                    .borrow()
                    .iter()
                    .map(
                        |dir| match dir.strip_prefix(&root).unwrap().to_string_lossy() {
                            name if name.is_empty() => ".".to_owned(),
                            name => name.into_owned(),
                        },
                    )
                    .collect()
            });
            assert_eq!(listed, expected, "scope {scope}");
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
            cannot_check();
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
