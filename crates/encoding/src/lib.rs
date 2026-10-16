// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What a file's bytes are, as far as quaff can tell without guessing.
//!
//! A textual query is matched in the file's own encoding: it is encoded the way the file is,
//! rather than the file being decoded.  This crate knows what bytes are in a given encoding, and
//! reads the declarations that live in the bytes themselves - a byte-order mark, a Python file's
//! coding line - and whether the bytes are proven UTF-8.  Deciding which declaration applies to a
//! file, including ones kept outside it, is the caller's.  Nothing is inferred from statistics.
//!
//! Python source is read as `CPython` reads it, by the [`python`] module's rules and `CPython`'s
//! own codecs.  `architecture/encoding.md` describes this crate.

// Generated from the oracle's codecs; the generator owns its layout.
#[rustfmt::skip]
mod codecs;
pub mod python;

pub use python::{Codec, MultiByteCodec, Read, Refusal, SourceEncoding};

/// How many leading bytes git examines for a NUL when deciding a file is binary.
///
/// Read from git's `xdiff-interface.c`: `#define FIRST_FEW_BYTES 8000`, and `buffer_is_binary`
/// returns whether `memchr(ptr, 0, size)` finds a NUL within at most that many bytes.
const BINARY_PROBE: usize = 8000;

/// A file's text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8, with or without a byte-order mark.
    Utf8,
    /// UTF-16, little-endian, declared by its byte-order mark.
    Utf16Le,
    /// UTF-16, big-endian, declared by its byte-order mark.
    Utf16Be,
    /// UTF-32, little-endian, declared by its byte-order mark.
    Utf32Le,
    /// UTF-32, big-endian, declared by its byte-order mark.
    Utf32Be,
    /// A single-byte encoding, declared, such as Latin-1 or KOI8-R.
    SingleByte(Codec),
    /// A multi-byte encoding, declared, such as Shift-JIS or EUC-JP.
    MultiByte(MultiByteCodec),
    /// Not proven UTF-8 and nothing declared that quaff reads: some ASCII-compatible encoding,
    /// but which is unknown.
    Unknown8Bit,
}

impl From<SourceEncoding> for Encoding {
    fn from(encoding: SourceEncoding) -> Encoding {
        match encoding {
            SourceEncoding::Utf8 => Encoding::Utf8,
            SourceEncoding::SingleByte(codec) => Encoding::SingleByte(codec),
            SourceEncoding::MultiByte(codec) => Encoding::MultiByte(codec),
        }
    }
}

/// What a file's bytes are.
#[derive(Debug, PartialEq, Eq)]
pub enum Content {
    /// Text in `encoding`, starting `body` bytes in, after any byte-order mark.
    Text {
        /// The encoding.
        encoding: Encoding,
        /// How many bytes of byte-order mark come before the text.
        body: usize,
    },
    /// Binary, by git's test: a NUL in the first 8000 bytes, with no byte-order mark.
    Binary,
}

/// The encoding a byte-order mark at the start of `bytes` declares, and how long the mark is.
///
/// UTF-32's marks are tested before UTF-16's because the little-endian UTF-32 mark begins with
/// the UTF-16 one.
#[must_use]
pub fn byte_order_mark(bytes: &[u8]) -> Option<(Encoding, usize)> {
    const MARKS: [(&[u8], Encoding); 5] = [
        (&[0xEF, 0xBB, 0xBF], Encoding::Utf8),
        (&[0xFF, 0xFE, 0x00, 0x00], Encoding::Utf32Le),
        (&[0x00, 0x00, 0xFE, 0xFF], Encoding::Utf32Be),
        (&[0xFF, 0xFE], Encoding::Utf16Le),
        (&[0xFE, 0xFF], Encoding::Utf16Be),
    ];
    MARKS
        .into_iter()
        .find(|(mark, _)| bytes.starts_with(mark))
        .map(|(mark, encoding)| (encoding, mark.len()))
}

/// Whether git would call `bytes` binary: a NUL in its first 8000 bytes.
#[must_use]
pub fn looks_binary(bytes: &[u8]) -> bool {
    memchr::memchr(0, &bytes[..bytes.len().min(BINARY_PROBE)]).is_some()
}

/// Why bytes are not proven UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unproven {
    /// They are not valid UTF-8.
    Invalid,
    /// They are valid UTF-8 but switch into a multi-byte set the ISO-2022 way, with `ESC $`, so
    /// they may be ISO-2022 text that is seven-bit and only looks like UTF-8.
    Iso2022Switch,
}

/// Whether `bytes` are proven UTF-8: valid UTF-8 with no ISO-2022 switch into a multi-byte set.
///
/// ISO-2022-JP is seven-bit, so its bytes are valid UTF-8 and an ASCII query could match inside
/// its Japanese text.  It switches into Japanese with an escape sequence that starts `ESC $`,
/// anywhere in the text, so the whole of it is checked.  Colour codes start `ESC [` and are not
/// mistaken for it.
///
/// # Errors
///
/// Returns why they are not proven.
pub fn proven_utf8(bytes: &[u8]) -> Result<(), Unproven> {
    if std::str::from_utf8(bytes).is_err() {
        return Err(Unproven::Invalid);
    }
    if memchr::memmem::find(bytes, b"\x1b$").is_some() {
        return Err(Unproven::Iso2022Switch);
    }
    Ok(())
}

/// Decides what the bytes of a file are by what they hold alone: a byte-order mark, then git's
/// binary test, then whether they are proven UTF-8, and otherwise an unknown 8-bit encoding.
///
/// A byte-order mark wins, because it is a declaration, and a UTF-16 or UTF-32 file is full of
/// NULs that would otherwise make it binary.  Declarations kept elsewhere - a Python file's
/// coding line, a project's - are the caller's to weigh first.
#[must_use]
pub fn classify(bytes: &[u8]) -> Content {
    if let Some((encoding, body)) = byte_order_mark(bytes) {
        return Content::Text { encoding, body };
    }
    if looks_binary(bytes) {
        return Content::Binary;
    }
    let encoding = if proven_utf8(bytes).is_ok() {
        Encoding::Utf8
    } else {
        Encoding::Unknown8Bit
    };
    Content::Text { encoding, body: 0 }
}

/// What an encoding named outside a Python file is to quaff, by `CPython`'s codec lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    /// An encoding quaff reads.
    Read(Encoding),
    /// A text encoding `CPython` reads and quaff does not yet, by its `CPython` codec name.
    Unread(&'static str),
    /// No `CPython` text codec has the name.
    Unknown,
}

/// The encoding `name` means, looked up as `CPython` looks a codec name up, so that the spellings
/// git's attributes and `EditorConfig` use - `SHIFT-JIS`, `ISO-8859-1`, `UTF-16LE` - find the
/// same codecs a Python file's declaration would.
///
/// UTF-16 and UTF-32 are read by name only with their byte order: plain `UTF-16` needs a
/// byte-order mark to say which, and with one, the mark has decided before any name is read.
#[must_use]
pub fn named(name: &str) -> Named {
    use python::Module;
    let Some((module, kind)) = python::resolve(name) else {
        return Named::Unknown;
    };
    match kind {
        Module::Utf8 => Named::Read(Encoding::Utf8),
        Module::SingleByte(index) => Named::Read(Encoding::SingleByte(Codec::at(index))),
        Module::MultiByte(index) => Named::Read(Encoding::MultiByte(MultiByteCodec::at(index))),
        Module::Unread => match module {
            "utf_16_le" => Named::Read(Encoding::Utf16Le),
            "utf_16_be" => Named::Read(Encoding::Utf16Be),
            "utf_32_le" => Named::Read(Encoding::Utf32Le),
            "utf_32_be" => Named::Read(Encoding::Utf32Be),
            _ => Named::Unread(module),
        },
        Module::NotText | Module::Undefined => Named::Unknown,
    }
}

/// How a piece of text is spelled in an encoding.
#[derive(Debug, PartialEq, Eq)]
pub enum Spelling {
    /// As these bytes, and no others.
    Bytes(Vec<u8>),
    /// Not at all: a character in it is one the encoding cannot hold, so the text cannot occur.
    Impossible,
    /// Not as any one run of bytes: the encoding is unknown, or it spells a character in the
    /// text more than one way, so no search for one run of bytes would be honest.
    Ambiguous,
}

impl Encoding {
    /// The width of one code unit, in bytes; a match must start on a unit boundary.
    #[must_use]
    pub fn unit(self) -> usize {
        match self {
            Encoding::Utf8
            | Encoding::SingleByte(_)
            | Encoding::MultiByte(_)
            | Encoding::Unknown8Bit => 1,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            Encoding::Utf32Le | Encoding::Utf32Be => 4,
        }
    }

    /// How `text` is spelled in this encoding.
    ///
    /// In an unknown 8-bit encoding, ASCII text is the same bytes in every ASCII-compatible
    /// encoding, but any other character has no single spelling.  In a single-byte or
    /// multi-byte encoding, a character it cannot hold makes the text impossible, and one it
    /// spells several ways - a few codecs do - ambiguous; impossible wins, since the text cannot
    /// occur either way.
    #[must_use]
    pub fn encode(self, text: &str) -> Spelling {
        match self {
            Encoding::Utf8 => Spelling::Bytes(text.as_bytes().to_vec()),
            Encoding::Unknown8Bit if text.is_ascii() => Spelling::Bytes(text.as_bytes().to_vec()),
            Encoding::Unknown8Bit => Spelling::Ambiguous,
            Encoding::SingleByte(codec) => {
                let mut bytes = Vec::with_capacity(text.len());
                let mut ambiguous = false;
                for c in text.chars() {
                    let mut spellings = codec.spellings(c);
                    match (spellings.next(), spellings.next()) {
                        (None, _) => return Spelling::Impossible,
                        (Some(byte), None) => bytes.push(byte),
                        (Some(_), Some(_)) => ambiguous = true,
                    }
                }
                if ambiguous {
                    Spelling::Ambiguous
                } else {
                    Spelling::Bytes(bytes)
                }
            }
            Encoding::MultiByte(codec) => {
                let mut bytes = Vec::with_capacity(text.len() * 2);
                let mut ambiguous = false;
                for c in text.chars() {
                    match codec.spellings(c).as_slice() {
                        [] => return Spelling::Impossible,
                        [only] => bytes.extend_from_slice(only),
                        _ => ambiguous = true,
                    }
                }
                if ambiguous {
                    Spelling::Ambiguous
                } else {
                    Spelling::Bytes(bytes)
                }
            }
            Encoding::Utf16Le => {
                Spelling::Bytes(text.encode_utf16().flat_map(u16::to_le_bytes).collect())
            }
            Encoding::Utf16Be => {
                Spelling::Bytes(text.encode_utf16().flat_map(u16::to_be_bytes).collect())
            }
            Encoding::Utf32Le => Spelling::Bytes(
                text.chars()
                    .flat_map(|c| u32::from(c).to_le_bytes())
                    .collect(),
            ),
            Encoding::Utf32Be => Spelling::Bytes(
                text.chars()
                    .flat_map(|c| u32::from(c).to_be_bytes())
                    .collect(),
            ),
        }
    }

    /// Whether every byte of `bytes` decodes in this encoding, with nothing left over.
    ///
    /// Says only that the bytes could be in this encoding, never that they are: many runs of
    /// bytes read cleanly in several encodings.
    #[must_use]
    pub fn reads_cleanly(self, bytes: &[u8]) -> bool {
        match self {
            Encoding::Utf8 => std::str::from_utf8(bytes).is_ok(),
            Encoding::SingleByte(codec) => bytes.iter().all(|&b| codec.decode(b).is_some()),
            Encoding::MultiByte(codec) => {
                let mut at = 0;
                while at < bytes.len() {
                    match codec.read(&bytes[at..]) {
                        Read::Char(_, width) => at += width,
                        Read::Invalid(_) => return false,
                    }
                }
                true
            }
            Encoding::Unknown8Bit => bytes.is_ascii(),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let mut clean = bytes.len().is_multiple_of(2);
                self.each_char(bytes, |_, c| {
                    clean &= c != '\u{FFFD}';
                    clean
                });
                clean
            }
            Encoding::Utf32Le | Encoding::Utf32Be => {
                let mut clean = bytes.len().is_multiple_of(4);
                self.each_char(bytes, |_, c| {
                    clean &= c != '\u{FFFD}';
                    clean
                });
                clean
            }
        }
    }

    /// Calls `visit` with each character of `bytes` and the offset of its first byte, in
    /// order, until it returns `false`.
    ///
    /// Nothing is collected, so a search needs no memory for the characters it walks past.
    /// Bytes that do not decode - an invalid sequence in UTF-8, any non-ASCII byte in an
    /// unknown 8-bit encoding, a byte a single-byte encoding decodes to nothing, an unpaired
    /// surrogate, a truncated unit - each count as one character, `U+FFFD`, so that positions
    /// stay countable.  A multi-byte encoding counts them as `CPython` does, one character for
    /// each error `CPython` reports, which is a byte, or a character cut short at the end.  For a single-byte encoding such
    /// as Latin-1 that is one column a byte, which is what an editor showing the file in that
    /// encoding counts.
    pub fn each_char(self, bytes: &[u8], mut visit: impl FnMut(usize, char) -> bool) {
        match self {
            Encoding::Utf8 => {
                // A UTF-8 file declared by its byte-order mark may still hold invalid bytes, so
                // valid runs and invalid bytes are walked separately.
                let mut offset = 0;
                for chunk in bytes.utf8_chunks() {
                    for (i, c) in chunk.valid().char_indices() {
                        if !visit(offset + i, c) {
                            return;
                        }
                    }
                    offset += chunk.valid().len();
                    for i in 0..chunk.invalid().len() {
                        if !visit(offset + i, '\u{FFFD}') {
                            return;
                        }
                    }
                    offset += chunk.invalid().len();
                }
            }
            Encoding::SingleByte(codec) => {
                for (i, &b) in bytes.iter().enumerate() {
                    if !visit(i, codec.decode(b).unwrap_or('\u{FFFD}')) {
                        return;
                    }
                }
            }
            Encoding::MultiByte(codec) => {
                let mut at = 0;
                while at < bytes.len() {
                    let (c, width) = match codec.read(&bytes[at..]) {
                        Read::Char(c, width) => (c, width),
                        Read::Invalid(width) => ('\u{FFFD}', width),
                    };
                    if !visit(at, c) {
                        return;
                    }
                    at += width;
                }
            }
            Encoding::Unknown8Bit => {
                for (i, &b) in bytes.iter().enumerate() {
                    let c = if b.is_ascii() {
                        char::from(b)
                    } else {
                        '\u{FFFD}'
                    };
                    if !visit(i, c) {
                        return;
                    }
                }
            }
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let little = self == Encoding::Utf16Le;
                let units = bytes.as_chunks::<2>().0.iter().map(|&pair| {
                    if little {
                        u16::from_le_bytes(pair)
                    } else {
                        u16::from_be_bytes(pair)
                    }
                });
                let mut offset = 0;
                for decoded in char::decode_utf16(units) {
                    let (c, width) = match decoded {
                        Ok(c) => (c, c.len_utf16() * 2),
                        Err(_) => ('\u{FFFD}', 2),
                    };
                    if !visit(offset, c) {
                        return;
                    }
                    offset += width;
                }
                if !bytes.len().is_multiple_of(2) {
                    visit(bytes.len() - 1, '\u{FFFD}');
                }
            }
            Encoding::Utf32Le | Encoding::Utf32Be => {
                let little = self == Encoding::Utf32Le;
                for (i, &quad) in bytes.as_chunks::<4>().0.iter().enumerate() {
                    let value = if little {
                        u32::from_le_bytes(quad)
                    } else {
                        u32::from_be_bytes(quad)
                    };
                    if !visit(i * 4, char::from_u32(value).unwrap_or('\u{FFFD}')) {
                        return;
                    }
                }
                if !bytes.len().is_multiple_of(4) {
                    visit(bytes.len() - bytes.len() % 4, '\u{FFFD}');
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_order_mark_decides_the_encoding() {
        assert_eq!(
            classify(b"\xEF\xBB\xBFtext"),
            Content::Text {
                encoding: Encoding::Utf8,
                body: 3
            }
        );
        assert_eq!(
            classify(b"\xFF\xFEt\x00"),
            Content::Text {
                encoding: Encoding::Utf16Le,
                body: 2
            }
        );
        assert_eq!(
            classify(b"\xFE\xFF\x00t"),
            Content::Text {
                encoding: Encoding::Utf16Be,
                body: 2
            }
        );
        assert_eq!(
            classify(b"\xFF\xFE\x00\x00t\x00\x00\x00"),
            Content::Text {
                encoding: Encoding::Utf32Le,
                body: 4
            }
        );
        assert_eq!(
            classify(b"\x00\x00\xFE\xFF\x00\x00\x00t"),
            Content::Text {
                encoding: Encoding::Utf32Be,
                body: 4
            }
        );
    }

    #[test]
    fn a_nul_in_the_first_8000_bytes_is_binary_and_later_is_not() {
        assert_eq!(classify(b"a\0b"), Content::Binary);
        let mut late = vec![b'a'; BINARY_PROBE];
        late.push(0);
        // Valid UTF-8, since a NUL is a valid UTF-8 character.
        assert_eq!(
            classify(&late),
            Content::Text {
                encoding: Encoding::Utf8,
                body: 0
            }
        );
        let mut edge = vec![b'a'; BINARY_PROBE - 1];
        edge.push(0);
        assert_eq!(classify(&edge), Content::Binary);
    }

    #[test]
    fn invalid_utf8_without_a_mark_is_an_unknown_8bit_encoding() {
        assert_eq!(
            classify(b"caf\xE9"),
            Content::Text {
                encoding: Encoding::Unknown8Bit,
                body: 0
            }
        );
        assert_eq!(
            classify(b""),
            Content::Text {
                encoding: Encoding::Utf8,
                body: 0
            }
        );
    }

    #[test]
    fn a_query_is_encoded_the_way_the_file_is() {
        let bytes = |b: &[u8]| Spelling::Bytes(b.to_vec());
        assert_eq!(Encoding::Utf16Le.encode("hé"), bytes(&[b'h', 0, 0xE9, 0]));
        assert_eq!(Encoding::Utf16Be.encode("hé"), bytes(&[0, b'h', 0, 0xE9]));
        assert_eq!(Encoding::Utf32Be.encode("h"), bytes(&[0, 0, 0, b'h']));
        assert_eq!(Encoding::Unknown8Bit.encode("TODO"), bytes(b"TODO"));
        assert_eq!(Encoding::Unknown8Bit.encode("café"), Spelling::Ambiguous);
    }

    fn single_byte(name: &str) -> Encoding {
        Encoding::SingleByte(Codec::named(name).unwrap())
    }

    #[test]
    fn a_query_in_a_single_byte_encoding_is_spelled_its_way_or_not_at_all() {
        assert_eq!(
            single_byte("koi8_r").encode("да"),
            Spelling::Bytes(vec![0xC4, 0xC1])
        );
        assert_eq!(
            single_byte("latin_1").encode("é"),
            Spelling::Bytes(vec![0xE9])
        );
        assert_eq!(single_byte("koi8_r").encode("dé"), Spelling::Impossible);
        assert_eq!(single_byte("mac_arabic").encode("a b"), Spelling::Ambiguous);
        // A character the codec cannot hold settles it, ambiguous or not.
        assert_eq!(
            single_byte("mac_arabic").encode(" \u{4e00}"),
            Spelling::Impossible
        );
    }

    #[test]
    fn utf8_is_proven_only_without_an_iso_2022_switch() {
        assert_eq!(proven_utf8(b"plain"), Ok(()));
        assert_eq!(proven_utf8("caf\u{e9}".as_bytes()), Ok(()));
        assert_eq!(proven_utf8(b"caf\xE9"), Err(Unproven::Invalid));
        // ISO-2022-JP switches into Japanese with ESC $ B, anywhere in the text.
        assert_eq!(
            proven_utf8(b"import os\n# \x1b$B%3%a%s%H\x1b(B\n"),
            Err(Unproven::Iso2022Switch)
        );
        // Colour codes, and a terminal's switch back to ASCII, are not that.
        assert_eq!(proven_utf8(b"\x1b[31mred\x1b(B\x1b[m"), Ok(()));
        assert_eq!(
            classify(b"\x1b$Bx"),
            Content::Text {
                encoding: Encoding::Unknown8Bit,
                body: 0
            }
        );
    }

    #[test]
    fn an_encoding_named_outside_python_is_found_as_cpython_finds_it() {
        let sjis = MultiByteCodec::named("shift_jis").unwrap();
        assert_eq!(named("SHIFT-JIS"), Named::Read(Encoding::MultiByte(sjis)));
        assert_eq!(named("ISO-8859-1"), Named::Read(single_byte("latin_1")));
        assert_eq!(named("latin1"), Named::Read(single_byte("latin_1")));
        assert_eq!(named("UTF-8"), Named::Read(Encoding::Utf8));
        assert_eq!(named("UTF-16LE"), Named::Read(Encoding::Utf16Le));
        assert_eq!(named("utf-32-be"), Named::Read(Encoding::Utf32Be));
        // Plain UTF-16 says nothing of byte order without a mark.
        assert_eq!(named("UTF-16"), Named::Unread("utf_16"));
        assert_eq!(named("GBK"), Named::Unread("gbk"));
        assert_eq!(named("rot13"), Named::Unknown);
        assert_eq!(named("no-such-encoding"), Named::Unknown);
    }

    #[test]
    fn bytes_read_cleanly_only_where_every_byte_decodes() {
        let euc = Encoding::MultiByte(MultiByteCodec::named("euc_jp").unwrap());
        assert!(euc.reads_cleanly(b"x \xa5\xa2"));
        assert!(!euc.reads_cleanly(b"x \xa5"));
        assert!(single_byte("latin_1").reads_cleanly(b"caf\xe9"));
        assert!(!single_byte("cp1252").reads_cleanly(b"\x81"));
        assert!(!Encoding::Utf8.reads_cleanly(b"caf\xe9"));
    }

    #[test]
    fn a_query_in_a_multi_byte_encoding_is_spelled_its_way_or_not_at_all() {
        let sjis = Encoding::MultiByte(MultiByteCodec::named("shift_jis").unwrap());
        assert_eq!(sjis.encode("A日"), Spelling::Bytes(vec![b'A', 0x93, 0xfa]));
        assert_eq!(sjis.encode("é"), Spelling::Impossible);
        let cp932 = Encoding::MultiByte(MultiByteCodec::named("cp932").unwrap());
        assert_eq!(cp932.encode("∪"), Spelling::Ambiguous);
    }

    fn chars(encoding: Encoding, bytes: &[u8]) -> Vec<(usize, char)> {
        let mut found = Vec::new();
        encoding.each_char(bytes, |offset, c| {
            found.push((offset, c));
            true
        });
        found
    }

    #[test]
    fn a_byte_order_mark_does_not_vouch_for_every_byte() {
        // Declared UTF-8 with one stray byte: the multi-byte character before it is still one
        // character, and the stray byte is one.
        assert_eq!(
            chars(Encoding::Utf8, b"\xC3\xA9 \xE9x"),
            vec![(0, 'é'), (2, ' '), (3, '\u{FFFD}'), (4, 'x')]
        );
    }

    #[test]
    fn each_byte_of_a_longer_invalid_sequence_is_one_character() {
        // A truncated three-byte sequence: two bytes, then a valid character.
        assert_eq!(
            chars(Encoding::Utf8, b"\xE2\x82x"),
            vec![(0, '\u{FFFD}'), (1, '\u{FFFD}'), (2, 'x')]
        );
    }

    #[test]
    fn a_truncated_utf32_tail_is_one_character_at_its_own_offset() {
        assert_eq!(
            // Three whole units and one stray byte, so the tail's offset, 12, is neither the
            // length less a whole unit nor the length less its unit count.
            chars(Encoding::Utf32Le, b"a\0\0\0b\0\0\0c\0\0\0d"),
            vec![(0, 'a'), (4, 'b'), (8, 'c'), (12, '\u{FFFD}')]
        );
    }

    #[test]
    fn visiting_stops_when_asked() {
        let mut seen = 0;
        Encoding::Utf8.each_char(b"abcdef", |_, _| {
            seen += 1;
            seen < 2
        });
        assert_eq!(seen, 2);
    }

    #[test]
    fn characters_carry_their_byte_offsets() {
        assert_eq!(
            chars(Encoding::Utf8, "aé b".as_bytes()),
            vec![(0, 'a'), (1, 'é'), (3, ' '), (4, 'b')]
        );
        assert_eq!(
            chars(Encoding::Unknown8Bit, b"a\xE9b"),
            vec![(0, 'a'), (1, '\u{FFFD}'), (2, 'b')]
        );
        assert_eq!(
            chars(single_byte("cp1252"), b"a\x80\x81"),
            vec![(0, 'a'), (1, '€'), (2, '\u{FFFD}')]
        );
        // A surrogate pair is one character, four bytes wide.
        let emoji: Vec<u8> = "a😀b".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(
            chars(Encoding::Utf16Le, &emoji),
            vec![(0, 'a'), (2, '😀'), (6, 'b')]
        );
        assert_eq!(
            chars(Encoding::Utf16Le, b"a\x00b"),
            vec![(0, 'a'), (2, '\u{FFFD}')]
        );
        assert_eq!(
            chars(Encoding::Utf32Be, b"\x00\x00\x00a\x00\x11\x00\x00"),
            vec![(0, 'a'), (4, '\u{FFFD}')]
        );
    }
}
