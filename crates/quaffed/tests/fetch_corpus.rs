//! `scripts/fetch-corpus` fetches exactly the pinned archive, extracts only what it names, and
//! has the oracle list what it refuses.
//!
//! Each test builds a small archive with Python's own `tarfile`, names it in a manifest by a
//! `file://` URL, and runs the script with `python3`, giving the manifest that Python's version as
//! the oracle so the script's version check passes.  The real corpus is fetched by the
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

/// Runs `python3` with `args`, failing the test with what it said if it cannot run at all.
fn python(args: &[&str]) -> Output {
    Command::new("python3")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("running python3, which these tests need: {err}"))
}

/// The running `python3`'s version, `MAJOR.MINOR.PATCH`, used as the fixture's oracle.
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

/// A throwaway directory, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-fetch-corpus-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("making the scratch directory");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Builds `archive.tgz` in `dir` holding `members`, each a name and its content, and returns its
/// SHA-256.  Built by Python so that a member can carry a name no file system would hold.  A
/// content of `->target` makes the member a symbolic link to `target`.
fn build_archive(dir: &Path, members: &[(&str, &str)]) -> String {
    let listed: Vec<String> = members
        .iter()
        .map(|(name, content)| format!("({name:?}, {content:?})"))
        .collect();
    let script = format!(
        "import hashlib, io, sys, tarfile\n\
         path = sys.argv[1]\n\
         with tarfile.open(path, 'w:gz') as tar:\n\
         \x20   for name, content in [{}]:\n\
         \x20       info = tarfile.TarInfo(name)\n\
         \x20       if content.startswith('->'):\n\
         \x20           info.type = tarfile.SYMTYPE\n\
         \x20           info.linkname = content[2:]\n\
         \x20           tar.addfile(info)\n\
         \x20           continue\n\
         \x20       data = content.encode()\n\
         \x20       info.size = len(data)\n\
         \x20       tar.addfile(info, io.BytesIO(data))\n\
         print(hashlib.sha256(open(path, 'rb').read()).hexdigest())\n",
        listed.join(", ")
    );
    let archive = dir.join("archive.tgz");
    let out = python(&["-c", &script, archive.to_str().expect("a UTF-8 path")]);
    assert!(
        out.status.success(),
        "building the archive: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("a digest")
        .trim()
        .to_owned()
}

/// Writes a manifest naming one source, `fixture`, and returns its path.
fn manifest(dir: &Path, sha256: &str, oracle: &str, floor: usize, archive: &Path) -> PathBuf {
    let path = dir.join("manifest.toml");
    let text = format!(
        "[fixture]\n\
         tag = \"v1\"\n\
         archive = \"file://{}\"\n\
         sha256 = \"{sha256}\"\n\
         root = \"Root-1\"\n\
         path = \"Lib/test\"\n\
         oracle = \"{oracle}\"\n\
         floor = {floor}\n",
        archive.display()
    );
    fs::write(&path, text).expect("writing the manifest");
    path
}

/// Runs the script against `manifest`, fetching into `dest`.
fn fetch(manifest: &Path, dest: &Path) -> Output {
    let script = workspace_root().join("scripts/fetch-corpus");
    python(&[
        script.to_str().expect("a UTF-8 path"),
        "--manifest",
        manifest.to_str().expect("a UTF-8 path"),
        "--dest",
        dest.to_str().expect("a UTF-8 path"),
    ])
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Three files under the path, one the oracle refuses, and one file outside the path.
const MEMBERS: &[(&str, &str)] = &[
    ("Root-1/Lib/test/good.py", "x = 1\n"),
    (
        "Root-1/Lib/test/sub/also_good.py",
        "def f():\n    return 2\n",
    ),
    ("Root-1/Lib/test/bad.py", "def f(:\n"),
    ("Root-1/Lib/test/data.txt", "not python\n"),
    ("Root-1/Tools/outside.py", "y = 2\n"),
];

#[test]
fn it_extracts_only_the_path_and_lists_what_the_oracle_refuses() {
    let scratch = Scratch::new();
    let sha = build_archive(&scratch.0, MEMBERS);
    let manifest = manifest(
        &scratch.0,
        &sha,
        &python_version(),
        3,
        &scratch.0.join("archive.tgz"),
    );
    let dest = scratch.0.join("corpus");
    let out = fetch(&manifest, &dest);
    assert!(out.status.success(), "{}", stderr(&out));

    let tree = dest.join("fixture");
    assert!(tree.join("Lib/test/good.py").is_file());
    assert!(tree.join("Lib/test/sub/also_good.py").is_file());
    assert!(tree.join("Lib/test/data.txt").is_file());
    assert!(
        !tree.join("Tools").exists(),
        "a file outside the path was extracted"
    );

    let refused = fs::read_to_string(tree.join("refused.txt")).expect("refused.txt");
    let entries: Vec<&str> = refused.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(entries.len(), 1, "{refused}");
    assert!(
        entries[0].starts_with("Lib/test/bad.py\tSyntaxError: "),
        "{refused}"
    );
    let version = python_version();
    assert_eq!(
        refused.lines().next(),
        Some(format!("# 3 files read; CPython {version} accepts 2 and refuses 1.").as_str())
    );
    assert!(
        stdout(&out).contains("- Lib/test/bad.py"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn an_archive_whose_hash_differs_is_refused_and_nothing_is_extracted() {
    let scratch = Scratch::new();
    build_archive(&scratch.0, MEMBERS);
    let wrong = "0".repeat(64);
    let manifest = manifest(
        &scratch.0,
        &wrong,
        &python_version(),
        3,
        &scratch.0.join("archive.tgz"),
    );
    let dest = scratch.0.join("corpus");
    let out = fetch(&manifest, &dest);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains(&format!("not the pinned {wrong}; nothing was extracted")),
        "{err}"
    );
    assert!(!dest.join("fixture").exists());
}

#[test]
fn fewer_files_than_the_floor_fails_saying_how_many() {
    let scratch = Scratch::new();
    let sha = build_archive(&scratch.0, MEMBERS);
    let manifest = manifest(
        &scratch.0,
        &sha,
        &python_version(),
        10,
        &scratch.0.join("archive.tgz"),
    );
    let dest = scratch.0.join("corpus");
    let out = fetch(&manifest, &dest);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(
            "read 3 .py files for 'fixture' under 'Lib/test', fewer than the floor of 10"
        ),
        "{}",
        stderr(&out)
    );
    assert!(!dest.join("fixture").exists());
}

#[test]
fn another_python_than_the_oracle_is_refused_naming_both() {
    let scratch = Scratch::new();
    let sha = build_archive(&scratch.0, MEMBERS);
    let manifest = manifest(
        &scratch.0,
        &sha,
        "2.7.18",
        3,
        &scratch.0.join("archive.tgz"),
    );
    let out = fetch(&manifest, &scratch.0.join("corpus"));
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("is judged by CPython 2.7.18, but this is "),
        "{err}"
    );
    assert!(err.contains("python2.7 scripts/fetch-corpus"), "{err}");
}

#[test]
fn a_member_that_would_escape_the_directory_is_refused() {
    let scratch = Scratch::new();
    let sha = build_archive(
        &scratch.0,
        &[
            ("Root-1/Lib/test/good.py", "x = 1\n"),
            // Extraction happens in dest/.fixture-XXXX/tree, so five steps up from Lib/test is
            // the scratch directory itself: outside everything the run cleans up.
            ("Root-1/Lib/test/../../../../../escaped.py", "y = 2\n"),
        ],
    );
    let manifest = manifest(
        &scratch.0,
        &sha,
        &python_version(),
        1,
        &scratch.0.join("archive.tgz"),
    );
    let dest = scratch.0.join("corpus");
    let out = fetch(&manifest, &dest);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("extracting 'Root-1/Lib/test/' from the archive"),
        "{}",
        stderr(&out)
    );
    assert!(!scratch.0.join("escaped.py").exists());
    assert!(!dest.join("escaped.py").exists());
    assert!(!dest.join("fixture").exists());
}

#[test]
fn a_completed_fetch_at_the_same_pin_is_not_fetched_again() {
    let scratch = Scratch::new();
    let sha = build_archive(&scratch.0, MEMBERS);
    let version = python_version();
    let first = manifest(
        &scratch.0,
        &sha,
        &version,
        3,
        &scratch.0.join("archive.tgz"),
    );
    let dest = scratch.0.join("corpus");
    assert!(fetch(&first, &dest).status.success());
    // The archive is gone, so a second fetch could only succeed by reusing the first.
    fs::remove_file(scratch.0.join("archive.tgz")).expect("removing the archive");
    let out = fetch(&first, &dest);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("3 files read"), "{}", stdout(&out));
}

#[test]
fn a_manifest_missing_a_field_is_refused_naming_it() {
    let scratch = Scratch::new();
    let path = scratch.0.join("manifest.toml");
    fs::write(&path, "[fixture]\narchive = \"file:///x\"\n").expect("writing the manifest");
    let out = fetch(&path, &scratch.0.join("corpus"));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("source 'fixture' in ")
            && stderr(&out).contains("has no sha256, root, path, oracle, floor"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_link_pointing_outside_the_directory_is_refused() {
    let scratch = Scratch::new();
    let sha = build_archive(
        &scratch.0,
        &[
            ("Root-1/Lib/test/good.py", "x = 1\n"),
            ("Root-1/Lib/test/link.py", "->/etc/passwd"),
        ],
    );
    let archive = scratch.0.join("archive.tgz");
    let manifest = manifest(&scratch.0, &sha, &python_version(), 1, &archive);
    let dest = scratch.0.join("corpus");
    let out = fetch(&manifest, &dest);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("extracting 'Root-1/Lib/test/' from the archive"),
        "{}",
        stderr(&out)
    );
    assert!(!dest.join("fixture").exists());
}

#[test]
fn a_changed_pin_is_fetched_again() {
    let scratch = Scratch::new();
    let version = python_version();
    let first_dir = scratch.0.join("first");
    let second_dir = scratch.0.join("second");
    fs::create_dir_all(&first_dir).expect("making a directory");
    fs::create_dir_all(&second_dir).expect("making a directory");
    let first_sha = build_archive(&first_dir, MEMBERS);
    let mut more = MEMBERS.to_vec();
    more.push(("Root-1/Lib/test/added.py", "z = 3\n"));
    let second_sha = build_archive(&second_dir, &more);
    let dest = scratch.0.join("corpus");
    let first = manifest(
        &first_dir,
        &first_sha,
        &version,
        3,
        &first_dir.join("archive.tgz"),
    );
    assert!(fetch(&first, &dest).status.success());
    let second = manifest(
        &second_dir,
        &second_sha,
        &version,
        3,
        &second_dir.join("archive.tgz"),
    );
    let out = fetch(&second, &dest);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dest.join("fixture/Lib/test/added.py").is_file());
    assert!(stdout(&out).contains("4 files read"), "{}", stdout(&out));
}

#[test]
fn a_failed_download_names_the_url_and_leaves_nothing_behind() {
    let scratch = Scratch::new();
    let missing = scratch.0.join("missing.tgz");
    let manifest = manifest(&scratch.0, &"0".repeat(64), &python_version(), 1, &missing);
    let dest = scratch.0.join("corpus");
    let out = fetch(&manifest, &dest);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains(&format!(
            "downloading the archive for 'fixture' from 'file://{}': ",
            missing.display()
        )),
        "{err}"
    );
    let left: Vec<_> = fs::read_dir(&dest)
        .map(|entries| entries.map(|e| e.expect("an entry").path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn a_failed_refetch_keeps_the_previous_tree() {
    let scratch = Scratch::new();
    let version = python_version();
    let sha = build_archive(&scratch.0, MEMBERS);
    let archive = scratch.0.join("archive.tgz");
    let dest = scratch.0.join("corpus");
    let good = manifest(&scratch.0, &sha, &version, 3, &archive);
    assert!(fetch(&good, &dest).status.success());
    let stamp = fs::read_to_string(dest.join("fixture/.fetched")).expect("the stamp");
    // A new pin whose archive fails its hash: the refetch fails, and the old tree stands.
    let bad = manifest(&scratch.0, &"0".repeat(64), &version, 3, &archive);
    assert_eq!(fetch(&bad, &dest).status.code(), Some(1));
    assert!(dest.join("fixture/Lib/test/good.py").is_file());
    assert_eq!(
        fs::read_to_string(dest.join("fixture/.fetched")).expect("the stamp"),
        stamp
    );
}
