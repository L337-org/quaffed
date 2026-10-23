//! `scripts/count-structural` counts, with `CPython`'s parser, the matches the structural patterns
//! should find in a fetched source.
//!
//! Each test builds a small fetched source, names the running `python3` as its oracle in a
//! manifest so the script's version check passes, and runs it.  The real corpus is counted by the
//! **Real-world corpus** CI job; `architecture/verification.md` describes it.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

fn python(args: &[&str]) -> Output {
    Command::new("python3")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("running python3, which these tests need: {err}"))
}

fn python_version() -> String {
    let out = python(&[
        "-c",
        "import sys; print('.'.join(map(str, sys.version_info[:3])))",
    ]);
    String::from_utf8(out.stdout)
        .expect("a version")
        .trim()
        .to_owned()
}

/// A throwaway fetched source and manifest, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    /// A source named `cpython` holding `files`, with `refused` listed in its `refused.txt`, and
    /// a manifest naming `oracle` as its oracle.
    fn new(oracle: &str, files: &[(&str, &str)], refused: &[&str]) -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-count-structural-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let source = dir.join("corpus/cpython");
        for (path, content) in files {
            let path = source.join(path);
            fs::create_dir_all(path.parent().expect("a parent")).expect("making a directory");
            fs::write(path, content).expect("writing a corpus file");
        }
        let mut list = format!("# {} files read\n", files.len());
        for path in refused {
            writeln!(list, "{path}\tSyntaxError: invalid syntax").expect("writing to a string");
        }
        fs::write(source.join("refused.txt"), list).expect("writing refused.txt");
        fs::write(
            dir.join("manifest.toml"),
            format!("[cpython]\noracle = \"{oracle}\"\n"),
        )
        .expect("writing the manifest");
        Scratch(dir)
    }

    fn count(&self) -> Output {
        let script = workspace_root().join("scripts/count-structural");
        let manifest = self.0.join("manifest.toml");
        let corpus = self.0.join("corpus");
        python(&[
            script.to_str().expect("a UTF-8 path"),
            "--manifest",
            manifest.to_str().expect("a UTF-8 path"),
            "--dest",
            corpus.to_str().expect("a UTF-8 path"),
        ])
    }

    fn counts(&self) -> String {
        fs::read_to_string(self.0.join("corpus/cpython/structural.txt"))
            .expect("reading structural.txt")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One file with a known number of each pattern's matches, and the near misses beside each.
const SOURCE: &str = "\
isinstance(x, int)
isinstance(x)
print()
print(1, 2)
self.assertEqual(a, b)
other.assertEqual(a, b)
try:
    f()
except E:
    g()
try:
    f()
except E as e:
    g()
def h():
    a, b = f()
    return (a, b)
def k():
    v = 1
    g()
    return v
";

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn each_pattern_is_counted_by_cpythons_tree_and_refused_files_are_left_out() {
    let scratch = Scratch::new(
        &python_version(),
        &[("Lib/a.py", SOURCE), ("Lib/broken.py", "def f(:\n")],
        &["Lib/broken.py"],
    );
    let out = scratch.count();
    assert!(out.status.success(), "{}", stderr(&out));
    let counts = scratch.counts();
    assert!(
        counts.starts_with("# 1 files; counted by CPython "),
        "{counts}"
    );
    for expected in [
        "1\tsingle and unbound holes\tisinstance($x, $_)",
        "1\tan unbound sequence\tprint($...)",
        "1\ta named sequence\tself.assertEqual($args...)",
        "1\tsequences bounded by their node\ttry:\\n    $...\\nexcept $e:\\n    $...",
        "1\ta name used twice across statements\t$x = $y\\nreturn $x",
        "1\ta sequence between two statements\t$x = $y\\n$...\\nreturn $x",
    ] {
        assert!(
            counts.lines().any(|line| line == expected),
            "{expected:?} in\n{counts}"
        );
    }
}

#[test]
fn another_python_is_refused_naming_the_oracle() {
    let scratch = Scratch::new("0.0.1", &[("Lib/a.py", SOURCE)], &[]);
    let out = scratch.count();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("is judged by CPython 0.0.1"), "{err}");
    assert!(!scratch.0.join("corpus/cpython/structural.txt").exists());
}

#[test]
fn a_source_with_no_accepted_file_fails_rather_than_counting_nothing() {
    let scratch = Scratch::new(
        &python_version(),
        &[("Lib/broken.py", "def f(:\n")],
        &["Lib/broken.py"],
    );
    let out = scratch.count();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("holds no file the oracle accepts"),
        "{}",
        stderr(&out)
    );
}
