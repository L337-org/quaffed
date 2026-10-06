//! docker-mcp never enters either corpus.
//!
//! docker-mcp is the repository quaff's first version will be trialled on, so code from it in
//! the corpora would tune quaff against the very code it is then judged by, and flatter the
//! trial.  Code enters the corpora two ways, and this test closes both: a fetched source named in
//! `corpus/real-world.toml`, and a file added to `corpus/synthetic/`.  `verification.md` says why.

use std::fs;
use std::path::{Path, PathBuf};

/// How docker-mcp is written: its repository and distribution name, and its Python package.
const NAMES: &[&str] = &["docker-mcp", "docker_mcp"];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/python; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/python has a grandparent")
        .to_path_buf()
}

/// Whether `text` names docker-mcp, in any case.
fn names_docker_mcp(text: &str) -> bool {
    let lower = text.to_lowercase();
    NAMES.iter().any(|name| lower.contains(name))
}

/// The lines of the manifest that name docker-mcp, comments aside: a comment may explain the
/// exclusion, but no value may point at it.
fn offending_manifest_lines(manifest: &str) -> Vec<String> {
    manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && names_docker_mcp(line))
        .map(str::to_owned)
        .collect()
}

/// Every file under `dir` whose content or path names docker-mcp, relative to `dir`.
fn offending_files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, found: &mut Vec<String>, read: &mut usize) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(root, &path, found, read);
                continue;
            }
            *read += 1;
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            let bytes =
                fs::read(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
            if names_docker_mcp(&relative) || names_docker_mcp(&String::from_utf8_lossy(&bytes)) {
                found.push(relative);
            }
        }
    }
    let mut found = Vec::new();
    let mut read = 0;
    walk(dir, dir, &mut found, &mut read);
    // A walk that read nothing would pass having checked nothing.
    assert!(read > 0, "found no files under {}", dir.display());
    found.sort();
    found
}

#[test]
fn no_fetched_source_is_docker_mcp() {
    let path = workspace_root().join("corpus/real-world.toml");
    let manifest =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    let lines = offending_manifest_lines(&manifest);
    assert!(
        lines.is_empty(),
        "{} names docker-mcp, the repository the first version is trialled on, which no corpus \
         may hold: {}",
        path.display(),
        lines.join(" | ")
    );
}

#[test]
fn no_synthetic_file_is_from_docker_mcp() {
    let dir = workspace_root().join("corpus/synthetic");
    let files = offending_files(&dir);
    assert!(
        files.is_empty(),
        "these files under {} name docker-mcp, the repository the first version is trialled on, \
         which no corpus may hold: {}",
        dir.display(),
        files.join(", ")
    );
}

#[test]
fn docker_mcp_is_found_however_a_source_names_it() {
    for value in [
        "repository = \"https://github.com/L337-org/docker-mcp\"",
        "repository = \"git@github.com:L337-org/docker-mcp.git\"",
        "archive = \"https://github.com/L337-org/Docker-MCP/archive/refs/tags/v1.tar.gz\"",
        "root = \"docker_mcp-1.0\"",
    ] {
        let manifest = format!("[trial]\n{value}\n");
        assert_eq!(offending_manifest_lines(&manifest), [value], "{value}");
    }
    assert_eq!(
        offending_manifest_lines("# docker-mcp is excluded\n[cpython]\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_synthetic_file_from_docker_mcp_is_found_by_content_or_path() {
    let dir = std::env::temp_dir().join(format!("quaffed-exclusion-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("modules")).expect("making the directory");
    fs::write(dir.join("modules/clean.py"), "x = 1\n").expect("writing");
    fs::write(
        dir.join("modules/copied.py"),
        "from docker_mcp.server import run\n",
    )
    .expect("writing");
    fs::write(dir.join("modules/docker-mcp-sample.py"), "y = 2\n").expect("writing");
    let found = offending_files(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(found, ["modules/copied.py", "modules/docker-mcp-sample.py"]);
}
