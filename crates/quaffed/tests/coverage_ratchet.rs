//! `scripts/coverage-ratchet` fails when a gated metric falls against main, names the files where
//! it fell, reports condition coverage without failing on it, and fails rather than passes when
//! it has nothing to compare.
//!
//! Each test writes llvm-cov summaries in the shape `scripts/coverage` produces and runs the script
//! with `python3`.  The measurement itself runs in CI's **Coverage** workflow;
//! `architecture/verification.md` describes the ratchet.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// One metric of one file in a summary: the llvm-cov field, then covered and count.
type Metric<'a> = (&'a str, u64, u64);

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

/// A throwaway directory, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-coverage-ratchet-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("making the scratch directory");
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes an llvm-cov summary named `name`: one file entry per `(filename, metrics)`, where
    /// each metric is `(field, covered, count)`, with totals summed over the files.
    fn summary(&self, name: &str, files: &[(&str, &[Metric])]) -> PathBuf {
        let mut totals: Vec<(String, u64, u64)> = Vec::new();
        let mut entries = Vec::new();
        for (filename, metrics) in files {
            let mut fields = Vec::new();
            for (field, covered, count) in *metrics {
                fields.push(format!(
                    "\"{field}\": {{\"count\": {count}, \"covered\": {covered}, \"percent\": 0}}"
                ));
                match totals.iter_mut().find(|t| t.0 == *field) {
                    Some(total) => {
                        total.1 += covered;
                        total.2 += count;
                    }
                    None => totals.push(((*field).to_owned(), *covered, *count)),
                }
            }
            entries.push(format!(
                "{{\"filename\": \"{filename}\", \"summary\": {{{}}}}}",
                fields.join(", ")
            ));
        }
        let totals: Vec<String> = totals
            .iter()
            .map(|(field, covered, count)| {
                format!(
                    "\"{field}\": {{\"count\": {count}, \"covered\": {covered}, \"percent\": 0}}"
                )
            })
            .collect();
        let json = format!(
            "{{\"type\": \"llvm.coverage.json.export\", \"version\": \"2.0.1\", \"data\": [{{\"files\": [{}], \"totals\": {{{}}}}}]}}",
            entries.join(", "),
            totals.join(", ")
        );
        let path = self.path(name);
        fs::write(&path, json).expect("writing a summary");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs the script with `--NAME FILE` pairs, failing the test if it cannot run at all.
fn ratchet(args: &[(&str, &Path)]) -> Output {
    let mut command = Command::new("python3");
    command.arg(workspace_root().join("scripts/coverage-ratchet"));
    for (name, path) in args {
        command.arg(format!("--{name}")).arg(path);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("running python3, which these tests need: {err}"))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const SAME: &[Metric] = &[("lines", 90, 100), ("regions", 45, 50)];
const CONDITIONS: &[Metric] = &[("branches", 18, 20)];

/// The four summaries with every metric the same on both sides, as a base to vary.
fn unchanged(scratch: &Scratch) -> [(&'static str, PathBuf); 4] {
    [
        (
            "base-stable",
            scratch.summary("base-stable.json", &[("src/a.rs", SAME)]),
        ),
        (
            "head-stable",
            scratch.summary("head-stable.json", &[("src/a.rs", SAME)]),
        ),
        (
            "base-condition",
            scratch.summary("base-condition.json", &[("src/a.rs", CONDITIONS)]),
        ),
        (
            "head-condition",
            scratch.summary("head-condition.json", &[("src/a.rs", CONDITIONS)]),
        ),
    ]
}

fn run(sides: &[(&'static str, PathBuf)]) -> Output {
    let args: Vec<(&str, &Path)> = sides.iter().map(|(n, p)| (*n, p.as_path())).collect();
    ratchet(&args)
}

#[test]
fn unchanged_coverage_passes_and_shows_every_metric() {
    let scratch = Scratch::new();
    let out = run(&unchanged(&scratch));
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(text.starts_with("## Coverage\n"), "{text}");
    assert!(
        text.contains("No gated metric fell against main."),
        "{text}"
    );
    assert!(
        text.contains("| lines | 90.00% (90/100) | 90.00% (90/100) | +0.00 | gated |"),
        "{text}"
    );
    assert!(
        text.contains("| regions | 90.00% (45/50) | 90.00% (45/50) | +0.00 | gated |"),
        "{text}"
    );
    assert!(
        text.contains("| conditions | 90.00% (18/20) | 90.00% (18/20) | +0.00 | report only |"),
        "{text}"
    );
}

#[test]
fn a_drop_in_lines_fails_naming_the_file_where_it_fell() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[1].1 = scratch.summary(
        "head-stable.json",
        &[
            ("src/a.rs", SAME),
            ("src/new.rs", &[("lines", 2, 10), ("regions", 5, 5)]),
        ],
    );
    let out = run(&sides);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(
        text.contains("**Coverage fell against main: lines.**"),
        "{text}"
    );
    assert!(text.contains("**Where it fell: lines**"), "{text}");
    assert!(
        text.contains("- `src/new.rs`: 8 more uncovered, now 2/10"),
        "{text}"
    );
    assert!(
        !text.contains("src/a.rs`"),
        "a file that did not fall is named: {text}"
    );
    assert!(
        stderr(&out).contains("coverage fell against main: lines"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_drop_in_regions_fails_on_its_own() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[1].1 = scratch.summary(
        "head-stable.json",
        &[("src/a.rs", &[("lines", 90, 100), ("regions", 44, 50)])],
    );
    let out = run(&sides);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("**Coverage fell against main: regions.**"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("- `src/a.rs`: 1 more uncovered, now 44/50"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_drop_in_condition_coverage_is_reported_without_failing() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[3].1 = scratch.summary(
        "head-condition.json",
        &[("src/a.rs", &[("branches", 17, 20)])],
    );
    let out = run(&sides);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(
        text.contains("| conditions | 90.00% (18/20) | 85.00% (17/20) | -5.00 | report only |"),
        "{text}"
    );
    assert!(
        text.contains("**Where it fell (report only): conditions**"),
        "{text}"
    );
    assert!(
        text.contains("No gated metric fell against main."),
        "{text}"
    );
}

#[test]
fn coverage_that_rises_passes() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[1].1 = scratch.summary(
        "head-stable.json",
        &[("src/a.rs", &[("lines", 95, 100), ("regions", 50, 50)])],
    );
    let out = run(&sides);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("| +5.00 | gated |"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn no_coverage_data_from_the_pull_request_fails_rather_than_passes() {
    let scratch = Scratch::new();
    for (label, head) in [
        ("missing", scratch.path("absent.json")),
        ("empty", scratch.summary("empty.json", &[])),
        ("not json", {
            let path = scratch.path("garbage.json");
            fs::write(&path, "not json").expect("writing");
            path
        }),
    ] {
        let mut sides = unchanged(&scratch).to_vec();
        sides[1].1 = head;
        let out = run(&sides);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{label}: {}{}",
            stdout(&out),
            stderr(&out)
        );
        assert!(
            stdout(&out).contains("**Not evaluated, so the ratchet fails:**"),
            "{label}: {}",
            stdout(&out)
        );
        assert!(
            stdout(&out).contains("lines: the pull request has no figure"),
            "{label}: {}",
            stdout(&out)
        );
        assert!(
            stderr(&out).contains("coverage-ratchet: not evaluated"),
            "{label}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_gated_metric_with_no_figure_on_main_reports_no_baseline_and_fails() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[0].1 = scratch.path("absent.json");
    let out = run(&sides);
    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(
        text.contains("| lines | no baseline: no summary at"),
        "{text}"
    );
    assert!(
        text.contains("lines: main has no figure to compare with"),
        "{text}"
    );
}

#[test]
fn a_report_only_metric_with_no_baseline_is_reported_and_passes() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch).to_vec();
    sides.remove(2);
    let out = run(&sides);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains(
            "| conditions | no baseline: not measured | 90.00% (18/20) | - | report only |"
        ),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_metric_that_counts_nothing_has_no_figure() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[0].1 = scratch.summary(
        "base-stable.json",
        &[("src/a.rs", &[("lines", 0, 0), ("regions", 45, 50)])],
    );
    let out = run(&sides);
    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("| lines | no baseline: nothing counted for lines |"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn the_summary_is_appended_to_the_named_file() {
    let scratch = Scratch::new();
    let summary = scratch.path("summary.md");
    fs::write(&summary, "earlier section\n").expect("writing");
    let mut sides = unchanged(&scratch).to_vec();
    sides.push(("summary", summary.clone()));
    let out = run(&sides);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let written = fs::read_to_string(&summary).expect("reading the summary");
    assert!(
        written.starts_with("earlier section\n## Coverage\n"),
        "{written}"
    );
    assert_eq!(&written["earlier section\n".len()..], stdout(&out));
}
