//! End-to-end tests: the built `quaff` binary, run as a user runs it.
//!
//! Each test runs the real binary in a throwaway project directory, records what it printed
//! and how it exited as a reviewed snapshot, and checks the files it left behind.  A changed
//! message or exit code therefore fails until the snapshot is reviewed, and shows on the pull
//! request as a diff.  `architecture/testing.md` says how to add a test and how to review
//! a snapshot.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use insta_cmd::assert_cmd_snapshot;

/// Asserts a snapshot of `quaff`'s result with the workspace version shown as `[VERSION]`.
///
/// The version is declared in one place, and a snapshot that recorded it would be a second
/// that every version bump had to re-accept.  `the_version_it_reports_is_the_workspace_version`
/// checks the real value instead.
macro_rules! assert_quaff_snapshot {
    ($command:expr) => {{
        let mut settings = insta::Settings::clone_current();
        settings.add_filter(&version_pattern(), "[VERSION]");
        settings.bind(|| assert_cmd_snapshot!($command));
    }};
}

/// A regular expression matching the workspace version as a whole word.
///
/// Every regular-expression metacharacter is escaped, so a version with build metadata such as
/// `1.0.0+abc` is matched literally rather than read as a quantifier.
fn version_pattern() -> String {
    format!(r"\b{}\b", escape_regex(env!("CARGO_PKG_VERSION")))
}

/// Returns `text` with every regular-expression metacharacter backslash-escaped.
fn escape_regex(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Returns `path` if a binary is there, and fails the test naming it if not.
///
/// A missing binary must fail the run: a harness that skipped would report every end-to-end
/// test as passing having run nothing.
fn binary_at(path: &Path) -> PathBuf {
    assert!(
        path.is_file(),
        "the quaff binary is not at {}, so no end-to-end test can run; cargo builds it for \
         integration tests, so this means the build or the target directory is broken",
        path.display()
    );
    path.to_path_buf()
}

/// The binary under test.
///
/// Cargo sets `CARGO_BIN_EXE_quaff` when it builds the integration tests, so a crate without
/// the binary does not compile rather than skipping.
fn quaff_binary() -> PathBuf {
    binary_at(Path::new(env!("CARGO_BIN_EXE_quaff")))
}

/// One entry in a project, as far as an edit could change it.
///
/// Read without following symbolic links, so a link replaced by a file holding the same content
/// is a change, as is a dropped executable bit or a directory created and left empty.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File {
        mode: u32,
        content: Vec<u8>,
    },
    Dir {
        mode: u32,
    },
    Symlink {
        target: PathBuf,
    },
    /// A FIFO, socket or device: recorded by its mode, never read, because reading a FIFO
    /// waits for a writer.
    Special {
        mode: u32,
    },
}

/// A throwaway project directory, and what it held before `quaff` ran.
struct Project {
    dir: PathBuf,
    before: BTreeMap<PathBuf, Entry>,
}

impl Project {
    /// Creates a project holding `files`, each a path relative to the project and its content.
    fn new(files: &[(&str, &str)]) -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-end-to-end-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier run whose process ID this one reuses would start the
        // project with stale files, so anything but "not there" stops the test.
        match fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                panic!("clearing {} before the test: {err}", dir.display())
            }
            _ => {}
        }
        for (path, content) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a project file has a parent"))
                .expect("creating a project directory");
            fs::write(&path, content).expect("writing a project file");
        }
        fs::create_dir_all(&dir).expect("creating the project");
        let before = snapshot_files(&dir);
        Project { dir, before }
    }

    /// Adds a file holding exactly `content`, which need not be text, and records it as part of
    /// the project before `quaff` runs.
    fn add_bytes(&mut self, path: &str, content: &[u8]) {
        let path = self.dir.join(path);
        fs::create_dir_all(path.parent().expect("a project file has a parent"))
            .expect("creating a project directory");
        fs::write(&path, content).expect("writing a project file");
        self.before = snapshot_files(&self.dir);
    }

    /// Returns a command that runs `quaff` with `args` from `subdir` of the project.
    fn quaff_in(&self, subdir: &str, args: &[&str]) -> Command {
        let mut command = self.quaff(args);
        command.current_dir(self.dir.join(subdir));
        command
    }

    /// Returns a command that runs `quaff` with `args` from the project's root.
    ///
    /// The environment is cleared, so nothing from the machine running the tests reaches the
    /// binary's output or the snapshot, save the coverage profile path below.  Arguments should
    /// be relative to the project, because the snapshot records them.
    ///
    /// `LLVM_PROFILE_FILE` alone is passed through, when the tests run under coverage: without
    /// it, the instrumented binary writes its profile into the project it runs in, which
    /// `assert_unchanged` reports as a change, and the coverage it measured is lost.  It reaches
    /// the snapshot's metadata, which insta does not compare, and never its output.
    fn quaff(&self, args: &[&str]) -> Command {
        let mut command = Command::new(quaff_binary());
        command.args(args).current_dir(&self.dir).env_clear();
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    /// Fails, naming every entry added, removed or changed, if the project is not as it was.
    ///
    /// An entry is a file, a directory or a symbolic link; a file's content and permission bits,
    /// a directory's permission bits and a link's target are what can change.
    fn assert_unchanged(&self) {
        let after = snapshot_files(&self.dir);
        let mut changes = Vec::new();
        for (path, entry) in &after {
            match self.before.get(path) {
                None => changes.push(format!("added {}", path.display())),
                Some(old) if old != entry => changes.push(format!("changed {}", path.display())),
                Some(_) => {}
            }
        }
        for path in self.before.keys().filter(|p| !after.contains_key(*p)) {
            changes.push(format!("removed {}", path.display()));
        }
        assert!(
            changes.is_empty(),
            "quaff changed the project: {}",
            changes.join(", ")
        );
    }
}

impl Project {
    /// Fails unless the files named, and only those, changed, each to exactly the bytes given;
    /// every other entry must be as it was, permission bits included.
    fn assert_changed(&self, changed: &[(&str, &[u8])]) {
        let after = snapshot_files(&self.dir);
        let mut expected = self.before.clone();
        for (path, content) in changed {
            let path = PathBuf::from(path);
            let Some(Entry::File { mode, .. }) = self.before.get(&path) else {
                panic!("{} was not a file before", path.display());
            };
            expected.insert(
                path,
                Entry::File {
                    mode: *mode,
                    content: content.to_vec(),
                },
            );
        }
        for (path, entry) in &expected {
            let found = after.get(path);
            assert_eq!(
                found,
                Some(entry),
                "{} is not as expected after the run",
                path.display()
            );
        }
        let added: Vec<_> = after
            .keys()
            .filter(|p| !expected.contains_key(*p))
            .collect();
        assert!(added.is_empty(), "quaff added {added:?}");
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        // Not checked: a panic here, during the unwinding of a failed test, would abort the
        // whole run and hide that failure.  A directory left behind is cleared by the next
        // Project::new that reuses its name.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Reads every entry under `dir`, keyed by its path relative to `dir`.
///
/// Symbolic links are recorded, never followed, so a link loop cannot recurse.
fn snapshot_files(dir: &Path) -> BTreeMap<PathBuf, Entry> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Entry>) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()))
                .path();
            let meta = fs::symlink_metadata(&path)
                .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
            let mode = meta.permissions().mode() & 0o7777;
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .to_path_buf();
            let recorded = if meta.file_type().is_symlink() {
                let target = fs::read_link(&path)
                    .unwrap_or_else(|err| panic!("reading the link {}: {err}", path.display()));
                Entry::Symlink { target }
            } else if meta.is_dir() {
                walk(root, &path, found);
                Entry::Dir { mode }
            } else if meta.is_file() {
                let content = fs::read(&path)
                    .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
                Entry::File { mode, content }
            } else {
                Entry::Special { mode }
            };
            found.insert(relative, recorded);
        }
    }
    let mut found = BTreeMap::new();
    walk(dir, dir, &mut found);
    found
}

/// Takes every permission from a path for as long as it lives, and gives them back when it is
/// dropped - including when the test fails - so no run leaves a locked file behind.
struct Locked {
    path: PathBuf,
    mode: fs::Permissions,
}

impl Locked {
    fn new(path: PathBuf) -> Self {
        Locked::to(path, 0o000)
    }

    /// Gives the path only the permissions `mode`, until it is dropped.
    fn to(path: PathBuf, mode: u32) -> Self {
        let was = fs::metadata(&path).expect("reading the mode").permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("locking");
        Locked { path, mode: was }
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::set_permissions(&self.path, self.mode.clone());
    }
}

const PROJECT: &[(&str, &str)] = &[
    (".git/HEAD", "ref: refs/heads/main\n"),
    (
        "app.py",
        "def main():\n    print('hello')  # TODO: greet by name\n",
    ),
    ("pkg/__init__.py", ""),
    (
        "pkg/client.py",
        "# TODO: retry on 429\nclass Client:\n    pass  # TODO TODO\n",
    ),
    ("README.md", "Nothing to do here.\n"),
];

#[test]
fn with_no_arguments_it_prints_help_and_exits_3() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&[]));
    project.assert_unchanged();
}

#[test]
fn asked_for_help_it_prints_help() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&["--help"]));
    let long = project.quaff(&["--help"]).output().expect("running quaff");
    let short = project.quaff(&["-h"]).output().expect("running quaff");
    assert_eq!(long, short, "-h and --help differ");
    project.assert_unchanged();
}

#[test]
fn the_version_it_reports_is_the_workspace_version() {
    let output = Project::new(PROJECT)
        .quaff(&["--help"])
        .output()
        .expect("running quaff");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with(&format!("quaff {} ", env!("CARGO_PKG_VERSION"))),
        "{stdout:?}"
    );
}

#[test]
fn a_bare_query_finds_every_match_in_the_project() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    project.assert_unchanged();
}

#[test]
fn a_string_option_finds_the_same_and_may_be_repeated() {
    let project = Project::new(PROJECT);
    let bare = project.quaff(&["TODO"]).output().expect("running quaff");
    let option = project
        .quaff(&["-s", "TODO"])
        .output()
        .expect("running quaff");
    assert_eq!(bare, option, "a bare query and -s differ");
    assert_quaff_snapshot!(project.quaff(&["-s", "class", "-s", "TODO", "pkg"]));
    project.assert_unchanged();
}

#[test]
fn finding_nothing_exits_1_and_is_not_an_error() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&["FIXME"]));
    project.assert_unchanged();
}

#[test]
fn from_a_subdirectory_it_searches_the_whole_project() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff_in("pkg", &["TODO"]));
    project.assert_unchanged();
}

#[test]
fn with_no_project_root_it_searches_the_current_directory() {
    let project = Project::new(&[("a.txt", "TODO\n"), ("sub/b.txt", "TODO\n")]);
    assert_quaff_snapshot!(project.quaff_in("sub", &["TODO"]));
    project.assert_unchanged();
}

#[test]
fn ignored_files_and_version_control_metadata_are_not_searched() {
    let project = Project::new(&[
        (".git/HEAD", "TODO in git metadata\n"),
        (".git/info/exclude", "excluded.py\n"),
        (".gitignore", "build/\n"),
        (".ignore", "*.log\n"),
        (".hidden.py", "TODO in a hidden file\n"),
        ("build/out.py", "TODO in an ignored directory\n"),
        ("debug.log", "TODO in an ignored file\n"),
        ("excluded.py", "TODO in an excluded file\n"),
        ("kept.py", "TODO in a kept file\n"),
        ("vendor/.hg/store", "TODO in other metadata\n"),
    ]);
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    project.assert_unchanged();
}

#[test]
fn a_scope_narrows_the_search_to_a_directory_or_a_file() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&["TODO", "pkg"]));
    assert_quaff_snapshot!(project.quaff(&["TODO", "app.py"]));
    project.assert_unchanged();
}

#[test]
fn more_than_one_scope_is_refused_naming_glob_expansion() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&["TODO", "app.py", "pkg/client.py"]));
    project.assert_unchanged();
}

#[test]
fn a_scope_that_does_not_exist_is_an_io_error_naming_it() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&["TODO", "missing"]));
    assert_quaff_snapshot!(project.quaff(&["TODO", "src/**/*.py"]));
    project.assert_unchanged();
}

#[test]
fn a_scope_outside_the_project_is_refused() {
    let project = Project::new(&[
        ("outside.py", "TODO\n"),
        ("inner/.git/HEAD", ""),
        ("inner/a.py", "TODO\n"),
    ]);
    assert_quaff_snapshot!(project.quaff_in("inner", &["TODO", ".."]));
    project.assert_unchanged();
}

#[test]
fn with_no_project_a_scope_outside_the_current_directory_is_refused() {
    let project = Project::new(&[("a.txt", "TODO\n"), ("sub/b.txt", "TODO\n")]);
    assert_quaff_snapshot!(project.quaff_in("sub", &["TODO", ".."]));
    // Inside it, a scope still narrows as usual.
    assert_quaff_snapshot!(project.quaff(&["TODO", "sub"]));
    project.assert_unchanged();
}

#[test]
fn anchors_and_other_punctuation_are_literal() {
    let project = Project::new(&[(".git/HEAD", ""), ("a.txt", "^x$ and x\n[a-z]* and abc\n")]);
    assert_quaff_snapshot!(project.quaff(&["^x$"]));
    assert_quaff_snapshot!(project.quaff(&["-s", "[a-z]*"]));
    project.assert_unchanged();
}

#[test]
fn text_starting_with_a_dash_is_searched_after_a_double_dash() {
    let project = Project::new(&[(".git/HEAD", ""), ("a.txt", "x --verbose y\n")]);
    assert_quaff_snapshot!(project.quaff(&["--", "--verbose"]));
    project.assert_unchanged();
}

#[test]
fn a_query_spanning_lines_is_folded_onto_one_output_line() {
    let project = Project::new(&[(".git/HEAD", ""), ("a.py", "if ready:\n    return value\n")]);
    assert_quaff_snapshot!(project.quaff(&["-s", "ready:\n    return"]));
    project.assert_unchanged();
}

#[test]
fn each_usage_error_exits_3_saying_what_to_do() {
    let project = Project::new(PROJECT);
    assert_quaff_snapshot!(project.quaff(&[""]));
    assert_quaff_snapshot!(project.quaff(&["-s"]));
    assert_quaff_snapshot!(project.quaff(&["--frobnicate"]));
    assert_quaff_snapshot!(project.quaff(&["-p", ""]));
    assert_quaff_snapshot!(project.quaff(&["--output=json"]));
    assert_quaff_snapshot!(project.quaff(&["-s", ""]));
    assert_quaff_snapshot!(project.quaff(&["--"]));
    assert_quaff_snapshot!(project.quaff(&["-e"]));
    assert_quaff_snapshot!(project.quaff(&["--file"]));
    project.assert_unchanged();
}

#[test]
fn a_query_that_is_not_utf8_is_refused() {
    use std::os::unix::ffi::OsStrExt;
    let project = Project::new(PROJECT);
    let mut command = project.quaff(&[]);
    command.arg(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    assert_quaff_snapshot!(command);
    project.assert_unchanged();
}

#[test]
fn an_expression_that_is_not_utf8_is_refused_naming_which() {
    use std::os::unix::ffi::OsStrExt;
    let project = Project::new(PROJECT);
    let mut command = project.quaff(&["-e", "find \"TODO\"", "-e"]);
    command.arg(std::ffi::OsStr::from_bytes(b"find \"caf\xe9\""));
    assert_quaff_snapshot!(command);
    project.assert_unchanged();
}

#[test]
fn an_unparseable_ignore_line_is_skipped_and_the_rest_of_the_file_applies() {
    let project = Project::new(&[
        (".git/HEAD", ""),
        (".gitignore", "build/\na{b\n"),
        ("a.py", "TODO\n"),
        ("build/out.py", "TODO in an ignored directory\n"),
    ]);
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    project.assert_unchanged();
}

#[test]
fn a_directory_that_cannot_be_listed_stops_the_run_naming_it() {
    let project = Project::new(&[(".git/HEAD", ""), ("locked/a.py", "TODO\n")]);
    let locked = Locked::new(project.dir.join("locked"));
    if fs::read_dir(&locked.path).is_ok() {
        // Root lists a mode-000 directory, so the failure cannot happen; say so rather than
        // pass having checked nothing.
        eprintln!("skipped: this user can list a mode-000 directory, so it cannot fail");
        return;
    }
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    // Unlocked before comparing, because the comparison lists every directory.
    drop(locked);
    project.assert_unchanged();
}

#[test]
fn a_declaration_file_that_cannot_be_read_stops_the_run_naming_it() {
    // info/attributes, which is never searched itself, so only the declaration reader meets it.
    let project = Project::new(&[
        (".git/HEAD", ""),
        (
            ".git/info/attributes",
            "*.c working-tree-encoding=SHIFT-JIS\n",
        ),
        ("a.c", "TODO\n"),
    ]);
    let locked = Locked::new(project.dir.join(".git/info/attributes"));
    if fs::read(&locked.path).is_ok() {
        // Root reads a mode-000 file, so the failure cannot happen; say so rather than pass
        // having checked nothing.
        eprintln!("skipped: this user can read a mode-000 file, so it cannot fail");
        return;
    }
    // A file is never read in the wrong encoding for want of its declaration.
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    drop(locked);
    project.assert_unchanged();
}

#[test]
fn special_files_are_not_read_and_the_run_says_so() {
    let mut project = Project::new(PROJECT);
    let fifo = project.dir.join("pipe");
    let made = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("running mkfifo");
    assert!(made.success(), "mkfifo {} failed", fifo.display());
    project.before = snapshot_files(&project.dir);
    assert_quaff_snapshot!(project.quaff(&["TODO", "."]));
    project.assert_unchanged();
}

#[test]
fn binary_files_are_not_searched_and_the_run_says_so() {
    let mut project = Project::new(PROJECT);
    project.add_bytes("image.bin", b"TODO\0\x01\x02");
    assert_quaff_snapshot!(project.quaff(&["TODO", "image.bin"]));
    project.assert_unchanged();
}

#[test]
fn files_are_searched_in_their_own_encoding() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    // Latin-1, with no declaration: an unknown 8-bit encoding.
    project.add_bytes("latin1.txt", b"caf\xe9 TODO\n");
    // UTF-16LE, declared by its byte-order mark.
    let mut utf16 = vec![0xFF, 0xFE];
    utf16.extend("caf\u{e9} TODO\n".encode_utf16().flat_map(u16::to_le_bytes));
    project.add_bytes("utf16.txt", &utf16);
    project.add_bytes("utf8.txt", "caf\u{e9} TODO\n".as_bytes());
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    // A non-ASCII query has no one spelling in an unknown 8-bit encoding.
    assert_quaff_snapshot!(project.quaff(&["caf\u{e9}"]));
    project.assert_unchanged();
}

#[test]
fn python_files_are_searched_in_the_encoding_they_declare() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    project.add_bytes(
        "latin1.py",
        b"# -*- coding: latin-1 -*-\nname = 'caf\xe9'\n",
    );
    // The same bytes, but not Python: nothing is declared, so an unknown 8-bit encoding.
    project.add_bytes(
        "latin1.txt",
        b"# -*- coding: latin-1 -*-\nname = 'caf\xe9'\n",
    );
    // Declared on the second line, after a blank or comment first line, as Python allows.
    project.add_bytes(
        "koi8.py",
        b"#!/usr/bin/env python\n# coding: koi8-r\ngreeting = '\xd0\xd2\xc9\xd7\xc5\xd4'\n",
    );
    // Stubs and windowed scripts are Python too.
    project.add_bytes("latin1.pyi", b"# coding: latin-1\nname: 'caf\xe9'\n");
    project.add_bytes("latin1.pyw", b"# coding: latin-1\nname = 'caf\xe9'\n");
    // mac_arabic spells the space two ways, so a query with a space has no one spelling there.
    project.add_bytes("arabic.py", b"# coding: mac-arabic\nx = 1\n");
    assert_quaff_snapshot!(project.quaff(&["caf\u{e9}"]));
    assert_quaff_snapshot!(project.quaff(&["\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}"]));
    assert_quaff_snapshot!(project.quaff(&["x = 1"]));
    // An assertion cannot be checked over it, and its failure names the file and why.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"x = 1\" expect 1"]));
    project.assert_unchanged();
}

#[test]
fn a_python_file_python_would_not_read_is_not_searched_and_the_run_says_so() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("good.py", "# TODO\n")]);
    // An encoding Python does not know, as in CPython's tokenizedata/bad_coding.py.
    project.add_bytes("unknown.py", b"# -*- coding: uft-8 -*-\n# TODO\n");
    // A byte-order mark and a declaration of anything but UTF-8, as in bad_coding2.py.
    project.add_bytes("marked.py", b"\xef\xbb\xbf# coding: latin-1\n# TODO\n");
    // An encoding Python reads and quaff does not: its encoding is unknown, so its match
    // is only possible, and noted, naming the declaration.
    project.add_bytes("chinese.py", b"# coding: gbk\n# TODO\n");
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    // Hits alone decide: the files not searched, and the possible match, are noted.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\" expect 1"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\" expect 2"]));
    project.assert_unchanged();
}

#[test]
fn symbolic_links_are_not_followed_and_the_run_says_so() {
    let mut project = Project::new(PROJECT);
    std::os::unix::fs::symlink("app.py", project.dir.join("link.py")).expect("making a link");
    project.before = snapshot_files(&project.dir);
    assert_quaff_snapshot!(project.quaff(&["TODO", "."]));
    project.assert_unchanged();
}

#[test]
fn a_file_that_cannot_be_read_stops_the_run_naming_it() {
    let project = Project::new(PROJECT);
    let locked = Locked::new(project.dir.join("pkg/client.py"));
    if fs::read(&locked.path).is_ok() {
        // Root reads a mode-000 file, so the failure cannot happen; say so rather than pass
        // having checked nothing.
        eprintln!("skipped: this user can read a mode-000 file, so it cannot fail");
        return;
    }
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    // Unlocked before comparing, because the comparison reads every file; the mode is then the
    // original, so a change quaff made would still show.
    drop(locked);
    project.assert_unchanged();
}

// Scripts: `-e`, `-f` and `-f -`, and what the script language does when run.

/// A project for scripts: the files a script searches, and scripts kept apart from them under
/// `scripts/`, which every run narrows away from with the scope `src`.
const SCRIPTED: &[(&str, &str)] = &[
    (".git/HEAD", "ref: refs/heads/main\n"),
    (
        "src/app.py",
        "def main():\n    print('a;b')  # TODO: greet\n    return value\n",
    ),
    ("src/lib.py", "# TODO: retry\nclass Client:\n    pass\n"),
    (
        "scripts/checks.quaff",
        "# Comments are ignored outside operands.\nfind \"retry\"   # trailing\nfind \"# TODO\" \
         # a # inside an operand is content\n",
    ),
    (
        "scripts/bad.quaff",
        "find \"a\"\nfind \"b\" expect 1 in \"src\"\n",
    ),
    (
        "scripts/unknown.quaff",
        "find \"a\"\n\ninsert \"x\" after \"a\"\n",
    ),
];

#[test]
fn every_source_runs_in_the_order_given() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(
        project
            .quaff(&[
                "-e",
                "find \"Client\"",
                "-f",
                "scripts/checks.quaff",
                "-s",
                "def",
                "-e",
                "find \"value\"",
                "-f",
                "-",
                "src",
            ])
            .pass_stdin("find \"print\"\n")
    );
    project.assert_unchanged();
}

#[test]
fn a_semicolon_inside_an_operand_is_never_a_separator() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"a;b\"; find \"class\"", "src"]));
    project.assert_unchanged();
}

#[test]
fn an_assertion_block_that_holds_exits_0() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find \"TODO\" expect 2\nfind \"FIXME\" expect none\nfind \"Client\" expect at least 1",
        "src",
    ]));
    project.assert_unchanged();
}

#[test]
fn an_assertion_block_reports_every_failure_and_stops_the_run_with_exit_2() {
    let project = Project::new(SCRIPTED);
    // The second and fourth fail; both are reported, and the `find` after the block never runs.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find \"TODO\" expect 2\nfind \"Client\" expect 3\nfind \"FIXME\" expect none\n\
         find \"pass\" expect at most 0\nfind \"def\"",
        "src",
    ]));
    project.assert_unchanged();
}

#[test]
fn a_find_without_an_expect_after_a_block_still_runs_when_the_block_holds() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\" expect 2; find \"def\"", "src"]));
    project.assert_unchanged();
}

#[test]
fn line_breaks_join_and_anchors_match_as_the_language_says() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    project.add_bytes("lf.txt", b"x\nend\n  x\n");
    project.add_bytes("crlf.txt", b"x\r\nend\r\n");
    project.add_bytes("cr.txt", b"x\rend\r");
    // `$^` matches LF, CRLF and a lone CR alike.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"x\" $^ \"end\""]));
    // Anchored, `x` matches only a whole line; unanchored it matches the indented one too.
    assert_quaff_snapshot!(project.quaff(&["-e", "find ^\"x\"$", "lf.txt"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"x\"", "lf.txt"]));
    // Adjacent pieces join with nothing between them.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"e\" \"nd\"", "lf.txt"]));
    project.assert_unchanged();
}

#[test]
fn a_fenced_operand_holds_its_own_delimiter() {
    let project = Project::new(&[(".git/HEAD", ""), ("a.txt", "say \"hi\" and C:\\temp\n")]);
    assert_quaff_snapshot!(
        project.quaff(&["-e", "find \"\"\" say \"hi\" \"\"\"; find \"C:\\temp\""])
    );
    project.assert_unchanged();
}

#[test]
fn each_malformed_script_exits_3_naming_where() {
    let project = Project::new(SCRIPTED);
    // An unfenced delimiter inside an operand, reported where the operand closed.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"say \"hi\"\""]));
    // A clause out of order, in a script file, by line and column.
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/bad.quaff"]));
    // A textual operation spelled with a pattern, on standard input.
    assert_quaff_snapshot!(
        project
            .quaff(&["-f", "-"])
            .pass_stdin("\nreplace string `a` with `b` expect 1\n")
    );
    // An anchor on a backticked operand, in the second -e.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"a\"", "-e", "find ^`f()`"]));
    // An edit with no expect, refused by the checker.
    assert_quaff_snapshot!(project.quaff(&["-e", "replace string \"a\" with \"b\""]));
    project.assert_unchanged();
}

#[test]
fn a_script_that_is_not_utf8_exits_3_naming_where() {
    let mut project = Project::new(SCRIPTED);
    project.add_bytes("scripts/latin1.quaff", b"find \"a\"\nfind \"caf\xe9\"\n");
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/latin1.quaff"]));
    project.assert_unchanged();
}

#[test]
fn each_construct_outside_the_mvp_exits_5_naming_it() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/unknown.quaff"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "expect 1"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"a\" $x \"b\""]));
    assert_quaff_snapshot!(project.quaff(&["-e", "replace `f($a)` with `g(${a|x})` expect 1"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find `f($a)` as call"]));
    project.assert_unchanged();
}

#[test]
fn each_construct_not_built_yet_exits_5_naming_it() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&["-e", "replace `f($a)` with `g($a)` expect 1"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"a\" in \"src/**\""]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"a\" where not FILE matches \"x\""]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"a\"; delete `a` expect 1"]));
    project.assert_unchanged();
}

#[test]
fn nothing_runs_when_any_source_is_wrong() {
    let project = Project::new(SCRIPTED);
    // The first -e is fine, and finds something; the error in the second stops the run first.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\"", "-e", "fnd \"x\"", "src"]));
    project.assert_unchanged();
}

#[test]
fn a_script_file_that_cannot_be_read_exits_7_naming_it() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/missing.quaff"]));
    project.assert_unchanged();
}

#[test]
fn standard_input_can_be_a_source_only_once() {
    let project = Project::new(SCRIPTED);
    assert_quaff_snapshot!(project.quaff(&["-f", "-", "-f", "-"]));
    project.assert_unchanged();
}

#[test]
fn an_assertion_is_decided_by_hits_alone_whatever_files_it_could_not_search() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("utf8.txt", "TODO here\n")]);
    // Latin-1, with no declaration: its encoding is unknown, so "café" has no one spelling in
    // it, and its "TODO" is only a possible match.
    project.add_bytes("latin1.txt", b"caf\xe9 TODO\n");
    project.add_bytes("legacy/old.txt", b"na\xefve\n");
    // Both hold, by the hits: the files not searched and the possible match are noted.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find \"caf\u{e9}\" expect none\nfind \"TODO\" expect 1",
    ]));
    // A count the hits do not meet fails, whatever the unsearched file might hold.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"na\u{ef}ve\" expect 1", "legacy"]));
    // A find after a passing block of several assertions keeps its note: the assertions'
    // patterns come before it, so a note out of step with them would be lost.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find \"TODO\" expect 1\nfind \"here\" expect at least 0\nfind \"caf\u{e9}\"",
    ]));
    // A find without an expect only notes the files, and exits by what it found.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"caf\u{e9}\""]));
    project.assert_unchanged();
}

#[test]
fn possible_matches_never_decide_an_assertion() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("menu.txt", "caf\u{e9} au lait\n")]);
    project.add_bytes("latin1.txt", b"caf\xe9 TODO\n");
    // The Latin-1 file's "TODO" might be real, but `none` counts hits, and there are none: it
    // holds, the possible match noted - the accepted cost of a file left undeclared.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\" expect none"]));
    // Nor does it meet `exactly 1`.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\" expect 1"]));
    // The UTF-8 file's hit meets `exactly 1` for a query the Latin-1 file was not searched for.
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"caf\u{e9}\" expect 1"]));
    project.assert_unchanged();
}

#[test]
fn japanese_is_found_in_files_declaring_shift_jis_cp932_or_euc_jp() {
    let mut project = Project::new(&[
        (".git/HEAD", ""),
        (
            ".gitattributes",
            "*.c working-tree-encoding=EUC-JP\n*.h working-tree-encoding=CP932\n\
             *.cc working-tree-encoding=SHIFT-JIS\n",
        ),
    ]);
    // "日本" in each codec, declared both ways, at a known line and column.
    project.add_bytes(
        "sjis.py",
        b"# coding: shift_jis\nname = '\x93\xfa\x96\x7b'\n",
    );
    project.add_bytes("windows.py", b"# coding: cp932\n# \x93\xfa\x96\x7b\n");
    project.add_bytes("euc.py", b"# coding: euc_jp\nx = 1  # \xc6\xfc\xcb\xdc\n");
    project.add_bytes("sjis.cc", b"// \x93\xfa\x96\x7b\n");
    // "①" is 87 40 in cp932 and not in Shift-JIS at all, so finding it shows cp932 itself is
    // read, not Shift-JIS under another name.
    project.add_bytes(
        "windows.h",
        b"#define X 1\n/* \x93\xfa\x96\x7b \x87\x40 */\n",
    );
    project.add_bytes("euc.c", b"/* \xc6\xfc\xcb\xdc */\n");
    assert_quaff_snapshot!(project.quaff(&["\u{65e5}\u{672c}"]));
    assert_quaff_snapshot!(project.quaff(&["\u{2460}"]));
    project.assert_unchanged();
}

#[test]
fn bytes_inside_a_multi_byte_character_are_never_a_match() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    // "ア" is 83 41 in Shift-JIS, its second byte an "A"; "ソ" is 83 5C, its second byte a
    // backslash.  Only the real "A" on line 2 is a hit.
    project.add_bytes("a.py", b"# coding: shift_jis\n\x83\x41 A \x83\x5c\n");
    assert_quaff_snapshot!(project.quaff(&["A", "a.py"]));
    assert_quaff_snapshot!(project.quaff(&["\\", "a.py"]));
    project.assert_unchanged();
}

#[test]
fn the_first_declaration_that_applies_decides() {
    let mut project = Project::new(&[
        (".git/HEAD", ""),
        (".gitattributes", "*.py working-tree-encoding=SHIFT-JIS\n"),
        (".editorconfig", "root = true\n[*]\ncharset = latin1\n"),
    ]);
    // The coding line comes before .gitattributes: read as Latin-1, so "é" is a hit.
    project.add_bytes("coded.py", b"# coding: latin-1\nx = 'caf\xe9'\n");
    // .gitattributes comes before EditorConfig: read as Shift-JIS.
    project.add_bytes("plain.py", b"x = '\x93\xfa'\n");
    // EditorConfig decides a file nothing earlier declares, latin1 included.
    project.add_bytes("notes.txt", b"caf\xe9\n");
    assert_quaff_snapshot!(project.quaff(&["caf\u{e9}"]));
    assert_quaff_snapshot!(project.quaff(&["\u{65e5}"]));
    project.assert_unchanged();
}

#[test]
fn an_unknown_encoding_gives_possible_matches_and_names_what_declared_it() {
    let mut project = Project::new(&[
        (".git/HEAD", ""),
        (
            ".gitattributes",
            "*.cn working-tree-encoding=GBK\n*.xx working-tree-encoding=MADE-UP\n\
             !*.zz working-tree-encoding=X\n",
        ),
    ]);
    // Declared in an encoding quaff does not read, and in one no codec has.  The third line
    // is a negative pattern, which git ignores in an attributes file: the run says so.
    project.add_bytes("a.cn", b"TODO \xc4\xe3\n");
    project.add_bytes("a.xx", b"TODO\n");
    // Valid UTF-8, but switching into Japanese the ISO-2022 way.
    project.add_bytes("mail.txt", b"TODO \x1b$B%3\x1b(B\n");
    // Colour codes are not that: a plain UTF-8 hit.
    project.add_bytes("log.txt", b"\x1b[31mTODO\x1b[m\n");
    // An absence is certain: no "TODO" bytes, no possible match, and nothing to say.
    project.add_bytes("empty.cn", b"\xc4\xe3\n");
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    // A non-ASCII query is not searched there, and the note names the declaration.
    assert_quaff_snapshot!(project.quaff(&["\u{4f60}"]));
    project.assert_unchanged();
}

#[test]
fn a_declaration_is_named_from_where_quaff_runs_and_a_nested_repository_has_its_own() {
    let mut project = Project::new(&[
        (".git/HEAD", ""),
        (
            ".gitattributes",
            "*.txt working-tree-encoding=UTF-16LE\n*.cn working-tree-encoding=GBK\n\
             *.w working-tree-encoding=UTF-16\n!*.zz working-tree-encoding=X\n",
        ),
        ("vendor/.git/HEAD", ""),
    ]);
    // Run from sub/, the declaring file and the skipped line are named from there.
    project.add_bytes("sub/a.cn", b"TODO \xc4\xe3\n");
    // A nested repository takes its attributes from its own files alone, so the outer *.txt
    // line does not make this UTF-8 file UTF-16.
    project.add_bytes("vendor/notes.txt", b"TODO\n");
    // UTF-16 by a name that leaves the byte order to a mark, with no mark: an encoding quaff
    // does not read, not a binary file, though it is full of NULs.
    project.add_bytes("sub/wide.w", b"T\0O\0D\0O\0\n\0");
    assert_quaff_snapshot!(project.quaff_in("sub", &["TODO"]));
    project.assert_unchanged();
}

#[test]
fn the_locale_hints_with_no_project_and_never_when_it_is_utf8() {
    let mut project = Project::new(&[]);
    project.add_bytes("euc.txt", b"TODO \xc6\xfc\xcb\xdc\n");
    // With no project there is no version control system to declare it in.
    let mut command = project.quaff(&["TODO"]);
    command.env("LANG", "ja_JP.eucJP");
    assert_quaff_snapshot!(command);
    // A UTF-8 locale gives no hint: the file is not UTF-8, so it cannot read cleanly in it.
    let mut command = project.quaff(&["TODO"]);
    command.env("LC_ALL", "en_GB.UTF-8");
    assert_quaff_snapshot!(command);
    project.assert_unchanged();
}

#[test]
fn the_locale_is_only_ever_a_hint() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    // EUC-JP, undeclared: valid in a Japanese locale's encoding, but a locale says nothing of a
    // project's files, so its encoding stays unknown and its match only possible.
    project.add_bytes("euc.txt", b"TODO \xc6\xfc\xcb\xdc\n");
    let mut command = project.quaff(&["TODO"]);
    command.env("LANG", "ja_JP.eucJP");
    assert_quaff_snapshot!(command);
    // Without the locale, the same answer and no hint.
    assert_quaff_snapshot!(project.quaff(&["TODO"]));
    project.assert_unchanged();
}

#[test]
fn a_binary_file_does_not_fail_an_assertion() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("a.txt", "nothing\n")]);
    project.add_bytes("image.bin", b"TODO\0caf\xc3\xa9");
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"TODO\" expect none"]));
    project.assert_unchanged();
}

#[test]
fn a_scripts_leading_byte_order_mark_is_skipped() {
    let mut project = Project::new(SCRIPTED);
    project.add_bytes("scripts/bom.quaff", b"\xEF\xBB\xBFfind \"Client\"\n");
    project.add_bytes("scripts/bom-error.quaff", b"\xEF\xBB\xBFfnd \"x\"\n");
    project.add_bytes(
        "scripts/bom-later.quaff",
        b"find \"a\"\n\xEF\xBB\xBFfind \"b\"\n",
    );
    // A script file with a byte-order mark runs as it would without one.
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/bom.quaff", "src"]));
    // Locations count from after the mark: the first visible character is 1:1.
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/bom-error.quaff"]));
    // The same on standard input.
    assert_quaff_snapshot!(
        project
            .quaff(&["-f", "-", "src"])
            .pass_stdin(&b"\xEF\xBB\xBFfind \"Client\"\n"[..])
    );
    // A byte-order mark anywhere else is an error outside an operand, as before.
    assert_quaff_snapshot!(project.quaff(&["-f", "scripts/bom-later.quaff"]));
    project.assert_unchanged();
}

#[test]
fn a_program_with_no_statements_exits_3_naming_its_sources() {
    let mut project = Project::new(SCRIPTED);
    project.add_bytes("scripts/comments.quaff", b"# nothing here yet\n\n");
    // An empty -e, as `-e "$CHECKS"` gives with the variable unset.
    assert_quaff_snapshot!(project.quaff(&["-e", ""]));
    // Several sources, none with a statement.
    assert_quaff_snapshot!(
        project
            .quaff(&[
                "-e",
                "# a comment",
                "-f",
                "scripts/comments.quaff",
                "-f",
                "-"
            ])
            .pass_stdin("\n")
    );
    // One source with a statement is a program, even beside an empty one.
    assert_quaff_snapshot!(project.quaff(&["-e", "", "-e", "find \"Client\"", "src"]));
    project.assert_unchanged();
}

#[test]
fn a_failed_assertion_comes_before_the_notes_and_later_statements_note_nothing() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("a.txt", "TODO\n")]);
    project.add_bytes("image.bin", b"TODO\0");
    project.add_bytes("latin1.txt", b"caf\xe9\n");
    // A note from the find before the block, the failure, then what was not looked at.  The
    // non-ASCII find after the failed block never runs, so the Latin-1 file is not noted for
    // it.  Anchors stay in the messages, outside the quotes.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find ^\"FIXME\"$\nfind \"TODO\" expect none\nfind \"caf\u{e9}\"",
    ]));
    project.assert_unchanged();
}

#[test]
#[should_panic(expected = "the quaff binary is not at /nonexistent/quaff,")]
fn a_missing_binary_fails_rather_than_skips() {
    binary_at(Path::new("/nonexistent/quaff"));
}

#[test]
fn the_version_pattern_escapes_every_metacharacter() {
    assert_eq!(escape_regex("0.1.0"), r"0\.1\.0");
    assert_eq!(escape_regex("1.0.0-rc.1"), r"1\.0\.0-rc\.1");
    assert_eq!(escape_regex("1.0.0+build.5"), r"1\.0\.0\+build\.5");
}

#[test]
#[should_panic(
    expected = "quaff changed the project: changed app.py, added new.py, removed pkg/__init__.py"
)]
fn a_changed_project_is_reported_file_by_file() {
    let project = Project::new(PROJECT);
    fs::write(project.dir.join("app.py"), "changed\n").expect("changing a file");
    fs::write(project.dir.join("new.py"), "").expect("adding a file");
    fs::remove_file(project.dir.join("pkg/__init__.py")).expect("removing a file");
    project.assert_unchanged();
}

#[test]
#[should_panic(
    expected = "quaff changed the project: changed app.py, added empty, changed link.py"
)]
fn a_change_of_kind_or_permissions_is_reported() {
    let mut project = Project::new(PROJECT);
    std::os::unix::fs::symlink("app.py", project.dir.join("link.py")).expect("making a link");
    project.before = snapshot_files(&project.dir);
    // The link replaced by a regular file holding the same content.
    let content = fs::read(project.dir.join("app.py")).expect("reading app.py");
    fs::remove_file(project.dir.join("link.py")).expect("removing the link");
    fs::write(project.dir.join("link.py"), content).expect("writing in its place");
    // Only the permission bits changed.
    fs::set_permissions(
        project.dir.join("app.py"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("changing the mode");
    // A directory created and left empty.
    fs::create_dir(project.dir.join("empty")).expect("making a directory");
    project.assert_unchanged();
}

// Structural search: a pattern's shape in Python files.

/// A project of Python and other files, for structural search.
const STRUCTURAL: &[(&str, &str)] = &[
    (".git/HEAD", ""),
    (
        "src/app.py",
        "def handle(req, resp):\n    return handle_request(req,\n        resp)\n\n\
         handle_request(a)\nclient.handle_request(a, b)\nhandle_request(handle_request(a, b), c)\n",
    ),
    ("src/strings.py", "log(\"$x\")\nlog('$x')\nlog(\"y\")\n"),
    ("README.md", "Call handle_request(a, b) to start.\n"),
];

#[test]
fn a_structural_pattern_finds_each_match_with_its_holes() {
    let project = Project::new(STRUCTURAL);
    // Every match of the shape, a multi-line one folded onto one line, a match inside a match
    // as its own line; never the attribute call, never the text in README.md.
    assert_quaff_snapshot!(project.quaff(&["-p", "handle_request($a, $b)"]));
    // A sequence hole takes one or more arguments, never none.
    assert_quaff_snapshot!(project.quaff(&["--pattern=handle_request($args...)"]));
    // A hole inside a string is literal text, and quotes take no part.
    assert_quaff_snapshot!(project.quaff(&["-p", "log(\"$x\")"]));
    project.assert_unchanged();
}

#[test]
fn a_backticked_find_in_a_script_is_structural_and_can_be_asserted() {
    let project = Project::new(STRUCTURAL);
    assert_quaff_snapshot!(project.quaff(&["-e", "find `handle_request($a)` expect 1"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find `handle_request($a)` expect 2"]));
    // Textual and structural statements in one script, each by its own operand.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find \"handle_request\" expect 6\nfind `handle_request($a, $b)` expect 3",
    ]));
    project.assert_unchanged();
}

#[test]
fn a_scope_with_no_python_says_so_rather_than_finding_nothing() {
    let project = Project::new(STRUCTURAL);
    // A search exits 1, saying no file in scope has structure, not merely that nothing matched.
    assert_quaff_snapshot!(project.quaff(&["-p", "handle_request($a, $b)", "README.md"]));
    // An assertion over it is not a passed one, whatever it asks.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find `handle_request($a, $b)` expect none",
        "README.md",
    ]));
    project.assert_unchanged();
}

#[test]
fn a_scope_whose_python_cannot_be_searched_says_so_rather_than_finding_nothing() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("README.md", "f(x)\n")]);
    // Python is in scope, but binary, so counted with the binary files: the run says no Python
    // file could be searched, not that none has structure.
    project.add_bytes("compiled.py", b"f(x)\x00\n");
    assert_quaff_snapshot!(project.quaff(&["-p", "f($a)"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find `f($a)` expect none"]));
    project.assert_unchanged();
}

#[test]
fn a_structural_pattern_with_a_line_break_is_named_on_one_line() {
    let project = Project::new(STRUCTURAL);
    assert_quaff_snapshot!(project.quaff(&["-p", "if $c:\n    return 1"]));
    project.assert_unchanged();
}

#[test]
fn a_python_file_that_does_not_parse_refuses_a_structural_search_with_exit_9() {
    let mut project = Project::new(STRUCTURAL);
    project.add_bytes("src/broken.py", b"def handle(:\n");
    // A file CPython refuses to read does not parse either.
    project.add_bytes("src/bad_coding.py", b"# -*- coding: uft-8 -*-\nx = 1\n");
    // Nor does one holding a byte its declared encoding lacks, though a lossy decoding would
    // parse, nor UTF-16, which CPython refuses for its NUL bytes.
    project.add_bytes(
        "src/bad_byte.py",
        b"# coding: utf-8\nx = '\xff'\nhandle_request(x, y)\n",
    );
    project.add_bytes("src/wide.py", b"\xff\xfex\x00\n\x00");
    // Nothing is printed, every such file is named with where and why, and the way to exclude
    // them is said.
    assert_quaff_snapshot!(project.quaff(&["-p", "handle_request($a, $b)"]));
    // A narrower scope that leaves them out is searched.
    assert_quaff_snapshot!(project.quaff(&["-p", "handle_request($a, $b)", "src/app.py"]));
    // A textual search over the same files runs: it needs no parse tree.
    assert_quaff_snapshot!(project.quaff(&["-s", "handle_request(a)"]));
    project.assert_unchanged();
}

#[test]
fn a_python_file_is_searched_in_its_declared_encoding() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    // Latin-1, declared: decoded before parsing, and placed by its characters, so the match
    // after the é starts in column 13, not 14.
    project.add_bytes(
        "latin1.py",
        b"# -*- coding: latin-1 -*-\nname = '\xe9'; f(name)\n",
    );
    assert_quaff_snapshot!(project.quaff(&["-p", "f($a)"]));
    project.assert_unchanged();
}

#[test]
fn a_python_file_quaff_cannot_read_refuses_a_structural_search_with_exit_9() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("a.py", "f(x)\n")]);
    // GBK, declared: Python reads it and quaff does not, so a count without it could not be
    // trusted - CPython counts two calls here - and the run is refused rather than passed.
    project.add_bytes("gbk.py", b"# -*- coding: gbk -*-\nf(y)\n");
    assert_quaff_snapshot!(project.quaff(&["-p", "f($a)"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find `f($a)` expect 1"]));
    // A textual search needs no parse, so it still runs over the file.
    assert_quaff_snapshot!(project.quaff(&["-s", "f("]));
    project.assert_unchanged();
}

#[test]
fn a_run_holding_a_structural_pattern_is_refused_whole_textual_queries_included() {
    let mut project = Project::new(STRUCTURAL);
    project.add_bytes("src/broken.py", b"def handle(:\n");
    // Alone, a textual query searches the broken file and the rest.
    let alone = project.quaff(&["-s", "handle"]).output().unwrap();
    assert_eq!(alone.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&alone.stdout).contains("src/broken.py:1:5-1:10: handle"));
    // Beside a structural one, the run is refused before either prints anything.
    assert_quaff_snapshot!(project.quaff(&["-s", "handle", "-p", "f($a)"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find \"handle\"\nfind `f($a)`"]));
    project.assert_unchanged();
}

#[test]
fn a_pattern_that_cannot_be_compiled_exits_3_saying_where() {
    let project = Project::new(STRUCTURAL);
    assert_quaff_snapshot!(project.quaff(&["-p", "handle_request($a"]));
    assert_quaff_snapshot!(project.quaff(&["-p", "$x"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find `f($_...)`"]));
    assert_quaff_snapshot!(project.quaff(&["-e", "find `f($x, $x...)`"]));
    // Placed past a longer fence and the space that pads it, at the `$`, character 12.
    assert_quaff_snapshot!(project.quaff(&["-e", "find ``` f($_...) ```"]));
    // A -p pattern has no fence, so a backtick it starts with is its first character.
    assert_quaff_snapshot!(project.quaff(&["-p", "`f(x)"]));
    // A sequence that starts or ends a run of statements would reach the edge of its block.
    assert_quaff_snapshot!(project.quaff(&["-p", "handle_request($a, $b)\n$rest..."]));
    // A run whose search would grow faster than the square of a block's length.
    assert_quaff_snapshot!(project.quaff(&["-p", "w()\n$a...\n$b...\nx()"]));
    assert_quaff_snapshot!(
        project.quaff(&["-p", "w()\n$a...\nx()\n$b...\ny()\n$a...\nx()\n$b...\nz()"])
    );
    project.assert_unchanged();
}

/// A project of plain text files, for replacing.
const EDITABLE: &[(&str, &str)] = &[
    (".git/HEAD", ""),
    ("a.txt", "TODO one\nTODO two\n"),
    ("b.txt", "TODO\n"),
    ("c.txt", "nothing\n"),
];

#[test]
fn a_replace_whose_count_holds_writes_every_match_and_says_so() {
    let project = Project::new(EDITABLE);
    assert_quaff_snapshot!(
        project.quaff(&["-e", "replace string \"TODO\" with \"DONE\" expect 3"])
    );
    project.assert_changed(&[("a.txt", b"DONE one\nDONE two\n"), ("b.txt", b"DONE\n")]);
}

#[test]
fn a_replace_whose_count_does_not_hold_exits_2_and_writes_nothing() {
    let project = Project::new(EDITABLE);
    assert_quaff_snapshot!(
        project.quaff(&["-e", "replace string \"TODO\" with \"DONE\" expect 2"])
    );
    project.assert_unchanged();
}

#[test]
fn every_expect_form_decides_a_replace() {
    // Each holds or fails as it would on a find of the same text; a failure writes nothing.
    for (count, holds) in [
        ("3", true),
        ("at least 1", true),
        ("at most 2", false),
        ("3 or none", true),
        ("none", false),
        ("any", true),
    ] {
        let project = Project::new(EDITABLE);
        let script = format!("replace string \"TODO\" with \"DONE\" expect {count}");
        let output = project.quaff(&["-e", &script]).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(if holds { 0 } else { 2 }),
            "expect {count}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if holds {
            project.assert_changed(&[("a.txt", b"DONE one\nDONE two\n"), ("b.txt", b"DONE\n")]);
        } else {
            project.assert_unchanged();
        }
    }
    // A count that allows none holds on a replace that matches nothing, and writes nothing.
    let project = Project::new(EDITABLE);
    let output = project
        .quaff(&[
            "-e",
            "replace string \"absent\" with \"x\" expect 3 or none",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    project.assert_unchanged();
}

#[test]
fn an_acting_statement_without_an_expect_is_refused_before_anything_runs() {
    let project = Project::new(EDITABLE);
    assert_quaff_snapshot!(project.quaff(&["-e", "replace string \"TODO\" with \"DONE\""]));
    project.assert_unchanged();
}

#[test]
fn line_endings_are_kept_byte_for_byte_and_a_written_line_break_is_the_files_own() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    project.add_bytes("lf.txt", b"a;b\nkeep\n");
    project.add_bytes("crlf.txt", b"a;b\r\nkeep\r\n");
    project.add_bytes("mixed.txt", b"a;b\r\nc;d\nkeep\r");
    project.add_bytes("none.txt", b"a;b");
    assert_quaff_snapshot!(
        project.quaff(&["-e", "replace string \";\" with \"\" $^ \"\" expect 5"])
    );
    project.assert_changed(&[
        ("lf.txt", b"a\nb\nkeep\n"),
        ("crlf.txt", b"a\r\nb\r\nkeep\r\n"),
        ("mixed.txt", b"a\r\nb\r\nc\nd\nkeep\r"),
        ("none.txt", b"a\nb"),
    ]);
}

#[test]
fn joined_pieces_line_breaks_and_anchors_match_in_a_replace_as_in_a_find() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    project.add_bytes("a.txt", b"import os\r\n  import sys\nx = 1\r\n");
    // An anchored pattern matches only at the start of a line; the line break matches CRLF.
    let output = project
        .quaff(&[
            "-e",
            "replace string ^\"im\" \"port \" with \"from \" expect 1",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = project
        .quaff(&["-e", "replace string \"os\"$^\"  \" with \"os; \" expect 1"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    project.assert_changed(&[("a.txt", b"from os; import sys\nx = 1\r\n")]);
}

#[test]
fn a_python_file_is_written_back_in_its_declared_encoding_or_with_its_byte_order_mark() {
    let mut project = Project::new(&[(".git/HEAD", "")]);
    // Shift-JIS, declared by its coding line: "x = '日本'".
    project.add_bytes(
        "sjis.py",
        b"# -*- coding: shift_jis -*-\nx = '\x93\xfa\x96\x7b'\n",
    );
    project.add_bytes("bom.py", b"\xef\xbb\xbfx = '\xe6\x97\xa5\xe6\x9c\xac'\n");
    assert_quaff_snapshot!(project.quaff(&["-e", "replace string \"日本\" with \"ア\" expect 2"]));
    project.assert_changed(&[
        // "ア" is 83 41 in Shift-JIS, and E3 82 A2 in UTF-8.
        ("sjis.py", b"# -*- coding: shift_jis -*-\nx = '\x83\x41'\n"),
        ("bom.py", b"\xef\xbb\xbfx = '\xe3\x82\xa2'\n"),
    ]);
}

#[test]
fn a_file_of_unknown_encoding_is_never_edited_and_its_matches_do_not_count() {
    let mut project = Project::new(&[(".git/HEAD", ""), ("utf8.txt", "TODO here\n")]);
    // Latin-1, undeclared: its "TODO" is only a possible match.
    project.add_bytes("latin1.txt", b"caf\xe9 TODO\n");
    // The same count a find of "TODO" would give: the hit in utf8.txt alone.
    assert_quaff_snapshot!(
        project.quaff(&["-e", "replace string \"TODO\" with \"DONE\" expect 1"])
    );
    project.assert_changed(&[("utf8.txt", b"DONE here\n")]);
}

#[test]
fn a_replace_that_would_break_a_python_file_that_parsed_is_refused_with_exit_9() {
    let project = Project::new(&[
        (".git/HEAD", ""),
        ("fine.py", "def f(x):\n    return x\n"),
        ("other.txt", "def f(x)\n"),
    ]);
    assert_quaff_snapshot!(project.quaff(&["-e", "replace string \"(x)\" with \"(x\" expect 2"]));
    project.assert_unchanged();
}

#[test]
fn a_python_file_that_did_not_parse_is_still_edited_textually() {
    let project = Project::new(&[(".git/HEAD", ""), ("broken.py", "def f(:\n    TODO\n")]);
    assert_quaff_snapshot!(
        project.quaff(&["-e", "replace string \"TODO\" with \"pass\" expect 1"])
    );
    project.assert_changed(&[("broken.py", b"def f(:\n    pass\n")]);
}

#[test]
fn a_dry_run_reports_what_would_change_and_writes_nothing() {
    let project = Project::new(EDITABLE);
    assert_quaff_snapshot!(project.quaff(&[
        "--dry-run",
        "-e",
        "replace string \"TODO\" with \"DONE\" expect 3"
    ]));
    project.assert_unchanged();
}

#[test]
fn a_replacement_a_files_encoding_cannot_write_is_refused_with_exit_9() {
    let mut project = Project::new(&[
        (".git/HEAD", ""),
        (".editorconfig", "[*.txt]\ncharset = latin1\n"),
        ("utf8.md", "TODO\n"),
    ]);
    project.add_bytes("latin1.txt", b"caf\xe9 TODO\n");
    assert_quaff_snapshot!(
        project.quaff(&["-e", "replace string \"TODO\" with \"日本\" expect 2"])
    );
    project.assert_unchanged();
}

#[test]
fn a_later_statement_sees_an_earlier_edit() {
    let project = Project::new(EDITABLE);
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "replace string \"TODO\" with \"DONE\" expect 3\n\
         replace string \"DONE one\" with \"finished\" expect 1\n\
         find \"DONE\" expect 2"
    ]));
    project.assert_changed(&[("a.txt", b"finished\nDONE two\n"), ("b.txt", b"DONE\n")]);
}

#[test]
fn a_file_that_cannot_be_written_stops_the_run_before_any_file_is_written() {
    let project = Project::new(EDITABLE);
    // Readable, so it is searched, but not writable.
    let read_only = Locked::to(project.dir.join("b.txt"), 0o444);
    let output = project
        .quaff(&["-e", "replace string \"TODO\" with \"DONE\" expect 3"])
        .output()
        .unwrap();
    drop(read_only);
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "a run that failed printed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("quaff: opening \"b.txt\" to write the edits: ")
            && stderr.trim_end().ends_with("; nothing was written"),
        "{stderr}"
    );
    project.assert_unchanged();
}

#[test]
fn a_structural_search_after_a_replace_sees_the_code_as_edited() {
    let project = Project::new(&[(".git/HEAD", ""), ("fine.py", "f(x)\nh(y)\n")]);
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "find `f($a)` expect 1\nreplace string \"f(\" with \"g(\" expect 1\nfind `g($a)` expect 1"
    ]));
    project.assert_changed(&[("fine.py", b"g(x)\nh(y)\n")]);
}

#[test]
fn a_structural_search_needing_a_file_an_earlier_replace_broke_refuses_the_edit() {
    let project = Project::new(&[(".git/HEAD", ""), ("fine.py", "def f(x):\n    return x\n")]);
    // Refused as an edit, naming the statement that broke the file, not as a file that cannot
    // be parsed, which it can, on disk.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "replace string \"(x)\" with \"(x\" expect 1\nfind `f($a)`"
    ]));
    // A replace whose own count fails decides the run first, as an assertion that failed.
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "replace string \"(x)\" with \"(x\" expect 5\nfind `f($a)`"
    ]));
    project.assert_unchanged();
}

#[test]
fn an_assertion_that_fails_after_a_replace_says_nothing_was_written() {
    let project = Project::new(EDITABLE);
    assert_quaff_snapshot!(project.quaff(&[
        "-e",
        "replace string \"TODO\" with \"DONE\" expect 3\nfind \"DONE\" expect none"
    ]));
    project.assert_unchanged();
}
