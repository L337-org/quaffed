//! The verification-tool manifest, the script that installs from it, and the CI configuration
//! that must not go around it.
//!
//! `tools.toml` is the one place a tool version is named, so that a local run and CI cannot
//! disagree about what clean means.  These tests hold that in place: the manifest is well
//! formed, `scripts/install-tools` reads it exactly as this file does, the script stops with
//! the tool and version named when an install fails, no workflow names a version or installs a
//! tool itself, and `gungraun-runner` moves with the `gungraun` library.
//! `architecture/verification.md` has the reasoning.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SECTIONS: [&str; 3] = ["cargo-install", "toolchain", "toolchain-components"];

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    section: String,
    name: String,
    value: String,
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// Parses the manifest's TOML subset, failing on any line outside it.
///
/// The same grammar as `parse_manifest` in `scripts/install-tools`, written independently so
/// that `the_install_script_reads_the_manifest_as_this_test_does` can compare the two.
fn parse_manifest(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut section: Option<&str> = None;
    for (index, line) in text.lines().enumerate() {
        let lineno = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if !SECTIONS.contains(&name) {
                return Err(format!("line {lineno}: unknown section [{name}]"));
            }
            section = Some(name);
            continue;
        }
        let Some((name, quoted)) = line.split_once(" = ") else {
            return Err(format!(
                "line {lineno}: not a section header or entry: {line:?}"
            ));
        };
        let value = quoted
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .filter(|v| !v.is_empty() && !v.contains('"'))
            .ok_or_else(|| format!("line {lineno}: value is not one quoted string: {line:?}"))?;
        let name_ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !name_ok {
            return Err(format!(
                "line {lineno}: name is not lower-case kebab: {name:?}"
            ));
        }
        let section =
            section.ok_or_else(|| format!("line {lineno}: entry before any section: {line:?}"))?;
        entries.push(Entry {
            section: section.to_owned(),
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(entries)
}

fn is_exact_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_dated_nightly(value: &str) -> bool {
    // nightly-YYYY-MM-DD: an undated channel moves underneath every run that uses it.
    let Some(date) = value.strip_prefix("nightly-") else {
        return false;
    };
    let parts: Vec<&str> = date.split('-').collect();
    parts.len() == 3
        && [4, 2, 2]
            .iter()
            .zip(&parts)
            .all(|(len, p)| p.len() == *len && p.chars().all(|c| c.is_ascii_digit()))
}

/// Returns every way the parsed manifest breaks the rules `architecture/verification.md` sets.
fn manifest_problems(entries: &[Entry]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    let toolchains: BTreeSet<&str> = entries
        .iter()
        .filter(|e| e.section == "toolchain")
        .map(|e| e.name.as_str())
        .collect();
    for e in entries {
        // One namespace, because `install-tools NAME` looks a name up in both.
        if e.section != "toolchain-components" && !seen.insert(e.name.as_str()) {
            problems.push(format!("{} is named twice", e.name));
        }
        match e.section.as_str() {
            "cargo-install" if !is_exact_version(&e.value) => problems.push(format!(
                "{} = {:?} is not an exact MAJOR.MINOR.PATCH version",
                e.name, e.value
            )),
            "toolchain" if !is_dated_nightly(&e.value) => problems.push(format!(
                "{} = {:?} is not a nightly pinned by date (nightly-YYYY-MM-DD)",
                e.name, e.value
            )),
            "toolchain-components" if !toolchains.contains(e.name.as_str()) => {
                problems.push(format!(
                    "components are listed for {}, which is not a [toolchain]",
                    e.name
                ));
            }
            _ => {}
        }
    }
    problems
}

fn manifest() -> Vec<Entry> {
    let path = workspace_root().join("tools.toml");
    parse_manifest(&read(&path)).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

#[test]
fn the_manifest_is_well_formed() {
    let entries = manifest();
    // An empty manifest would pass every rule below by having nothing to break them.
    assert!(
        entries.iter().any(|e| e.section == "cargo-install"),
        "tools.toml names no [cargo-install] tool, so the checks below verified nothing"
    );
    let problems = manifest_problems(&entries);
    assert!(problems.is_empty(), "tools.toml: {}", problems.join("; "));
}

#[test]
fn malformed_manifests_are_rejected_by_the_test_and_the_script_alike() {
    for (index, (text, line, expected)) in [
        (
            "[cargo-install]\ncargo-insta = \"1.48.0\" # pinned\n",
            2,
            "not a section header",
        ),
        (
            "[cargo-install]\ncargo-insta = 1.48.0\n",
            2,
            "not a section header",
        ),
        (
            "[cargo-install]\ncargo-insta = \"\"\n",
            2,
            "not a section header",
        ),
        (
            "[cargo-install]\nCargo_Insta = \"1.48.0\"\n",
            2,
            "not a section header",
        ),
        ("cargo-insta = \"1.48.0\"\n", 1, "entry before any section"),
        ("[cargo]\ncargo-insta = \"1.48.0\"\n", 1, "unknown section"),
    ]
    .into_iter()
    .enumerate()
    {
        let err = parse_manifest(text).expect_err(text);
        assert!(
            err.contains(&format!("line {line}")),
            "{text:?} gave {err:?}, not line {line}"
        );
        // The script must refuse the same input at the same line, or the two readings differ
        // on exactly the inputs the --list comparison never sees.
        let sandbox = Sandbox::new(&format!("malformed-{index}"), text);
        let output = sandbox.run(&["--list"]);
        assert!(!output.status.success(), "the script accepted {text:?}");
        let err = stderr(&output);
        assert!(
            err.contains(&format!("tools.toml:{line}: {expected}")),
            "the script gave {err:?} for {text:?}"
        );
    }
    for (text, expected) in [
        ("[cargo-install]\ncargo-insta = \"1.48\"\n", "not an exact"),
        (
            "[cargo-install]\ncargo-insta = \"^1.48.0\"\n",
            "not an exact",
        ),
        (
            "[toolchain]\nnightly-coverage = \"nightly\"\n",
            "pinned by date",
        ),
        (
            "[toolchain]\nnightly-coverage = \"nightly-2026-10\"\n",
            "pinned by date",
        ),
        (
            "[toolchain-components]\nnightly-x = \"llvm-tools-preview\"\n",
            "not a [toolchain]",
        ),
        (
            "[cargo-install]\nx = \"1.0.0\"\n[toolchain]\nx = \"nightly-2026-10-01\"\n",
            "named twice",
        ),
    ] {
        let entries = parse_manifest(text).expect(text);
        let problems = manifest_problems(&entries).join("; ");
        assert!(
            problems.contains(expected),
            "{text:?} gave {problems:?}, not {expected:?}"
        );
    }
}

/// A copy of `scripts/install-tools` beside a fixture manifest, in a throwaway directory.
///
/// The script finds its manifest at `../tools.toml` from its own directory, so a copy reads
/// the fixture and never the repository's file.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(label: &str, manifest: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "quaffed-install-tools-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("scripts")).expect("creating the sandbox");
        fs::create_dir_all(dir.join("bin")).expect("creating the sandbox's bin");
        fs::copy(
            workspace_root().join("scripts/install-tools"),
            dir.join("scripts/install-tools"),
        )
        .expect("copying scripts/install-tools");
        fs::write(dir.join("tools.toml"), manifest).expect("writing the fixture manifest");
        // The script runs with PATH set to bin/ alone, so only the stubs written there exist.
        // It needs `dirname` besides its builtins.
        let dirname = ["/usr/bin/dirname", "/bin/dirname"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("dirname in /usr/bin or /bin");
        std::os::unix::fs::symlink(dirname, dir.join("bin/dirname")).expect("linking dirname");
        Sandbox { dir }
    }

    /// Writes an executable stub that appends its arguments to `<name>.calls` and then runs
    /// `body`.
    fn stub(&self, name: &str, body: &str) {
        let path = self.dir.join("bin").join(name);
        let calls = self.dir.join(format!("{name}.calls"));
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{body}\n",
                calls.display()
            ),
        )
        .expect("writing a stub");
        let mut perms = fs::metadata(&path).expect("stub metadata").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&path, perms).expect("making a stub executable");
    }

    fn calls(&self, name: &str) -> Vec<String> {
        fs::read_to_string(self.dir.join(format!("{name}.calls")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn run(&self, args: &[&str]) -> Output {
        // An absolute bash, because PATH below holds only the stubs.  On macOS this is the
        // system's bash 3.2, the oldest the script has to run under.
        let bash = ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("bash in /bin or /usr/bin");
        Command::new(bash)
            .arg(self.dir.join("scripts/install-tools"))
            .args(args)
            .env_clear()
            .env("PATH", self.dir.join("bin"))
            .output()
            .expect("running bash")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_install_script_reads_the_manifest_as_this_test_does() {
    let output = Command::new("bash")
        .arg(workspace_root().join("scripts/install-tools"))
        .arg("--list")
        .output()
        .expect("running bash");
    assert!(
        output.status.success(),
        "install-tools --list: {}",
        stderr(&output)
    );
    let listed: Vec<Entry> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.splitn(3, ' ');
            let mut next = || fields.next().unwrap_or_default().to_owned();
            Entry {
                section: next(),
                name: next(),
                value: next(),
            }
        })
        .collect();
    assert_eq!(listed, manifest());
}

#[test]
fn the_install_script_rejects_a_line_outside_the_subset() {
    let sandbox = Sandbox::new(
        "subset",
        "[cargo-install]\ncargo-insta = \"1.48.0\" # pinned\n",
    );
    let output = sandbox.run(&["--list"]);
    assert!(!output.status.success(), "a trailing comment was accepted");
    assert!(
        stderr(&output).contains("tools.toml:2: not a section header"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_install_script_installs_the_pinned_version_exactly() {
    let sandbox = Sandbox::new("exact", "[cargo-install]\ncargo-insta = \"1.48.0\"\n");
    sandbox.stub("cargo", "exit 0");
    let output = sandbox.run(&["cargo-insta"]);
    assert!(output.status.success(), "{}", stderr(&output));
    // `=`: a bare version is a caret requirement to `cargo install`, which would take a newer
    // compatible release.
    assert_eq!(
        sandbox.calls("cargo"),
        ["install --locked --version =1.48.0 cargo-insta"]
    );
}

#[test]
fn the_install_script_stops_naming_the_tool_and_version_when_cargo_fails() {
    let sandbox = Sandbox::new(
        "cargo-fails",
        "[cargo-install]\ncargo-insta = \"1.48.0\"\ncargo-mutants = \"27.1.0\"\n",
    );
    // Exit status and message as cargo 1.98.1 gave them for a version not in the registry.
    sandbox.stub(
        "cargo",
        "echo 'error: could not find `cargo-insta` in registry `crates-io` with version `=1.48.0`' >&2\nexit 101",
    );
    let output = sandbox.run(&["--all"]);
    assert!(
        !output.status.success(),
        "a failed install was reported as success"
    );
    let err = stderr(&output);
    assert!(
        err.contains("installing cargo-insta 1.48.0 failed: cargo install exited 101"),
        "{err}"
    );
    assert!(
        err.contains("could not find `cargo-insta`"),
        "cargo's own error was lost: {err}"
    );
    assert_eq!(
        sandbox.calls("cargo").len(),
        1,
        "carried on after a failure"
    );
}

#[test]
fn the_install_script_stops_naming_the_channel_when_rustup_fails() {
    let sandbox = Sandbox::new(
        "rustup-fails",
        "[toolchain]\nnightly-coverage = \"nightly-2026-10-01\"\n\
         [toolchain-components]\nnightly-coverage = \"llvm-tools-preview\"\n",
    );
    // Exit status and message as rustup 1.29.1 gave them for a channel with no release.
    sandbox.stub(
        "rustup",
        "echo \"error: no release found for 'nightly-2026-10-01'\" >&2\nexit 1",
    );
    let output = sandbox.run(&["nightly-coverage"]);
    assert!(
        !output.status.success(),
        "a failed install was reported as success"
    );
    assert_eq!(
        sandbox.calls("rustup"),
        ["toolchain install nightly-2026-10-01 --profile minimal --component llvm-tools-preview"]
    );
    assert!(
        stderr(&output).contains(
            "installing nightly-coverage (nightly-2026-10-01 with llvm-tools-preview) failed: rustup exited 1"
        ),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("no release found"),
        "rustup's own error was lost: {}",
        stderr(&output)
    );
}

#[test]
fn the_install_script_says_what_is_missing() {
    let sandbox = Sandbox::new(
        "missing",
        "[cargo-install]\ncargo-insta = \"1.48.0\"\n[toolchain]\nnightly-coverage = \"nightly-2026-10-01\"\n",
    );
    for (args, expected) in [
        (
            vec!["cargo-insta"],
            "installing cargo-insta 1.48.0 needs cargo, which is not on PATH",
        ),
        (
            vec!["nightly-coverage"],
            "needs rustup, which is not on PATH",
        ),
        (vec!["cargo-nope"], "cargo-nope is not in"),
        (vec![], "name the tools to install"),
        (
            vec!["--cargo", "nightly-coverage"],
            "--cargo takes no other arguments",
        ),
        (
            vec!["--list", "cargo-insta"],
            "--list takes no other arguments",
        ),
        (
            vec!["cargo-insta", "--all"],
            "options cannot follow tool names",
        ),
    ] {
        let output = sandbox.run(&args);
        assert!(!output.status.success(), "{args:?} succeeded");
        assert!(
            stderr(&output).contains(expected),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn cargo_installs_every_cargo_tool_and_all_adds_the_toolchains() {
    let sandbox = Sandbox::new(
        "selection",
        "[cargo-install]\ncargo-insta = \"1.48.0\"\ncargo-mutants = \"27.1.0\"\n\
         [toolchain]\nnightly-coverage = \"nightly-2026-10-01\"\n",
    );
    sandbox.stub("cargo", "exit 0");
    sandbox.stub("rustup", "exit 0");
    let cargo_calls = [
        "install --locked --version =1.48.0 cargo-insta",
        "install --locked --version =27.1.0 cargo-mutants",
    ];
    // --cargo leaves toolchains alone, which is what keeps a nightly out of a job that does
    // not use it.
    let output = sandbox.run(&["--cargo"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sandbox.calls("cargo"), cargo_calls);
    assert!(
        sandbox.calls("rustup").is_empty(),
        "--cargo installed a toolchain"
    );

    let output = sandbox.run(&["--all"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sandbox.calls("cargo")[2..], cargo_calls);
    assert_eq!(
        sandbox.calls("rustup"),
        ["toolchain install nightly-2026-10-01 --profile minimal"]
    );
}

/// Returns each line of a workflow or action file that installs a tool or names a version
/// itself, rather than going through `scripts/install-tools`.
///
/// Comments are dropped first: a YAML comment starts at a `#` at the beginning of a line or
/// after whitespace.  That misreads a ` #` inside a quoted string as a comment, which can only
/// hide a violation inside a string literal, never invent one.
fn workflow_violations(label: &str, text: &str, tools: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = match raw.find('#') {
            Some(0) => "",
            Some(i) if raw[..i].ends_with(char::is_whitespace) => &raw[..i],
            _ => raw,
        };
        let mut reasons: Vec<String> = Vec::new();
        for (pattern, why) in [
            ("cargo install", "installs a tool itself"),
            ("cargo binstall", "installs a tool itself"),
            ("cargo-binstall", "installs a tool itself"),
            ("install-action", "installs a tool itself"),
            ("rust-toolchain@", "names a toolchain in the workflow"),
            ("toolchain:", "names a toolchain in the workflow"),
            ("rustup component add", "names a toolchain in the workflow"),
            ("rustup default", "names a toolchain in the workflow"),
            ("rustup install ", "names a toolchain in the workflow"),
            ("rustup update ", "names a toolchain in the workflow"),
            ("rustup run ", "names a toolchain in the workflow"),
            ("rustup override", "names a toolchain in the workflow"),
            ("cargo +", "names a toolchain in the workflow"),
            ("+nightly", "names a toolchain in the workflow"),
        ] {
            if line.contains(pattern) {
                reasons.push(format!("{why} ({pattern:?})"));
            }
        }
        // `rustup toolchain install` with no argument reads rust-toolchain.toml; any argument
        // names a toolchain.
        if let Some(rest) = line.split("rustup toolchain install").nth(1)
            && !rest.trim().is_empty()
        {
            reasons.push("names a toolchain in the workflow (rustup toolchain install ...)".into());
        }
        if line.to_ascii_lowercase().contains("rustup_toolchain") {
            reasons.push("names a toolchain in the workflow (RUSTUP_TOOLCHAIN)".into());
        }
        if line.contains("nightly-20") {
            reasons.push("names a dated nightly".into());
        }
        for tool in tools {
            if line.contains(&format!("{tool}@")) || line.contains(&format!("{tool} --version")) {
                reasons.push(format!("names a version of {tool}"));
            }
        }
        for why in reasons {
            found.push(format!("{label}:{}: {why}: {}", index + 1, raw.trim()));
        }
    }
    found
}

fn yaml_files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry
            .unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()))
            .path();
        if path.is_dir() {
            yaml_files_under(&path, found);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml")
        {
            found.push(path);
        }
    }
}

fn tool_names() -> Vec<String> {
    manifest()
        .into_iter()
        .filter(|e| e.section != "toolchain-components")
        .map(|e| e.name)
        .collect()
}

#[test]
fn no_workflow_installs_a_tool_or_names_a_version() {
    let root = workspace_root();
    let mut files = Vec::new();
    yaml_files_under(&root.join(".github/workflows"), &mut files);
    yaml_files_under(&root.join(".github/actions"), &mut files);
    // Finding nothing would pass having checked nothing.
    assert!(
        files.iter().any(|f| f.ends_with("premerge.yaml")),
        "the scan did not find .github/workflows/premerge.yaml, so it checked nothing"
    );
    let tools = tool_names();
    let mut violations = Vec::new();
    for file in &files {
        let label = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        violations.extend(workflow_violations(&label, &read(file), &tools));
    }
    assert!(
        violations.is_empty(),
        "install tools with scripts/install-tools and pin them in tools.toml:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_workflow_check_catches_each_way_of_naming_a_version() {
    let tools = vec!["cargo-insta".to_owned()];
    for line in [
        "        run: cargo install --locked cargo-insta",
        "        run: cargo binstall cargo-insta",
        "      - uses: taiki-e/install-action@0123456789abcdef0123456789abcdef01234567",
        "      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567",
        "          toolchain: nightly",
        "        run: rustup toolchain install nightly-2026-10-01",
        "        run: rustup toolchain install stable",
        "        run: rustup component add llvm-tools-preview",
        "        run: rustup default nightly",
        "        run: rustup install nightly",
        "        run: rustup update nightly",
        "        run: rustup run nightly cargo llvm-cov",
        "          RUSTUP_TOOLCHAIN: nightly",
        "        run: cargo +nightly llvm-cov",
        "        run: RUSTUP_TOOLCHAIN=nightly-2026-10-01 cargo llvm-cov",
        "          tools: cargo-insta@1.48.0",
    ] {
        let found = workflow_violations("fixture.yaml", line, &tools);
        assert!(!found.is_empty(), "not caught: {line}");
        assert!(found[0].starts_with("fixture.yaml:1: "), "{found:?}");
    }
    for line in [
        "        run: rustup toolchain install",
        "        run: scripts/install-tools cargo-insta nightly-coverage",
        "        run: cargo insta test --check",
        "# A comment may mention cargo install and nightly-2026-10-01.",
        "        run: cargo test  # not cargo install",
    ] {
        let found = workflow_violations("fixture.yaml", line, &tools);
        assert!(found.is_empty(), "wrongly flagged: {found:?}");
    }
}

/// Returns every version of `package` that `Cargo.lock` records.
fn locked_versions(lock: &str, package: &str) -> Vec<String> {
    let mut versions = Vec::new();
    let mut current: Option<&str> = None;
    for line in lock.lines() {
        if line == "[[package]]" {
            current = None;
        } else if let Some(name) = line.strip_prefix("name = ") {
            current = Some(name.trim_matches('"'));
        } else if let Some(version) = line.strip_prefix("version = ")
            && current == Some(package)
        {
            versions.push(version.trim_matches('"').to_owned());
        }
    }
    versions
}

/// Checks that the manifest's `gungraun-runner` and the locked `gungraun` library agree.
///
/// gungraun refuses to run against a runner of a different version, so they move together:
/// either both are present at one version, or neither is.
fn gungraun_pair(runner: Option<&str>, library: &[String]) -> Result<(), String> {
    match (runner, library) {
        (None, []) => Ok(()),
        (Some(runner), [only]) if runner == only => Ok(()),
        (Some(runner), []) => Err(format!(
            "tools.toml pins gungraun-runner {runner}, but no gungraun library is in Cargo.lock"
        )),
        (None, _) => Err(format!(
            "Cargo.lock has gungraun {}, but tools.toml has no gungraun-runner",
            library.join(", ")
        )),
        (Some(runner), _) => Err(format!(
            "tools.toml pins gungraun-runner {runner}, but Cargo.lock has gungraun {}",
            library.join(", ")
        )),
    }
}

#[test]
fn gungraun_runner_and_library_agree() {
    let runner = manifest()
        .into_iter()
        .find(|e| e.section == "cargo-install" && e.name == "gungraun-runner")
        .map(|e| e.value);
    let library = locked_versions(&read(&workspace_root().join("Cargo.lock")), "gungraun");
    if let Err(err) = gungraun_pair(runner.as_deref(), &library) {
        panic!("{err}");
    }
}

#[test]
fn the_gungraun_check_fails_on_every_mismatch() {
    let v = |s: &str| vec![s.to_owned()];
    assert!(gungraun_pair(None, &[]).is_ok());
    assert!(gungraun_pair(Some("0.20.0"), &v("0.20.0")).is_ok());
    for (runner, library, expected) in [
        (
            Some("0.20.0"),
            v("0.20.1"),
            "Cargo.lock has gungraun 0.20.1",
        ),
        (Some("0.20.0"), vec![], "no gungraun library"),
        (None, v("0.20.0"), "no gungraun-runner"),
        (
            Some("0.20.0"),
            vec!["0.20.0".into(), "0.19.0".into()],
            "0.20.0, 0.19.0",
        ),
    ] {
        let err = gungraun_pair(runner, &library).expect_err(expected);
        assert!(err.contains(expected), "{err}");
    }
    let lock = "[[package]]\nname = \"gungraun\"\nversion = \"0.20.0\"\n\n\
                [[package]]\nname = \"gungraun-macros\"\nversion = \"0.20.9\"\n";
    assert_eq!(locked_versions(lock, "gungraun"), ["0.20.0"]);
}
