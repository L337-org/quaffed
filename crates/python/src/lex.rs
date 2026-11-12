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
    /// Where each logical line starts: the first token of the text, and the first after each
    /// logical line ends, in order.  A line continued inside a bracket or after a backslash is
    /// not one, so its indentation says nothing of depth.
    pub logical_starts: Vec<usize>,
    /// Every part of a string whose text is the string's own - a plain string, or an f-string's
    /// or t-string's literal text between fields - with how its literal is written, in order.
    pub strings: Vec<StringPart>,
}

/// One part of a string literal and how the literal is written, which decides how text can be
/// written into it as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringPart {
    /// Its byte range: a plain string's whole token, prefix and quotes included, or the literal
    /// text of an f-string or t-string between its fields.
    pub range: Range<usize>,
    /// Whether the literal is raw, its backslashes text rather than escapes.
    pub raw: bool,
    /// Whether it is a bytes literal, which holds ASCII only.
    pub bytes: bool,
    /// Whether it is an f-string or t-string, in which a brace opens a field.
    pub formatted: bool,
    /// Its quotes: one quote character, or three.
    pub quote: String,
}

/// `value` written into `part` so that the literal holds it as text: in a string that is not
/// raw, each backslash and quote character escaped, and line breaks too unless the quotes are
/// triple; each brace doubled in an f-string or t-string.  `None` where the literal cannot hold
/// it as text: a raw string holding its own quote character or a line break it cannot, or
/// ending in a backslash, and a bytes literal holding anything but ASCII.
#[must_use]
pub fn escaped(value: &str, part: &StringPart) -> Option<String> {
    let quote = part.quote.chars().next()?;
    let triple = part.quote.len() == 3;
    if part.bytes && !value.is_ascii() {
        return None;
    }
    if part.raw
        && (value.contains(quote)
            || value.ends_with('\\')
            || (!triple && value.contains(['\n', '\r'])))
    {
        return None;
    }
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' if !part.raw => out.push_str("\\\\"),
            c if c == quote && !part.raw => {
                out.push('\\');
                out.push(c);
            }
            '\n' if !triple => out.push_str("\\n"),
            '\r' if !triple => out.push_str("\\r"),
            '{' | '}' if part.formatted => {
                out.push(c);
                out.push(c);
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// Records the token `kind` at `range` of `text` if it is part of a string, with how its literal
/// is written; `formatted` holds how each f-string or t-string still open was opened.
fn string_part(
    lexed: &mut Lexed,
    formatted: &mut Vec<(String, String)>,
    text: &str,
    kind: TokenKind,
    range: &Range<usize>,
) {
    match kind {
        TokenKind::FStringStart | TokenKind::TStringStart => {
            formatted.push(quoting(&text[range.clone()]));
        }
        TokenKind::FStringEnd | TokenKind::TStringEnd => {
            formatted.pop();
        }
        TokenKind::String => {
            let (prefix, quote) = quoting(&text[range.clone()]);
            lexed.strings.push(StringPart {
                range: range.clone(),
                raw: prefix.contains('r'),
                bytes: prefix.contains('b'),
                formatted: false,
                quote,
            });
        }
        TokenKind::FStringMiddle | TokenKind::TStringMiddle => {
            if let Some((prefix, quote)) = formatted.last() {
                lexed.strings.push(StringPart {
                    range: range.clone(),
                    raw: prefix.contains('r'),
                    bytes: false,
                    formatted: true,
                    quote: quote.clone(),
                });
            }
        }
        _ => {}
    }
}

/// The prefix and quotes a string, f-string or t-string opens with, from the text of its first
/// token: the letters, then one quote character or three.
fn quoting(opening: &str) -> (String, String) {
    let prefix: String = opening
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_lowercase();
    let rest = &opening[prefix.len()..];
    let quote = if rest.starts_with("'''") || rest.starts_with("\"\"\"") {
        rest[..3].to_owned()
    } else {
        rest[..1.min(rest.len())].to_owned()
    };
    (prefix, quote)
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
    // How each f-string or t-string still open was opened, innermost last.
    let mut formatted: Vec<(String, String)> = Vec::new();
    // Whether the next token that is not layout starts a logical line.
    let mut line_start = true;
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
                | TokenKind::EndOfFile
        ) {
            last = Some((kind, range.start));
            if line_start {
                lexed.logical_starts.push(range.start);
                line_start = false;
            }
        }
        if kind == TokenKind::Newline {
            line_start = true;
        }
        string_part(&mut lexed, &mut formatted, text, kind, &range);
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
    fn a_value_is_escaped_as_each_kind_of_string_needs() {
        let text = "a = 'x'\nb = r\"\"\"y\"\"\"\nc = f'{z}w'\nd = b'v'\n";
        let parts = lex(text).strings;
        let shown: Vec<(&str, bool, bool, bool, &str)> = parts
            .iter()
            .map(|p| {
                (
                    &text[p.range.clone()],
                    p.raw,
                    p.bytes,
                    p.formatted,
                    p.quote.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("'x'", false, false, false, "'"),
                ("r\"\"\"y\"\"\"", true, false, false, "\"\"\""),
                ("w", false, false, true, "'"),
                ("b'v'", false, true, false, "'"),
            ]
        );
        let value = "it's {a}\\ \"b\"\n";
        assert_eq!(
            escaped(value, &parts[0]).as_deref(),
            Some("it\\'s {a}\\\\ \"b\"\\n")
        );
        assert_eq!(escaped(value, &parts[1]), None);
        assert_eq!(escaped("say \"hi\"", &parts[1]), None);
        assert_eq!(escaped("say 'hi'", &parts[1]).as_deref(), Some("say 'hi'"));
        assert_eq!(
            escaped(value, &parts[2]).as_deref(),
            Some("it\\'s {{a}}\\\\ \"b\"\\n")
        );
        assert_eq!(escaped("caf\u{e9}", &parts[3]), None);
    }

    #[test]
    fn a_logical_line_starts_its_first_token_and_a_continued_one_does_not() {
        let text = "x = \\\n    [1,\n     2]\nif y:\n    z()\n";
        let starts: Vec<&str> = lex(text)
            .logical_starts
            .iter()
            .map(|&at| &text[at..=at])
            .collect();
        assert_eq!(starts, ["x", "i", "z"]);
    }

    #[test]
    fn a_fragment_with_holes_still_lexes() {
        assert_ne!(token_kinds("call($a)"), Vec::<String>::new());
    }
}
