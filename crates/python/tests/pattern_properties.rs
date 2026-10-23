//! Properties of the pattern compiler that hold for any pattern, checked on generated ones.
//!
//! - **Nothing makes it panic**: every input is a pattern or an error.
//! - **A pattern without a hole keeps its text exactly**, so it matches what was written.
//! - **Every hole written in code becomes one hole, in order**, whatever names it carries.
//! - **A hole written inside a string literal is literal text**, restored as written, whatever
//!   the string's prefix, an f-string's or t-string's literal text included.
//! - **A sequence hole in a run of statements is refused at its two ends and only there.**

use proptest::prelude::*;
use quaffed_python::{PatternError, compile_pattern};

/// Pieces of Python and of hole syntax, so generated patterns hit the compiler's edges: holes
/// next to each other, a bare `$`, `${`, quotes and comments.
fn piece() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "$a", "$_", "$b...", "$...", "$_...", "$", "${", "$9", "f(", ")", ", ", "x", " + ", "\"",
        "'", "#", "\n", "    ", "def ", ":", "[", "]", "é", "\r\n", ".", "=",
    ])
}

/// Python's keywords and soft keywords, which are not names in a call.
const KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "type", "while",
    "with", "yield",
];

/// A name: a letter first, then letters, digits or `_`, and never a keyword, so that it is a
/// name wherever the tests put it.
fn name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,6}".prop_filter("a keyword is not a name", |n| {
        !KEYWORDS.contains(&n.as_str())
    })
}

proptest! {
    #[test]
    fn any_text_is_a_pattern_or_an_error(pieces in prop::collection::vec(piece(), 0..12)) {
        let _ = compile_pattern(&pieces.concat());
    }

    #[test]
    fn a_sequence_in_a_run_of_statements_is_refused_exactly_at_its_ends(
        calls in prop::collection::vec(name(), 2..6),
        at in any::<prop::sample::Index>(),
    ) {
        let at = at.index(calls.len());
        let lines: Vec<String> = calls
            .iter()
            .enumerate()
            .map(|(i, callee)| if i == at { "$s...".to_owned() } else { format!("{callee}()") })
            .collect();
        let text = lines.join("\n");
        let edge = at == 0 || at == calls.len() - 1;
        match compile_pattern(&text) {
            Err(PatternError::UnboundedSequence { leading, .. }) => {
                prop_assert!(edge, "{text:?} refused with its sequence in the middle");
                prop_assert_eq!(leading, at == 0);
            }
            Ok(pattern) => {
                prop_assert!(!edge, "{text:?} compiled with a sequence at an edge");
                prop_assert_eq!(pattern.unbounded_sequence(), None);
            }
            Err(err) => prop_assert!(false, "{text:?}: {err}"),
        }
    }

    #[test]
    fn a_pattern_without_a_hole_keeps_its_text(
        callee in name(),
        argument in name(),
        spaces in "[ ]{0,3}",
    ) {
        let text = format!("{callee}({spaces}{argument}{spaces})");
        let pattern = compile_pattern(&text).unwrap();
        prop_assert_eq!(pattern.arena().text(), text.as_str());
        prop_assert_eq!(pattern.holes().count(), 0);
    }

    #[test]
    fn every_hole_in_code_is_one_hole_in_order(
        names in prop::collection::vec(name(), 1..5),
        sequence in any::<bool>(),
    ) {
        // A name may repeat, but one used both for a node and for a sequence is refused, which
        // another test covers; so the sequence, if there is one, gets a name of its own, which
        // a capital letter makes, since the generated names have none.
        let mut names = names;
        let last = names.len() - 1;
        if sequence {
            names[last] = format!("{}S", names[last]);
        }
        let arguments: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(i, n)| if sequence && i == last { format!("${n}...") } else { format!("${n}") })
            .collect();
        let pattern = compile_pattern(&format!("call({})", arguments.join(", "))).unwrap();
        let found: Vec<_> = pattern.holes().map(|(_, h)| (h.name.clone(), h.sequence)).collect();
        let expected: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (Some(n.clone()), sequence && i == last))
            .collect();
        prop_assert_eq!(found, expected);
    }

    #[test]
    fn a_hole_in_a_string_is_literal_text(
        before in "[a-z ]{0,5}",
        hole in name(),
        after in "[a-z ]{0,5}",
        quote in prop::sample::select(vec!["\"", "'", "'''", "\"\"\""]),
        prefix in prop::sample::select(vec!["", "r", "b", "f", "t", "rf"]),
    ) {
        let text = format!("f({prefix}{quote}{before}${hole}{after}{quote})");
        let pattern = compile_pattern(&text).unwrap();
        prop_assert_eq!(pattern.holes().count(), 0);
        prop_assert_eq!(pattern.arena().text(), text.as_str());
    }
}
