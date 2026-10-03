// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Finding a textual query in one file's bytes.

use crate::encoding::Encoding;

/// A line and column, both counted from 1, the column in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// The line, from 1.  LF, CRLF and a lone CR each end one.
    pub line: usize,
    /// The column, from 1, in characters of the file's encoding.
    pub column: usize,
}

/// One match: where it starts and where its last character is, both inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// The position of the match's first character.
    pub start: Position,
    /// The position of the match's last character.
    pub end: Position,
}

/// Finds every occurrence of `query` in `text`, the body of a file in `encoding` - after any
/// byte-order mark - by encoding the query the way the file is.
///
/// Matches are found left to right and do not overlap, so `aa` occurs once in `aaa`.  The query
/// is literal: nothing in it is special.
///
/// An empty query matches nothing: every position would otherwise be an occurrence of it, and
/// the command line refuses one before it gets here.
///
/// Returns `None` when the query has no honest spelling in the encoding - a non-ASCII query in
/// an unknown 8-bit encoding - so that the caller reports the file as not searched rather than
/// as holding no matches.
pub fn search(text: &[u8], encoding: Encoding, query: &str) -> Option<Vec<Match>> {
    let needle = encoding.encode(query)?;
    if needle.is_empty() {
        return Some(Vec::new());
    }
    let offsets = occurrences(text, &needle, encoding.unit());
    if offsets.is_empty() {
        return Some(Vec::new());
    }
    Some(positions(text, encoding, &offsets, needle.len()))
}

/// Byte offsets of each non-overlapping occurrence of `needle`, starting on a code unit.
///
/// A byte match that straddles two code units is not a match of the text, so the search moves
/// on by one byte and tries again rather than skipping past it.
fn occurrences(text: &[u8], needle: &[u8], unit: usize) -> Vec<usize> {
    let finder = memchr::memmem::Finder::new(needle);
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(i) = finder.find(&text[from..]) {
        let at = from + i;
        if at % unit == 0 {
            found.push(at);
            from = at + needle.len();
        } else {
            from = at + 1;
        }
    }
    found
}

/// Turns each match's byte range into positions, in one pass over the file's characters.
///
/// Only the current line and column are kept, never a table of every character, so the memory
/// a search needs beyond the file's own bytes does not grow with the file.  The walk stops at
/// the last match's last character.
fn positions(text: &[u8], encoding: Encoding, offsets: &[usize], len: usize) -> Vec<Match> {
    // Each match's first and last byte, in order: matches do not overlap, so starts and ends
    // interleave.
    let targets: Vec<usize> = offsets
        .iter()
        .flat_map(|&start| [start, start + len - 1])
        .collect();
    let mut placed = Vec::with_capacity(targets.len());
    let mut line = 1;
    let mut column = 1;
    let mut previous: Option<(char, Position)> = None;
    encoding.each_char(text, |offset, c| {
        // The LF of a CRLF belongs to the line its CR ended, one column on.
        if previous.is_some_and(|(p, _)| p == '\r') && c != '\n' {
            line += 1;
            column = 1;
        }
        let here = Position { line, column };
        // A target before this character lies in the previous one: the character containing a
        // byte is the last one starting at or before it.
        while let Some(&target) = targets.get(placed.len()) {
            if target >= offset {
                break;
            }
            placed.push(
                previous
                    .expect("a target lies at or after the first character")
                    .1,
            );
        }
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        previous = Some((c, here));
        // Carry on until every target is placed; the rest of the file is not walked.
        placed.len() < targets.len()
    });
    // Targets in the last character visited.
    placed.resize(
        targets.len(),
        previous.expect("a match has at least one character").1,
    );
    placed
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[start, end]| Match { start, end })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, column: usize) -> Position {
        Position { line, column }
    }

    use crate::encoding::{Content, classify};

    /// What the run makes of a whole file's bytes: classified first, then searched.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Binary,
        NoSpelling,
        Found(Vec<Match>),
    }

    fn outcome(bytes: &[u8], query: &str) -> Outcome {
        match classify(bytes) {
            Content::Binary => Outcome::Binary,
            Content::Text { encoding, body } => match search(&bytes[body..], encoding, query) {
                None => Outcome::NoSpelling,
                Some(matches) => Outcome::Found(matches),
            },
        }
    }

    fn found(bytes: &[u8], query: &str) -> Vec<(Position, Position)> {
        match outcome(bytes, query) {
            Outcome::Found(matches) => matches.iter().map(|m| (m.start, m.end)).collect(),
            other => panic!("expected a search, got {other:?}"),
        }
    }

    #[test]
    fn matches_are_found_left_to_right_without_overlapping() {
        assert_eq!(found(b"aaa", "aa"), vec![(pos(1, 1), pos(1, 2))]);
        assert_eq!(
            found(b"x TODO y TODO\n", "TODO"),
            vec![(pos(1, 3), pos(1, 6)), (pos(1, 10), pos(1, 13))]
        );
        assert_eq!(found(b"nothing", "TODO"), vec![]);
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_looping() {
        assert_eq!(found(b"abc", ""), vec![]);
    }

    #[test]
    fn the_query_is_literal() {
        assert_eq!(found(b"a.c abc", "a.c"), vec![(pos(1, 1), pos(1, 3))]);
        assert_eq!(found(b"^x$ x", "^x$"), vec![(pos(1, 1), pos(1, 3))]);
    }

    #[test]
    fn lines_end_at_lf_crlf_and_a_lone_cr() {
        assert_eq!(
            found(b"a\nb\r\nc\rTODO", "TODO"),
            vec![(pos(4, 1), pos(4, 4))]
        );
    }

    #[test]
    fn a_match_spanning_lines_ends_on_its_last_line() {
        assert_eq!(
            found(b"x one\ntwo y", "one\ntwo"),
            vec![(pos(1, 3), pos(2, 3))]
        );
        // The line ending is the match's last character: it sits at the end of line 1.
        assert_eq!(found(b"one\ntwo", "one\n"), vec![(pos(1, 1), pos(1, 4))]);
        assert_eq!(found(b"one\r\ntwo", "\r\n"), vec![(pos(1, 4), pos(1, 5))]);
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        assert_eq!(
            found("é TODO".as_bytes(), "TODO"),
            vec![(pos(1, 3), pos(1, 6))]
        );
        assert_eq!(found("aé".as_bytes(), "é"), vec![(pos(1, 2), pos(1, 2))]);
    }

    #[test]
    fn utf16_is_searched_in_utf16() {
        let mut file = vec![0xFF, 0xFE];
        file.extend("é TODO\nTODO".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            found(&file, "TODO"),
            vec![(pos(1, 3), pos(1, 6)), (pos(2, 1), pos(2, 4))]
        );
    }

    #[test]
    fn a_byte_match_across_code_units_is_not_a_match() {
        // In UTF-16LE, U+4100 then U+0000 is 00 41 00 00, holding the bytes 41 00 - an "A" -
        // at an odd offset, across two units.  Only the real "A" after them may match.
        let mut file = vec![0xFF, 0xFE];
        file.extend("\u{4100}\u{0}A".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(found(&file, "A"), vec![(pos(1, 3), pos(1, 3))]);
    }

    #[test]
    fn an_unknown_8bit_file_is_searched_for_ascii_and_not_otherwise() {
        assert_eq!(found(b"caf\xE9 TODO", "TODO"), vec![(pos(1, 6), pos(1, 9))]);
        assert_eq!(outcome(b"caf\xE9", "café"), Outcome::NoSpelling);
    }

    #[test]
    fn a_binary_file_is_not_searched() {
        assert_eq!(outcome(b"TODO\0", "TODO"), Outcome::Binary);
    }

    #[test]
    fn an_invalid_byte_in_declared_utf8_is_one_column_and_the_rest_are_characters() {
        assert_eq!(
            found(b"\xEF\xBB\xBF\xC3\xA9 TODO\n\xE9", "TODO"),
            vec![(pos(1, 3), pos(1, 6))]
        );
    }

    #[test]
    fn positions_hold_at_the_very_end_of_a_file() {
        assert_eq!(found(b"abc\nTODO", "TODO"), vec![(pos(2, 1), pos(2, 4))]);
        assert_eq!(found(b"x\r", "\r"), vec![(pos(1, 2), pos(1, 2))]);
    }

    #[test]
    fn a_utf8_byte_order_mark_is_not_a_column() {
        assert_eq!(
            found(b"\xEF\xBB\xBFTODO", "TODO"),
            vec![(pos(1, 1), pos(1, 4))]
        );
    }
}
