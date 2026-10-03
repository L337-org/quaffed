// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The quaff script language, for the MVP subset: from text to the operation representation.
//!
//! [`parse_source`] reads one source - a script file, standard input, or one `-e` expression -
//! and appends its statements to a [`Program`].  Sources compose in the order they are parsed.
//! Inline text goes through the same parser as a file and is never split on `;` first, because
//! an operand may contain one.  `architecture/script.md` specifies the grammar as built.

pub mod lexer;
pub mod location;
pub mod parser;

use quaffed_representation::program::Program;
use quaffed_representation::span::Source;

pub use parser::{ErrorKind, ParseError};

/// Parses `text`, which came from `source`, appending its statements to `program`.
///
/// The source is added to `program.sources` first, so spans in the statements - and in the
/// error - index it.
///
/// # Errors
///
/// Returns the first [`ParseError`] in the text: [`ErrorKind::Malformed`] for a script that
/// must be fixed, exit 3, or [`ErrorKind::Unknown`] for a statement or construct this build
/// does not have, exit 5.  Statements before the error have already been appended; a caller
/// that fails on the error discards the program.
pub fn parse_source(program: &mut Program, source: Source, text: &str) -> Result<(), ParseError> {
    program.sources.push(source);
    let index = program.sources.len() - 1;
    let tokens = lexer::tokens(text).map_err(|err| ParseError {
        kind: ErrorKind::Malformed,
        message: err.message,
        start: err.start,
        end: err.end,
    })?;
    parser::parse(&tokens, index, program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quaffed_representation::program::Statement;

    #[test]
    fn each_source_is_registered_and_statements_append_in_order() {
        let mut program = Program::default();
        parse_source(&mut program, Source::Expression(1), "find \"a\"").unwrap();
        parse_source(&mut program, Source::Stdin, "find \"b\"; find \"c\"").unwrap();
        assert_eq!(program.sources, [Source::Expression(1), Source::Stdin]);
        assert_eq!(program.body().len(), 3);
        assert_eq!(program.body()[0].span.source, 0);
        assert_eq!(program.body()[2].span.source, 1);
        assert!(matches!(program.body()[2].node, Statement::Find(_)));
    }

    #[test]
    fn a_lexer_error_is_a_malformed_script() {
        let mut program = Program::default();
        let err = parse_source(&mut program, Source::Stdin, "find \"x").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Malformed);
        assert_eq!(err.start, 5);
    }
}
