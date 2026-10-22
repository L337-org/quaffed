//! Properties of the pattern compiler that hold for any pattern, checked on generated ones.
//!
//! - **Nothing makes it panic**: every input is a pattern or an error.
//! - **A pattern without a hole keeps its text exactly**, so it matches what was written.
//! - **Every hole written in code becomes one hole, in order**, whatever names it carries.
//! - **A hole written inside a string literal is literal text**, restored as written, whatever
//!   the string's prefix, an f-string's or t-string's literal text included.

use proptest::prelude::*;
use quaffed_python::compile_pattern;

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
        names in prop::collection::btree_set(name(), 1..5),
        sequence in any::<bool>(),
    ) {
        // Distinct names: one name as both a node and a sequence is refused, which another
        // test covers.
        let names: Vec<String> = names.into_iter().collect();
        let last = names.len() - 1;
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
