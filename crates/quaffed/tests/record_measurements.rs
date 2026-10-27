//! `scripts/record-measurements` turns the budgets' and the benchmarks' measurements into `InfluxDB`
//! points that carry the run's commit, operating system, architecture and image and the time each
//! was measured; posts them, retrying what may pass with time and keeping what does not pass for
//! later, without ever following a redirect or printing a credential; and catches up on what
//! earlier runs kept, deleting each artefact once it is posted.
//!
//! Each test runs the script with `python3`, against a server on the loopback interface standing
//! in for `InfluxDB` and, for catching up, a fake `gh`.  The real posting happens in CI's **Record
//! measurements** workflow after a merge to main; `architecture/performance.md` describes it.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

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
            "quaffed-record-measurements-{}-{}",
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic while a failed test unwinds would abort the run and hide it.
        let _ = fs::remove_dir_all(&self.0);
    }
}

const PASSWORD: &str = "a-password-that-must-never-be-printed";

/// The variables GitHub sets in the job that measured, as a run on an arm64 runner has them.
const CONTEXT: &[(&str, &str)] = &[
    ("GITHUB_SHA", "0123456789abcdef"),
    ("RUNNER_OS", "Linux"),
    ("RUNNER_ARCH", "ARM64"),
    ("ImageOS", "ubuntu24"),
    ("ImageVersion", "20261020.1"),
];

/// The script, run with only the variables given and the ones it always needs.
fn record(args: &[&str], vars: &[(&str, &str)], path: Option<&Path>) -> Output {
    let mut command = Command::new("python3");
    command
        .arg(workspace_root().join("scripts/record-measurements"))
        .args(args)
        .env_clear()
        .env("RECORD_MEASUREMENTS_DELAYS", "0,0");
    let system_path = std::env::var("PATH").unwrap_or_default();
    match path {
        Some(first) => command.env("PATH", format!("{}:{system_path}", first.display())),
        None => command.env("PATH", system_path),
    };
    for (name, value) in vars {
        command.env(name, value);
    }
    command
        .output()
        .expect("running scripts/record-measurements")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const BUDGETS: &str = r#"{"version": 1, "scenarios": {
  "search": {"measured_at": 1793098000, "instructions": 1034000, "peak_memory_kib": 5600, "paths_opened": 17, "bytes_read": 5951},
  "startup": {"measured_at": 1793097900, "instructions": 235000, "peak_memory_kib": 5500, "paths_opened": 1, "bytes_read": 0}}}"#;

#[test]
fn the_budgets_are_one_point_per_scenario_carrying_the_run_and_when_it_was_measured() {
    let scratch = Scratch::new();
    fs::write(scratch.path("budgets.json"), BUDGETS).unwrap();
    let out = scratch.path("budgets.lp");
    let output = record(
        &[
            "budgets",
            path_str(&scratch.path("budgets.json")),
            "--out",
            path_str(&out),
        ],
        CONTEXT,
        None,
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        "quaffed_budget,arch=arm64,os=Linux,runner_image=ubuntu24-20261020.1,scenario=search \
         instructions=1034000i,peak_memory_kib=5600i,paths_opened=17i,bytes_read=5951i,\
         commit=\"0123456789abcdef\" 1793098000\n\
         quaffed_budget,arch=arm64,os=Linux,runner_image=ubuntu24-20261020.1,scenario=startup \
         instructions=235000i,peak_memory_kib=5500i,paths_opened=1i,bytes_read=0i,\
         commit=\"0123456789abcdef\" 1793097900\n"
    );
}

/// The Callgrind counts a benchmark point records, with a value each.
const COUNTS: &[(&str, u64)] = &[
    ("Ir", 363_445),
    ("Dr", 72_789),
    ("Dw", 71_790),
    ("I1mr", 699),
    ("D1mr", 747),
    ("D1mw", 38),
    ("ILmr", 164),
    ("DLmr", 9),
    ("DLmw", 0),
    ("L1hits", 506_540),
    ("LLhits", 1_221),
    ("RamHits", 173),
    ("TotalRW", 508_324),
    ("EstimatedCycles", 519_150),
];

/// Writes one benchmark's summary under `dir`, where gungraun puts it, with the counts given, a
/// rate that is not recorded, and the time its profile started.
fn write_summary(dir: &Path, function: &str, id: &str, counts: &[(&str, u64)], version: &str) {
    let at = dir.join(format!(
        "quaffed-python/front_end/front_end/{function}.{id}"
    ));
    fs::create_dir_all(&at).expect("making a benchmark's directory");
    let mut metrics: Vec<String> = counts
        .iter()
        .map(|(name, value)| format!(r#""{name}": {{"values": {{"new": {value}}}}}"#))
        .collect();
    metrics.push(r#""L1HitRate": {"values": {"new": 99.7}}"#.to_owned());
    let json = format!(
        r#"{{"version": "{version}", "function_name": "{function}", "id": "{id}",
            "profiles": [{{"tool": "Callgrind", "started_at": "2026-10-27T10:48:31.523938378Z",
            "data": {{"total": {{"metrics": {{{}}}}}}}}}]}}"#,
        metrics.join(", ")
    );
    fs::write(at.join("summary.json"), json).expect("writing a summary");
}

#[test]
fn each_benchmark_is_one_point_with_every_callgrind_count_and_no_rate() {
    let scratch = Scratch::new();
    let counts = scratch.path("counts");
    write_summary(&counts, "matching", "a_name_used_twice", COUNTS, "7");
    write_summary(&counts, "ruff_parse", "synthetic", COUNTS, "7");
    let out = scratch.path("benchmarks.lp");
    let output = record(
        &["benchmarks", path_str(&counts), "--out", path_str(&out)],
        CONTEXT,
        None,
    );
    assert!(output.status.success(), "{}", text(&output));
    let points = fs::read_to_string(&out).unwrap();
    let fields = "Ir=363445i,Dr=72789i,Dw=71790i,I1mr=699i,D1mr=747i,D1mw=38i,ILmr=164i,DLmr=9i,\
                  DLmw=0i,L1hits=506540i,LLhits=1221i,RamHits=173i,TotalRW=508324i,\
                  EstimatedCycles=519150i";
    assert_eq!(
        points,
        format!(
            "quaffed_benchmark,arch=arm64,benchmark=matching.a_name_used_twice,function=matching,\
             os=Linux,runner_image=ubuntu24-20261020.1 {fields},commit=\"0123456789abcdef\" \
             1793098111\n\
             quaffed_benchmark,arch=arm64,benchmark=ruff_parse.synthetic,function=ruff_parse,\
             os=Linux,runner_image=ubuntu24-20261020.1 {fields},commit=\"0123456789abcdef\" \
             1793098111\n"
        )
    );
}

#[test]
fn without_the_runs_context_nothing_is_written_naming_what_is_missing() {
    let scratch = Scratch::new();
    fs::write(scratch.path("budgets.json"), BUDGETS).unwrap();
    let out = scratch.path("budgets.lp");
    let context: Vec<_> = CONTEXT
        .iter()
        .filter(|(n, _)| *n != "ImageVersion")
        .copied()
        .collect();
    let output = record(
        &[
            "budgets",
            path_str(&scratch.path("budgets.json")),
            "--out",
            path_str(&out),
        ],
        &context,
        None,
    );
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("ImageVersion is not set"),
        "{}",
        text(&output)
    );
    assert!(!out.exists());
}

#[test]
fn an_architecture_it_does_not_name_is_refused() {
    let scratch = Scratch::new();
    fs::write(scratch.path("budgets.json"), BUDGETS).unwrap();
    let mut context = CONTEXT.to_vec();
    context.retain(|(n, _)| *n != "RUNNER_ARCH");
    context.push(("RUNNER_ARCH", "ARM"));
    let output = record(
        &[
            "budgets",
            path_str(&scratch.path("budgets.json")),
            "--out",
            path_str(&scratch.path("x")),
        ],
        &context,
        None,
    );
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("RUNNER_ARCH 'ARM'"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_summary_without_a_count_or_in_another_format_or_none_at_all_is_refused() {
    let scratch = Scratch::new();
    let missing = scratch.path("missing");
    write_summary(&missing, "matching", "a", &COUNTS[1..], "7");
    let other = scratch.path("other");
    write_summary(&other, "matching", "a", COUNTS, "8");
    let none = scratch.path("none");
    fs::create_dir_all(&none).unwrap();
    for (dir, said) in [
        (&missing, "has no Ir"),
        (&other, "summary version '8'"),
        (&none, "no summary.json"),
    ] {
        let output = record(
            &[
                "benchmarks",
                path_str(dir),
                "--out",
                path_str(&scratch.path("x.lp")),
            ],
            CONTEXT,
            None,
        );
        assert_eq!(output.status.code(), Some(2), "{}", text(&output));
        assert!(
            text(&output).contains(said),
            "expected {said:?} in {}",
            text(&output)
        );
    }
}

/// One request as the stand-in server received it.
#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

/// A server on the loopback interface answering each request with the next of `answers`, a
/// status and any extra header, and keeping what it received.
fn server(answers: &[(u16, &str)]) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binding the stand-in server");
    let url = format!("http://{}/write-here", listener.local_addr().unwrap());
    let received = Arc::new(Mutex::new(Vec::new()));
    let answers: Vec<(u16, String)> = answers.iter().map(|(s, h)| (*s, (*h).to_owned())).collect();
    let kept = Arc::clone(&received);
    thread::spawn(move || {
        for (status, header) in answers {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
            let (mut length, mut authorization) = (0, None);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                let (name, value) = line.split_once(": ").unwrap_or((line, ""));
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.parse().unwrap(),
                    "authorization" => authorization = Some(value.to_owned()),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            kept.lock().unwrap().push(Request {
                method,
                path,
                authorization,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
            let reply = if status == 204 {
                ""
            } else {
                r#"{"error":"stand-in"}"#
            };
            let response = format!(
                "HTTP/1.1 {status} Stand-in\r\n{header}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let mut stream = reader.into_inner();
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (url, received)
}

/// The endpoint's variables for a stand-in server at `url`.
fn endpoint(url: &str) -> Vec<(&'static str, String)> {
    vec![
        ("QUAFFED_INFLUX_URL", url.to_owned()),
        ("QUAFFED_INFLUX_USER", "github_ci".to_owned()),
        ("QUAFFED_INFLUX_PASSWORD", PASSWORD.to_owned()),
        ("RECORD_MEASUREMENTS_ALLOW_PLAINTEXT_HTTP", "1".to_owned()),
    ]
}

fn borrowed<'a>(vars: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    vars.iter().map(|(n, v)| (*n, v.as_str())).collect()
}

const POINTS: &str = "quaffed_budget,scenario=search instructions=1i 1793098000\n";

/// Posts one file of points to a stand-in server, giving the output and the kept directories.
fn post_to(answers: &[(u16, &str)]) -> (Output, Arc<Mutex<Vec<Request>>>, Scratch) {
    let scratch = Scratch::new();
    fs::write(scratch.path("points.lp"), POINTS).unwrap();
    let (url, received) = server(answers);
    let vars = endpoint(&url);
    let output = record(
        &[
            "post",
            path_str(&scratch.path("points.lp")),
            "--pending",
            path_str(&scratch.path("pending")),
            "--rejected",
            path_str(&scratch.path("rejected")),
        ],
        &borrowed(&vars),
        None,
    );
    assert!(
        !text(&output).contains(PASSWORD),
        "the password was printed: {}",
        text(&output)
    );
    (output, received, scratch)
}

#[test]
fn a_post_sends_the_points_with_the_credentials_to_the_endpoint() {
    let (output, received, scratch) = post_to(&[(204, "")]);
    assert!(output.status.success(), "{}", text(&output));
    let received = received.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "POST");
    assert_eq!(received[0].path, "/write-here");
    assert_eq!(received[0].body, POINTS);
    // github_ci:<password>, as HTTP basic authentication spells it.
    let expected = format!("github_ci:{PASSWORD}");
    let decoded = received[0]
        .authorization
        .as_deref()
        .and_then(|a| a.strip_prefix("Basic "))
        .map(decode_base64)
        .unwrap_or_default();
    assert_eq!(decoded, expected);
    assert!(!scratch.path("pending").exists() && !scratch.path("rejected").exists());
}

#[test]
fn a_failure_that_may_pass_is_retried_until_it_does() {
    let (output, received, scratch) = post_to(&[(500, ""), (503, ""), (204, "")]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(received.lock().unwrap().len(), 3);
    assert!(!scratch.path("pending").exists());
}

#[test]
fn points_that_could_not_be_posted_are_kept_for_later_and_the_run_fails() {
    let (output, received, scratch) = post_to(&[(503, ""), (503, ""), (503, "")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(received.lock().unwrap().len(), 3, "every attempt was made");
    assert_eq!(
        fs::read_to_string(scratch.path("pending/points.lp")).unwrap(),
        POINTS
    );
    assert!(
        text(&output).contains("InfluxDB answered 503"),
        "{}",
        text(&output)
    );
}

#[test]
fn points_influxdb_refuses_as_malformed_are_kept_apart_without_retrying() {
    let (output, received, scratch) = post_to(&[(400, "")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(
        received.lock().unwrap().len(),
        1,
        "a malformed point is not retried"
    );
    assert_eq!(
        fs::read_to_string(scratch.path("rejected/points.lp")).unwrap(),
        POINTS
    );
    assert!(!scratch.path("pending").exists());
}

#[test]
fn refused_credentials_keep_the_points_for_later_without_retrying() {
    let (output, received, scratch) = post_to(&[(401, "")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(received.lock().unwrap().len(), 1);
    assert!(scratch.path("pending/points.lp").exists());
    assert!(
        text(&output).contains("check the endpoint's secrets"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_redirect_is_not_followed_so_the_credentials_go_nowhere_else() {
    let (elsewhere, reached) = server(&[(204, "")]);
    let location = format!("Location: {elsewhere}\r\n");
    let (output, _, scratch) = post_to(&[(302, location.as_str())]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        reached.lock().unwrap().is_empty(),
        "the redirect was followed"
    );
    assert!(scratch.path("pending/points.lp").exists());
}

#[test]
fn an_endpoint_that_is_not_https_is_refused() {
    let scratch = Scratch::new();
    fs::write(scratch.path("points.lp"), POINTS).unwrap();
    let mut vars = endpoint("http://127.0.0.1:9/write");
    vars.retain(|(n, _)| *n != "RECORD_MEASUREMENTS_ALLOW_PLAINTEXT_HTTP");
    let output = record(
        &[
            "post",
            path_str(&scratch.path("points.lp")),
            "--pending",
            "p",
            "--rejected",
            "r",
        ],
        &borrowed(&vars),
        None,
    );
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("not an https:// URL"),
        "{}",
        text(&output)
    );
}

/// A fake `gh` in its own directory, listing `listed` as the kept artefacts and downloading
/// `files` from each, logging every call in `calls` beside it.  `gone` makes a download find the
/// artefact deleted.
fn fake_gh(scratch: &Scratch, listed: &str, files: &[(&str, &str)], gone: bool) -> PathBuf {
    let bin = scratch.path("bin");
    let artefact = bin.join("artefact");
    fs::create_dir_all(&artefact).unwrap();
    for (name, content) in files {
        fs::write(artefact.join(name), content).unwrap();
    }
    fs::write(bin.join("listing"), listed).unwrap();
    if gone {
        fs::write(bin.join("gone"), "").unwrap();
    }
    let script = r#"#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
echo "$*" >> "$here/calls"
if [ "$1" = api ] && [ "$2" = --method ]; then exit 0; fi
if [ "$1" = api ]; then cat "$here/listing"; exit 0; fi
if [ "$1" = run ]; then
  if [ -f "$here/gone" ]; then echo "no valid artifacts found to download" >&2; exit 1; fi
  while [ "$#" -gt 0 ]; do [ "$1" = --dir ] && dir="$2"; shift; done
  cp "$here"/artefact/* "$dir"/
fi
"#;
    let gh = bin.join("gh");
    fs::write(&gh, script).unwrap();
    make_executable(&gh);
    bin
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn catch_up(scratch: &Scratch, bin: &Path, url: &str) -> Output {
    let mut vars = endpoint(url);
    vars.push(("GITHUB_REPOSITORY", "L337-org/quaffed".to_owned()));
    vars.push(("HOME", scratch.0.display().to_string()));
    let output = record(
        &[
            "catch-up",
            "--rejected",
            path_str(&scratch.path("rejected")),
        ],
        &borrowed(&vars),
        Some(bin),
    );
    assert!(
        !text(&output).contains(PASSWORD),
        "the password was printed: {}",
        text(&output)
    );
    output
}

fn calls(bin: &Path) -> String {
    fs::read_to_string(bin.join("calls")).unwrap_or_default()
}

#[test]
fn a_kept_artefact_is_posted_and_then_deleted() {
    let scratch = Scratch::new();
    let bin = fake_gh(&scratch, "77\t1234\n", &[("budgets.lp", POINTS)], false);
    let (url, received) = server(&[(204, "")]);
    let output = catch_up(&scratch, &bin, &url);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(received.lock().unwrap()[0].body, POINTS);
    let calls = calls(&bin);
    assert!(calls.contains("run download 1234"), "{calls}");
    assert!(
        calls.contains("api --method DELETE repos/L337-org/quaffed/actions/artifacts/77"),
        "{calls}"
    );
}

#[test]
fn a_kept_artefact_that_still_cannot_be_posted_stays_and_the_run_fails() {
    let scratch = Scratch::new();
    let bin = fake_gh(&scratch, "77\t1234\n", &[("budgets.lp", POINTS)], false);
    let (url, _) = server(&[(503, ""), (503, ""), (503, "")]);
    let output = catch_up(&scratch, &bin, &url);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(!calls(&bin).contains("DELETE"), "{}", calls(&bin));
}

#[test]
fn with_nothing_kept_catching_up_downloads_and_posts_nothing() {
    let scratch = Scratch::new();
    let bin = fake_gh(&scratch, "", &[], false);
    let (url, received) = server(&[]);
    let output = catch_up(&scratch, &bin, &url);
    assert!(output.status.success(), "{}", text(&output));
    assert!(received.lock().unwrap().is_empty());
    assert_eq!(
        calls(&bin).lines().count(),
        1,
        "only the listing: {}",
        calls(&bin)
    );
}

#[test]
fn an_artefact_another_run_already_posted_and_deleted_is_passed_over() {
    let scratch = Scratch::new();
    let bin = fake_gh(&scratch, "77\t1234\n", &[], true);
    let (url, received) = server(&[]);
    let output = catch_up(&scratch, &bin, &url);
    assert!(output.status.success(), "{}", text(&output));
    assert!(received.lock().unwrap().is_empty());
    assert!(text(&output).contains("already gone"), "{}", text(&output));
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("a scratch path is UTF-8")
}

/// Standard base64, decoded, for reading back the credentials a request carried.
fn decode_base64(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for byte in text.bytes().filter(|b| *b != b'=') {
        let value = ALPHABET
            .iter()
            .position(|c| *c == byte)
            .expect("a base64 character");
        bits = (bits << 6) | u32::try_from(value).unwrap();
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 0xff).unwrap());
        }
    }
    String::from_utf8(out).expect("the credentials are UTF-8")
}
