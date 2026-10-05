// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The rules a program must satisfy before anything runs, checked over the representation so
//! they hold however the program was written.

use std::fmt;

use crate::program::{Action, Counted, Filter, Operand, Program, Query, Statement};
use crate::span::{Span, Spanned};

/// Why a program cannot run, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// What is wrong.
    pub kind: ViolationKind,
    /// Where it was written.
    pub span: Span,
}

/// What is wrong with a program.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ViolationKind {
    /// An edit with no `expect`.  Every edit declares how many it expects.
    MissingExpect,
    /// An `expect` on an edit whose target is a node already bound, which is exactly one by
    /// construction, so a count says nothing.
    ExpectOnBoundTarget,
    /// A construct outside the MVP subset, by name: an unknown statement to this build.
    OutsideMvp(&'static str),
}

impl Violation {
    /// Whether this is a construct this build does not have, rather than a malformed program.
    /// The command line gives the two different exit codes; its exit-code table says which.
    #[must_use]
    pub fn is_unknown_statement(&self) -> bool {
        matches!(self.kind, ViolationKind::OutsideMvp(_))
    }
}

impl fmt::Display for Violation {
    /// The violation's message; where it is, the caller says, because only the caller holds
    /// the source text to turn a span into a line and column.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}

impl std::error::Error for Violation {}

impl fmt::Display for ViolationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViolationKind::MissingExpect => f.write_str(
                "an edit needs an expect clause saying how many it should change - `expect 1`, \
                 or `expect any` to opt out",
            ),
            ViolationKind::ExpectOnBoundTarget => f.write_str(
                "an edit of a node already bound takes no expect clause: there is exactly one",
            ),
            ViolationKind::OutsideMvp(construct) => {
                write!(f, "unknown statement: {construct} is not in this build")
            }
        }
    }
}

/// Checks `program` against the assertion rules, then against the MVP subset.
///
/// # Errors
///
/// Returns the first [`Violation`] in source order.  The assertion rules come first, so a
/// program that breaks one is told about it whatever else it uses.
pub fn check(program: &Program) -> Result<(), Violation> {
    for statement in program.body() {
        if let Statement::Act(action) = &statement.node {
            check_action(action, statement.span)?;
        }
    }
    match first_outside_mvp(program) {
        Some((construct, span)) => Err(Violation {
            kind: ViolationKind::OutsideMvp(construct),
            span,
        }),
        None => Ok(()),
    }
}

fn check_action(action: &Action, span: Span) -> Result<(), Violation> {
    let bound = matches!(action.query.operand.node, Operand::Bound(_));
    match (bound, action.expect.first()) {
        (false, None) => Err(Violation {
            kind: ViolationKind::MissingExpect,
            span,
        }),
        (true, Some(expect)) => Err(Violation {
            kind: ViolationKind::ExpectOnBoundTarget,
            span: expect.span,
        }),
        _ => Ok(()),
    }
}

/// The first construct in `program` outside the MVP subset, with where it was written.
///
/// The one answer to "is this in the MVP".  What it finds is reported as an unknown
/// statement, naming it.
#[must_use]
pub fn first_outside_mvp(program: &Program) -> Option<(&'static str, Span)> {
    program
        .body()
        .iter()
        .find_map(|statement| match &statement.node {
            Statement::Find(query) => query_outside_mvp(query),
            Statement::Act(action) => {
                query_outside_mvp(&action.query).or_else(|| match &action.kind {
                    crate::program::ActionKind::Replace(replacement) => {
                        operand_outside_mvp(replacement)
                    }
                    crate::program::ActionKind::Delete => None,
                })
            }
            Statement::Assert(block) => block.iter().find_map(|expectation| {
                let Counted::Matches(query) = &expectation.node.counted;
                query_outside_mvp(query)
            }),
        })
}

fn query_outside_mvp(query: &Query) -> Option<(&'static str, Span)> {
    operand_outside_mvp(&query.operand).or_else(|| {
        query
            .filter
            .as_ref()
            .and_then(|filter| filter_outside_mvp(filter))
    })
}

fn operand_outside_mvp(operand: &Spanned<Operand>) -> Option<(&'static str, Span)> {
    match &operand.node {
        Operand::Bound(_) => Some(("a bound node, needing `as` or `for`,", operand.span)),
        Operand::Text(_) | Operand::Pattern(_) => None,
    }
}

fn filter_outside_mvp(filter: &Spanned<Filter>) -> Option<(&'static str, Span)> {
    match &filter.node {
        Filter::And(parts) => parts.iter().find_map(filter_outside_mvp),
        Filter::Not(part) => filter_outside_mvp(part),
        Filter::Matches(_, operand) | Filter::Contains(_, operand) => operand_outside_mvp(operand),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{ActionKind, Count, Expectation, Subject, TextPattern};

    fn at(start: usize) -> Span {
        Span {
            source: 0,
            start,
            end: start + 1,
        }
    }

    fn text(t: &str, start: usize) -> Spanned<Operand> {
        Spanned::new(Operand::Text(TextPattern::literal(t)), at(start))
    }

    fn program_of(statement: Statement) -> Program {
        let mut program = Program::default();
        program.push(Spanned::new(statement, at(0)));
        program
    }

    fn delete(operand: Spanned<Operand>, expect: Vec<Spanned<Count>>) -> Statement {
        Statement::Act(Action::new(ActionKind::Delete, Query::new(operand), expect))
    }

    #[test]
    fn an_edit_without_an_expect_is_refused_where_it_was_written() {
        let violation = check(&program_of(delete(text("x", 0), vec![]))).unwrap_err();
        assert_eq!(violation.kind, ViolationKind::MissingExpect);
        assert_eq!(violation.span, at(0));
        assert!(!violation.is_unknown_statement());
    }

    #[test]
    fn an_edit_with_an_expect_passes() {
        let expect = vec![Spanned::new(Count::Exactly(1), at(5))];
        assert_eq!(check(&program_of(delete(text("x", 0), expect))), Ok(()));
    }

    #[test]
    fn an_expect_on_a_bound_target_is_refused_at_the_expect() {
        let bound = Spanned::new(Operand::Bound("f".into()), at(0));
        let expect = vec![Spanned::new(Count::Exactly(1), at(7))];
        let violation = check(&program_of(delete(bound, expect))).unwrap_err();
        assert_eq!(violation.kind, ViolationKind::ExpectOnBoundTarget);
        assert_eq!(violation.span, at(7));
    }

    #[test]
    fn a_bound_target_with_no_expect_passes_the_rules_but_is_outside_the_mvp() {
        let bound = Spanned::new(Operand::Bound("f".into()), at(4));
        let violation = check(&program_of(delete(bound, vec![]))).unwrap_err();
        assert!(violation.is_unknown_statement());
        assert_eq!(violation.span, at(4));
    }

    #[test]
    fn a_bound_node_anywhere_is_found_outside_the_mvp() {
        let in_filter = Query {
            filter: Some(Spanned::new(
                Filter::Not(Box::new(Spanned::new(
                    Filter::Matches(
                        Subject::File,
                        Spanned::new(Operand::Bound("x".into()), at(9)),
                    ),
                    at(8),
                ))),
                at(7),
            )),
            ..Query::new(text("a", 0))
        };
        let program = program_of(Statement::Find(in_filter));
        assert_eq!(
            first_outside_mvp(&program).map(|(_, span)| span),
            Some(at(9))
        );
        let in_assert = program_of(Statement::Assert(vec![Spanned::new(
            Expectation::of_matches(
                Spanned::new(Count::None, at(0)),
                Query::new(Spanned::new(Operand::Bound("y".into()), at(3))),
            ),
            at(0),
        )]));
        assert_eq!(
            first_outside_mvp(&in_assert).map(|(_, span)| span),
            Some(at(3))
        );
    }

    #[test]
    fn a_bound_node_as_a_replacement_or_under_and_or_contains_is_found_too() {
        let bound = |start| Spanned::new(Operand::Bound("b".into()), at(start));
        let replace = program_of(Statement::Act(Action::new(
            ActionKind::Replace(bound(6)),
            Query::new(text("a", 0)),
            vec![Spanned::new(Count::Any, at(9))],
        )));
        assert_eq!(first_outside_mvp(&replace).map(|(_, s)| s), Some(at(6)));
        let filtered = |filter| {
            program_of(Statement::Find(Query::with(
                text("a", 0),
                Vec::new(),
                Some(Spanned::new(filter, at(2))),
            )))
        };
        let contains = Filter::Contains(Subject::File, bound(5));
        assert_eq!(
            first_outside_mvp(&filtered(contains.clone())).map(|(_, s)| s),
            Some(at(5))
        );
        let and = Filter::And(vec![
            Spanned::new(Filter::Matches(Subject::File, text("x", 3)), at(3)),
            Spanned::new(contains, at(4)),
        ]);
        assert_eq!(
            first_outside_mvp(&filtered(and)).map(|(_, s)| s),
            Some(at(5))
        );
        let clean = Filter::And(vec![Spanned::new(
            Filter::Contains(Subject::File, text("x", 3)),
            at(3),
        )]);
        assert_eq!(first_outside_mvp(&filtered(clean)), None);
    }

    #[test]
    fn the_mvp_subset_passes() {
        let mut program = Program::default();
        program.push(Spanned::new(
            Statement::Find(Query::new(text("a", 0))),
            at(0),
        ));
        program.push(Spanned::new(
            delete(
                Spanned::new(Operand::Pattern("f($x)".into()), at(2)),
                vec![Spanned::new(Count::Any, at(3))],
            ),
            at(2),
        ));
        assert_eq!(check(&program), Ok(()));
    }

    #[test]
    fn each_violation_says_what_to_do() {
        assert!(
            ViolationKind::MissingExpect
                .to_string()
                .contains("expect any")
        );
        assert_eq!(
            ViolationKind::OutsideMvp("`rename`").to_string(),
            "unknown statement: `rename` is not in this build"
        );
        let bound = Violation {
            kind: ViolationKind::OutsideMvp("a bound node, needing `as` or `for`,"),
            span: at(0),
        };
        assert_eq!(
            bound.to_string(),
            "unknown statement: a bound node, needing `as` or `for`, is not in this build"
        );
    }
}
