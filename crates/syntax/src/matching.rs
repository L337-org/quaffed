// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Structural matching: where a pattern's shape occurs in a parsed file.
//!
//! Two nodes match when they are of the same kind and their parts match in order, a part being
//! a child node or a token the adapter kept between the children.  Comments, whitespace, line
//! breaks, indentation and redundant parentheses are not tokens the adapter keeps, so they take
//! no part.  A node with a value - a string literal's content - matches by that value alone.  A
//! hole matches any node, a sequence hole one or more consecutive children of one node with
//! whatever tokens stand between them, and a name used twice must match equal code both times.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::{Arena, Hole, NodeId, Pattern, Token};

/// One place a pattern matched: the range it covers in the text the arena was built over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The matched code's byte range.
    pub range: Range<usize>,
}

/// Every place `pattern` matches in `target`, in source order: by where each starts, then by
/// where it ends.  A match inside another match is a match of its own.
///
/// A pattern of one node matches any node of that shape.  A pattern of several - statements in
/// a row - matches them standing consecutively among one node's children.
///
/// # Panics
///
/// If the pattern and the arena are of different languages, which is a defect in the caller.
#[must_use]
pub fn find(pattern: &Pattern, target: &Arena<'_>) -> Vec<Match> {
    let shape = pattern.arena();
    assert_eq!(
        shape.language(),
        target.language(),
        "a pattern was matched against another language's arena"
    );
    // A pattern compiler refuses a pattern with no code, or with nothing but holes, so a pattern
    // of one node is one with a shape.
    let content: Vec<NodeId> = shape.children(shape.root()).collect();
    let mut found = Vec::new();
    if let [only] = content.as_slice() {
        for node in target.nodes() {
            let mut bindings = Bindings::new();
            if Matcher::new(pattern, target).node(*only, node, &mut bindings) {
                found.push(Match {
                    range: target.range(node),
                });
            }
        }
    } else {
        let wanted = between_first_and_last(&parts(shape, shape.root()));
        for node in target.nodes() {
            let available = parts(target, node);
            for (start, first) in available.iter().enumerate() {
                let Part::Child(first) = first else { continue };
                // The shortest run from each start that matches, so that a run is reported once.
                for (end, last) in available.iter().enumerate().skip(start) {
                    let Part::Child(last) = last else { continue };
                    let mut bindings = Bindings::new();
                    if Matcher::new(pattern, target).parts(
                        &wanted,
                        &available[start..=end],
                        &mut bindings,
                    ) {
                        found.push(Match {
                            range: target.range(*first).start..target.range(*last).end,
                        });
                        break;
                    }
                }
            }
        }
    }
    found.sort_by_key(|m| (m.range.start, m.range.end));
    found.dedup();
    found
}

/// A node's parts, in source order: its children and the tokens the adapter kept between them.
#[derive(Debug, Clone, Copy)]
enum Part<'a> {
    Child(NodeId),
    Token(&'a Token),
}

fn parts<'a>(arena: &'a Arena<'_>, node: NodeId) -> Vec<Part<'a>> {
    let mut out = Vec::new();
    let mut tokens = arena.tokens_in(arena.range(node)).iter().peekable();
    for child in arena.children(node) {
        let range = arena.range(child);
        while let Some(token) = tokens.next_if(|t| arena.token_range(t).end <= range.start) {
            out.push(Part::Token(token));
        }
        // Tokens inside the child are the child's own.
        while tokens
            .next_if(|t| arena.token_range(t).start < range.end)
            .is_some()
        {}
        out.push(Part::Child(child));
    }
    out.extend(tokens.map(Part::Token));
    out
}

/// The parts from the first child to the last, without the tokens around them: what a pattern
/// of several statements asks of the run it matches.  Such a pattern has children, since a
/// pattern compiler refuses one with no code.
fn between_first_and_last<'a>(all: &[Part<'a>]) -> Vec<Part<'a>> {
    let is_child = |p: &Part<'_>| matches!(p, Part::Child(_));
    let first = all
        .iter()
        .position(is_child)
        .expect("a pattern with code has children");
    let last = all
        .iter()
        .rposition(is_child)
        .expect("a pattern with code has children");
    all[first..=last].to_vec()
}

/// What a named hole matched, as the parts it covers: one child for a hole, a run of a node's
/// parts for a sequence.  A name is one or the other throughout a pattern, which its compiler
/// checks, so two bindings of a name are always of the same form.
type Bound<'a> = Vec<Part<'a>>;

type Bindings<'a> = BTreeMap<String, Bound<'a>>;

/// A pattern and the arena it is matched against.
struct Matcher<'p, 't> {
    shape: &'p Arena<'static>,
    pattern: &'p Pattern,
    target: &'t Arena<'t>,
}

impl<'p, 't> Matcher<'p, 't> {
    fn new(pattern: &'p Pattern, target: &'t Arena<'_>) -> Matcher<'p, 't> {
        Matcher {
            shape: pattern.arena(),
            pattern,
            target,
        }
    }

    fn hole(&self, node: NodeId) -> Option<&'p Hole> {
        self.pattern.hole(node)
    }

    fn node(&self, wanted: NodeId, node: NodeId, bindings: &mut Bindings<'t>) -> bool {
        if let Some(hole) = self.hole(wanted) {
            return bind(self.target, hole, vec![Part::Child(node)], bindings);
        }
        if self.shape.kind(wanted) != self.target.kind(node) {
            return false;
        }
        // A value belongs to a kind, so two nodes of one kind both have one or neither does.
        if let Some(value) = self.shape.value(wanted) {
            return self.target.value(node) == Some(value);
        }
        let wanted_parts = parts(self.shape, wanted);
        let node_parts = parts(self.target, node);
        self.parts(&wanted_parts, &node_parts, bindings)
    }

    fn parts(
        &self,
        wanted: &[Part<'_>],
        available: &[Part<'t>],
        bindings: &mut Bindings<'t>,
    ) -> bool {
        let Some((first, rest)) = wanted.split_first() else {
            return available.is_empty();
        };
        match first {
            Part::Token(token) => match available.split_first() {
                Some((Part::Token(other), others)) => {
                    self.shape.token_text(token) == self.target.token_text(other)
                        && self.parts(rest, others, bindings)
                }
                _ => false,
            },
            Part::Child(child) => match self.hole(*child) {
                Some(hole) if hole.sequence => {
                    // One or more of the available parts, from a child to a child, shortest
                    // first; whichever split matches first is kept.
                    if !matches!(available.first(), Some(Part::Child(_))) {
                        return false;
                    }
                    for end in 0..available.len() {
                        if !matches!(available[end], Part::Child(_)) {
                            continue;
                        }
                        let mut attempt = bindings.clone();
                        let run = available[..=end].to_vec();
                        if bind(self.target, hole, run, &mut attempt)
                            && self.parts(rest, &available[end + 1..], &mut attempt)
                        {
                            *bindings = attempt;
                            return true;
                        }
                    }
                    false
                }
                _ => match available.split_first() {
                    Some((Part::Child(node), others)) => {
                        let mut attempt = bindings.clone();
                        if self.node(*child, *node, &mut attempt)
                            && self.parts(rest, others, &mut attempt)
                        {
                            *bindings = attempt;
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                },
            },
        }
    }
}

/// Binds `bound` to `hole`'s name, or checks it equals what the name already holds.  A hole
/// with no name binds nothing and matches anything.
fn bind<'t>(
    target: &'t Arena<'t>,
    hole: &Hole,
    bound: Bound<'t>,
    bindings: &mut Bindings<'t>,
) -> bool {
    let Some(name) = &hole.name else {
        return true;
    };
    match bindings.get(name) {
        None => {
            bindings.insert(name.clone(), bound);
            true
        }
        Some(earlier) => equal(target, earlier, &bound),
    }
}

/// Whether two places in one arena hold equal code, by the same comparison as a match.
fn equal(target: &Arena<'_>, a: &Bound<'_>, b: &Bound<'_>) -> bool {
    Same { arena: target }.parts(a, b)
}

/// The comparison with no holes, both sides in one arena.
struct Same<'a, 't> {
    arena: &'a Arena<'t>,
}

impl Same<'_, '_> {
    fn node(&self, a: NodeId, b: NodeId) -> bool {
        if self.arena.kind(a) != self.arena.kind(b) {
            return false;
        }
        if let Some(value) = self.arena.value(a) {
            return self.arena.value(b) == Some(value);
        }
        self.parts(&parts(self.arena, a), &parts(self.arena, b))
    }

    fn parts(&self, a: &[Part<'_>], b: &[Part<'_>]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|pair| match pair {
                (Part::Token(x), Part::Token(y)) => {
                    self.arena.token_text(x) == self.arena.token_text(y)
                }
                (Part::Child(x), Part::Child(y)) => self.node(*x, *y),
                _ => false,
            })
    }
}
