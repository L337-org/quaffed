//! Every corpus file is parsed into an arena and printed back byte for byte, and `CPython` decides
//! which files should parse.
//!
//! A file's bytes are decoded as quaff decodes them (`quaffed-encoding`), parsed into an arena,
//! printed from the arena - each node's children in order with the text between them, and the
//! text around the root - and encoded back.  The result must be the file's bytes.  That holds
//! only if the arena's ranges nest, keep source order and stay inside the text, and decoding and
//! encoding lose nothing.  It does not show that every construct became a node: a construct left
//! out prints as text between nodes, which is what the corpus's coverage check is for.
//!
//! The synthetic corpus runs in the ordinary test run; every file in it must round-trip.  The
//! real-world corpus is fetched, not checked in, so its test is ignored by default and the
//! **Real-world corpus** CI job runs it with `--ignored`: every file `CPython` accepts must
//! round-trip, and every file it refuses, as `refused.txt` records, must be refused.
//! `architecture/corpus.md` describes both.

mod common;
mod fetched;

use std::fs;
use std::path::{Path, PathBuf};

use common::{GROUPS, Mode, group_files, workspace_root};
use fetched::{Fetched, fetched, python_files, sources};
use quaffed_encoding::python::Source;
use quaffed_python::{ParseError, parse_expression, parse_ipython, parse_module};
use quaffed_syntax::Arena;

/// How the arena is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Printer {
    /// Every byte: each node's children with the text between them, and the text around the
    /// root.
    Faithful,
    /// Only the nodes: a node with children prints its children and nothing between them.  The
    /// one way this shape of arena can lose bytes, so the round trip must catch it.
    DropsTheTextBetweenNodes,
}

/// One step of the printer's walk.
enum Step {
    Enter(quaffed_syntax::NodeId),
    Leave(quaffed_syntax::NodeId),
}

/// The arena's text as `printer` prints it.  It never panics on a bad range, the root's
/// included: a range that runs backwards, past the text or through a character prints nothing,
/// so the result differs and the file is reported with the rest.
fn print(arena: &Arena<'_>, printer: Printer) -> String {
    let text = arena.text();
    let slice = |from: usize, to: usize| text.get(from..to).unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    if printer == Printer::Faithful {
        out.push_str(slice(0, arena.range(arena.root()).start));
    }
    // A node is finished when it is left, so the walk keeps, for each open node, where its text
    // has been printed up to.
    let mut cursor: Vec<usize> = Vec::new();
    let mut steps = vec![Step::Enter(arena.root())];
    while let Some(step) = steps.pop() {
        match step {
            Step::Enter(node) => {
                let range = arena.range(node);
                if let Some(parent) = cursor.last_mut() {
                    if printer == Printer::Faithful {
                        out.push_str(slice(*parent, range.start));
                    }
                    *parent = range.end;
                }
                cursor.push(range.start);
                steps.push(Step::Leave(node));
                let children: Vec<_> = arena.children(node).collect();
                if children.is_empty() {
                    out.push_str(slice(range.start, range.end));
                    *cursor.last_mut().expect("just pushed") = range.end;
                }
                steps.extend(children.into_iter().rev().map(Step::Enter));
            }
            Step::Leave(node) => {
                let printed_to = cursor.pop().expect("entered");
                if printer == Printer::Faithful {
                    out.push_str(slice(printed_to, arena.range(node).end));
                }
            }
        }
    }
    if printer == Printer::Faithful {
        out.push_str(slice(arena.range(arena.root()).end, text.len()));
    }
    out
}

/// What the front end made of a file.
enum Made {
    /// Its bytes, decoded, parsed, printed and encoded back.
    Bytes(Vec<u8>),
    /// It was refused, decoding or parsing, and why.
    Refused(String),
    /// It parsed, but the printed text could not be encoded back.
    Unencodable(String),
}

fn front_end(bytes: &[u8], mode: Mode, printer: Printer) -> Made {
    let source = match Source::decode(bytes) {
        Ok(source) => source,
        Err(err) => return Made::Refused(format!("decoding: {err}")),
    };
    let parse: fn(&str) -> Result<Arena<'_>, ParseError> = match mode {
        Mode::Module => parse_module,
        Mode::Expression => parse_expression,
        Mode::Ipython => parse_ipython,
    };
    let arena = match parse(&source.text) {
        Ok(arena) => arena,
        Err(err) => return Made::Refused(format!("parsing: {err}")),
    };
    match source.encode(&print(&arena, printer)) {
        Ok(bytes_back) => Made::Bytes(bytes_back),
        Err(err) => Made::Unencodable(err.to_string()),
    }
}

/// What a file should do.
enum Expect<'a> {
    /// Parse and print back byte for byte.
    RoundTrip,
    /// Be refused, as `CPython` refuses it, for this reason.
    Refused(&'a str),
}

/// Why `bytes`, the file `label`, did not do as `expect` says, or `None` if it did.
fn check(
    label: &str,
    bytes: &[u8],
    mode: Mode,
    expect: &Expect<'_>,
    printer: Printer,
) -> Option<String> {
    match (front_end(bytes, mode, printer), expect) {
        (Made::Bytes(bytes_back), Expect::RoundTrip) => first_difference(bytes, &bytes_back)
            .map(|at| describe_difference(label, bytes, &bytes_back, at)),
        (Made::Bytes(_), Expect::Refused(reason)) => Some(format!(
            "{label}: parsed, but CPython refuses it ({reason})"
        )),
        (Made::Refused(why), Expect::RoundTrip) => {
            Some(format!("{label}: refused, {why}, but it should parse"))
        }
        (Made::Refused(_), Expect::Refused(_)) => None,
        (Made::Unencodable(why), _) => Some(format!(
            "{label}: the printed text cannot be encoded back: {why}"
        )),
    }
}

/// The first byte at which `a` and `b` differ, counting a missing byte as a difference.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}

fn describe_difference(label: &str, expected: &[u8], got: &[u8], at: usize) -> String {
    let window = |bytes: &[u8]| {
        let end = bytes.len().min(at + 16);
        String::from_utf8_lossy(&bytes[at.min(end)..end]).into_owned()
    };
    format!(
        "{label}: printed back differs from byte {at} of {}: the file has {:?}, the print has \
         {:?} ({} bytes printed)",
        expected.len(),
        window(expected),
        window(got),
        got.len()
    )
}

/// Every failure across the synthetic corpus, each file expected to round-trip, and how many
/// files were read.
fn judge_synthetic(printer: Printer) -> (Vec<String>, usize) {
    let mut failures = Vec::new();
    let mut read = 0;
    for group in &GROUPS {
        for path in group_files(group) {
            let bytes =
                fs::read(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
            read += 1;
            let label = format!("{}/{}", group.dir, name(&path));
            failures.extend(check(
                &label,
                &bytes,
                group.mode,
                &Expect::RoundTrip,
                printer,
            ));
        }
    }
    (failures, read)
}

fn name(path: &Path) -> String {
    path.file_name()
        .expect("a file")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn every_synthetic_file_round_trips() {
    let (failures, read) = judge_synthetic(Printer::Faithful);
    assert!(read > 0, "read no files from the synthetic corpus");
    assert!(
        failures.is_empty(),
        "{} of {read} synthetic files did not round-trip:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_printer_that_drops_the_text_between_nodes_fails_on_every_synthetic_file() {
    let (failures, read) = judge_synthetic(Printer::DropsTheTextBetweenNodes);
    assert_eq!(
        failures.len(),
        read,
        "the lossy printer passed on some files, so the round trip checks nothing there:\n{}",
        failures.join("\n")
    );
    assert!(
        failures.iter().all(|f| f.contains("printed back differs")),
        "{failures:#?}"
    );
}

// The real-world corpus.

/// Every failure across one fetched source, judged against its oracle, and how many files
/// were checked.
fn judge_fetched(source: &Fetched, printer: Printer) -> (Vec<String>, usize) {
    let files = python_files(&source.dir);
    let mut failures = Vec::new();
    if files.len() != source.read || files.is_empty() {
        failures.push(format!(
            "{}: found {} .py files, but the oracle read {}",
            source.dir.display(),
            files.len(),
            source.read
        ));
    }
    for missing in source
        .refused
        .keys()
        .filter(|p| files.binary_search(p).is_err())
    {
        failures.push(format!(
            "{missing}: refused.txt lists it, but it is not there"
        ));
    }
    for relative in &files {
        let path = source.dir.join(relative);
        let bytes =
            fs::read(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        let expect = match source.refused.get(relative) {
            Some(reason) => Expect::Refused(reason),
            None => Expect::RoundTrip,
        };
        failures.extend(check(relative, &bytes, Mode::Module, &expect, printer));
    }
    (failures, files.len())
}

#[test]
#[ignore = "needs the real-world corpus from scripts/fetch-corpus; the Real-world corpus CI job \
            runs it with --ignored"]
fn every_real_world_file_round_trips_or_is_refused_as_cpython_refuses() {
    let sources = sources(&workspace_root().join("target/corpus"));
    let mut failures = Vec::new();
    let mut read = 0;
    for dir in &sources {
        let source = fetched(dir).unwrap_or_else(|err| panic!("{err}"));
        let (found, checked) = judge_fetched(&source, Printer::Faithful);
        failures.extend(found);
        read += checked;
        println!(
            "{}: {checked} files, {} refused as the oracle refuses them",
            dir.display(),
            source.refused.len()
        );
    }
    assert!(
        failures.is_empty(),
        "{} problems across {read} real-world files:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// The harness's own behaviour, on small corpora it writes.

/// A throwaway fetched source: `files` written under it, and a `refused.txt` with `header`
/// and `refused` lines.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str, files: &[(&str, &[u8])], header: &str, refused: &[&str]) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("quaffed-round-trip-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (path, bytes) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a parent"))
                .expect("creating the scratch tree");
            fs::write(&path, bytes).expect("writing a scratch file");
        }
        fs::create_dir_all(&dir).expect("creating the scratch directory");
        let mut list = vec![header.to_owned()];
        list.extend(refused.iter().map(|line| (*line).to_owned()));
        fs::write(dir.join("refused.txt"), list.join("\n") + "\n").expect("writing refused.txt");
        Scratch(dir)
    }

    fn judge(&self) -> Vec<String> {
        let source = fetched(&self.0).unwrap();
        judge_fetched(&source, Printer::Faithful).0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_fetched_source_that_agrees_with_its_oracle_passes() {
    let scratch = Scratch::new(
        "agrees",
        &[("Lib/a.py", b"x = 1\r\n"), ("Lib/sub/b.py", b"def (:\n")],
        "# 2 files read; CPython accepts 1 and refuses 1.",
        &["Lib/sub/b.py\tSyntaxError: invalid syntax"],
    );
    assert_eq!(scratch.judge(), Vec::<String>::new());
}

#[test]
fn a_file_the_oracle_refuses_that_parses_fails_the_check() {
    let scratch = Scratch::new(
        "parses",
        &[("a.py", b"x = 1\n")],
        "# 1 files read; CPython accepts 0 and refuses 1.",
        &["a.py\tSyntaxError: pretend"],
    );
    assert_eq!(
        scratch.judge(),
        ["a.py: parsed, but CPython refuses it (SyntaxError: pretend)"]
    );
}

#[test]
fn a_file_the_oracle_accepts_that_is_refused_fails_the_check() {
    let scratch = Scratch::new(
        "refused",
        &[("a.py", b"def (:\n")],
        "# 1 files read; CPython accepts 1 and refuses 0.",
        &[],
    );
    let failures = scratch.judge();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with("a.py: refused, parsing: at byte 4"),
        "{failures:?}"
    );
}

#[test]
fn a_source_with_no_files_fails_rather_than_passes() {
    let scratch = Scratch::new(
        "empty",
        &[],
        "# 0 files read; CPython accepts 0 and refuses 0.",
        &[],
    );
    let failures = scratch.judge();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("found 0 .py files, but the oracle read 0"),
        "{failures:?}"
    );
}

#[test]
fn a_count_that_disagrees_with_the_oracles_fails() {
    let scratch = Scratch::new(
        "count",
        &[("a.py", b"x = 1\n")],
        "# 2 files read; CPython accepts 2 and refuses 0.",
        &[],
    );
    let failures = scratch.judge();
    assert!(
        failures[0].ends_with("found 1 .py files, but the oracle read 2"),
        "{failures:?}"
    );
}

#[test]
fn a_refused_file_that_is_not_there_fails() {
    let scratch = Scratch::new(
        "missing",
        &[("a.py", b"x = 1\n")],
        "# 1 files read; CPython accepts 1 and refuses 0.",
        &["gone.py\tSyntaxError: pretend"],
    );
    assert_eq!(
        scratch.judge(),
        ["gone.py: refused.txt lists it, but it is not there"]
    );
}

#[test]
fn a_difference_names_the_file_and_the_first_differing_byte() {
    assert_eq!(first_difference(b"abc", b"abc"), None);
    assert_eq!(first_difference(b"abc", b"abx"), Some(2));
    assert_eq!(first_difference(b"abc", b"ab"), Some(2));
    assert_eq!(first_difference(b"ab", b"abc"), Some(2));
    assert_eq!(
        describe_difference("m.py", b"x = 1\n", b"x=1\n", 1),
        "m.py: printed back differs from byte 1 of 6: the file has \" = 1\\n\", the print has \
         \"=1\\n\" (4 bytes printed)"
    );
}

#[test]
fn a_non_utf8_file_round_trips_through_its_declared_encoding() {
    let mut koi8 = b"# -*- coding: koi8-r -*-\ns = '".to_vec();
    koi8.extend([0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4]);
    koi8.extend(b"'\n");
    assert_eq!(
        check(
            "koi8.py",
            &koi8,
            Mode::Module,
            &Expect::RoundTrip,
            Printer::Faithful
        ),
        None
    );
    let marked = b"\xEF\xBB\xBFx = 1\r\ny = 2\rz = 3\n";
    assert_eq!(
        check(
            "bom.py",
            marked,
            Mode::Module,
            &Expect::RoundTrip,
            Printer::Faithful
        ),
        None
    );
}

#[test]
fn a_range_out_of_place_prints_nothing_rather_than_panicking() {
    let text = "ab";
    let mut builder = quaffed_syntax::Builder::new(quaffed_python::PYTHON, text).unwrap();
    builder.enter(quaffed_syntax::Kind::new("Root"), 0..2);
    builder.enter(quaffed_syntax::Kind::new("Past"), 1..9);
    builder.leave();
    builder.leave();
    let arena = builder.finish();
    assert_eq!(print(&arena, Printer::Faithful), "a");

    // A root past the text, or through a character, is reported rather than panicking.
    for (text, root) in [("ab", 1..9), ("\u{e9}x", 1..3)] {
        let mut builder = quaffed_syntax::Builder::new(quaffed_python::PYTHON, text).unwrap();
        builder.enter(quaffed_syntax::Kind::new("Root"), root);
        builder.leave();
        let arena = builder.finish();
        assert_ne!(print(&arena, Printer::Faithful), text);
    }
}
