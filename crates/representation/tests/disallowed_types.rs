//! This crate's `clippy.toml` forbids `HashMap` and `HashSet`, and this test shows the lint
//! firing rather than trusting the configuration to be read.
//!
//! It builds a throwaway crate beside the build's own output, gives it this crate's
//! `clippy.toml`, and runs clippy over it twice: once with a `HashMap`, which must fail naming
//! the lint, and once without, which must pass - so a failure is the lint and not a broken
//! harness.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Writes a crate whose library is `source`, under the build's scratch directory, and returns
/// its directory.
fn scratch_crate(name: &str, source: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    // A leftover from an earlier run would be overwritten file by file anyway; clearing it
    // keeps a removed file from lingering.  Anything but "not there" stops the test.
    match fs::remove_dir_all(&dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            panic!("clearing {}: {err}", dir.display())
        }
        _ => {}
    }
    fs::create_dir_all(dir.join("src")).expect("creating the scratch crate");
    // Its own workspace, so cargo does not take it for a member of this one.
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n"
        ),
    )
    .expect("writing the scratch manifest");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("clippy.toml"),
        dir.join("clippy.toml"),
    )
    .expect("copying this crate's clippy.toml");
    fs::write(dir.join("src/lib.rs"), source).expect("writing the scratch library");
    dir
}

/// Runs clippy over the crate in `dir` as CI does, returning whether it passed and what it said.
fn clippy(dir: &Path) -> (bool, String) {
    let output = Command::new(env!("CARGO"))
        .args(["clippy", "--quiet", "--", "-D", "warnings"])
        .current_dir(dir)
        .output()
        .expect("running cargo clippy");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_hash_map_is_refused_naming_the_lint() {
    let dir = scratch_crate(
        "disallowed_types_hash_map",
        "//! Scratch.\n\n/// Scratch.\npub fn make() -> std::collections::HashMap<u8, u8> {\n    \
         std::collections::HashMap::new()\n}\n",
    );
    let (passed, said) = clippy(&dir);
    assert!(!passed, "clippy accepted a HashMap:\n{said}");
    assert!(said.contains("disallowed_types"), "{said}");
    assert!(said.contains("std::collections::HashMap"), "{said}");
}

#[test]
fn a_hash_set_is_refused_naming_the_lint() {
    let dir = scratch_crate(
        "disallowed_types_hash_set",
        "//! Scratch.\n\n/// Scratch.\npub fn make() -> std::collections::HashSet<u8> {\n    \
         std::collections::HashSet::new()\n}\n",
    );
    let (passed, said) = clippy(&dir);
    assert!(!passed, "clippy accepted a HashSet:\n{said}");
    assert!(said.contains("std::collections::HashSet"), "{said}");
}

#[test]
fn the_same_crate_with_a_vec_passes() {
    let dir = scratch_crate(
        "disallowed_types_control",
        "//! Scratch.\n\n/// Scratch.\npub fn make() -> Vec<u8> {\n    Vec::new()\n}\n",
    );
    let (passed, said) = clippy(&dir);
    assert!(
        passed,
        "clippy refused the control crate, so the harness is broken:\n{said}"
    );
}
