// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What a rewrite needs to know of Python text that the arena leaves out: where its comments
//! and multi-line strings are, the unit it indents by, and the kinds of its tokens.
//!
//! Text is lexed as a module without regard to whether it parses, since a rewrite template is a
//! fragment with holes in it, and a fragment is lexed only to compare it with itself.

use std::ops::Range;

use ruff_python_ast::token::TokenKind;
use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::Ranged;

/// The parts of Python text a rewrite must handle with care.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lexed {
    /// Every comment's byte range, `#` included, in order.
    pub comments: Vec<Range<usize>>,
    /// The byte range of every string, or f-string or t-string literal part, that spans more than
    /// one line, in order: a line starting inside one is part of the string's value, so it is
    /// never re-indented.
    pub multi_line_strings: Vec<Range<usize>>,
    /// The whitespace the first indented block is indented by, if any block is.
    pub indent_unit: Option<String>,
}

/// `text` lexed for the parts a rewrite must handle with care.
#[must_use]
pub fn lex(text: &str) -> Lexed {
    let parsed = ruff_python_parser::parse_unchecked(text, ParseOptions::from(Mode::Module));
    let mut lexed = Lexed::default();
    for token in parsed.tokens() {
        let range = usize::from(token.start())..usize::from(token.end());
        match token.kind() {
            TokenKind::Comment => lexed.comments.push(range),
            TokenKind::String | TokenKind::FStringMiddle | TokenKind::TStringMiddle
                if text[range.clone()].contains(['\n', '\r']) =>
            {
                lexed.multi_line_strings.push(range);
            }
            TokenKind::Indent if lexed.indent_unit.is_none() => {
                // The first indent opens the first indented block, so its line's whitespace is
                // one level of it.
                let line_start = text[..range.end].rfind(['\n', '\r']).map_or(0, |i| i + 1);
                let unit: String = text[line_start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                if !unit.is_empty() {
                    lexed.indent_unit = Some(unit);
                }
            }
            _ => {}
        }
    }
    lexed
}

/// The kind of each token in `text`, lexed as a module, without layout: what a value written into
/// a literal must leave as it was, so that it cannot close the literal or open a field in it.
#[must_use]
pub fn token_kinds(text: &str) -> Vec<String> {
    let parsed = ruff_python_parser::parse_unchecked(text, ParseOptions::from(Mode::Module));
    parsed
        .tokens()
        .iter()
        .map(ruff_python_ast::token::Token::kind)
        .filter(|kind| {
            !matches!(
                kind,
                TokenKind::NonLogicalNewline
                    | TokenKind::Newline
                    | TokenKind::Indent
                    | TokenKind::Dedent
                    | TokenKind::EndOfFile
            )
        })
        .map(|kind| format!("{kind:?}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_multi_line_strings_are_found_and_one_line_strings_are_not() {
        let text = "x = 1  # one\ns = '''a\nb'''\nt = 'c'\n# two\n";
        let lexed = lex(text);
        let comments: Vec<&str> = lexed.comments.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(comments, ["# one", "# two"]);
        let strings: Vec<&str> = lexed
            .multi_line_strings
            .iter()
            .map(|r| &text[r.clone()])
            .collect();
        assert_eq!(strings, ["'''a\nb'''"]);
    }

    #[test]
    fn the_indent_unit_is_the_first_blocks() {
        assert_eq!(
            lex("if a:\n  b()\n  if c:\n    d()\n")
                .indent_unit
                .as_deref(),
            Some("  ")
        );
        assert_eq!(
            lex("def f():\n\treturn 1\n").indent_unit.as_deref(),
            Some("\t")
        );
        assert_eq!(lex("x = 1\n").indent_unit, None);
    }

    #[test]
    fn a_value_that_closes_a_string_changes_its_token_kinds() {
        assert_eq!(token_kinds("'a x b'"), token_kinds("'a yy b'"));
        assert_ne!(token_kinds("'a x b'"), token_kinds("'a ' + y + ' b'"));
        assert_ne!(token_kinds("f'a x b'"), token_kinds("f'a {y} b'"));
    }

    #[test]
    fn a_fragment_with_holes_still_lexes() {
        assert_ne!(token_kinds("call($a)"), Vec::<String>::new());
    }
}
