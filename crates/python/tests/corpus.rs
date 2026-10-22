//! The synthetic corpus covers Python's whole grammar, as the pinned Ruff knows it.
//!
//! The corpus, under `corpus/synthetic/` at the workspace root, is the primary input
//! to any invariant harness, budget or benchmark, so a construct missing from it is a construct
//! none of them checks.  This test parses every file - `modules/*.py` as modules,
//! `expressions/*.pyexpr` as single expressions, `ipython/*.ipy` as `IPython` source - and
//! fails naming each kind of node that no file contains.  `architecture/verification.md` says
//! how to add to the corpus.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use common::{GROUPS, Mode, files, group_files, synthetic};
use quaffed_python::{Kind, all_kinds, kinds_in_expression, kinds_in_ipython, kinds_in_module};

/// Every kind found across the corpus, failing on any file that does not parse.
fn kinds_in_corpus() -> BTreeSet<Kind> {
    let mut found = BTreeSet::new();
    for group in &GROUPS {
        let parse: Parser = match group.mode {
            Mode::Module => kinds_in_module,
            Mode::Expression => kinds_in_expression,
            Mode::Ipython => kinds_in_ipython,
        };
        for path in group_files(group) {
            found.extend(parsed(&path, parse));
        }
    }
    found
}

/// The kinds `parse` finds in the file at `path`, failing the test if it does not parse.
fn parsed(path: &Path, parse: Parser) -> BTreeSet<Kind> {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|err| panic!("{} is not UTF-8: {err}", path.display()));
    // A byte-order mark is the file's encoding declaration, not source; CPython strips it before
    // parsing.
    let source = text.strip_prefix('\u{feff}').unwrap_or(&text);
    parse(source).unwrap_or_else(|err| panic!("parsing {}: {err}", path.display()))
}

/// One of the adapter's ways of parsing source into the kinds it contains.
type Parser = fn(&str) -> Result<BTreeSet<Kind>, quaffed_python::ParseError>;

/// The kinds in `all` that are not in `found`.
fn missing(all: &[Kind], found: &BTreeSet<Kind>) -> Vec<&'static str> {
    all.iter()
        .filter(|kind| !found.contains(kind))
        .map(|kind| kind.name())
        .collect()
}

#[test]
fn every_kind_in_the_grammar_appears_in_the_synthetic_corpus() {
    let found = kinds_in_corpus();
    let gaps = missing(all_kinds(), &found);
    assert!(
        gaps.is_empty(),
        "{} kinds of node appear in no file of the synthetic corpus: {}.  Add Python that \
         produces each, as architecture/verification.md describes",
        gaps.len(),
        gaps.join(", ")
    );
}

#[test]
fn removing_a_kind_from_what_was_found_fails_the_check() {
    let mut found = kinds_in_corpus();
    let removed = *found
        .iter()
        .find(|kind| kind.name() == "PatternMatchStar")
        .expect("the corpus has a star pattern");
    found.remove(&removed);
    assert_eq!(missing(all_kinds(), &found), ["PatternMatchStar"]);
}

/// The bytes of every module file, by name, so a form can be looked for in the raw file.
fn module_bytes() -> Vec<(String, Vec<u8>)> {
    files(&synthetic().join("modules"), "py")
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a file")
                .to_string_lossy()
                .into_owned();
            let bytes =
                fs::read(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
            (name, bytes)
        })
        .collect()
}

/// Whether `bytes` has each of the three line endings: LF alone, CRLF, and CR alone.
fn line_endings(bytes: &[u8]) -> (bool, bool, bool) {
    let mut lf = false;
    let mut crlf = false;
    let mut cr = false;
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => crlf = true,
            b'\r' => cr = true,
            b'\n' if i == 0 || bytes[i - 1] != b'\r' => lf = true,
            _ => {}
        }
    }
    (lf, crlf, cr)
}

#[test]
fn the_corpus_holds_every_layout_form_byte_for_byte() {
    let files = module_bytes();
    let any = |what: &str, holds: &dyn Fn(&[u8]) -> bool| {
        assert!(
            files.iter().any(|(_, bytes)| holds(bytes)),
            "no file of the synthetic corpus has {what}"
        );
    };
    any("a UTF-8 byte-order mark", &|b| {
        b.starts_with(b"\xEF\xBB\xBF")
    });
    any("LF, CRLF and a lone CR in one file", &|b| {
        line_endings(b) == (true, true, true)
    });
    any("a line indented with a tab", &|b| {
        b.windows(2).any(|w| w == b"\n\t")
    });
    any("a line indented with spaces", &|b| {
        b.windows(5).any(|w| w == b"\n    ")
    });
    any("non-ASCII text", &|b| !b.is_ascii());
    any("a line-bound # noqa pragma", &|b| {
        b.windows(6).any(|w| w == b"# noqa")
    });
}

#[test]
fn the_line_ending_reader_tells_the_three_apart() {
    assert_eq!(line_endings(b"a\nb"), (true, false, false));
    assert_eq!(line_endings(b"a\r\nb"), (false, true, false));
    assert_eq!(line_endings(b"a\rb"), (false, false, true));
    assert_eq!(line_endings(b"\na\r\nb\rc"), (true, true, true));
}
