// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Script text to tokens.
//!
//! Operands follow the one rule settled for them: nothing inside an operand is escaped.  An
//! operand opens with a run of one delimiter, or of three or more, and closes at the next run
//! of exactly that length; two delimiters in a row are the empty operand; in an operand fenced
//! with three or more, content that both starts and ends with a space loses one from each end.
//! Only `"` and `` ` `` are delimiters.

use std::fmt;

/// One token, with the byte range it occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// Its first byte.
    pub start: usize,
    /// One past its last byte.
    pub end: usize,
}

/// The kinds of token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A word: a keyword, `FILE`, or a statement this build does not have.
    Word(String),
    /// A whole number.
    Number(u64),
    /// A double-quoted operand's content.
    Quoted(String),
    /// A backticked operand's content.
    Backticked(String),
    /// `$name`, `$_`, `$name...` or `$...`: the name without `$`, and whether it is a sequence.
    Metavariable {
        /// The name, without `$`; `_` for an unbound hole.
        name: String,
        /// Whether it is a sequence, written with a trailing `...`.
        sequence: bool,
    },
    /// `$^`: a line break between two pieces.
    LineBreak,
    /// `^`: the start anchor.
    StartAnchor,
    /// `$` alone: the end anchor.
    EndAnchor,
    /// A newline or `;`: the end of a statement.
    Separator,
    /// `[`, which only `where ... in [...]`, outside the MVP, would use.
    OpenBracket,
    /// `]`, the end of such a list.
    CloseBracket,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Word(word) => write!(f, "`{word}`"),
            TokenKind::Number(n) => write!(f, "the number {n}"),
            TokenKind::Quoted(_) => f.write_str("a quoted operand"),
            TokenKind::Backticked(_) => f.write_str("a backticked operand"),
            TokenKind::Metavariable { name, sequence } => {
                write!(f, "`${name}{}`", if *sequence { "..." } else { "" })
            }
            TokenKind::LineBreak => f.write_str("`$^`"),
            TokenKind::StartAnchor => f.write_str("`^`"),
            TokenKind::EndAnchor => f.write_str("`$`"),
            TokenKind::Separator => f.write_str("the end of the statement"),
            TokenKind::OpenBracket => f.write_str("`[`"),
            TokenKind::CloseBracket => f.write_str("`]`"),
        }
    }
}

/// Text that is not a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    /// What is wrong, and what to do about it.
    pub message: String,
    /// Where it starts.
    pub start: usize,
    /// One past where it ends.
    pub end: usize,
}

/// Splits `text` into tokens.  Comments - `#` to the end of a line, outside operands - and
/// whitespace other than newlines are dropped.
///
/// # Errors
///
/// Returns a [`LexError`] for an operand that never closes, a character that starts no token,
/// or a number too large to count.
pub fn tokens(text: &str) -> Result<Vec<Token>, LexError> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        let kind = match c {
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'\n' | b';' => {
                i += 1;
                TokenKind::Separator
            }
            b'"' | b'`' => {
                let (content, end) = operand(text, i)?;
                i = end;
                if c == b'"' {
                    TokenKind::Quoted(content)
                } else {
                    TokenKind::Backticked(content)
                }
            }
            b'^' => {
                i += 1;
                TokenKind::StartAnchor
            }
            b'[' => {
                i += 1;
                TokenKind::OpenBracket
            }
            b']' => {
                i += 1;
                TokenKind::CloseBracket
            }
            b'$' => {
                let (kind, end) = dollar(bytes, i);
                i = end;
                kind
            }
            b'0'..=b'9' => {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let digits = &text[start..i];
                let n = digits.parse().map_err(|_| LexError {
                    message: format!("the number {digits} is too large to count"),
                    start,
                    end: i,
                })?;
                TokenKind::Number(n)
            }
            c if is_word_start(c) => {
                while i < bytes.len() && is_word_byte(bytes[i]) {
                    i += 1;
                }
                TokenKind::Word(text[start..i].to_owned())
            }
            _ => {
                let ch = text[start..]
                    .chars()
                    .next()
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                return Err(LexError {
                    message: format!(
                        "unexpected {ch:?}: text to search for is quoted, as \"TODO\", and a \
                         pattern is backticked"
                    ),
                    start,
                    end: start + ch.len_utf8(),
                });
            }
        };
        found.push(Token {
            kind,
            start,
            end: i,
        });
    }
    Ok(found)
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// A `$` and what follows it: a metavariable when a name or `_` follows, `$^` a line break,
/// anything else the end anchor.
fn dollar(bytes: &[u8], at: usize) -> (TokenKind, usize) {
    let mut i = at + 1;
    if bytes.get(i) == Some(&b'^') {
        return (TokenKind::LineBreak, i + 1);
    }
    if bytes[i..].starts_with(b"...") {
        return (
            TokenKind::Metavariable {
                name: "_".into(),
                sequence: true,
            },
            i + 3,
        );
    }
    if bytes.get(i).is_some_and(|&c| is_word_start(c)) {
        let name_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let name = String::from_utf8_lossy(&bytes[name_start..i]).into_owned();
        let sequence = bytes[i..].starts_with(b"...");
        if sequence {
            i += 3;
        }
        return (TokenKind::Metavariable { name, sequence }, i);
    }
    (TokenKind::EndAnchor, at + 1)
}

/// The length of the run of `delimiter` starting at `at`.
fn run(bytes: &[u8], at: usize, delimiter: u8) -> usize {
    bytes[at..].iter().take_while(|&&b| b == delimiter).count()
}

/// Reads the operand whose opening delimiter is at `at`, returning its content and the byte
/// after its closing delimiter.
fn operand(text: &str, at: usize) -> Result<(String, usize), LexError> {
    let bytes = text.as_bytes();
    let delimiter = bytes[at];
    let fence = run(bytes, at, delimiter);
    if fence == 2 {
        return Ok((String::new(), at + 2));
    }
    let content_start = at + fence;
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i] == delimiter {
            let length = run(bytes, i, delimiter);
            if length == fence {
                let mut content = &text[content_start..i];
                // Markdown's rule, which is what lets content begin or end with the delimiter.
                if fence >= 3
                    && content.len() >= 2
                    && content.starts_with(' ')
                    && content.ends_with(' ')
                {
                    content = &content[1..content.len() - 1];
                }
                return Ok((content.to_owned(), i + fence));
            }
            i += length;
        } else {
            i += 1;
        }
    }
    let shown = if delimiter == b'"' { "\"" } else { "`" };
    Err(LexError {
        message: format!(
            "this operand is never closed: it opens with {fence} {shown} and closes at the next \
             run of exactly {fence}"
        ),
        start: at,
        end: content_start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        tokens(text).unwrap().into_iter().map(|t| t.kind).collect()
    }

    fn one_operand(text: &str) -> String {
        match kinds(text).as_slice() {
            [TokenKind::Quoted(content) | TokenKind::Backticked(content)] => content.clone(),
            other => panic!("{text} gave {other:?}"),
        }
    }

    /// The twelve spellings on the syntax page, each with the content it must produce.
    #[test]
    fn every_spelling_on_the_syntax_page_gives_its_stated_content() {
        let cases = [
            (r#""C:\temp""#, r"C:\temp"),
            (r#""don't""#, "don't"),
            (r#"""" say "hi" """"#, r#"say "hi""#),
            (r#"""" "hi" """"#, r#""hi""#),
            (r#""""x""y""""#, r#"x""y"#),
            (r#""""#, ""),
            (r#"`re.compile("\d+")`"#, r#"re.compile("\d+")"#),
            (r#"`p = "a\\b"`"#, r#"p = "a\\b""#),
            (r#"`x = """doc"""`"#, r#"x = """doc""""#),
            ("```x = \"use `ls`\"```", "x = \"use `ls`\""),
            ("```doc = \"see ``foo``\"```", "doc = \"see ``foo``\""),
            ("````md = \"```py\"````", "md = \"```py\""),
        ];
        for (spelling, content) in cases {
            assert_eq!(one_operand(spelling), content, "{spelling}");
        }
    }

    #[test]
    fn a_single_delimiter_operand_is_never_trimmed() {
        assert_eq!(one_operand(r#"" a ""#), " a ");
        assert_eq!(one_operand(r#"""" a """"#), "a");
    }

    #[test]
    fn a_run_of_another_length_inside_an_operand_is_content() {
        assert_eq!(one_operand(r#""a""b""#), r#"a""b"#);
    }

    #[test]
    fn an_operand_may_span_lines() {
        assert_eq!(one_operand("\"one\ntwo\""), "one\ntwo");
    }

    #[test]
    fn an_unclosed_operand_says_how_it_must_close() {
        let err = tokens(r#"find """x"""#).unwrap_err();
        assert_eq!(err.start, 5);
        assert!(err.message.contains("exactly 3"), "{}", err.message);
    }

    #[test]
    fn dollar_forms_are_told_apart() {
        assert_eq!(
            kinds("$x $_ $xs... $... $^ $"),
            [
                TokenKind::Metavariable {
                    name: "x".into(),
                    sequence: false
                },
                TokenKind::Metavariable {
                    name: "_".into(),
                    sequence: false
                },
                TokenKind::Metavariable {
                    name: "xs".into(),
                    sequence: true
                },
                TokenKind::Metavariable {
                    name: "_".into(),
                    sequence: true
                },
                TokenKind::LineBreak,
                TokenKind::EndAnchor,
            ]
        );
        assert_eq!(
            kinds(r#"^"a"$^"b"$"#),
            [
                TokenKind::StartAnchor,
                TokenKind::Quoted("a".into()),
                TokenKind::LineBreak,
                TokenKind::Quoted("b".into()),
                TokenKind::EndAnchor,
            ]
        );
    }

    #[test]
    fn comments_go_and_separators_stay() {
        assert_eq!(
            kinds("find \"a # not a comment\" # a comment\n; expect 3"),
            [
                TokenKind::Word("find".into()),
                TokenKind::Quoted("a # not a comment".into()),
                TokenKind::Separator,
                TokenKind::Separator,
                TokenKind::Word("expect".into()),
                TokenKind::Number(3),
            ]
        );
    }

    #[test]
    fn a_stray_character_says_what_operands_look_like() {
        let err = tokens("find TODO'").unwrap_err();
        assert_eq!(err.start, 9);
        assert!(
            err.message.starts_with("unexpected '\\''"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_unclosed_backticked_operand_names_its_own_delimiter() {
        let err = tokens("find ```x``").unwrap_err();
        assert_eq!(
            err.message,
            "this operand is never closed: it opens with 3 ` and closes at the next run of \
             exactly 3"
        );
    }

    #[test]
    fn a_stray_character_covers_the_whole_character() {
        let err = tokens("find \u{e9}").unwrap_err();
        assert_eq!((err.start, err.end), (5, 7));
    }

    #[test]
    fn words_take_letters_digits_underscores_and_hyphens() {
        assert_eq!(kinds("a_b-c9"), [TokenKind::Word("a_b-c9".into())]);
    }

    #[test]
    fn every_token_kind_describes_itself() {
        let described: Vec<String> = [
            TokenKind::Word("find".into()),
            TokenKind::Number(3),
            TokenKind::Quoted("x".into()),
            TokenKind::Backticked("x".into()),
            TokenKind::Metavariable {
                name: "x".into(),
                sequence: true,
            },
            TokenKind::LineBreak,
            TokenKind::StartAnchor,
            TokenKind::EndAnchor,
            TokenKind::Separator,
            TokenKind::OpenBracket,
            TokenKind::CloseBracket,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            described,
            [
                "`find`",
                "the number 3",
                "a quoted operand",
                "a backticked operand",
                "`$x...`",
                "`$^`",
                "`^`",
                "`$`",
                "the end of the statement",
                "`[`",
                "`]`"
            ]
        );
    }

    #[test]
    fn a_number_too_large_is_refused() {
        let err = tokens("expect 99999999999999999999999").unwrap_err();
        assert!(err.message.contains("too large"), "{}", err.message);
    }

    #[test]
    fn tokens_carry_their_byte_ranges() {
        let found = tokens("find \"ab\"").unwrap();
        assert_eq!((found[1].start, found[1].end), (5, 9));
    }
}
