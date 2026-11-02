//! `scripts/oracle-python`, the edit-invariant harness's oracle for Python, says which of the
//! files it is given the oracle refuses, one line each, and exits 0 only once it has judged them
//! all.
//!
//! Each test names the running `python3` as the oracle in a manifest, so the script's version
//! check passes, as `check_synthetic.rs` does.  `architecture/corpus.md` describes the harness.

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

/// Throwaway files and a manifest, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(oracle: &str, files: &[(&str, &str)]) -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-oracle-python-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("making the directory");
        for (path, content) in files {
            fs::write(dir.join(path), content).expect("writing a file");
        }
        fs::write(
            dir.join("manifest.toml"),
            format!("[cpython]\noracle = \"{oracle}\"\n"),
        )
        .expect("writing the manifest");
        Scratch(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().expect("a UTF-8 path").to_owned()
    }

    fn judge(&self, names: &[&str]) -> Output {
        let paths: Vec<String> = names.iter().map(|name| self.path(name)).collect();
        self.judge_paths(&paths)
    }

    /// Runs the oracle on `paths` exactly as written.
    fn judge_paths(&self, paths: &[String]) -> Output {
        let script = workspace_root().join("scripts/oracle-python");
        let manifest = self.path("manifest.toml");
        let mut args = vec![
            script.to_str().expect("a UTF-8 path").to_owned(),
            "--manifest".to_owned(),
            manifest,
        ];
        args.extend(paths.iter().cloned());
        python(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn it_prints_each_refused_file_with_a_reason_and_nothing_for_an_accepted_one() {
    let scratch = Scratch::new(
        &python_version(),
        &[
            ("good.py", "x = 1\n"),
            ("bad.py", "def f(:\n"),
            ("worse.py", "x = (\n"),
        ],
    );
    let out = scratch.judge(&["good.py", "bad.py", "worse.py"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines: Vec<String> = stdout(&out).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    for (line, name) in lines.iter().zip(["bad.py", "worse.py"]) {
        let (path, reason) = line.split_once('\t').expect("a path, a tab and a reason");
        assert_eq!(path, scratch.path(name));
        assert!(reason.starts_with("SyntaxError: "), "{reason}");
    }
}

#[test]
fn a_file_it_cannot_read_stops_it_rather_than_counting_as_refused() {
    let scratch = Scratch::new(&python_version(), &[("good.py", "x = 1\n")]);
    let out = scratch.judge(&["good.py", "missing.py"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("missing.py") && stderr(&out).contains("to judge it"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn another_python_than_the_oracle_is_refused() {
    let scratch = Scratch::new("2.7.18", &[("good.py", "x = 1\n")]);
    let out = scratch.judge(&["good.py"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("the oracle is CPython 2.7.18, but this is "),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_refused_path_is_printed_exactly_as_given() {
    let scratch = Scratch::new(&python_version(), &[("bad.py", "def f(:\n")]);
    // A path library would tidy these into one plain path, which the harness was not given.
    let given = [
        format!("{}//bad.py", scratch.0.display()),
        format!("{}/./bad.py", scratch.0.display()),
    ];
    let out = scratch.judge_paths(&given);
    assert!(out.status.success(), "{}", stderr(&out));
    let printed: Vec<String> = stdout(&out)
        .lines()
        .map(|line| {
            line.split_once('\t')
                .expect("a path and a reason")
                .0
                .to_owned()
        })
        .collect();
    assert_eq!(printed, given);
}
