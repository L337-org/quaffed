//! End-to-end tests: the built `quaff` binary, run as a user runs it.
//!
//! Each test runs the real binary in a throwaway project directory, records what it printed
//! and how it exited as a reviewed snapshot, and checks the files it left behind.  A changed
//! message or exit code therefore fails until the snapshot is reviewed, and shows on the pull
//! request as a diff.  `architecture/verification.md` says how to add a test and how to review
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
#[derive(Debug, PartialEq, Eq)]
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
    /// binary or the snapshot.  Arguments should be relative to the project, because the
    /// snapshot records them.
    fn quaff(&self, args: &[&str]) -> Command {
        let mut command = Command::new(quaff_binary());
        command.args(args).current_dir(&self.dir).env_clear();
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
        let mode = fs::metadata(&path).expect("reading the mode").permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("locking");
        Locked { path, mode }
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
    assert_quaff_snapshot!(project.quaff(&["-p", "handle($a)"]));
    assert_quaff_snapshot!(project.quaff(&["--pattern=handle($a)"]));
    assert_quaff_snapshot!(project.quaff(&["-s", ""]));
    assert_quaff_snapshot!(project.quaff(&["--"]));
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
fn an_ignore_file_line_the_walker_cannot_parse_stops_the_run_naming_it() {
    let project = Project::new(&[
        (".git/HEAD", ""),
        (".gitignore", "build/\na{b\n"),
        ("a.py", "TODO\n"),
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
