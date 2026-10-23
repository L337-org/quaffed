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
//!
//! A list of parts with a sequence hole in it is matched by backtracking that remembers, for each
//! place in the pattern and place in the code, whether the rest matched from there, so that no
//! pair is worked out twice; [`List`] says how, and what it costs.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
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
/// ([`Pattern::unbounded_sequence`]) or is too costly to search ([`Pattern::costly`]), which is a
/// defect in its compiler.
#[must_use]
pub fn find(pattern: &Pattern, target: &Arena<'_>) -> Vec<Match> {
    assert!(
        pattern.unbounded_sequence().is_none(),
        "a pattern compiler let through a sequence hole at the edge of a run of statements"
    );
    assert!(
        pattern.costly().is_none(),
        "a pattern compiler let through a pattern too costly to search"
    );
    find_with(pattern, target, true)
}

/// [`find`], remembering what each place in a list matched and skipping to where fixed code can
/// stand where `remember`, and otherwise by backtracking alone: the slow way, which the fast one
/// is held to.
fn find_with(pattern: &Pattern, target: &Arena<'_>, remember: bool) -> Vec<Match> {
    let shape = pattern.arena();
    assert_eq!(
        shape.language(),
        target.language(),
        "a pattern was matched against another language's arena"
    );
    let matcher = Matcher::new(pattern, target, remember);
    // A pattern compiler refuses a pattern with no code, or with nothing but holes, so a pattern
    // of one node is one with a shape.
    let content: Vec<NodeId> = shape.children(shape.root()).collect();
    let mut found = Vec::new();
    if let [only] = content.as_slice() {
        for node in target.nodes() {
            let mut bindings = matcher.unbound();
            if matcher.node(*only, node, &mut bindings) {
                found.push(Match {
                    range: target.range(node),
                });
            }
        }
    } else {
        for node in target.nodes() {
            let available = parts(target, node);
            let in_a_list =
                |part: &Part<'_>| matches!(part, Part::Child(c) if target.list(*c).is_some());
            if !available.iter().any(in_a_list) {
                continue;
            }
            let ends = list_ends(target, &available);
            for (start, first) in available.iter().enumerate() {
                // A run of statements stays in one list - one block's statements - so it never
                // takes in a header, a clause, or another block.
                let Part::Child(first) = first else { continue };
                if target.list(*first).is_none() {
                    continue;
                }
                // Matched from this start as far as the pattern goes: one try per start, whatever
                // the run's length.
                let mut bindings = matcher.unbound();
                let at = Place {
                    owner: node,
                    from: start,
                    limit: ends[start],
                };
                if let Some(end) = matcher.list(Within::Run, at, &available, 0, &mut bindings) {
                    found.push(Match {
                        range: target.range(*first).start
                            ..target.range(statement(&available, end - 1)).end,
                    });
                }
            }
        }
    }
    found.sort_by_key(|m| (m.range.start, m.range.end));
    found.dedup();
    found
}

/// The statement at `at` among a node's parts, where a run of statements ends.
fn statement(available: &[Part<'_>], at: usize) -> NodeId {
    match available[at] {
        Part::Child(child) => child,
        Part::Token(_) => unreachable!("a run of statements ends at a statement"),
    }
}

/// For each of a node's parts, where the list its child belongs to ends: the index of the first
/// child of another list after it, or the end.  Found in one pass from the back, so that each
/// start of a run, and each sequence, need not look ahead for it.
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

/// Why a pattern would make a search cost more than the square of a list's length, so that a
/// compiler refuses it: [`Pattern::costly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Costly {
    /// Two sequences stand together in a run of statements with no statement between them, so
    /// nothing settles how the statements divide between them: the second of them.
    Together(NodeId),
    /// A second place where what a sequence takes decides what later code must match, each such
    /// place multiplying a search's work by the length of the list it stands in: the hole that
    /// makes it one.
    SecondChoice(NodeId),
}

impl Pattern {
    /// Whether matching this pattern could cost more than the square of the length of a list it
    /// is matched in, and why.
    ///
    /// A list with sequence holes is matched by remembering, for each place in the pattern and
    /// place in the code, whether the rest matched from there ([`List`]), which costs about the
    /// list's length times the pattern's.  That holds while the rest depends only on where it
    /// starts.  A **choice** is a place where it depends on more: where a name is bound in a way
    /// that varies with where an earlier sequence stopped, and is used later in the same list, or
    /// where a sequence named elsewhere in the list starts.  Each choice multiplies the work by
    /// the list's length, so one makes it quadratic and a second cubic.  The fixed code before a
    /// list's first sequence is not a choice, since where it stands is where the list, or the
    /// run, starts.
    ///
    /// Every language's compiler refuses a pattern this names, and [`find`] panics on one, as a
    /// defect in its compiler.  Two sequences standing together in a run of statements are
    /// refused too: matching them costs nothing more, but nothing settles how the statements
    /// divide between them.
    #[must_use]
    pub fn costly(&self) -> Option<Costly> {
        let shape = self.arena();
        let root = shape.root();
        let mut choices = Vec::new();
        if shape.children(root).nth(1).is_some() {
            let run = between_first_and_last(&parts(shape, root));
            if let Some(second) = together(self, &run) {
                return Some(Costly::Together(second));
            }
            choices.extend(list_choices(self, &run));
        }
        for node in shape.nodes().filter(|node| *node != root) {
            let wanted = parts(shape, node);
            if wanted.iter().any(|part| is_sequence(self, part)) {
                choices.extend(list_choices(self, &wanted));
            }
        }
        choices.sort_unstable();
        choices.dedup();
        choices.get(1).copied().map(Costly::SecondChoice)
    }
}

fn is_sequence(pattern: &Pattern, part: &Part<'_>) -> bool {
    matches!(part, Part::Child(child) if pattern.hole(*child).is_some_and(|hole| hole.sequence))
}

/// The second of two sequences standing together in a run, if any do.
fn together(pattern: &Pattern, run: &[Part<'_>]) -> Option<NodeId> {
    run.windows(2).find_map(|pair| match pair {
        [first, Part::Child(second)]
            if is_sequence(pattern, first) && is_sequence(pattern, &pair[1]) =>
        {
            Some(*second)
        }
        _ => None,
    })
}

/// The choices a list of a pattern's parts makes, each named by the hole that makes it: see
/// [`Pattern::costly`].
///
/// The list is split at its sequences into segments, the fixed code between them, possibly
/// none.  Each hole within the list belongs to one part of it, numbered in order - segment 0,
/// the sequence before segment 1, segment 1, and so on - a sequence being a part of its own and
/// any other hole lying inside one of a segment's children.  A segment after the first makes a
/// choice when a name first bound by the sequence before it or within it is used further on in
/// the list, or when the sequence after it is named elsewhere in the list, since that sequence
/// then holds different code from a later place.
fn list_choices(pattern: &Pattern, wanted: &[Part<'_>]) -> Vec<NodeId> {
    let shape = pattern.arena();
    let mut segments: Vec<Vec<NodeId>> = vec![Vec::new()];
    let mut holes = Vec::new();
    for part in wanted {
        match part {
            Part::Child(child) if is_sequence(pattern, part) => {
                holes.push(*child);
                segments.push(Vec::new());
            }
            Part::Child(child) => segments.last_mut().expect("begun with one").push(*child),
            Part::Token(_) => {}
        }
    }
    let part_of = |node: NodeId| -> Option<usize> {
        if let Some(at) = holes.iter().position(|hole| *hole == node) {
            return Some(2 * at + 1);
        }
        let range = shape.range(node);
        segments
            .iter()
            .position(|segment| {
                segment.iter().any(|child| {
                    let outer = shape.range(*child);
                    outer.start <= range.start && range.end <= outer.end
                })
            })
            .map(|segment| 2 * segment)
    };
    // Each name's first and last part in the list, and the hole it is first bound by: holes
    // come in pre-order, which is the order of the parts, so the first seen is the first.
    let mut uses: BTreeMap<&str, (usize, usize, NodeId)> = BTreeMap::new();
    for (node, hole) in pattern.holes() {
        if let (Some(name), Some(at)) = (&hole.name, part_of(node)) {
            uses.entry(name).or_insert((at, at, node)).1 = at;
        }
    }
    (1..segments.len())
        .filter_map(|i| {
            let next = holes.get(i).copied().filter(|next| {
                pattern
                    .hole(*next)
                    .and_then(|hole| hole.name.as_deref())
                    .is_some_and(|name| (uses[name].0, uses[name].1) != (2 * i + 1, 2 * i + 1))
            });
            let bound_here = uses
                .values()
                .find(|&&(first, last, _)| 2 * i - 1 <= first && first <= 2 * i && last > 2 * i)
                .map(|&(_, _, node)| node);
            next.or(bound_here)
        })
        .collect()
}

/// What a named hole matched: one child for a hole, a run of a node's parts for a sequence, by
/// that node and the range of its parts.  A name is one or the other throughout a pattern,
/// which its compiler checks, so two bindings of a name are always of the same form.
#[derive(Debug, Clone)]
enum Bound {
    Node(NodeId),
    Run(NodeId, Range<usize>),
}

impl Bound {
    /// Where it is, as a key: what a remembered result was worked out with.
    fn key(&self) -> Key {
        match self {
            Bound::Node(node) => Key::Node(*node),
            Bound::Run(owner, range) => Key::Run(*owner, range.start, range.end),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Node(NodeId),
    Run(NodeId, usize, usize),
}

/// What each name used more than once is bound to so far, by [`Matcher::slots`]'s numbering.  A
/// name used once binds nothing, since nothing is compared with it.
type Bindings = Vec<Option<Bound>>;

/// Which of a pattern's lists of parts is being matched: a node's parts, or the run of
/// statements a pattern of several is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Within {
    Node(NodeId),
    Run,
}

/// Where in the code a list is being matched: among `owner`'s parts, from `from`, up to `limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Place {
    owner: NodeId,
    from: usize,
    limit: usize,
}

/// A list of a pattern's parts, as matched: a node's parts, all of which a match takes, or the
/// run of statements a pattern of several is, which a match takes from wherever it starts.
///
/// **How it is matched.**  The parts are matched in order, a sequence hole taking the fewest
/// parts that let the rest match, as backtracking would.  Where the list has a sequence, the
/// outcome of matching its rest from each pair of places - a part of the pattern, a part of the
/// code - is remembered, with the bindings it made, so that no pair is worked out twice.  The
/// outcome depends only on the pair and on what the names the rest uses are bound to, so those
/// are part of what it is remembered by.
///
/// Two further steps skip what cannot match.  A sequence stops only where the fixed code after
/// it can stand, found once per list by the code alone.  And where that code stands but the rest
/// fails after it, the rest fails after every later place too - whatever stands after a later
/// place also stands after this one - unless the place changes what the rest is matched with:
/// when the fixed code binds a name used later, when the sequence after it is named again, when
/// the list must end where the code does, with no sequence after it to take the difference, or
/// when another list follows, which a later place could let the next sequence start in.  Only
/// there are later places tried.
///
/// **What it costs.**  Without a choice ([`Pattern::costly`]), each pair is worked out once, so
/// a list costs about its length times the pattern's; each choice multiplies that by the list's
/// length, and a compiler refuses a second.
struct List<'p> {
    wanted: Vec<Part<'p>>,
    /// Whether a match must take every part, as in a node's parts, rather than stop where the
    /// pattern does, as a run of statements does.
    whole: bool,
    /// Whether any part is a sequence hole, without which nothing is worth remembering.
    sequences: bool,
    /// The names used in `wanted[i..]`, each part's descendants included, by slot.
    uses: Vec<Vec<usize>>,
    /// For each part, where the fixed code it starts ends: the next sequence, or the end.
    segment_end: Vec<usize>,
    /// For each sequence hole, whether the first place the fixed code after it matches settles
    /// the rest, so that no later place need be tried.
    settled: Vec<bool>,
}

/// A pattern and the arena it is matched against, with what has been worked out so far.
struct Matcher<'p, 't> {
    shape: &'p Arena<'static>,
    pattern: &'p Pattern,
    target: &'t Arena<'t>,
    remember: bool,
    /// Each name used more than once, numbered.
    slots: BTreeMap<&'p str, usize>,
    /// Each list of the pattern's, by node, the run of statements last.
    lists: RefCell<Vec<Option<Rc<List<'p>>>>>,
    parts: RefCell<HashMap<NodeId, Rc<[Part<'t>]>>>,
    ends: RefCell<HashMap<NodeId, Rc<[usize]>>>,
    codes: RefCell<HashMap<NodeId, Rc<Codes>>>,
    /// Where the fixed code starting at a list's part stands among a node's parts.
    stands: RefCell<HashMap<Stands, Rc<[usize]>>>,
    /// The outcome of matching a list's rest from a pair of places, with the names it uses
    /// bound as they were: where the match ended, and what it bound.
    outcomes: RefCell<HashMap<Remembered, Outcome>>,
}

/// A list's part where fixed code starts, and the node among whose parts it is looked for.
type Stands = (Within, usize, NodeId);

#[derive(Debug, PartialEq, Eq, Hash)]
struct Remembered {
    within: Within,
    place: Place,
    at: usize,
    bound: Vec<Option<Key>>,
}

type Outcome = Option<(usize, Vec<(usize, Bound)>)>;

impl<'p, 't> Matcher<'p, 't> {
    fn new(pattern: &'p Pattern, target: &'t Arena<'_>, remember: bool) -> Matcher<'p, 't> {
        let mut counts: BTreeMap<&'p str, usize> = BTreeMap::new();
        for (_, hole) in pattern.holes() {
            if let Some(name) = &hole.name {
                *counts.entry(name).or_default() += 1;
            }
        }
        let slots = counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .enumerate()
            .map(|(slot, (name, _))| (name, slot))
            .collect();
        Matcher {
            shape: pattern.arena(),
            pattern,
            target,
            remember,
            slots,
            lists: RefCell::new(vec![None; pattern.arena().nodes().count() + 1]),
            parts: RefCell::default(),
            ends: RefCell::default(),
            codes: RefCell::default(),
            stands: RefCell::default(),
            outcomes: RefCell::default(),
        }
    }

    fn unbound(&self) -> Bindings {
        vec![None; self.slots.len()]
    }

    fn slot(&self, hole: &Hole) -> Option<usize> {
        hole.name
            .as_deref()
            .and_then(|name| self.slots.get(name).copied())
    }

    fn parts_of(&self, node: NodeId) -> Rc<[Part<'t>]> {
        if let Some(found) = self.parts.borrow().get(&node) {
            return Rc::clone(found);
        }
        let made: Rc<[Part<'t>]> = parts(self.target, node).into();
        self.parts.borrow_mut().insert(node, Rc::clone(&made));
        made
    }

    fn ends_of(&self, node: NodeId) -> Rc<[usize]> {
        if let Some(found) = self.ends.borrow().get(&node) {
            return Rc::clone(found);
        }
        let made: Rc<[usize]> = list_ends(self.target, &self.parts_of(node)).into();
        self.ends.borrow_mut().insert(node, Rc::clone(&made));
        made
    }

    fn codes_of(&self, node: NodeId) -> Rc<Codes> {
        if let Some(found) = self.codes.borrow().get(&node) {
            return Rc::clone(found);
        }
        let made = Rc::new(Codes::new(self.target, &self.parts_of(node)));
        self.codes.borrow_mut().insert(node, Rc::clone(&made));
        made
    }

    /// The slots of the names used by the holes within `part`, itself included.
    fn slots_in(&self, part: &Part<'_>) -> Vec<usize> {
        let Part::Child(child) = part else {
            return Vec::new();
        };
        let outer = self.shape.range(*child);
        self.pattern
            .holes()
            .filter(|(node, _)| {
                let range = self.shape.range(*node);
                outer.start <= range.start && range.end <= outer.end
            })
            .filter_map(|(_, hole)| self.slot(hole))
            .collect()
    }

    fn list_of(&self, within: Within) -> Rc<List<'p>> {
        let index = match within {
            Within::Node(node) => node.0 as usize,
            Within::Run => self.lists.borrow().len() - 1,
        };
        if let Some(found) = &self.lists.borrow()[index] {
            return Rc::clone(found);
        }
        let shape = self.shape;
        let (wanted, whole) = match within {
            Within::Node(node) => (parts(shape, node), true),
            Within::Run => (between_first_and_last(&parts(shape, shape.root())), false),
        };
        let sequence = |part: &Part<'_>| is_sequence(self.pattern, part);
        let len = wanted.len();
        let mut uses = vec![Vec::new(); len + 1];
        for i in (0..len).rev() {
            let mut here = uses[i + 1].clone();
            here.extend(self.slots_in(&wanted[i]));
            here.sort_unstable();
            here.dedup();
            uses[i] = here;
        }
        let mut segment_end = vec![len; len + 1];
        for i in (0..len).rev() {
            segment_end[i] = if sequence(&wanted[i]) {
                i
            } else {
                segment_end[i + 1]
            };
        }
        let settled = (0..len)
            .map(|i| {
                if !sequence(&wanted[i]) || i + 1 == len {
                    return false;
                }
                let end = segment_end[i + 1];
                // The fixed code after the sequence, which must be some.
                if end == i + 1 {
                    return false;
                }
                // With no sequence after it, a list that must end where the code does is settled
                // only by the place that ends it.
                if end == len && whole {
                    return false;
                }
                // A sequence after it named again holds other code from a later place.
                if let Some(Part::Child(next)) = wanted.get(end)
                    && self
                        .pattern
                        .hole(*next)
                        .and_then(|hole| self.slot(hole))
                        .is_some()
                {
                    return false;
                }
                // A name the code binds that the rest uses takes what this place gives it.
                let binds: Vec<usize> = wanted[i + 1..end]
                    .iter()
                    .flat_map(|part| self.slots_in(part))
                    .collect();
                !binds.iter().any(|slot| uses[end].contains(slot))
            })
            .collect();
        let made = Rc::new(List {
            sequences: wanted.iter().any(sequence),
            wanted,
            whole,
            uses,
            segment_end,
            settled,
        });
        self.lists.borrow_mut()[index] = Some(Rc::clone(&made));
        made
    }

    fn node(&self, wanted: NodeId, node: NodeId, bindings: &mut Bindings) -> bool {
        if let Some(hole) = self.pattern.hole(wanted) {
            return self.bind(hole, Bound::Node(node), bindings);
        }
        if self.shape.kind(wanted) != self.target.kind(node) {
            return false;
        }
        // A value belongs to a kind, so two nodes of one kind both have one or neither does.
        if let Some(value) = self.shape.value(wanted) {
            return self.target.value(node) == Some(value);
        }
        let list = self.list_of(Within::Node(wanted));
        let available = parts(self.target, node);
        // A node with nothing to compare inside it - no children, no tokens, no value - compares
        // by its text, so that an adapter that kept no token for a leaf cannot make every leaf
        // of a kind alike.
        if list.wanted.is_empty() && available.is_empty() {
            return self.shape.source(wanted) == self.target.source(node);
        }
        let at = Place {
            owner: node,
            from: 0,
            limit: available.len(),
        };
        self.list(Within::Node(wanted), at, &available, 0, bindings)
            .is_some()
    }

    /// Matches the list's parts from `at` onwards against the code from `place.from`, and says
    /// where the match ends, keeping what it bound only if it matches.
    fn list(
        &self,
        within: Within,
        place: Place,
        available: &[Part<'t>],
        at: usize,
        bindings: &mut Bindings,
    ) -> Option<usize> {
        let list = self.list_of(within);
        let remembered = (self.remember && list.sequences).then(|| Remembered {
            within,
            place,
            at,
            bound: list.uses[at]
                .iter()
                .map(|&slot| bindings[slot].as_ref().map(Bound::key))
                .collect(),
        });
        if let Some(key) = &remembered {
            let known = self.outcomes.borrow().get(key).cloned();
            if let Some(outcome) = known {
                return outcome.map(|(end, made)| {
                    for (slot, bound) in made {
                        bindings[slot] = Some(bound);
                    }
                    end
                });
            }
        }
        let mut attempt = bindings.clone();
        let end = self.step(&list, within, place, available, at, &mut attempt);
        if let Some(key) = remembered {
            let made = end.map(|end| {
                let made = list.uses[at]
                    .iter()
                    .filter(|&&slot| bindings[slot].is_none())
                    .filter_map(|&slot| attempt[slot].clone().map(|bound| (slot, bound)))
                    .collect();
                (end, made)
            });
            self.outcomes.borrow_mut().insert(key, made);
        }
        if end.is_some() {
            *bindings = attempt;
        }
        end
    }

    fn step(
        &self,
        list: &List<'p>,
        within: Within,
        place: Place,
        available: &[Part<'t>],
        at: usize,
        bindings: &mut Bindings,
    ) -> Option<usize> {
        let Some(wanted) = list.wanted.get(at) else {
            return (!list.whole || place.from == place.limit).then_some(place.from);
        };
        let here = available[..place.limit].get(place.from).copied()?;
        let next = Place {
            from: place.from + 1,
            ..place
        };
        match (*wanted, here) {
            (Part::Token(token), Part::Token(other)) => {
                if self.shape.token_text(token) != self.target.token_text(other) {
                    return None;
                }
                self.list(within, next, available, at + 1, bindings)
            }
            (Part::Child(child), Part::Child(node)) => match self.pattern.hole(child) {
                Some(hole) if hole.sequence => {
                    self.sequence(list, within, place, available, at, bindings)
                }
                _ => {
                    if !self.node(child, node, bindings) {
                        return None;
                    }
                    self.list(within, next, available, at + 1, bindings)
                }
            },
            _ => None,
        }
    }

    /// Matches the sequence hole at `at` from `place.from`, and the rest after it.
    fn sequence(
        &self,
        list: &List<'p>,
        within: Within,
        place: Place,
        available: &[Part<'t>],
        at: usize,
        bindings: &mut Bindings,
    ) -> Option<usize> {
        let Part::Child(child) = list.wanted[at] else {
            unreachable!("a sequence hole is a child");
        };
        let hole = self.pattern.hole(child).expect("a sequence hole is a hole");
        // A run stays in one list, so `$body...` never takes in a clause or a header.
        let block_end = self.ends_of(place.owner)[place.from].min(place.limit);
        let stops = (place.from..block_end).filter(|&k| matches!(available[k], Part::Child(_)));
        let after = |stop: usize| Place {
            from: stop + 1,
            ..place
        };
        let slot = self.slot(hole);
        if let Some(slot) = slot
            && let Some(earlier) = bindings[slot].clone()
        {
            // A name already bound takes equal code again, which leaves it one end.
            let end = if self.remember {
                self.equal_run(&earlier, place, available, block_end)?
            } else {
                stops
                    .map(|stop| stop + 1)
                    .find(|&end| self.equal(&earlier, &Bound::Run(place.owner, place.from..end)))?
            };
            return self.list(
                within,
                Place { from: end, ..place },
                available,
                at + 1,
                bindings,
            );
        }
        if let Some(slot) = slot {
            // The first of a name used again: each length binds other code, so each is tried.
            for stop in stops {
                let mut attempt = bindings.clone();
                attempt[slot] = Some(Bound::Run(place.owner, place.from..stop + 1));
                if let Some(end) = self.list(within, after(stop), available, at + 1, &mut attempt) {
                    *bindings = attempt;
                    return Some(end);
                }
            }
            return None;
        }
        let next = at + 1;
        if !self.remember || next == list.wanted.len() || list.segment_end[next] == next {
            for stop in stops {
                if let Some(end) = self.list(within, after(stop), available, at + 1, bindings) {
                    return Some(end);
                }
            }
            return None;
        }
        // Stopped only where the fixed code after the sequence stands, nearest first.
        let segment = next..list.segment_end[next];
        let stands = self.stands(within, list, next, place.owner, available);
        let first = stands.partition_point(|&start| start <= place.from);
        for &start in &stands[first..] {
            if start > block_end {
                break;
            }
            if !matches!(available[start - 1], Part::Child(_)) {
                continue;
            }
            // The code may stand here by itself and not with what the names hold so far, which a
            // later place can still give it.
            let mut probe = bindings.clone();
            if self
                .fixed(
                    list,
                    segment.clone(),
                    Place {
                        from: start,
                        ..place
                    },
                    available,
                    &mut probe,
                )
                .is_none()
            {
                continue;
            }
            if let Some(end) = self.list(within, after(start - 1), available, at + 1, bindings) {
                return Some(end);
            }
            // Only where no other list follows can a later place give the rest nothing new: one
            // that lets the next sequence start in the next list, an `else` clause's, gives it a
            // run this place cannot.
            if list.settled[at] && block_end == place.limit {
                return None;
            }
        }
        None
    }

    /// Where the fixed code `list.wanted[at..]` up to the next sequence stands among `owner`'s
    /// parts, by the code alone: every place it matches with nothing bound, in order.  With
    /// names bound it can only match at fewer.
    fn stands(
        &self,
        within: Within,
        list: &List<'p>,
        at: usize,
        owner: NodeId,
        available: &[Part<'t>],
    ) -> Rc<[usize]> {
        let key = (within, at, owner);
        if let Some(found) = self.stands.borrow().get(&key) {
            return Rc::clone(found);
        }
        let segment = at..list.segment_end[at];
        let len = available.len();
        let made: Rc<[usize]> = (0..len)
            .filter(|&from| {
                let place = Place {
                    owner,
                    from,
                    limit: len,
                };
                self.fixed(list, segment.clone(), place, available, &mut self.unbound())
                    .is_some()
            })
            .collect();
        self.stands.borrow_mut().insert(key, Rc::clone(&made));
        made
    }

    /// Matches the fixed parts `list.wanted[segment]`, which hold no sequence, from
    /// `place.from`, and says where they end.
    fn fixed(
        &self,
        list: &List<'p>,
        segment: Range<usize>,
        place: Place,
        available: &[Part<'t>],
        bindings: &mut Bindings,
    ) -> Option<usize> {
        let mut from = place.from;
        for wanted in &list.wanted[segment] {
            let here = available[..place.limit].get(from).copied()?;
            let matched = match (wanted, here) {
                (Part::Token(token), Part::Token(other)) => {
                    self.shape.token_text(token) == self.target.token_text(other)
                }
                (Part::Child(child), Part::Child(node)) => self.node(*child, node, bindings),
                _ => false,
            };
            if !matched {
                return None;
            }
            from += 1;
        }
        Some(from)
    }

    /// Where a run from `place.from` holding the same code as `earlier` ends, within the list
    /// that ends at `block_end`: the one place, since every part adds code; `None` if there is
    /// none, or it does not end at a child.
    fn equal_run(
        &self,
        earlier: &Bound,
        place: Place,
        available: &[Part<'t>],
        block_end: usize,
    ) -> Option<usize> {
        let Bound::Run(owner, range) = earlier else {
            unreachable!("a compiler refuses one name for a node and a sequence");
        };
        let theirs = self.codes_of(*owner);
        let length = theirs.lengths[range.end] - theirs.lengths[range.start];
        let hash = theirs.hash(range.clone());
        let ours = self.codes_of(place.owner);
        let end = ours
            .lengths
            .binary_search(&(ours.lengths[place.from] + length))
            .ok()?;
        if end <= place.from
            || end > block_end
            || !matches!(available[end - 1], Part::Child(_))
            || ours.hash(place.from..end) != hash
        {
            return None;
        }
        self.equal(earlier, &Bound::Run(place.owner, place.from..end))
            .then_some(end)
    }

    /// Binds `bound` to `hole`'s name, or checks it equals what the name already holds.  A hole
    /// whose name is used once binds nothing and matches anything.
    fn bind(&self, hole: &Hole, bound: Bound, bindings: &mut Bindings) -> bool {
        let Some(slot) = self.slot(hole) else {
            return true;
        };
        match &bindings[slot] {
            None => {
                bindings[slot] = Some(bound);
                true
            }
            Some(earlier) => self.equal(earlier, &bound),
        }
    }

    /// Whether two places in the arena hold equal code: the same tokens, values and texts in the
    /// same order, whatever kinds of node hold them.
    fn equal(&self, a: &Bound, b: &Bound) -> bool {
        let as_parts = |bound: &Bound| -> Vec<Part<'t>> {
            match bound {
                Bound::Node(node) => vec![Part::Child(*node)],
                Bound::Run(owner, range) => self.parts_of(*owner)[range.clone()].to_vec(),
            }
        };
        Same { arena: self.target }.parts(&as_parts(a), &as_parts(b))
    }
}

/// The code of a place in an arena, as a match compares it, with no kinds in it: each kept
/// token's text, each value, and the text of a node with nothing inside it, in source order,
/// with where each nested node of more than one part begins and ends.  Two places with the same
/// code are equal whatever role they stand in, so a name used twice matches `a` as an argument,
/// a keyword, an attribute and a parameter alike, since each of those is a node of one part, or
/// a chain of them.  The boundaries keep apart trees whose tokens read alike: `(a + b) * c` and
/// `a + b * c`, and `[(a, b)]` and `[a, b]` once a tuple's own parentheses take no part.
#[derive(Debug, PartialEq, Eq, Hash)]
enum Code<'a> {
    Token(&'a str),
    Value(&'a str),
    Text(&'a str),
    Begin,
    End,
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

    /// The code of `of`, a place's parts.  The place itself is not bounded, so a node and a
    /// run compare by what is inside them; each node within it of more than one part is.
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
                    match inner.len() {
                        0 => out.push(Code::Text(self.arena.source(*child))),
                        1 => self.code(&inner, out),
                        _ => {
                            out.push(Code::Begin);
                            self.code(&inner, out);
                            out.push(Code::End);
                        }
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

/// The remembering matcher held to plain backtracking over generated lists: blocks of three
/// kinds of statement, in one list or two, and patterns of fixed statements, single holes and
/// sequences, named or not, names repeated or not, as a run of statements and as the body of a
/// node, with and without a token between each statement.
#[cfg(test)]
mod remembered_against_backtracking {
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
    #[should_panic(expected = "a pattern compiler let through a pattern too costly")]
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
    fn a_run_with_tokens_between_its_statements_is_matched() {
        let pattern = separated_pattern();
        assert_eq!(pattern.costly(), None);
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

    /// Words as statements of a module, or, with `wrapped`, of a `Def` node the module holds,
    /// split into two lists at `split`, like a branch and its else clause, with a `;` token
    /// between two statements of one list if `separated`.  Gives the arena and each word's node.
    fn block(
        words: &[String],
        split: usize,
        separated: bool,
        wrapped: bool,
    ) -> (Arena<'static>, Vec<NodeId>) {
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut separators = Vec::new();
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                if separated && i != split {
                    text.push_str(" ;");
                    separators.push(text.len() - 1);
                }
                text.push(' ');
            }
            spans.push(text.len()..text.len() + word.len());
            text.push_str(word);
        }
        let text: &'static str = Box::leak(text.into_boxed_str());
        let mut builder = Builder::new(Language::new("toy"), text).unwrap();
        builder.enter(Kind::new("Module"), 0..text.len());
        if wrapped {
            builder.enter(Kind::new("Def"), 0..text.len());
        }
        builder.begin_list();
        let mut separators = separators.into_iter().peekable();
        for (i, span) in spans.iter().enumerate() {
            if i == split && i > 0 {
                builder.end_list();
                builder.begin_list();
            }
            while let Some(at) = separators.next_if(|&at| at < span.start) {
                builder.token(at..at + 1, crate::TokenText::Source);
            }
            builder.enter(Kind::new("Statement"), span.clone());
            builder.leave();
        }
        builder.end_list();
        if wrapped {
            builder.leave();
        }
        builder.leave();
        let arena = builder.finish().into_owned();
        let parent = if wrapped {
            arena.children(arena.root()).next().unwrap()
        } else {
            arena.root()
        };
        let nodes = arena.children(parent).collect();
        (arena, nodes)
    }

    /// A pattern of the items as the statements of a `Def` node, as [`block`] lays them out.
    fn nested_pattern(items: &[Item], separated: bool) -> Pattern {
        let words: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                Item::Fixed(s) => STATEMENTS[*s].to_owned(),
                Item::Single(_) | Item::Sequence(_) => format!("h{i}"),
            })
            .collect();
        let (arena, nodes) = block(&words, words.len(), separated, true);
        let holes = items
            .iter()
            .zip(nodes)
            .filter_map(|(item, node)| {
                let (name, sequence) = match item {
                    Item::Fixed(_) => return None,
                    Item::Single(name) => (*name, false),
                    Item::Sequence(name) => (name.map(|n| n + 10), true),
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

    fn nested_target(statements: &[usize], split: usize, separated: bool) -> Arena<'static> {
        let words: Vec<String> = statements
            .iter()
            .map(|&s| STATEMENTS[s].to_owned())
            .collect();
        block(&words, split, separated, true).0
    }

    #[test]
    fn choices_inside_a_node_are_judged_as_in_a_run() {
        use Item::{Fixed, Sequence, Single};
        // One choice inside a node is allowed, a second refused, as at the top level.
        let once = nested_pattern(&[Sequence(Some(0)), Fixed(1), Sequence(Some(0))], false);
        assert_eq!(once.costly(), None);
        let twice = nested_pattern(
            &[
                Fixed(0),
                Sequence(Some(0)),
                Fixed(1),
                Sequence(Some(1)),
                Fixed(2),
                Sequence(Some(0)),
                Fixed(1),
                Sequence(Some(1)),
            ],
            false,
        );
        assert!(matches!(twice.costly(), Some(Costly::SecondChoice(_))));
        // Sequences standing together inside a node decide nothing and cost nothing more.
        let together = nested_pattern(&[Sequence(None), Sequence(None), Single(None)], true);
        assert_eq!(together.costly(), None);
    }

    /// Inside a node holding two lists, a later place of the code after a sequence can let the
    /// next sequence start in the second list: here only the last `w` of the first list lets
    /// `$...` take the whole second one, so a failure after the first `w` settles nothing.
    #[test]
    fn a_later_place_is_tried_when_another_list_follows() {
        use Item::{Fixed, Sequence};
        let pattern = nested_pattern(&[Sequence(None), Fixed(0), Sequence(None)], false);
        let statements = [0, 0, 1, 0, 0, 1, 0];
        let target = nested_target(&statements, 5, false);
        let backtracked = find_with(&pattern, &target, false);
        assert_eq!(backtracked.len(), 1);
        assert_eq!(find_with(&pattern, &target, true), backtracked);
    }

    /// Three unnamed sequences between fixed statements inside a node, which backtracking alone
    /// matches in time growing with the cube of the body's length, are matched in about its
    /// length: here a body of 2000 statements is searched well within the test's patience.
    #[test]
    fn sequences_inside_a_node_are_matched_without_trying_every_division() {
        use Item::{Fixed, Sequence};
        let pattern = nested_pattern(
            &[
                Fixed(0),
                Sequence(None),
                Fixed(1),
                Sequence(None),
                Fixed(1),
                Sequence(None),
                Fixed(2),
            ],
            false,
        );
        let mut statements = vec![0];
        statements.extend(std::iter::repeat_n(1, 2000));
        let target = nested_target(&statements, statements.len(), false);
        let started = std::time::Instant::now();
        assert_eq!(find(&pattern, &target), []);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        #[test]
        fn remembering_finds_what_backtracking_finds(
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

        #[test]
        fn remembering_finds_what_backtracking_finds_inside_a_node(
            statements in prop::collection::vec(0usize..3, 0..14),
            split in 0usize..14,
            items in prop::collection::vec(item(), 1..7),
            separated: bool,
        ) {
            // Inside a node a sequence may start or end the list, bounded by the node.
            let pattern = nested_pattern(&items, separated);
            let target = nested_target(&statements, split.min(statements.len()), separated);
            prop_assert_eq!(
                find_with(&pattern, &target, true),
                find_with(&pattern, &target, false),
                "{:?} over {:?}, split at {}, separated: {}",
                items,
                statements,
                split,
                separated
            );
        }
    }
}
