// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Python's front end: the one place the engine reaches Python through.
//!
//! Each answer is one this crate already gives, or quaff's own reading of a Python file's bytes
//! in `quaffed-encoding`, which reads them as `CPython` does: Python's front end uses both tools,
//! and the engine sees neither.

use quaffed_encoding::{Refusal, Source, python};
use quaffed_syntax::{
    Arena, CompileError, Declared, DeclaredEncoding, Editing, FrontEnd, Kind, Language, Lexed,
    Misplaced, ParseError, Pattern, Refused, Rewrite, StringPart,
};
use ruff_python_ast::NodeKind;

use crate::kinds::kind_of;
use crate::{
    PYTHON, all_kinds, code_tokens, compile_pattern, compile_rewrite, escaped, lex, misplaced,
    parse_module, token_kinds,
};

/// Python's rules for statements, lists and layout, as an edit needs them.
///
/// - A list, a tuple and a call's arguments can each lose an item and still mean the rest: `[]`,
///   `()` and `f()` are what they were less one.  A set cannot, since `{}` is a dict, and a
///   parameter is not among them because no pattern of its own matches one.
/// - A tuple of one item needs its comma, `(a,)`, or it is that item alone.
/// - Blocks are decided by indentation, and a block may follow its header's colon on the
///   header's line, `if x: a(); b()`, after which statements share the line.
/// - The keywords are those [`misplaced`] finds outside what they need; `async` stands for
///   `async with` and `async for`.
static EDITING: Editing = Editing {
    droppable_items: &[
        kind_of(NodeKind::ExprList),
        kind_of(NodeKind::ExprTuple),
        kind_of(NodeKind::Arguments),
    ],
    lone_item_separated: &[kind_of(NodeKind::ExprTuple)],
    item_separator: ',',
    statement_separator: Some(';'),
    line_continuation: Some('\\'),
    header_block_opener: Some(':'),
    grouping_bracket: '(',
    misplaceable: &["return", "yield", "await", "async", "break", "continue"],
};

/// Python, as the engine reaches it.
#[derive(Debug, Clone, Copy)]
pub struct Python;

impl FrontEnd for Python {
    fn language(&self) -> Language {
        PYTHON
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi", "pyw"]
    }

    fn kinds(&self) -> &'static [Kind] {
        all_kinds()
    }

    fn editing(&self) -> &'static Editing {
        &EDITING
    }

    /// PEP 263's coding line, on the first or second line, read as `CPython` reads it.  A codec
    /// `CPython` reads and quaff does not is an encoding quaff does not read rather than a
    /// refusal, since Python would run the file.
    fn declared_encoding(&self, bytes: &[u8]) -> Option<Result<Option<Declared>, Refused>> {
        let declared = |line, encoding| Declared {
            what: "coding line",
            line,
            encoding,
        };
        Some(match python::declared_encoding(bytes) {
            Ok(None) => Ok(None),
            Ok(Some((encoding, line))) => {
                Ok(Some(declared(line, DeclaredEncoding::Read(encoding))))
            }
            Err(Refusal::Unread { name, codec, line }) => Ok(Some(declared(
                line,
                DeclaredEncoding::Unread { name, codec },
            ))),
            Err(refusal) => Err(Refused::new(refusal.to_string())),
        })
    }

    /// As `CPython` decodes a file, refusing what it refuses, whatever else declares the file's
    /// encoding for the textual tier.
    fn decode(&self, bytes: &[u8]) -> Result<Source, Refused> {
        python::decode(bytes).map_err(|refusal| Refused::new(refusal.to_string()))
    }

    fn parse<'a>(&self, text: &'a str) -> Result<Arena<'a>, ParseError> {
        parse_module(text)
    }

    fn compile_pattern(&self, text: &str) -> Result<Pattern, CompileError> {
        compile_pattern(text).map_err(|err| CompileError::new(err.to_string(), err.offset()))
    }

    fn compile_rewrite(&self, text: &str, pattern: &Pattern) -> Result<Rewrite, CompileError> {
        compile_rewrite(text, pattern)
            .map_err(|err| CompileError::new(err.to_string(), err.offset()))
    }

    fn lex(&self, text: &str) -> Lexed {
        lex(text)
    }

    fn code_tokens<'a>(&self, text: &'a str) -> Vec<&'a str> {
        code_tokens(text)
    }

    fn token_kinds(&self, text: &str) -> Vec<String> {
        token_kinds(text)
    }

    fn escaped(&self, value: &str, part: &StringPart) -> Option<String> {
        escaped(value, part)
    }

    fn misplaced(&self, text: &str) -> Result<Vec<Misplaced>, ParseError> {
        misplaced(text)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn python_claims_its_three_extensions_and_nothing_else() {
        for name in ["a.py", "src/a.pyi", "a.pyw"] {
            assert!(Python.claims(Path::new(name)), "{name}");
        }
        for name in ["a.pyc", "a.PY", "py", "a.py.txt", "a.ipynb"] {
            assert!(!Python.claims(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn a_compile_error_keeps_the_adapter_s_message_and_place() {
        let err = compile_pattern("f($x").unwrap_err();
        let through = Python.compile_pattern("f($x").unwrap_err();
        assert_eq!(through.to_string(), err.to_string());
        assert_eq!(through.offset(), err.offset());
        let pattern = compile_pattern("f($x)").unwrap();
        let err = compile_rewrite("g($y)", &pattern).unwrap_err();
        let through = Python.compile_rewrite("g($y)", &pattern).unwrap_err();
        assert_eq!(through.to_string(), err.to_string());
        assert_eq!(through.offset(), err.offset());
    }

    #[test]
    fn the_coding_line_is_read_as_cpython_reads_it() {
        let Some(Ok(Some(declared))) =
            Python.declared_encoding(b"#!/usr/bin/env python\n# coding: latin-1\n")
        else {
            panic!("a coding line on the second line is not read")
        };
        let Ok(Some((encoding, 2))) =
            python::declared_encoding(b"#!/usr/bin/env python\n# coding: latin-1\n")
        else {
            panic!("CPython's reading changed")
        };
        assert_eq!((declared.what, declared.line), ("coding line", 2));
        assert_eq!(declared.encoding, DeclaredEncoding::Read(encoding));
        assert_eq!(Python.declared_encoding(b"x = 1\n"), Some(Ok(None)));
    }

    #[test]
    fn a_codec_python_reads_and_quaff_does_not_is_declared_and_unread() {
        let Some(Ok(Some(declared))) = Python.declared_encoding(b"# coding: utf-16\n") else {
            panic!("an unread codec is not a declaration")
        };
        assert_eq!(
            declared.encoding,
            DeclaredEncoding::Unread {
                name: "utf-16".to_owned(),
                codec: "utf_16"
            }
        );
    }

    #[test]
    fn a_refusal_says_what_cpython_s_reader_says() {
        for bytes in [&b"# coding: nonsense\n"[..], b"x = 1\0\n"] {
            let refusal = python::decode(bytes).unwrap_err().to_string();
            assert_eq!(Python.decode(bytes).unwrap_err().to_string(), refusal);
        }
        let refusal = python::declared_encoding(b"# coding: nonsense\n")
            .unwrap_err()
            .to_string();
        assert_eq!(
            Python.declared_encoding(b"# coding: nonsense\n"),
            Some(Err(Refused::new(refusal)))
        );
    }
}
