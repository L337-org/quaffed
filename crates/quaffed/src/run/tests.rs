// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The run engine's tests from end to end: whole runs, through every phase.

use super::*;
use super::{evaluate::*, execute::*, failure::*, report::*};
use std::fs;
use std::io::{self, Write};

use quaffed_syntax::FrontEnd;

use crate::cli::{self, SourceArg};

/// A standard output whose reader has gone.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_closed_pipe_ends_the_run_quietly() {
    assert!(!print(&mut Closed, "x").unwrap());
}

#[test]
fn with_the_reader_gone_assertions_still_decide_the_outcome() {
    let dir = std::env::temp_dir().join(format!("quaffed-closed-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("creating the directory");
    fs::write(dir.join("a.txt"), "TODO\nTODO\n").expect("writing");
    let outcome = |script: &str| {
        let sources = vec![SourceArg::Expression {
            number: 1,
            text: script.to_owned(),
        }];
        run(
            sources,
            None,
            &cli::Options::default(),
            &dir,
            &mut io::empty(),
            &mut Closed,
            &mut io::sink(),
        )
    };
    let failed = outcome("find \"TODO\" expect 1");
    let held = outcome("find \"TODO\" expect 2");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(failed.expect("running"), Outcome::AssertionFailed);
    assert_eq!(held.expect("running"), Outcome::Success);
}

#[test]
fn every_file_is_read_from_disk_once_whatever_reads_it() {
    // Each rules file is in the scope as well, so it is both read for its rules - by
    // discovery or a declaration reader - and searched for the query.
    let dir = std::env::temp_dir().join(format!("quaffed-run-once-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for (path, text) in [
        (".git/info/attributes", "*.md text\n"),
        (".gitignore", "*.log\n"),
        (".ignore", "*.tmp\n"),
        (".gitattributes", "*.c text\n"),
        (".editorconfig", "root = true\n"),
        ("sub/.gitignore", "*.bak\n"),
        ("sub/.editorconfig", "[*]\n"),
        ("sub/a.txt", "TODO\n"),
        ("b.py", "TODO = 1\n"),
    ] {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let dir = fs::canonicalize(&dir).unwrap();
    crate::contents::DISK_READS.with(|reads| reads.borrow_mut().clear());
    let outcome = run(
        vec![SourceArg::Query(cli::Query {
            text: "TODO".into(),
            source: cli::Source::Positional,
        })],
        None,
        &cli::Options::default(),
        &dir,
        &mut io::empty(),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    let reads = crate::contents::DISK_READS.with(|reads| reads.borrow().clone());
    let _ = fs::remove_dir_all(&dir);
    assert!(outcome.is_ok(), "{outcome:?}");
    let twice: Vec<_> = reads
        .iter()
        .filter(|path| reads.iter().filter(|read| read == path).count() > 1)
        .collect();
    assert!(twice.is_empty(), "read more than once: {twice:?}");
    for rules in [
        ".gitignore",
        ".ignore",
        ".gitattributes",
        ".editorconfig",
        "sub/.gitignore",
    ] {
        assert!(
            reads.contains(&dir.join(rules)),
            "{rules} never read: {reads:?}"
        );
    }
}

#[test]
fn a_git_pointer_file_is_read_from_disk_once() {
    // A linked worktree's .git is a file naming its own git directory, whose commondir names
    // the main one, where info/ is; both are read for the repository's info/exclude and
    // again for its info/attributes.
    let dir = std::env::temp_dir().join(format!("quaffed-run-pointer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let gitdir = dir.join("gitdir");
    let project = dir.join("project");
    for (path, text) in [
        ("gitdir/info/attributes", "*.md text\n"),
        ("gitdir/info/exclude", "*.tmp\n"),
        ("gitdir/worktrees/project/commondir", "../..\n"),
        ("project/a.txt", "TODO\n"),
    ] {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let worktree = gitdir.join("worktrees/project");
    fs::write(
        project.join(".git"),
        format!("gitdir: {}\n", worktree.display()),
    )
    .unwrap();
    let project = fs::canonicalize(&project).unwrap();
    crate::contents::DISK_READS.with(|reads| reads.borrow_mut().clear());
    let outcome = run(
        vec![SourceArg::Query(cli::Query {
            text: "TODO".into(),
            source: cli::Source::Positional,
        })],
        None,
        &cli::Options::default(),
        &project,
        &mut io::empty(),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    let reads = crate::contents::DISK_READS.with(|reads| reads.borrow().clone());
    let _ = fs::remove_dir_all(&dir);
    assert!(outcome.is_ok(), "{outcome:?}");
    for pointer in [project.join(".git"), worktree.join("commondir")] {
        let count = reads.iter().filter(|read| **read == pointer).count();
        assert_eq!(
            count,
            1,
            "{} read {count} times: {reads:?}",
            pointer.display()
        );
    }
}

#[test]
fn a_search_holds_the_code_its_matches_cover_not_the_file_they_are_in() {
    let mut text = "a = 1\n".repeat(5000);
    text.push_str("TODO = 2\nTODO = 33\n");
    let (held, printed) = held_and_printed_in(&text, "find `TODO = $v` expect 2");
    assert!(printed > 0);
    // The two matches, each a fixed size, and their code copied: none of the rest.
    let code = "TODO = 2".len() + "TODO = 33".len();
    assert_eq!(held, 2 * std::mem::size_of::<Hit>() + code);
    // Matches covering more than the file hold the file instead, never more than it.
    let (held, printed) = held_and_printed(50, "find `x = $v\n$mid...\nend()` expect any");
    let block: usize = (0..50).map(|i| format!("x = {i}\n").len()).sum::<usize>() + 6;
    assert!(printed > block);
    assert_eq!(held, 49 * std::mem::size_of::<Hit>() + block);
}

#[test]
fn an_edit_holds_what_it_wrote_not_a_copy_of_the_file() {
    let dir = std::env::temp_dir().join(format!("quaffed-run-held-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".git")).unwrap();
    let mut text = "a = 1\n".repeat(5000);
    text.push_str("TODO = 2\n");
    fs::write(dir.join("big.py"), &text).unwrap();
    let dir = fs::canonicalize(&dir).unwrap();
    let held = |script: &str| {
        let outcome = run(
            vec![SourceArg::Expression {
                number: 1,
                text: script.to_owned(),
            }],
            None,
            &cli::Options {
                dry_run: true,
                ..cli::Options::default()
            },
            &dir,
            &mut io::empty(),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert!(outcome.is_ok(), "{script}: {outcome:?}");
        CHANGES_HELD.with(std::cell::Cell::get)
    };
    let textual = held(r#"replace string "TODO" with "DONE" expect 1"#);
    let structural = held("replace `TODO = $v` with `DONE = $v` expect 1");
    let both = held(
        "replace string \"TODO\" with \"DONE\" expect 1\nreplace `DONE = $v` with `DONE = ($v)` expect 1",
    );
    let deleted = held("delete `TODO = $v` expect 1");
    // A match nested in another's hole is rewritten inside the outer one's stretch.
    fs::write(dir.join("big.py"), text.clone() + "f(f(1))\n").unwrap();
    let nested = held("replace `f($x)` with `g($x)` expect 2");
    let _ = fs::remove_dir_all(&dir);
    // Each edit holds the bytes it wrote - "DONE", then the match rewritten whole - and none
    // of the thousands it left as they were.
    assert_eq!(textual, "DONE".len());
    assert_eq!(structural, "DONE = 2".len());
    assert_eq!(both, "DONE".len() + "DONE = (2)".len());
    // A deletion writes nothing over its line, and holds nothing.
    assert_eq!(deleted, 0);
    assert_eq!(nested, "g(g(1))".len());
}

/// What a run on a block of `n` statements, each followed by every later one to the block's
/// end, holds until it prints, and how much it prints: the matches overlap, so what they
/// cover is about the square of the block's length.
fn held_and_printed(n: usize, script: &str) -> (usize, usize) {
    let mut block = String::new();
    for i in 0..n {
        std::fmt::Write::write_fmt(&mut block, format_args!("x = {i}\n"))
            .expect("a String takes any text");
    }
    block.push_str("end()\n");
    held_and_printed_in(&block, script)
}

/// What a run of `script` over a project holding one Python file, `text`, holds until it
/// prints, by [`Searched::held_for_printing`], and how many bytes it prints.
fn held_and_printed_in(text: &str, script: &str) -> (usize, usize) {
    // One project per call: tests run on several threads of one process at once.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quaffed-run-block-{}-{call}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::write(dir.join("block.py"), text).unwrap();
    let dir = fs::canonicalize(&dir).unwrap();
    let mut out = Vec::new();
    let outcome = run(
        vec![SourceArg::Expression {
            number: 1,
            text: script.to_owned(),
        }],
        None,
        &cli::Options {
            dry_run: true,
            ..cli::Options::default()
        },
        &dir,
        &mut io::empty(),
        &mut out,
        &mut Vec::new(),
    );
    let _ = fs::remove_dir_all(&dir);
    assert!(outcome.is_ok(), "{script}: {outcome:?}");
    (HELD.with(std::cell::Cell::get), out.len())
}

/// The matches a run holds, each a fixed size, and the files it holds grow with the code
/// while what it prints grows with the square.  What this cannot see is memory a match owns,
/// which `Hit` being `Copy` rules out where it is declared.
#[test]
fn the_matches_and_files_a_run_holds_grow_with_the_code_not_with_what_it_prints() {
    let block = |end: &str| format!("find `x = $v\n$mid...\n{end}` expect any");
    for script in [
        block("end()"),
        format!(
            "{}\nreplace `end()` with `finish()` expect 1",
            block("end()")
        ),
        format!(
            "replace `end()` with `finish()` expect 1\n{}",
            block("finish()")
        ),
        format!("{}\ndelete `end()` expect 1", block("end()")),
        format!(
            "replace string \"end()\" with \"done()\" expect 1\n{}\n\
                 replace `done()` with `end()` expect 1\n{}",
            block("done()"),
            block("end()")
        ),
    ] {
        let (held, printed) = held_and_printed(100, &script);
        let (held_twice, printed_twice) = held_and_printed(200, &script);
        // Twice the code prints about four times as much, and holds about twice as much.
        assert!(
            printed_twice > 3 * printed,
            "{script}: printed {printed} then {printed_twice}"
        );
        assert!(
            held_twice < 3 * held,
            "{script}: held {held} then {held_twice}"
        );
        assert!(held < printed, "{script}: held {held}, printed {printed}");
    }
}

#[test]
fn a_script_in_the_scope_is_read_from_disk_once() {
    // `quaff -f scripts/fix.quaff` from the project's root: the script is read to run it,
    // and is in the scope, so searched as well.
    let dir = std::env::temp_dir().join(format!("quaffed-run-script-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("scripts/fix.quaff"), "find \"TODO\"\n").unwrap();
    fs::write(dir.join("a.txt"), "TODO\n").unwrap();
    let dir = fs::canonicalize(&dir).unwrap();
    let script = dir.join("scripts/fix.quaff");
    crate::contents::DISK_READS.with(|reads| reads.borrow_mut().clear());
    let mut out = Vec::new();
    let outcome = run(
        vec![SourceArg::File(script.clone().into_os_string())],
        None,
        &cli::Options::default(),
        &dir,
        &mut io::empty(),
        &mut out,
        &mut Vec::new(),
    );
    let reads = crate::contents::DISK_READS.with(|reads| reads.borrow().clone());
    let _ = fs::remove_dir_all(&dir);
    assert!(outcome.is_ok(), "{outcome:?}");
    // The script holds "TODO" too, so it was searched as well as run.
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("fix.quaff"), "{out}");
    let count = reads.iter().filter(|read| **read == script).count();
    assert_eq!(count, 1, "the script read {count} times: {reads:?}");
}

/// What a run over two languages, Python and the test language Toy, gave: its outcome, what
/// it printed, its notes, and each file's text afterwards.
struct TwoLanguages {
    outcome: Result<Outcome, Failure>,
    out: String,
    notes: String,
    after: Vec<String>,
}

/// Runs `script` over `files`, each a path and its text, in a fresh directory, as a run that
/// parses Python and Toy.
fn in_two_languages(name: &str, script: &str, files: &[(&str, &str)]) -> TwoLanguages {
    use crate::languages::{PYTHON, TOY};
    in_languages(&[PYTHON, TOY], name, script, files)
}

/// Runs `script` over `files`, each a path and its text, in a fresh directory, as a run that
/// parses `languages`.
fn in_languages(
    languages: &[&'static dyn FrontEnd],
    name: &str,
    script: &str,
    files: &[(&str, &str)],
) -> TwoLanguages {
    let dir = std::env::temp_dir().join(format!(
        "quaffed-two-languages-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let dir = fs::canonicalize(&dir).unwrap();
    let (mut out, mut notes) = (Vec::new(), Vec::new());
    let outcome = run_in(
        languages,
        vec![SourceArg::Expression {
            number: 1,
            text: script.to_owned(),
        }],
        None,
        &cli::Options::default(),
        &dir,
        &mut io::empty(),
        &mut out,
        &mut notes,
    );
    let after = files
        .iter()
        .map(|(path, _)| fs::read_to_string(dir.join(path)).unwrap())
        .collect();
    let _ = fs::remove_dir_all(&dir);
    TwoLanguages {
        outcome,
        out: String::from_utf8(out).unwrap(),
        notes: String::from_utf8(notes).unwrap(),
        after,
    }
}

#[test]
fn a_binary_file_is_never_searched_whatever_its_language() {
    let ran = in_two_languages(
        "binary",
        "find `f($x)`",
        &[
            ("a.py", "f(1)\n"),
            ("b.toy", "f(\0)\n"),
            ("c.bin", "f(\0)\n"),
        ],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert!(
        !ran.out.contains("b.toy") && !ran.out.contains("c.bin"),
        "{}",
        ran.out
    );
}

#[test]
fn a_rewrite_both_languages_read_edits_each_language_s_files() {
    let ran = in_two_languages(
        "rewrite-both",
        "replace `f($x)` with `g($x)` expect 2",
        &[("a.py", "f(1)\n"), ("b.toy", "f(2)\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert_eq!(ran.after, ["g(1)\n", "g(2)\n"]);
}

#[test]
fn a_file_its_language_cannot_decode_or_parse_refuses_a_replace() {
    // Toy decodes as Python does, so an encoding Python does not know is refused.
    for (name, script, text) in [
        (
            "undecoded",
            "replace `f($x)` with `g($x)` expect 1",
            "# coding: nonsense\nf(1)\n",
        ),
        (
            "unwritten-unparsed",
            "replace `f($x)` with `lambda: $x` expect 1",
            "f(1\n",
        ),
    ] {
        let ran = in_two_languages(name, script, &[("a.py", "f(1)\n"), ("b.toy", text)]);
        assert!(
            matches!(&ran.outcome, Err(Failure::Unparseable { files }) if files.len() == 1),
            "{name}: {:?}",
            ran.outcome
        );
        assert_eq!(ran.after[0], "f(1)\n", "{name}: nothing is written");
    }
}

#[test]
fn a_pattern_both_languages_read_is_matched_in_each_language_s_files() {
    let ran = in_two_languages(
        "both",
        "find `f($x)`",
        &[("a.py", "f(1)\n"), ("b.toy", "f(2)\n"), ("c.txt", "f(3)\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert!(
        ran.out.contains("a.py") && ran.out.contains("b.toy"),
        "{}",
        ran.out
    );
    assert!(!ran.out.contains("c.txt"), "{}", ran.out);
}

#[test]
fn a_file_whose_language_cannot_read_the_pattern_is_not_applicable_and_counted() {
    let ran = in_two_languages(
        "one-reads",
        "find `g = lambda: f($x)`",
        &[
            ("a.py", "g = lambda: f(1)\n"),
            ("b.toy", "g = lambda: f(2)\n"),
        ],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert!(ran.out.contains("a.py"), "{}", ran.out);
    assert!(!ran.out.contains("b.toy"), "{}", ran.out);
    assert!(
        ran.notes
            .contains("1 file not searched for a structural pattern its language cannot read"),
        "{}",
        ran.notes
    );
    let ran = in_two_languages(
        "one-reads-of-two",
        "find `g = lambda: f($x)`",
        &[
            ("a.py", "g = lambda: f(1)\n"),
            ("b.toy", "g = lambda: f(2)\n"),
            ("c.toy", "g = lambda: f(3)\n"),
        ],
    );
    assert!(
        ran.notes
            .contains("2 files not searched for structural patterns their language cannot read"),
        "{}",
        ran.notes
    );
}

#[test]
fn a_pattern_no_file_s_language_reads_says_so_rather_than_nothing_found() {
    let ran = in_two_languages(
        "unread-only",
        "find `g = lambda: f($x)`",
        &[("b.toy", "g = lambda: f(2)\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::NothingFound)),
        "{:?}",
        ran.outcome
    );
    assert!(
        ran.notes
            .contains("no file in scope is in a language that reads it; 1 file not searched"),
        "{}",
        ran.notes
    );
    let ran = in_two_languages(
        "unread-and-text",
        "find `g = lambda: f($x)`",
        &[("b.toy", "g = lambda: f(2)\n"), ("c.txt", "g\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::NothingFound)),
        "{:?}",
        ran.outcome
    );
    assert!(
        ran.notes.contains(
            "no file in scope is in a language that reads it; 1 file in another language \
                 and 1 text-tier file not searched"
        ),
        "{}",
        ran.notes
    );
}

#[test]
fn a_pattern_no_language_reads_is_refused_with_what_each_said() {
    let ran = in_two_languages("none-reads", "find `lambda f(`", &[("a.py", "x\n")]);
    let Err(Failure::Script { message, .. }) = &ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert!(message.starts_with("as Python: "), "{message}");
    assert!(
        message.contains("; as Toy: `lambda` is not Toy"),
        "{message}"
    );
}

#[test]
fn a_rewrite_one_language_cannot_write_refuses_the_run_where_it_matches_there() {
    let files = [("a.py", "f(1)\n"), ("b.toy", "f(2)\n")];
    let ran = in_two_languages(
        "unwritten",
        "replace `f($x)` with `lambda: $x` expect 2",
        &files,
    );
    let Err(Failure::EditRefused { files: refused, .. }) = &ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].0.ends_with("b.toy"), "{refused:?}");
    assert!(
        refused[0].1.contains(
            "line 1, column 1: the rewrite cannot be written in Toy: `lambda` is not Toy"
        ),
        "{refused:?}"
    );
    assert_eq!(ran.after, ["f(1)\n", "f(2)\n"], "nothing is written");
    // Where no file of the language that cannot write it matches, the edit is made.
    let ran = in_two_languages(
        "unwritten-unmatched",
        "replace `f($x)` with `lambda: $x` expect 1",
        &[("a.py", "f(1)\n"), ("b.toy", "g(2)\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert_eq!(ran.after, ["lambda: 1\n", "g(2)\n"]);
}

#[test]
fn a_rewrite_no_language_that_reads_its_pattern_can_write_is_refused() {
    let ran = in_two_languages(
        "rewrite-none",
        "replace `f($x)` with `lambda $x:` expect 1",
        &[("a.py", "f(1)\n")],
    );
    let Err(Failure::Script { message, .. }) = &ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert!(message.starts_with("as Python: "), "{message}");
    assert!(message.contains("; as Toy: "), "{message}");
    assert_eq!(ran.after, ["f(1)\n"]);
}

#[test]
fn a_where_one_language_cannot_read_leaves_that_language_s_files_not_applicable() {
    let ran = in_two_languages(
        "where",
        "find `g = $v` where $v matches `lambda: $b`",
        &[("a.py", "g = lambda: 1\n"), ("b.toy", "g = lambda: 2\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert!(ran.out.contains("a.py"), "{}", ran.out);
    assert!(!ran.out.contains("b.toy"), "{}", ran.out);
    // Nor is a `where` compiled for a language that does not read the statement's pattern.
    let ran = in_two_languages(
        "where-unread-pattern",
        "find `g = lambda: $b` where $b matches `1`",
        &[("a.py", "g = lambda: 1\n"), ("b.toy", "g = lambda: 1\n")],
    );
    assert!(
        matches!(ran.outcome, Ok(Outcome::Success)),
        "{:?}",
        ran.outcome
    );
    assert!(
        ran.out.contains("a.py") && !ran.out.contains("b.toy"),
        "{}",
        ran.out
    );
}

#[test]
fn a_where_no_language_reading_the_pattern_can_read_is_refused() {
    let ran = in_two_languages(
        "where-none",
        "find `f($x)` where $x matches `g(`",
        &[("a.py", "f(1)\n")],
    );
    assert!(
        matches!(
            &ran.outcome,
            Err(Failure::Script { message, .. }) if !message.contains("as Toy")
        ),
        "the first language's error alone: {:?}",
        ran.outcome
    );
}

#[test]
fn a_rewrite_is_checked_again_once_a_where_leaves_its_pattern_unread() {
    use crate::languages::{PLAIN, TOY};
    // Toy reads the pattern and the `where` but not the rewrite; Plain reads the pattern and
    // the rewrite but not the `where`.  Once the `where` leaves Plain out, no language that
    // reads the pattern reads the rewrite, so the run stops before reading any file.
    let ran = in_languages(
        &[TOY, PLAIN],
        "rewrite-after-where",
        "replace `f($x)` with `lambda: $x` where $x matches `yielded` expect 1",
        &[("a.toy", "f(yielded)\n"), ("b.plain", "f(yielded)\n")],
    );
    assert!(
        matches!(
            &ran.outcome,
            Err(Failure::Script { message, .. }) if message.contains("`lambda` is not Toy")
        ),
        "{:?}",
        ran.outcome
    );
    assert_eq!(ran.after, ["f(yielded)\n", "f(yielded)\n"]);
}

#[test]
fn a_file_unparseable_for_a_search_and_an_unwritable_rewrite_is_named_once() {
    let ran = in_two_languages(
        "unparsed-once",
        "find `f($x)`\nreplace `f($x)` with `lambda: $x` expect any",
        &[("a.py", "f(1)\n"), ("b.toy", "f(2\n")],
    );
    assert!(
        matches!(&ran.outcome, Err(Failure::Unparseable { files }) if files.len() == 1),
        "{:?}",
        ran.outcome
    );
}
