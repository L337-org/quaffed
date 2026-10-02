//! End-to-end tests: the built `quaff` binary, run as a user runs it.
//!
//! Each test runs the real binary in a throwaway project directory, records what it printed
//! and how it exited as a reviewed snapshot, and checks the files it left behind.  A changed
//! message or exit code therefore fails until the snapshot is reviewed, and shows on the pull
//! request as a diff.  `architecture/verification.md` says how to add a test and how to review
//! a snapshot.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use insta_cmd::assert_cmd_snapshot;

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

/// A throwaway project directory, and what it held before `quaff` ran.
struct Project {
    dir: PathBuf,
    before: BTreeMap<PathBuf, Vec<u8>>,
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
        let _ = fs::remove_dir_all(&dir);
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

    /// Fails, naming every file added, removed or changed, if the project is not as it was.
    fn assert_unchanged(&self) {
        let after = snapshot_files(&self.dir);
        let mut changes = Vec::new();
        for (path, content) in &after {
            match self.before.get(path) {
                None => changes.push(format!("added {}", path.display())),
                Some(old) if old != content => changes.push(format!("changed {}", path.display())),
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
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Reads every file under `dir`, keyed by its path relative to `dir`.
fn snapshot_files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()))
                .path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                let content = fs::read(&path)
                    .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
                let relative = path
                    .strip_prefix(root)
                    .expect("under the root")
                    .to_path_buf();
                found.insert(relative, content);
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(dir, dir, &mut found);
    found
}

const PROJECT: &[(&str, &str)] = &[
    ("app.py", "def main():\n    print('hello')\n"),
    ("pkg/__init__.py", ""),
];

#[test]
fn with_no_arguments_it_says_nothing_is_implemented() {
    let project = Project::new(PROJECT);
    assert_cmd_snapshot!(project.quaff(&[]));
    project.assert_unchanged();
}

#[test]
fn asked_for_help_it_says_nothing_is_implemented() {
    let project = Project::new(PROJECT);
    assert_cmd_snapshot!(project.quaff(&["--help"]));
    project.assert_unchanged();
}

#[test]
#[should_panic(expected = "the quaff binary is not at")]
fn a_missing_binary_fails_rather_than_skips() {
    binary_at(Path::new("/nonexistent/quaff"));
}

#[test]
#[should_panic(expected = "quaff changed the project: changed app.py, added new.py")]
fn a_changed_project_is_reported_file_by_file() {
    let project = Project::new(PROJECT);
    fs::write(project.dir.join("app.py"), "changed\n").expect("changing a file");
    fs::write(project.dir.join("new.py"), "").expect("adding a file");
    project.assert_unchanged();
}
