// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Finding a textual query in one file's bytes.

use crate::encoding::{self, Content, Encoding};

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

/// What searching one file found.  A file that was not searched says why, so that the run can
/// report it rather than count it as a file with no matches.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Searched; every match, in order.  Possibly none.
    Searched(Vec<Match>),
    /// Not searched: binary, by git's test.
    Binary,
    /// Not searched: in an unknown 8-bit encoding, where a non-ASCII query has no honest
    /// spelling.
    UnknownEncoding,
}

/// Finds every occurrence of `query` in `bytes`, in the file's own encoding.
///
/// Matches are found left to right and do not overlap, so `aa` occurs once in `aaa`.  The query
/// is literal: nothing in it is special.
pub fn search(bytes: &[u8], query: &str) -> Outcome {
    let (encoding, body) = match encoding::classify(bytes) {
        Content::Binary => return Outcome::Binary,
        Content::Text { encoding, body } => (encoding, body),
    };
    let Some(needle) = encoding.encode(query) else {
        return Outcome::UnknownEncoding;
    };
    let text = &bytes[body..];
    let offsets = occurrences(text, &needle, encoding.unit());
    if offsets.is_empty() {
        return Outcome::Searched(Vec::new());
    }
    Outcome::Searched(positions(text, encoding, &offsets, needle.len()))
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
fn positions(text: &[u8], encoding: Encoding, offsets: &[usize], len: usize) -> Vec<Match> {
    let chars = encoding.chars(text);
    // The line and column of every character, so a match's last character can be found by its
    // offset whichever way the encoding is laid out.
    let mut placed = Vec::with_capacity(chars.len());
    let mut line = 1;
    let mut column = 1;
    let mut previous = None;
    for &(offset, c) in &chars {
        // The LF of a CRLF belongs to the line its CR ended, one column on.
        if previous == Some('\r') && c != '\n' {
            line += 1;
            column = 1;
        }
        placed.push((offset, Position { line, column }));
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        previous = Some(c);
    }
    let at = |offset: usize| -> Position {
        // The character containing `offset`: the last one starting at or before it.
        let i = placed.partition_point(|(start, _)| *start <= offset) - 1;
        placed[i].1
    };
    offsets
        .iter()
        .map(|&start| Match {
            start: at(start),
            end: at(start + len - 1),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, column: usize) -> Position {
        Position { line, column }
    }

    fn found(bytes: &[u8], query: &str) -> Vec<(Position, Position)> {
        match search(bytes, query) {
            Outcome::Searched(matches) => matches.iter().map(|m| (m.start, m.end)).collect(),
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
        assert_eq!(search(b"caf\xE9", "café"), Outcome::UnknownEncoding);
    }

    #[test]
    fn a_binary_file_is_not_searched() {
        assert_eq!(search(b"TODO\0", "TODO"), Outcome::Binary);
    }

    #[test]
    fn a_utf8_byte_order_mark_is_not_a_column() {
        assert_eq!(
            found(b"\xEF\xBB\xBFTODO", "TODO"),
            vec![(pos(1, 1), pos(1, 4))]
        );
    }
}
