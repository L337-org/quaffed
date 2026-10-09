// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Python source's encoding, read as `CPython` reads it.
//!
//! A Python file is UTF-8 unless its first or second line declares otherwise (PEP 263), and a
//! UTF-8 byte-order mark allows no declaration but UTF-8.  The rules here are `CPython`'s
//! tokenizer's, followed to the letter, quirks included, because a file quaff and `CPython` read
//! differently is a file quaff misreads: `Parser/tokenizer/helpers.c` and `string_tokenizer.c`
//! for finding the declaration, `Objects/unicodeobject.c` and `Lib/encodings/__init__.py` for
//! resolving its name, all at the oracle's release.  `architecture/encoding.md` specifies it.

use std::fmt;

use crate::codecs::{ALIASES, MODULES, SINGLE_BYTE, UNDEFINED};

/// What a codec module of `CPython`'s is, as far as quaff reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Module {
    /// Each byte decodes alone, to one character or to none: the index of its table in
    /// `SINGLE_BYTE`.
    SingleByte(usize),
    /// UTF-8.
    Utf8,
    /// A text encoding `CPython` reads and quaff does not: multi-byte, stateful, or UTF-16 or
    /// UTF-32.
    Unread,
    /// A codec, but not of text: `rot13`, `hex`, `zlib` and the like.
    NotText,
    /// The `undefined` codec, which refuses everything.
    Undefined,
}

/// A single-byte codec of `CPython`'s, such as Latin-1 or KOI8-R: each byte decodes alone, to
/// one character or to none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Codec(usize);

impl Codec {
    /// The codec by the name of its module in `CPython`'s `encodings` package, such as `koi8_r`,
    /// or `None` if there is no single-byte codec of that name.
    #[must_use]
    pub fn named(module: &str) -> Option<Codec> {
        match module_kind(module)? {
            Module::SingleByte(index) => Some(Codec(index)),
            _ => None,
        }
    }

    /// The name of its module in `CPython`'s `encodings` package, such as `koi8_r`.
    #[must_use]
    pub fn name(self) -> &'static str {
        SINGLE_BYTE[self.0].0
    }

    /// The character `byte` decodes to, or `None` if `CPython` decodes it to nothing.
    #[must_use]
    pub fn decode(self, byte: u8) -> Option<char> {
        match SINGLE_BYTE[self.0].1[usize::from(byte)] {
            UNDEFINED => None,
            unit => char::from_u32(u32::from(unit)),
        }
    }

    /// The bytes that decode to `c`, in order: none if the codec cannot hold it, and more than
    /// one for the few characters some codecs spell several ways, such as the space in
    /// `mac_arabic`.
    pub fn spellings(self, c: char) -> impl Iterator<Item = u8> {
        (0..=u8::MAX).filter(move |&byte| self.decode(byte) == Some(c))
    }
}

impl fmt::Debug for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How a Python file's bytes decode, by what it declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEncoding {
    /// UTF-8: declared, marked, or by default.
    Utf8,
    /// A single-byte codec, declared.
    SingleByte(Codec),
}

/// Why `CPython` refuses to read a Python file, and so quaff does too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A NUL byte, which no Python source may contain.
    Nul {
        /// The line it is on, from 1.
        line: usize,
    },
    /// A declared encoding Python does not know.
    Unknown {
        /// The name, as the file spells it.
        name: String,
        /// The line declaring it.
        line: usize,
    },
    /// A declared codec that is not a text encoding, such as `rot13`.
    NotText {
        /// The name, as the file spells it.
        name: String,
        /// The line declaring it.
        line: usize,
    },
    /// The `undefined` codec, declared.
    Undefined {
        /// The name, as the file spells it.
        name: String,
        /// The line declaring it.
        line: usize,
    },
    /// A declared encoding `CPython` reads and quaff does not: a multi-byte or stateful encoding,
    /// or UTF-16 or UTF-32.  This one is quaff's limit, not Python's.
    Unread {
        /// The name, as the file spells it.
        name: String,
        /// The `CPython` codec it names, such as `shift_jis`.
        codec: &'static str,
        /// The line declaring it.
        line: usize,
    },
    /// A UTF-8 byte-order mark and a declaration of anything but UTF-8.
    MarkConflict {
        /// The name, as the file spells it.
        name: String,
        /// The line declaring it.
        line: usize,
    },
    /// A byte that does not decode in the file's encoding.
    Invalid {
        /// The byte.
        byte: u8,
        /// The line it is on, from 1.
        line: usize,
        /// The encoding, and the line that declared it if one did.
        encoding: Described,
    },
}

/// The encoding a byte failed to decode in, as a message names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Described {
    /// UTF-8, because nothing was declared.
    Undeclared,
    /// UTF-8 because a byte-order mark says so.
    Marked,
    /// The declaration's name, as the file spells it, and its line.
    Declared {
        /// The name, as the file spells it.
        name: String,
        /// The line declaring it.
        line: usize,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Nul { line } => {
                write!(
                    f,
                    "line {line} has a NUL byte, which Python source cannot contain"
                )
            }
            Refusal::Unknown { name, line } => write!(
                f,
                "line {line} declares the encoding {name:?}, which Python does not know"
            ),
            Refusal::NotText { name, line } => write!(
                f,
                "line {line} declares the encoding {name:?}, which is a Python codec but not \
                 a text encoding"
            ),
            Refusal::Undefined { name, line } => write!(
                f,
                "line {line} declares the encoding {name:?}, Python's codec that decodes \
                 nothing"
            ),
            Refusal::Unread { name, line, .. } => write!(
                f,
                "line {line} declares the encoding {name:?}, which Python reads and quaff does \
                 not: quaff reads UTF-8 and single-byte encodings"
            ),
            Refusal::MarkConflict { name, line } => write!(
                f,
                "it starts with a UTF-8 byte-order mark, but line {line} declares the encoding \
                 {name:?}; with a mark, Python accepts only a declaration of \"utf-8\""
            ),
            Refusal::Invalid {
                byte,
                line,
                encoding,
            } => {
                write!(f, "byte {byte:#04x} on line {line} is not valid ")?;
                match encoding {
                    Described::Undeclared => write!(
                        f,
                        "UTF-8, and no encoding is declared; declare one on the first or second \
                         line as PEP 263 says, or re-encode the file as UTF-8"
                    ),
                    Described::Marked => {
                        write!(f, "UTF-8, which the file's byte-order mark declares")
                    }
                    Described::Declared { name, line } => {
                        write!(f, "{name:?}, the encoding line {line} declares")
                    }
                }
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// The UTF-8 byte-order mark.
pub(crate) const UTF8_MARK: &[u8] = &[0xEF, 0xBB, 0xBF];

/// A Python file's text, decoded as `CPython` decodes it, with what it takes to write it back.
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
    /// Decodes a whole Python file as `CPython` does, refusing what `CPython` refuses.
    ///
    /// # Errors
    ///
    /// Returns the [`Refusal`] `CPython`'s own refusal corresponds to: a NUL byte, a declared
    /// encoding that is unknown, not of text or the `undefined` codec, a byte-order mark with a
    /// declaration of anything but UTF-8, or a byte invalid in the encoding.  Also a declared
    /// encoding `CPython` reads and quaff does not, [`Refusal::Unread`], which is quaff's own
    /// limit.  Nothing partial is returned.
    pub fn decode(bytes: &[u8]) -> Result<Source, Refusal> {
        // CPython refuses a NUL before it looks at anything else.
        if let Some(at) = memchr::memchr(0, bytes) {
            return Err(Refusal::Nul {
                line: line_of(bytes, at),
            });
        }
        let (encoding, body) = encoding(bytes)?;
        let source_encoding = encoding.source_encoding();
        let rest = &bytes[body..];
        let text = match encoding {
            Resolved::Utf8(described) => match std::str::from_utf8(rest) {
                Ok(text) => text.to_owned(),
                Err(err) => {
                    let at = err.valid_up_to();
                    return Err(Refusal::Invalid {
                        byte: rest[at],
                        line: line_of(rest, at),
                        encoding: described,
                    });
                }
            },
            Resolved::SingleByte(codec, name, declared_on) => {
                let mut text = String::with_capacity(rest.len());
                for (at, &byte) in rest.iter().enumerate() {
                    let Some(c) = codec.decode(byte) else {
                        return Err(Refusal::Invalid {
                            byte,
                            line: line_of(rest, at),
                            encoding: Described::Declared {
                                name,
                                line: declared_on,
                            },
                        });
                    };
                    text.push(c);
                }
                text
            }
        };
        Ok(Source {
            text,
            encoding: source_encoding,
            byte_order_mark: body > 0,
        })
    }

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
        }
        Ok(bytes)
    }
}

/// An encoding a declaration, a mark or the default settled on, before a byte is decoded.
enum Resolved {
    /// UTF-8, and why.
    Utf8(Described),
    /// A single-byte codec, the name it was declared by, and the line declaring it.
    SingleByte(Codec, String, usize),
}

impl Resolved {
    fn source_encoding(&self) -> SourceEncoding {
        match self {
            Resolved::Utf8(_) => SourceEncoding::Utf8,
            Resolved::SingleByte(codec, ..) => SourceEncoding::SingleByte(*codec),
        }
    }
}

/// The encoding a Python file's bytes are in, and how many bytes of mark come before its text.
///
/// The declaration is resolved and its conflicts with a mark refused, but no byte past the
/// declaration is checked; [`Source::decode`] does that.
fn encoding(bytes: &[u8]) -> Result<(Resolved, usize), Refusal> {
    let marked = bytes.starts_with(UTF8_MARK);
    let body = if marked { UTF8_MARK.len() } else { 0 };
    let Some(declaration) = declaration(&bytes[body..]) else {
        let why = if marked {
            Described::Marked
        } else {
            Described::Undeclared
        };
        return Ok((Resolved::Utf8(why), body));
    };
    let normal = normal_name(declaration.name);
    let name = declaration.name.to_owned();
    let line = declaration.line;
    if marked && normal != "utf-8" {
        return Err(Refusal::MarkConflict { name, line });
    }
    // CPython takes exactly "utf-8", after its own normalising, as UTF-8 without looking the
    // codec up.
    if normal == "utf-8" {
        let why = if marked {
            Described::Marked
        } else {
            Described::Declared { name, line }
        };
        return Ok((Resolved::Utf8(why), body));
    }
    let resolved = match resolve(normal) {
        None => return Err(Refusal::Unknown { name, line }),
        Some((_, Module::NotText)) => return Err(Refusal::NotText { name, line }),
        Some((_, Module::Undefined)) => return Err(Refusal::Undefined { name, line }),
        Some((codec, Module::Unread)) => return Err(Refusal::Unread { name, codec, line }),
        Some((_, Module::Utf8)) => Resolved::Utf8(Described::Declared { name, line }),
        Some((_, Module::SingleByte(index))) => Resolved::SingleByte(Codec(index), name, line),
    };
    Ok((resolved, body))
}

/// The encoding a Python file declares, honouring a UTF-8 byte-order mark as `CPython` does, for
/// a reader that tolerates bytes the encoding does not decode, as a textual search does.
///
/// Returns `Ok(None)` when nothing is declared, so that the caller decides what undeclared
/// bytes are, and the declaration's encoding otherwise.
///
/// # Errors
///
/// Returns the [`Refusal`] for a declaration `CPython` would refuse whatever the rest of the
/// file holds: one that is unknown, not of text, the `undefined` codec, one quaff does not read,
/// or one conflicting with a byte-order mark.
pub fn declared_encoding(bytes: &[u8]) -> Result<Option<SourceEncoding>, Refusal> {
    let marked = bytes.starts_with(UTF8_MARK);
    if declaration(&bytes[if marked { UTF8_MARK.len() } else { 0 }..]).is_none() {
        return Ok(None);
    }
    Ok(Some(encoding(bytes)?.0.source_encoding()))
}

/// A declaration as `CPython` finds it: the name, as the file spells it, and the line it is on.
#[derive(Debug, PartialEq, Eq)]
struct Declaration<'a> {
    name: &'a str,
    line: usize,
}

/// The encoding declared on the first or second line of `body`, the bytes after any mark.
///
/// `CPython`'s `decode_str`: the first two lines are found after line endings are translated,
/// so LF, CRLF and a lone CR each end one, and a last line without one is given one.  The
/// second line is read only if the first is blank or nothing but a comment.
fn declaration(body: &[u8]) -> Option<Declaration<'_>> {
    let (first, rest) = split_line(body)?;
    if let Some(name) = coding_spec(first) {
        return Some(Declaration { name, line: 1 });
    }
    // Only whitespace, then a comment or the end, keeps CPython looking.
    let code_first = first
        .iter()
        .take_while(|&&b| b != b'#')
        .any(|&b| !matches!(b, b' ' | b'\t' | 0x0C));
    if code_first {
        return None;
    }
    let (second, _) = split_line(rest)?;
    coding_spec(second).map(|name| Declaration { name, line: 2 })
}

/// The first line of `bytes` without its ending, and the bytes after the ending, or `None` if
/// there are no bytes.
fn split_line(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if bytes.is_empty() {
        return None;
    }
    Some(match memchr::memchr2(b'\n', b'\r', bytes) {
        None => (bytes, &[]),
        Some(at) => {
            let width = if bytes[at..].starts_with(b"\r\n") {
                2
            } else {
                1
            };
            (&bytes[..at], &bytes[at + width..])
        }
    })
}

/// The name in a line's coding spec: `CPython`'s `get_coding_spec`.
///
/// The spec must be in a comment with only spaces, tabs and form feeds before it, and is the
/// first `coding:` or `coding=` followed, after any spaces or tabs, by a name of ASCII letters,
/// digits, `-`, `_` and `.`.  `CPython` stops searching seven bytes before the end of the line,
/// which changes nothing: a spec with a name needs at least eight.
fn coding_spec(line: &[u8]) -> Option<&str> {
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'#' => break,
            b' ' | b'\t' | 0x0C => i += 1,
            _ => return None,
        }
    }
    while i < line.len() {
        if line[i..].starts_with(b"coding") && matches!(line.get(i + 6), Some(b':' | b'=')) {
            let mut begin = i + 7;
            while matches!(line.get(begin), Some(b' ' | b'\t')) {
                begin += 1;
            }
            let end = begin
                + line[begin..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                    .count();
            if begin < end {
                return Some(
                    std::str::from_utf8(&line[begin..end]).expect("the name's bytes are ASCII"),
                );
            }
        }
        i += 1;
    }
    None
}

/// `CPython`'s `get_normal_name`: `utf-8` and the spellings of Latin-1 it recognises, in its first
/// 12 bytes, lower-cased with `_` as `-`; any other name unchanged.
fn normal_name(name: &str) -> &str {
    let folded: String = name
        .bytes()
        .take(12)
        .map(|b| {
            if b == b'_' {
                '-'
            } else {
                char::from(b.to_ascii_lowercase())
            }
        })
        .collect();
    let starts = |prefix: &str| folded.starts_with(prefix);
    if folded == "utf-8" || starts("utf-8-") {
        "utf-8"
    } else if ["latin-1", "iso-8859-1", "iso-latin-1"].contains(&folded.as_str())
        || starts("latin-1-")
        || starts("iso-8859-1-")
        || starts("iso-latin-1-")
    {
        "iso-8859-1"
    } else {
        name
    }
}

/// The codec module `CPython`'s codec lookup finds for `name`, and its kind, or `None` if it
/// finds none.
///
/// `CPython` lower-cases the name and collapses each run of anything but ASCII letters, digits
/// and `.` into one `_`, dropping it at either end; looks that up in its aliases, as it is and
/// with `.` as `_`; then tries the aliased module, then the name itself, as a module of the
/// `encodings` package, passing over any containing a `.`.
fn resolve(name: &str) -> Option<(&'static str, Module)> {
    let mut normal = String::with_capacity(name.len());
    let mut punctuation = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            if punctuation && !normal.is_empty() {
                normal.push('_');
            }
            punctuation = false;
            normal.push(c.to_ascii_lowercase());
        } else {
            punctuation = true;
        }
    }
    let alias = |key: &str| {
        ALIASES
            .binary_search_by(|(alias, _)| (*alias).cmp(key))
            .ok()
            .map(|i| ALIASES[i].1)
    };
    let aliased = alias(&normal).or_else(|| alias(&normal.replace('.', "_")));
    aliased
        .into_iter()
        .chain([normal.as_str()])
        .filter(|module| !module.is_empty() && !module.contains('.'))
        .find_map(|module| {
            let i = MODULES
                .binary_search_by(|(name, _)| (*name).cmp(module))
                .ok()?;
            Some(MODULES[i])
        })
}

/// The kind of the codec module named exactly `module`.
fn module_kind(module: &str) -> Option<Module> {
    MODULES
        .binary_search_by(|(name, _)| (*name).cmp(module))
        .ok()
        .map(|i| MODULES[i].1)
}

/// The line, from 1, that byte `at` of `bytes` is on: LF, CRLF and a lone CR each end one.
fn line_of(bytes: &[u8], at: usize) -> usize {
    let before = &bytes[..at];
    let lf = memchr::memchr_iter(b'\n', before).count();
    let lone_cr = memchr::memchr_iter(b'\r', before)
        .filter(|&i| bytes.get(i + 1) != Some(&b'\n'))
        .count();
    1 + lf + lone_cr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latin_1() -> Codec {
        Codec::named("latin_1").unwrap()
    }

    fn koi8_r() -> Codec {
        Codec::named("koi8_r").unwrap()
    }

    fn declared(body: &[u8]) -> Option<(&str, usize)> {
        declaration(body).map(|d| (d.name, d.line))
    }

    #[test]
    fn the_tables_are_sorted_for_binary_search() {
        assert!(MODULES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(ALIASES.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn every_single_byte_codec_spells_a_line_ending_one_way() {
        // A textual search spells CR and LF in the file's encoding to find line endings.
        for index in 0..SINGLE_BYTE.len() {
            let codec = Codec(index);
            for c in ['\r', '\n'] {
                assert_eq!(codec.spellings(c).count(), 1, "{codec:?} spells {c:?}");
            }
        }
    }

    #[test]
    fn a_declaration_on_the_first_line_is_read() {
        assert_eq!(
            declared(b"# -*- coding: latin-1 -*-\n"),
            Some(("latin-1", 1))
        );
        assert_eq!(
            declared(b"# vim: set fileencoding=koi8-r :\n"),
            Some(("koi8-r", 1))
        );
        assert_eq!(declared(b"#coding:utf8"), Some(("utf8", 1)));
        assert_eq!(
            declared(b"\t \x0c# coding: x.y_z-1\n"),
            Some(("x.y_z-1", 1))
        );
    }

    #[test]
    fn the_second_line_is_read_only_after_a_blank_or_comment_first_line() {
        assert_eq!(
            declared(b"#!/usr/bin/env python\n# coding: latin-1\n"),
            Some(("latin-1", 2))
        );
        assert_eq!(declared(b"\n# coding: latin-1\n"), Some(("latin-1", 2)));
        assert_eq!(declared(b"   \r\n# coding: latin-1"), Some(("latin-1", 2)));
        assert_eq!(declared(b"x = 1\n# coding: latin-1\n"), None);
        assert_eq!(declared(b"#\n#\n# coding: latin-1\n"), None);
    }

    #[test]
    fn a_lone_cr_ends_a_line() {
        assert_eq!(
            declared(b"#!\r# coding: latin-1\rx = 1\r"),
            Some(("latin-1", 2))
        );
    }

    #[test]
    fn a_spec_must_be_a_comment_with_only_whitespace_before_it() {
        assert_eq!(declared(b"x = 1  # coding: latin-1\n"), None);
        assert_eq!(declared(b"coding: latin-1\n"), None);
    }

    #[test]
    fn the_first_coding_with_a_name_wins() {
        assert_eq!(declared(b"# coding: \n"), None);
        assert_eq!(declared(b"# coding coding=koi8-r\n"), Some(("koi8-r", 1)));
        assert_eq!(
            declared(b"# coding: \x82 coding: cp1252\n"),
            Some(("cp1252", 1))
        );
        assert_eq!(declared(b"# coding:latin-1,koi8-r\n"), Some(("latin-1", 1)));
        // "coding" must be followed straight away by ":" or "=".
        assert_eq!(declared(b"# coding : latin-1\n"), None);
    }

    #[test]
    fn a_name_running_to_the_end_of_the_line_is_read() {
        assert_eq!(declared(b"#coding:a"), Some(("a", 1)));
        assert_eq!(declared(b"#coding:"), None);
        assert_eq!(declared(b"\n   #coding:a\n"), Some(("a", 2)));
        assert_eq!(declared(b"\n   #coding:\n"), None);
    }

    #[test]
    fn normal_names_are_cpythons() {
        assert_eq!(normal_name("UTF_8"), "utf-8");
        assert_eq!(normal_name("utf-8-sig"), "utf-8");
        // Only the first 12 bytes are folded, so a long name with the prefix is still UTF-8.
        assert_eq!(normal_name("utf-8-and-a-long-tail"), "utf-8");
        assert_eq!(normal_name("utf8"), "utf8");
        assert_eq!(normal_name("Latin_1"), "iso-8859-1");
        assert_eq!(normal_name("Iso-Latin-1-X"), "iso-8859-1");
        assert_eq!(normal_name("latin1"), "latin1");
        assert_eq!(normal_name("koi8-r"), "koi8-r");
    }

    #[test]
    fn names_resolve_as_cpythons_codec_lookup_does() {
        let codec = |name: &str| resolve(name).map(|(codec, _)| codec);
        assert_eq!(codec("iso-8859-1"), Some("latin_1"));
        assert_eq!(codec("KOI8-R"), Some("koi8_r"));
        assert_eq!(codec("koi8__r"), Some("koi8_r"));
        assert_eq!(codec("-koi8-r-"), Some("koi8_r"));
        assert_eq!(codec("iso8859.1"), Some("latin_1"));
        assert_eq!(codec("utf8"), Some("utf_8"));
        assert_eq!(codec("u8"), Some("utf_8"));
        assert_eq!(codec("cp1252"), Some("cp1252"));
        assert_eq!(codec("windows-1252"), Some("cp1252"));
        assert_eq!(codec("sjis"), Some("shift_jis"));
        assert_eq!(codec("rot13"), Some("rot_13"));
        assert_eq!(codec("uft-8"), None);
        // Windows' own codecs exist only on Windows, so elsewhere CPython does not know them.
        assert_eq!(codec("mbcs"), None);
        assert_eq!(codec("encodings.latin_1"), None);
    }

    #[test]
    fn single_byte_codecs_decode_as_cpython_does() {
        assert_eq!(latin_1().decode(0xE9), Some('é'));
        assert_eq!(latin_1().decode(0x80), Some('\u{80}'));
        assert_eq!(koi8_r().decode(0xC1), Some('а'));
        let cp1252 = Codec::named("cp1252").unwrap();
        assert_eq!(cp1252.decode(0x80), Some('€'));
        assert_eq!(cp1252.decode(0x81), None);
        let ascii = Codec::named("ascii").unwrap();
        assert_eq!(ascii.decode(0x7F), Some('\u{7F}'));
        assert_eq!(ascii.decode(0x80), None);
        assert_eq!(Codec::named("shift_jis"), None);
        assert_eq!(format!("{:?}", koi8_r()), "koi8_r");
    }

    #[test]
    fn a_character_may_have_no_spelling_or_several() {
        assert_eq!(koi8_r().spellings('é').count(), 0);
        let mac_arabic = Codec::named("mac_arabic").unwrap();
        assert_eq!(mac_arabic.spellings(' ').collect::<Vec<_>>(), [0x20, 0xA0]);
    }

    fn decoded(bytes: &[u8]) -> Result<Source, String> {
        Source::decode(bytes).map_err(|refusal| refusal.to_string())
    }

    #[test]
    fn undeclared_source_is_utf8() {
        let source = Source::decode("x = 'é'\n".as_bytes()).unwrap();
        assert_eq!(source.text, "x = 'é'\n");
        assert_eq!(source.encoding, SourceEncoding::Utf8);
        assert!(!source.byte_order_mark);
    }

    #[test]
    fn declared_source_decodes_by_its_declaration() {
        let source = Source::decode(b"# coding: latin-1\nx = '\xe9'\n").unwrap();
        assert_eq!(source.text, "# coding: latin-1\nx = 'é'\n");
        assert_eq!(source.encoding, SourceEncoding::SingleByte(latin_1()));
        let source =
            Source::decode(b"#!/usr/bin/env python\n# coding: koi8-r\nx = '\xc1'\n").unwrap();
        assert_eq!(
            source.text,
            "#!/usr/bin/env python\n# coding: koi8-r\nx = 'а'\n"
        );
        let source = Source::decode(b"# coding: utf8\nx = '\xc3\xa9'\n").unwrap();
        assert_eq!(source.encoding, SourceEncoding::Utf8);
        assert_eq!(source.text, "# coding: utf8\nx = 'é'\n");
    }

    #[test]
    fn a_byte_order_mark_is_kept_out_of_the_text_and_written_back() {
        let bytes = b"\xef\xbb\xbf# coding: utf-8\nx = '\xc3\xa9'\n";
        let source = Source::decode(bytes).unwrap();
        assert!(source.byte_order_mark);
        assert_eq!(source.text, "# coding: utf-8\nx = 'é'\n");
        assert_eq!(source.encode(&source.text).unwrap(), bytes);
    }

    #[test]
    fn decoded_source_encodes_back_to_its_own_bytes() {
        for bytes in [
            &b"# coding: latin-1\r\nx = '\xe9\xff\x80'\r"[..],
            &b"# coding: koi8-r\nx = '\xc1\xd2'\n"[..],
            &b"# coding: cp1252\nx = '\x80'\n"[..],
            "x = 'é'\n".as_bytes(),
        ] {
            let source = Source::decode(bytes).unwrap();
            assert_eq!(source.encode(&source.text).unwrap(), bytes);
        }
    }

    #[test]
    fn text_the_encoding_cannot_hold_is_not_encoded() {
        let source = Source::decode(b"# coding: koi8-r\n").unwrap();
        let err = source.encode("é").unwrap_err();
        assert_eq!(err.to_string(), "'é' cannot be written in \"koi8_r\"");
        let source = Source::decode(b"# coding: mac-arabic\n").unwrap();
        let err = source.encode(" ").unwrap_err();
        assert_eq!(
            err.to_string(),
            "' ' has more than one spelling in \"mac_arabic\", so which to write is not known"
        );
    }

    #[test]
    fn each_refusal_says_why() {
        assert_eq!(
            decoded(b"x = 1\n# two\nx\x00\n"),
            Err("line 3 has a NUL byte, which Python source cannot contain".into())
        );
        // The shape of CPython's tokenizedata/bad_coding.py.
        assert_eq!(
            decoded(b"# -*- coding: uft-8 -*-\nprint('h\xc3\xa9')\n"),
            Err("line 1 declares the encoding \"uft-8\", which Python does not know".into())
        );
        assert_eq!(
            decoded(b"#!\n# coding: rot13\n"),
            Err(
                "line 2 declares the encoding \"rot13\", which is a Python codec but not a \
                 text encoding"
                    .into()
            )
        );
        assert_eq!(
            decoded(b"# coding: undefined\n"),
            Err(
                "line 1 declares the encoding \"undefined\", Python's codec that decodes \
                 nothing"
                    .into()
            )
        );
        assert_eq!(
            decoded(b"# coding: sjis\n"),
            Err(
                "line 1 declares the encoding \"sjis\", which Python reads and quaff does \
                 not: quaff reads UTF-8 and single-byte encodings"
                    .into()
            )
        );
        // The shape of CPython's tokenizedata/bad_coding2.py.
        assert_eq!(
            decoded(b"\xef\xbb\xbf#coding: utf8\nprint('\xe6\x88\x91')\n"),
            Err(
                "it starts with a UTF-8 byte-order mark, but line 1 declares the encoding \
                 \"utf8\"; with a mark, Python accepts only a declaration of \"utf-8\""
                    .into()
            )
        );
        // The shape of CPython's tokenizedata/badsyntax_pep3120.py.
        assert_eq!(
            decoded(b"print('\xf6sterreich')\n"),
            Err(
                "byte 0xf6 on line 1 is not valid UTF-8, and no encoding is declared; declare \
                 one on the first or second line as PEP 263 says, or re-encode the file as \
                 UTF-8"
                    .into()
            )
        );
        assert_eq!(
            decoded(b"\xef\xbb\xbfx = 1\r\ny = '\xe9'\n"),
            Err(
                "byte 0xe9 on line 2 is not valid UTF-8, which the file's byte-order mark \
                 declares"
                    .into()
            )
        );
        assert_eq!(
            decoded(b"# coding: utf-8\nx = 1\ry = '\xe9'\n"),
            Err("byte 0xe9 on line 3 is not valid \"utf-8\", the encoding line 1 declares".into())
        );
        assert_eq!(
            decoded(b"# coding: cp1252\nx = '\x81'\n"),
            Err("byte 0x81 on line 2 is not valid \"cp1252\", the encoding line 1 declares".into())
        );
    }

    #[test]
    fn a_nul_is_refused_before_the_declaration_is_read() {
        assert_eq!(
            Source::decode(b"# coding: uft-8\n\x00"),
            Err(Refusal::Nul { line: 2 })
        );
    }

    #[test]
    fn a_byte_order_mark_allows_utf8_declared_any_way_cpython_normalises_to_it() {
        for declaration in ["utf-8", "UTF_8", "utf-8-sig", "Utf_8-Whatever"] {
            let bytes = format!("\u{feff}# coding: {declaration}\nx = 1\n");
            let source = Source::decode(bytes.as_bytes()).unwrap();
            assert!(source.byte_order_mark, "{declaration}");
        }
        assert!(matches!(
            Source::decode(b"\xef\xbb\xbf#\n# coding: latin-1\n"),
            Err(Refusal::MarkConflict { line: 2, .. })
        ));
    }

    #[test]
    fn a_declaration_is_read_whatever_the_rest_holds() {
        assert_eq!(declared_encoding(b"x = '\xe9'\n"), Ok(None));
        assert_eq!(
            declared_encoding(b"# coding: koi8-r\n\xff"),
            Ok(Some(SourceEncoding::SingleByte(koi8_r())))
        );
        // Bytes UTF-8 cannot decode are a reader's to tolerate, not a refusal of the
        // declaration.
        assert_eq!(
            declared_encoding(b"# coding: utf-8\n\xff"),
            Ok(Some(SourceEncoding::Utf8))
        );
        assert_eq!(
            declared_encoding(b"\xef\xbb\xbf# coding: utf-8\n"),
            Ok(Some(SourceEncoding::Utf8))
        );
        assert!(matches!(
            declared_encoding(b"# coding: uft-8\n"),
            Err(Refusal::Unknown { .. })
        ));
        assert!(matches!(
            declared_encoding(b"\xef\xbb\xbf# coding: latin-1\n"),
            Err(Refusal::MarkConflict { .. })
        ));
    }
}
