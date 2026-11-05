// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Matching a glob against a value, by the segments of the kind of value it is.
//!
//! A glob is compiled to a regular expression, which `regex-automata` matches in time linear in
//! the value however the glob is written, so no glob, however many stars and alternatives it
//! has, can make a match slow.  What a segment is - a path's directory name, a string's line -
//! is the one thing that differs between the two kinds: `*`, `?` and a class stay within one,
//! and `**` crosses them.

use std::fmt::Write as _;

use quaffed_representation::glob::{Class, ClassItem, Glob, GlobPart};
use regex_automata::meta::Regex;

/// The kind of value a glob is matched against, which decides what a segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A path, with `/` between its segments.
    Path,
    /// A string, with its lines as segments.
    Value,
}

impl Kind {
    /// One character that does not end a segment, as a regular expression class's members.
    fn within(self) -> &'static str {
        match self {
            Kind::Path => "/",
            Kind::Value => "\\r\\n",
        }
    }
}

/// A compiled glob.
#[derive(Debug, Clone)]
pub struct Matcher {
    regex: Regex,
}

/// A glob too large to compile.
#[derive(Debug)]
pub struct TooLarge(Box<regex_automata::meta::BuildError>);

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "this glob is too large to match: {}", self.0)
    }
}

impl std::error::Error for TooLarge {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

impl Matcher {
    /// Compiles `glob` to match the whole of a value of `kind`, or, with `anywhere`, any part of
    /// it, as `contains` does: as if written between `**` and `**`, except at an end its anchor
    /// pins.  Anchors are otherwise no-ops, since a glob already matches a whole value.
    ///
    /// # Errors
    ///
    /// Returns [`TooLarge`] when the glob would need more memory to match than a glob is allowed,
    /// which only an enormous one does.
    pub fn new(glob: &Glob, kind: Kind, anywhere: bool) -> Result<Matcher, TooLarge> {
        let mut pattern = String::from(r"\A");
        if anywhere && !glob.start_anchor {
            pattern.push_str("(?s:.)*");
        }
        write_glob(&mut pattern, glob, kind);
        if anywhere && !glob.end_anchor {
            pattern.push_str("(?s:.)*");
        }
        pattern.push_str(r"\z");
        Regex::new(&pattern)
            .map(|regex| Matcher { regex })
            .map_err(|err| TooLarge(Box::new(err)))
    }

    /// Whether `value` matches.
    #[must_use]
    pub fn is_match(&self, value: &str) -> bool {
        self.regex.is_match(value)
    }
}

/// Appends `glob` to `pattern` as a regular expression.
fn write_glob(pattern: &mut String, glob: &Glob, kind: Kind) {
    for part in glob.parts() {
        match part {
            GlobPart::Literal(text) => {
                for c in text.chars() {
                    write_char(pattern, c);
                }
            }
            GlobPart::LineBreak => pattern.push_str(r"(?:\r\n|\n|\r)"),
            GlobPart::Star => {
                let _ = write!(pattern, "[^{}]*", kind.within());
            }
            GlobPart::DoubleStar => pattern.push_str("(?s:.)*"),
            GlobPart::One => {
                let _ = write!(pattern, "[^{}]", kind.within());
            }
            GlobPart::Class(class) => write_class(pattern, class, kind),
            GlobPart::Alternatives(alternatives) => {
                pattern.push_str("(?:");
                for (i, alternative) in alternatives.iter().enumerate() {
                    if i > 0 {
                        pattern.push('|');
                    }
                    write_glob(pattern, alternative, kind);
                }
                pattern.push(')');
            }
            // `GlobPart` is non-exhaustive for later additions; one this matcher does not know
            // could match anything, so it matches nothing rather than too much.
            _ => pattern.push_str("[^\\s\\S]"),
        }
    }
}

/// Appends a class that never matches a segment's end.
fn write_class(pattern: &mut String, class: &Class, kind: Kind) {
    let mut members = String::new();
    for item in &class.items {
        match *item {
            ClassItem::One(c) => write_char(&mut members, c),
            ClassItem::Range(low, high) => {
                write_char(&mut members, low);
                members.push('-');
                write_char(&mut members, high);
            }
        }
    }
    if class.negated {
        let _ = write!(pattern, "[^{members}{}]", kind.within());
    } else {
        let _ = write!(pattern, "[[{members}]&&[^{}]]", kind.within());
    }
}

/// Appends `c` so that it matches only itself, in a class or out of one: ASCII punctuation, the
/// only characters a regular expression gives meaning to, by its code point.
fn write_char(pattern: &mut String, c: char) {
    if c.is_ascii_punctuation() || c.is_ascii_whitespace() {
        let _ = write!(pattern, "\\x{{{:x}}}", u32::from(c));
    } else {
        pattern.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Builds a glob from a compact spelling, for tests only: `*`, `**`, `?` and `\n` are those
    /// parts, `[...]` a class as the script writes one, and everything else literal.
    fn glob(spelled: &str) -> Glob {
        let mut glob = Glob::default();
        let mut rest = spelled;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("**") {
                glob.push(GlobPart::DoubleStar);
                rest = after;
            } else if let Some(after) = rest.strip_prefix('*') {
                glob.push(GlobPart::Star);
                rest = after;
            } else if let Some(after) = rest.strip_prefix('?') {
                glob.push(GlobPart::One);
                rest = after;
            } else if let Some(after) = rest.strip_prefix('\n') {
                glob.push(GlobPart::LineBreak);
                rest = after;
            } else if let Some(after) = rest.strip_prefix('[') {
                let (negated, after) = match after.strip_prefix('!') {
                    Some(after) => (true, after),
                    None => (false, after),
                };
                // A `]` first is a member, as the lexer reads it.
                let close = 1 + after[1..].find(']').expect("a closed class");
                let members: Vec<char> = after[..close].chars().collect();
                let items = if members.len() == 3 && members[1] == '-' {
                    vec![ClassItem::Range(members[0], members[2])]
                } else {
                    members.into_iter().map(ClassItem::One).collect()
                };
                glob.push(GlobPart::Class(Class { negated, items }));
                rest = &after[close + 1..];
            } else {
                let c = rest.chars().next().expect("not empty");
                glob.push_literal(c.to_string());
                rest = &rest[c.len_utf8()..];
            }
        }
        glob
    }

    fn matches(spelled: &str, kind: Kind, value: &str) -> bool {
        Matcher::new(&glob(spelled), kind, false)
            .expect("compiles")
            .is_match(value)
    }

    #[test]
    fn in_a_path_one_star_stays_within_a_directory_and_two_cross_them() {
        assert!(matches("src/*.py", Kind::Path, "src/a.py"));
        assert!(!matches("src/*.py", Kind::Path, "src/sub/a.py"));
        assert!(matches("src/**.py", Kind::Path, "src/sub/a.py"));
        assert!(matches("**/test_*.py", Kind::Path, "tests/test_a.py"));
        assert!(!matches(
            "**/test_*.py",
            Kind::Path,
            "tests/test_x/helper.py"
        ));
        // `..` is an ordinary segment.
        assert!(matches("**/test_*.py", Kind::Path, "../tests/test_a.py"));
        assert!(!matches("?", Kind::Path, "/"));
        assert!(!matches("[!a]", Kind::Path, "/"));
        assert!(!matches("[%-0]", Kind::Path, "/"));
    }

    #[test]
    fn in_a_string_one_star_stays_within_a_line_and_two_cross_them() {
        assert!(matches("https*", Kind::Value, "https://x/y"));
        assert!(matches("fred*\n", Kind::Value, "fred and the rest\n"));
        assert!(!matches("fred*", Kind::Value, "fred\nmore"));
        assert!(matches("fred**", Kind::Value, "fred\nmore"));
        // A line break part is any of the three, a CRLF counting as one.
        for value in ["a\nb", "a\r\nb", "a\rb"] {
            assert!(matches("a\nb", Kind::Value, value), "{value:?}");
        }
        assert!(!matches("a?b", Kind::Value, "a\nb"));
        assert!(!matches("a[!x]b", Kind::Value, "a\rb"));
    }

    #[test]
    fn a_glob_matches_the_whole_value_and_contains_any_part_of_it() {
        assert!(!matches("foo", Kind::Value, "a foo"));
        let anywhere = |spelled: &str, start: bool, end: bool, value: &str| {
            let mut glob = glob(spelled);
            glob.start_anchor = start;
            glob.end_anchor = end;
            Matcher::new(&glob, Kind::Value, true)
                .unwrap()
                .is_match(value)
        };
        assert!(anywhere("foo", false, false, "a\nfoo\nb"));
        assert!(anywhere("foo", true, false, "foo b"));
        assert!(!anywhere("foo", true, false, "a foo"));
        assert!(!anywhere("foo", false, true, "foo b"));
        assert!(anywhere("foo", true, true, "foo"));
    }

    #[test]
    fn classes_and_alternatives_match_as_written() {
        assert!(matches("v[0-9]", Kind::Value, "v7"));
        assert!(!matches("v[0-9]", Kind::Value, "vx"));
        assert!(matches("[!a]", Kind::Value, "b"));
        assert!(!matches("[!a]", Kind::Value, "a"));
        assert!(matches("[]]", Kind::Value, "]"));
        assert!(matches("[\\^]", Kind::Value, "^"));
        let mut either = Glob::default();
        either.push(GlobPart::Alternatives(vec![glob("src"), glob("lib*")]));
        either.push_literal("/a".into());
        let either = Matcher::new(&either, Kind::Path, false).unwrap();
        assert!(either.is_match("src/a"));
        assert!(either.is_match("library/a"));
        assert!(!either.is_match("doc/a"));
        // No alternative is empty, so the alternatives cannot be left out.
        assert!(!either.is_match("/a"));
    }

    #[test]
    fn a_question_mark_matches_exactly_one_character() {
        for kind in [Kind::Path, Kind::Value] {
            assert!(matches("a?c", kind, "abc"));
            assert!(matches("a?c", kind, "a\u{e9}c"));
            assert!(!matches("a?c", kind, "ac"));
            assert!(!matches("a?c", kind, "abbc"));
        }
    }

    #[test]
    fn a_glob_too_large_to_compile_says_so_and_keeps_its_cause() {
        // Comfortably past the regular expression engine's default size limit, which 2^18
        // characters already exceed.
        const TOO_LARGE: usize = 1 << 19;
        let glob = Glob::literal("\u{e9}".repeat(TOO_LARGE));
        let err = Matcher::new(&glob, Kind::Value, false).expect_err("too large");
        let message = err.to_string();
        assert!(
            message.starts_with("this glob is too large to match: "),
            "{message}"
        );
        let cause = std::error::Error::source(&err).expect("the build error is kept");
        assert!(message.ends_with(&cause.to_string()), "{message}");
    }

    proptest! {
        /// Literal text matches exactly itself, whatever it holds: nothing in it is special, to
        /// the glob or to the regular expression it is compiled to.
        #[test]
        fn literal_text_matches_exactly_itself(text in "\\PC{0,12}", other in "\\PC{0,12}") {
            for kind in [Kind::Path, Kind::Value] {
                let matcher = Matcher::new(&Glob::literal(text.clone()), kind, false).unwrap();
                prop_assert!(matcher.is_match(&text));
                prop_assert_eq!(matcher.is_match(&other), other == text);
            }
        }

        /// A star matches any run within a segment, and none across one.
        #[test]
        fn a_star_matches_exactly_the_runs_within_a_segment(run in "[a-z/\n]{0,8}") {
            let mut glob = Glob::literal("<");
            glob.push(GlobPart::Star);
            glob.push_literal(">".into());
            let value = format!("<{run}>");
            let path = Matcher::new(&glob, Kind::Path, false).unwrap();
            prop_assert_eq!(path.is_match(&value), !run.contains('/'));
            let string = Matcher::new(&glob, Kind::Value, false).unwrap();
            prop_assert_eq!(string.is_match(&value), !run.contains('\n'));
        }
    }
}
