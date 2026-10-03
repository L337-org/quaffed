// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Tokens to statements in the operation representation, for the MVP subset.
//!
//! A word in statement position that is not a statement this build has is an unknown
//! statement, exit 5, naming it, and so is any construct outside the MVP; anything else
//! malformed is a parse error, exit 3.  Nothing is parsed and ignored.

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
    scope: Vec<Spanned<String>>,
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
                message: "`delete` removes matched nodes, so it takes a backticked pattern, not                           quoted text; to remove text, replace it with nothing:                           replace string \"...\" with \"\""
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

    fn scope_glob(&mut self, keyword: &Token) -> Result<Spanned<String>, ParseError> {
        match self.next() {
            Some(
                token @ Token {
                    kind: TokenKind::Quoted(glob),
                    ..
                },
            ) => Ok(Spanned::new(glob.clone(), self.token_span(token))),
            Some(
                token @ Token {
                    kind: TokenKind::Metavariable { .. },
                    ..
                },
            ) => Err(Self::unknown(
                "unknown construct: `in` over a bound set is not in this build".into(),
                token,
            )),
            other => Err(Self::malformed(
                format!(
                    "`in` takes a quoted glob, such as in \"src/**/*.py\", but found {}",
                    describe(other)
                ),
                other.or(Some(keyword)),
                0,
            )),
        }
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
    fn term(&mut self) -> Result<Spanned<Filter>, ParseError> {
        let Some(first) = self.next() else {
            return Err(Self::malformed(
                "expected a condition after `where`, such as where not FILE matches \
                 \"**/test_*.py\""
                    .into(),
                None,
                self.end_position(),
            ));
        };
        if first.kind == TokenKind::Word("not".into()) {
            let inner = self.term()?;
            let span = self.span(first.start, inner.span.end);
            return Ok(Spanned::new(Filter::Not(Box::new(inner)), span));
        }
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
                         example, where $x matches \"test_*\""
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
        let operand = self.operand(OperandUse::Comparison)?;
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
            r#"find "x" in "src/**" where not FILE matches "*_test.py" and $a contains "b" expect 1"#,
        ) else {
            panic!()
        };
        let quaffed_representation::program::Counted::Matches(query) = &block[0].node.counted
        else {
            panic!("counted matches")
        };
        assert_eq!(query.scope[0].node, "src/**");
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

    #[test]
    fn delete_takes_a_pattern_never_text() {
        let err = error(r#"delete "x" expect 1"#);
        assert_eq!(
            (err.kind, err.start, err.end),
            (ErrorKind::Malformed, 7, 10)
        );
        assert!(err.message.contains("replace string"), "{}", err.message);
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
            ("find \"x\" in `src`", "quoted glob"),
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
                 $x matches \"test_*\"",
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
