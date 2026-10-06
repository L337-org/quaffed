//! `scripts/check-synthetic` has the oracle judge every Python file in the synthetic corpus.
//!
//! Each test builds a small corpus, names the running `python3` as the oracle in a manifest so
//! the script's version check passes, and runs it.  The real corpus is checked by the
//! **Real-world corpus** CI job; `architecture/verification.md` describes it.

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

/// A throwaway corpus and manifest, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(oracle: &str, files: &[(&str, &str)]) -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-check-synthetic-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        for (path, content) in files {
            let path = dir.join("corpus").join(path);
            fs::create_dir_all(path.parent().expect("a parent")).expect("making a directory");
            fs::write(path, content).expect("writing a corpus file");
        }
        fs::write(
            dir.join("manifest.toml"),
            format!("[cpython]\noracle = \"{oracle}\"\n"),
        )
        .expect("writing the manifest");
        Scratch(dir)
    }

    fn check(&self) -> Output {
        let script = workspace_root().join("scripts/check-synthetic");
        let manifest = self.0.join("manifest.toml");
        let corpus = self.0.join("corpus");
        python(&[
            script.to_str().expect("a UTF-8 path"),
            "--manifest",
            manifest.to_str().expect("a UTF-8 path"),
            "--corpus",
            corpus.to_str().expect("a UTF-8 path"),
        ])
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

const VALID: &[(&str, &str)] = &[
    ("modules/a.py", "x = 1\n"),
    ("expressions/e.pyexpr", "f(x)\n"),
    ("ipython/i.ipy", "%timeit f()\n"),
];

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_valid_corpus_passes_and_ipython_is_not_judged() {
    let scratch = Scratch::new(&python_version(), VALID);
    let out = scratch.check();
    assert!(out.status.success(), "{}", stderr(&out));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("1 modules, 1 expressions"), "{said}");
    assert!(
        said.contains("ipython/ is not Python and is not judged"),
        "{said}"
    );
}

#[test]
fn a_file_the_oracle_refuses_fails_naming_it_in_its_mode() {
    let mut files = VALID.to_vec();
    files.push(("modules/bad.py", "def f(:\n"));
    // A statement is valid Python, but not as a single expression.
    files.push(("expressions/statement.pyexpr", "x = 1\n"));
    let scratch = Scratch::new(&python_version(), &files);
    let out = scratch.check();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("refuses 2 file(s) of the synthetic corpus"),
        "{err}"
    );
    assert!(err.contains("modules/bad.py: SyntaxError: "), "{err}");
    assert!(
        err.contains("expressions/statement.pyexpr: SyntaxError: "),
        "{err}"
    );
}

#[test]
fn another_python_than_the_oracle_is_refused() {
    let scratch = Scratch::new("2.7.18", VALID);
    let out = scratch.check();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("the oracle is CPython 2.7.18, but this is "),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_empty_group_fails_rather_than_passing_having_checked_nothing() {
    let scratch = Scratch::new(&python_version(), &[("modules/a.py", "x = 1\n")]);
    let out = scratch.check();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("found no *.pyexpr files under "),
        "{}",
        stderr(&out)
    );
}
