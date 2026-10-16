//! `scripts/test-deb` retries a registry's server error, and nothing else.
//!
//! Building a distribution's image pulls its base image from a registry outside the test's
//! control, and a server error from it says nothing about the package.  These run the script
//! with a stand-in `docker` first on the path, which fails an image build as each case needs,
//! and check how often the build was tried and what the script said.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

/// A throwaway directory holding a package file and a stand-in `docker`.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    /// `build` is the shell the stand-in runs for `docker build`, with `$n` the attempt number.
    fn new(label: &str, build: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("quaffed-test-deb-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).expect("creating the sandbox");
        // Only its name is read before docker is asked anything.
        fs::write(dir.join("quaffed_1.0_amd64.deb"), "").expect("writing a package file");
        let counter = dir.join("builds");
        let docker = dir.join("bin/docker");
        fs::write(
            &docker,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  version) echo amd64 ;;\n  build)\n    \
                 n=$(( $(cat '{counter}' 2>/dev/null || echo 0) + 1 ))\n    \
                 echo \"$n\" > '{counter}'\n    {build}\n    ;;\n  run) exit 0 ;;\nesac\n",
                counter = counter.display()
            ),
        )
        .expect("writing the stand-in docker");
        let mut perms = fs::metadata(&docker)
            .expect("stand-in metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&docker, perms).expect("making the stand-in executable");
        Sandbox { dir }
    }

    fn builds(&self) -> usize {
        fs::read_to_string(self.dir.join("builds"))
            .map_or(0, |text| text.trim().parse().expect("a count"))
    }

    /// Runs the install test on one distribution, with no pause between attempts.
    fn run(&self) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.dir.join("bin").display());
        Command::new("bash")
            .arg(workspace_root().join("scripts/test-deb"))
            .arg(self.dir.join("quaffed_1.0_amd64.deb"))
            .arg("debian-13")
            .env_clear()
            .env("PATH", path)
            .env("TEST_DEB_RETRY_PAUSE", "0")
            .output()
            .expect("running bash")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// What docker said when Docker Hub answered a pull with 502 Bad Gateway.
const BAD_GATEWAY: &str = "echo 'ERROR: failed to build: failed to solve: debian:13: failed to \
     copy: httpReadSeeker: failed open: unexpected status code \
     https://registry-1.docker.io/v2/library/debian/manifests/sha256:0: 502 Bad Gateway' >&2; \
     exit 1";

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_registry_server_error_is_retried_and_the_test_goes_on() {
    let sandbox = Sandbox::new(
        "once",
        &format!("if [ \"$n\" -eq 1 ]; then {BAD_GATEWAY}; fi; echo sha256:built"),
    );
    let output = sandbox.run();
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert_eq!(sandbox.builds(), 2, "{err}");
    assert!(
        err.contains("the registry answered the debian-13 image build with a server error"),
        "{err}"
    );
    assert!(err.contains("502 Bad Gateway"), "{err}");
}

#[test]
fn a_registry_that_keeps_failing_fails_after_three_attempts_with_docker_s_error() {
    let sandbox = Sandbox::new("always", BAD_GATEWAY);
    let output = sandbox.run();
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert_eq!(sandbox.builds(), 3, "{err}");
    assert!(err.contains("could not build the debian-13 image"), "{err}");
    assert!(err.contains("attempt 3"), "{err}");
    assert!(err.contains("unexpected status code"), "{err}");
}

#[test]
fn any_other_build_failure_is_not_retried() {
    let sandbox = Sandbox::new(
        "other",
        "echo 'ERROR: failed to solve: failed to read dockerfile: no such file' >&2; exit 1",
    );
    let output = sandbox.run();
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert_eq!(sandbox.builds(), 1, "{err}");
    assert!(!err.contains("retrying"), "{err}");
    assert!(err.contains("failed to read dockerfile"), "{err}");
}
