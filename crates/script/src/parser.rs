// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Tokens to statements in the operation representation, for the MVP subset.
//!
//! A word in statement position that is not a statement this build has is an unknown
//! statement, exit 5, naming it, and so is any construct outside the MVP; anything else
//! malformed is a parse error, exit 3.  Nothing is parsed and ignored.

use quaffed_representation::glob::{Class, ClassItem, Glob, GlobPart};
use quaffed_representation::program::{
    Action, ActionKind, Count, Expectation, Filter, Operand, Program, Query, Statement, Subject,
    TextPattern,
};
use quaffed_representation::span::{Span, Spanned};

use crate::lexer::{Token, TokenKind};

/// Why a script cannot be parsed, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Malformed, or a construct this build does not have.
    pub kind: ErrorKind,
    /// What is wrong and what to do about it.
    pub message: String,
    /// Where, as a byte range in the source.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

/// The two kinds of parse failure, which exit differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Malformed: fix the script.  Exit 3.
    Malformed,
    /// A statement or construct this build does not have.  Exit 5.
    Unknown,
}

/// Statements this build does not have, each named as the unknown statement it is.
const NOT_IN_THIS_BUILD: &[&str] = &[
    "insert", "rename", "for", "case", "embedded", "function", "load", "config", "write",
];

/// Clauses this build does not have.
const CLAUSES_NOT_IN_THIS_BUILD: &[&str] = &["as", "reject"];

/// Context names this build does not have.
const CONTEXTS_NOT_IN_THIS_BUILD: &[&str] = &["ENCLOSING", "LANGUAGE"];

/// The clauses, in the one order they may appear.
const CLAUSE_ORDER: [&str; 3] = ["in", "where", "expect"];

/// Parses `tokens`, from source number `source`, appending each statement to `program`.
///
/// # Errors
///
/// Returns the first [`ParseError`], in source order.
pub fn parse(tokens: &[Token], source: usize, program: &mut Program) -> Result<(), ParseError> {
    let mut parser = Parser {
        tokens,
        at: 0,
        source,
    };
    loop {
        parser.skip_separators();
        if parser.peek().is_none() {
            return Ok(());
        }
        let statement = parser.statement()?;
        program.push(statement);
        match parser.peek() {
            None
            | Some(Token {
                kind: TokenKind::Separator,
                ..
            }) => {}
            Some(token) => return Err(parser.after_statement(token)),
        }
    }
}

struct Parser<'t> {
    tokens: &'t [Token],
    at: usize,
    source: usize,
}

/// What a statement's clauses said.
#[derive(Default)]
struct Clauses {
    scope: Vec<Spanned<Glob>>,
    filter: Option<Spanned<Filter>>,
    expect: Option<Spanned<Count>>,
}

impl<'t> Parser<'t> {
    fn peek(&self) -> Option<&'t Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<&'t Token> {
        let token = self.tokens.get(self.at);
        if token.is_some() {
            self.at += 1;
        }
        token
    }

    fn skip_separators(&mut self) {
        while self.peek().is_some_and(|t| t.kind == TokenKind::Separator) {
            self.at += 1;
        }
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span {
            source: self.source,
            start,
            end,
        }
    }

    fn token_span(&self, token: &Token) -> Span {
        self.span(token.start, token.end)
    }

    /// The end of the input, as a zero-width position after the last token.
    fn end_position(&self) -> usize {
        self.tokens.last().map_or(0, |t| t.end)
    }

    fn malformed(message: String, token: Option<&Token>, fallback: usize) -> ParseError {
        let (start, end) = token.map_or((fallback, fallback), |t| (t.start, t.end));
        ParseError {
            kind: ErrorKind::Malformed,
            message,
            start,
            end,
        }
    }

    fn unknown(message: String, token: &Token) -> ParseError {
        ParseError {
            kind: ErrorKind::Unknown,
            message,
            start: token.start,
            end: token.end,
        }
    }

    fn word(&self) -> Option<&'t str> {
        match self.peek() {
            Some(Token {
                kind: TokenKind::Word(word),
                ..
            }) => Some(word),
            _ => None,
        }
    }

    fn statement(&mut self) -> Result<Spanned<Statement>, ParseError> {
        let first = self.peek().expect("a statement starts at a token");
        let word = match &first.kind {
            TokenKind::Quoted(_) | TokenKind::Backticked(_) | TokenKind::StartAnchor => {
                return self.find(first);
            }
            TokenKind::Word(word) => word.as_str(),
            other => {
                return Err(Self::malformed(
                    format!(
                        "a statement cannot start with {other}: write a statement, such as \
                         find \"TODO\""
                    ),
                    Some(first),
                    0,
                ));
            }
        };
        match word {
            "find" => {
                self.at += 1;
                self.find(first)
            }
            "replace" => self.replace(first),
            "delete" => self.delete(first),
            "expect" => Err(Self::unknown(
                "unknown statement: a standalone `expect` is not in this build; put the expect \
                 on the find it counts, as find \"TODO\" expect none"
                    .into(),
                first,
            )),
            _ if NOT_IN_THIS_BUILD.contains(&word) => Err(Self::unknown(
                format!("unknown statement: `{word}` is not in this build"),
                first,
            )),
            _ => Err(Self::unknown(
                format!(
                    "unknown statement: `{word}`; the statements in this build are find, \
                     replace and delete"
                ),
                first,
            )),
        }
    }

    /// `find X clauses`, or a bare operand; `first` is the statement's first token.
    fn find(&mut self, first: &Token) -> Result<Spanned<Statement>, ParseError> {
        let operand = self.operand(OperandUse::Search)?;
        let clauses = self.clauses()?;
        let span = self.token_span(first).through(self.last_span());
        let query = Query::with(operand, clauses.scope, clauses.filter);
        let statement = match clauses.expect {
            None => Statement::Find(query),
            Some(count) => Statement::Assert(vec![Spanned::new(
                Expectation::of_matches(count, query),
                span,
            )]),
        };
        Ok(Spanned::new(statement, span))
    }

    /// `replace string "X" with "Y" clauses`, or `` replace `X` with `Y` clauses ``.
    fn replace(&mut self, first: &Token) -> Result<Spanned<Statement>, ParseError> {
        self.at += 1;
        let textual = self.word() == Some("string");
        if textual {
            self.at += 1;
        }
        let target = self.operand(OperandUse::Search)?;
        Self::check_spelling(&target, textual, "replace")?;
        match self.next() {
            Some(Token {
                kind: TokenKind::Word(word),
                ..
            }) if word == "with" => {}
            other => {
                return Err(Self::malformed(
                    format!(
                        "expected `with` and the replacement, but found {}",
                        describe(other)
                    ),
                    other,
                    self.end_position(),
                ));
            }
        }
        let replacement = self.operand(OperandUse::Replacement)?;
        Self::check_spelling(&replacement, textual, "replace")?;
        let clauses = self.clauses()?;
        let span = self.token_span(first).through(self.last_span());
        Ok(Spanned::new(
            Statement::Act(Action::new(
                ActionKind::Replace(replacement),
                Query::with(target, clauses.scope, clauses.filter),
                clauses.expect.into_iter().collect(),
            )),
            span,
        ))
    }

    /// `` delete `X` clauses ``.  `delete` removes matched nodes, so its operand is a pattern;
    /// there is no textual delete, and a quoted operand is refused rather than read as one.
    fn delete(&mut self, first: &Token) -> Result<Spanned<Statement>, ParseError> {
        self.at += 1;
        let target = self.operand(OperandUse::Search)?;
        if matches!(target.node, Operand::Text(_)) {
            return Err(ParseError {
                kind: ErrorKind::Malformed,
                message: "`delete` removes matched nodes, so it takes a backticked pattern, not \
                          quoted text; to remove text, replace it with nothing: \
                          replace string \"...\" with \"\""
                    .into(),
                start: target.span.start,
                end: target.span.end,
            });
        }
        let clauses = self.clauses()?;
        let span = self.token_span(first).through(self.last_span());
        Ok(Spanned::new(
            Statement::Act(Action::new(
                ActionKind::Delete,
                Query::with(target, clauses.scope, clauses.filter),
                clauses.expect.into_iter().collect(),
            )),
            span,
        ))
    }

    /// Textual operations are spelled textually: `replace string` takes quoted operands, and
    /// `replace` backticked ones.  A mismatch is refused rather than guessed at.
    fn check_spelling(
        operand: &Spanned<Operand>,
        textual: bool,
        statement: &str,
    ) -> Result<(), ParseError> {
        let is_text = matches!(operand.node, Operand::Text(_));
        if is_text == textual {
            return Ok(());
        }
        let message = if textual {
            format!(
                "`{statement} string` replaces text, so it takes quoted operands, not a \
                 backticked pattern; for a structural replace, drop `string`"
            )
        } else {
            format!(
                "`{statement}` with a quoted operand would be a textual replace: write \
                 `{statement} string`, or backtick both operands for a structural one"
            )
        };
        Err(ParseError {
            kind: ErrorKind::Malformed,
            message,
            start: operand.span.start,
            end: operand.span.end,
        })
    }

    fn last_span(&self) -> Span {
        let token = &self.tokens[self.at.saturating_sub(1)];
        self.token_span(token)
    }

    /// An operand: a backticked pattern, or textual pieces with line breaks and anchors.
    fn operand(&mut self, usage: OperandUse) -> Result<Spanned<Operand>, ParseError> {
        let Some(first) = self.peek() else {
            return Err(Self::malformed(
                "expected an operand - quoted text or a backticked pattern - but the script \
                 ends"
                    .into(),
                None,
                self.end_position(),
            ));
        };
        if let TokenKind::Backticked(pattern) = &first.kind {
            self.at += 1;
            if usage == OperandUse::Replacement
                && let Some(filter) = value_filter(pattern)
            {
                return Err(Self::unknown(
                    format!("unknown construct: the value filter `{filter}` is not in this build"),
                    first,
                ));
            }
            self.refuse_textual_markers_after()?;
            return Ok(Spanned::new(
                Operand::Pattern(pattern.clone()),
                self.token_span(first),
            ));
        }
        self.textual(first, usage)
    }

    /// After a backticked operand, a marker or another piece is a parse error: a structural
    /// pattern is already anchored by its shape.
    fn refuse_textual_markers_after(&self) -> Result<(), ParseError> {
        match self.peek() {
            Some(
                token @ Token {
                    kind:
                        TokenKind::EndAnchor
                        | TokenKind::LineBreak
                        | TokenKind::Quoted(_)
                        | TokenKind::Backticked(_),
                    ..
                },
            ) => Err(Self::malformed(
                format!(
                    "{} cannot follow a backticked pattern: joining, `$^` and anchors are for \
                     quoted text, and a pattern is already anchored by its shape",
                    token.kind
                ),
                Some(token),
                0,
            )),
            _ => Ok(()),
        }
    }

    /// Quoted pieces, joined, with `$^` between them and anchors at the ends.
    fn textual(
        &mut self,
        first: &Token,
        usage: OperandUse,
    ) -> Result<Spanned<Operand>, ParseError> {
        let mut pattern = TextPattern::default();
        if first.kind == TokenKind::StartAnchor {
            Self::refuse_anchor(first, usage)?;
            pattern.start_anchor = true;
            self.at += 1;
            if let Some(
                pattern_token @ Token {
                    kind: TokenKind::Backticked(_),
                    ..
                },
            ) = self.peek()
            {
                return Err(Self::malformed(
                    "`^` cannot anchor a backticked pattern: anchors are for quoted text, and a \
                     pattern is already anchored by its shape"
                        .into(),
                    Some(pattern_token),
                    0,
                ));
            }
        }
        let mut pieces = 0;
        let mut last_end = first.end;
        loop {
            let token = self.peek();
            match token.map(|t| &t.kind) {
                Some(TokenKind::Quoted(text)) => {
                    pattern.push_literal(text.clone());
                    pieces += 1;
                    last_end = token.expect("peeked").end;
                    self.at += 1;
                }
                Some(TokenKind::LineBreak) if pieces > 0 => {
                    let line_break = token.expect("peeked");
                    self.at += 1;
                    if !matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Quoted(_))) {
                        return Err(Self::malformed(
                            "`$^` is a line break between two quoted pieces, so a piece must \
                             follow it"
                                .into(),
                            Some(line_break),
                            0,
                        ));
                    }
                    pattern.push_line_break();
                }
                Some(TokenKind::Metavariable { .. }) if pieces > 0 => {
                    return Err(Self::unknown(
                        "unknown construct: a metavariable between quoted pieces is textual \
                         capture, which is not in this build"
                            .into(),
                        token.expect("peeked"),
                    ));
                }
                Some(
                    TokenKind::Star
                    | TokenKind::DoubleStar
                    | TokenKind::Question
                    | TokenKind::Class { .. }
                    | TokenKind::OpenBrace,
                ) if pieces > 0 => {
                    return Err(Self::wildcard_in_operand(token.expect("peeked")));
                }
                Some(TokenKind::EndAnchor) if pieces > 0 => {
                    let anchor = token.expect("peeked");
                    Self::refuse_anchor(anchor, usage)?;
                    pattern.end_anchor = true;
                    last_end = anchor.end;
                    self.at += 1;
                    break;
                }
                _ => break,
            }
        }
        if pieces == 0 {
            let found = self.peek();
            return Err(Self::malformed(
                format!(
                    "expected an operand - quoted text or a backticked pattern - but found {}",
                    describe(found)
                ),
                found,
                self.end_position(),
            ));
        }
        if usage == OperandUse::Search && pattern.is_empty() {
            return Err(Self::malformed(
                "this operand is empty, so it would match nothing anyone means: give the text \
                 to search for"
                    .into(),
                Some(first),
                0,
            ));
        }
        Ok(Spanned::new(
            Operand::Text(pattern),
            self.span(first.start, last_end),
        ))
    }

    /// The error for a wildcard after an operand's pieces: an operand is matched exactly.
    fn wildcard_in_operand(wildcard: &Token) -> ParseError {
        Self::malformed(
            format!(
                "{} is a wildcard, and wildcards are for `in` and `where`: an operand is matched \
                 exactly, so a literal one goes inside the quotes, as \"a*b\"",
                wildcard.kind
            ),
            Some(wildcard),
            0,
        )
    }

    fn refuse_anchor(anchor: &Token, usage: OperandUse) -> Result<(), ParseError> {
        if usage == OperandUse::Replacement {
            return Err(Self::malformed(
                format!(
                    "{} cannot be in a replacement: an anchor matches a position, and a \
                     replacement only writes text",
                    anchor.kind
                ),
                Some(anchor),
                0,
            ));
        }
        Ok(())
    }

    /// `in`, `where` and `expect`, each at most once and in that order.
    fn clauses(&mut self) -> Result<Clauses, ParseError> {
        let mut clauses = Clauses::default();
        let mut reached = None;
        while let Some(word) = self.word() {
            let keyword = self.peek().expect("peeked");
            if CLAUSES_NOT_IN_THIS_BUILD.contains(&word) {
                return Err(Self::unknown(
                    format!("unknown construct: the `{word}` clause is not in this build"),
                    keyword,
                ));
            }
            let Some(position) = CLAUSE_ORDER.iter().position(|c| *c == word) else {
                break;
            };
            if let Some(previous) = reached
                && position <= previous
            {
                let message = if position == previous {
                    format!("`{word}` is repeated: each clause appears at most once")
                } else {
                    format!(
                        "`{word}` cannot follow `{}`: clauses come in the order in, where, expect",
                        CLAUSE_ORDER[previous]
                    )
                };
                return Err(Self::malformed(message, Some(keyword), 0));
            }
            reached = Some(position);
            self.at += 1;
            match word {
                "in" => clauses.scope.push(self.scope_glob(keyword)?),
                "where" => clauses.filter = Some(self.condition()?),
                _ => clauses.expect = Some(self.count(keyword)?),
            }
        }
        Ok(clauses)
    }

    fn scope_glob(&mut self, keyword: &Token) -> Result<Spanned<Glob>, ParseError> {
        match self.peek() {
            Some(
                token @ Token {
                    kind: TokenKind::Metavariable { .. },
                    ..
                },
            ) => Err(Self::unknown(
                "unknown construct: `in` over a bound set is not in this build".into(),
                token,
            )),
            Some(
                token @ Token {
                    kind: TokenKind::Backticked(_),
                    ..
                },
            ) => Err(Self::malformed(
                "`in` takes a path or a glob - quoted text and wildcards, as in \"src/\"** - \
                 not a backticked pattern"
                    .into(),
                Some(token),
                0,
            )),
            _ => self.glob(GlobUse::Path, keyword),
        }
    }

    /// A glob: quoted pieces and wildcard markers, joined, with `^` before them and `$` after
    /// them for a value, and `$^` among them.  `after` is the token before it, for an error at
    /// the end of the script.
    fn glob(&mut self, usage: GlobUse, after: &Token) -> Result<Spanned<Glob>, ParseError> {
        let mut glob = Glob::default();
        let start = self.peek().map_or(after.end, |t| t.start);
        if let Some(anchor) = self.peek().filter(|t| t.kind == TokenKind::StartAnchor) {
            Self::refuse_in_path(anchor, usage)?;
            glob.start_anchor = true;
            self.at += 1;
        }
        if self.glob_parts(&mut glob, usage, false)? == 0 {
            let found = self.peek();
            return Err(Self::malformed(
                format!(
                    "expected {} - quoted text and wildcards, as {} - but found {}",
                    usage.what(),
                    usage.example(),
                    describe(found)
                ),
                found,
                self.end_position(),
            ));
        }
        if let Some(anchor) = self.peek().filter(|t| t.kind == TokenKind::EndAnchor) {
            Self::refuse_in_path(anchor, usage)?;
            glob.end_anchor = true;
            self.at += 1;
        }
        let span = self.span(start, self.last_span().end);
        Ok(Spanned::new(glob, span))
    }

    /// Appends the glob parts that follow to `glob`, returning how many there were.  Inside
    /// braces, `nested`, they end at a `,` or `}`, and anything else that is not a part is an
    /// error, since alternatives are quoted as everything literal is.
    fn glob_parts(
        &mut self,
        glob: &mut Glob,
        usage: GlobUse,
        nested: bool,
    ) -> Result<usize, ParseError> {
        let mut parts = 0;
        while let Some(token) = self.peek() {
            let part = match &token.kind {
                TokenKind::Quoted(text) => GlobPart::Literal(text.clone()),
                TokenKind::Star => GlobPart::Star,
                TokenKind::DoubleStar => GlobPart::DoubleStar,
                TokenKind::Question => GlobPart::One,
                TokenKind::Class { negated, items } => Self::class(token, *negated, items)?,
                TokenKind::LineBreak => {
                    Self::refuse_in_path(token, usage)?;
                    GlobPart::LineBreak
                }
                TokenKind::OpenBrace => {
                    self.at += 1;
                    GlobPart::Alternatives(self.alternatives(token, usage)?)
                }
                TokenKind::Metavariable { .. } if parts > 0 => {
                    return Err(Self::unknown(
                        "unknown construct: a metavariable among a glob's pieces is textual \
                         capture, which is not in this build"
                            .into(),
                        token,
                    ));
                }
                // Inside braces, `,` and `}` end an alternative; outside them, anything that is
                // not a part ends the glob.
                TokenKind::Comma | TokenKind::CloseBrace => break,
                _ if nested => {
                    return Err(Self::malformed(
                        format!(
                            "expected an alternative - quoted text and wildcards - or `,` or `}}`, \
                             but found {}: alternatives are quoted, as {{\"src\",\"tests\"}}",
                            token.kind
                        ),
                        Some(token),
                        0,
                    ));
                }
                _ => break,
            };
            if !matches!(part, GlobPart::Alternatives(_)) {
                self.at += 1;
            }
            glob.push(part);
            parts += 1;
        }
        Ok(parts)
    }

    /// The alternatives after `open`, a `{`, up to and including their `}`.
    fn alternatives(&mut self, open: &Token, usage: GlobUse) -> Result<Vec<Glob>, ParseError> {
        let mut alternatives = Vec::new();
        loop {
            let mut alternative = Glob::default();
            if self.glob_parts(&mut alternative, usage, true)? == 0 {
                let found = self.peek();
                return Err(Self::malformed(
                    format!(
                        "an alternative is empty here: write \"\" for one that is nothing, but \
                         found {}",
                        describe(found)
                    ),
                    found.or(Some(open)),
                    0,
                ));
            }
            alternatives.push(alternative);
            match self.next() {
                Some(Token {
                    kind: TokenKind::Comma,
                    ..
                }) => {}
                Some(Token {
                    kind: TokenKind::CloseBrace,
                    ..
                }) => return Ok(alternatives),
                _ => {
                    return Err(Self::malformed(
                        "these alternatives are never closed: they run from { to the matching }"
                            .into(),
                        Some(open),
                        0,
                    ));
                }
            }
        }
    }

    /// A class, refused if it holds a line break, which no class can match.
    fn class(token: &Token, negated: bool, items: &[ClassItem]) -> Result<GlobPart, ParseError> {
        let holds_break = items.iter().any(|item| match *item {
            ClassItem::One(c) => c == '\n' || c == '\r',
            ClassItem::Range(low, high) => {
                (low..=high).contains(&'\n') || (low..=high).contains(&'\r')
            }
        });
        if holds_break {
            return Err(Self::malformed(
                "a class matches one character within a line, so it cannot hold a line break; \
                 $^ is a line break"
                    .into(),
                Some(token),
                0,
            ));
        }
        Ok(GlobPart::Class(Class {
            negated,
            items: items.to_vec(),
        }))
    }

    /// Refuses an anchor or `$^` in a path glob: a path has no lines.
    fn refuse_in_path(token: &Token, usage: GlobUse) -> Result<(), ParseError> {
        if usage == GlobUse::Path {
            return Err(Self::malformed(
                format!(
                    "{} cannot be in a path: a path has no lines, and a glob over one is \
                     anchored at both ends already",
                    token.kind
                ),
                Some(token),
                0,
            ));
        }
        Ok(())
    }

    /// `term (and term)*`.
    fn condition(&mut self) -> Result<Spanned<Filter>, ParseError> {
        let first = self.term()?;
        let mut parts = vec![first];
        while self.word() == Some("and") {
            self.at += 1;
            parts.push(self.term()?);
        }
        if let Some(or) = self
            .peek()
            .filter(|t| t.kind == TokenKind::Word("or".into()))
        {
            return Err(Self::malformed(
                "there is no `or` in a where clause: write a statement for each alternative".into(),
                Some(or),
                0,
            ));
        }
        if parts.len() == 1 {
            return Ok(parts.pop().expect("one part"));
        }
        let span = Span {
            source: self.source,
            start: parts[0].span.start,
            end: parts.last().expect("parts").span.end,
        };
        Ok(Spanned::new(Filter::And(parts), span))
    }

    /// `not term`, or `subject matches|contains operand`.
    ///
    /// A run of `not`s is read in a loop and kept as the one negation, or none, that it means,
    /// so `not not x` is `x`.  Recursing once per `not` would let a long enough run exhaust the
    /// stack and abort, where every other malformed script exits 3, and a nested `Not` per word
    /// would carry the same depth into every walk of the program.
    fn term(&mut self) -> Result<Spanned<Filter>, ParseError> {
        let mut negated = false;
        let mut start = None;
        while let Some(token) = self
            .peek()
            .filter(|t| t.kind == TokenKind::Word("not".into()))
        {
            self.at += 1;
            start.get_or_insert(token.start);
            negated = !negated;
        }
        let test = self.test()?;
        let Some(start) = start else {
            return Ok(test);
        };
        let span = self.span(start, test.span.end);
        if negated {
            Ok(Spanned::new(Filter::Not(Box::new(test)), span))
        } else {
            Ok(Spanned::new(test.node, span))
        }
    }

    /// `subject matches|contains operand`.
    fn test(&mut self) -> Result<Spanned<Filter>, ParseError> {
        let Some(first) = self.next() else {
            return Err(Self::malformed(
                "expected a condition after `where`, such as where not FILE matches \
                 **\"/test_\"*\".py\""
                    .into(),
                None,
                self.end_position(),
            ));
        };
        let subject = match &first.kind {
            TokenKind::Metavariable {
                name,
                sequence: false,
            } => Subject::Metavariable(name.clone()),
            TokenKind::Word(word) if word == "FILE" => Subject::File,
            TokenKind::Word(word) if CONTEXTS_NOT_IN_THIS_BUILD.contains(&word.as_str()) => {
                return Err(Self::unknown(
                    format!("unknown construct: the context name `{word}` is not in this build"),
                    first,
                ));
            }
            other => {
                return Err(Self::malformed(
                    format!(
                        "a condition tests a metavariable or FILE, but found {other}; for \
                         example, where $x matches \"test_\"*"
                    ),
                    Some(first),
                    0,
                ));
            }
        };
        let comparison = self.next();
        let matches = match comparison.map(|t| &t.kind) {
            Some(TokenKind::Word(word)) if word == "matches" => true,
            Some(TokenKind::Word(word)) if word == "contains" => false,
            Some(TokenKind::Word(word)) if word == "in" => {
                return Err(Self::unknown(
                    "unknown construct: `where ... in [...]` is not in this build".into(),
                    comparison.expect("matched"),
                ));
            }
            _ => {
                return Err(Self::malformed(
                    format!(
                        "expected `matches` or `contains` after the subject, but found {}",
                        describe(comparison)
                    ),
                    comparison,
                    self.end_position(),
                ));
            }
        };
        let comparison = comparison.expect("matched");
        let operand = match (self.peek().map(|t| &t.kind), &subject) {
            (Some(TokenKind::Backticked(_)), Subject::File) => {
                return Err(Self::malformed(
                    "FILE is a path, so it is compared with a glob - quoted text and wildcards, \
                     as **\"/test_\"*\".py\" - not a backticked pattern"
                        .into(),
                    self.peek(),
                    0,
                ));
            }
            (Some(TokenKind::Backticked(_)), _) => self.operand(OperandUse::Comparison)?,
            (_, Subject::File) => {
                let glob = self.glob(GlobUse::Path, comparison)?;
                Spanned::new(Operand::Glob(glob.node), glob.span)
            }
            _ => {
                let glob = self.glob(GlobUse::Value, comparison)?;
                Spanned::new(Operand::Glob(glob.node), glob.span)
            }
        };
        let span = self.span(first.start, operand.span.end);
        let filter = if matches {
            Filter::Matches(subject, operand)
        } else {
            Filter::Contains(subject, operand)
        };
        Ok(Spanned::new(filter, span))
    }

    /// The count after `expect`.
    fn count(&mut self, keyword: &Token) -> Result<Spanned<Count>, ParseError> {
        let Some(first) = self.next() else {
            return Err(Self::malformed(
                "expected a count after `expect`: a number, at least N, at most N, N or none, \
                 none or any"
                    .into(),
                Some(keyword),
                0,
            ));
        };
        let count = match &first.kind {
            TokenKind::Number(n) => self.count_after_number(*n)?,
            TokenKind::Word(word) if word == "at" => self.bounded_count()?,
            TokenKind::Word(word) if word == "none" => Count::None,
            TokenKind::Word(word) if word == "any" => {
                // A clause keyword after `any` is the next clause, or a clause out of order,
                // which `clauses` reports as the malformed script it is.
                let is_set = |token: &&Token| match &token.kind {
                    TokenKind::Word(word) => {
                        !CLAUSE_ORDER.contains(&word.as_str())
                            && !CLAUSES_NOT_IN_THIS_BUILD.contains(&word.as_str())
                    }
                    _ => false,
                };
                if let Some(set) = self.peek().filter(is_set) {
                    return Err(Self::unknown(
                        format!(
                            "unknown construct: counting the finding set {} is not in this \
                             build",
                            set.kind
                        ),
                        set,
                    ));
                }
                Count::Any
            }
            TokenKind::Word(word) if word == "applicable" || word == "no" => {
                return Err(Self::unknown(
                    format!("unknown construct: `expect {word}` is not in this build"),
                    first,
                ));
            }
            other => {
                return Err(Self::malformed(
                    format!(
                        "expected a count after `expect` - a number, at least N, at most N, N \
                         or none, none or any - but found {other}"
                    ),
                    Some(first),
                    0,
                ));
            }
        };
        Ok(Spanned::new(
            count,
            self.span(first.start, self.last_span().end),
        ))
    }

    /// `N`, or `N or none`, after the number `n`.
    fn count_after_number(&mut self, n: u64) -> Result<Count, ParseError> {
        if self.word() != Some("or") {
            return Ok(Count::Exactly(n));
        }
        self.at += 1;
        match self.next() {
            Some(Token {
                kind: TokenKind::Word(word),
                ..
            }) if word == "none" => Ok(Count::ExactlyOrNone(n)),
            other => Err(Self::malformed(
                format!(
                    "expected `none` after `{n} or`, as expect {n} or none, but found {}",
                    describe(other)
                ),
                other,
                self.end_position(),
            )),
        }
    }

    /// `least N` or `most N`, after `at`.
    fn bounded_count(&mut self) -> Result<Count, ParseError> {
        let bound = self.next();
        let at_least = match bound.map(|t| &t.kind) {
            Some(TokenKind::Word(w)) if w == "least" => true,
            Some(TokenKind::Word(w)) if w == "most" => false,
            _ => {
                return Err(Self::malformed(
                    format!(
                        "expected `least` or `most` after `at`, but found {}",
                        describe(bound)
                    ),
                    bound,
                    self.end_position(),
                ));
            }
        };
        match self.next() {
            Some(Token {
                kind: TokenKind::Number(n),
                ..
            }) => Ok(if at_least {
                Count::AtLeast(*n)
            } else {
                Count::AtMost(*n)
            }),
            other => Err(Self::malformed(
                format!("expected a number, but found {}", describe(other)),
                other,
                self.end_position(),
            )),
        }
    }

    /// The error for a token that cannot follow a complete statement.
    fn after_statement(&self, token: &Token) -> ParseError {
        let previous = &self.tokens[self.at - 1];
        let follows_an_operand = matches!(
            previous.kind,
            TokenKind::Quoted(_) | TokenKind::Backticked(_)
        );
        // Text straight after an operand's closing delimiter is most likely the rest of an
        // operand that held its own delimiter and needed a longer fence.
        if follows_an_operand && token.start == previous.end {
            return ParseError {
                kind: ErrorKind::Malformed,
                message: format!(
                    "{} follows straight on from an operand that closed here: if the operand \
                     contains its own delimiter, open and close it with a longer run, as \
                     \"\"\" say \"hi\" \"\"\"",
                    token.kind
                ),
                start: previous.end - 1,
                end: previous.end,
            };
        }
        if let TokenKind::Word(word) = &token.kind {
            if CLAUSE_ORDER.contains(&word.as_str()) {
                return Self::malformed(
                    format!(
                        "`{word}` cannot come here: clauses come in the order in, where, expect, \
                         each at most once"
                    ),
                    Some(token),
                    0,
                );
            }
            if CLAUSES_NOT_IN_THIS_BUILD.contains(&word.as_str()) {
                return Self::unknown(
                    format!("unknown construct: the `{word}` clause is not in this build"),
                    token,
                );
            }
        }
        Self::malformed(
            format!(
                "expected the end of the statement - a new line or `;` - but found {}",
                token.kind
            ),
            Some(token),
            0,
        )
    }
}

/// What a glob is compared with, which decides what may appear in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobUse {
    /// A path: `in`, and `FILE`.  No anchors and no line breaks.
    Path,
    /// A metavariable's value, a string.
    Value,
}

impl GlobUse {
    /// What is expected, as an error names it.
    fn what(self) -> &'static str {
        match self {
            GlobUse::Path => "a path or a glob",
            GlobUse::Value => "a glob to compare with",
        }
    }

    /// An example of one.
    fn example(self) -> &'static str {
        match self {
            GlobUse::Path => "\"src/\"**",
            GlobUse::Value => "\"https\"*",
        }
    }
}

/// How an operand is used, which decides what may appear in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandUse {
    /// What a statement searches for.  Anchors allowed; empty text refused.
    Search,
    /// What a replace writes.  No anchors; empty text allowed, to delete text.
    Replacement,
    /// What a `where` compares against.
    Comparison,
}

/// The first value filter in a pattern - `${name|...}` - if there is one.
fn value_filter(pattern: &str) -> Option<&str> {
    let mut rest = pattern;
    while let Some(i) = rest.find("${") {
        let after = &rest[i + 2..];
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if name_len > 0 && after[name_len..].starts_with('|') {
            let close = after.find('}').map_or(after.len(), |c| c + 1);
            return Some(&rest[i..i + 2 + close]);
        }
        rest = after;
    }
    None
}

fn describe(token: Option<&Token>) -> String {
    token.map_or_else(|| "the end of the script".into(), |t| t.kind.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use quaffed_representation::program::TextPart;

    fn program(text: &str) -> Result<Program, ParseError> {
        let tokens = lexer::tokens(text).map_err(|e| ParseError {
            kind: ErrorKind::Malformed,
            message: e.message,
            start: e.start,
            end: e.end,
        })?;
        let mut program = Program::default();
        parse(&tokens, 0, &mut program)?;
        Ok(program)
    }

    fn error(text: &str) -> ParseError {
        program(text).expect_err(text)
    }

    fn only(text: &str) -> Statement {
        let program = program(text).unwrap();
        assert_eq!(program.body().len(), 1, "{text}");
        program.body()[0].node.clone()
    }

    fn text_operand(statement: &Statement) -> &TextPattern {
        match statement {
            Statement::Find(query) => match &query.operand.node {
                Operand::Text(pattern) => pattern,
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn find_is_optional_before_an_operand() {
        assert_eq!(only("find \"TODO\""), only("\"TODO\""));
        assert!(matches!(only("`f($x)`"), Statement::Find(_)));
    }

    #[test]
    fn a_find_with_an_expect_is_an_assertion_and_consecutive_ones_are_one_block() {
        let program = program("find \"a\" expect 1\nfind \"b\" expect none; find \"c\"").unwrap();
        assert!(matches!(&program.body()[0].node, Statement::Assert(block) if block.len() == 2));
        assert!(matches!(program.body()[1].node, Statement::Find(_)));
    }

    #[test]
    fn every_count_form_parses() {
        let counts = [
            ("3", Count::Exactly(3)),
            ("at least 1", Count::AtLeast(1)),
            ("at most 5", Count::AtMost(5)),
            ("3 or none", Count::ExactlyOrNone(3)),
            ("none", Count::None),
            ("any", Count::Any),
        ];
        for (written, count) in counts {
            let statement = only(&format!("find \"x\" expect {written}"));
            let Statement::Assert(block) = statement else {
                panic!("{written}")
            };
            assert_eq!(block[0].node.count.node, count, "{written}");
        }
    }

    #[test]
    fn pieces_join_and_markers_build_the_pattern() {
        let statement = only(r#"find ^"def run():" $^ "    pass" " " "x"$"#);
        let pattern = text_operand(&statement);
        assert!(pattern.start_anchor && pattern.end_anchor);
        assert_eq!(
            pattern.parts(),
            [
                TextPart::Literal("def run():".into()),
                TextPart::LineBreak,
                TextPart::Literal("    pass x".into()),
            ]
        );
    }

    #[test]
    fn a_semicolon_inside_an_operand_survives() {
        let statement = only(r#"find "a; b""#);
        assert_eq!(text_operand(&statement), &TextPattern::literal("a; b"));
    }

    #[test]
    fn layout_and_comments_do_not_change_the_program() {
        let tight = program("find \"a\" expect 1;find `f($x)`").unwrap();
        let loose =
            program("# a comment\n\n   find   \"a\"   expect   1   # why\n\n find `f($x)`\n")
                .unwrap();
        assert_eq!(tight, loose);
        assert_ne!(tight, program("find \"a\" expect 2;find `f($x)`").unwrap());
    }

    #[test]
    fn replace_and_delete_parse_with_their_expects() {
        let Statement::Act(action) = only(r#"replace string "a" with "b" expect 2"#) else {
            panic!()
        };
        assert!(matches!(action.kind, ActionKind::Replace(_)));
        assert_eq!(action.expect[0].node, Count::Exactly(2));
        let Statement::Act(action) = only("delete `pass` expect any") else {
            panic!()
        };
        assert_eq!(action.kind, ActionKind::Delete);
        // Without an expect the action parses; the checker refuses it.
        let Statement::Act(action) = only("replace `a` with `b`") else {
            panic!()
        };
        assert_eq!(action.expect, []);
    }

    #[test]
    fn textual_operations_are_spelled_textually() {
        let err = error("replace string `a` with `b` expect 1");
        assert_eq!((err.kind, err.start), (ErrorKind::Malformed, 15));
        assert!(err.message.contains("drop `string`"), "{}", err.message);
        let err = error(r#"replace "a" with "b" expect 1"#);
        assert_eq!(err.start, 8);
        assert!(err.message.contains("replace string"), "{}", err.message);
    }

    #[test]
    fn clauses_parse_in_order_and_refuse_any_other() {
        let Statement::Assert(block) = only(
            r#"find "x" in "src/"** where not FILE matches *"_test.py" and $a contains "b" expect 1"#,
        ) else {
            panic!()
        };
        let quaffed_representation::program::Counted::Matches(query) = &block[0].node.counted
        else {
            panic!("counted matches")
        };
        assert_eq!(
            query.scope[0].node.parts(),
            [GlobPart::Literal("src/".into()), GlobPart::DoubleStar]
        );
        assert!(matches!(
            query.filter.as_ref().unwrap().node,
            Filter::And(_)
        ));
        let out_of_order = error(r#"find "x" expect 1 in "src""#);
        assert!(
            out_of_order.message.contains("cannot follow `expect`"),
            "{}",
            out_of_order.message
        );
        assert_eq!(out_of_order.start, 18);
        let repeated = error(r#"find "x" in "a" in "b""#);
        assert!(
            repeated.message.contains("repeated"),
            "{}",
            repeated.message
        );
        assert_eq!(repeated.start, 16);
        // After `expect any`, a clause keyword is a clause, not a finding set.
        let after_any = error(r#"find "x" expect any in "src""#);
        assert_eq!(after_any.kind, ErrorKind::Malformed);
        assert!(
            after_any.message.contains("cannot follow `expect`"),
            "{}",
            after_any.message
        );
        assert_eq!(after_any.start, 20);
        let repeated_after_any = error("delete `x` expect any expect 1");
        assert_eq!(repeated_after_any.kind, ErrorKind::Malformed);
        assert!(
            repeated_after_any.message.contains("repeated"),
            "{}",
            repeated_after_any.message
        );
        let not_built_after_any = error("find `x` expect any as y");
        assert_eq!(not_built_after_any.kind, ErrorKind::Unknown);
        assert!(
            not_built_after_any.message.contains("`as`"),
            "{}",
            not_built_after_any.message
        );
    }

    /// The glob of a `find "x" where ... matches GLOB` statement's one test.
    fn compared(text: &str) -> Glob {
        let Statement::Find(query) = only(text) else {
            panic!("{text} is a find")
        };
        match query.filter.map(|f| f.node) {
            Some(Filter::Matches(_, operand) | Filter::Contains(_, operand)) => {
                match operand.node {
                    Operand::Glob(glob) => glob,
                    other => panic!("{text} compares with {other:?}"),
                }
            }
            other => panic!("{text} gave {other:?}"),
        }
    }

    #[test]
    fn a_glob_is_quoted_pieces_and_the_wildcards_between_them() {
        use quaffed_representation::glob::ClassItem::{One, Range};
        let glob = compared(r#"find "x" where $s matches *"?page="*"#);
        assert_eq!(
            glob.parts(),
            [
                GlobPart::Star,
                GlobPart::Literal("?page=".into()),
                GlobPart::Star
            ]
        );
        let glob = compared(r#"find "x" where $s matches "v"[0-9][!a] ? ** $^ "end""#);
        assert_eq!(
            glob.parts(),
            [
                GlobPart::Literal("v".into()),
                GlobPart::Class(Class {
                    negated: false,
                    items: vec![Range('0', '9')]
                }),
                GlobPart::Class(Class {
                    negated: true,
                    items: vec![One('a')]
                }),
                GlobPart::One,
                GlobPart::DoubleStar,
                GlobPart::LineBreak,
                GlobPart::Literal("end".into()),
            ]
        );
        let glob = compared(r#"find "x" where $s contains ^"a"$"#);
        assert!(glob.start_anchor && glob.end_anchor);
        assert_eq!(glob.as_literal(), Some("a"));
        // Alternatives hold globs, nested braces included, and adjacent text joins across none.
        let glob = compared(r#"find "x" where FILE matches {"src","lib"{"/a","/b"}}"/"*"#);
        let GlobPart::Alternatives(alternatives) = &glob.parts()[0] else {
            panic!("{glob:?}")
        };
        assert_eq!(alternatives[0], Glob::literal("src"));
        assert_eq!(alternatives[1].parts().len(), 2);
        assert_eq!(
            &glob.parts()[1..],
            [GlobPart::Literal("/".into()), GlobPart::Star]
        );
        let Statement::Find(query) = only(r#"find "x" in ":/tests/"**".py""#) else {
            panic!()
        };
        assert_eq!(
            query.scope[0].node.parts(),
            [
                GlobPart::Literal(":/tests/".into()),
                GlobPart::DoubleStar,
                GlobPart::Literal(".py".into())
            ]
        );
    }

    #[test]
    fn a_glob_refuses_what_its_value_cannot_have() {
        let cases = [
            (
                r#"find "x" in ^"src""#,
                "`^` cannot be in a path: a path has no lines",
            ),
            (r#"find "x" in "src"$"#, "`$` cannot be in a path"),
            (
                r#"find "x" where FILE matches "a"$^"b""#,
                "`$^` cannot be in a path",
            ),
            (
                r#"find "x" where FILE matches `f()`"#,
                "FILE is a path, so it is compared with a glob",
            ),
            (r#"find "x" in `src`"#, "not a backticked pattern"),
            (
                r#"find "x" in expect 1"#,
                "expected a path or a glob - quoted text and wildcards, as \"src/\"** - but \
                 found `expect`",
            ),
            (
                r#"find "x" where $s matches"#,
                "expected a glob to compare with",
            ),
            (
                r#"find "x" in {src,"lib"}"#,
                "expected an alternative - quoted text and wildcards - or `,` or `}`, but found \
                 `src`: alternatives are quoted",
            ),
            (r#"find "x" in {"a",}"#, "an alternative is empty here"),
            (r#"find "x" in {"a""#, "these alternatives are never closed"),
            (
                "find \"x\" where $s matches [a\nb]",
                "cannot hold a line break",
            ),
            (
                r#"find "a"*"b""#,
                "`*` is a wildcard, and wildcards are for `in` and `where`",
            ),
            (
                r#"replace string "a" with "b"? expect 1"#,
                "`?` is a wildcard",
            ),
        ];
        for (text, wanted) in cases {
            let err = error(text);
            assert_eq!(err.kind, ErrorKind::Malformed, "{text}");
            assert!(err.message.contains(wanted), "{text}: {}", err.message);
        }
        let capture = error(r#"find "x" where $s matches "a"$b"#);
        assert_eq!(capture.kind, ErrorKind::Unknown);
        // Only between pieces is a metavariable or a wildcard read as one: first, it is not
        // a glob or an operand at all.
        for (text, wanted) in [
            (
                r#"find "x" where $s matches $t"#,
                "expected a glob to compare with - quoted text and wildcards, as \"https\"* - but \
                 found `$t`",
            ),
            (
                r#"find *"x""#,
                "expected an operand - quoted text or a backticked pattern - but found `*`",
            ),
        ] {
            let err = error(text);
            assert_eq!(err.kind, ErrorKind::Malformed, "{text}");
            assert_eq!(err.message, wanted, "{text}");
        }
    }

    #[test]
    fn a_class_is_refused_only_if_it_holds_a_line_break() {
        for (class, refused) in [
            ("[\n]", true),
            ("[\r]", true),
            ("[\t-\n]", true),
            ("[\r-\x0e]", true),
            ("[\t-\t]", false),
            // Between LF and CR, and holding neither.
            ("[\x0b-\x0c]", false),
            ("[\x0e-~]", false),
        ] {
            let text = format!(r#"find "x" where $s matches {class}"#);
            assert_eq!(program(&text).is_err(), refused, "{class:?}");
        }
    }

    #[test]
    fn a_metavariable_is_compared_with_a_backticked_pattern_by_its_shape() {
        let Statement::Find(query) = only("find `f($x)` where $x matches `g($y)`") else {
            panic!("a find")
        };
        match query.filter.map(|f| f.node) {
            Some(Filter::Matches(_, operand)) => {
                assert_eq!(operand.node, Operand::Pattern("g($y)".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_run_of_nots_is_the_one_negation_it_means_however_long() {
        let plain = program(r#"find "x" where FILE matches "a""#).unwrap();
        let once = program(r#"find "x" where not FILE matches "a""#).unwrap();
        assert_ne!(plain, once);
        assert_eq!(
            program(r#"find "x" where not not FILE matches "a""#).unwrap(),
            plain
        );
        assert_eq!(
            program(r#"find "x" where not not not FILE matches "a""#).unwrap(),
            once
        );
        // Deep enough to have overflowed the stack when each `not` recursed.
        let many = format!(
            r#"find "x" where {}FILE matches "a""#,
            "not ".repeat(200_001)
        );
        assert_eq!(program(&many).unwrap(), once);
        let Statement::Find(query) = only(r#"find "x" where not not FILE matches "a""#) else {
            panic!("a find")
        };
        // The span still starts at the first `not`.
        assert_eq!(query.filter.unwrap().span.start, 15);
    }

    #[test]
    fn delete_takes_a_pattern_never_text() {
        let err = error(r#"delete "x" expect 1"#);
        assert_eq!(
            (err.kind, err.start, err.end),
            (ErrorKind::Malformed, 7, 10)
        );
        assert_eq!(
            err.message,
            "`delete` removes matched nodes, so it takes a backticked pattern, not quoted text; \
             to remove text, replace it with nothing: replace string \"...\" with \"\""
        );
    }

    #[test]
    fn anchors_and_joining_on_a_pattern_are_refused() {
        for text in [
            "find ^`x`",
            "find `x`$",
            "find `x` $^ \"y\"",
            "find `x` \"y\"",
        ] {
            let err = error(text);
            assert_eq!(err.kind, ErrorKind::Malformed, "{text}");
            assert!(
                err.message.contains("already anchored"),
                "{text}: {}",
                err.message
            );
        }
    }

    #[test]
    fn anchors_in_a_replacement_are_refused() {
        let err = error(r#"replace string "a" with "b"$ expect 1"#);
        assert!(err.message.contains("only writes"), "{}", err.message);
    }

    #[test]
    fn a_line_break_needs_a_piece_after_it() {
        let err = error(r#"find "a" $^"#);
        assert!(err.message.contains("must follow"), "{}", err.message);
    }

    #[test]
    fn an_empty_search_is_refused_but_an_empty_replacement_is_not() {
        assert!(error(r#"find """#).message.contains("empty"));
        assert!(program(r#"replace string "a" with "" expect 1"#).is_ok());
    }

    #[test]
    fn an_unfenced_delimiter_is_named_where_the_operand_closed() {
        let err = error(r#"find "say "hi"""#);
        assert_eq!(err.kind, ErrorKind::Malformed);
        assert_eq!((err.start, err.end), (10, 11));
        assert!(err.message.contains("longer run"), "{}", err.message);
    }

    #[test]
    fn statements_this_build_does_not_have_are_unknown() {
        for (text, needle) in [
            (
                "rename `f` to `g` expect 1",
                "`rename` is not in this build",
            ),
            ("fnd \"x\"", "unknown statement: `fnd`"),
            ("expect none `import pdb`", "standalone `expect`"),
            ("find \"a\" $x \"b\"", "textual capture"),
            ("replace `x` with `${x|content}` expect 1", "`${x|content}`"),
            ("find `f` as $x", "`as` clause"),
            ("find `f` reject `g`", "`reject` clause"),
            ("find `f` in $set", "bound set"),
            ("find `f` where ENCLOSING matches `def`", "`ENCLOSING`"),
            ("find `f` where $x in [\"a\"]", "where ... in"),
            ("find `f` expect any orphans", "finding set"),
            ("find `f` expect applicable 3", "applicable"),
            ("for $x in y", "`for` is not in this build"),
        ] {
            let err = error(text);
            assert_eq!(err.kind, ErrorKind::Unknown, "{text}");
            assert!(err.message.contains(needle), "{text}: {}", err.message);
        }
    }

    #[test]
    fn malformed_statements_name_what_was_expected() {
        for (text, needle) in [
            ("find", "but the script ends"),
            ("find \"x\" expect", "expected a count"),
            ("find \"x\" expect at 3", "`least` or `most`"),
            ("find \"x\" expect 3 or 4", "expected `none`"),
            ("find \"x\" in `src`", "not a backticked pattern"),
            ("find \"x\" where FILE", "`matches` or `contains`"),
            (
                "find \"x\" where FILE matches \"a\" or FILE matches \"b\"",
                "no `or`",
            ),
            ("replace string \"a\" \"b\"", "expected `with`"),
            ("$x", "cannot start with"),
            ("find \"x\" \"y\" z", "expected the end of the statement"),
        ] {
            let err = error(text);
            assert_eq!(err.kind, ErrorKind::Malformed, "{text}");
            assert!(err.message.contains(needle), "{text}: {}", err.message);
        }
    }

    /// Each message in full, so a branch that produced another's words would show.
    #[test]
    fn error_messages_are_exact() {
        for (text, message) in [
            (
                "fnd \"x\"",
                "unknown statement: `fnd`; the statements in this build are find, replace and \
                 delete",
            ),
            (
                "load \"x\"",
                "unknown statement: `load` is not in this build",
            ),
            (
                "find $x",
                "expected an operand - quoted text or a backticked pattern - but found `$x`",
            ),
            (
                "find $",
                "expected an operand - quoted text or a backticked pattern - but found `$`",
            ),
            (
                "find $^ \"x\"",
                "expected an operand - quoted text or a backticked pattern - but found `$^`",
            ),
            (
                "replace string \"a\" by \"b\"",
                "expected `with` and the replacement, but found `by`",
            ),
            (
                "find \"x\" where FOO matches \"a\"",
                "a condition tests a metavariable or FILE, but found `FOO`; for example, where \
                 $x matches \"test_\"*",
            ),
            (
                "find \"x\" where FILE equals \"a\"",
                "expected `matches` or `contains` after the subject, but found `equals`",
            ),
            (
                "find \"x\" expect lots",
                "expected a count after `expect` - a number, at least N, at most N, N or none, \
                 none or any - but found `lots`",
            ),
            (
                "find \"x\" expect 3 or some",
                "expected `none` after `3 or`, as expect 3 or none, but found `some`",
            ),
            (
                "find \"x\" expect at",
                "expected `least` or `most` after `at`, but found the end of the script",
            ),
            (
                "find \"x\" expect at foo 3",
                "expected `least` or `most` after `at`, but found `foo`",
            ),
            (
                "find \"x\" expect at most",
                "expected a number, but found the end of the script",
            ),
            (
                "find \"x\" expect no orphans",
                "unknown construct: `expect no` is not in this build",
            ),
        ] {
            assert_eq!(error(text).message, message, "{text}");
        }
    }

    #[test]
    fn an_error_at_the_end_points_just_past_the_last_token() {
        let err = error("find \"x\" expect at");
        assert_eq!((err.start, err.end), (18, 18));
        let err = error("find");
        assert_eq!((err.start, err.end), (4, 4));
    }

    #[test]
    fn the_value_filter_is_found_only_in_its_own_form() {
        assert_eq!(value_filter("f(${x|content})"), Some("${x|content}"));
        assert_eq!(value_filter("f(${x})"), None);
        assert_eq!(value_filter("${ | }"), None);
        assert_eq!(value_filter("${|x}"), None);
    }
}
