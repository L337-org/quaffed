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

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

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
/// If the pattern and the arena are of different languages, which is a defect in the caller, or
/// the pattern has a sequence hole at the start or end of a run of statements
/// ([`Pattern::unbounded_sequence`]) or a run too costly to search ([`Pattern::costly`]), which
/// is a defect in its compiler.
#[must_use]
pub fn find(pattern: &Pattern, target: &Arena<'_>) -> Vec<Match> {
    assert!(
        pattern.unbounded_sequence().is_none(),
        "a pattern compiler let through a sequence hole at the edge of a run of statements"
    );
    assert!(
        pattern.costly().is_none(),
        "a pattern compiler let through a run of statements too costly to search"
    );
    find_with(pattern, target, true)
}

/// [`find`], matching a run of statements by [`Run`]'s index where `indexed` and it can, and
/// otherwise by backtracking alone: the slow way, which the fast one is held to.
fn find_with(pattern: &Pattern, target: &Arena<'_>, indexed: bool) -> Vec<Match> {
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
        let run = Run::new(pattern, &wanted).filter(|_| indexed);
        for node in target.nodes() {
            let available = parts(target, node);
            let in_a_list =
                |part: &Part<'_>| matches!(part, Part::Child(c) if target.list(*c).is_some());
            if !available.iter().any(in_a_list) {
                continue;
            }
            // Jumping to a segment's next place relies on a block's statements standing with
            // nothing between them, as they do in an arena that keeps no separator tokens.
            let separated = separated(target, &available);
            match &run {
                Some(run) if !separated => run.find_in(pattern, target, &available, &mut found),
                _ => backtrack(pattern, target, &wanted, &available, &mut found),
            }
        }
    }
    found.sort_by_key(|m| (m.range.start, m.range.end));
    found.dedup();
    found
}

/// Every match of a run of statements, `wanted`, among a node's parts, `available`, by trying it
/// from each start and backtracking through each sequence's every length: what [`Run`] finds
/// faster, and the way for a pattern it cannot index.
fn backtrack<'t>(
    pattern: &Pattern,
    target: &'t Arena<'t>,
    wanted: &[Part<'_>],
    available: &[Part<'t>],
    found: &mut Vec<Match>,
) {
    let limits = list_ends(target, available);
    for (start, first) in available.iter().enumerate() {
        let Part::Child(first) = first else { continue };
        // A run of statements stays in one list - one block's statements - so it never
        // takes in a header, a clause, or another block.
        if target.list(*first).is_none() {
            continue;
        }
        let limit = limits[start];
        // Matched from this start as far as the pattern goes: one try per start, whatever
        // the run's length.
        let mut bindings = Bindings::new();
        if let Some(taken) = Matcher::new(pattern, target).consume(
            wanted,
            &available[start..limit],
            &mut bindings,
            false,
        ) {
            let Part::Child(end) = available[start + taken - 1] else {
                unreachable!("a run of statements ends at a statement");
            };
            found.push(Match {
                range: target.range(*first).start..target.range(end).end,
            });
        }
    }
}

/// A run of statements split at its sequence holes into **segments**, the fixed code between
/// them: `w()`, `$a...`, `x()`, `$b...`, `y()` is the segments `w()`, `x()` and `y()` with `$a`
/// and `$b` between.  An edge is never a sequence, so there is a segment at each end.
///
/// Rather than trying every length of every sequence from every start, it finds once per block
/// where each segment's shape stands, then matches from each start by jumping to the places after
/// the run so far where the next segment stands, nearest first: a sequence takes the fewest
/// statements that let the rest match.  So it finds the matches backtracking would, in the same
/// order of preference.
///
/// Where the rest of the run fails after a segment's nearest place, it fails after every later
/// place too - whatever stands after a later place also stands after this one - unless the
/// choice of place changes what the rest is matched with.  It does when the step binds a name
/// used further on, or when the sequence after the segment has a name used elsewhere, since that
/// sequence then holds different code from a later start.  Only there are the later places
/// tried, so a pattern without such names costs a binary search per segment per start.
struct Run<'w> {
    segments: Vec<Vec<Part<'w>>>,
    /// The sequence hole before each segment after the first.
    holes: Vec<NodeId>,
    /// For each segment, whether a failure after its nearest place is a failure after every
    /// later place, so that no later place need be tried.
    settled: Vec<bool>,
}

/// A run of statements split at its sequences, with each step's choice of place: whether it
/// decides what later code must match, and through which hole.
struct Steps<'w> {
    segments: Vec<Vec<Part<'w>>>,
    holes: Vec<NodeId>,
    /// For each segment, the hole through which its choice of place decides what later code
    /// must match, if it does: a name first bound in placing it and used further on, or the
    /// sequence after it, which a later place gives other code, when that sequence's name is
    /// used elsewhere.
    choices: Vec<Option<NodeId>>,
}

/// Why a run of statements cannot be split into steps.
enum Unsplit {
    /// Two sequences stand together, the second of them named: nothing between them to look for.
    Together(NodeId),
    /// A segment starts or ends with a token.
    Tokens,
}

impl<'w> Steps<'w> {
    fn of(pattern: &Pattern, wanted: &[Part<'w>]) -> Result<Steps<'w>, Unsplit> {
        let shape = pattern.arena();
        let mut segments = vec![Vec::new()];
        let mut holes = Vec::new();
        for part in wanted {
            match part {
                Part::Child(child) if pattern.hole(*child).is_some_and(|hole| hole.sequence) => {
                    if segments.last().is_some_and(Vec::is_empty) && !holes.is_empty() {
                        return Err(Unsplit::Together(*child));
                    }
                    holes.push(*child);
                    segments.push(Vec::new());
                }
                _ => segments.last_mut().expect("begun with one").push(*part),
            }
        }
        // A segment from a statement to a statement, so that the sequences between them are
        // all that lies between: with a token at an edge, what a sequence may take would depend
        // on the tokens beside it.
        let bounded_by_statements = |segment: &Vec<Part<'_>>| {
            matches!(segment.first(), Some(Part::Child(_)))
                && matches!(segment.last(), Some(Part::Child(_)))
        };
        if !segments.iter().all(bounded_by_statements) {
            return Err(Unsplit::Tokens);
        }
        // Each hole belongs to one part of the run, numbered in order - segment 0, the sequence
        // before segment 1, segment 1, and so on: a sequence is a part of its own, and any other
        // hole lies inside one of a segment's statements.
        let part_of = |node: NodeId| -> usize {
            if let Some(at) = holes.iter().position(|hole| *hole == node) {
                return 2 * at + 1;
            }
            let range = shape.range(node);
            let segment = segments
                .iter()
                .position(|segment| {
                    segment.iter().any(|part| {
                        matches!(part, Part::Child(child)
                            if shape.range(*child).start <= range.start
                                && range.end <= shape.range(*child).end)
                    })
                })
                .expect("every hole of a run lies in one of its parts");
            2 * segment
        };
        // Each name's first and last part, and the hole it is first bound by: holes come in
        // pre-order, which is the order of the parts, so the first seen is the first.
        let mut uses: BTreeMap<&str, (usize, usize, NodeId)> = BTreeMap::new();
        for (node, hole) in pattern.holes() {
            if let Some(name) = &hole.name {
                let at = part_of(node);
                uses.entry(name).or_insert((at, at, node)).1 = at;
            }
        }
        let choices = (0..segments.len())
            .map(|i| {
                // The sequence after segment `i` starts where it ends, so a name it carries that
                // is used elsewhere takes other code from a later place.
                let next = holes.get(i).copied().filter(|next| {
                    pattern
                        .hole(*next)
                        .and_then(|hole| hole.name.as_deref())
                        .is_some_and(|name| (uses[name].0, uses[name].1) != (2 * i + 1, 2 * i + 1))
                });
                // Placing segment `i` binds what the sequence before it and the segment itself
                // name first; a name of those used further on takes what this place gives it.
                let bound_here = uses
                    .values()
                    .find(|&&(first, last, _)| {
                        (2 * i).saturating_sub(1) <= first && first <= 2 * i && last > 2 * i
                    })
                    .map(|&(_, _, node)| node);
                // Segment 0 is placed by the start itself, which is tried everywhere anyway.
                if i == 0 { None } else { next.or(bound_here) }
            })
            .collect();
        Ok(Steps {
            segments,
            holes,
            choices,
        })
    }
}

/// Why a pattern's run of statements would make a search cost more than the square of a block's
/// length, so that a compiler refuses it: [`Pattern::costly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Costly {
    /// Two sequences stand together with no statement between them, so nothing settles how the
    /// statements divide between them, and every division is tried: the second of them.
    Together(NodeId),
    /// A second step in the run whose choice of place decides what later code must match, each
    /// such step multiplying a search's work by the block's length: the hole that makes it one.
    SecondChoice(NodeId),
}

impl Pattern {
    /// Whether matching this pattern's run of statements could cost more than the square of a
    /// block's length, and why.  A run where each sequence's place is settled by the statements
    /// around it costs about the block's length, and one step whose choice decides what later
    /// code must match - a sequence named twice, say - makes it quadratic; a second such step
    /// makes it cubic, and so on.  Every language's compiler refuses a pattern this names, and
    /// [`find`] panics on one, as a defect in its compiler.
    #[must_use]
    pub fn costly(&self) -> Option<Costly> {
        let shape = self.arena();
        // A pattern of one node is matched node by node, not as a run.
        shape.children(shape.root()).nth(1)?;
        let wanted = between_first_and_last(&parts(shape, shape.root()));
        match Steps::of(self, &wanted) {
            Err(Unsplit::Together(hole)) => Some(Costly::Together(hole)),
            // An adapter whose statements carry tokens between them is matched by backtracking;
            // none does yet.
            Err(Unsplit::Tokens) => None,
            Ok(steps) => steps
                .choices
                .iter()
                .flatten()
                .nth(1)
                .copied()
                .map(Costly::SecondChoice),
        }
    }
}

impl<'w> Run<'w> {
    /// The run `wanted` as segments, or `None` if two sequences stand together with no segment
    /// between them to look for, or a segment starts or ends with a token, which only
    /// backtracking can match.
    fn new(pattern: &Pattern, wanted: &[Part<'w>]) -> Option<Run<'w>> {
        let steps = Steps::of(pattern, wanted).ok()?;
        Some(Run {
            segments: steps.segments,
            holes: steps.holes,
            settled: steps.choices.iter().map(Option::is_none).collect(),
        })
    }

    /// Every match of the run among a node's parts, `available`, added to `found`.
    fn find_in<'t>(
        &self,
        pattern: &Pattern,
        target: &'t Arena<'t>,
        available: &[Part<'t>],
        found: &mut Vec<Match>,
    ) {
        let matcher = Matcher::new(pattern, target);
        let limits = list_ends(target, available);
        // Where each segment's shape stands, by the bindings it makes itself: a segment holds no
        // sequence, so where it matches it takes exactly its own length.  A segment starts with a
        // statement, so it stands only at a statement in a block.
        let index: Vec<Vec<usize>> = self
            .segments
            .iter()
            .map(|segment| {
                (0..available.len())
                    .filter(|&at| {
                        matches!(available[at], Part::Child(c) if target.list(c).is_some())
                            && matcher
                                .consume(
                                    segment,
                                    &available[at..limits[at]],
                                    &mut Bindings::new(),
                                    false,
                                )
                                .is_some()
                    })
                    .collect()
            })
            .collect();
        // One copy of the parts, which every run a named sequence binds is a range of.
        let shared: Rc<[Part<'t>]> = available.into();
        let search = Search {
            run: self,
            matcher: &matcher,
            available,
            index: &index,
            shared: &shared,
            codes: OnceCell::new(),
        };
        for &start in &index[0] {
            let limit = limits[start];
            // The first segment's bindings start from nothing, as the index found them.
            let mut bindings = Bindings::new();
            let taken = matcher
                .consume(
                    &self.segments[0],
                    &available[start..limit],
                    &mut bindings,
                    false,
                )
                .expect("the index found the first segment here with no bindings");
            if let Some(end) = search.extend(1, start + taken, limit, &bindings) {
                found.push(Match {
                    range: target.range(statement(available, start)).start
                        ..target.range(statement(available, end - 1)).end,
                });
            }
        }
    }
}

/// One node's parts, and where each segment of a run stands among them.
struct Search<'s, 'w, 'p, 't> {
    run: &'s Run<'w>,
    matcher: &'s Matcher<'p, 't>,
    available: &'s [Part<'t>],
    index: &'s [Vec<usize>],
    shared: &'s Rc<[Part<'t>]>,
    /// The parts' code hashed, made when a name is first used twice.
    codes: OnceCell<Codes>,
}

impl<'t> Search<'_, '_, '_, 't> {
    /// Where a run from `at` holding the same code as `earlier` ends, within the block that ends
    /// at `limit`: the one place, since every part adds code; `None` if there is none, or it
    /// does not start and end at a statement.
    fn equal_run(&self, earlier: &Bound<'_>, at: usize, limit: usize) -> Option<usize> {
        let Bound::Run(parts, range) = earlier else {
            unreachable!("a compiler refuses one name for a node and a sequence");
        };
        let target = self.matcher.target;
        let codes = self
            .codes
            .get_or_init(|| Codes::new(target, self.available));
        // A run this search bound is a range of these same parts, whose code is hashed already.
        let (length, hash) = if Rc::ptr_eq(parts, self.shared) {
            (
                codes.lengths[range.end] - codes.lengths[range.start],
                codes.hash(range.clone()),
            )
        } else {
            length_and_hash(target, &parts[range.clone()])
        };
        let end = codes
            .lengths
            .binary_search(&(codes.lengths[at] + length))
            .ok()?;
        // The run starts at a statement, `at` being one; it must end at one too, since a block
        // runs up to the next and can end with tokens, such as an `else:` before its clause.
        if end > limit
            || !matches!(self.available[end - 1], Part::Child(_))
            || codes.hash(at..end) != hash
        {
            return None;
        }
        Same { arena: target }
            .parts(&parts[range.clone()], &self.available[at..end])
            .then_some(end)
    }

    /// Matches segment `i` onwards, the run so far ending at `at` and its block at `limit`, and
    /// says where the match ends.
    fn extend(&self, i: usize, at: usize, limit: usize, bindings: &Bindings<'t>) -> Option<usize> {
        let Some(segment) = self.run.segments.get(i) else {
            return Some(at);
        };
        let hole = self
            .matcher
            .hole(self.run.holes[i - 1])
            .expect("a run's sequences are holes");
        let places = &self.index[i];
        // Every segment from here on must still stand somewhere after `at`, or no choice of
        // places here can complete the run.
        if self.index[i..]
            .iter()
            .any(|later| later.last().is_none_or(|&last| last <= at))
        {
            return None;
        }
        // A name the run has already bound must take equal code again, which leaves it one end:
        // where the code from `at` first equals the code it bound.  The segment stands there or
        // nowhere.
        if let Some(name) = &hole.name
            && let Some(earlier) = bindings.get(name)
        {
            let place = self.equal_run(earlier, at, limit)?;
            places.binary_search(&place).ok()?;
            let mut attempt = bindings.clone();
            let taken = self.matcher.consume(
                segment,
                &self.available[place..limit],
                &mut attempt,
                false,
            )?;
            return self.extend(i + 1, place + taken, limit, &attempt);
        }
        // The sequence before this segment takes at least one part, so the segment stands after
        // `at`; and the first place it can stand is where the fewest are taken.
        let from = places.partition_point(|&place| place <= at);
        for &place in &places[from..] {
            if place >= limit {
                break;
            }
            // A run is from a statement to a statement: the index holds only statements, and a
            // block searched this way has nothing between its statements.  A name bound here is
            // bound for the first time, a name bound before having been dealt with above, so
            // binding it always succeeds.
            let mut attempt = bindings.clone();
            let bound = bind(
                self.matcher.target,
                hole,
                Bound::Run(Rc::clone(self.shared), at..place),
                &mut attempt,
            );
            debug_assert!(bound, "a sequence's first binding always holds");
            // A segment that does not match here with the bindings so far may match at a later
            // place, so only a failure further on can settle it.
            let Some(taken) =
                self.matcher
                    .consume(segment, &self.available[place..limit], &mut attempt, false)
            else {
                continue;
            };
            if let Some(end) = self.extend(i + 1, place + taken, limit, &attempt) {
                return Some(end);
            }
            if self.run.settled[i] {
                return None;
            }
        }
        None
    }
}

/// Whether a token stands between two statements of one block among a node's parts, as a
/// separator would.  Tokens before a block's first statement or after its last - a `:`, an
/// `else` - stand between blocks, not within one.
fn separated(arena: &Arena<'_>, available: &[Part<'_>]) -> bool {
    let mut previous: Option<u32> = None;
    let mut token_since = false;
    for part in available {
        match part {
            Part::Token(_) => token_since = true,
            Part::Child(child) => {
                let list = arena.list(*child);
                if token_since && list.is_some() && list == previous {
                    return true;
                }
                previous = list;
                token_since = false;
            }
        }
    }
    false
}

/// The statement at `at` among a node's parts, where a run's index has put a statement.
fn statement(available: &[Part<'_>], at: usize) -> NodeId {
    match available[at] {
        Part::Child(child) => child,
        Part::Token(_) => unreachable!("a run of statements starts and ends at a statement"),
    }
}

/// For each of a node's parts, where the list its child belongs to ends: the index of the first
/// child of another list after it, or the end.  Found in one pass from the back, so that each
/// start of a run need not look ahead for it.
fn list_ends(arena: &Arena<'_>, available: &[Part<'_>]) -> Vec<usize> {
    let mut ends = vec![available.len(); available.len()];
    let mut end = available.len();
    let mut later: Option<(usize, Option<u32>)> = None;
    for (i, part) in available.iter().enumerate().rev() {
        if let Part::Child(child) = part {
            let list = arena.list(*child);
            if let Some((index, later_list)) = later
                && later_list != list
            {
                end = index;
            }
            later = Some((i, list));
        }
        ends[i] = end;
    }
    ends
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

/// What a named hole matched: one child for a hole, a run of a node's parts for a sequence.  A
/// name is one or the other throughout a pattern, which its compiler checks, so two bindings of
/// a name are always of the same form.  A run is a range of a copy of the parts it was cut from:
/// its own, for a sequence's first try, and then one copy shared by every longer try, so trying a
/// longer one costs nothing to bind.
#[derive(Debug, Clone)]
enum Bound<'a> {
    Node(NodeId),
    Run(Rc<[Part<'a>]>, Range<usize>),
}

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
            return bind(self.target, hole, Bound::Node(node), bindings);
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
        // A node with nothing to compare inside it - no children, no tokens, no value - compares
        // by its text, so that an adapter that kept no token for a leaf cannot make every leaf
        // of a kind alike.
        if wanted_parts.is_empty() && node_parts.is_empty() {
            return self.shape.source(wanted) == self.target.source(node);
        }
        self.parts(&wanted_parts, &node_parts, bindings)
    }

    fn parts(
        &self,
        wanted: &[Part<'_>],
        available: &[Part<'t>],
        bindings: &mut Bindings<'t>,
    ) -> bool {
        self.consume(wanted, available, bindings, true).is_some()
    }

    /// Matches `wanted` against the start of `available`, and says how many of its parts the
    /// match took, each sequence hole the shortest way that lets the rest match; with `whole`,
    /// only a match that takes them all counts.
    fn consume(
        &self,
        wanted: &[Part<'_>],
        available: &[Part<'t>],
        bindings: &mut Bindings<'t>,
        whole: bool,
    ) -> Option<usize> {
        let Some((first, rest)) = wanted.split_first() else {
            return (!whole || available.is_empty()).then_some(0);
        };
        match first {
            Part::Token(token) => match available.split_first() {
                Some((Part::Token(other), others))
                    if self.shape.token_text(token) == self.target.token_text(other) =>
                {
                    self.consume(rest, others, bindings, whole).map(|n| n + 1)
                }
                _ => None,
            },
            Part::Child(child) => match self.hole(*child) {
                Some(hole) if hole.sequence => {
                    // One or more of the available parts, from a child to a child, the shortest
                    // run first: the first that lets the rest match is kept.
                    let Some(Part::Child(first)) = available.first() else {
                        return None;
                    };
                    // A run stays in one list, so `$body...` never takes in a clause or a header.
                    let block = self.target.list(*first);
                    let in_block = |part: &Part<'_>| !matches!(part, Part::Child(c) if self.target.list(*c) != block);
                    // A named hole binds a run.  The first try - usually the one that matches -
                    // copies only its own run; once one has failed, every longer run is a prefix
                    // of one copy of the list's parts, so a try binds without copying, and a run
                    // of n children costs n tries rather than n copies of up to n parts each.
                    // `$...` binds nothing, so it copies nothing.
                    let mut shared: Option<Rc<[Part<'t>]>> = None;
                    let mut tried = false;
                    // Walked forward, stopping at the list's end as it reaches it, so a first try
                    // that matches looks no further.
                    for end in 0..available.len() {
                        if !in_block(&available[end]) {
                            break;
                        }
                        if !matches!(available[end], Part::Child(_)) {
                            continue;
                        }
                        let mut attempt = bindings.clone();
                        let bound = if hole.name.is_none() {
                            true
                        } else {
                            let run = if tried {
                                let shared = shared.get_or_insert_with(|| {
                                    let within = available
                                        .iter()
                                        .position(|part| !in_block(part))
                                        .unwrap_or(available.len());
                                    available[..within].into()
                                });
                                Bound::Run(Rc::clone(shared), 0..end + 1)
                            } else {
                                Bound::Run(available[..=end].into(), 0..end + 1)
                            };
                            tried = true;
                            bind(self.target, hole, run, &mut attempt)
                        };
                        if bound
                            && let Some(n) =
                                self.consume(rest, &available[end + 1..], &mut attempt, whole)
                        {
                            *bindings = attempt;
                            return Some(end + 1 + n);
                        }
                    }
                    None
                }
                _ => match available.split_first() {
                    Some((Part::Child(node), others)) => {
                        let mut attempt = bindings.clone();
                        if self.node(*child, *node, &mut attempt)
                            && let Some(n) = self.consume(rest, others, &mut attempt, whole)
                        {
                            *bindings = attempt;
                            Some(n + 1)
                        } else {
                            None
                        }
                    }
                    _ => None,
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

/// Whether two places in one arena hold equal code: the same tokens, values and texts in the
/// same order, whatever kinds of node hold them.
fn equal(target: &Arena<'_>, a: &Bound<'_>, b: &Bound<'_>) -> bool {
    fn as_parts<'b>(bound: &Bound<'b>) -> Vec<Part<'b>> {
        match bound {
            Bound::Node(node) => vec![Part::Child(*node)],
            Bound::Run(parts, range) => parts[range.clone()].to_vec(),
        }
    }
    Same { arena: target }.parts(&as_parts(a), &as_parts(b))
}

/// The code of a place in an arena, as a match compares it, with no kinds in it: each kept
/// token's text, each value, and the text of a node with nothing inside it, in source order.
/// Two places with the same code are equal whatever role they stand in, so a name used twice
/// matches `a` as an argument, a keyword, an attribute and a parameter alike.
#[derive(Debug, PartialEq, Eq, Hash)]
enum Code<'a> {
    Token(&'a str),
    Value(&'a str),
    Text(&'a str),
}

/// The code of a node's parts, hashed once, so that whether a run holds the same code as a run
/// a name bound costs a binary search and a hash rather than a comparison of every part.
///
/// The hashes are polynomial over each piece of code's own hash, wrapping at 64 bits, so two runs
/// with equal code have equal hashes.  Two with different code can collide, so a run whose hash
/// matches is still compared in full: a collision costs a comparison, never a wrong match.
struct Codes {
    /// How many pieces of code the parts before each index hold.
    lengths: Vec<usize>,
    /// The hash of the code of the parts before each index.
    hashes: Vec<u64>,
    /// `BASE` to each power up to the total number of pieces.
    powers: Vec<u64>,
}

/// An odd multiplier, so that every power of it is odd and none is zero.
const BASE: u64 = 0x9E37_79B9_7F4A_7C15;

impl Codes {
    fn new(arena: &Arena<'_>, available: &[Part<'_>]) -> Codes {
        let same = Same { arena };
        let mut codes = Codes {
            lengths: vec![0],
            hashes: vec![0],
            powers: vec![1],
        };
        let (mut length, mut hash) = (0, 0u64);
        let mut code = Vec::new();
        for part in available {
            code.clear();
            same.code(std::slice::from_ref(part), &mut code);
            for piece in &code {
                hash = hash.wrapping_mul(BASE).wrapping_add(piece_hash(piece));
                let last = *codes.powers.last().expect("begun with one");
                codes.powers.push(last.wrapping_mul(BASE));
                length += 1;
            }
            codes.lengths.push(length);
            codes.hashes.push(hash);
        }
        codes
    }

    /// The hash of the code of the parts `range` indexes.
    fn hash(&self, range: Range<usize>) -> u64 {
        let pieces = self.lengths[range.end] - self.lengths[range.start];
        self.hashes[range.end]
            .wrapping_sub(self.hashes[range.start].wrapping_mul(self.powers[pieces]))
    }
}

/// One piece of code's hash, by the standard library's hasher with its fixed keys.
fn piece_hash(piece: &Code<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    piece.hash(&mut hasher);
    hasher.finish()
}

/// The code `earlier` holds, as a number of pieces and their hash, as [`Codes`] counts them.
fn length_and_hash(arena: &Arena<'_>, earlier: &[Part<'_>]) -> (usize, u64) {
    let mut code = Vec::new();
    Same { arena }.code(earlier, &mut code);
    let hash = code.iter().fold(0u64, |hash, piece| {
        hash.wrapping_mul(BASE).wrapping_add(piece_hash(piece))
    });
    (code.len(), hash)
}

/// Both sides of a comparison with no holes, in one arena.
struct Same<'a, 't> {
    arena: &'a Arena<'t>,
}

impl<'a> Same<'a, '_> {
    fn parts(&self, a: &[Part<'_>], b: &[Part<'_>]) -> bool {
        let (mut x, mut y) = (Vec::new(), Vec::new());
        self.code(a, &mut x);
        self.code(b, &mut y);
        x == y
    }

    fn code(&self, of: &[Part<'_>], out: &mut Vec<Code<'a>>) {
        for part in of {
            match part {
                Part::Token(token) => out.push(Code::Token(self.arena.token_text(token))),
                Part::Child(child) => {
                    if let Some(value) = self.arena.value(*child) {
                        out.push(Code::Value(value));
                        continue;
                    }
                    let inner = parts(self.arena, *child);
                    if inner.is_empty() {
                        out.push(Code::Text(self.arena.source(*child)));
                    } else {
                        self.code(&inner, out);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Builder, Kind, Language, TokenText};

    const TOY: Language = Language::new("toy");

    /// A toy call, `name(args)`, as an arena: a Call over a Name and its arguments, each a
    /// Name, with `(`, `,` and `)` kept as tokens.  A `$` before an argument makes it a hole in a
    /// pattern; `$...` a sequence hole.
    fn call(text: &str) -> (Arena<'static>, Vec<(NodeId, Hole)>) {
        let open = text.find('(').unwrap();
        let mut builder = Builder::new(TOY, text).unwrap();
        builder.enter(Kind::new("Call"), 0..text.len());
        builder.enter(Kind::new("Name"), 0..open);
        builder.leave();
        builder.token(open..open + 1, TokenText::Source);
        let mut holes = Vec::new();
        let mut at = open + 1;
        let close = text.len() - 1;
        let mut index = 2;
        while at < close {
            let end = text[at..close].find(',').map_or(close, |i| at + i);
            let arg = text[at..end].trim();
            let start = at + text[at..end].find(arg).unwrap();
            builder.enter(Kind::new("Name"), start..start + arg.len());
            builder.leave();
            if let Some(name) = arg.strip_prefix('$') {
                let (name, sequence) = match name.strip_suffix("...") {
                    Some(name) => (name, true),
                    None => (name, false),
                };
                let name = (!name.is_empty() && name != "_").then(|| name.to_owned());
                holes.push((NodeId(index), Hole { name, sequence }));
            }
            index += 1;
            if end < close {
                builder.token(end..end + 1, TokenText::Source);
            }
            at = end + 1;
        }
        builder.token(close..close + 1, TokenText::Source);
        builder.leave();
        (builder.finish().into_owned(), holes)
    }

    /// A pattern over a root holding one toy call.
    fn pattern(text: &str) -> Pattern {
        let (inner, holes) = call(text);
        // Wrap the call in a root, as a language's pattern compiler does with a module.
        let mut builder = Builder::new(TOY, inner.text()).unwrap();
        builder.enter(Kind::new("Root"), 0..text.len());
        build_copy(&inner, inner.root(), &mut builder);
        builder.leave();
        let arena = builder.finish().into_owned();
        let shifted = holes
            .into_iter()
            .map(|(n, h)| (NodeId(n.0 + 1), h))
            .collect();
        Pattern::new(arena, shifted)
    }

    fn build_copy(from: &Arena<'_>, node: NodeId, builder: &mut Builder<'_>) {
        builder.enter(from.kind(node), from.range(node));
        let children: Vec<_> = from.children(node).collect();
        let tokens = from.tokens_in(from.range(node));
        let mut tokens = tokens.iter().peekable();
        for child in children {
            while let Some(t) =
                tokens.next_if(|t| from.token_range(t).end <= from.range(child).start)
            {
                builder.token(from.token_range(t), TokenText::Source);
            }
            build_copy(from, child, builder);
        }
        for t in tokens {
            builder.token(from.token_range(t), TokenText::Source);
        }
        builder.leave();
    }

    fn found(pattern_text: &str, target_text: &str) -> Vec<String> {
        let (target, _) = call(target_text);
        find(&pattern(pattern_text), &target)
            .into_iter()
            .map(|m| target_text[m.range].to_owned())
            .collect()
    }

    #[test]
    fn a_shape_matches_with_its_holes_and_not_otherwise() {
        assert_eq!(found("f(a,b)", "f(a,b)"), ["f(a,b)"]);
        assert_eq!(found("f(a,b)", "f(a,c)"), Vec::<String>::new());
        assert_eq!(found("f($x,b)", "f(q,b)"), ["f(q,b)"]);
        assert_eq!(found("f($x,$x)", "f(q,q)"), ["f(q,q)"]);
        assert_eq!(found("f($x,$x)", "f(q,r)"), Vec::<String>::new());
        assert_eq!(found("f($...)", "f(a,b,c)"), ["f(a,b,c)"]);
        assert_eq!(found("f(a,$rest...)", "f(a,b,c)"), ["f(a,b,c)"]);
        assert_eq!(found("f(a,$rest...)", "f(a)"), Vec::<String>::new());
    }

    #[test]
    fn tokens_and_values_are_read_back_as_given() {
        let mut builder = Builder::new(TOY, "'x' + \"x\"").unwrap();
        builder.enter(Kind::new("Sum"), 0..9);
        builder.enter(Kind::new("Text"), 0..3);
        builder.value("x".into());
        builder.leave();
        builder.token(4..5, TokenText::Source);
        builder.enter(Kind::new("Text"), 6..9);
        builder.value("x".into());
        builder.leave();
        builder.leave();
        let arena = builder.finish();
        let nodes: Vec<_> = arena.nodes().collect();
        assert_eq!(arena.value(nodes[1]), Some("x"));
        assert_eq!(arena.value(nodes[0]), None);
        let tokens = arena.tokens_in(0..9);
        assert_eq!(tokens.len(), 1);
        assert_eq!(arena.token_text(&tokens[0]), "+");
        assert_eq!(arena.tokens_in(0..4), &[]);
        let fixed = Token {
            start: 0,
            end: 1,
            compare: TokenText::Fixed("open"),
        };
        assert_eq!(arena.token_text(&fixed), "open");
    }

    #[test]
    fn a_run_never_crosses_from_one_list_into_another() {
        // Two lists under one node: a, b in the first, c in the second.
        let text = "abc";
        let mut builder = Builder::new(TOY, text).unwrap();
        builder.enter(Kind::new("Block"), 0..3);
        builder.begin_list();
        for i in 0..2 {
            builder.enter(Kind::new("S"), i..i + 1);
            builder.leave();
        }
        builder.end_list();
        builder.begin_list();
        builder.enter(Kind::new("S"), 2..3);
        builder.leave();
        builder.end_list();
        builder.leave();
        let target = builder.finish();
        let nodes: Vec<_> = target.nodes().collect();
        assert_eq!(target.list(nodes[0]), None);
        assert_eq!(target.list(nodes[1]), target.list(nodes[2]));
        assert_ne!(target.list(nodes[2]), target.list(nodes[3]));
    }

    #[test]
    #[should_panic(expected = "a pattern was matched against another language's arena")]
    fn a_pattern_of_another_language_is_a_defect() {
        let mut builder = Builder::new(Language::new("other"), "f()").unwrap();
        builder.enter(Kind::new("Call"), 0..3);
        builder.leave();
        let _ = find(&pattern("f(a)"), &builder.finish());
    }
}

/// The indexed matcher held to backtracking over generated runs: blocks of three kinds of
/// statement, in one list or two, and patterns of fixed statements, single holes and sequences,
/// named or not, names repeated or not.
#[cfg(test)]
mod indexed_against_backtracking {
    use super::*;
    use crate::{Builder, Hole, Kind, Language};
    use proptest::prelude::*;

    const STATEMENTS: [&str; 3] = ["w", "x", "y"];

    /// A target: statements in a module, split into two lists at `split`, like a branch and its
    /// else clause.
    fn target(statements: &[usize], split: usize) -> Arena<'static> {
        let text: String = statements
            .iter()
            .map(|&s| STATEMENTS[s])
            .collect::<Vec<_>>()
            .join(" ");
        let text: &'static str = Box::leak(text.into_boxed_str());
        let mut builder = Builder::new(Language::new("toy"), text).unwrap();
        builder.enter(Kind::new("Module"), 0..text.len());
        builder.begin_list();
        for (i, &s) in statements.iter().enumerate() {
            if i == split {
                builder.end_list();
                builder.begin_list();
            }
            builder.enter(Kind::new("Statement"), i * 2..i * 2 + STATEMENTS[s].len());
            builder.leave();
        }
        builder.end_list();
        builder.leave();
        builder.finish().into_owned()
    }

    /// One part of a generated pattern.
    #[derive(Debug, Clone)]
    enum Item {
        Fixed(usize),
        Single(Option<usize>),
        Sequence(Option<usize>),
    }

    fn item() -> impl Strategy<Value = Item> {
        let name = prop::option::of(0usize..2);
        prop_oneof![
            3 => (0usize..3).prop_map(Item::Fixed),
            1 => name.clone().prop_map(Item::Single),
            2 => name.prop_map(Item::Sequence),
        ]
    }

    /// A pattern of the items as statements of a module, each a leaf whose text is its
    /// statement, so that fixed ones compare by text and holes match anything.
    fn pattern(items: &[Item]) -> Pattern {
        let words: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                Item::Fixed(s) => STATEMENTS[*s].to_owned(),
                Item::Single(_) | Item::Sequence(_) => format!("h{i}"),
            })
            .collect();
        let text: &'static str = Box::leak(words.join(" ").into_boxed_str());
        let mut builder = Builder::new(Language::new("toy"), text).unwrap();
        builder.enter(Kind::new("Module"), 0..text.len());
        let mut at = 0;
        for word in &words {
            builder.enter(Kind::new("Statement"), at..at + word.len());
            builder.leave();
            at += word.len() + 1;
        }
        builder.leave();
        let arena = builder.finish().into_owned();
        let nodes: Vec<NodeId> = arena.children(arena.root()).collect();
        let holes = items
            .iter()
            .zip(nodes)
            .filter_map(|(item, node)| {
                let (name, sequence) = match item {
                    Item::Fixed(_) => return None,
                    Item::Single(name) => (name, false),
                    // A sequence's name is kept apart from a single hole's, as a compiler refuses
                    // one name used both ways.
                    Item::Sequence(name) => (&name.map(|n| n + 10), true),
                };
                Some((
                    node,
                    Hole {
                        name: name.map(|n| format!("n{n}")),
                        sequence,
                    },
                ))
            })
            .collect();
        Pattern::new(arena, holes)
    }

    /// The `n`th statement of a generated pattern.
    fn nth(pattern: &Pattern, n: usize) -> NodeId {
        let arena = pattern.arena();
        arena.children(arena.root()).nth(n).unwrap()
    }

    #[test]
    fn one_choice_that_matters_is_allowed_and_a_second_is_costly() {
        use Item::{Fixed, Sequence, Single};
        // A sequence named twice: one step decides what the second must match.
        let once = pattern(&[
            Fixed(0),
            Sequence(Some(0)),
            Fixed(1),
            Sequence(Some(0)),
            Fixed(2),
        ]);
        assert_eq!(once.costly(), None);
        // A name bound in one statement and used in another, across sequences.
        let single = pattern(&[
            Fixed(0),
            Sequence(None),
            Single(Some(0)),
            Sequence(None),
            Single(Some(0)),
            Fixed(2),
        ]);
        assert_eq!(single.costly(), None);
        // Two sequences each named twice: placing `x` decides `$b`, and placing `y` decides
        // the second `$a`, which is named as the second step that matters.
        let twice = pattern(&[
            Fixed(0),
            Sequence(Some(0)),
            Fixed(1),
            Sequence(Some(1)),
            Fixed(2),
            Sequence(Some(0)),
            Fixed(1),
            Sequence(Some(1)),
            Fixed(0),
        ]);
        assert_eq!(twice.costly(), Some(Costly::SecondChoice(nth(&twice, 5))));
        // One sequence named twice, but with a step before each use.
        let spread = pattern(&[
            Fixed(0),
            Sequence(None),
            Fixed(1),
            Sequence(Some(0)),
            Fixed(2),
            Sequence(Some(0)),
            Fixed(0),
        ]);
        assert_eq!(spread.costly(), Some(Costly::SecondChoice(nth(&spread, 5))));
        // Unnamed sequences between fixed statements decide nothing.
        let free = pattern(&[Fixed(0), Sequence(None), Fixed(1), Sequence(None), Fixed(2)]);
        assert_eq!(free.costly(), None);
    }

    #[test]
    fn two_sequences_together_are_costly_whatever_their_names() {
        use Item::{Fixed, Sequence};
        let together = pattern(&[Fixed(0), Sequence(None), Sequence(None), Fixed(1)]);
        assert_eq!(together.costly(), Some(Costly::Together(nth(&together, 2))));
    }

    #[test]
    #[should_panic(expected = "a pattern compiler let through a run of statements too costly")]
    fn matching_a_costly_pattern_is_a_defect() {
        use Item::{Fixed, Sequence};
        let together = pattern(&[Fixed(0), Sequence(None), Sequence(None), Fixed(1)]);
        let _ = find(&together, &target(&[0, 1], 2));
    }

    /// A repeated sequence's run can line up while the segment after it fails on a name bound
    /// before: here `$v` is `y`, and where the second `$a` ends, `x y` stands rather than `y y`.
    #[test]
    fn a_repeated_runs_segment_can_still_fail_on_an_earlier_name() {
        use Item::{Fixed, Sequence, Single};
        let items = [
            Fixed(0),
            Single(Some(0)),
            Sequence(Some(1)),
            Fixed(1),
            Sequence(Some(1)),
            Single(Some(0)),
            Fixed(2),
        ];
        let pattern = pattern(&items);
        assert_eq!(pattern.costly(), None);
        let statements = [0, 2, 0, 1, 0, 1, 2];
        let target = target(&statements, statements.len());
        assert_eq!(
            find_with(&pattern, &target, true),
            find_with(&pattern, &target, false)
        );
    }

    /// A pattern `w ; $a ; x`, its statements with a token between each.
    fn separated_pattern() -> Pattern {
        let text = "w ; h ; x";
        let mut builder = Builder::new(Language::new("toy"), text).unwrap();
        builder.enter(Kind::new("Module"), 0..text.len());
        for (at, statement) in [(0, true), (2, false), (4, true), (6, false), (8, true)] {
            if statement {
                builder.enter(Kind::new("Statement"), at..at + 1);
                builder.leave();
            } else {
                builder.token(at..at + 1, crate::TokenText::Source);
            }
        }
        builder.leave();
        let arena = builder.finish().into_owned();
        let hole = arena.children(arena.root()).nth(1).unwrap();
        Pattern::new(
            arena,
            vec![(
                hole,
                Hole {
                    name: None,
                    sequence: true,
                },
            )],
        )
    }

    #[test]
    fn a_run_with_tokens_between_its_statements_is_matched_by_backtracking() {
        let pattern = separated_pattern();
        // Not indexed, so not judged by the index's costs either.
        assert_eq!(pattern.costly(), None);
        let wanted = between_first_and_last(&parts(pattern.arena(), pattern.arena().root()));
        assert!(Run::new(&pattern, &wanted).is_none());
        let text = "w ; y ; x";
        let mut builder = Builder::new(Language::new("toy"), text).unwrap();
        builder.enter(Kind::new("Module"), 0..text.len());
        builder.begin_list();
        for (at, statement) in [(0, true), (2, false), (4, true), (6, false), (8, true)] {
            if statement {
                builder.enter(Kind::new("Statement"), at..at + 1);
                builder.leave();
            } else {
                builder.token(at..at + 1, crate::TokenText::Source);
            }
        }
        builder.end_list();
        builder.leave();
        let target = builder.finish().into_owned();
        let found = find(&pattern, &target);
        assert_eq!(found, [Match { range: 0..9 }]);
    }

    /// A block `a b` under a node, with a token before it - like the `:` of a `def` - and, if
    /// `between`, one between its statements too, like a `;`.
    fn block_with_tokens(between: bool) -> Arena<'static> {
        let text = if between { ": a ; b" } else { ": a b" };
        let mut builder = Builder::new(Language::new("toy"), text).unwrap();
        builder.enter(Kind::new("Def"), 0..text.len());
        builder.token(0..1, crate::TokenText::Source);
        builder.begin_list();
        builder.enter(Kind::new("Statement"), 2..3);
        builder.leave();
        if between {
            builder.token(4..5, crate::TokenText::Source);
        }
        let b = text.len() - 1;
        builder.enter(Kind::new("Statement"), b..b + 1);
        builder.leave();
        builder.end_list();
        builder.leave();
        builder.finish().into_owned()
    }

    #[test]
    fn only_a_token_between_a_blocks_statements_separates_them() {
        for (between, expected) in [(false, false), (true, true)] {
            let arena = block_with_tokens(between);
            let available = parts(&arena, arena.root());
            assert_eq!(
                separated(&arena, &available),
                expected,
                "between: {between}"
            );
        }
    }

    /// A sequence whose name is used again holds different code from a later start, so a failure
    /// after the nearest place of the segment before it settles nothing: here `$b` is `x x` from
    /// the first `x`, which does not repeat after `y`, and `x` from the next, which does.
    #[test]
    fn a_later_place_is_tried_when_the_next_sequence_is_named_again() {
        let items = [
            Item::Fixed(0),
            Item::Sequence(None),
            Item::Fixed(1),
            Item::Sequence(Some(0)),
            Item::Fixed(2),
            Item::Sequence(Some(0)),
            Item::Fixed(0),
        ];
        let pattern = pattern(&items);
        let statements = [0, 2, 1, 1, 1, 2, 1, 0];
        let target = target(&statements, statements.len());
        let backtracked = find_with(&pattern, &target, false);
        assert_eq!(backtracked.len(), 1);
        assert_eq!(find_with(&pattern, &target, true), backtracked);
    }

    /// A segment that binds a name used further on changes what the rest is matched with, so a
    /// failure after its nearest place settles nothing: here `$v` is `x` from the first place,
    /// with no `x y` after it, and `w` from the next, with `w y` after it.
    #[test]
    fn a_later_place_is_tried_when_the_segment_binds_a_name_used_further_on() {
        let items = [
            Item::Fixed(0),
            Item::Sequence(None),
            Item::Single(Some(0)),
            Item::Sequence(None),
            Item::Single(Some(0)),
            Item::Fixed(2),
        ];
        let pattern = pattern(&items);
        let statements = [0, 0, 1, 0, 0, 0, 2];
        let target = target(&statements, statements.len());
        let backtracked = find_with(&pattern, &target, false);
        assert_eq!(backtracked.len(), 2);
        assert_eq!(find_with(&pattern, &target, true), backtracked);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        #[test]
        fn indexing_finds_what_backtracking_finds(
            statements in prop::collection::vec(0usize..3, 1..24),
            split in 0usize..24,
            first in 0usize..3,
            middle in prop::collection::vec(item(), 0..5),
            last in 0usize..3,
        ) {
            // Fixed at both ends, as a compiler requires of a run of statements.
            let items: Vec<Item> = std::iter::once(Item::Fixed(first))
                .chain(middle)
                .chain(std::iter::once(Item::Fixed(last)))
                .collect();
            let pattern = pattern(&items);
            prop_assert_eq!(pattern.unbounded_sequence(), None);
            let target = target(&statements, split.min(statements.len()));
            prop_assert_eq!(
                find_with(&pattern, &target, true),
                find_with(&pattern, &target, false),
                "{:?} over {:?}",
                items,
                statements
            );
        }
    }
}
