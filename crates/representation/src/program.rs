// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A program: what a script and the command line mean, as operations, with no syntax left.
//!
//! The DSL parser and the command line both produce a [`Program`], and nothing else reaches the
//! engine.  Enums are `non_exhaustive` because the command set is additive-only: a construct
//! outside the MVP joins as a new variant, never by reshaping one that exists.

use std::path::PathBuf;

use crate::span::{Source, Span, Spanned};

/// A whole program: its sources, its statements in source order, and the scope the command line
/// narrowed it to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Program {
    /// Every source, in command-line order; a [`Span`]'s `source` indexes this.
    pub sources: Vec<Source>,
    /// The statements, in source order.
    pub body: Vec<Spanned<Statement>>,
    /// The one positional scope argument, which narrows every statement's scope.
    pub narrow_to: Option<PathBuf>,
}

/// One statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Statement {
    /// Search, and report what was found.
    Find(Query),
    /// Replace or delete, with the assertions that guard it.
    Act(Action),
    /// A block of assertions, evaluated to the end before a failure stops the run.
    ///
    /// A maximal run of asserting statements - in the MVP, `find` statements carrying an
    /// `expect` - grouped as the program is built, so the same words either side of an action
    /// stay separate questions.
    Assert(Vec<Spanned<Expectation>>),
}

/// What to look for, where, and which results to keep.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Query {
    /// What to match.  Its kind is the matching mode, never a separate flag.
    pub operand: Spanned<Operand>,
    /// `in` globs, resolved against the project root; empty for the whole project.
    pub scope: Vec<Spanned<String>>,
    /// The `where` filter, if any.
    pub filter: Option<Spanned<Filter>>,
}

impl Query {
    /// A query for `operand` over the whole project, unfiltered.
    #[must_use]
    pub fn new(operand: Spanned<Operand>) -> Self {
        Query {
            operand,
            scope: Vec::new(),
            filter: None,
        }
    }
}

/// What a query matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Operand {
    /// A quoted operand: text, compared as text, never parsed.
    Text(TextPattern),
    /// A backticked operand: a snippet of the target language, carried as written.
    ///
    /// It is compiled by the engine through the front end for each file's language, so this
    /// crate depends on no parser.
    Pattern(String),
    /// A node already bound by an earlier statement.  Outside the MVP: nothing in it can bind.
    Bound(String),
}

/// A textual operand: literal text and line breaks, optionally anchored at either end.
///
/// Built from the pieces a script writes - adjacent quoted operands, `$^` between them, `^`
/// before the first and `$` after the last.  Adjacent literal text is joined as it is built,
/// so `"ab"` and `"a" "b"` are the same pattern.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TextPattern {
    /// Matches only at the start of a line.
    pub start_anchor: bool,
    /// The parts, in order, with no two literals adjacent.
    parts: Vec<TextPart>,
    /// Matches only at the end of a line.
    pub end_anchor: bool,
}

/// One part of a [`TextPattern`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextPart {
    /// Text that must appear exactly.
    Literal(String),
    /// A line break: LF, CRLF or CR, whichever the file has there.
    LineBreak,
}

impl TextPattern {
    /// An unanchored pattern of one literal, as `-s` and the bare query give.
    pub fn literal(text: impl Into<String>) -> Self {
        let mut pattern = TextPattern::default();
        pattern.push_literal(text.into());
        pattern
    }

    /// Appends literal text, joining it to a literal it follows.  Empty text adds nothing.
    pub fn push_literal(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(TextPart::Literal(last)) = self.parts.last_mut() {
            last.push_str(&text);
        } else {
            self.parts.push(TextPart::Literal(text));
        }
    }

    /// Appends a line break.
    pub fn push_line_break(&mut self) {
        self.parts.push(TextPart::LineBreak);
    }

    /// The parts, in order.
    #[must_use]
    pub fn parts(&self) -> &[TextPart] {
        &self.parts
    }

    /// Whether the pattern would match nothing at all: no text and no line break.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

/// A `where` filter over what a query found.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Filter {
    /// Every part holds.
    And(Vec<Spanned<Filter>>),
    /// The part does not hold.
    Not(Box<Spanned<Filter>>),
    /// The subject's whole value matches the operand.
    Matches(Subject, Spanned<Operand>),
    /// Some part of the subject's value matches the operand.
    Contains(Subject, Spanned<Operand>),
}

/// What a filter tests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Subject {
    /// A metavariable bound by the query's pattern, by name, without its `$`.
    Metavariable(String),
    /// `FILE`: the path of the file a match is in, relative to the project root.
    File,
}

/// An edit, and the assertions that guard it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Action {
    /// What the edit does.
    pub kind: ActionKind,
    /// What it applies to.
    pub query: Query,
    /// Its `expect` clauses.  Required: an edit with none is refused.
    pub expect: Vec<Spanned<Count>>,
}

impl Action {
    /// An edit of `kind` to what `query` finds, guarded by `expect`.
    #[must_use]
    pub fn new(kind: ActionKind, query: Query, expect: Vec<Spanned<Count>>) -> Self {
        Action {
            kind,
            query,
            expect,
        }
    }
}

/// The kind of edit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActionKind {
    /// Replace each match with this operand: text for text, a pattern for a pattern.
    Replace(Spanned<Operand>),
    /// Remove each match.
    Delete,
}

/// One assertion: a count of something counted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Expectation {
    /// The count that must hold.
    pub count: Spanned<Count>,
    /// What is counted.
    pub counted: Counted,
}

impl Expectation {
    /// An assertion that `query` finds `count` matches.
    #[must_use]
    pub fn of_matches(count: Spanned<Count>, query: Query) -> Self {
        Expectation {
            count,
            counted: Counted::Matches(query),
        }
    }
}

/// What an assertion counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Counted {
    /// A query's matches.
    Matches(Query),
}

/// A count an assertion requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Count {
    /// Exactly this many.
    Exactly(u64),
    /// This many or more.
    AtLeast(u64),
    /// This many or fewer.
    AtMost(u64),
    /// Exactly this many, or none: the idempotent refactor.
    ExactlyOrNone(u64),
    /// None at all.
    None,
    /// Any number: the explicit opt-out on an edit.
    Any,
}

impl Count {
    /// Whether `found` satisfies the count.
    #[must_use]
    pub fn holds(self, found: u64) -> bool {
        match self {
            Count::Exactly(n) => found == n,
            Count::AtLeast(n) => found >= n,
            Count::AtMost(n) => found <= n,
            Count::ExactlyOrNone(n) => found == n || found == 0,
            Count::None => found == 0,
            Count::Any => true,
        }
    }
}

impl Program {
    /// Appends `statement`, grouping consecutive assertion blocks into one.
    ///
    /// An `Assert` that follows another `Assert` joins it, so a run of asserting statements is
    /// one block however many statements wrote it.  Within a block, an expectation equal to one
    /// already there - the same statement, wherever it was written - is not added again.
    pub fn push(&mut self, statement: Spanned<Statement>) {
        if let Statement::Assert(new) = &statement.node
            && let Some(Spanned {
                node: Statement::Assert(block),
                ..
            }) = self.body.last_mut()
        {
            for expectation in new {
                if !block.contains(expectation) {
                    block.push(expectation.clone());
                }
            }
            return;
        }
        self.body.push(statement);
    }
}

/// The span of a statement, for building one where only its parts are known.
#[must_use]
pub fn statement_span(first: Span, last: Span) -> Span {
    Span {
        source: first.source,
        start: first.start,
        end: if last.source == first.source {
            last.end
        } else {
            first.end
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: usize) -> Span {
        Span {
            source: 0,
            start,
            end: start + 1,
        }
    }

    fn find_text(text: &str, start: usize) -> Query {
        Query::new(Spanned::new(
            Operand::Text(TextPattern::literal(text)),
            at(start),
        ))
    }

    fn assert_of(text: &str, count: Count, start: usize) -> Spanned<Statement> {
        Spanned::new(
            Statement::Assert(vec![Spanned::new(
                Expectation::of_matches(Spanned::new(count, at(start)), find_text(text, start)),
                at(start),
            )]),
            at(start),
        )
    }

    fn block_sizes(program: &Program) -> Vec<usize> {
        program
            .body
            .iter()
            .map(|s| match &s.node {
                Statement::Assert(block) => block.len(),
                _ => 0,
            })
            .collect()
    }

    #[test]
    fn adjacent_literals_join_and_empty_text_adds_nothing() {
        let mut joined = TextPattern::literal("a");
        joined.push_literal("b".into());
        joined.push_literal(String::new());
        assert_eq!(joined, TextPattern::literal("ab"));
        let mut broken = TextPattern::literal("a");
        broken.push_line_break();
        broken.push_literal("b".into());
        assert_eq!(
            broken.parts(),
            [
                TextPart::Literal("a".into()),
                TextPart::LineBreak,
                TextPart::Literal("b".into())
            ]
        );
        assert!(TextPattern::default().is_empty());
        assert!(!TextPattern::literal("a").is_empty());
        let mut only_a_break = TextPattern::default();
        only_a_break.push_line_break();
        assert!(!only_a_break.is_empty());
    }

    #[test]
    fn a_run_of_assertions_is_one_block_and_an_action_splits_runs() {
        let mut program = Program::default();
        program.push(assert_of("a", Count::Exactly(1), 0));
        program.push(assert_of("b", Count::None, 10));
        let action = Action::new(
            ActionKind::Delete,
            find_text("c", 20),
            vec![Spanned::new(Count::Any, at(20))],
        );
        program.push(Spanned::new(Statement::Act(action), at(20)));
        program.push(assert_of("a", Count::Exactly(1), 30));
        assert_eq!(block_sizes(&program), [2, 0, 1]);
    }

    #[test]
    fn the_same_assertion_twice_in_a_block_is_kept_once() {
        let mut program = Program::default();
        program.push(assert_of("a", Count::Exactly(1), 0));
        // The same statement, written somewhere else.
        program.push(assert_of("a", Count::Exactly(1), 50));
        // A different count is a different statement.
        program.push(assert_of("a", Count::Exactly(2), 60));
        assert_eq!(block_sizes(&program), [2]);
    }

    #[test]
    fn each_count_holds_exactly_where_it_should() {
        assert!(Count::Exactly(2).holds(2) && !Count::Exactly(2).holds(3));
        assert!(
            Count::AtLeast(2).holds(2) && Count::AtLeast(2).holds(9) && !Count::AtLeast(2).holds(1)
        );
        assert!(
            Count::AtMost(2).holds(0) && Count::AtMost(2).holds(2) && !Count::AtMost(2).holds(3)
        );
        assert!(Count::ExactlyOrNone(2).holds(0) && Count::ExactlyOrNone(2).holds(2));
        assert!(!Count::ExactlyOrNone(2).holds(1));
        assert!(Count::None.holds(0) && !Count::None.holds(1));
        assert!(Count::Any.holds(0) && Count::Any.holds(u64::MAX));
    }

    #[test]
    fn a_statement_span_covers_its_parts_within_one_source() {
        let first = Span {
            source: 0,
            start: 3,
            end: 5,
        };
        let last = Span {
            source: 0,
            start: 9,
            end: 12,
        };
        assert_eq!(
            statement_span(first, last),
            Span {
                source: 0,
                start: 3,
                end: 12
            }
        );
        let elsewhere = Span {
            source: 1,
            start: 0,
            end: 2,
        };
        assert_eq!(statement_span(first, elsewhere), first);
    }
}
