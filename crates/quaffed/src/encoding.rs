// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What a file's bytes are, as far as a textual search can tell without guessing.
//!
//! A query is matched in the file's own encoding: it is encoded the way the file is, rather
//! than the file being decoded.  The encoding is taken from what the file declares - a
//! byte-order mark - or from the bytes being valid UTF-8, and never inferred from statistics.
//! Anything else is binary, by git's own test, or an unknown 8-bit encoding, where an ASCII
//! query is the same bytes in every ASCII-compatible encoding and anything else cannot be
//! matched honestly.

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
    /// Not UTF-8 and nothing declared: some 8-bit encoding, but which is unknown.
    Unknown8Bit,
}

/// What a file's bytes are.
#[derive(Debug, PartialEq, Eq)]
pub enum Content {
    /// Text in `encoding`, starting `body` bytes in, after any byte-order mark.
    Text { encoding: Encoding, body: usize },
    /// Binary, by git's test: a NUL in the first 8000 bytes, with no byte-order mark.
    Binary,
}

/// Decides what `bytes` are.
///
/// A byte-order mark wins, because it is a declaration, and a UTF-16 or UTF-32 file is full of
/// NULs that would otherwise make it binary.  UTF-32's marks are tested before UTF-16's because
/// the little-endian UTF-32 mark begins with the UTF-16 one.
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

impl Encoding {
    /// The width of one code unit, in bytes; a match must start on a unit boundary.
    pub fn unit(self) -> usize {
        match self {
            Encoding::Utf8 | Encoding::Unknown8Bit => 1,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            Encoding::Utf32Le | Encoding::Utf32Be => 4,
        }
    }

    /// Returns `text` encoded as this encoding, or `None` if it cannot be, honestly.
    ///
    /// Only an unknown 8-bit encoding refuses: ASCII text is the same bytes in every
    /// ASCII-compatible encoding, but any other character has no single spelling there.
    pub fn encode(self, text: &str) -> Option<Vec<u8>> {
        match self {
            Encoding::Utf8 => Some(text.as_bytes().to_vec()),
            Encoding::Unknown8Bit => text.is_ascii().then(|| text.as_bytes().to_vec()),
            Encoding::Utf16Le => Some(text.encode_utf16().flat_map(u16::to_le_bytes).collect()),
            Encoding::Utf16Be => Some(text.encode_utf16().flat_map(u16::to_be_bytes).collect()),
            Encoding::Utf32Le => Some(
                text.chars()
                    .flat_map(|c| u32::from(c).to_le_bytes())
                    .collect(),
            ),
            Encoding::Utf32Be => Some(
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
    /// unknown 8-bit encoding, an unpaired surrogate, a truncated unit - each count as one
    /// character, `U+FFFD`, so that positions stay countable.  For a single-byte encoding such
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
        assert_eq!(Encoding::Utf16Le.encode("hé"), Some(vec![b'h', 0, 0xE9, 0]));
        assert_eq!(Encoding::Utf16Be.encode("hé"), Some(vec![0, b'h', 0, 0xE9]));
        assert_eq!(Encoding::Utf32Be.encode("h"), Some(vec![0, 0, 0, b'h']));
        assert_eq!(Encoding::Unknown8Bit.encode("TODO"), Some(b"TODO".to_vec()));
        assert_eq!(Encoding::Unknown8Bit.encode("café"), None);
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
