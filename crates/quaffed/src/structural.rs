// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Finding a structural pattern in one Python file's text.
//!
//! The file is parsed into an arena (`quaffed-python`), the pattern's shape is looked for in it
//! (`quaffed-syntax`), and each match's byte range is turned into the line and column an
//! editor shows, counted as a textual search counts them.

use std::collections::BTreeMap;
use std::ops::Range;

use quaffed_python::{ParseError, parse_module};
use quaffed_syntax::{Pattern, find};

use crate::search::{Match, Position};

/// One structural match: where it is, and the code it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its first and last character.
    pub at: Match,
    /// The code it covers, as written.
    pub text: String,
}

/// Why a Python file could not be parsed: where its first error is, and what the parser said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unparsed {
    /// Where the error starts.
    pub at: Position,
    /// What the parser said, verbatim.
    pub message: String,
}

/// Every match of each of `patterns` in `text`, a Python file's decoded text, in source order.
///
/// # Errors
///
/// Returns [`Unparsed`] if the text is not Python, saying where and why; a file that does not
/// parse is searched for nothing.
pub fn search(text: &str, patterns: &[&Pattern]) -> Result<Vec<Vec<Found>>, Unparsed> {
    let arena = parse_module(text).map_err(|err| unparsed(text, err))?;
    let ranges: Vec<Vec<Range<usize>>> = patterns
        .iter()
        .map(|pattern| find(pattern, &arena).into_iter().map(|m| m.range).collect())
        .collect();
    // Every first and last character, placed in one walk of the text.
    let wanted: Vec<usize> = ranges
        .iter()
        .flatten()
        .flat_map(|range| [range.start, last_char(text, range)])
        .collect();
    let placed = positions(text, &wanted);
    Ok(ranges
        .into_iter()
        .map(|in_pattern| {
            in_pattern
                .into_iter()
                .map(|range| Found {
                    at: Match {
                        start: placed[&range.start],
                        end: placed[&last_char(text, &range)],
                    },
                    text: text[range].to_owned(),
                })
                .collect()
        })
        .collect())
}

/// Why `text` did not parse, placed in it: where Ruff's error starts, or the start of the
/// file for text too long to parse at all.
fn unparsed(text: &str, err: ParseError) -> Unparsed {
    match err {
        ParseError::Invalid { message, offset } => Unparsed {
            at: positions(text, &[offset])[&offset],
            message,
        },
        ParseError::TooLong(err) => Unparsed {
            at: Position { line: 1, column: 1 },
            message: err.to_string(),
        },
    }
}

/// Where the last character of `range` starts; the start itself for an empty range.
fn last_char(text: &str, range: &Range<usize>) -> usize {
    text[range.clone()]
        .char_indices()
        .last()
        .map_or(range.start, |(i, _)| range.start + i)
}

/// The position of each of `offsets` in `text`, each the start of a character or the end of the
/// text, by one walk that counts lines and columns as a textual search does: LF, CRLF and a
/// lone CR each end a line, the LF of a CRLF on the CR's line, and a column is a character.
fn positions(text: &str, offsets: &[usize]) -> BTreeMap<usize, Position> {
    let mut wanted: Vec<usize> = offsets.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    let mut placed = BTreeMap::new();
    let mut next = wanted.iter().peekable();
    let mut line = 1;
    let mut column = 1;
    let mut previous = None;
    for (offset, c) in text.char_indices() {
        if previous == Some('\r') && c != '\n' {
            line += 1;
            column = 1;
        }
        while next.next_if(|&&o| o <= offset).is_some() {
            placed.insert(offset, Position { line, column });
        }
        if next.peek().is_none() {
            return placed;
        }
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        previous = Some(c);
    }
    // An offset at the end of the text, such as an error at the end of the file.
    if previous == Some('\r') {
        line += 1;
        column = 1;
    }
    for &offset in next {
        placed.insert(offset, Position { line, column });
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;
    use quaffed_python::compile_pattern;

    fn pos(line: usize, column: usize) -> Position {
        Position { line, column }
    }

    #[test]
    fn a_match_is_placed_by_its_first_and_last_character() {
        let pattern = compile_pattern("connect($a)").unwrap();
        // On an attribute, `client.connect(host)` is another shape, so only the bare call matches.
        let text = "client.connect(x)\nif ready: connect(host)\n";
        let found = search(text, &[&pattern]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0],
            [Found {
                at: Match {
                    start: pos(2, 11),
                    end: pos(2, 23)
                },
                text: "connect(host)".into()
            }]
        );
    }

    #[test]
    fn lines_and_columns_count_as_a_textual_search_counts_them() {
        // A lone CR and a CRLF each end a line, and a column is a character, not a byte.
        let text = "a = 1\rb = 'é'\r\nc = f(é)\n";
        let placed = positions(text, &[0, 6, 15, 16, 21]);
        assert_eq!(placed[&0], pos(1, 1));
        assert_eq!(placed[&6], pos(2, 1));
        // The LF of the CRLF is on the CR's line.
        assert_eq!(placed[&15], pos(2, 9));
        assert_eq!(placed[&16], pos(3, 1));
        assert_eq!(placed[&21], pos(3, 6));
        // The end of the text, after a final CR.
        assert_eq!(positions("a\r", &[2])[&2], pos(2, 1));
    }

    #[test]
    fn a_match_that_spans_lines_ends_on_its_last_line() {
        let pattern = compile_pattern("f($a, $b)").unwrap();
        let text = "f(\n    1,\n    2,\n)\n";
        let found = search(text, &[&pattern]).unwrap();
        assert_eq!(
            found[0][0].at,
            Match {
                start: pos(1, 1),
                end: pos(4, 1)
            }
        );
    }

    #[test]
    fn a_file_that_does_not_parse_says_where_and_why() {
        let pattern = compile_pattern("f($a)").unwrap();
        let err = search("x = 1\ndef (:\n", &[&pattern]).unwrap_err();
        assert_eq!(err.at, pos(2, 5));
        assert_eq!(err.message, "Expected an identifier");
    }

    #[test]
    fn text_too_long_to_parse_is_placed_at_the_start() {
        let err = ParseError::TooLong(quaffed_syntax::TooLong { len: 5_000_000_000 });
        let found = unparsed("x", err);
        assert_eq!(found.at, pos(1, 1));
        assert!(found.message.starts_with("the text is 5000000000 bytes"));
    }

    #[test]
    fn matches_inside_matches_are_each_placed() {
        let pattern = compile_pattern("f($x)").unwrap();
        let found = search("f(f(1))\n", &[&pattern]).unwrap();
        let texts: Vec<&str> = found[0].iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["f(f(1))", "f(1)"]);
        assert_eq!(
            found[0][1].at,
            Match {
                start: pos(1, 3),
                end: pos(1, 6)
            }
        );
    }
}
