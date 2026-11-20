// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Planning: a program into the steps this build can run, each with its patterns, and each
//! structural pattern and rewrite compiled for each language.
//!
//! The second phase.  In: the [`Built`] program, and the languages quaff parses.  Out: a [`Plan`],
//! which every phase after it reads, and which scoping completes with each `where`.

use quaffed_representation::program::{
    Action, ActionKind, Count, Counted, Expectation, Operand, Program, Query, Statement,
    TextPattern,
};
use quaffed_representation::span::{Span, Spanned};
use quaffed_syntax::FrontEnd;

use super::compile::{Compiled, compile};
use super::failure::Failure;
use super::sources::Built;

/// What a run does, worked out from its program before any file is looked at.
pub(super) struct Plan<'p> {
    /// The steps, in program order.
    pub(super) steps: Vec<Step<'p>>,
    /// Every pattern the steps search for, in the order [`Step::patterns`] lists them, with its
    /// query: the run's patterns, which a pattern's index is a place in.
    pub(super) searching: Vec<Searching<'p>>,
    /// What each of the run's patterns is replaced with, by its index: `None` for one only
    /// searched for.
    pub(super) replacements: Vec<Option<Replacement<'p>>>,
    /// Each structural replace's rewrite template, as written.
    pub(super) rewrites: Vec<PlannedRewrite<'p>>,
    /// The structural patterns and rewrites, compiled for each language, and once scoping has
    /// compiled them, the `where`s.
    pub(super) compiled: Vec<Compiled>,
}

/// Plans `built`'s program: its steps, and each structural pattern and rewrite in it compiled for
/// each of `languages`, in order, before any file is read.
///
/// # Errors
///
/// Returns a [`Failure`] for a construct in the MVP that is not built yet, a pattern no language
/// reads, or a rewrite no language that reads its pattern reads.
pub(super) fn plan<'p>(
    languages: &[&'static dyn FrontEnd],
    built: &'p Built,
) -> Result<Plan<'p>, Failure> {
    let (mut steps, structural, rewrites) =
        steps(&built.program).map_err(|(construct, span)| Failure::Unknown {
            location: built.locate(span, span.start),
            message: format!("{construct} is not built yet"),
        })?;
    let compiled = compile(languages, &structural, &rewrites, built)?;
    for step in &mut steps {
        if let Step::Replace(replacing) = step
            && let ReplaceKind::Structural { rewrite, .. } = replacing.kind
        {
            // A rewrite that writes a comment replaces the code with it; only one that writes
            // nothing deletes.
            replacing.deletes = rewrites[rewrite].0.trim().is_empty();
        }
    }
    let searching = steps.iter().flat_map(Step::patterns).collect();
    let replacements = steps.iter().flat_map(Step::replacements).collect();
    Ok(Plan {
        steps,
        searching,
        replacements,
        rewrites,
        compiled,
    })
}

/// What `delete` replaces each match with: a rewrite with no code.
static NOTHING: Operand = Operand::Pattern(String::new());

/// One step of a program this build can run, with the query of each pattern in it, whose scope
/// and filter decide which files it looks at.
pub(super) enum Step<'p> {
    /// Search, and print what was found.
    Find(Wanted<'p>, &'p Query),
    /// Search for each, print what was found, and check each count.
    Assert(Vec<Assertion<'p>>),
    /// Replace text, print what was replaced, and check its count.
    Replace(Replacing<'p>),
}

/// A replace this build can run.
pub(super) struct Replacing<'p> {
    /// What it replaces, and with what.
    kind: ReplaceKind<'p>,
    /// The count its `expect` requires.
    pub(super) count: Count,
    /// Where the statement was written.
    pub(super) span: Span,
    /// Its query.
    pub(super) query: &'p Query,
    /// Whether its rewrite is empty, so that it deletes what matched, however it is spelled: set
    /// once the rewrite is compiled, and only for how its outcome is worded.  One that is only a
    /// comment is a replace.
    pub(super) deletes: bool,
}

impl Replacing<'_> {
    /// What the statement does, as a message names it.
    pub(super) fn verb(&self) -> &'static str {
        if self.deletes { "delete" } else { "replace" }
    }
}

/// What a replace replaces, and with what.
#[derive(Clone, Copy)]
enum ReplaceKind<'p> {
    /// Text, with text.
    Text {
        pattern: &'p TextPattern,
        with: &'p TextPattern,
    },
    /// A structural pattern, with a rewrite template: the pattern as a search finds it, and the
    /// template's index among the run's rewrites.
    Structural { pattern: Wanted<'p>, rewrite: usize },
}

impl<'p> Replacing<'p> {
    /// What it replaces, as a search finds it.
    pub(super) fn wanted(&self) -> Wanted<'p> {
        match self.kind {
            ReplaceKind::Text { pattern, .. } => Wanted::Text(pattern),
            ReplaceKind::Structural { pattern, .. } => pattern,
        }
    }
}

/// What one of the run's patterns is replaced with, if it is replaced.
#[derive(Clone, Copy)]
pub(super) enum Replacement<'p> {
    /// Text.
    Text(&'p TextPattern),
    /// A rewrite template, by its index among the run's rewrites.
    Structural(usize),
}

/// A structural replace's rewrite template as written: its text, where it was written, and the
/// index of the compiled pattern whose captures it writes back.
pub(super) type PlannedRewrite<'p> = (&'p str, Span, usize);

/// A program's steps, each structural pattern in it with where it was written, and each rewrite
/// template.
type Planned<'p> = (Vec<Step<'p>>, Vec<(&'p str, Span)>, Vec<PlannedRewrite<'p>>);

/// What a query searches for: text, or a structural pattern.
#[derive(Clone, Copy)]
pub(super) enum Wanted<'p> {
    /// A textual pattern, matched in every text file.
    Text(&'p TextPattern),
    /// A structural pattern, matched in every file whose language reads it: as written, and
    /// where its compiled forms are in the run's list of them.
    Structural { written: &'p str, compiled: usize },
}

/// One assertion in a block this build can run.
pub(super) struct Assertion<'p> {
    pub(super) pattern: Wanted<'p>,
    pub(super) count: Count,
    pub(super) span: Span,
    pub(super) query: &'p Query,
}

/// One pattern the run searches for, and the query it is from.
#[derive(Clone, Copy)]
pub(super) struct Searching<'p> {
    pub(super) pattern: Wanted<'p>,
    pub(super) query: &'p Query,
}

impl<'p> Step<'p> {
    /// The step's patterns in order.  One list serves the search and the notes, so they cannot
    /// disagree about which pattern is which.
    pub(super) fn patterns(&self) -> Vec<Searching<'p>> {
        match self {
            Step::Find(pattern, query) => vec![Searching {
                pattern: *pattern,
                query,
            }],
            Step::Assert(block) => block
                .iter()
                .map(|a| Searching {
                    pattern: a.pattern,
                    query: a.query,
                })
                .collect(),
            Step::Replace(replacing) => vec![Searching {
                pattern: replacing.wanted(),
                query: replacing.query,
            }],
        }
    }

    /// The replacement each of the step's patterns is replaced with, in the order
    /// [`Step::patterns`] lists them: `None` for a pattern only searched for.
    pub(super) fn replacements(&self) -> Vec<Option<Replacement<'p>>> {
        match self {
            Step::Find(..) => vec![None],
            Step::Assert(block) => block.iter().map(|_| None).collect(),
            Step::Replace(replacing) => vec![Some(match replacing.kind {
                ReplaceKind::Text { with, .. } => Replacement::Text(with),
                ReplaceKind::Structural { rewrite, .. } => Replacement::Structural(rewrite),
            })],
        }
    }
}

/// The steps `program` runs as, and each structural pattern in it in order with where it was
/// written; or the first construct in it that is in the MVP but not built yet, with where it was
/// written.
fn steps(program: &Program) -> Result<Planned<'_>, (&'static str, Span)> {
    let mut steps = Vec::new();
    let mut structural = Vec::new();
    let mut rewrites = Vec::new();
    for statement in program.body() {
        steps.push(match &statement.node {
            Statement::Find(query) => Step::Find(buildable(query, &mut structural)?, query),
            Statement::Assert(block) => Step::Assert(
                block
                    .iter()
                    .map(|a| assertion(a, &mut structural))
                    .collect::<Result<_, _>>()?,
            ),
            Statement::Act(action) => Step::Replace(replacing(
                action,
                statement.span,
                &mut structural,
                &mut rewrites,
            )?),
            _ => return Err(("this statement", statement.span)),
        });
    }
    Ok((steps, structural, rewrites))
}

/// The replace `action` is, if this build can run it.  A structural one's pattern joins
/// `structural` and its template `rewrites`, to be compiled before any file is read.
pub(super) fn replacing<'p>(
    action: &'p Action,
    span: Span,
    structural: &mut Vec<(&'p str, Span)>,
    rewrites: &mut Vec<PlannedRewrite<'p>>,
) -> Result<Replacing<'p>, (&'static str, Span)> {
    // `delete X` is `replace X with ``: a rewrite with no code, whose matches go with their
    // separator or their line.  It has no behaviour of its own.
    let (with, with_span) = match &action.kind {
        ActionKind::Replace(with) => (&with.node, with.span),
        ActionKind::Delete => (&NOTHING, span),
        _ => return Err(("this kind of edit", span)),
    };
    let kind = match (&action.query.operand.node, with) {
        (Operand::Text(pattern), Operand::Text(with)) => ReplaceKind::Text { pattern, with },
        (Operand::Pattern(_), Operand::Pattern(written)) => {
            let pattern = buildable(&action.query, structural)?;
            let Wanted::Structural { compiled, .. } = pattern else {
                unreachable!("a backticked operand is a structural pattern");
            };
            rewrites.push((written.as_str(), with_span, compiled));
            ReplaceKind::Structural {
                pattern,
                rewrite: rewrites.len() - 1,
            }
        }
        _ => return Err(("this kind of replace", span)),
    };
    // The checker has refused an edit with no `expect`, and the parser takes at most one.
    let Some(count) = action.expect.first() else {
        return Err(("a replace with no `expect`", span));
    };
    Ok(Replacing {
        kind,
        count: count.node,
        span,
        query: &action.query,
        deletes: false,
    })
}

pub(super) fn assertion<'p>(
    expectation: &'p Spanned<Expectation>,
    structural: &mut Vec<(&'p str, Span)>,
) -> Result<Assertion<'p>, (&'static str, Span)> {
    let Counted::Matches(query) = &expectation.node.counted else {
        return Err(("this assertion", expectation.span));
    };
    Ok(Assertion {
        pattern: buildable(query, structural)?,
        count: expectation.node.count.node,
        span: expectation.span,
        query,
    })
}

/// The pattern of `query`, if this build can search for it.  A structural one joins
/// `structural`, to be compiled with the rest before any file is read.
pub(super) fn buildable<'p>(
    query: &'p Query,
    structural: &mut Vec<(&'p str, Span)>,
) -> Result<Wanted<'p>, (&'static str, Span)> {
    match &query.operand.node {
        Operand::Text(pattern) => Ok(Wanted::Text(pattern)),
        Operand::Pattern(written) => {
            structural.push((written.as_str(), query.operand.span));
            Ok(Wanted::Structural {
                written,
                compiled: structural.len() - 1,
            })
        }
        _ => Err(("this kind of operand", query.operand.span)),
    }
}
