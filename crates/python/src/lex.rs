// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What a rewrite needs to know of Python text that the arena leaves out: where its comments,
//! multi-line strings and brackets are, the unit it indents by, and the kinds of its tokens.
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
    /// Every pair of brackets, round, square or curly, in the order they open.  The braces that
    /// open and close an f-string's or t-string's fields are not brackets but part of the string.
    pub brackets: Vec<Bracket>,
}

/// One pair of brackets in Python text.
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

/// What a bracket's opening token is inside, for pairing it with its closing one.
enum Open {
    Bracket(usize),
    /// An f-string or t-string, inside which a brace opens a field.
    String,
    /// A field of one, or a format spec's nested field; `true` once its format spec has begun,
    /// after which a brace opens a nested field rather than a display.
    Field(bool),
}

/// `text` lexed for the parts a rewrite must handle with care.
#[must_use]
pub fn lex(text: &str) -> Lexed {
    let parsed = ruff_python_parser::parse_unchecked(text, ParseOptions::from(Mode::Module));
    let mut lexed = Lexed::default();
    let mut open: Vec<Open> = Vec::new();
    // The last token that is not layout, for a trailing comma.
    let mut last: Option<(TokenKind, usize)> = None;
    for token in parsed.tokens() {
        let range = usize::from(token.start())..usize::from(token.end());
        let kind = token.kind();
        match kind {
            TokenKind::Lpar | TokenKind::Lsqb => open.push(Open::Bracket(range.start)),
            TokenKind::Lbrace => open.push(match open.last() {
                Some(Open::String | Open::Field(true)) => Open::Field(false),
                _ => Open::Bracket(range.start),
            }),
            // A colon directly in a field, not in a bracket inside it, starts its format spec.
            TokenKind::Colon => {
                if let Some(Open::Field(spec)) = open.last_mut() {
                    *spec = true;
                }
            }
            TokenKind::FStringStart | TokenKind::TStringStart => open.push(Open::String),
            TokenKind::Rpar
            | TokenKind::Rsqb
            | TokenKind::Rbrace
            | TokenKind::FStringEnd
            | TokenKind::TStringEnd => {
                if let Some(Open::Bracket(start)) = open.pop() {
                    lexed.brackets.push(Bracket {
                        open: start,
                        close: range.start,
                        trailing_comma: match last {
                            Some((TokenKind::Comma, at)) => Some(at),
                            _ => None,
                        },
                    });
                }
            }
            _ => {}
        }
        if !matches!(
            kind,
            TokenKind::Comment
                | TokenKind::NonLogicalNewline
                | TokenKind::Newline
                | TokenKind::Indent
                | TokenKind::Dedent
        ) {
            last = Some((kind, range.start));
        }
        match kind {
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
    lexed.brackets.sort_unstable_by_key(|bracket| bracket.open);
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
    fn brackets_pair_with_their_trailing_commas_and_a_string_field_is_not_one() {
        let text = "f(a, [b,  # c\n  ],\n  {d: e}, s'{x}', f'{g(h,)}', f'{ {i: 1,} :>{w}}')\n";
        let found: Vec<(&str, Option<&str>)> = lex(text)
            .brackets
            .iter()
            .map(|b| {
                (
                    &text[b.open..=b.close],
                    b.trailing_comma.map(|at| &text[at..b.close]),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (&text[1..text.len() - 1], None),
                ("[b,  # c\n  ]", Some(",  # c\n  ")),
                ("{d: e}", None),
                ("(h,)", Some(",")),
                ("{i: 1,}", Some(",")),
            ]
        );
    }

    #[test]
    fn a_fragment_with_holes_still_lexes() {
        assert_ne!(token_kinds("call($a)"), Vec::<String>::new());
    }
}
