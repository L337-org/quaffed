//! `scripts/coverage-ratchet` fails when a gated metric is below its fixed floor, or when the lines
//! a pull request adds are covered below their floor with more than a few uncovered; it reports
//! main's figures and condition coverage without failing on them, and fails rather than passes
//! when it has nothing to judge.
//!
//! Each test writes llvm-cov summaries, lcov and a diff in the shapes `scripts/coverage` and
//! `git diff` produce, and runs the script with `python3`.  The floors are the script's own, so a
//! test whose figures sit just either side of one moves with it.  The measurement itself runs in CI's **Coverage** workflow;
//! `architecture/testing.md` describes the ratchet.

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

impl Scratch {
    /// Writes lcov named `name`: for each file, each instrumented line and its hits.
    fn lcov(&self, name: &str, files: &[(&str, &[(u32, u32)])]) -> PathBuf {
        let mut records = Vec::new();
        for (file, lines) in files {
            records.push(format!("SF:{file}"));
            records.extend(lines.iter().map(|(line, hits)| format!("DA:{line},{hits}")));
            records.push("end_of_record".to_owned());
        }
        let mut text = records.join("\n");
        text.push('\n');
        let path = self.path(name);
        fs::write(&path, text).expect("writing lcov");
        path
    }

    /// Writes `text` as a diff named `name`.
    fn diff(&self, name: &str, text: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, text).expect("writing a diff");
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

/// Above both floors: 99% of lines, 98% of regions.
const SAME: &[Metric] = &[("lines", 99, 100), ("regions", 49, 50)];
const CONDITIONS: &[Metric] = &[("branches", 18, 20)];

/// A diff adding lines 10 and 11 to `src/a.rs`.
const ADDS_TWO: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                        @@ -9,0 +10,2 @@ fn f() {\n+    one();\n+    two();\n";

/// Every input, with both sides the same and the new lines covered, as a base to vary.
fn unchanged(scratch: &Scratch) -> Vec<(&'static str, PathBuf)> {
    vec![
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
        (
            "head-lcov",
            scratch.lcov("head.lcov", &[("src/a.rs", &[(9, 1), (10, 3), (11, 1)])]),
        ),
        ("diff", scratch.diff("changes.diff", ADDS_TWO)),
    ]
}

/// `sides` with the input named `name` replaced by `path`.
fn with(
    mut sides: Vec<(&'static str, PathBuf)>,
    name: &str,
    path: PathBuf,
) -> Vec<(&'static str, PathBuf)> {
    let slot = sides
        .iter_mut()
        .find(|(n, _)| *n == name)
        .expect("a named input");
    slot.1 = path;
    sides
}

fn run(sides: &[(&'static str, PathBuf)]) -> Output {
    let args: Vec<(&str, &Path)> = sides.iter().map(|(n, p)| (*n, p.as_path())).collect();
    ratchet(&args)
}

/// Asserts the exit code, showing both streams if it differs, and returns standard output.
fn exits(out: &Output, code: i32) -> String {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}{}",
        stdout(out),
        stderr(out)
    );
    stdout(out)
}

#[test]
fn coverage_at_its_floors_passes_and_shows_every_metric() {
    let scratch = Scratch::new();
    let text = exits(&run(&unchanged(&scratch)), 0);
    assert!(text.starts_with("## Coverage\n"), "{text}");
    assert!(
        text.contains("Every gated metric is at or above its floor, overall and on the new code."),
        "{text}"
    );
    assert!(
        text.contains("| lines | 99.00% (99/100) | 99.00% (99/100) | +0.00 | floor 98% |"),
        "{text}"
    );
    assert!(
        text.contains("| regions | 98.00% (49/50) | 98.00% (49/50) | +0.00 | floor 97.5% |"),
        "{text}"
    );
    assert!(
        text.contains("| conditions | 90.00% (18/20) | 90.00% (18/20) | +0.00 | report only |"),
        "{text}"
    );
    assert!(
        text.contains("**New code:** 100.00% (2/2) of the coverable lines this pull request adds"),
        "{text}"
    );
}

#[test]
fn lines_below_their_floor_fail() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "head-stable",
        scratch.summary(
            "head-stable.json",
            &[
                ("src/a.rs", SAME),
                ("src/new.rs", &[("lines", 2, 10), ("regions", 5, 5)]),
            ],
        ),
    );
    let out = run(&sides);
    let text = exits(&out, 1);
    assert!(text.contains("**Below its floor: lines.**"), "{text}");
    assert!(
        text.contains("- `src/new.rs`: 8 more uncovered, now 2/10"),
        "{text}"
    );
    assert!(
        stderr(&out).contains("below its floor: lines"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn regions_below_their_floor_fail_on_their_own() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "head-stable",
        scratch.summary(
            "head-stable.json",
            &[("src/a.rs", &[("lines", 99, 100), ("regions", 48, 50)])],
        ),
    );
    let text = exits(&run(&sides), 1);
    assert!(text.contains("**Below its floor: regions.**"), "{text}");
}

#[test]
fn a_fall_against_main_above_the_floors_passes_and_is_reported() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "base-stable",
        scratch.summary(
            "base-stable.json",
            &[("src/a.rs", &[("lines", 100, 100), ("regions", 50, 50)])],
        ),
    );
    let text = exits(&run(&sides), 0);
    assert!(
        text.contains("| lines | 100.00% (100/100) | 99.00% (99/100) | -1.00 | floor 98% |"),
        "{text}"
    );
    assert!(
        text.contains("**lines**, files with more uncovered than on main (reported, not gated):"),
        "{text}"
    );
    assert!(
        text.contains("- `src/a.rs`: 1 more uncovered, now 99/100"),
        "{text}"
    );
}

#[test]
fn a_drop_in_condition_coverage_is_reported_without_failing() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "head-condition",
        scratch.summary(
            "head-condition.json",
            &[("src/a.rs", &[("branches", 17, 20)])],
        ),
    );
    let text = exits(&run(&sides), 0);
    assert!(
        text.contains("| conditions | 90.00% (18/20) | 85.00% (17/20) | -5.00 | report only |"),
        "{text}"
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
        let out = run(&with(unchanged(&scratch), "head-stable", head));
        let text = exits(&out, 2);
        assert!(
            text.contains("**Not evaluated, so the check fails:**"),
            "{label}: {text}"
        );
        assert!(
            text.contains("lines: the pull request has no figure"),
            "{label}: {text}"
        );
        assert!(
            stderr(&out).contains("coverage-ratchet: not evaluated"),
            "{label}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn no_figure_for_main_is_reported_and_passes_since_main_is_not_gated() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "base-stable",
        scratch.path("absent.json"),
    );
    let text = exits(&run(&sides), 0);
    assert!(
        text.contains("| lines | no figure: no summary at"),
        "{text}"
    );
}

#[test]
fn a_metric_that_counts_nothing_has_no_figure() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "head-stable",
        scratch.summary(
            "head-stable.json",
            &[("src/a.rs", &[("lines", 0, 0), ("regions", 49, 50)])],
        ),
    );
    let text = exits(&run(&sides), 2);
    assert!(
        text.contains("lines: the pull request has no figure (nothing counted for lines)"),
        "{text}"
    );
}

#[test]
fn the_summary_is_appended_to_the_named_file() {
    let scratch = Scratch::new();
    let summary = scratch.path("summary.md");
    fs::write(&summary, "earlier section\n").expect("writing");
    let mut sides = unchanged(&scratch);
    sides.push(("summary", summary.clone()));
    let out = run(&sides);
    let text = exits(&out, 0);
    let written = fs::read_to_string(&summary).expect("reading the summary");
    assert!(
        written.starts_with("earlier section\n## Coverage\n"),
        "{written}"
    );
    assert_eq!(&written["earlier section\n".len()..], text);
}

/// Lcov for `src/a.rs` with lines 10 to 19 instrumented, the first `covered` of them run.
fn ten_new_lines(scratch: &Scratch, covered: u32) -> Vec<(&'static str, PathBuf)> {
    let lines: Vec<(u32, u32)> = (10..20)
        .map(|line| (line, u32::from(line < 10 + covered)))
        .collect();
    let mut diff = String::from(
        "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -9,0 +10,10 @@\n",
    );
    for _ in 10..20 {
        diff += "+    step();\n";
    }
    let sides = with(
        unchanged(scratch),
        "head-lcov",
        scratch.lcov("head.lcov", &[("src/a.rs", &lines)]),
    );
    with(sides, "diff", scratch.diff("changes.diff", &diff))
}

#[test]
fn new_lines_covered_below_their_floor_fail_naming_the_lines() {
    let scratch = Scratch::new();
    // 7 of 10: below 90%, with 3 uncovered, more than the 2 allowed.
    let out = run(&ten_new_lines(&scratch, 7));
    let text = exits(&out, 1);
    assert!(text.contains("**Below its floor: new code.**"), "{text}");
    assert!(
        text.contains("70.00% (7/10) of the coverable lines this pull request adds are covered"),
        "{text}"
    );
    assert!(text.contains("**3 are uncovered, so it fails.**"), "{text}");
    assert!(text.contains("- `src/a.rs`: lines 17-19"), "{text}");
    assert!(
        stderr(&out).contains("below its floor: new code"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn new_lines_below_their_floor_pass_within_the_allowance() {
    let scratch = Scratch::new();
    // 8 of 10: below 90%, but only 2 uncovered.
    let text = exits(&run(&ten_new_lines(&scratch, 8)), 0);
    assert!(
        text.contains("80.00% (8/10) of the coverable lines"),
        "{text}"
    );
    assert!(text.contains("- `src/a.rs`: lines 18-19"), "{text}");
}

#[test]
fn a_one_line_change_left_uncovered_passes() {
    let scratch = Scratch::new();
    let sides = with(
        unchanged(&scratch),
        "head-lcov",
        scratch.lcov("head.lcov", &[("src/a.rs", &[(10, 0), (11, 0)])]),
    );
    let text = exits(&run(&sides), 0);
    assert!(text.contains("0.00% (0/2)"), "{text}");
    assert!(text.contains("- `src/a.rs`: lines 10-11"), "{text}");
}

#[test]
fn a_pull_request_adding_no_coverable_line_passes_saying_so() {
    let scratch = Scratch::new();
    let diff = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n\
                @@ -1,0 +2 @@\n+More words.\n\
                diff --git a/src/gone.rs b/src/gone.rs\n--- a/src/gone.rs\n+++ /dev/null\n\
                @@ -1,2 +0,0 @@\n-fn gone() {}\n-\n\
                diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                @@ -30,0 +31 @@\n+// a comment llvm-cov does not instrument\n";
    let sides = with(
        unchanged(&scratch),
        "diff",
        scratch.diff("changes.diff", diff),
    );
    let text = exits(&run(&sides), 0);
    assert!(
        text.contains(
            "**New code:** this pull request adds no coverable line, so there is nothing to judge."
        ),
        "{text}"
    );
}

#[test]
fn context_lines_move_the_count_without_counting_as_added() {
    let scratch = Scratch::new();
    // A diff with context: line 10 is context, 11 is added, 12 context.
    let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                @@ -10,2 +10,3 @@\n fn f() {\n+    added();\n }\n";
    let sides = with(
        unchanged(&scratch),
        "head-lcov",
        scratch.lcov("head.lcov", &[("src/a.rs", &[(10, 0), (11, 1), (12, 0)])]),
    );
    let sides = with(sides, "diff", scratch.diff("changes.diff", diff));
    let text = exits(&run(&sides), 0);
    assert!(text.contains("100.00% (1/1)"), "{text}");
}

#[test]
fn missing_or_unreadable_new_code_inputs_fail_rather_than_pass() {
    let scratch = Scratch::new();
    let quoted = "diff --git \"a/src/\\303\\251.rs\" \"b/src/\\303\\251.rs\"\n\
                  --- \"a/src/\\303\\251.rs\"\n+++ \"b/src/\\303\\251.rs\"\n@@ -1,0 +2 @@\n+x();\n";
    for (label, name, path, says) in [
        (
            "no lcov",
            "head-lcov",
            scratch.path("absent.lcov"),
            "no lcov at",
        ),
        (
            "empty lcov",
            "head-lcov",
            scratch.lcov("empty.lcov", &[]),
            "records no file",
        ),
        ("no diff", "diff", scratch.path("absent.diff"), "no diff at"),
        (
            "quoted path",
            "diff",
            scratch.diff("quoted.diff", quoted),
            "names a path this cannot read",
        ),
    ] {
        let out = run(&with(unchanged(&scratch), name, path));
        let text = exits(&out, 2);
        assert!(text.contains("- new code: "), "{label}: {text}");
        assert!(text.contains(says), "{label}: {text}");
    }
}

#[test]
fn without_a_second_measurement_the_summary_says_determinism_was_not_checked() {
    let scratch = Scratch::new();
    let out = run(&unchanged(&scratch));
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("**Determinism:** not checked: no second measurement"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_second_measurement_that_matches_passes() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides.push((
        "head-stable-repeat",
        scratch.summary("repeat.json", &[("src/a.rs", SAME)]),
    ));
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
            "**Determinism:** two measurements of the pull request matched in every file."
        ),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_second_measurement_that_differs_fails_naming_the_file_and_both_figures() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides.push((
        "head-stable-repeat",
        scratch.summary(
            "repeat.json",
            &[("src/a.rs", &[("lines", 99, 100), ("regions", 48, 50)])],
        ),
    ));
    let out = run(&sides);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(
        text.contains("**two measurements of the same code differ in 1 file(s)**"),
        "{text}"
    );
    assert!(
        text.contains("- `src/a.rs`: regions 49/50 in one measurement, regions 48/50 in the other"),
        "{text}"
    );
    assert!(
        stderr(&out)
            .contains("not evaluated - determinism: two measurements of the same code differ"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_second_measurement_that_is_missing_fails() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides.push(("head-stable-repeat", scratch.path("absent.json")));
    let out = run(&sides);
    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    assert!(
        stdout(&out)
            .contains("determinism: the second measurement of the pull request has no figure"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_difference_outside_lines_and_regions_is_named_by_the_field_that_differs() {
    let scratch = Scratch::new();
    let mut sides = unchanged(&scratch);
    sides[1].1 = scratch.summary(
        "head-stable.json",
        &[(
            "src/a.rs",
            &[
                ("lines", 99, 100),
                ("regions", 49, 50),
                ("instantiations", 3, 4),
            ],
        )],
    );
    sides.push((
        "head-stable-repeat",
        scratch.summary(
            "repeat.json",
            &[(
                "src/a.rs",
                &[
                    ("lines", 99, 100),
                    ("regions", 49, 50),
                    ("instantiations", 4, 4),
                ],
            )],
        ),
    ));
    let out = run(&sides);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains(
            "- `src/a.rs`: instantiations 3/4 in one measurement, instantiations 4/4 in the other"
        ),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_diff_holding_bytes_that_are_not_utf8_is_read() {
    let scratch = Scratch::new();
    // A Latin-1 file's added line, then the Rust file's: the first must not stop the second.
    let mut diff = b"diff --git a/latin1.txt b/latin1.txt\n--- a/latin1.txt\n+++ b/latin1.txt\n\
                     @@ -0,0 +1 @@\n+caf\xe9\n"
        .to_vec();
    diff.extend_from_slice(ADDS_TWO.as_bytes());
    let path = scratch.path("changes.diff");
    fs::write(&path, diff).expect("writing a diff");
    let text = exits(&run(&with(unchanged(&scratch), "diff", path)), 0);
    assert!(text.contains("100.00% (2/2)"), "{text}");
}
