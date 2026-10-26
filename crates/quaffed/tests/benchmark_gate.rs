//! `scripts/benchmark-gate` fails a benchmark more than 2% above main's count, naming it and both
//! counts, fails an arena that costs more than its budget's share of Ruff's parse, and fails
//! rather than passes when it has nothing to compare.
//!
//! Each test writes gungraun summaries in the shape `--save-summary=json` produces and runs the
//! script with `python3`.  The counting itself runs in CI's **Benchmarks** job, where
//! `scripts/break-benchmarks` shows the gate failing on a really slowed build;
//! `architecture/verification.md` describes the gate.

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

/// A throwaway directory, removed when the test ends however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-benchmark-gate-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("making the scratch directory");
        Scratch(dir)
    }

    /// A side's directory holding one summary per `(function, id, instructions)`.
    fn side(&self, name: &str, benchmarks: &[(&str, &str, u64)]) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).expect("making a side's directory");
        for (function, id, instructions) in benchmarks {
            write_summary(&dir, function, id, &instructions.to_string(), "7");
        }
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes one benchmark's summary under `dir`, where gungraun puts it, with the parts of the
/// format the gate reads.
fn write_summary(dir: &Path, function: &str, id: &str, instructions: &str, version: &str) {
    let at = dir.join(format!(
        "quaffed-python/front_end/front_end/{function}.{id}"
    ));
    fs::create_dir_all(&at).expect("making a benchmark's directory");
    let json = format!(
        r#"{{"version": "{version}", "function_name": "{function}", "id": "{id}", "profiles": [{{"tool": "Callgrind", "data": {{"total": {{"metrics": {{"Ir": {{"values": {{"new": {instructions}}}}}, "EstimatedCycles": {{"values": {{"new": 1}}}}}}}}}}}}]}}"#
    );
    fs::write(at.join("summary.json"), json).expect("writing a summary");
}

/// Main's counts, as a base to vary: the arena costs 50% of Ruff's parse.
const MAIN: &[(&str, &str, u64)] = &[
    ("ruff_parse", "synthetic", 1_000_000),
    ("parse_into_arena", "synthetic", 1_500_000),
    ("matching", "a_name_used_twice", 200_000),
];

fn gate(main: &Path, pull_request: &Path, extra: &[&str]) -> Output {
    Command::new("python3")
        .arg(workspace_root().join("scripts/benchmark-gate"))
        .arg("--main")
        .arg(main)
        .arg("--pull-request")
        .arg(pull_request)
        .args(extra)
        .output()
        .unwrap_or_else(|err| panic!("running python3, which these tests need: {err}"))
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Main's counts with `changes` applied on the pull request's side.
fn with(changes: &[(&str, &str, u64)]) -> Vec<(&'static str, &'static str, u64)> {
    MAIN.iter()
        .map(|&(function, id, count)| {
            let on_the_pull_request = changes
                .iter()
                .find(|(f, i, _)| *f == function && *i == id)
                .map_or(count, |c| c.2);
            (function, id, on_the_pull_request)
        })
        .collect()
}

#[test]
fn unchanged_counts_pass_and_show_every_benchmark_and_the_arenas_share() {
    let scratch = Scratch::new();
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", MAIN),
        &["--label", "amd64"],
    );
    let all = text(&out);
    assert_eq!(out.status.code(), Some(0), "{all}");
    assert!(all.starts_with("## Instruction counts (amd64)\n"), "{all}");
    assert!(
        all.contains("| `matching a_name_used_twice` | 200,000 | 200,000 | +0.0% |"),
        "{all}"
    );
    assert!(
        all.contains("| `ruff_parse synthetic` | 1,000,000 | 1,000,000 | +0.0% |"),
        "{all}"
    );
    assert!(
        all.contains(
            "Building the arena costs **50.0%** of Ruff's parse, against a budget of 60%; on main it costs 50.0%."
        ),
        "{all}"
    );
}

#[test]
fn a_rise_of_more_than_two_percent_fails_naming_the_benchmark_and_both_counts() {
    let scratch = Scratch::new();
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", &with(&[("matching", "a_name_used_twice", 204_002)])),
        &[],
    );
    let all = text(&out);
    assert_eq!(out.status.code(), Some(1), "{all}");
    assert!(
        all.contains(
            "benchmark-gate: matching a_name_used_twice: 200,000 on main, 204,002 on the pull request, +2.0%, more than the 2% allowed"
        ),
        "{all}"
    );
    assert!(
        all.contains("| `matching a_name_used_twice` | 200,000 | 204,002 | +2.0% **regressed** |"),
        "{all}"
    );
}

#[test]
fn a_rise_of_exactly_two_percent_passes() {
    let scratch = Scratch::new();
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", &with(&[("matching", "a_name_used_twice", 204_000)])),
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
}

#[test]
fn a_fall_passes() {
    let scratch = Scratch::new();
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", &with(&[("matching", "a_name_used_twice", 100_000)])),
        &[],
    );
    let all = text(&out);
    assert_eq!(out.status.code(), Some(0), "{all}");
    assert!(all.contains("| 200,000 | 100,000 | -50.0% |"), "{all}");
}

#[test]
fn an_arena_over_its_budget_fails_saying_the_share_and_the_budget() {
    let scratch = Scratch::new();
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", &with(&[("parse_into_arena", "synthetic", 1_610_000)])),
        &[],
    );
    let all = text(&out);
    assert_eq!(out.status.code(), Some(1), "{all}");
    assert!(
        all.contains(
            "benchmark-gate: building the arena costs 61.0% of Ruff's parse, over its budget of 60%"
        ),
        "{all}"
    );
}

#[test]
fn a_slower_arena_cannot_hide_inside_a_faster_parse() {
    // Ruff's parse falls and the arena's cost rises by as much: the total moves by nothing, so
    // the 2% rule passes it, and only the budget fails it.
    let scratch = Scratch::new();
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", &with(&[("ruff_parse", "synthetic", 900_000)])),
        &[],
    );
    let all = text(&out);
    assert_eq!(out.status.code(), Some(1), "{all}");
    assert!(
        all.contains("building the arena costs 66.7% of Ruff's parse, over its budget of 60%"),
        "{all}"
    );
    assert!(!all.contains("more than the 2% allowed"), "{all}");
}

#[test]
fn a_new_benchmark_and_a_removed_one_are_reported_and_pass() {
    let scratch = Scratch::new();
    let mut pr: Vec<_> = MAIN.iter().filter(|b| b.0 != "matching").copied().collect();
    pr.push(("compiling", "a_name_used_twice", 50_000));
    let out = gate(&scratch.side("main", MAIN), &scratch.side("pr", &pr), &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(0), "{all}");
    assert!(
        all.contains("| `compiling a_name_used_twice` | new | 50,000 | - |"),
        "{all}"
    );
    assert!(
        all.contains("| `matching a_name_used_twice` | 200,000 | removed | - |"),
        "{all}"
    );
}

#[test]
fn main_with_no_benchmarks_is_accepted_only_when_it_says_so() {
    let scratch = Scratch::new();
    let pr = scratch.side("pr", MAIN);
    let said = scratch.side("said", &[]);
    fs::write(said.join("no-benchmarks"), "").expect("writing the marker");
    let out = gate(&said, &pr, &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(0), "{all}");
    assert!(
        all.contains("main has no benchmarks, so nothing was compared; every benchmark is new."),
        "{all}"
    );

    let silent = scratch.side("silent", &[]);
    let out = gate(&silent, &pr, &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    assert!(
        all.contains("benchmark-gate: not evaluated - main: no benchmark summary under"),
        "{all}"
    );
}

#[test]
fn no_benchmark_on_the_pull_request_fails_rather_than_passes() {
    let scratch = Scratch::new();
    let main = scratch.side("main", MAIN);
    let empty = scratch.side("pr", &[]);
    let out = gate(&main, &empty, &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    assert!(
        all.contains("benchmark-gate: not evaluated - pull request: no benchmark summary under"),
        "{all}"
    );

    fs::write(empty.join("no-benchmarks"), "").expect("writing the marker");
    let out = gate(&main, &empty, &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    assert!(
        all.contains("says it has no benchmarks, which only main may"),
        "{all}"
    );

    let out = gate(&main, &scratch.0.join("never-made"), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn a_summary_in_a_format_it_does_not_know_is_refused() {
    let scratch = Scratch::new();
    let pr = scratch.side("pr", MAIN);
    write_summary(&pr, "matching", "another", "100", "8");
    let out = gate(&scratch.side("main", MAIN), &pr, &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    assert!(
        all.contains("summary version '8', where this script reads version 7"),
        "{all}"
    );
}

#[test]
fn a_count_that_is_not_a_positive_whole_number_is_refused() {
    let scratch = Scratch::new();
    let pr = scratch.side("pr", MAIN);
    write_summary(&pr, "matching", "another", "0", "7");
    let out = gate(&scratch.side("main", MAIN), &pr, &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    assert!(
        all.contains("instruction count 0 is not a positive whole number"),
        "{all}"
    );
}

#[test]
fn without_both_of_the_arenas_benchmarks_the_gate_has_not_passed() {
    let scratch = Scratch::new();
    let pr: Vec<_> = MAIN
        .iter()
        .filter(|b| b.0 != "ruff_parse")
        .copied()
        .collect();
    let out = gate(&scratch.side("main", MAIN), &scratch.side("pr", &pr), &[]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    assert!(
        all.contains("the arena's share cannot be worked out"),
        "{all}"
    );
}

#[test]
fn the_summary_is_appended_to_the_named_file() {
    let scratch = Scratch::new();
    let file = scratch.0.join("summary.md");
    fs::write(&file, "before\n").expect("writing the summary file");
    let out = gate(
        &scratch.side("main", MAIN),
        &scratch.side("pr", MAIN),
        &["--summary", file.to_str().expect("a UTF-8 path")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let written = fs::read_to_string(&file).expect("reading the summary file");
    assert!(
        written.starts_with("before\n## Instruction counts\n"),
        "{written}"
    );
}
