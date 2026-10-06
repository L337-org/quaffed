//! The synthetic corpus covers Python's whole grammar, as the pinned Ruff knows it.
//!
//! The corpus, under `corpus/synthetic/` at the workspace root, is the input the invariant
//! harness, the budgets and the benchmarks will measure, so a construct missing from it is a
//! construct none of them checks.  This test parses every file - `modules/*.py` as modules,
//! `expressions/*.pyexpr` as single expressions, `ipython/*.ipy` as `IPython` source - and
//! fails naming each kind of node that no file contains.  `architecture/verification.md` says
//! how to add to the corpus.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use quaffed_python::{Kind, all_kinds, kinds_in_expression, kinds_in_ipython, kinds_in_module};

/// Fewer files than this means the corpus was not found or was emptied, not that it is complete.
const MODULE_FLOOR: usize = 8;
const EXPRESSION_FLOOR: usize = 2;
const IPYTHON_FLOOR: usize = 1;

fn corpus() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/python; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/python has a grandparent")
        .join("corpus/synthetic")
}

/// The files under `dir` with `extension`, sorted, so failures read the same on every run.
fn files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()))
        .map(|entry| entry.expect("reading a directory entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .collect();
    found.sort();
    found
}

/// Every kind found across the corpus, failing on any file that does not parse.
fn kinds_in_corpus() -> BTreeSet<Kind> {
    let root = corpus();
    let modules = files(&root.join("modules"), "py");
    let expressions = files(&root.join("expressions"), "pyexpr");
    let ipython = files(&root.join("ipython"), "ipy");
    assert!(
        modules.len() >= MODULE_FLOOR,
        "found {} module files under {}, fewer than the floor of {MODULE_FLOOR}",
        modules.len(),
        root.join("modules").display()
    );
    assert!(
        expressions.len() >= EXPRESSION_FLOOR,
        "found {} expression files under {}, fewer than the floor of {EXPRESSION_FLOOR}",
        expressions.len(),
        root.join("expressions").display()
    );
    assert!(
        ipython.len() >= IPYTHON_FLOOR,
        "found {} IPython files under {}, fewer than the floor of {IPYTHON_FLOOR}",
        ipython.len(),
        root.join("ipython").display()
    );
    let mut found = BTreeSet::new();
    for path in &modules {
        found.extend(parsed(path, kinds_in_module));
    }
    for path in &expressions {
        found.extend(parsed(path, kinds_in_expression));
    }
    for path in &ipython {
        found.extend(parsed(path, kinds_in_ipython));
    }
    found
}

/// The kinds `parse` finds in the file at `path`, failing the test if it does not parse.
fn parsed(path: &Path, parse: Parser) -> BTreeSet<Kind> {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|err| panic!("{} is not UTF-8: {err}", path.display()));
    // A byte-order mark is the file's encoding declaration, not source; CPython strips it before
    // parsing, and so will the front end.
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
    files(&corpus().join("modules"), "py")
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
