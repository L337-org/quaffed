//! Instruction counts of the Python front end and the matcher, over the synthetic corpus only,
//! with what a structural replace adds to them: matching with captures, compiling a rewrite
//! template, and lexing a file for what a rewrite must handle with care.
//!
//! Run under gungraun's runner and Valgrind's Callgrind, Linux only: `cargo bench -p
//! quaffed-python --bench front_end`.  `scripts/benchmarks` counts one tree's, and
//! `scripts/benchmark-gate` compares main's with a pull request's; architecture/performance.md
//! has how, and why.
//!
//! Reading and decoding the files, building arenas to search and compiling patterns are setup,
//! which gungraun does not count.  Each benchmark hands its input back, so that freeing it falls
//! outside what is counted too, and each counts only the work it names.  Ruff's parse
//! is counted alone as well as with the arena built from it, so that what the arena costs can be
//! held to its own budget as a share of the parse.

// gungraun's macros generate modules and constants for each benchmark, with no documentation
// to carry; the benchmarks themselves are described where they are written.
#![allow(missing_docs)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use common::{GROUPS, Mode, group_files};
use gungraun::prelude::*;
use quaffed_encoding::python::Source;
use quaffed_python::{
    Rewrite, compile_pattern, compile_rewrite, lex, parse_expression, parse_ipython, parse_module,
};
use quaffed_syntax::{Arena, Pattern, find, find_captured};
use ruff_python_parser::ParseOptions;

/// Every file of the synthetic corpus, decoded as quaff decodes it, with how it is parsed.
fn corpus() -> Vec<(Mode, String)> {
    GROUPS
        .iter()
        .flat_map(|group| {
            group_files(group).into_iter().map(move |path| {
                let bytes = std::fs::read(&path)
                    .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
                let source = Source::decode(&bytes)
                    .unwrap_or_else(|err| panic!("decoding {}: {err}", path.display()));
                (group.mode, source.text)
            })
        })
        .collect()
}

/// `text` parsed into an arena as `mode` says.
fn arena(mode: Mode, text: &str) -> Arena<'_> {
    let parse = match mode {
        Mode::Module => parse_module,
        Mode::Expression => parse_expression,
        Mode::Ipython => parse_ipython,
    };
    parse(text).unwrap_or_else(|err| panic!("parsing a synthetic corpus file: {err}"))
}

// Ruff's parse of every file, and nothing of quaff's: what the arena's cost is a share of.
#[library_benchmark]
#[bench::synthetic(corpus())]
fn ruff_parse(files: Vec<(Mode, String)>) -> Vec<(Mode, String)> {
    for (mode, text) in &files {
        let mode = match mode {
            Mode::Module => ruff_python_parser::Mode::Module,
            Mode::Expression => ruff_python_parser::Mode::Expression,
            Mode::Ipython => ruff_python_parser::Mode::Ipython,
        };
        black_box(
            ruff_python_parser::parse(black_box(text), ParseOptions::from(mode))
                .unwrap_or_else(|err| panic!("Ruff parsing a synthetic corpus file: {err}")),
        );
    }
    files
}

// Ruff's parse of every file with the arena built from it, as quaff parses a file.
#[library_benchmark]
#[bench::synthetic(corpus())]
fn parse_into_arena(files: Vec<(Mode, String)>) -> Vec<(Mode, String)> {
    for (mode, text) in &files {
        black_box(arena(*mode, black_box(text)));
    }
    files
}

/// `pattern` compiled, with every file of the corpus parsed into an arena to search.
fn searching(pattern: &str) -> (Pattern, Vec<Arena<'static>>) {
    let pattern = compile_pattern(pattern)
        .unwrap_or_else(|err| panic!("compiling the benchmark's pattern {pattern:?}: {err}"));
    let arenas = corpus()
        .iter()
        .map(|(mode, text)| arena(*mode, text).into_owned())
        .collect();
    (pattern, arenas)
}

// One pattern searched for in every file.  The patterns are the real-world check's, which
// between them use each kind of hole and a name used twice.
#[library_benchmark]
#[bench::single_and_unbound_holes(searching("isinstance($x, $_)"))]
#[bench::an_unbound_sequence(searching("print($...)"))]
#[bench::a_named_sequence(searching("self.assertEqual($args...)"))]
#[bench::sequences_bounded_by_their_node(searching("try:\n    $...\nexcept $e:\n    $..."))]
#[bench::a_name_used_twice(searching("$x = $y\nreturn $x"))]
#[bench::a_sequence_between_two_statements(searching("$x = $y\n$...\nreturn $x"))]
fn matching(searching: (Pattern, Vec<Arena<'static>>)) -> (Pattern, Vec<Arena<'static>>) {
    let (pattern, arenas) = &searching;
    for arena in arenas {
        black_box(find(black_box(pattern), black_box(arena)));
    }
    searching
}

// Each pattern compiled.
#[library_benchmark]
#[bench::single_and_unbound_holes("isinstance($x, $_)")]
#[bench::a_sequence_between_two_statements("$x = $y\n$...\nreturn $x")]
fn compiling(pattern: &str) -> Pattern {
    compile_pattern(black_box(pattern))
        .unwrap_or_else(|err| panic!("compiling the benchmark's pattern {pattern:?}: {err}"))
}

/// `pattern` compiled with `kept`, the names whose captures a replace asks for, and every file of
/// the corpus parsed into an arena to search.
fn capturing(
    pattern: &str,
    kept: &'static [&'static str],
) -> (Pattern, &'static [&'static str], Vec<Arena<'static>>) {
    let (pattern, arenas) = searching(pattern);
    (pattern, kept, arenas)
}

// One pattern searched for in every file as a replace searches, asking for what its holes
// captured: a node each, and a sequence.
#[library_benchmark]
#[bench::single_holes(capturing("isinstance($x, $t)", &["x", "t"]))]
#[bench::a_named_sequence(capturing("self.assertEqual($args...)", &["args"]))]
fn matching_with_captures(
    capturing: (Pattern, &'static [&'static str], Vec<Arena<'static>>),
) -> (Pattern, &'static [&'static str], Vec<Arena<'static>>) {
    let (pattern, kept, arenas) = &capturing;
    for arena in arenas {
        black_box(find_captured(black_box(pattern), black_box(arena), kept));
    }
    capturing
}

/// `pattern` compiled, to compile `template` against.
fn rewriting(pattern: &str, template: &'static str) -> (Pattern, &'static str) {
    let pattern = compile_pattern(pattern)
        .unwrap_or_else(|err| panic!("compiling the benchmark's pattern {pattern:?}: {err}"));
    (pattern, template)
}

// A rewrite template compiled against its pattern: holes in code, and in a string and a comment.
#[library_benchmark]
#[bench::holes_reordered(rewriting("isinstance($x, $t)", "instance_of($t, $x)"))]
#[bench::holes_in_literals(rewriting(
    "def $f(): $body...",
    "def $f():  # was $f\n    \"\"\"TODO: document $f.\"\"\"\n    $body..."
))]
fn compiling_rewrite(rewriting: (Pattern, &'static str)) -> (Pattern, Rewrite) {
    let (pattern, template) = rewriting;
    let rewrite = compile_rewrite(black_box(template), black_box(&pattern))
        .unwrap_or_else(|err| panic!("compiling the benchmark's rewrite {template:?}: {err}"));
    (pattern, rewrite)
}

// Every file lexed for its comments, multi-line strings, brackets and indent unit, as a replace
// lexes each file it rewrites.
#[library_benchmark]
#[bench::synthetic(corpus())]
fn lexing(files: Vec<(Mode, String)>) -> Vec<(Mode, String)> {
    for (_, text) in &files {
        black_box(lex(black_box(text)));
    }
    files
}

library_benchmark_group!(
    name = front_end,
    benchmarks = [
        ruff_parse,
        parse_into_arena,
        compiling,
        matching,
        matching_with_captures,
        compiling_rewrite,
        lexing
    ]
);

main!(library_benchmark_groups = front_end);
