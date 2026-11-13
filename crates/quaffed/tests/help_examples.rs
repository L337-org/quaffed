// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Every example in `quaff --help`, run against the built `quaff`.
//!
//! The help shows a small project, then examples as `$ quaff ...` lines, each said to exit 0 run
//! from that project's root.  This test reads the help as the binary prints it, builds the project
//! from the files it shows, runs each example in a fresh copy, and fails, naming the example,
//! wherever one does not exit 0 or cannot be run as written.  It also fails where no example uses
//! a form or statement the help must teach by example, so that one cannot quietly be dropped.
//! The help's examples section of `architecture/testing.md` says how the help is read.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

mod shell;

use shell::{run_quaff, split_command};

/// What every form and statement the help must show an example of looks like in a command: each
/// must appear in at least one example.  A form or statement this build gains joins the list.
const MUST_SHOW: &[(&str, &str)] = &[
    ("a bare query", "$ quaff TODO "),
    ("a scope", "quaff TODO tests"),
    ("a scope from the project root", " :/"),
    ("--", "quaff -- "),
    ("-s", " -s "),
    ("-p", " -p "),
    ("-e", " -e "),
    ("-f FILE", " -f checks.quaff"),
    ("-f -", "| quaff -f -"),
    ("--dry-run", " --dry-run "),
    ("-o text", " -o text "),
    ("--version", " --version"),
    ("find of text", "find \""),
    ("find of a pattern", "find `"),
    ("an anchor", "^\""),
    ("a line break", "$^"),
    ("a fenced operand", "\"\"\" "),
    ("a $name... hole", "...)"),
    ("replace string", "replace string \""),
    ("replace of a pattern", "replace `"),
    ("delete", "delete `"),
    ("in", " in \""),
    ("where", " where "),
    ("where not", " where not "),
    ("and", " and "),
    ("matches", " matches "),
    ("contains", " contains "),
    ("FILE", "FILE "),
    ("a test of a metavariable", "where $"),
    ("expect N", "expect 1'"),
    ("expect at least", "expect at least "),
    ("expect at most", "expect at most "),
    ("expect N or none", " or none"),
    ("expect none", "expect none"),
    ("expect any", "expect any"),
    ("a block of assertions", "; find"),
];

/// A file the help shows, as a path line then `  | ` lines.
#[derive(Debug, PartialEq, Eq)]
struct Shown {
    path: String,
    content: String,
}

/// One example: a `  $ ` line of the help.
#[derive(Debug, PartialEq, Eq)]
struct Example {
    /// The line in the help, counting from 1.
    line: usize,
    /// The command as the help writes it, after the `$ `.
    command: String,
}

/// The help, read: the project the examples run in, and the examples.
#[derive(Debug, Default)]
struct Help {
    project: Vec<Shown>,
    examples: Vec<Example>,
}

/// Reads the project and the examples from `help`.
///
/// A file is a line of two spaces and a path - no space in it, and a `.` - followed directly by
/// lines of `  |`, each the file's next line after the `  | ` or the bare `  |` of an empty one.
/// An example is a line starting `  $ `.
fn read_help(help: &str) -> Help {
    let lines: Vec<&str> = help.lines().collect();
    let mut read = Help::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(command) = line.strip_prefix("  $ ") {
            read.examples.push(Example {
                line: i + 1,
                command: command.to_owned(),
            });
        } else if let Some(path) = line.strip_prefix("  ")
            && !path.contains(' ')
            && path.contains('.')
            && lines
                .get(i + 1)
                .is_some_and(|next| content_line(next).is_some())
        {
            let mut content = String::new();
            while let Some(text) = lines.get(i + 1).and_then(|next| content_line(next)) {
                content.push_str(text);
                content.push('\n');
                i += 1;
            }
            read.project.push(Shown {
                path: path.to_owned(),
                content,
            });
        }
        i += 1;
    }
    read
}

/// The file content a `  |` line holds, or `None` if it is not one.
fn content_line(line: &str) -> Option<&str> {
    if line == "  |" {
        Some("")
    } else {
        line.strip_prefix("  | ")
    }
}

/// How an example's command is run: the words after `quaff`, and what goes to standard input.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    args: Vec<String>,
    stdin: Option<String>,
}

/// Reads `command` as a shell would, or says why it cannot be run as written.
///
/// It is `quaff` and its arguments, or `echo` of one word piped into `quaff`, whose standard input
/// is then that word and a newline, as `echo` writes it.
fn read_command(command: &str) -> Result<Run, String> {
    let (stdin, rest) = match command.split_once(" | ") {
        Some((echo, rest)) => {
            let words = words(echo)?;
            match words.as_slice() {
                [first, word] if first == "echo" => (Some(format!("{word}\n")), rest),
                _ => return Err("what is piped into quaff is not echo of one word".into()),
            }
        }
        None => (None, command),
    };
    let mut words = words(rest)?;
    if words.first().map(String::as_str) != Some("quaff") {
        return Err("the command is not quaff".into());
    }
    words.remove(0);
    Ok(Run { args: words, stdin })
}

/// `command` split into words, or why it cannot be.
fn words(command: &str) -> Result<Vec<String>, String> {
    split_command(command)?.ok_or_else(|| "a quote is never closed".to_string())
}

/// A fresh copy of the help's project, removed when dropped.
struct ProjectDir(PathBuf);

impl Drop for ProjectDir {
    fn drop(&mut self) {
        // Not checked: a panic here, during the unwinding of a failed test, would abort the
        // whole run and hide that failure.  A directory left behind is cleared by the next
        // build_project that reuses its name.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Creates a fresh directory holding `project` and an empty `.git`, so that it is a project as
/// the help says.
fn build_project(project: &[Shown]) -> ProjectDir {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "quaffed-help-examples-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    // A directory left by an earlier run whose process ID this one reuses would hold stale
    // files, so anything but "not there" stops the test.
    match fs::remove_dir_all(&dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            panic!("clearing {} before the test: {err}", dir.display())
        }
        _ => {}
    }
    fs::create_dir_all(dir.join(".git")).expect("creating the project");
    for file in project {
        let path = dir.join(&file.path);
        fs::create_dir_all(path.parent().expect("a shown file has a parent"))
            .expect("creating a project directory");
        fs::write(&path, &file.content).expect("writing a project file");
    }
    ProjectDir(dir)
}

/// Everything wrong with the examples in `help`, each naming the example; empty if every one
/// exits 0 and every form and statement in [`MUST_SHOW`] has one.
fn failures(help: &str) -> Vec<String> {
    let read = read_help(help);
    let mut failures = Vec::new();
    if read.project.is_empty() || read.examples.is_empty() {
        failures.push(
            "the help shows no project or no examples, so nothing was checked; the format this \
             test reads is in architecture/testing.md"
                .to_string(),
        );
        return failures;
    }
    for (what, needle) in MUST_SHOW {
        if !read
            .examples
            .iter()
            .any(|e| format!("$ {} ", e.command).contains(needle))
        {
            failures.push(format!("no example shows {what}: none holds {needle:?}"));
        }
    }
    for example in &read.examples {
        let run = match read_command(&example.command) {
            Ok(run) => run,
            Err(why) => {
                failures.push(format!(
                    "help line {}: $ {}\n  {why}, so it cannot be checked",
                    example.line, example.command
                ));
                continue;
            }
        };
        let dir = build_project(&read.project);
        let ran = run_quaff(&dir.0, &run.args, run.stdin.as_deref());
        if ran.exit != Some(0) {
            let exit = ran
                .exit
                .map_or_else(|| "a signal".to_string(), |c| c.to_string());
            let mut report = format!(
                "help line {}: $ {}\n  exited with {exit}, where the help says every example \
                 exits 0; it printed:\n",
                example.line, example.command
            );
            for line in ran.output.lines() {
                let _ = writeln!(report, "    {line}");
            }
            failures.push(report);
        }
    }
    failures
}

/// The help, as the built `quaff` prints it.
fn the_help() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_quaff"))
        .arg("--help")
        .output()
        .expect("running quaff --help");
    assert!(output.status.success(), "quaff --help failed: {output:?}");
    String::from_utf8(output.stdout).expect("the help is UTF-8")
}

#[test]
fn every_example_in_the_help_exits_0_and_every_form_has_one() {
    let failures = failures(&the_help());
    assert!(
        failures.is_empty(),
        "the help's examples fail {} checks; architecture/testing.md says how the help is \
         read:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A help in the format this test reads, with `examples` after a one-file project.
fn tiny_help(examples: &str) -> String {
    format!("quaff\n\n  a.py\n  | x = 1  # TODO\n  |\n  | y = 2\n\n{examples}\n")
}

/// The failures for `tiny_help(examples)` other than forms it does not show.
fn run_failures(examples: &str) -> Vec<String> {
    failures(&tiny_help(examples))
        .into_iter()
        .filter(|f| !f.starts_with("no example shows"))
        .collect()
}

#[test]
fn the_project_is_read_with_its_empty_lines() {
    let read = read_help(&tiny_help("  $ quaff TODO"));
    assert_eq!(
        read.project,
        vec![Shown {
            path: "a.py".into(),
            content: "x = 1  # TODO\n\ny = 2\n".into(),
        }]
    );
    assert_eq!(
        read.examples,
        vec![Example {
            line: 8,
            command: "quaff TODO".into(),
        }]
    );
}

#[test]
fn an_example_that_exits_0_passes() {
    assert_eq!(run_failures("  $ quaff TODO"), Vec::<String>::new());
}

#[test]
fn an_example_that_exits_otherwise_fails_naming_its_line_and_what_it_printed() {
    let failures = run_failures("  $ quaff -e 'find \"TODO\" expect 2'");
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0]
            .starts_with("help line 8: $ quaff -e 'find \"TODO\" expect 2'\n  exited with 2"),
        "{}",
        failures[0]
    );
    assert!(
        failures[0].contains("expected exactly 2 matches of \"TODO\", found 1"),
        "{}",
        failures[0]
    );
}

#[test]
fn standard_input_is_what_echo_writes() {
    assert_eq!(
        read_command("echo 'find \"x\"' | quaff -f -"),
        Ok(Run {
            args: vec!["-f".into(), "-".into()],
            stdin: Some("find \"x\"\n".into()),
        })
    );
    assert_eq!(
        run_failures("  $ echo 'find \"TODO\" expect 1' | quaff -f -"),
        Vec::<String>::new()
    );
}

#[test]
fn an_example_that_cannot_be_run_as_written_fails() {
    for (example, why) in [
        ("  $ cat a.py", "the command is not quaff"),
        ("  $ quaff -p \"f($x)\"", "which a shell would expand"),
        ("  $ quaff 'TODO", "a quote is never closed"),
        ("  $ printf x | quaff -f -", "is not echo of one word"),
    ] {
        let failures = run_failures(example);
        assert_eq!(failures.len(), 1, "{example}: {failures:?}");
        assert!(failures[0].contains(why), "{example}: {}", failures[0]);
        assert!(failures[0].contains("cannot be checked"), "{}", failures[0]);
    }
}

#[test]
fn a_form_no_example_shows_fails() {
    let failures = failures(&tiny_help("  $ quaff TODO"));
    assert!(
        failures.contains(&"no example shows delete: none holds \"delete `\"".to_string()),
        "{failures:?}"
    );
}

#[test]
fn a_help_with_no_project_or_no_examples_fails() {
    for help in ["quaff\n\n  $ quaff TODO\n", "quaff\n\n  a.py\n  | x\n"] {
        let failures = failures(help);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].contains("nothing was checked"),
            "{}",
            failures[0]
        );
    }
}

#[test]
fn each_example_s_project_is_removed_after_it_runs() {
    let dir = build_project(&[Shown {
        path: "src/a.py".into(),
        content: "a = 1\n".into(),
    }]);
    let path = dir.0.clone();
    assert!(path.join("src/a.py").is_file());
    drop(dir);
    assert!(!path.exists(), "{} was left behind", path.display());
}
