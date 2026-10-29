// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Finding a textual query in one file's bytes.

use quaffed_representation::program::{TextPart, TextPattern};

use quaffed_encoding::{Encoding, Spelling};

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

/// Finds every match of `pattern` in `text`, the body of a file in `encoding` - after any
/// byte-order mark - by encoding the pattern's text the way the file is.
///
/// Matches are found left to right and do not overlap, so `aa` occurs once in `aaa`.  Literal
/// text is literal: nothing in it is special.  A match starts and ends on a character: in a
/// multi-byte encoding such as Shift-JIS, where a character's second byte can be an ASCII byte,
/// bytes that match inside a character are not a match.  A line break matches LF, CRLF or a lone CR,
/// whichever the file has there; a start anchor holds at the start of the file or after a line
/// ending, and an end anchor at the end of the file or before one.
///
/// An empty pattern matches nothing: every position would otherwise be a match, and the
/// command line and the parser both refuse one before it gets here.
///
/// Returns `None` when the pattern's text has no one spelling in the encoding, so that the
/// caller reports the file as not searched rather than as holding no matches: non-ASCII text in
/// an unknown 8-bit encoding, or a character an encoding spells more than one way.  A
/// character the encoding cannot hold at all is no matches, since the text cannot occur there.
pub fn search(text: &[u8], encoding: Encoding, pattern: &TextPattern) -> Option<Vec<Match>> {
    let compiled = match Compiled::new(pattern, encoding) {
        Ok(compiled) => compiled,
        Err(Unspelled::Impossible) => return Some(Vec::new()),
        Err(Unspelled::Ambiguous) => return None,
    };
    if compiled.parts.is_empty() {
        return Some(Vec::new());
    }
    let starts = Starts::of(text, encoding);
    let ranges = compiled.ranges(text, &starts);
    if ranges.is_empty() {
        return Some(Vec::new());
    }
    Some(positions(text, encoding, &ranges))
}

/// What replacing a pattern in one file's bytes gives.
#[derive(Debug, PartialEq, Eq)]
pub enum Replaced {
    /// The pattern has no one spelling in the encoding, so the file was not searched for it, as
    /// [`search`] returns `None`.
    NotSearched,
    /// The matches, where they were before the edit, and the file's new bytes: `None` when there
    /// were none, so nothing changes.
    Edited {
        /// Each match replaced, in order.
        matches: Vec<Match>,
        /// The text with every match replaced, or `None` with no match.
        text: Option<Vec<u8>>,
    },
    /// The pattern matched, but the encoding cannot write the replacement: the text named holds
    /// a character it cannot hold, or spells more than one way.
    Unwritable {
        /// The matches, which still count, since the file holds them.
        matches: Vec<Match>,
        /// The replacement's text that cannot be written.
        text: String,
        /// Whether the encoding spells it more than one way, rather than not at all.
        ambiguous: bool,
    },
}

/// Replaces every match of `pattern` in `text`, the body of a file in `encoding` after any
/// byte-order mark, with `with`, finding matches as [`search`] does.
///
/// Every byte outside the matches is left as it was.  The replacement is spelled in the file's
/// encoding; a line break in it is written as the file's own line ending: the one ending the line
/// the match starts on, or where that line has none, the last one before it, or LF in a file with
/// none.
pub fn replace(
    text: &[u8],
    encoding: Encoding,
    pattern: &TextPattern,
    with: &TextPattern,
) -> Replaced {
    let compiled = match Compiled::new(pattern, encoding) {
        Ok(compiled) => compiled,
        Err(Unspelled::Impossible) => {
            return Replaced::Edited {
                matches: Vec::new(),
                text: None,
            };
        }
        Err(Unspelled::Ambiguous) => return Replaced::NotSearched,
    };
    if compiled.parts.is_empty() {
        return Replaced::Edited {
            matches: Vec::new(),
            text: None,
        };
    }
    let starts = Starts::of(text, encoding);
    let ranges = compiled.ranges(text, &starts);
    if ranges.is_empty() {
        return Replaced::Edited {
            matches: Vec::new(),
            text: None,
        };
    }
    let matches = positions(text, encoding, &ranges);
    // Each literal of the replacement spelled once; the line breaks depend on where each match is.
    let mut spelled = Vec::new();
    for part in with.parts() {
        spelled.push(match part {
            TextPart::Literal(literal) => match encoding.encode(literal) {
                Spelling::Bytes(bytes) => Some(bytes),
                Spelling::Impossible | Spelling::Ambiguous => {
                    return Replaced::Unwritable {
                        matches,
                        text: literal.clone(),
                        ambiguous: matches!(encoding.encode(literal), Spelling::Ambiguous),
                    };
                }
            },
            // A line break is the file's own, found per match.
            _ => None,
        });
    }
    let breaks = spelled.iter().any(Option::is_none);
    let mut endings = Endings::default();
    let mut edited = Vec::with_capacity(text.len());
    let mut from = 0;
    for &(start, end) in &ranges {
        edited.extend_from_slice(&text[from..start]);
        // Found once a match, and only for a replacement that writes a line break.
        let ending = if breaks {
            endings.for_match(&compiled, text, start)
        } else {
            &[][..]
        };
        for bytes in &spelled {
            edited.extend_from_slice(bytes.as_deref().unwrap_or(ending));
        }
        from = end;
    }
    edited.extend_from_slice(&text[from..]);
    Replaced::Edited {
        matches,
        text: Some(edited),
    }
}

/// The line endings a replace has found, remembered as the matches move forward through the file,
/// so that a long last line with no ending is not scanned once a match.
#[derive(Default)]
struct Endings {
    /// The next line ending at or after the last match, where it starts, and its bytes; `None`
    /// before the first lookup.  A start of `usize::MAX` means there is none.
    next: Option<(usize, Vec<u8>)>,
    /// The ending a match on the last line, which has none, takes: found once.
    last_line: Option<Vec<u8>>,
}

impl Endings {
    /// The line ending a line break written at `at` takes, `at` never before the last one asked.
    fn for_match<'e>(&'e mut self, compiled: &Compiled, text: &[u8], at: usize) -> &'e [u8] {
        let stale = self.next.as_ref().is_none_or(|(start, _)| *start < at);
        if stale {
            self.next = Some(
                compiled
                    .next_ending(text, at)
                    .unwrap_or((usize::MAX, Vec::new())),
            );
        }
        match &self.next {
            Some((start, bytes)) if *start != usize::MAX => bytes,
            _ => self
                .last_line
                .get_or_insert_with(|| compiled.ending_before(text, at)),
        }
    }
}

/// Where a file's characters start, for an encoding where that is not known from a byte's offset
/// alone.
enum Starts {
    /// Every code unit starts a character, as far as a match is concerned.
    Units(usize),
    /// One bit a byte, set where a character starts, and at the end.
    Bits(Vec<u64>),
}

impl Starts {
    fn of(text: &[u8], encoding: Encoding) -> Starts {
        if !matches!(encoding, Encoding::MultiByte(_)) {
            return Starts::Units(encoding.unit());
        }
        // One bit a byte rather than a list of offsets, so the memory a search needs beyond
        // the file's own bytes is an eighth of them.
        let mut bits = vec![0u64; text.len() / 64 + 1];
        let mut set = |at: usize| bits[at / 64] |= 1 << (at % 64);
        encoding.each_char(text, |at, _| {
            set(at);
            true
        });
        set(text.len());
        Starts::Bits(bits)
    }

    /// Whether a character starts at byte `at`, or `at` is the end.
    fn at(&self, at: usize) -> bool {
        match self {
            Starts::Units(unit) => at.is_multiple_of(*unit),
            Starts::Bits(bits) => bits[at / 64] & (1 << (at % 64)) != 0,
        }
    }
}

/// One part of a pattern, spelled in the file's encoding.
enum Part {
    /// These exact bytes.
    Literal(Vec<u8>),
    /// A line ending: CRLF, LF or CR.
    LineBreak,
}

/// Why a pattern has no one run of bytes in an encoding.
enum Unspelled {
    /// A character in it is one the encoding cannot hold.
    Impossible,
    /// The encoding is unknown, or spells a character in it more than one way.
    Ambiguous,
}

/// A pattern spelled in one file's encoding.
struct Compiled {
    parts: Vec<Part>,
    start_anchor: bool,
    end_anchor: bool,
    cr: Vec<u8>,
    lf: Vec<u8>,
    unit: usize,
}

impl Compiled {
    /// Spells `pattern` in `encoding`.  A literal that cannot occur settles it whatever the
    /// others are, so one that is ambiguous does not stop the rest being spelled.
    fn new(pattern: &TextPattern, encoding: Encoding) -> Result<Self, Unspelled> {
        let mut parts = Vec::new();
        let mut ambiguous = false;
        for part in pattern.parts() {
            parts.push(match part {
                TextPart::Literal(text) => match encoding.encode(text) {
                    Spelling::Bytes(bytes) => Part::Literal(bytes),
                    Spelling::Impossible => return Err(Unspelled::Impossible),
                    Spelling::Ambiguous => {
                        ambiguous = true;
                        continue;
                    }
                },
                TextPart::LineBreak => Part::LineBreak,
                // A part this build cannot match is never produced for it: the parser refuses
                // textual capture as an unknown construct.
                _ => return Err(Unspelled::Ambiguous),
            });
        }
        // Every encoding spells CR and LF one way, which the encoding crate's tests hold every
        // codec to, so an ambiguous line ending would be a codec this search was never given.
        let (Spelling::Bytes(cr), Spelling::Bytes(lf)) =
            (encoding.encode("\r"), encoding.encode("\n"))
        else {
            return Err(Unspelled::Ambiguous);
        };
        if ambiguous {
            return Err(Unspelled::Ambiguous);
        }
        Ok(Compiled {
            parts,
            start_anchor: pattern.start_anchor,
            end_anchor: pattern.end_anchor,
            cr,
            lf,
            unit: encoding.unit(),
        })
    }

    /// The length of the line ending at the start of `rest`, if there is one: CRLF before CR,
    /// so a CRLF file's break is one break, not two.
    fn line_ending(&self, rest: &[u8]) -> Option<usize> {
        if rest.starts_with(&self.cr) {
            if rest[self.cr.len()..].starts_with(&self.lf) {
                return Some(self.cr.len() + self.lf.len());
            }
            return Some(self.cr.len());
        }
        rest.starts_with(&self.lf).then_some(self.lf.len())
    }

    /// The first line ending at or after byte `at`: where it starts, and its bytes.  This is the
    /// one ending the line `at` is on.
    ///
    /// A line ending's bytes are never part of another character, in any encoding quaff reads:
    /// in UTF-16 and UTF-32 the search steps by code unit, and in the multi-byte encodings no
    /// trailing byte is CR or LF.
    fn next_ending(&self, text: &[u8], at: usize) -> Option<(usize, Vec<u8>)> {
        let mut position = at - at % self.unit;
        while position < text.len() {
            if let Some(length) = self.line_ending(&text[position..]) {
                return Some((position, text[position..position + length].to_vec()));
            }
            position += self.unit;
        }
        None
    }

    /// The line ending a line break written on the last line, which has none, takes: the last
    /// one before `at`, or LF in a file with none.
    fn ending_before(&self, text: &[u8], at: usize) -> Vec<u8> {
        let mut position = at - at % self.unit;
        while position >= self.unit {
            position -= self.unit;
            if let Some(length) = self.line_ending(&text[position..]) {
                // The LF of a CRLF is found first going back; its CR before it is the ending.
                if text[position..].starts_with(&self.lf)
                    && position >= self.unit
                    && text[position - self.unit..].starts_with(&self.cr)
                {
                    return text[position - self.unit..position + length].to_vec();
                }
                return text[position..position + length].to_vec();
            }
        }
        self.lf.clone()
    }

    /// Whether the code unit just before `at` ends a line.  Between the CR and the LF of a CRLF
    /// is inside one line ending, not after it.
    fn after_line_ending(&self, text: &[u8], at: usize) -> bool {
        at == 0 || {
            let previous = &text[at - self.unit..at];
            previous == self.lf.as_slice()
                || (previous == self.cr.as_slice() && !self.inside_crlf(text, at))
        }
    }

    /// Whether `at` falls between the CR and the LF of a CRLF.
    fn inside_crlf(&self, text: &[u8], at: usize) -> bool {
        at >= self.unit && text[at - self.unit..at] == *self.cr && text[at..].starts_with(&self.lf)
    }

    /// Where the pattern matches if it starts at `at`: the byte after its end.
    fn match_at(&self, text: &[u8], at: usize) -> Option<usize> {
        let mut position = at;
        for part in &self.parts {
            let rest = &text[position..];
            position += match part {
                Part::Literal(bytes) => rest.starts_with(bytes).then_some(bytes.len())?,
                Part::LineBreak => self.line_ending(rest)?,
            };
        }
        // A match that ends on the CR of a CRLF ends inside the line ending, not before it.
        let end_holds = !self.end_anchor
            || position == text.len()
            || (self.line_ending(&text[position..]).is_some() && !self.inside_crlf(text, position));
        end_holds.then_some(position)
    }

    /// Byte ranges of every non-overlapping match, left to right, each starting and ending on a
    /// character.
    ///
    /// Candidates are found by the first literal's bytes where the pattern starts with one -
    /// a byte match straddling two code units, or starting inside a multi-byte character, is
    /// not a match of the text - and otherwise at every code unit.
    fn ranges(&self, text: &[u8], starts: &Starts) -> Vec<(usize, usize)> {
        let first = match self.parts.first() {
            Some(Part::Literal(bytes)) => Some(memchr::memmem::Finder::new(bytes)),
            _ => None,
        };
        let mut found = Vec::new();
        let mut from = 0;
        while from < text.len() {
            let candidate = match &first {
                Some(finder) => match finder.find(&text[from..]) {
                    Some(i) => from + i,
                    None => break,
                },
                None => from,
            };
            let usable = starts.at(candidate)
                && (!self.start_anchor || self.after_line_ending(text, candidate));
            match usable
                .then(|| self.match_at(text, candidate))
                .flatten()
                .filter(|&end| starts.at(end))
            {
                Some(end) => {
                    found.push((candidate, end));
                    from = end;
                }
                None => from = candidate + 1,
            }
        }
        found
    }
}

/// Turns each match's byte range into positions, in one pass over the file's characters.
///
/// Only the current line and column are kept, never a table of every character, so the memory
/// a search needs beyond the file's own bytes does not grow with the file.  The walk stops at
/// the last match's last character.
pub(crate) fn positions(text: &[u8], encoding: Encoding, ranges: &[(usize, usize)]) -> Vec<Match> {
    // Each match's first and last byte, in order: matches do not overlap, so starts and ends
    // interleave.
    let targets: Vec<usize> = ranges
        .iter()
        .flat_map(|&(start, end)| [start, end - 1])
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

    use std::path::Path;

    use crate::contents::Contents;
    use crate::editorconfig::EditorConfig;
    use crate::resolve::{Declarations, File, Resolution, resolve};

    /// The encoding the run decides for a file of `bytes` with nothing declaring one, and where
    /// its text starts: `None` for binary.  A file of unknown encoding is searched as the run
    /// searches it, as unknown 8-bit text.
    fn decided(bytes: &[u8]) -> Option<(Encoding, usize)> {
        let nowhere = Path::new("/nonexistent-quaffed-search-test");
        let mut declarations = Declarations {
            workspace: None,
            editorconfig: EditorConfig::new(nowhere, &Contents::default()),
            cwd: nowhere.to_path_buf(),
            contents: Contents::default(),
        };
        let file = File {
            path: &nowhere.join("file.txt"),
            bytes,
        };
        match resolve(&file, &mut declarations).expect("nothing to read") {
            Resolution::Binary => None,
            Resolution::Known { encoding, body, .. } => Some((encoding, body)),
            Resolution::Unknown(_) => Some((Encoding::Unknown8Bit, 0)),
            Resolution::Refused(refusal) => panic!("only Python is refused: {refusal:?}"),
        }
    }

    /// What the run makes of a whole file's bytes with nothing declaring their encoding: its
    /// encoding decided first, then searched.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Binary,
        NoSpelling,
        Found(Vec<Match>),
    }

    fn outcome(bytes: &[u8], query: &str) -> Outcome {
        match decided(bytes) {
            None => Outcome::Binary,
            Some((encoding, body)) => {
                match search(&bytes[body..], encoding, &TextPattern::literal(query)) {
                    None => Outcome::NoSpelling,
                    Some(matches) => Outcome::Found(matches),
                }
            }
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

    /// A pattern from pieces: `|` is a line break, and `^` and `$` at the ends are anchors.
    fn pattern(written: &str) -> TextPattern {
        let mut pattern = TextPattern::default();
        let mut body = written;
        if let Some(rest) = body.strip_prefix('^') {
            pattern.start_anchor = true;
            body = rest;
        }
        if let Some(rest) = body.strip_suffix('$') {
            pattern.end_anchor = true;
            body = rest;
        }
        for (i, piece) in body.split('|').enumerate() {
            if i > 0 {
                pattern.push_line_break();
            }
            pattern.push_literal(piece.to_owned());
        }
        pattern
    }

    fn found_pattern(bytes: &[u8], written: &str) -> Vec<(Position, Position)> {
        let Some((encoding, body)) = decided(bytes) else {
            panic!("binary")
        };
        search(&bytes[body..], encoding, &pattern(written))
            .expect("a spelling")
            .iter()
            .map(|m| (m.start, m.end))
            .collect()
    }

    #[test]
    fn a_line_break_matches_lf_crlf_and_cr_alike() {
        for file in [&b"x one\ntwo"[..], b"x one\r\ntwo", b"x one\rtwo"] {
            assert_eq!(
                found_pattern(file, "one|two"),
                vec![(pos(1, 3), pos(2, 3))],
                "{file:?}"
            );
        }
        assert_eq!(found_pattern(b"one two", "one|two"), vec![]);
    }

    #[test]
    fn a_crlf_is_one_break_not_two() {
        assert_eq!(
            found_pattern(b"a\r\n\r\nb", "a||b"),
            vec![(pos(1, 1), pos(3, 1))]
        );
        assert_eq!(found_pattern(b"a\r\nb", "a||b"), vec![]);
    }

    #[test]
    fn anchors_hold_only_at_line_ends() {
        let file = b"import x\n  import y\nfrom import\r\nimport";
        assert_eq!(
            found_pattern(file, "^import"),
            vec![(pos(1, 1), pos(1, 6)), (pos(4, 1), pos(4, 6))]
        );
        assert_eq!(
            found_pattern(file, "import$"),
            vec![(pos(3, 6), pos(3, 11)), (pos(4, 1), pos(4, 6))]
        );
        assert_eq!(
            found_pattern(file, "^import$"),
            vec![(pos(4, 1), pos(4, 6))]
        );
        assert_eq!(
            found_pattern(b"pass\rpass ", "^pass$"),
            vec![(pos(1, 1), pos(1, 4))]
        );
    }

    #[test]
    fn a_crlf_is_one_line_ending_to_the_anchors() {
        // Between the CR and the LF is inside the line ending, so a pattern answers a CRLF
        // file as it answers the same file with LF endings.
        for file in [&b"a\r\nx"[..], b"a\nx"] {
            assert_eq!(found_pattern(file, "^|x"), vec![], "{file:?}");
            assert_eq!(found_pattern(file, "^\nx"), vec![], "{file:?}");
        }
        // A match that ends on the CR of a CRLF ends inside it, not before one; after a lone
        // CR, the next CR is a line ending of its own.
        assert_eq!(found_pattern(b"a\r\nb", "a\r$"), vec![]);
        assert_eq!(
            found_pattern(b"a\r\r", "a\r$"),
            vec![(pos(1, 1), pos(1, 2))]
        );
        // The same in UTF-16, where a line ending is two-byte code units.
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("a\r\nx".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(found_pattern(&utf16, "^|x"), vec![]);
        assert_eq!(found_pattern(&utf16, "^x"), vec![(pos(2, 1), pos(2, 1))]);
    }

    #[test]
    fn a_pattern_may_start_with_a_line_break() {
        assert_eq!(
            found_pattern(b"a\nb\nb", "|b"),
            vec![(pos(1, 2), pos(2, 1)), (pos(2, 2), pos(3, 1))]
        );
    }

    #[test]
    fn line_breaks_and_anchors_work_in_utf16() {
        let mut file = vec![0xFF, 0xFE];
        file.extend("x\r\nimport y".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            found_pattern(&file, "x|import"),
            vec![(pos(1, 1), pos(2, 6))]
        );
        assert_eq!(
            found_pattern(&file, "^import"),
            vec![(pos(2, 1), pos(2, 6))]
        );
        assert_eq!(found_pattern(&file, "x$"), vec![(pos(1, 1), pos(1, 1))]);
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

    fn found_in(codec: &str, bytes: &[u8], query: &str) -> Option<Vec<(Position, Position)>> {
        let encoding = Encoding::SingleByte(quaffed_encoding::Codec::named(codec).unwrap());
        search(bytes, encoding, &TextPattern::literal(query))
            .map(|matches| matches.iter().map(|m| (m.start, m.end)).collect())
    }

    #[test]
    fn a_single_byte_file_is_searched_in_its_own_bytes_one_column_a_byte() {
        // "x = 'да'" in KOI8-R, then the same on a CRLF line.
        let file = b"x = '\xc4\xc1'\r\ny = '\xc4\xc1'";
        assert_eq!(
            found_in("koi8_r", file, "да"),
            Some(vec![(pos(1, 6), pos(1, 7)), (pos(2, 6), pos(2, 7))])
        );
        // A byte the codec decodes to nothing is one column.
        assert_eq!(
            found_in("cp1252", b"\x81 TODO", "TODO"),
            Some(vec![(pos(1, 3), pos(1, 6))])
        );
    }

    #[test]
    fn text_a_single_byte_encoding_cannot_hold_is_no_matches_and_ambiguous_text_is_unsearched() {
        // KOI8-R has no "é", so it cannot be in the file: an answer, not a skipped file.
        assert_eq!(found_in("koi8_r", b"caf\xc5", "café"), Some(vec![]));
        // mac_arabic spells the space two ways, so "a b" has no one spelling to search for.
        assert_eq!(found_in("mac_arabic", b"a b", "a b"), None);
        assert_eq!(
            found_in("mac_arabic", b"a b", "b"),
            Some(vec![(pos(1, 3), pos(1, 3))])
        );
    }

    fn found_in_multi_byte(codec: &str, bytes: &[u8], query: &str) -> Vec<(Position, Position)> {
        let encoding = Encoding::MultiByte(quaffed_encoding::MultiByteCodec::named(codec).unwrap());
        search(bytes, encoding, &TextPattern::literal(query))
            .expect("a spelling")
            .iter()
            .map(|m| (m.start, m.end))
            .collect()
    }

    #[test]
    fn a_multi_byte_file_is_searched_by_character() {
        // "x = '日本'" then "# 日本" in Shift-JIS: one column a character, not a byte.
        let file = b"x = '\x93\xfa\x96\x7b'\n# \x93\xfa\x96\x7b";
        assert_eq!(
            found_in_multi_byte("shift_jis", file, "日本"),
            vec![(pos(1, 6), pos(1, 7)), (pos(2, 3), pos(2, 4))]
        );
        // "ア" in EUC-JP, after a three-byte character.
        assert_eq!(
            found_in_multi_byte("euc_jp", b"\x8f\xb0\xa1\xa5\xa2", "ア"),
            vec![(pos(1, 2), pos(1, 2))]
        );
    }

    #[test]
    fn bytes_that_match_inside_a_multi_byte_character_are_not_a_match() {
        // "ア" is 83 41 in Shift-JIS: its second byte is an "A".  Only the real "A" matches.
        assert_eq!(
            found_in_multi_byte("shift_jis", b"\x83\x41A", "A"),
            vec![(pos(1, 2), pos(1, 2))]
        );
        // "ソ" is 83 5C: its second byte is a backslash.
        assert_eq!(found_in_multi_byte("shift_jis", b"\x83\x5C", "\\"), vec![]);
    }

    #[test]
    fn a_multi_byte_files_characters_start_where_its_reading_says() {
        // "Aア" then a lone first byte: characters start at 0, 1 and 3, and the end at 4 counts.
        let encoding =
            Encoding::MultiByte(quaffed_encoding::MultiByteCodec::named("shift_jis").unwrap());
        let starts = Starts::of(b"A\x83\x41\x83", encoding);
        let found: Vec<usize> = (0..=4).filter(|&at| starts.at(at)).collect();
        assert_eq!(found, [0, 1, 3, 4]);
        // Any other encoding answers by code unit alone.
        assert!(Starts::of(b"ab", Encoding::Utf16Le).at(2));
        assert!(!Starts::of(b"ab", Encoding::Utf16Le).at(1));
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

    /// `text` with `pattern` replaced by `with`, both written as for `pattern`.
    fn replaced(encoding: Encoding, text: &[u8], pattern: &str, with: &str) -> Replaced {
        super::replace(
            text,
            encoding,
            &self::pattern(pattern),
            &self::pattern(with),
        )
    }

    fn edited(encoding: Encoding, text: &[u8], pattern: &str, with: &str) -> Vec<u8> {
        match replaced(encoding, text, pattern, with) {
            Replaced::Edited {
                text: Some(text), ..
            } => text,
            other => panic!("expected an edit, got {other:?}"),
        }
    }

    #[test]
    fn a_replace_changes_only_the_matched_bytes() {
        assert_eq!(
            edited(Encoding::Utf8, b"a TODO b TODO\n", "TODO", "DONE"),
            b"a DONE b DONE\n"
        );
        // Shorter and longer replacements, and an empty one, which deletes.
        assert_eq!(edited(Encoding::Utf8, b"xaay", "aa", "b"), b"xby");
        assert_eq!(edited(Encoding::Utf8, b"xay", "a", "bbb"), b"xbbby");
        assert_eq!(edited(Encoding::Utf8, b"xay", "a", ""), b"xy");
        // Matches do not overlap, as in a search.
        assert_eq!(edited(Encoding::Utf8, b"aaa", "aa", "b"), b"ba");
    }

    #[test]
    fn no_match_is_no_edit() {
        assert_eq!(
            replaced(Encoding::Utf8, b"nothing", "TODO", "DONE"),
            Replaced::Edited {
                matches: vec![],
                text: None
            }
        );
    }

    #[test]
    fn a_line_break_in_the_replacement_is_the_files_own() {
        // "|" is a line break, in the pattern and the replacement alike.
        assert_eq!(edited(Encoding::Utf8, b"a;b\n", ";", "|"), b"a\nb\n");
        assert_eq!(edited(Encoding::Utf8, b"a;b\r\n", ";", "|"), b"a\r\nb\r\n");
        assert_eq!(edited(Encoding::Utf8, b"a;b\r", ";", "|"), b"a\rb\r");
        // In a mixed file, the ending of the line the match starts on.
        assert_eq!(
            edited(Encoding::Utf8, b"a;b\r\nc;d\n", ";", "|"),
            b"a\r\nb\r\nc\nd\n"
        );
        // A last line with no ending takes the ending before it; a file with none takes LF.
        assert_eq!(
            edited(Encoding::Utf8, b"x\r\na;b", ";", "|"),
            b"x\r\na\r\nb"
        );
        assert_eq!(edited(Encoding::Utf8, b"a;b", ";", "|"), b"a\nb");
        // Several matches on that last line all take the ending before it, and matches on lines
        // with endings each take their own.
        assert_eq!(
            edited(Encoding::Utf8, b"a;b\nx\r\nc;d;e", ";", "|"),
            b"a\nb\nx\r\nc\r\nd\r\ne"
        );
        // A match that spans lines takes the ending of the line it starts on.
        assert_eq!(
            edited(Encoding::Utf8, b"a\r\nb\nc", "a|b", "x|y"),
            b"x\r\ny\nc"
        );
    }

    #[test]
    fn a_line_break_in_utf16_is_written_in_utf16() {
        let utf16 =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        assert_eq!(
            edited(Encoding::Utf16Le, &utf16("a;b\r\n"), ";", "|"),
            utf16("a\r\nb\r\n")
        );
        assert_eq!(
            edited(Encoding::Utf16Le, &utf16("a;b"), ";", "|"),
            utf16("a\nb")
        );
    }

    #[test]
    fn a_multi_byte_file_is_edited_in_its_own_encoding_and_only_at_whole_characters() {
        let sjis =
            Encoding::MultiByte(quaffed_encoding::MultiByteCodec::named("shift_jis").unwrap());
        // "ア" is 83 41: its "A" byte is not a match, so only the real "A" is replaced, and the
        // replacement "日本" is written as Shift-JIS.
        assert_eq!(
            edited(sjis, b"\x83\x41A", "A", "日本"),
            b"\x83\x41\x93\xfa\x96\x7b"
        );
    }

    #[test]
    fn a_replacement_the_encoding_cannot_write_is_refused_with_its_matches() {
        let koi8 = Encoding::SingleByte(quaffed_encoding::Codec::named("koi8_r").unwrap());
        let Replaced::Unwritable {
            matches,
            text,
            ambiguous,
        } = replaced(koi8, b"x = 1", "x", "café")
        else {
            panic!("expected a refusal")
        };
        assert_eq!(matches.len(), 1);
        assert_eq!(text, "café");
        assert!(!ambiguous);
        // mac_arabic spells the space two ways.
        let mac_arabic =
            Encoding::SingleByte(quaffed_encoding::Codec::named("mac_arabic").unwrap());
        assert!(matches!(
            replaced(mac_arabic, b"x", "x", "a b"),
            Replaced::Unwritable {
                ambiguous: true,
                ..
            }
        ));
        // With no match there is nothing to write, so nothing is refused.
        assert_eq!(
            replaced(koi8, b"y", "x", "café"),
            Replaced::Edited {
                matches: vec![],
                text: None
            }
        );
    }

    #[test]
    fn a_pattern_with_no_one_spelling_is_not_searched_for_a_replace() {
        let mac_arabic =
            Encoding::SingleByte(quaffed_encoding::Codec::named("mac_arabic").unwrap());
        assert_eq!(
            replaced(mac_arabic, b"a b", "a b", "x"),
            Replaced::NotSearched
        );
    }

    #[test]
    fn a_utf8_byte_order_mark_is_not_a_column() {
        assert_eq!(
            found(b"\xEF\xBB\xBFTODO", "TODO"),
            vec![(pos(1, 1), pos(1, 4))]
        );
    }
}
