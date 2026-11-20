// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The front end: what the engine asks of a language, and the types the answers come in.
//!
//! Each language's adapter implements [`FrontEnd`] once, whatever tools it uses to answer, and
//! the engine reaches the language only through it, so a language is added without changing the
//! engine.  Everything that crosses is quaff's own type, defined here, never a parser's.
//!
//! A question added later is a method with a default answer, so every language already written
//! compiles unchanged and takes the default.  A default is given only where an honest one
//! exists, such as a language that declares no encoding, and it never claims more than the
//! language was asked to support.  `architecture/syntax.md` describes the interface and what a new
//! language implements.

use std::fmt;
use std::ops::Range;
use std::path::Path;

use quaffed_encoding::{Source, SourceEncoding};

use crate::{Arena, Kind, Language, Pattern, TooLong};

/// A language, as the engine reaches it: which files are its own, how they are read, and how a
/// pattern or a rewrite in it is compiled.
///
/// Every method answers for text in this language alone.  Only [`FrontEnd::claims`] is about a
/// file; the rest take text, so the same questions can be asked of code embedded in another
/// language's file once a scope can name it.
pub trait FrontEnd: Sync {
    /// The language every arena this front end builds carries.
    fn language(&self) -> Language;

    /// The extensions that name its files, without the dot, as the help lists them.
    fn extensions(&self) -> &'static [&'static str];

    /// Every kind of node its arenas hold, as its parser names them: what the engine never
    /// spells, which a test in the binary holds it to.
    fn kinds(&self) -> &'static [Kind];

    /// The language's rules for statements, lists and layout, as an edit needs them.  There is
    /// no default: every language states its own.
    fn editing(&self) -> &'static Editing;

    /// Whether the file at `path` is one of this language's, by its name alone.
    fn claims(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| self.extensions().contains(&extension))
    }

    /// The encoding a file of this language declares in its own text, and the line that declares
    /// it: `Ok(None)` where it declares none, and `None` where the language has no such
    /// declaration at all, which is the default.
    ///
    /// # Errors
    ///
    /// Returns the [`Refused`] the language's own reader would give for the declaration.
    fn declared_encoding(&self, bytes: &[u8]) -> Option<Result<Option<Declared>, Refused>> {
        let _ = bytes;
        None
    }

    /// A whole file's bytes decoded as the language's own reader decodes them, with what it takes
    /// to write the text back.
    ///
    /// # Errors
    ///
    /// Returns the [`Refused`] the language's own reader would give.  Nothing partial is
    /// returned.
    fn decode(&self, bytes: &[u8]) -> Result<Source, Refused>;

    /// `text`, a whole file's worth, parsed into an arena.
    ///
    /// # Errors
    ///
    /// Returns the first [`ParseError`] if `text` is not valid in the language, or is too long
    /// to parse.  Nothing partial is returned.
    fn parse<'a>(&self, text: &'a str) -> Result<Arena<'a>, ParseError>;

    /// A backticked pattern compiled for matching files of this language.
    ///
    /// # Errors
    ///
    /// Returns a [`CompileError`] where the pattern is not one in this language, with its place
    /// in the pattern as written where it has one.
    fn compile_pattern(&self, text: &str) -> Result<Pattern, CompileError>;

    /// A rewrite template compiled against the pattern whose captures it writes back.
    ///
    /// # Errors
    ///
    /// Returns a [`CompileError`] where the template is not one in this language or writes a
    /// capture the pattern does not make, with its place in the template where it has one.
    fn compile_rewrite(&self, text: &str, pattern: &Pattern) -> Result<Rewrite, CompileError>;

    /// The parts of `text` a rewrite must handle with care, whether or not it parses, since a
    /// rewrite template is a fragment with holes in it.
    fn lex(&self, text: &str) -> Lexed;

    /// The text of each token in `text` that carries code, in order: comments, line breaks and
    /// indentation left out.
    fn code_tokens<'a>(&self, text: &'a str) -> Vec<&'a str>;

    /// The kind of each token in `text`, in order, as the language's lexer names it.
    fn token_kinds(&self, text: &str) -> Vec<String>;

    /// `value` written into `part` so that the literal holds it as text, or `None` where the
    /// literal cannot hold it.  The default is `None`: a language that says nothing of its
    /// strings has nothing written into one, and such an edit is refused.
    fn escaped(&self, value: &str, part: &StringPart) -> Option<String> {
        let _ = (value, part);
        None
    }

    /// Everything in `text`, parsed as a whole file, that stands outside the construct it needs,
    /// in source order.  The default is none: a language with no such construct.
    ///
    /// # Errors
    ///
    /// Returns the first [`ParseError`] if `text` does not parse.  Nothing partial is returned.
    fn misplaced(&self, text: &str) -> Result<Vec<Misplaced>, ParseError> {
        let _ = text;
        Ok(Vec::new())
    }
}

/// Why text could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The parser refused the text: its message, verbatim, and the byte offset where the error
    /// starts.
    Invalid {
        /// What the parser said.
        message: String,
        /// The byte offset in the text.
        offset: usize,
    },
    /// The text is longer than an arena's offsets reach, so it was not parsed.
    TooLong(TooLong),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Invalid { message, offset } => write!(f, "at byte {offset}: {message}"),
            ParseError::TooLong(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Invalid { .. } => None,
            ParseError::TooLong(err) => Some(err),
        }
    }
}

impl From<TooLong> for ParseError {
    fn from(err: TooLong) -> Self {
        ParseError::TooLong(err)
    }
}

/// An encoding a file declares in its own text, as its language reads the declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// What the language calls such a declaration, as a message names it: for Python, "coding
    /// line".
    pub what: &'static str,
    /// The line it is on, from 1.
    pub line: usize,
    /// The encoding it declares.
    pub encoding: DeclaredEncoding,
}

/// The encoding a [`Declared`] names, as far as quaff reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredEncoding {
    /// One quaff reads.
    Read(SourceEncoding),
    /// One the language reads and quaff does not: its name as the file spells it, and the codec
    /// the name is quaff's name for.  Such a file is of unknown encoding rather than refused,
    /// since its language would read it.
    Unread {
        /// The name, as the file spells it.
        name: String,
        /// The codec, by its `CPython` name, as quaff names every codec.
        codec: &'static str,
    },
}

/// Why a language's own reader refuses a file: what it would say, in the language's own terms,
/// as a message gives it after the file's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    message: String,
}

impl Refused {
    /// The refusal `message` says.
    #[must_use]
    pub fn new(message: String) -> Refused {
        Refused { message }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

/// Why a pattern or a rewrite template could not be compiled: the language's own message, and
/// where in the text as written the error is, if it has a place.  The message leaves the place
/// out, for the caller to give in terms its reader knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    message: String,
    offset: Option<usize>,
}

impl CompileError {
    /// The error `message` says, at byte `offset` of the text as written if it has a place.
    #[must_use]
    pub fn new(message: String, offset: Option<usize>) -> CompileError {
        CompileError { message, offset }
    }

    /// Where in the text as written the error is, as a byte offset, if it has a place.
    #[must_use]
    pub fn offset(&self) -> Option<usize> {
        self.offset
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompileError {}

/// The parts of a language's text a rewrite must handle with care, which an arena leaves out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lexed {
    /// Every comment's byte range, its opening marker included, in order.
    pub comments: Vec<Range<usize>>,
    /// The byte range of every string, or literal part of an interpolated string, that spans
    /// more than one line, in order: a line starting inside one is part of the string's value,
    /// so it is never re-indented.
    pub multi_line_strings: Vec<Range<usize>>,
    /// The whitespace the first indented block is indented by, if any block is.
    pub indent_unit: Option<String>,
    /// Every pair of brackets, round, square or curly, in the order they open.  Braces that
    /// open and close an interpolated string's fields are not brackets but part of the string.
    pub brackets: Vec<Bracket>,
    /// Where each logical line starts: the first token of the text, and the first after each
    /// logical line ends, in order.  A line continued inside a bracket or by the language's own
    /// continuation is not one, so its indentation says nothing of depth.
    pub logical_starts: Vec<usize>,
    /// Every part of a string whose text is the string's own - a plain string, or an
    /// interpolated string's literal text between fields - with how its literal is written, in
    /// order.
    pub strings: Vec<StringPart>,
}

/// One part of a string literal and how the literal is written, which decides how text can be
/// written into it as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringPart {
    /// Its byte range: a plain string's whole token, prefix and quotes included, or the literal
    /// text of an interpolated string between its fields.
    pub range: Range<usize>,
    /// Whether the literal is raw, its backslashes text rather than escapes.
    pub raw: bool,
    /// Whether it is a bytes literal, which holds ASCII only.
    pub bytes: bool,
    /// Whether it is an interpolated string, in which a brace opens a field.
    pub formatted: bool,
    /// Its quotes: one quote character, or three.
    pub quote: String,
}

/// One pair of brackets in a language's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bracket {
    /// The byte offset of the opening bracket.
    pub open: usize,
    /// The byte offset of the closing bracket.
    pub close: usize,
    /// The byte offset of a comma that is the last token before the closing bracket, comments
    /// and line breaks aside, if there is one.
    pub trailing_comma: Option<usize>,
}

/// A statement or expression outside the construct it needs: what it is, as the language spells
/// it, what it needs, and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misplaced {
    /// What it is, as the language spells it, such as `break`.
    pub what: &'static str,
    /// What it needs to stand inside, as a message names it after "outside", such as "a loop".
    pub needs: &'static str,
    /// The byte offset in the text where it starts.
    pub offset: usize,
}

/// A language's rules for statements, lists and layout, which decide how an edit is turned into
/// text: what the arena does not say, since it holds only kinds, ranges and tokens.
///
/// What the arena does say is asked of it instead.  A statement is a node in one of the
/// statement lists the adapter marks, and a statement holds a block where a statement stands
/// inside it.  What an empty block needs is the language's own parse check, which refuses an
/// edit that leaves one its language does not allow.
///
/// Its fields are public and it has no constructor, so a field added later is a compile error in
/// every adapter until each states it: there is no honest default for any of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editing {
    /// The kinds whose items a deletion may simply leave out where it takes no separator, the
    /// rest standing as they were: in Python, `[debug]` becomes `[]`.  A kind left out is one
    /// where what is left would parse and mean something else, so such a deletion is refused.
    pub droppable_items: &'static [Kind],
    /// The kinds that need a separator after their only item, which without one would be that
    /// item alone: Python's tuple, `(a,)`.  A deletion that leaves one item writes it.
    pub lone_item_separated: &'static [Kind],
    /// What separates the items of a list: Python's comma.  A deletion takes it across line
    /// breaks, since it separates an item from its neighbour wherever the lines fall.
    pub item_separator: char,
    /// What separates statements sharing a line, if anything does: Python's semicolon.  A
    /// deletion takes it only from the match's own line.
    pub statement_separator: Option<char>,
    /// What continues a line onto the next when written just before its line break, if anything
    /// does: Python's backslash.  A line break after it is layout, not the end of a statement.
    pub line_continuation: Option<char>,
    /// What ends a header whose block may be written on the header's own line, as in Python's
    /// `if x: a()`, where indentation decides blocks.  `None` for a language whose blocks are
    /// not decided by layout: statements sharing a line are then never given lines of their own.
    pub header_block_opener: Option<char>,
    /// The bracket that only groups, which a node's range leaves out where it wraps one: Python's
    /// round bracket.  Every other bracket belongs to the node it opens.
    pub grouping_bracket: char,
    /// The words that can stand outside the construct they need, as [`FrontEnd::misplaced`]
    /// finds them, which a text search looks for in what an edit wrote before the parse it asks
    /// for: an edit that writes none of them leaves none outside.
    pub misplaceable: &'static [&'static str],
}

/// A compiled rewrite template: the template as written, and where in it each captured name is
/// written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    text: String,
    sites: Vec<Site>,
    has_code: bool,
}

impl Rewrite {
    /// The template `text`, writing back a captured name at each of `sites`, in the order
    /// written; `has_code` says whether it has code rather than being empty or only whitespace
    /// and comments.
    #[must_use]
    pub fn new(text: String, sites: Vec<Site>, has_code: bool) -> Rewrite {
        Rewrite {
            text,
            sites,
            has_code,
        }
    }

    /// The template as written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where each captured name is written back, in the order written.
    #[must_use]
    pub fn sites(&self) -> &[Site] {
        &self.sites
    }

    /// Whether the template has code, rather than being empty or only whitespace and comments.
    /// One with none deletes what matched, so the separator and the line around each match are
    /// the caller's to remove, where nothing in the template says what becomes of them.
    #[must_use]
    pub fn has_code(&self) -> bool {
        self.has_code
    }
}

/// One place in a template where a captured name is written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The stretch of the template it replaces: `$name`, `$name...` or `${name}`.
    pub written: Range<usize>,
    /// The name, without `$`.
    pub name: String,
    /// Whether it stands inside a string, an interpolated string's literal part, or a comment,
    /// where it interpolates, rather than in code.
    pub in_literal: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // The defaults and `claims` are tested over a front end in the binary, which has languages
    // to answer the rest: `crates/quaffed/src/languages.rs`.

    #[test]
    fn a_compile_error_says_its_message_and_keeps_its_place_apart() {
        let err = CompileError::new("not a pattern".to_owned(), Some(3));
        assert_eq!(err.to_string(), "not a pattern");
        assert_eq!(err.offset(), Some(3));
        assert_eq!(CompileError::new("empty".to_owned(), None).offset(), None);
    }
}
