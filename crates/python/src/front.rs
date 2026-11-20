// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Python's front end: the one place the engine reaches Python through.
//!
//! Each answer is one this crate already gives, or quaff's own reading of a Python file's bytes
//! in `quaffed-encoding`, which reads them as `CPython` does: Python's front end uses both tools,
//! and the engine sees neither.

use quaffed_encoding::{Refusal, Source, SourceEncoding, python};
use quaffed_syntax::{
    Arena, CompileError, FrontEnd, Language, Lexed, Misplaced, ParseError, Pattern, Rewrite,
    StringPart,
};

use crate::{
    PYTHON, code_tokens, compile_pattern, compile_rewrite, escaped, lex, misplaced, parse_module,
    token_kinds,
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

    /// PEP 263's coding line, on the first or second line, read as `CPython` reads it.
    fn declared_encoding(
        &self,
        bytes: &[u8],
    ) -> Option<Result<Option<(SourceEncoding, usize)>, Refusal>> {
        Some(python::declared_encoding(bytes))
    }

    /// As `CPython` decodes a file, refusing what it refuses, whatever else declares the file's
    /// encoding for the textual tier.
    fn decode(&self, bytes: &[u8]) -> Result<Source, Refusal> {
        Source::decode(bytes)
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
        assert_eq!(
            Python.declared_encoding(b"# -*- coding: latin-1 -*-\nx = 1\n"),
            Some(python::declared_encoding(
                b"# -*- coding: latin-1 -*-\nx = 1\n"
            ))
        );
        assert_eq!(Python.declared_encoding(b"x = 1\n"), Some(Ok(None)));
    }
}
