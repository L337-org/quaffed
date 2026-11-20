// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A file's text once decoded, with what it takes to write it back in the file's own encoding.
//!
//! Each language decides how its files are decoded - Python's by [`crate::python::decode`] - and
//! all of them give a [`Source`], so that an edit writes any file back the same way.

use std::fmt;

use crate::python::{Codec, MultiByteCodec, UTF8_MARK};

/// The encoding a file's text was decoded from, which it is written back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEncoding {
    /// UTF-8: declared, marked, or by default.
    Utf8,
    /// A single-byte codec, declared.
    SingleByte(Codec),
    /// A multi-byte codec, declared.
    MultiByte(MultiByteCodec),
}

/// A file's text, decoded as its language decodes it, with what it takes to write it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The text, after any byte-order mark, line endings as the file has them.
    pub text: String,
    /// The encoding it was decoded from.
    pub encoding: SourceEncoding,
    /// Whether the file starts with a UTF-8 byte-order mark.
    pub byte_order_mark: bool,
}

/// Text that has no one spelling in a file's encoding, so cannot be written to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unencodable {
    /// The first character with no one spelling.
    pub character: char,
    /// The encoding, by its `CPython` codec name.
    pub encoding: &'static str,
    /// Whether the encoding spells it several ways, rather than not at all.
    pub ambiguous: bool,
}

impl fmt::Display for Unencodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ambiguous {
            write!(
                f,
                "{:?} has more than one spelling in {:?}, so which to write is not known",
                self.character, self.encoding
            )
        } else {
            write!(
                f,
                "{:?} cannot be written in {:?}",
                self.character, self.encoding
            )
        }
    }
}

impl std::error::Error for Unencodable {}

impl Source {
    /// Encodes `text` the way this file is, byte-order mark first if it has one, so that
    /// encoding a file's own decoded text gives back its bytes exactly.
    ///
    /// # Errors
    ///
    /// Returns [`Unencodable`] naming the first character the encoding cannot hold, or holds
    /// under more than one byte, which only a few single-byte codecs do.
    pub fn encode(&self, text: &str) -> Result<Vec<u8>, Unencodable> {
        let mut bytes = Vec::with_capacity(text.len() + 3);
        if self.byte_order_mark {
            bytes.extend_from_slice(UTF8_MARK);
        }
        match self.encoding {
            SourceEncoding::Utf8 => bytes.extend_from_slice(text.as_bytes()),
            SourceEncoding::SingleByte(codec) => {
                for c in text.chars() {
                    let mut spellings = codec.spellings(c);
                    match (spellings.next(), spellings.next()) {
                        (Some(byte), None) => bytes.push(byte),
                        (first, _) => {
                            return Err(Unencodable {
                                character: c,
                                encoding: codec.name(),
                                ambiguous: first.is_some(),
                            });
                        }
                    }
                }
            }
            SourceEncoding::MultiByte(codec) => {
                for c in text.chars() {
                    let spellings = codec.spellings(c);
                    let [only] = spellings.as_slice() else {
                        return Err(Unencodable {
                            character: c,
                            encoding: codec.name(),
                            ambiguous: !spellings.is_empty(),
                        });
                    };
                    bytes.extend_from_slice(only);
                }
            }
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utf8_source_writes_its_mark_back_first() {
        let source = Source {
            text: String::new(),
            encoding: SourceEncoding::Utf8,
            byte_order_mark: true,
        };
        assert_eq!(source.encode("é").unwrap(), b"\xEF\xBB\xBF\xC3\xA9");
    }

    #[test]
    fn a_character_the_encoding_cannot_hold_is_named() {
        let latin_1 = Codec::named("latin_1").expect("latin-1 is a codec");
        let source = Source {
            text: String::new(),
            encoding: SourceEncoding::SingleByte(latin_1),
            byte_order_mark: false,
        };
        assert_eq!(source.encode("é").unwrap(), b"\xE9");
        let err = source.encode("€").unwrap_err();
        assert_eq!((err.character, err.ambiguous), ('€', false));
        assert_eq!(err.to_string(), "'€' cannot be written in \"latin_1\"");
    }
}
