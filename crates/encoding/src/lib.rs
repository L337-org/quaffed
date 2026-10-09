// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What a file's bytes are, as far as quaff can tell without guessing.
//!
//! A textual query is matched in the file's own encoding: it is encoded the way the file is,
//! rather than the file being decoded.  The encoding is taken from what the file declares - a
//! byte-order mark, or a Python file's encoding declaration - or from the bytes being valid
//! UTF-8, and never inferred from statistics.  Anything else is binary, by git's own test, or an
//! unknown 8-bit encoding, where an ASCII query is the same bytes in every ASCII-compatible
//! encoding and anything else cannot be matched honestly.
//!
//! Python source is read as `CPython` reads it, by the [`python`] module's rules and `CPython`'s
//! own codecs.  `architecture/encoding.md` specifies this crate.

// Generated from the oracle's codecs; the generator owns its layout.
#[rustfmt::skip]
mod codecs;
pub mod python;

pub use python::{Codec, Refusal, SourceEncoding};

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
    /// A single-byte encoding a Python file declares, such as Latin-1 or KOI8-R.
    SingleByte(Codec),
    /// Not UTF-8 and nothing declared: some 8-bit encoding, but which is unknown.
    Unknown8Bit,
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

/// Decides what the bytes of any file are, by its byte-order mark and its bytes alone.
///
/// A byte-order mark wins, because it is a declaration, and a UTF-16 or UTF-32 file is full of
/// NULs that would otherwise make it binary.  UTF-32's marks are tested before UTF-16's because
/// the little-endian UTF-32 mark begins with the UTF-16 one.
#[must_use]
pub fn classify(bytes: &[u8]) -> Content {
    const MARKS: [(&[u8], Encoding); 5] = [
        (&[0xEF, 0xBB, 0xBF], Encoding::Utf8),
        (&[0xFF, 0xFE, 0x00, 0x00], Encoding::Utf32Le),
        (&[0x00, 0x00, 0xFE, 0xFF], Encoding::Utf32Be),
        (&[0xFF, 0xFE], Encoding::Utf16Le),
        (&[0xFE, 0xFF], Encoding::Utf16Be),
    ];
    for (mark, encoding) in MARKS {
        if bytes.starts_with(mark) {
            return Content::Text {
                encoding,
                body: mark.len(),
            };
        }
    }
    if memchr::memchr(0, &bytes[..bytes.len().min(BINARY_PROBE)]).is_some() {
        return Content::Binary;
    }
    let encoding = if std::str::from_utf8(bytes).is_ok() {
        Encoding::Utf8
    } else {
        Encoding::Unknown8Bit
    };
    Content::Text { encoding, body: 0 }
}

/// Decides what the bytes of a Python file are, honouring the encoding it declares as well.
///
/// A byte-order mark for UTF-16 or UTF-32 decides as it does for any file, and a NUL makes the
/// file binary as it does for any other.  Otherwise a declaration, if there is one, decides: a
/// declared encoding is trusted the way a byte-order mark is, so bytes it does not decode are
/// tolerated, each counting as one character.  With no declaration, the file is classified as
/// any other.
///
/// # Errors
///
/// Returns the [`Refusal`] for a declaration that cannot be honoured whatever the rest of the
/// file holds: an encoding Python does not know, a codec that is not of text, one quaff does
/// not read, or a declaration that contradicts a UTF-8 byte-order mark.
pub fn classify_python(bytes: &[u8]) -> Result<Content, Refusal> {
    let content = classify(bytes);
    if !matches!(
        content,
        Content::Text {
            encoding: Encoding::Utf8 | Encoding::Unknown8Bit,
            ..
        }
    ) {
        return Ok(content);
    }
    let body = if bytes.starts_with(python::UTF8_MARK) {
        python::UTF8_MARK.len()
    } else {
        0
    };
    Ok(match python::declared_encoding(bytes)? {
        None => content,
        Some(SourceEncoding::Utf8) => Content::Text {
            encoding: Encoding::Utf8,
            body,
        },
        Some(SourceEncoding::SingleByte(codec)) => Content::Text {
            encoding: Encoding::SingleByte(codec),
            body,
        },
    })
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
            Encoding::Utf8 | Encoding::SingleByte(_) | Encoding::Unknown8Bit => 1,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            Encoding::Utf32Le | Encoding::Utf32Be => 4,
        }
    }

    /// How `text` is spelled in this encoding.
    ///
    /// In an unknown 8-bit encoding, ASCII text is the same bytes in every ASCII-compatible
    /// encoding, but any other character has no single spelling.  In a single-byte encoding, a
    /// character it cannot hold makes the text impossible, and one it spells several ways - a
    /// few codecs do - ambiguous; impossible wins, since the text cannot occur either way.
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

    /// Calls `visit` with each character of `bytes` and the offset of its first byte, in
    /// order, until it returns `false`.
    ///
    /// Nothing is collected, so a search needs no memory for the characters it walks past.
    /// Bytes that do not decode - an invalid sequence in UTF-8, any non-ASCII byte in an
    /// unknown 8-bit encoding, a byte a single-byte encoding decodes to nothing, an unpaired
    /// surrogate, a truncated unit - each count as one character, `U+FFFD`, so that positions
    /// stay countable.  For a single-byte encoding such
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
    fn a_python_files_declaration_decides_its_encoding() {
        assert_eq!(
            classify_python(b"# coding: koi8-r\n\xc1"),
            Ok(Content::Text {
                encoding: single_byte("koi8_r"),
                body: 0
            })
        );
        assert_eq!(
            classify_python(b"\xef\xbb\xbf# coding: utf-8\n"),
            Ok(Content::Text {
                encoding: Encoding::Utf8,
                body: 3
            })
        );
        // Declared UTF-8 vouches for the file as a mark does, invalid bytes and all.
        assert_eq!(
            classify_python(b"# coding: utf-8\n\xe9"),
            Ok(Content::Text {
                encoding: Encoding::Utf8,
                body: 0
            })
        );
        // Undeclared, it is classified as any file is.
        assert_eq!(
            classify_python(b"x = '\xe9'\n"),
            Ok(Content::Text {
                encoding: Encoding::Unknown8Bit,
                body: 0
            })
        );
        assert_eq!(
            classify_python(b"# coding: latin-1\n\0"),
            Ok(Content::Binary)
        );
        let utf16 = [&[0xFF, 0xFE][..], &b"#\0 \0"[..]].concat();
        assert_eq!(
            classify_python(&utf16),
            Ok(Content::Text {
                encoding: Encoding::Utf16Le,
                body: 2
            })
        );
        assert!(matches!(
            classify_python(b"# coding: uft-8\n"),
            Err(Refusal::Unknown { .. })
        ));
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
