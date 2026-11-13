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

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use crate::{Arena, Hole, Kind, NodeId, Pattern, Token};

/// One place a pattern matched: the range it covers in the text the arena was built over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The matched code's byte range.
    pub range: Range<usize>,
}

/// One place a pattern matched, with the code each name asked for captured there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// The matched code's byte range.
    pub range: Range<usize>,
    /// For each name asked for, in the order asked, the byte range of the code it captured:
    /// one node's, or a sequence's from the start of its first node to the end of its last,
    /// with whatever stands between them.  A name used more than once captures the code it
    /// matched first, which every later use matched as equal code.
    pub captures: Vec<Range<usize>>,
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
    find_with(pattern, target, true, &[], None)
}

/// [`find`], with the code each name in `kept` captured at each match.  Where a name could
/// capture different code at one match - a sequence beside a hole, say - it captures what the
/// search settles on: each sequence, left to right, takes the fewest nodes that let the rest
/// match.  Asking for captures never changes which matches are found; where a name is used more
/// than once, recording them can match a node once more per match found.
///
/// # Panics
///
/// As [`find`] does, and if a name in `kept` is not one the pattern binds, a defect in the
/// caller.
#[must_use]
pub fn find_captured(pattern: &Pattern, target: &Arena<'_>, kept: &[&str]) -> Vec<Captured> {
    assert!(
        pattern.unbounded_sequence().is_none(),
        "a pattern compiler let through a sequence hole at the edge of a run of statements"
    );
    assert!(
        pattern.costly().is_none(),
        "a pattern compiler let through a pattern too costly to search"
    );
    assert_binds(pattern, kept);
    find_in(pattern, target, true, &[], None, None, &[], kept)
}

/// [`find_where`], with the code each name in `kept` captured at each match kept, as
/// [`find_captured`] gives it.  The captures are those of a way of binding that `accept`
/// accepted: the first way the search found if it was accepted, and otherwise the first other
/// way that was.
///
/// # Panics
///
/// As [`find_where`] does, and if a name in `kept` is not one the pattern binds.
pub fn find_where_captured(
    pattern: &Pattern,
    target: &Arena<'_>,
    tested: &[&str],
    accept: &mut Accept<'_>,
    kept: &[&str],
) -> Vec<Captured> {
    check_where(pattern, tested);
    assert_binds(pattern, kept);
    find_in(pattern, target, true, tested, Some(accept), None, &[], kept)
}

/// Panics unless the pattern binds every name in `names`, by some named hole.
fn assert_binds(pattern: &Pattern, names: &[&str]) {
    for name in names {
        assert!(
            pattern
                .holes()
                .any(|(_, hole)| hole.name.as_deref() == Some(*name)),
            "a capture of ${name} was asked for, which its pattern does not bind"
        );
    }
}

/// Panics unless `tested` is a `where` [`find_where`] can search with.
fn check_where(pattern: &Pattern, tested: &[&str]) {
    assert!(
        pattern.unbounded_sequence().is_none(),
        "a pattern compiler let through a sequence hole at the edge of a run of statements"
    );
    assert!(
        pattern.costly_with(tested).is_none(),
        "a where was searched with that makes its pattern too costly to search"
    );
    for name in tested {
        assert!(
            pattern
                .holes()
                .any(|(_, hole)| hole.name.as_deref() == Some(*name) && !hole.sequence),
            "a where tested ${name}, which its pattern does not bind to one node"
        );
    }
}

/// Whether `pattern` matches the node `node` of `target` itself, or with `anywhere` that node or
/// anything inside it, with each name in `bound` already bound to its node: a `where` comparing
/// what a match bound with a pattern that shares names with the match's, which must then be the
/// same code.
///
/// # Panics
///
/// As [`find`] does, and if a name in `bound` is not one the pattern binds to one node, a defect
/// in the caller.
#[must_use]
pub fn matches_at(
    pattern: &Pattern,
    target: &Arena<'_>,
    node: NodeId,
    anywhere: bool,
    bound: &[(&str, NodeId)],
) -> bool {
    assert!(
        pattern.unbounded_sequence().is_none() && pattern.costly().is_none(),
        "a pattern compiler let through a pattern it refuses"
    );
    for (name, _) in bound {
        assert!(
            pattern
                .holes()
                .any(|(_, hole)| hole.name.as_deref() == Some(*name) && !hole.sequence),
            "a name bound in advance, ${name}, is not one the pattern binds to one node"
        );
    }
    let range = target.range(node);
    let found = find_in(pattern, target, true, &[], None, Some(&range), bound, &[]);
    if anywhere {
        !found.is_empty()
    } else {
        found.iter().any(|m| m.range == range)
    }
}

/// What a `where` over what a match binds asks of each way: given the node bound to each name it
/// tests, whether the match passes.
pub type Accept<'a> = dyn FnMut(&[NodeId]) -> bool + 'a;

/// [`find`], keeping only the matches that some way of binding the names in `tested` satisfies
/// `accept`, which is given the node bound to each of them, in that order: a `where` over what
/// a match binds.  A match is kept if any binding satisfies it, so that a name a match could
/// bind more than one way - inside a sequence, say - is judged by every way, never by the one
/// the search happens to find first.  Each way is tried once, within the cost
/// [`Pattern::costly_with`] bounds.
///
/// # Panics
///
/// As [`find`] does, and if the pattern with these names is too costly to search
/// ([`Pattern::costly_with`]) or a name in `tested` is not one the pattern binds to one node,
/// either of which is a defect in the caller.
pub fn find_where(
    pattern: &Pattern,
    target: &Arena<'_>,
    tested: &[&str],
    accept: &mut Accept<'_>,
) -> Vec<Match> {
    check_where(pattern, tested);
    find_with(pattern, target, true, tested, Some(accept))
}

/// [`find`], remembering what each place in a list matched and skipping to where fixed code can
/// stand where `remember`, and otherwise by backtracking alone: the slow way, which the fast one
/// is held to.  With `accept`, a match is kept only if some binding of `tested` satisfies it.
fn find_with(
    pattern: &Pattern,
    target: &Arena<'_>,
    remember: bool,
    tested: &[&str],
    accept: Option<&mut Accept<'_>>,
) -> Vec<Match> {
    find_in(pattern, target, remember, tested, accept, None, &[], &[])
        .into_iter()
        .map(|found| Match { range: found.range })
        .collect()
}

/// [`find_with`] among the nodes inside `within`, a byte range of the target, or everywhere, with
/// the names in `bound` already bound to those nodes, so that a match must give each the same
/// code, and the code each name in `kept` captured at each match.
#[expect(
    clippy::too_many_arguments,
    reason = "one search with every way it can be asked for, private to this module"
)]
fn find_in(
    pattern: &Pattern,
    target: &Arena<'_>,
    remember: bool,
    tested: &[&str],
    mut accept: Option<&mut Accept<'_>>,
    within: Option<&Range<usize>>,
    bound: &[(&str, NodeId)],
    kept: &[&str],
) -> Vec<Captured> {
    let shape = pattern.arena();
    assert_eq!(
        shape.language(),
        target.language(),
        "a pattern was matched against another language's arena"
    );
    let bound_names: Vec<&str> = bound.iter().map(|(name, _)| *name).collect();
    let named: Vec<&str> = tested.iter().chain(&bound_names).copied().collect();
    let matcher = Matcher::new(pattern, target, remember, &named, kept);
    let mut initial = matcher.unbound();
    for (name, node) in bound {
        initial[matcher.slots[name]] = Some(Bound::Node(*node));
    }
    let asked = Asked {
        tested: tested.iter().map(|name| matcher.slots[name]).collect(),
        kept: kept.iter().map(|name| matcher.slots[name]).collect(),
        initial,
    };
    // Only the nodes inside `within`, found without a walk of the whole arena, so a search at
    // one node costs what that node holds rather than what the file does.
    let candidates = || -> Box<dyn Iterator<Item = NodeId> + '_> {
        match within {
            Some(within) => Box::new(target.nodes_within(within.clone())),
            None => Box::new(target.nodes()),
        }
    };
    // A pattern compiler refuses a pattern with no code, or with nothing but holes, so a pattern
    // of one node is one with a shape.
    let content: Vec<NodeId> = shape.children(shape.root()).collect();
    let mut found = Vec::new();
    if let [only] = content.as_slice() {
        for node in candidates() {
            matcher.next_node();
            matcher.start();
            if let Some(captures) = node_found(&matcher, &asked, *only, node, accept.as_deref_mut())
            {
                found.push(Captured {
                    range: target.range(node),
                    captures,
                });
            }
        }
    } else {
        for node in candidates() {
            matcher.next_node();
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
                matcher.start();
                let at = Place {
                    owner: node,
                    from: start,
                    limit: ends[start],
                };
                if let Some((end, captures)) =
                    run_found(&matcher, &asked, at, &available, accept.as_deref_mut())
                {
                    found.push(Captured {
                        range: target.range(*first).start
                            ..target.range(statement(&available, end - 1)).end,
                        captures,
                    });
                }
            }
        }
    }
    found.sort_by_key(|m| (m.range.start, m.range.end));
    found.dedup_by(|later, earlier| later.range == earlier.range);
    #[cfg(test)]
    WORK.with(|work| work.set(matcher.work.get()));
    found
}

/// What a search asks of each match besides its shape, by slot: the names a `where` tests, the
/// names whose captures are kept, and what is bound before the match starts.
struct Asked {
    tested: Vec<usize>,
    kept: Vec<usize>,
    initial: Bindings,
}

impl Asked {
    /// Makes a match again from the same place with `make`, as the way `accepted` binds it, and
    /// gives what `make` returned with the bindings it made.  `accepted` holds the nodes that way
    /// bound the tested names to.  Where the accepted way is not the first the search found, its
    /// captures are found this way.
    ///
    /// Each tested name must first be bound to the very node the way bound it to, while the match
    /// is made again by plain backtracking; what was remembered is forgotten before and after,
    /// since outcomes worked out with pinned names would not hold for the search, nor the
    /// search's for the pinned match.
    fn again<T>(
        &self,
        matcher: &Matcher<'_, '_>,
        accepted: &[NodeId],
        make: impl FnOnce(&mut Bindings) -> T,
    ) -> (T, Bindings) {
        let mut again = self.initial.clone();
        let mut pins = vec![None; again.len()];
        for (&slot, &node) in self.tested.iter().zip(accepted) {
            pins[slot] = Some(node);
        }
        matcher.start();
        matcher.next_node();
        // Plain backtracking: the shortcuts assume a name takes any code equal to what it holds,
        // so one would give up on a later place where only the pinned node stands.
        let remember = matcher.remember.replace(false);
        *matcher.pins.borrow_mut() = pins;
        let outcome = make(&mut again);
        matcher.pins.borrow_mut().clear();
        matcher.remember.set(remember);
        matcher.start();
        matcher.next_node();
        (outcome, again)
    }
}

/// Whether the pattern of one node `only` matches `node`, kept by `accept` if there is one, with
/// the captures `asked` keeps if it does.
fn node_found(
    matcher: &Matcher<'_, '_>,
    asked: &Asked,
    only: NodeId,
    node: NodeId,
    accept: Option<&mut Accept<'_>>,
) -> Option<Vec<Range<usize>>> {
    let mut bindings = asked.initial.clone();
    if !matcher.node(only, node, &mut bindings) {
        return None;
    }
    let Some(accept) = accept else {
        return Some(matcher.captures(&bindings, &asked.kept));
    };
    let mut accepted = None;
    let mut recording = |nodes: &[NodeId]| {
        let yes = accept(nodes);
        if yes {
            accepted = Some(nodes.to_vec());
        }
        yes
    };
    if !matcher.node_accepted(only, node, &bindings, &asked.tested, &mut recording) {
        return None;
    }
    let accepted = accepted.expect("an accepted way was offered");
    if asked.kept.is_empty() || accepted == bound_nodes(&bindings, &asked.tested) {
        return Some(matcher.captures(&bindings, &asked.kept));
    }
    let (found_again, again) =
        asked.again(matcher, &accepted, |again| matcher.node(only, node, again));
    assert!(
        found_again,
        "a node matches again with a way it was accepted with"
    );
    Some(matcher.captures(&again, &asked.kept))
}

/// Where the run of statements the pattern is matches from `at` ends, kept by `accept` if there
/// is one, with the captures `asked` keeps, if it matches.
fn run_found<'t>(
    matcher: &Matcher<'_, 't>,
    asked: &Asked,
    at: Place,
    available: &[Part<'t>],
    accept: Option<&mut Accept<'_>>,
) -> Option<(usize, Vec<Range<usize>>)> {
    let mut bindings = asked.initial.clone();
    let end = matcher.list(Within::Run, at, available, 0, &mut bindings)?;
    let first_way = bound_nodes(&bindings, &asked.tested);
    let Some(accept) = accept else {
        return Some((end, matcher.captures(&bindings, &asked.kept)));
    };
    if accept(&first_way) {
        return Some((end, matcher.captures(&bindings, &asked.kept)));
    }
    let mut accepted = None;
    let mut recording = |nodes: &[NodeId]| {
        let yes = accept(nodes);
        if yes {
            accepted = Some(nodes.to_vec());
        }
        yes
    };
    let end = matcher.run_accepted(at, available, &asked.tested, &first_way, &mut recording)?;
    if asked.kept.is_empty() {
        return Some((end, Vec::new()));
    }
    let accepted = accepted.expect("an accepted way was offered");
    let (again_end, again) = asked.again(matcher, &accepted, |again| {
        matcher.list(Within::Run, at, available, 0, again)
    });
    assert_eq!(
        again_end,
        Some(end),
        "a run matches again, to the same end, with a way it was accepted with"
    );
    Some((end, matcher.captures(&again, &asked.kept)))
}

/// [`find`]'s matches, and the work the search took.
#[cfg(test)]
fn find_counting(pattern: &Pattern, target: &Arena<'_>) -> (Vec<Match>, usize) {
    WORK.with(|work| work.set(0));
    let found = find(pattern, target);
    (found, WORK.with(std::cell::Cell::get))
}

#[cfg(test)]
thread_local! {
    /// The work the last search on this thread took, by `Matcher::work`.
    static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One search made three ways, for the properties that hold captures: without captures, with
/// every name's captured, and with every name's captured by plain backtracking.
#[cfg(test)]
struct CaptureCheck {
    /// The matches found without asking for captures.
    plain: Vec<Range<usize>>,
    /// The work that took.
    plain_work: usize,
    /// The matches and captures found remembering.
    fast: Vec<Captured>,
    /// The work that took.
    fast_work: usize,
    /// The matches and captures found by backtracking alone.
    slow: Vec<Captured>,
    /// The work that took.
    slow_work: usize,
    /// Whether any name is compared - used more than once, or tested - so that a node may be
    /// matched by its ways of binding it, and matched again to record what it captured.
    compared: bool,
}

#[cfg(test)]
impl CaptureCheck {
    /// What asking for captures must leave alone and give: the same captures however the search
    /// runs, the same matches as without them, each capture inside its match and none
    /// overlapping another, since holes never nest; and the same work where no name is compared.
    /// Where one is, a match is matched again to record its captures, by plain backtracking, so
    /// the work is held to the search without captures plus what backtracking alone took, which
    /// includes every such match.
    fn holds(&self) -> Result<(), String> {
        if self.fast != self.slow {
            return Err(format!(
                "remembered {:?}, backtracked {:?}",
                self.fast, self.slow
            ));
        }
        let ranges: Vec<Range<usize>> = self.fast.iter().map(|f| f.range.clone()).collect();
        if ranges != self.plain {
            return Err(format!("captured {ranges:?}, plain {:?}", self.plain));
        }
        for found in &self.fast {
            for (i, capture) in found.captures.iter().enumerate() {
                if capture.start < found.range.start || found.range.end < capture.end {
                    return Err(format!("{capture:?} outside its match {found:?}"));
                }
                if found.captures[..i].iter().any(|other| {
                    other != capture && other.start < capture.end && capture.start < other.end
                }) {
                    return Err(format!("overlapping captures in {found:?}"));
                }
            }
        }
        let work_holds = if self.compared {
            self.fast_work <= self.plain_work + self.slow_work
        } else {
            self.fast_work == self.plain_work
        };
        if !work_holds {
            return Err(format!(
                "work {} with captures, {} without, {} backtracking alone, over {} matches: {:?}",
                self.fast_work,
                self.plain_work,
                self.slow_work,
                self.fast.len(),
                self.fast
            ));
        }
        Ok(())
    }
}

/// [`CaptureCheck`] for `pattern` over `target`, with a `where` testing `tested` if `accept`.
#[cfg(test)]
fn capture_check(
    pattern: &Pattern,
    target: &Arena<'_>,
    tested: &[&str],
    mut accept: Option<&mut Accept<'_>>,
) -> CaptureCheck {
    let mut kept: Vec<&str> = pattern
        .holes()
        .filter_map(|(_, hole)| hole.name.as_deref())
        .collect();
    kept.sort_unstable();
    let used = kept.len();
    kept.dedup();
    let compared = used > kept.len() || !tested.is_empty();
    let search = |remember: bool, kept: &[&str], accept: Option<&mut Accept<'_>>| {
        let found = find_in(pattern, target, remember, tested, accept, None, &[], kept);
        (found, WORK.with(std::cell::Cell::get))
    };
    let (plain, plain_work) = search(true, &[], accept.as_deref_mut());
    let (fast, fast_work) = search(true, &kept, accept.as_deref_mut());
    let (slow, slow_work) = search(false, &kept, accept);
    CaptureCheck {
        plain: plain.into_iter().map(|found| found.range).collect(),
        plain_work,
        fast,
        fast_work,
        slow,
        slow_work,
        compared,
    }
}

/// The node bound in each of `slots`, in order: each a name a `where` tests, which the pattern
/// binds to one node, so a match has bound it to one.
fn bound_nodes(bindings: &Bindings, slots: &[usize]) -> Vec<NodeId> {
    slots
        .iter()
        .map(|&slot| match &bindings[slot] {
            Some(Bound::Node(node)) => *node,
            other => unreachable!("a tested name is bound to one node by a match, not {other:?}"),
        })
        .collect()
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
    /// A state of the search that depends on more than two places where it can go more than one
    /// way, each multiplying its work by the code's length ([`Pattern::costly`]): the last
    /// sequence whose stop it depends on.
    ThirdPlace(NodeId),
}

impl Pattern {
    /// Whether matching this pattern could cost more than the square of the length of the code
    /// it is matched in, and why.
    ///
    /// The matcher remembers each state of a list's search - a part of the pattern, a place in
    /// the code, and what the names the rest reads hold - and works each out once ([`List`]), so
    /// its work is the number of states it makes.  This counts them as it makes them ([`Cost`]).
    /// Each state depends on some of the places where a search can go more than one way: a
    /// sequence's stop, and where a run of statements starts.  It takes as many forms as the
    /// code's length to the power of how many, and a pattern is costly where some state
    /// depends on more than two, counting a sequence's own stop where it tries each.
    ///
    /// What a state depends on follows the matcher.  A place stands a fixed way from the last
    /// stop before it; a single hole binds where it stands, and a sequence from there to its
    /// stop.  A node's list starts where the node stands, and since a node matches its whole
    /// list, a sequence with no free sequence after it in a node's list ends where the list
    /// does.  A sequence that repeats an earlier binding takes that binding's length.  A plain
    /// search stops a sequence whose name nothing else reads at the first place the code after it
    /// stands, when that settles the rest; over a run of statements it works out again, from
    /// every start, what it worked out with a name bound.  A `where` gathers every way of binding
    /// what it tests, carrying each to the end.  A place in the code, or a node, is one of as
    /// many as the code's length however many stops it depends on, a run of code one of the
    /// square; and a stop inside a node settles where the node stands, where that is a fixed way
    /// from what put it there.
    ///
    /// So `f(g($a..., x, $a...), h($b..., y, $b...))` is accepted, each list worked out once
    /// whatever the other took, and so is `f($a..., $b...) + g($a..., x, $b...)`, whose one stop
    /// divides `f`'s arguments; three stops whose bindings a later list reads together are not.
    ///
    /// Every language's compiler refuses a pattern this names, and [`find`] panics on one, as a
    /// defect in its compiler.  Two sequences standing together in a run of statements are
    /// refused too: matching them costs nothing more, but nothing settles how the statements
    /// divide between them.
    #[must_use]
    pub fn costly(&self) -> Option<Costly> {
        self.costly_with(&[])
    }

    /// [`Pattern::costly`], with the names in `tested` also read once a match is complete, as a
    /// `where` that tests them reads them: its search then gathers every way of binding them,
    /// and may cost more where the pattern alone does not.  [`find_where`] panics on a pattern and
    /// names this names, as a defect in its caller.
    #[must_use]
    pub fn costly_with(&self, tested: &[&str]) -> Option<Costly> {
        let shape = self.arena();
        let root = shape.root();
        let run = shape.children(root).nth(1).is_some();
        let wanted = if run {
            let run = between_first_and_last(&parts(shape, root));
            if let Some(second) = together(self, &run) {
                return Some(Costly::Together(second));
            }
            run
        } else {
            parts(shape, root)
        };
        // A run starts anywhere; a node's parts stand where the node does, a different node each
        // time, so that what one node costs only adds to what the others do.
        let at = if run {
            Free::from([START])
        } else {
            Free::new()
        };
        // A search first finds a match plainly, and a `where` then gathers every way of binding
        // what it tests, each costing what it costs.
        let mut plain = Cost::new(self, tested, run);
        plain.list(&Walk {
            owner: root,
            wanted: &wanted,
            whole: false,
            at: &at,
            exact: true,
            carried: &[],
            plain: true,
        });
        if let Some(found) = plain.found {
            return Some(Costly::ThirdPlace(found));
        }
        if !tested.is_empty() {
            let mut gathering = Cost::new(self, tested, run);
            gathering.list(&Walk {
                owner: root,
                wanted: &wanted,
                whole: false,
                at: &at,
                exact: true,
                carried: tested,
                plain: false,
            });
            if let Some(found) = gathering.found {
                return Some(Costly::ThirdPlace(found));
            }
        }
        None
    }
}

/// Whether `hole` is a sequence whose name an earlier hole binds, so that it is compared with
/// what that hole took rather than taking code of its own.
fn compared(pattern: &Pattern, hole: NodeId) -> bool {
    let Some(name) = pattern
        .hole(hole)
        .filter(|spelled| spelled.sequence)
        .and_then(|spelled| spelled.name.as_deref())
    else {
        return false;
    };
    // Holes come in pre-order, so the first with a name is where it is bound.
    pattern
        .holes()
        .find(|(_, spelled)| spelled.name.as_deref() == Some(name))
        .is_some_and(|(first, _)| first != hole)
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

/// The names a node of a pattern can bind in more than one way and that are used again after
/// it, each with the hole that first binds it: names first bound within the node and used by a
/// hole after it, if the node holds a sequence, without which every name in it binds one way.
/// A match of such a node is not final: where what follows fails on one of these names, the
/// node is asked for its next way of binding them ([`Matcher::node_ways`]).  A name in `tested`
/// is used after every node, since a `where` reads it once the match is complete.
fn exported<'p>(pattern: &'p Pattern, node: NodeId, tested: &[&str]) -> Vec<(&'p str, NodeId)> {
    let shape = pattern.arena();
    let outer = shape.range(node);
    // A descendant comes after its node in pre-order and lies within it; a hole that wraps the
    // node with the same range, as a statement holes its expression, lies within it but before.
    let inside = |n: NodeId| {
        let range = shape.range(n);
        n >= node && outer.start <= range.start && range.end <= outer.end
    };
    if pattern.hole(node).is_some()
        || !pattern
            .holes()
            .any(|(hole, spelled)| spelled.sequence && inside(hole))
    {
        return Vec::new();
    }
    // Holes come in pre-order, so the first seen of each name is where it is first bound.
    let mut first: BTreeMap<&str, NodeId> = BTreeMap::new();
    for (hole, spelled) in pattern.holes() {
        if let Some(name) = &spelled.name {
            first.entry(name).or_insert(hole);
        }
    }
    first
        .into_iter()
        .filter(|&(name, hole)| {
            inside(hole)
                && (tested.contains(&name)
                    || pattern.holes().any(|(later, spelled)| {
                        spelled.name.as_deref() == Some(name)
                            && shape.range(later).start >= outer.end
                    }))
        })
        .collect()
}

/// The places a search's work depends on, as [`Cost`] counts them: each a sequence's stop, by
/// its hole, or [`START`], where a run of statements starts.  A state of the search that depends
/// on `k` of them is worked out for as many as the list's length to the `k`th.
type Free = BTreeSet<u32>;

/// Where a run of statements starts, which a search tries every place for: see [`Free`].
const START: u32 = u32::MAX;

/// Where a shared sequence's state stands, apart from the stops its ways range over: see
/// [`Cost::list`].
const HERE: u32 = u32::MAX - 1;

/// How many times over a search works out each state of a pattern's lists, as
/// [`Pattern::costly`] judges it: see there.  It walks the lists as the matcher does, in order,
/// working out what each binding and each place in the code depends on, and finds the first
/// state that depends on more than two of [`Free`]'s places once its own sequence's stop is
/// counted.
struct Cost<'p> {
    pattern: &'p Pattern,
    /// The names a `where` tests, which its search carries to the end of every way.
    tested: &'p [&'p str],
    /// The names the matcher compares: used more than once, or tested.
    slots: BTreeSet<&'p str>,
    /// Each name's first hole, which binds it.
    first: BTreeMap<&'p str, NodeId>,
    /// What each first hole's binding depends on.
    bound: HashMap<NodeId, Free>,
    /// Whether the pattern is a run of statements, whose search starts at every place.
    run: bool,
    /// For each stop inside a node, the places that put the node where it stands: a place in a
    /// node's code is that node's, so it settles them too, and a state counts them once.
    implies: HashMap<u32, Free>,
    /// The hole or node at the first state found to cost more than the square.
    found: Option<NodeId>,
}

impl<'p> Cost<'p> {
    fn new(pattern: &'p Pattern, tested: &'p [&'p str], run: bool) -> Cost<'p> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        let mut first = BTreeMap::new();
        // Holes come in pre-order, which is the order a search binds them in.
        for (hole, spelled) in pattern.holes() {
            if let Some(name) = spelled.name.as_deref() {
                *counts.entry(name).or_default() += 1;
                first.entry(name).or_insert(hole);
            }
        }
        let slots = counts
            .into_iter()
            .filter(|(name, count)| *count > 1 || tested.contains(name))
            .map(|(name, _)| name)
            .collect();
        Cost {
            pattern,
            tested,
            slots,
            first,
            bound: HashMap::new(),
            run,
            implies: HashMap::new(),
            found: None,
        }
    }

    /// The names the matcher compares that holes within `part` use, itself included.
    fn names_in(&self, part: &Part<'_>) -> BTreeSet<&'p str> {
        let Part::Child(node) = part else {
            return BTreeSet::new();
        };
        let shape = self.pattern.arena();
        let outer = shape.range(*node);
        self.pattern
            .holes()
            .filter(|&(hole, _)| {
                let range = shape.range(hole);
                hole >= *node && outer.start <= range.start && range.end <= outer.end
            })
            .filter_map(|(_, spelled)| spelled.name.as_deref())
            .filter(|name| self.slots.contains(name))
            .collect()
    }

    /// What a name's binding depends on, once its first hole has been walked.
    fn depends(&self, name: &str) -> Free {
        self.first
            .get(name)
            .and_then(|hole| self.bound.get(hole))
            .cloned()
            .unwrap_or_default()
    }

    /// How many places' worth of values a name's binding can take at most: a node one, a run of
    /// code two.
    fn cap(&self, name: &str) -> usize {
        let sequence = self
            .first
            .get(name)
            .and_then(|hole| self.pattern.hole(*hole))
            .is_some_and(|hole| hole.sequence);
        if sequence { 2 } else { 1 }
    }

    fn free_sequence(&self, part: &Part<'_>) -> bool {
        matches!(part, Part::Child(child) if is_sequence(self.pattern, part) && !compared(self.pattern, *child))
    }

    /// Whether a plain search settles the sequence at `i` at the first place the fixed code
    /// after it stands, as the matcher's [`List`] works it out: then it tries no other.
    fn settled(&self, wanted: &[Part<'_>], whole: bool, i: usize) -> bool {
        let len = wanted.len();
        if !is_sequence(self.pattern, &wanted[i]) || i + 1 == len {
            return false;
        }
        // A name used again binds other code at each length, so each is tried.
        if let Part::Child(child) = wanted[i]
            && self
                .pattern
                .hole(child)
                .and_then(|hole| hole.name.as_deref())
                .is_some_and(|name| self.slots.contains(name))
        {
            return false;
        }
        let end = (i + 1..len)
            .find(|&j| is_sequence(self.pattern, &wanted[j]))
            .unwrap_or(len);
        if end == i + 1 || (end == len && whole) {
            return false;
        }
        if let Some(Part::Child(next)) = wanted.get(end)
            && self
                .pattern
                .hole(*next)
                .and_then(|hole| hole.name.as_deref())
                .is_some_and(|name| self.slots.contains(name))
        {
            return false;
        }
        let later: BTreeSet<&str> = wanted[end..]
            .iter()
            .flat_map(|part| self.names_in(part))
            .collect();
        !wanted[i + 1..end]
            .iter()
            .flat_map(|part| self.names_in(part))
            .any(|name| later.contains(name))
    }

    /// Walks a list, then counts its states.
    fn list(&mut self, walk: &Walk<'_, '_>) {
        let walked = self.walk(walk);
        self.count(walk, &walked);
    }

    /// Walks a list's parts in order, as a search does, working out where each state stands and
    /// what each binding depends on, and walking each node's own list where it stands.
    fn walk(&mut self, walk: &Walk<'_, '_>) -> Walked {
        let shape = self.pattern.arena();
        let len = walk.wanted.len();
        let mut walked = Walked {
            places: Vec::with_capacity(len + 1),
            stops: vec![None; len],
            ways: vec![None; len],
            shared: vec![false; len],
        };
        let mut here = (walk.at.clone(), walk.exact);
        for i in 0..len {
            walked.places.push(here.0.clone());
            let Part::Child(child) = walk.wanted[i] else {
                continue;
            };
            match self.pattern.hole(child) {
                Some(hole) if hole.sequence => {
                    if compared(self.pattern, child) {
                        // It takes the length of the code it repeats, wherever that came from,
                        // and so no longer gives back where it stood.
                        let name = hole.name.as_deref().expect("a compared hole has a name");
                        here.0.extend(self.depends(name));
                        here.1 = false;
                    } else {
                        here = self.sequence(walk, i, child, hole, &here, &mut walked);
                    }
                }
                Some(_) => {
                    self.bound.insert(child, here.0.clone());
                }
                None => {
                    // A plain search matches a pattern of one node plainly, and only a `where`
                    // then gathers its ways; a node inside gathers them for what it exports in
                    // either search, as its list records them.
                    let top = walk.owner == shape.root() && !self.run && walk.plain;
                    let exports: Vec<&'p str> = if top {
                        Vec::new()
                    } else {
                        exported(self.pattern, child, self.tested)
                            .into_iter()
                            .map(|(name, _)| name)
                            .filter(|name| self.slots.contains(name))
                            .collect()
                    };
                    let parts = parts(shape, child);
                    self.list(&Walk {
                        owner: child,
                        wanted: &parts,
                        whole: true,
                        at: &here.0,
                        exact: here.1,
                        carried: &exports,
                        plain: exports.is_empty(),
                    });
                    if !exports.is_empty() {
                        let free: Free =
                            exports.iter().flat_map(|name| self.depends(name)).collect();
                        let cap = exports.iter().map(|name| self.cap(name)).sum();
                        walked.ways[i] = Some((free, cap));
                    }
                }
            }
        }
        walked.places.push(here.0);
        walked
    }

    /// The sequence `child` at `i` of a list, standing `here`: records what it binds and the
    /// stop it tries, and says where the walk stands after it, and whether that is exact.
    fn sequence(
        &mut self,
        walk: &Walk<'_, '_>,
        i: usize,
        child: NodeId,
        hole: &Hole,
        here: &(Free, bool),
        walked: &mut Walked,
    ) -> (Free, bool) {
        let shape = self.pattern.arena();
        let wanted = walk.wanted;
        let settled = walk.plain && self.settled(wanted, walk.whole, i);
        // A node's parts all match, so a stop with no sequence of its own after it is wherever
        // the code after it, which ends where the node does, starts.
        let pinned = walk.whole && !wanted[i + 1..].iter().any(|part| self.free_sequence(part));
        // Gathering ways, a sequence whose take nothing reads or carries, and whose ways more
        // than one place asks for, has them from the next place's ([`Matcher::ways_sequence`]):
        // its states are one for each place it could stop and what the rest reads, wherever it
        // was reached from.
        walked.shared[i] = !walk.plain
            && (walk.owner == shape.root() && self.run
                || wanted[..i].iter().any(|part| self.free_sequence(part)))
            && hole.name.as_deref().is_none_or(|name| {
                !self.slots.contains(name)
                    || !walk.carried.contains(&name)
                        && !wanted[i + 1..]
                            .iter()
                            .any(|part| self.names_in(part).contains(name))
            });
        if !settled {
            walked.stops[i] = Some(child.0);
            // A place inside a node settles where the node stands, and so the places that put
            // it there where it stands a fixed way from them.
            if walk.whole && walk.exact {
                let mut settles = walk.at.clone();
                for place in walk.at {
                    settles.extend(self.implies.get(place).into_iter().flatten());
                }
                self.implies.insert(child.0, settles);
            }
        }
        let after = if settled {
            (here.0.clone(), false)
        } else if pinned {
            (walk.at.clone(), walk.exact)
        } else {
            (Free::from([child.0]), true)
        };
        let mut binding = here.0.clone();
        binding.extend(after.0.iter().copied());
        self.bound.insert(child, binding);
        after
    }

    /// Counts each state of a walked list: where it stands, what the names it reads were bound
    /// to before it, and what the ways it gathers bind from it on.
    fn count(&mut self, walk: &Walk<'_, '_>, walked: &Walked) {
        let shape = self.pattern.arena();
        let wanted = walk.wanted;
        for i in 0..=wanted.len() {
            if self.found.is_some() {
                return;
            }
            let later: BTreeSet<&'p str> = wanted[i..]
                .iter()
                .flat_map(|part| self.names_in(part))
                .collect();
            let bound_later: BTreeSet<NodeId> = wanted[i..]
                .iter()
                .filter_map(|part| match part {
                    Part::Child(child) => Some(*child),
                    Part::Token(_) => None,
                })
                .flat_map(|child| {
                    let outer = shape.range(child);
                    self.first
                        .values()
                        .copied()
                        .filter(move |&hole| {
                            let range = shape.range(hole);
                            hole >= child && outer.start <= range.start && range.end <= outer.end
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            let shared = walked.shared.get(i).copied().unwrap_or(false);
            // Each thing the state is worked out by, with what it depends on and how many
            // places' worth of values it can take at most: a place in the code or a node one, a
            // run of code two, its start and its end.  A shared sequence's state is where it
            // stands among the places it could stop, a place of its own whatever reached it;
            // the ways it holds are those from every later stop, so its stop counts only through
            // what those ways carry.
            let place = if shared {
                Free::from([HERE])
            } else {
                walked.places[i].clone()
            };
            let mut state: Vec<(Free, usize)> = vec![(place, 1)];
            let mut reads = false;
            for name in &later {
                if !bound_later.contains(&self.first[name]) {
                    state.push((self.depends(name), self.cap(name)));
                    reads = true;
                }
            }
            for name in walk.carried {
                if bound_later.contains(&self.first[name]) {
                    state.push((self.depends(name), self.cap(name)));
                }
            }
            // A plain search forgets what it worked out with a name bound at each start, so a
            // run's states that read one are worked out again from every start.
            if self.run && walk.plain && reads {
                state.push((Free::from([START]), 1));
            }
            if let Some(stop) = walked.stops.get(i).copied().flatten()
                && !shared
            {
                state.push((Free::from([stop]), 1));
            }
            if let Some(Some(gathered)) = walked.ways.get(i) {
                state.push(gathered.clone());
            }
            self.judge(&state);
        }
    }

    /// Records a state that costs more than the square, if this one does.
    fn judge(&mut self, state: &[(Free, usize)]) {
        let all: Free = state
            .iter()
            .flat_map(|(free, _)| free.iter().copied())
            .collect();
        let settled: Free = all
            .iter()
            .flat_map(|place| self.implies.get(place).into_iter().flatten())
            .copied()
            .collect();
        let free: Free = all.difference(&settled).copied().collect();
        let most: usize = state
            .iter()
            .filter(|(free, _)| !free.is_empty())
            .map(|(_, cap)| cap)
            .sum();
        if free.len().min(most) > 2 {
            // Named by the last sequence whose stop the state depends on, a hole of the
            // pattern's: it depends on at least two besides where a run starts.
            let last = free
                .iter()
                .rev()
                .find(|&&free| free != START && free != HERE);
            self.found = Some(NodeId(*last.expect("a costly state depends on a stop")));
        }
    }
}

/// A list as [`Cost::list`] walks it: whose it is, its parts, whether a match takes all of them,
/// where it stands, and how it is searched.  With `exact`, where the list stands is a fixed way
/// from each place `at` names, so that it gives them back; a plain search finds one way, and
/// one that gathers every way of binding `carried` carries them to the end.
struct Walk<'w, 't> {
    owner: NodeId,
    wanted: &'w [Part<'t>],
    whole: bool,
    at: &'w Free,
    exact: bool,
    carried: &'w [&'w str],
    plain: bool,
}

/// What walking a list found of each of its states: where it stands, the stop it tries, what
/// the ways a node there is asked for bind, and whether its sequence's ways are shared.
struct Walked {
    places: Vec<Free>,
    stops: Vec<Option<u32>>,
    ways: Vec<Option<(Free, usize)>>,
    shared: Vec<bool>,
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
/// Three further steps skip what cannot match.  A sequence stops only where the fixed code after
/// it could stand, found once per list as a match would find it but with every hole matching
/// anything, and a node holding a sequence judged by its kind alone - what any binding could let
/// it match, and no less.  A sequence gives up at once when some later fixed
/// code could stand nowhere after it.  And where the fixed code stands but the rest fails after
/// it, the rest fails after every later place too - whatever stands after a later place also
/// stands after this one - unless the place changes what the rest is matched with: when the
/// fixed code binds a name used later, when the sequence after it is named again, when the list
/// must end where the code does, with no sequence after it to take the difference, when another
/// list follows, which a later place could let the next sequence start in, or when what follows
/// the code here is a token, which the next sequence cannot start on.  Only there are later
/// places tried.
///
/// Outcomes that depend on what names are bound are forgotten at each start - each node a
/// pattern of one node is tried at, each start of a run - since another start binds them to
/// other code; those that depend on no binding are kept for the whole search.
///
/// **What it costs.**  Each state is worked out once, so a list costs about the number of
/// states: its length times the pattern's, times the list's length again for each place the
/// state depends on where the search can go more than one way ([`Pattern::costly`]).  A
/// compiler refuses a pattern with a state that depends on a third.
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
    /// Where each run of fixed code after a sequence starts.
    segments: Vec<usize>,
    /// For each part, the slots of the names it can bind in more than one way that are used
    /// after it ([`exported`]): empty for most, whose first match is final.
    exports: Vec<Vec<usize>>,
}

/// A pattern and the arena it is matched against, with what has been worked out so far.
struct Matcher<'p, 't> {
    shape: &'p Arena<'static>,
    pattern: &'p Pattern,
    target: &'t Arena<'t>,
    remember: std::cell::Cell<bool>,
    /// Each name used more than once, that a `where` tests, or whose capture is asked for,
    /// numbered.
    slots: BTreeMap<&'p str, usize>,
    /// For each slot, whether its name is compared - used more than once, or tested - rather
    /// than only kept for its capture.  A name only kept never steers the search: it is
    /// recorded where the search went, so asking for captures costs nothing in how the search
    /// runs and changes none of the matches it finds.
    compared: Vec<bool>,
    /// For each slot, whether its capture is asked for.
    asked: Vec<bool>,
    /// For each slot, the very node its name must first be bound to, if any: set while a match is
    /// made again as the way a `where` accepted, so that the search reproduces that way rather
    /// than one binding an earlier node with equal code ([`Asked::again`]).  Later uses of the
    /// name compare by code with that node, as ever.
    pins: RefCell<Vec<Option<NodeId>>>,
    /// The names a `where` tests, which it reads once a match is complete.
    tested: Vec<&'p str>,
    /// Each list of the pattern's, by node, the run of statements last.
    lists: RefCell<Vec<Option<Rc<List<'p>>>>>,
    parts: RefCell<HashMap<NodeId, Rc<[Part<'t>]>>>,
    ends: RefCell<HashMap<NodeId, Rc<[usize]>>>,
    codes: RefCell<HashMap<NodeId, Rc<Codes>>>,
    /// Where the fixed code starting at a list's part stands among a node's parts.
    stands: RefCell<HashMap<Stands, Rc<[usize]>>>,
    /// The outcome of matching a list's rest from a pair of places, with the names it uses
    /// bound as they were: where the match ended, and what it bound.  Those with no name bound
    /// are kept for the whole search, and those with one only for the start they were found at.
    outcomes: RefCell<HashMap<Remembered, Outcome>>,
    bound_outcomes: RefCell<HashMap<Remembered, Outcome>>,
    /// Every way a list's rest matches from a pair of places, by what the names it reads hold
    /// and which names' bindings were asked for ([`Matcher::ways`]): kept for every start in one
    /// node's parts, since none depends on where the match started.
    ways_seen: RefCell<HashMap<WaysAt, Rc<[Way]>>>,
    /// For each pair of nodes whose parts' code a name has been compared between, by the node
    /// it was bound in and the node it was compared in: nothing once compared, and the two
    /// nodes' code read together once compared again ([`Matcher::equal_run`]).
    joints: RefCell<Joints>,
    /// How many steps and pieces of compared code the search has taken, for tests that hold
    /// its growth.
    #[cfg(test)]
    work: std::cell::Cell<usize>,
}

/// A list's part where fixed code starts, and the node among whose parts it is looked for.
type Stands = (Within, usize, NodeId);

/// For each pair of nodes compared, nothing once compared and their code read together once
/// compared again: see [`Matcher::equal_run`].
type Joints = HashMap<(NodeId, NodeId), Option<Rc<Joint>>>;

/// Two nodes' code read together, as [`Matcher::joint`] makes it: how far any two places in it
/// agree, and where the second node's code starts.
struct Joint {
    common: Common,
    second: usize,
}

/// One way a list's rest matches, as [`Matcher::ways`] finds it: what it binds each name asked
/// for - only those it binds itself, from where the rest starts on, and `None` for the others -
/// and where among the list's code it ends.
type Way = (Vec<Option<Bound>>, usize);

/// Where [`Matcher::ways`] worked out a list's rest, and what that depends on: the list, the
/// pair of places, the names whose bindings were asked for, and what the names the rest reads
/// hold.  Nothing else changes the ways found, so a rest reached again - by another way of
/// binding an earlier name, or from another start - is never worked out twice.
#[derive(Debug, PartialEq, Eq, Hash)]
struct WaysAt {
    within: Within,
    place: Place,
    at: usize,
    exports: Vec<usize>,
    bound: Vec<Option<Key>>,
}

/// Where [`Matcher::ways`] works out `list`'s rest from `at` and `place`, for the names
/// `exports` holds, with `bindings` as they stand: see [`WaysAt`].
fn ways_at(
    list: &List<'_>,
    within: Within,
    place: Place,
    at: usize,
    exports: &[usize],
    bindings: &Bindings,
) -> WaysAt {
    WaysAt {
        within,
        place,
        at,
        exports: exports.to_vec(),
        bound: list.uses[at]
            .iter()
            .map(|&slot| bindings[slot].as_ref().map(Bound::key))
            .collect(),
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct Remembered {
    within: Within,
    place: Place,
    at: usize,
    bound: Vec<Option<Key>>,
}

type Outcome = Option<(usize, Vec<(usize, Bound)>)>;

impl<'p, 't> Matcher<'p, 't> {
    fn new(
        pattern: &'p Pattern,
        target: &'t Arena<'_>,
        remember: bool,
        tested: &[&str],
        kept: &[&str],
    ) -> Matcher<'p, 't> {
        let mut counts: BTreeMap<&'p str, usize> = BTreeMap::new();
        for (_, hole) in pattern.holes() {
            if let Some(name) = &hole.name {
                *counts.entry(name).or_default() += 1;
            }
        }
        let tested: Vec<&'p str> = counts
            .keys()
            .copied()
            .filter(|name| tested.contains(name))
            .collect();
        let named: Vec<(&'p str, bool)> = counts
            .into_iter()
            .filter_map(|(name, count)| {
                let compared = count > 1 || tested.contains(&name);
                (compared || kept.contains(&name)).then_some((name, compared))
            })
            .collect();
        let compared = named.iter().map(|&(_, compared)| compared).collect();
        let asked = named.iter().map(|(name, _)| kept.contains(name)).collect();
        let slots = named
            .into_iter()
            .enumerate()
            .map(|(slot, (name, _))| (name, slot))
            .collect();
        Matcher {
            shape: pattern.arena(),
            pattern,
            target,
            remember: std::cell::Cell::new(remember),
            slots,
            compared,
            asked,
            pins: RefCell::default(),
            tested,
            lists: RefCell::new(vec![None; pattern.arena().nodes().count() + 1]),
            parts: RefCell::default(),
            ends: RefCell::default(),
            codes: RefCell::default(),
            stands: RefCell::default(),
            outcomes: RefCell::default(),
            bound_outcomes: RefCell::default(),
            ways_seen: RefCell::default(),
            joints: RefCell::default(),
            #[cfg(test)]
            work: std::cell::Cell::new(0),
        }
    }

    /// Whether some way `wanted` matches `node` binds the tested names, in `slots`, as `accept`
    /// accepts: the way `first` bound them, which the search found first, and then each other.
    fn node_accepted(
        &self,
        wanted: NodeId,
        node: NodeId,
        first: &Bindings,
        slots: &[usize],
        accept: &mut Accept<'_>,
    ) -> bool {
        let first = bound_nodes(first, slots);
        if accept(&first) {
            return true;
        }
        // The first way is among the ways too, and it has been answered.
        self.node_ways(wanted, node, &self.unbound(), slots)
            .into_iter()
            .any(|way| {
                let mut bindings = self.unbound();
                for (slot, bound) in way {
                    bindings[slot] = Some(bound);
                }
                let nodes = bound_nodes(&bindings, slots);
                nodes != first && accept(&nodes)
            })
    }

    /// Where the first way the run of statements matches from `at` ends that binds the tested
    /// names, in `slots`, as `accept` accepts, if one does.
    fn run_accepted(
        &self,
        at: Place,
        available: &[Part<'t>],
        slots: &[usize],
        first: &[NodeId],
        accept: &mut Accept<'_>,
    ) -> Option<usize> {
        let found = self.ways(Within::Run, at, available, 0, self.unbound(), slots);
        found.iter().find_map(|(way, end)| {
            let mut bindings = self.unbound();
            for (&slot, bound) in slots.iter().zip(way) {
                bindings[slot].clone_from(bound);
            }
            // The first way has been answered already.
            let nodes = bound_nodes(&bindings, slots);
            (nodes != first && accept(&nodes)).then_some(*end)
        })
    }

    /// Forgets what depended on the bindings of the last start, before another.
    fn start(&self) {
        self.bound_outcomes.borrow_mut().clear();
    }

    /// Forgets the ways worked out among the last node's parts, before another's: they serve
    /// every start among one node's parts, and are kept no longer.
    fn next_node(&self) {
        self.ways_seen.borrow_mut().clear();
    }

    fn unbound(&self) -> Bindings {
        vec![None; self.slots.len()]
    }

    /// The byte range of the code bound in each of `slots`, in order: each a name a complete
    /// match has bound.
    fn captures(&self, bindings: &Bindings, slots: &[usize]) -> Vec<Range<usize>> {
        let part_range = |part: &Part<'_>| match part {
            Part::Child(child) => self.target.range(*child),
            Part::Token(token) => self.target.token_range(token),
        };
        slots
            .iter()
            .map(|&slot| match &bindings[slot] {
                Some(Bound::Node(node)) => self.target.range(*node),
                Some(Bound::Run(owner, parts)) => {
                    let parts = &self.parts_of(*owner)[parts.clone()];
                    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
                        unreachable!("a sequence binds one node or more")
                    };
                    part_range(first).start..part_range(last).end
                }
                None => unreachable!("a complete match binds every name its pattern has"),
            })
            .collect()
    }

    fn slot(&self, hole: &Hole) -> Option<usize> {
        hole.name
            .as_deref()
            .and_then(|name| self.slots.get(name).copied())
    }

    /// Records the run from `place` to `end` as what the sequence `hole` captured, if its name
    /// is only kept: one compared is bound by the search itself.
    fn keep(&self, hole: &Hole, place: Place, end: usize, bindings: &mut Bindings) {
        if let Some(slot) = self.slot(hole).filter(|&slot| !self.compared[slot]) {
            bindings[slot] = Some(Bound::Run(place.owner, place.from..end));
        }
    }

    /// Records what the names asked for within `wanted` captured, where `wanted` matched `node`
    /// by a way of binding the names used after it ([`Matcher::node_ways`]), which binds only
    /// those: a name only kept, and a name compared only inside the node, are left unbound.  The
    /// node is matched again with the way's bindings, and only those names are taken from it, so
    /// that nothing the rest is matched with changes.
    ///
    /// A name compared, first used inside the node and bound by the way to a node, must be bound
    /// to that very node when the node is matched again: compared only by code, it could bind an
    /// earlier node with equal code, and the names kept would then be the captures of another
    /// way.  So it is pinned there, as [`Asked::again`] pins a tested name, and the node is
    /// matched again by plain backtracking.
    fn keep_within(&self, wanted: NodeId, node: NodeId, bindings: &mut Bindings) {
        // A name used more than once inside the node is listed once for each use.
        let mut kept: Vec<usize> = self
            .slots_in(&Part::Child(wanted))
            .into_iter()
            .filter(|&slot| self.asked[slot] && bindings[slot].is_none())
            .collect();
        kept.sort_unstable();
        kept.dedup();
        if kept.is_empty() {
            return;
        }
        let mut again = bindings.clone();
        let firsts = self.first_used_within(wanted, bindings);
        let found = if firsts.is_empty() {
            self.node(wanted, node, &mut again)
        } else {
            let mut pins = self.pins.borrow().clone();
            pins.resize(again.len(), None);
            for &(slot, bound) in &firsts {
                again[slot] = None;
                pins[slot] = Some(bound);
            }
            // As for [`Asked::again`]: what was worked out without these pins would not hold
            // with them, nor theirs for the search, so it is forgotten before and after.
            let remember = self.remember.replace(false);
            let outer = self.pins.replace(pins);
            self.start();
            self.next_node();
            let found = self.node(wanted, node, &mut again);
            *self.pins.borrow_mut() = outer;
            self.remember.set(remember);
            self.start();
            self.next_node();
            found
        };
        assert!(
            found,
            "a node matches again with a way it was found to bind"
        );
        for slot in kept {
            bindings[slot].clone_from(&again[slot]);
        }
    }

    /// Each slot compared whose name is first used inside `wanted`, with the node `bindings` bind
    /// it to there, where it is bound to one node.
    fn first_used_within(&self, wanted: NodeId, bindings: &Bindings) -> Vec<(usize, NodeId)> {
        let outer = self.shape.range(wanted);
        let mut firsts = Vec::new();
        let mut seen = Vec::new();
        // Holes come in pre-order, so each name's first is the first met.
        for (hole_node, hole) in self.pattern.holes() {
            let Some(slot) = self.slot(hole) else {
                continue;
            };
            if seen.contains(&slot) {
                continue;
            }
            seen.push(slot);
            let range = self.shape.range(hole_node);
            if self.compared[slot]
                && outer.start <= range.start
                && range.end <= outer.end
                && let Some(Bound::Node(bound)) = &bindings[slot]
            {
                firsts.push((slot, *bound));
            }
        }
        firsts
    }

    /// The slot of `hole`'s name if the name is compared, not only kept: the only slots that
    /// may change how the search runs.
    fn compared_slot(&self, hole: &Hole) -> Option<usize> {
        self.slot(hole).filter(|&slot| self.compared[slot])
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
                        .and_then(|hole| self.compared_slot(hole))
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
        let segments = (1..len)
            .filter(|&i| sequence(&wanted[i - 1]) && !sequence(&wanted[i]))
            .collect();
        let exports = wanted
            .iter()
            .map(|part| match part {
                Part::Child(child) => exported(self.pattern, *child, &self.tested)
                    .into_iter()
                    .filter_map(|(name, _)| self.slots.get(name).copied())
                    .collect(),
                Part::Token(_) => Vec::new(),
            })
            .collect();
        let made = Rc::new(List {
            exports,
            segments,
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

    /// Each way `wanted` matches `node` that binds the names `exports` holds differently, in
    /// order of preference - the order backtracking finds them in - as what each binds them to.
    fn node_ways(
        &self,
        wanted: NodeId,
        node: NodeId,
        bindings: &Bindings,
        exports: &[usize],
    ) -> Vec<Vec<(usize, Bound)>> {
        if self.shape.kind(wanted) != self.target.kind(node) || self.shape.value(wanted).is_some() {
            // A node with a value holds no hole, so it has no names to export.
            return Vec::new();
        }
        let available = parts(self.target, node);
        let place = Place {
            owner: node,
            from: 0,
            limit: available.len(),
        };
        self.ways(
            Within::Node(wanted),
            place,
            &available,
            0,
            bindings.clone(),
            exports,
        )
        .iter()
        .map(|(way, _)| {
            way.iter()
                .zip(exports)
                .filter_map(|(bound, &slot)| bound.clone().map(|bound| (slot, bound)))
                .collect()
        })
        .collect()
    }

    /// Every way the list's parts from `at` onwards match the code from `place.from` that binds
    /// the names `exports` holds differently, in order of preference - the order backtracking
    /// finds them in - each with what the rest binds them to and where it ends: none if it does
    /// not match.  Each place is worked out once for what the names it reads hold, as
    /// [`Matcher::list`] works out one, and never for what was bound before it that it does not
    /// read, so that the ways after a name is bound are found once, however many ways it has.
    fn ways(
        &self,
        within: Within,
        place: Place,
        available: &[Part<'t>],
        at: usize,
        bindings: Bindings,
        exports: &[usize],
    ) -> Rc<[Way]> {
        let list = self.list_of(within);
        let key = self
            .remember
            .get()
            .then(|| ways_at(&list, within, place, at, exports, &bindings));
        if let Some(key) = &key
            && let Some(found) = self.ways_seen.borrow().get(key)
        {
            return Rc::clone(found);
        }
        let found: Rc<[Way]> = self
            .ways_step(&list, within, place, available, at, bindings, exports)
            .into();
        if let Some(key) = key {
            self.ways_seen.borrow_mut().insert(key, Rc::clone(&found));
        }
        found
    }

    /// Adds to `out` each way in `rest`, found after a step that took `before` to `after`, with
    /// what that step bound of the names `exports` holds, unless `out` has a way that binds them
    /// the same, which it found first and so prefers.
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "it counts its work only in tests")
    )]
    fn extend(
        &self,
        out: &mut Vec<Way>,
        seen: &mut HashSet<Vec<Option<Key>>>,
        (before, after): (&Bindings, &Bindings),
        exports: &[usize],
        rest: &[Way],
    ) {
        for (way, end) in rest {
            #[cfg(test)]
            self.work.set(self.work.get() + 1);
            let mut way = way.clone();
            for (bound, &slot) in way.iter_mut().zip(exports) {
                if before[slot].is_none() && after[slot].is_some() {
                    bound.clone_from(&after[slot]);
                }
            }
            if seen.insert(way.iter().map(|b| b.as_ref().map(Bound::key)).collect()) {
                out.push((way, *end));
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the state of one step, as `step` has it"
    )]
    fn ways_step(
        &self,
        list: &List<'p>,
        within: Within,
        place: Place,
        available: &[Part<'t>],
        at: usize,
        bindings: Bindings,
        exports: &[usize],
    ) -> Vec<Way> {
        #[cfg(test)]
        self.work.set(self.work.get() + 1);
        let next = |from: usize| Place { from, ..place };
        let Some(wanted) = list.wanted.get(at) else {
            if list.whole && place.from != place.limit {
                return Vec::new();
            }
            return vec![(vec![None; exports.len()], place.from)];
        };
        let Some(here) = available[..place.limit].get(place.from).copied() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        match (*wanted, here) {
            (Part::Token(token), Part::Token(other)) => {
                if self.shape.token_text(token) == self.target.token_text(other) {
                    let rest = self.ways(
                        within,
                        next(place.from + 1),
                        available,
                        at + 1,
                        bindings,
                        exports,
                    );
                    out.extend(rest.iter().cloned());
                }
            }
            (Part::Child(child), Part::Child(node)) => match self.pattern.hole(child) {
                Some(hole) if hole.sequence => {
                    return self
                        .ways_sequence(within, place, available, at, hole, bindings, exports);
                }
                _ if !list.exports[at].is_empty() => {
                    for way in self.node_ways(child, node, &bindings, &list.exports[at]) {
                        let mut attempt = bindings.clone();
                        for (slot, bound) in way {
                            attempt[slot] = Some(bound);
                        }
                        let rest = self.ways(
                            within,
                            next(place.from + 1),
                            available,
                            at + 1,
                            attempt.clone(),
                            exports,
                        );
                        self.extend(&mut out, &mut seen, (&bindings, &attempt), exports, &rest);
                    }
                }
                _ => {
                    let mut attempt = bindings.clone();
                    if self.node(child, node, &mut attempt) {
                        let rest = self.ways(
                            within,
                            next(place.from + 1),
                            available,
                            at + 1,
                            attempt.clone(),
                            exports,
                        );
                        self.extend(&mut out, &mut seen, (&bindings, &attempt), exports, &rest);
                    }
                }
            },
            _ => {}
        }
        out
    }

    /// [`Matcher::ways_step`] for the sequence hole `hole` at `at`: every length it can take.
    #[expect(
        clippy::too_many_arguments,
        reason = "the state of one step, as `step` has it"
    )]
    fn ways_sequence(
        &self,
        within: Within,
        place: Place,
        available: &[Part<'t>],
        at: usize,
        hole: &Hole,
        bindings: Bindings,
        exports: &[usize],
    ) -> Vec<Way> {
        let next = |from: usize| Place { from, ..place };
        let block_end = self.ends_of(place.owner)[place.from].min(place.limit);
        let stops: Vec<usize> = (place.from..block_end)
            .filter(|&k| matches!(available[k], Part::Child(_)))
            .collect();
        let slot = self.compared_slot(hole);
        if let Some(slot) = slot
            && let Some(earlier) = bindings[slot].clone()
        {
            let end = if self.remember.get() {
                self.equal_run(&earlier, place, available, block_end)
            } else {
                stops
                    .iter()
                    .map(|stop| stop + 1)
                    .find(|&end| self.equal(&earlier, &Bound::Run(place.owner, place.from..end)))
            };
            return end.map_or_else(Vec::new, |end| {
                self.ways(within, next(end), available, at + 1, bindings, exports)
                    .to_vec()
            });
        }
        let list = self.list_of(within);
        // More than one place asks for this sequence's ways where a run starts anywhere, or a
        // sequence of its own before it in the list stops anywhere; otherwise only one does,
        // and the ways from the other places would be worked out for nothing.
        let asked_often = matches!(within, Within::Run)
            || list.wanted[..at].iter().any(|part| {
                matches!(part, Part::Child(child)
                    if is_sequence(self.pattern, part) && !compared(self.pattern, *child))
            });
        if self.remember.get()
            && asked_often
            && slot
                .is_none_or(|slot| !exports.contains(&slot) && !list.uses[at + 1].contains(&slot))
        {
            // Nothing reads what the sequence takes, so the ways from one place are those after
            // stopping there, then those from the next place it could stop: worked out from the
            // block's last stop back, each once, however many places reach it.
            let mut later: Rc<[Way]> = Rc::from(Vec::new());
            for &stop in stops.iter().rev() {
                let key = ways_at(&list, within, next(stop), at, exports, &bindings);
                if stop != place.from
                    && let Some(found) = self.ways_seen.borrow().get(&key)
                {
                    later = Rc::clone(found);
                    continue;
                }
                let rest = self.ways(
                    within,
                    next(stop + 1),
                    available,
                    at + 1,
                    bindings.clone(),
                    exports,
                );
                let mut out = Vec::new();
                let mut seen = HashSet::new();
                for ways in [&rest, &later] {
                    self.extend(&mut out, &mut seen, (&bindings, &bindings), exports, ways);
                }
                later = out.into();
                if stop != place.from {
                    self.ways_seen.borrow_mut().insert(key, Rc::clone(&later));
                }
            }
            return later.to_vec();
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for stop in stops {
            let mut attempt = bindings.clone();
            if let Some(slot) = slot {
                attempt[slot] = Some(Bound::Run(place.owner, place.from..stop + 1));
            }
            let rest = self.ways(
                within,
                next(stop + 1),
                available,
                at + 1,
                attempt.clone(),
                exports,
            );
            self.extend(&mut out, &mut seen, (&bindings, &attempt), exports, &rest);
        }
        out
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
        let remembered = (self.remember.get() && list.sequences).then(|| Remembered {
            within,
            place,
            at,
            bound: list.uses[at]
                .iter()
                .map(|&slot| bindings[slot].as_ref().map(Bound::key))
                .collect(),
        });
        let table = |key: &Remembered| {
            if key.bound.iter().any(Option::is_some) {
                &self.bound_outcomes
            } else {
                &self.outcomes
            }
        };
        if let Some(key) = &remembered {
            let known = table(key).borrow().get(key).cloned();
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
            table(&key).borrow_mut().insert(key, made);
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
        #[cfg(test)]
        self.work.set(self.work.get() + 1);
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
                _ if !list.exports[at].is_empty() => {
                    // Each way the node can bind the names used after it, in order, until the
                    // rest matches with one.
                    for way in self.node_ways(child, node, bindings, &list.exports[at]) {
                        let mut attempt = bindings.clone();
                        for (slot, bound) in way {
                            attempt[slot] = Some(bound);
                        }
                        if let Some(end) = self.list(within, next, available, at + 1, &mut attempt)
                        {
                            self.keep_within(child, node, &mut attempt);
                            *bindings = attempt;
                            return Some(end);
                        }
                    }
                    None
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
        // Every later run of fixed code must still be able to stand somewhere after this sequence
        // starts, or no length it takes can complete the list.
        if self.remember.get()
            && list
                .segments
                .iter()
                .filter(|&&start| start > at)
                .any(|&start| {
                    let stands = self.stands(within, list, start, place.owner, available);
                    stands.last().is_none_or(|&last| last <= place.from)
                })
        {
            return None;
        }
        let slot = self.compared_slot(hole);
        // A name only kept is recorded where the search stopped it, and steers nothing.
        let keep = |bindings: &mut Bindings, end: usize| self.keep(hole, place, end, bindings);
        if let Some(slot) = slot
            && let Some(earlier) = bindings[slot].clone()
        {
            // A name already bound takes equal code again, which leaves it one end.
            let end = if self.remember.get() {
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
        if !self.remember.get() || next == list.wanted.len() || list.segment_end[next] == next {
            for stop in stops {
                if let Some(end) = self.list(within, after(stop), available, at + 1, bindings) {
                    keep(bindings, stop + 1);
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
            let Some(after_code) = self.fixed(
                list,
                segment.clone(),
                Place {
                    from: start,
                    ..place
                },
                available,
                &mut probe,
            ) else {
                continue;
            };
            if let Some(end) = self.list(within, after(start - 1), available, at + 1, bindings) {
                keep(bindings, start);
                return Some(end);
            }
            // Only where no other list follows can a later place give the rest nothing new: one
            // that lets the next sequence start in the next list, an `else` clause's, gives it a
            // run this place cannot.
            // And only where the next sequence could start right after the code: if a token
            // stands there, a later place may be the only one it can start after.
            if list.settled[at]
                && block_end == place.limit
                && matches!(available.get(after_code), Some(Part::Child(_)))
            {
                return None;
            }
        }
        None
    }

    /// Where the fixed code `list.wanted[at..]` up to the next sequence could stand among
    /// `owner`'s parts, in order, by [`Matcher::could_match`]: whatever names are bound, the code
    /// matches nowhere else, though it may match at fewer.
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
                list.wanted[segment.clone()]
                    .iter()
                    .enumerate()
                    .all(|(i, wanted)| match (wanted, available.get(from + i)) {
                        (Part::Token(token), Some(Part::Token(other))) => {
                            self.shape.token_text(token) == self.target.token_text(other)
                        }
                        (Part::Child(child), Some(Part::Child(node))) => {
                            self.could_match(*child, *node)
                        }
                        _ => false,
                    })
            })
            .collect();
        self.stands.borrow_mut().insert(key, Rc::clone(&made));
        made
    }

    /// Whether `wanted` could match `node` whatever names are bound: compared as a match would
    /// compare them, but with every hole matching anything, and with a node whose own parts hold
    /// a sequence judged by its kind alone, since what a name bound outside holds can change how
    /// much that sequence takes.
    fn could_match(&self, wanted: NodeId, node: NodeId) -> bool {
        if self.pattern.hole(wanted).is_some() {
            return true;
        }
        if self.shape.kind(wanted) != self.target.kind(node) {
            return false;
        }
        if let Some(value) = self.shape.value(wanted) {
            return self.target.value(node) == Some(value);
        }
        let list = self.list_of(Within::Node(wanted));
        if list.sequences {
            return true;
        }
        let available = parts(self.target, node);
        if list.wanted.is_empty() && available.is_empty() {
            return self.shape.source(wanted) == self.target.source(node);
        }
        list.wanted.len() == available.len()
            && list
                .wanted
                .iter()
                .zip(&available)
                .all(|(wanted, here)| match (wanted, here) {
                    (Part::Token(token), Part::Token(other)) => {
                        self.shape.token_text(token) == self.target.token_text(other)
                    }
                    (Part::Child(child), Part::Child(node)) => self.could_match(*child, *node),
                    _ => false,
                })
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
        for at in segment {
            let here = available[..place.limit].get(from).copied()?;
            let matched = match (list.wanted[at], here) {
                (Part::Token(token), Part::Token(other)) => {
                    self.shape.token_text(token) == self.target.token_text(other)
                }
                // A node that can bind a name more than one way stands if any way does; which
                // way the rest needs is the match's to find, so it binds nothing here.
                (Part::Child(child), Part::Child(node)) if !list.exports[at].is_empty() => !self
                    .node_ways(child, node, bindings, &list.exports[at])
                    .is_empty(),
                (Part::Child(child), Part::Child(node)) => self.node(child, node, bindings),
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
        let ours = self.codes_of(place.owner);
        let end = ours
            .lengths
            .binary_search(&(ours.lengths[place.from] + length))
            .ok()?;
        if end <= place.from || end > block_end || !matches!(available[end - 1], Part::Child(_)) {
            return None;
        }
        // Within one node's parts, equal code is a common prefix of the two places' code at
        // least as long as the run, read in constant time, so that a name used twice costs no
        // comparison as long as the code it holds.
        if *owner == place.owner {
            return (ours.common(theirs.lengths[range.start], ours.lengths[place.from]) >= length)
                .then_some(end);
        }
        if ours.hash(place.from..end) != theirs.hash(range.clone()) {
            return None;
        }
        // Between two nodes' parts, equal hashes are checked by reading the code: directly the
        // first time two nodes are compared, and from then on by a common prefix of the two
        // nodes' code read together, in constant time, so that comparing the same two nodes at
        // many places costs no more than within one.
        let pair = (*owner, place.owner);
        let joint = {
            let mut joints = self.joints.borrow_mut();
            match joints.get(&pair) {
                None => {
                    joints.insert(pair, None);
                    None
                }
                Some(None) => {
                    let made = Rc::new(self.joint(*owner, place.owner));
                    joints.insert(pair, Some(Rc::clone(&made)));
                    Some(made)
                }
                Some(Some(made)) => Some(Rc::clone(made)),
            }
        };
        let Some(joint) = joint else {
            return self
                .equal(earlier, &Bound::Run(place.owner, place.from..end))
                .then_some(end);
        };
        let theirs_at = theirs.lengths[range.start];
        let ours_at = joint.second + ours.lengths[place.from];
        (joint.common.common(theirs_at, ours_at) >= length).then_some(end)
    }

    /// The code of `first`'s parts and then `second`'s, numbered together and kept apart by a
    /// piece neither holds, with how far any two places in it agree.
    fn joint(&self, first: NodeId, second: NodeId) -> Joint {
        let same = Same { arena: self.target };
        let mut numbers: HashMap<Code<'_>, u32> = HashMap::new();
        let mut pieces = Vec::new();
        let mut code = Vec::new();
        let mut number = |code: &mut Vec<Code<'t>>, pieces: &mut Vec<u32>| {
            for piece in code.drain(..) {
                let next =
                    u32::try_from(numbers.len()).expect("fewer pieces than an arena's offsets");
                pieces.push(*numbers.entry(piece).or_insert(next));
            }
        };
        same.code(&self.parts_of(first), &mut code);
        number(&mut code, &mut pieces);
        // Numbers count up from zero, and an arena holds fewer pieces than this.
        pieces.push(u32::MAX);
        let second_at = pieces.len();
        same.code(&self.parts_of(second), &mut code);
        number(&mut code, &mut pieces);
        #[cfg(test)]
        self.work.set(self.work.get() + pieces.len());
        Joint {
            common: Common::new(&pieces),
            second: second_at,
        }
    }

    /// Binds `bound` to `hole`'s name, or checks it equals what the name already holds.  A hole
    /// whose name is used once binds nothing and matches anything.
    fn bind(&self, hole: &Hole, bound: Bound, bindings: &mut Bindings) -> bool {
        let Some(slot) = self.slot(hole) else {
            return true;
        };
        if !self.compared[slot] {
            // Only kept: used once, so nothing is compared with it.
            bindings[slot] = Some(bound);
            return true;
        }
        match &bindings[slot] {
            None => {
                if let Some(pin) = self.pins.borrow().get(slot).copied().flatten()
                    && bound.key() != Key::Node(pin)
                {
                    return false;
                }
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
        let (a, b) = (as_parts(a), as_parts(b));
        #[cfg(test)]
        self.work.set(self.work.get() + a.len() + b.len());
        Same { arena: self.target }.parts(&a, &b)
    }
}

/// The code of a place in an arena, as a match compares it, with no kinds in it but a value's:
/// each kept token's text, each value with the kind of node it is the value of - so a string's
/// and a bytes literal's alike content differ - and the text of a node with nothing inside it, in
/// source order,
/// with where each nested node of more than one part begins and ends.  Two places with the same
/// code are equal whatever role they stand in, so a name used twice matches `a` as an argument,
/// a keyword, an attribute and a parameter alike, since each of those is a node of one part, or
/// a chain of them.  The boundaries keep apart trees whose tokens read alike: `(a + b) * c` and
/// `a + b * c`, and `[(a, b)]` and `[a, b]` once a tuple's own parentheses take no part.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Code<'a> {
    Token(&'a str),
    Value(Kind, &'a str),
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
    /// Each piece of code, numbered so that equal pieces have equal numbers.
    pieces: Vec<u32>,
    /// How far the code from any two pieces agrees, made when first asked.
    common: OnceCell<Common>,
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
            pieces: Vec::new(),
            common: OnceCell::new(),
        };
        let mut numbers: HashMap<Code<'_>, u32> = HashMap::new();
        let (mut length, mut hash) = (0, 0u64);
        let mut code = Vec::new();
        for part in available {
            code.clear();
            same.code(std::slice::from_ref(part), &mut code);
            for piece in code.drain(..) {
                hash = hash.wrapping_mul(BASE).wrapping_add(piece_hash(&piece));
                let next =
                    u32::try_from(numbers.len()).expect("fewer pieces than an arena's offsets");
                codes.pieces.push(*numbers.entry(piece).or_insert(next));
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

    /// How many pieces of code from piece `a` and from piece `b` are equal, in order.
    fn common(&self, a: usize, b: usize) -> usize {
        self.common
            .get_or_init(|| Common::new(&self.pieces))
            .common(a, b)
    }
}

/// The longest common prefix of any two suffixes of a sequence, in constant time: a suffix
/// array, the common prefix of each suffix with the one before it in that order, and a sparse
/// table of minimums over those.  Two suffixes' common prefix is the least of the prefixes
/// between them in the order.  Made in time about the sequence's length times its logarithm
/// squared, and only for a node whose code a name used twice is compared within.
struct Common {
    len: usize,
    /// Each suffix's place in sorted order.
    rank: Vec<usize>,
    /// `table[j][i]`: the least common prefix among those of sorted suffixes `i..i + 2^j`.
    table: Vec<Vec<usize>>,
}

impl Common {
    fn new(sequence: &[u32]) -> Common {
        let len = sequence.len();
        // The suffix array, by doubling: suffixes sorted by their first 1, 2, 4... pieces.
        let mut order: Vec<usize> = (0..len).collect();
        let mut rank: Vec<usize> = sequence.iter().map(|&piece| piece as usize).collect();
        let mut next = vec![0; len];
        let mut span = 1;
        if len > 1 {
            loop {
                let key = |i: usize| (rank[i], rank.get(i + span).map_or(0, |r| r + 1));
                order.sort_unstable_by_key(|&i| key(i));
                next[order[0]] = 0;
                for w in 1..len {
                    next[order[w]] =
                        next[order[w - 1]] + usize::from(key(order[w - 1]) < key(order[w]));
                }
                std::mem::swap(&mut rank, &mut next);
                if rank[order[len - 1]] == len - 1 {
                    break;
                }
                span *= 2;
            }
        }
        if len == 1 {
            rank = vec![0];
        }
        // Each sorted suffix's common prefix with the one before it, by Kasai's walk.
        let mut prefix = vec![0; len];
        let mut h = 0;
        for i in 0..len {
            if rank[i] == 0 {
                h = 0;
                continue;
            }
            let j = order[rank[i] - 1];
            while i + h < len && j + h < len && sequence[i + h] == sequence[j + h] {
                h += 1;
            }
            prefix[rank[i]] = h;
            h = h.saturating_sub(1);
        }
        let mut table = vec![prefix];
        let mut width = 1;
        while 2 * width <= len {
            let last = table.last().expect("begun with one");
            let row = (0..=len - 2 * width)
                .map(|i| last[i].min(last[i + width]))
                .collect();
            table.push(row);
            width *= 2;
        }
        Common { len, rank, table }
    }

    fn common(&self, a: usize, b: usize) -> usize {
        if a == b {
            return self.len - a;
        }
        let (low, high) = if self.rank[a] < self.rank[b] {
            (self.rank[a] + 1, self.rank[b])
        } else {
            (self.rank[b] + 1, self.rank[a])
        };
        let level = (high - low + 1).ilog2() as usize;
        self.table[level][low].min(self.table[level][high + 1 - (1 << level)])
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
                        out.push(Code::Value(self.arena.kind(*child), value));
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

    /// The matches of `pattern_text` that some binding of `tested` lets `accept` keep, and
    /// every binding `accept` was offered, as the text each tested name was bound to.
    fn found_where(
        pattern_text: &str,
        target_text: &str,
        tested: &[&str],
        mut accept: impl FnMut(&[&str]) -> bool,
    ) -> (Vec<String>, Vec<Vec<String>>) {
        let (target, _) = call(target_text);
        let mut offered = Vec::new();
        let kept = find_where(&pattern(pattern_text), &target, tested, &mut |nodes| {
            let texts: Vec<&str> = nodes.iter().map(|&node| target.source(node)).collect();
            offered.push(texts.iter().map(|t| (*t).to_owned()).collect());
            accept(&texts)
        });
        let kept = kept
            .into_iter()
            .map(|m| target_text[m.range].to_owned())
            .collect();
        (kept, offered)
    }

    /// What each name in `kept` captured at each match, as text.
    fn captured(pattern_text: &str, target_text: &str, kept: &[&str]) -> Vec<Vec<String>> {
        let (target, _) = call(target_text);
        find_captured(&pattern(pattern_text), &target, kept)
            .into_iter()
            .map(|found| {
                found
                    .captures
                    .into_iter()
                    .map(|range| target_text[range].to_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_hole_captures_its_node_and_a_sequence_its_nodes_and_what_stands_between() {
        assert_eq!(
            captured("f($a,$b...)", "f(x,y,z)", &["b", "a"]),
            [["y,z", "x"]]
        );
    }

    #[test]
    fn a_sequence_captures_the_fewest_nodes_that_let_the_rest_match() {
        assert_eq!(
            captured("f($a...,$x,$b...)", "f(a,b,c,d)", &["a", "x", "b"]),
            [["a", "b", "c,d"]]
        );
    }

    #[test]
    fn a_name_used_twice_captures_the_code_it_matched_first() {
        let (target, _) = call("f(x,x)");
        let found = find_captured(&pattern("f($a,$a)"), &target, &["a"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].captures.len(), 1);
        assert_eq!(found[0].captures[0], 2..3);
    }

    #[test]
    fn a_where_captures_what_the_way_it_accepted_bound() {
        // The first way binds `$x` to b; only the way binding it to c passes.
        let (target, _) = call("f(a,b,c,d)");
        let found = find_where_captured(
            &pattern("f($a...,$x,$b...)"),
            &target,
            &["x"],
            &mut |nodes| target.source(nodes[0]) == "c",
            &["a", "x", "b"],
        );
        let texts: Vec<Vec<&str>> = found
            .iter()
            .map(|found| {
                found
                    .captures
                    .iter()
                    .map(|r| &target.text()[r.clone()])
                    .collect()
            })
            .collect();
        assert_eq!(texts, [["a,b", "c", "d"]]);
    }

    #[test]
    fn a_where_that_tells_equal_code_apart_captures_the_way_it_accepted() {
        // Both `c`s have equal code; only the second passes.  Made again with `$x` bound by
        // code, the search would take the first `c` and give `$b...` the rest.
        let (target, _) = call("f(a,c,b,c,d)");
        let found = find_where_captured(
            &pattern("f($a...,$x,$b...)"),
            &target,
            &["x"],
            &mut |nodes| target.range(nodes[0]) == (8..9),
            &["a", "x", "b"],
        );
        let texts: Vec<Vec<&str>> = found
            .iter()
            .map(|found| {
                found
                    .captures
                    .iter()
                    .map(|r| &target.text()[r.clone()])
                    .collect()
            })
            .collect();
        assert_eq!(texts, [["a,c,b", "c", "d"]]);
        assert_eq!(found[0].captures[1], 8..9);
    }

    #[test]
    #[should_panic(expected = "a capture of $y was asked for, which its pattern does not bind")]
    fn asking_for_a_name_the_pattern_does_not_bind_is_a_defect() {
        let (target, _) = call("f(x)");
        let _ = find_captured(&pattern("f($x)"), &target, &["y"]);
    }

    #[test]
    fn a_where_keeps_a_match_any_binding_of_its_names_satisfies() {
        // `$x` can be b or c: the first way found binds it to b.
        let (kept, offered) =
            found_where("f($a...,$x,$b...)", "f(a,b,c,d)", &["x"], |x| x == ["c"]);
        assert_eq!(kept, ["f(a,b,c,d)"]);
        assert!(offered.contains(&vec!["b".to_owned()]), "{offered:?}");
        assert!(offered.contains(&vec!["c".to_owned()]), "{offered:?}");
        // No way binds it to a.
        let (kept, offered) =
            found_where("f($a...,$x,$b...)", "f(a,b,c,d)", &["x"], |x| x == ["a"]);
        assert_eq!(kept, Vec::<String>::new());
        let mut every: Vec<Vec<String>> = offered;
        every.sort();
        every.dedup();
        assert_eq!(every, [vec!["b".to_owned()], vec!["c".to_owned()]]);
        // A name bound one way only is offered that way, once a match is found.
        let (kept, offered) = found_where("f($x,b)", "f(q,b)", &["x"], |_| true);
        assert_eq!(
            (kept, offered),
            (vec!["f(q,b)".to_owned()], vec![vec!["q".to_owned()]])
        );
    }

    #[test]
    fn a_name_bound_in_advance_must_match_the_same_code() {
        let (target, _) = call("f(a,b,a)");
        let call_node = target.root();
        let args: Vec<NodeId> = target.children(call_node).skip(1).collect();
        let (a, b) = (args[0], args[1]);
        let pattern = pattern("f($y,b,$z)");
        // `$y` bound to an `a` must find an `a` there; bound to `b`, it cannot.
        assert!(matches_at(&pattern, &target, call_node, false, &[("y", a)]));
        assert!(!matches_at(
            &pattern,
            &target,
            call_node,
            false,
            &[("y", b)]
        ));
        // Equal code, not the same node: the last `a` equals the first.
        assert!(matches_at(
            &pattern,
            &target,
            call_node,
            false,
            &[("y", args[2])]
        ));
        assert!(matches_at(
            &pattern,
            &target,
            call_node,
            false,
            &[("z", a), ("y", a)]
        ));
        // `matches` is the node itself; inside an argument there is no call to match.
        assert!(!matches_at(&pattern, &target, a, false, &[]));
        assert!(!matches_at(&pattern, &target, a, true, &[]));
        assert!(matches_at(&pattern, &target, call_node, true, &[]));
    }

    #[test]
    fn a_where_that_accepts_everything_finds_what_find_finds() {
        for (pattern_text, target_text) in [
            ("f($a...,$x,$b...)", "f(a,b,c,d)"),
            ("f($x,$x)", "f(q,q)"),
            ("f($x,$y...)", "f(p,q,r)"),
            ("f($x)", "g(a)"),
        ] {
            let (target, _) = call(target_text);
            let pattern = pattern(pattern_text);
            let everything = find_where(&pattern, &target, &["x"], &mut |_| true);
            assert_eq!(everything, find(&pattern, &target), "{pattern_text}");
        }
    }

    #[test]
    fn a_tested_name_is_one_more_use_of_it_to_the_cost_of_a_search() {
        // Alone, neither name is used again, so no sequence's stop decides anything later.
        let two = pattern("f($a...,$x,$b...,$y,$c...)");
        assert_eq!(two.costly(), None);
        // Testing both makes every pair of stops a way to try, the square; testing three, with
        // three stops, every triple of them, the cube.
        assert_eq!(two.costly_with(&["x"]), None);
        assert_eq!(two.costly_with(&["x", "y"]), None);
        assert!(matches!(
            pattern("f($a...,$x,$b...,$y,$c...,$z,$d...)").costly_with(&["x", "y", "z"]),
            Some(Costly::ThirdPlace(_))
        ));
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
    fn two_places_that_matter_are_allowed_and_a_third_is_costly() {
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
        // Two sequences each named twice: with where the run starts, the first `$a`'s stop and
        // the first `$b`'s are three places that decide what follows, named at the first `$b`.
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
        assert_eq!(twice.costly(), Some(Costly::ThirdPlace(nth(&twice, 3))));
        // One sequence named twice, but with a step before each use: where the run starts, and
        // where `$...` and the first `$a` stop, named at the last of them.
        let spread = pattern(&[
            Fixed(0),
            Sequence(None),
            Fixed(1),
            Sequence(Some(0)),
            Fixed(2),
            Sequence(Some(0)),
            Fixed(0),
        ]);
        assert_eq!(spread.costly(), Some(Costly::ThirdPlace(nth(&spread, 3))));
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
            find_with(&pattern, &target, true, &[], None),
            find_with(&pattern, &target, false, &[], None)
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
        let backtracked = find_with(&pattern, &target, false, &[], None);
        assert_eq!(backtracked.len(), 1);
        assert_eq!(find_with(&pattern, &target, true, &[], None), backtracked);
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
        let backtracked = find_with(&pattern, &target, false, &[], None);
        assert_eq!(backtracked.len(), 2);
        assert_eq!(find_with(&pattern, &target, true, &[], None), backtracked);
    }

    /// Words as statements of a `Def` node a module holds, split into two lists at `split`, like a
    /// branch and its else clause, with a `;` token between two statements of one list if
    /// `separated`.  Gives the arena and each word's node.
    fn block(words: &[String], split: usize, separated: bool) -> (Arena<'static>, Vec<NodeId>) {
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
        builder.enter(Kind::new("Def"), 0..text.len());
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
        builder.leave();
        builder.leave();
        let arena = builder.finish().into_owned();
        let parent = arena.children(arena.root()).next().unwrap();
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
        let (arena, nodes) = block(&words, words.len(), separated);
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
        block(&words, split, separated).0
    }

    #[test]
    fn a_node_starts_where_it_stands_and_a_run_anywhere() {
        use Item::{Fixed, Sequence, Single};
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
        // Two stops inside a node, which starts where it stands, are the square; the same in a
        // run, which starts anywhere, are the cube.
        assert_eq!(twice.costly(), None);
        assert!(matches!(
            pattern(&[
                Fixed(0),
                Sequence(Some(0)),
                Fixed(1),
                Sequence(Some(1)),
                Fixed(2),
                Sequence(Some(0)),
                Fixed(1),
                Sequence(Some(1)),
                Fixed(0),
            ])
            .costly(),
            Some(Costly::ThirdPlace(_))
        ));
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
        let backtracked = find_with(&pattern, &target, false, &[], None);
        assert_eq!(backtracked.len(), 1);
        assert_eq!(find_with(&pattern, &target, true, &[], None), backtracked);
    }

    /// A sequence takes children, and stops at one: where the code after it stands just past a
    /// token - here `x` after a `;`, against a pattern written without them - it cannot stop
    /// there, since it would end on the token.
    #[test]
    fn a_sequence_never_stops_on_a_token() {
        use Item::{Fixed, Sequence};
        let pattern = nested_pattern(&[Sequence(None), Fixed(1)], false);
        let target = nested_target(&[0, 1], 2, true);
        assert_eq!(find_with(&pattern, &target, false, &[], None), []);
        assert_eq!(find_with(&pattern, &target, true, &[], None), []);
        // Written with the token, it matches.
        let separated = nested_pattern(&[Sequence(None), Fixed(1)], true);
        assert_eq!(find_with(&separated, &target, true, &[], None).len(), 1);
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
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
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
                find_with(&pattern, &target, true, &[], None),
                find_with(&pattern, &target, false, &[], None),
                "{:?} over {:?}",
                items,
                statements
            );
            capture_check(&pattern, &target, &[], None)
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{items:?} over {statements:?}: {err}")))?;
        }

        /// A `where` keeps the same matches whether the search remembers or backtracks, so
        /// every way of binding the tested name was offered both ways; and one that keeps
        /// everything keeps what `find` finds.
        #[test]
        fn a_where_keeps_what_some_binding_passes_however_it_is_searched(
            statements in prop::collection::vec(0usize..3, 0..12),
            split in 0usize..12,
            before in prop::collection::vec(item(), 0..3),
            after in prop::collection::vec(item(), 0..3),
            separated: bool,
            tested in 0usize..2,
            wanted in 0usize..3,
        ) {
            // The tested name is bound by a single hole, among whatever else the pattern holds.
            let name = format!("n{tested}");
            let items: Vec<Item> = before
                .into_iter()
                .chain(std::iter::once(Item::Single(Some(tested))))
                .chain(after)
                .collect();
            let pattern = nested_pattern(&items, separated);
            prop_assume!(pattern.costly_with(&[name.as_str()]).is_none());
            let target = nested_target(&statements, split.min(statements.len()), separated);
            let mut passes = |nodes: &[NodeId]| target.source(nodes[0]) == STATEMENTS[wanted];
            let remembered = find_with(&pattern, &target, true, &[&name], Some(&mut passes));
            let backtracked = find_with(&pattern, &target, false, &[&name], Some(&mut passes));
            prop_assert_eq!(&remembered, &backtracked, "{:?} over {:?}", items, statements);
            let everything = find_with(&pattern, &target, true, &[&name], Some(&mut |_| true));
            prop_assert_eq!(everything, find(&pattern, &target));
            // The captures are a passing way's, whichever way the search went.
            let check = capture_check(&pattern, &target, &[&name], Some(&mut passes));
            check
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{items:?} over {statements:?}: {err}")))?;
            let mut kept: Vec<&str> =
                pattern.holes().filter_map(|(_, hole)| hole.name.as_deref()).collect();
            kept.sort_unstable();
            kept.dedup();
            let at = kept.iter().position(|kept| *kept == name).expect("the tested name is bound");
            for found in &check.fast {
                prop_assert_eq!(&target.text()[found.captures[at].clone()], STATEMENTS[wanted]);
            }
            // A `where` may tell apart nodes whose code is equal, by where they are: the
            // captures are still the accepted way's, never one with equal code found earlier.
            let mut placed = |nodes: &[NodeId]| target.range(nodes[0]).start % 3 == wanted;
            let check = capture_check(&pattern, &target, &[&name], Some(&mut placed));
            check
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{items:?} over {statements:?}: {err}")))?;
            for found in &check.fast {
                prop_assert_eq!(found.captures[at].start % 3, wanted, "{:?} over {:?}", items, statements);
            }
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
                find_with(&pattern, &target, true, &[], None),
                find_with(&pattern, &target, false, &[], None),
                "{:?} over {:?}, split at {}, separated: {}",
                items,
                statements,
                split,
                separated
            );
            capture_check(&pattern, &target, &[], None)
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{items:?} over {statements:?}: {err}")))?;
        }
    }
}

/// The remembering matcher held to plain backtracking over generated trees: patterns of nodes
/// within nodes, with single holes and sequences named or not, names shared between lists and
/// tokens anywhere, against targets of nested nodes with marked lists - the shapes the
/// run-shaped properties above cannot make.
#[cfg(test)]
mod trees {
    use super::*;
    use crate::{Builder, Hole, Kind, Language, TokenText};
    use proptest::prelude::*;

    /// A tree written as an s-expression: `(K a b)` a node of kind `K` over its children, a word a
    /// leaf compared by its text, `;` a token, `$x`, `$_`, `$x...` and `$...` holes, and `[ ... ]`
    /// a marked list among the enclosing node's children.
    #[derive(Debug, Clone)]
    enum Tree {
        Node(String, Vec<Tree>),
        Leaf(String),
        Token(String),
        Hole(String),
        List(Vec<Tree>),
    }

    fn words(text: &str) -> Vec<String> {
        let spaced = text
            .replace('(', " ( ")
            .replace(')', " ) ")
            .replace('[', " [ ")
            .replace(']', " ] ")
            .replace(';', " ; ");
        spaced.split_whitespace().map(str::to_owned).collect()
    }

    fn read(words: &[String], at: &mut usize, close: &str) -> Vec<Tree> {
        let mut out = Vec::new();
        while *at < words.len() && words[*at] != close {
            let word = words[*at].clone();
            *at += 1;
            out.push(match word.as_str() {
                "(" => {
                    let kind = words[*at].clone();
                    *at += 1;
                    Tree::Node(kind, read(words, at, ")"))
                }
                "[" => Tree::List(read(words, at, "]")),
                ";" => Tree::Token(word),
                _ if word.starts_with('$') => Tree::Hole(word),
                _ => Tree::Leaf(word),
            });
        }
        *at += 1;
        out
    }

    fn render(tree: &Tree, text: &mut String) {
        match tree {
            Tree::Node(_, children) | Tree::List(children) => {
                for child in children {
                    render(child, text);
                }
            }
            Tree::Leaf(word) | Tree::Token(word) | Tree::Hole(word) => {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(word);
            }
        }
    }

    struct Build<'a> {
        builder: Builder<'a>,
        text: &'a str,
        at: usize,
        next: u32,
        holes: Vec<(NodeId, Hole)>,
    }

    impl Build<'_> {
        fn add(&mut self, tree: &Tree) {
            while self.text.as_bytes().get(self.at) == Some(&b' ') {
                self.at += 1;
            }
            match tree {
                Tree::Node(kind, children) => {
                    let mut spelled = String::new();
                    render(tree, &mut spelled);
                    let kind = Kind::new(Box::leak(kind.clone().into_boxed_str()));
                    self.builder.enter(kind, self.at..self.at + spelled.len());
                    self.next += 1;
                    for child in children {
                        self.add(child);
                    }
                    self.builder.leave();
                }
                Tree::List(children) => {
                    self.builder.begin_list();
                    for child in children {
                        self.add(child);
                    }
                    self.builder.end_list();
                }
                Tree::Leaf(word) | Tree::Hole(word) => {
                    let node = NodeId(self.next);
                    self.builder
                        .enter(Kind::new("Leaf"), self.at..self.at + word.len());
                    self.next += 1;
                    self.builder.leave();
                    if let Tree::Hole(spelled) = tree {
                        let spelled = &spelled[1..];
                        let (name, sequence) = match spelled.strip_suffix("...") {
                            Some(name) => (name, true),
                            None => (spelled, false),
                        };
                        let name = (!name.is_empty() && name != "_").then(|| name.to_owned());
                        self.holes.push((node, Hole { name, sequence }));
                    }
                    self.at += word.len();
                }
                Tree::Token(word) => {
                    self.builder
                        .token(self.at..self.at + word.len(), TokenText::Source);
                    self.at += word.len();
                }
            }
        }
    }

    /// The trees `text` writes, under a root, as an arena, with its holes.
    fn build(text: &str) -> (Arena<'static>, Vec<(NodeId, Hole)>) {
        let words = words(text);
        let root = Tree::Node("Root".into(), read(&words, &mut 0, "\u{0}"));
        let mut spelled = String::new();
        render(&root, &mut spelled);
        let spelled: &'static str = Box::leak(spelled.into_boxed_str());
        let mut build = Build {
            builder: Builder::new(Language::new("toy"), spelled).unwrap(),
            text: spelled,
            at: 0,
            next: 0,
            holes: Vec::new(),
        };
        build.add(&root);
        let holes = build.holes;
        (build.builder.finish().into_owned(), holes)
    }

    fn pattern(text: &str) -> Pattern {
        let (arena, holes) = build(text);
        Pattern::new(arena, holes)
    }

    /// What `pattern` matches in `target`, remembering and by plain backtracking.
    fn both(pattern_text: &str, target_text: &str) -> (Vec<String>, Vec<String>) {
        let pattern = pattern(pattern_text);
        let (target, _) = build(target_text);
        let shown = |found: Vec<Match>| -> Vec<String> {
            found
                .into_iter()
                .map(|m| target.text()[m.range].to_owned())
                .collect()
        };
        (
            shown(find_with(&pattern, &target, true, &[], None)),
            shown(find_with(&pattern, &target, false, &[], None)),
        )
    }

    /// Where fixed code could stand is judged by kinds, not by matching with nothing bound: here
    /// `g`'s `$s` takes `a a` only once the outer `$s` is bound to it, so with nothing bound
    /// `g` would seem to stand nowhere it can.
    #[test]
    fn where_code_could_stand_holds_whatever_is_bound() {
        for (pattern, target) in [
            (
                "(f (k $s...) $... (g $s... $...) (h $s...))",
                "(f (k a a) w (g a a b) (h a a))",
            ),
            (
                "(k $s...) $... (g $s... $...) (h $s...)",
                "[(k a a) w (g a a b) (h a a)]",
            ),
        ] {
            let (fast, slow) = both(pattern, target);
            assert_eq!(fast, slow, "{pattern} over {target}");
            assert!(!slow.is_empty(), "{pattern} over {target}");
        }
    }

    /// A sequence cannot start on a token, so where one follows the fixed code a later place is
    /// still tried: here the first `x` is followed by `;`, the second by a child.
    #[test]
    fn a_later_place_is_tried_when_a_token_follows_the_code() {
        let (fast, slow) = both("(f $... x $... y)", "(f w x ; z x z y)");
        assert_eq!(fast, slow);
        assert_eq!(slow.len(), 1);
    }

    /// A run with one choice, a sequence named twice, over a block of one repeated statement:
    /// each start tries each length, and with equal code read in constant time the work grows
    /// with the square of the block's length - four times over twice the length, not eight, as
    /// comparing the code piece by piece made it.  With no `g` anywhere, each start gives up at
    /// once, and the work grows with the length alone.
    #[test]
    fn a_name_used_twice_over_repeated_code_stays_quadratic() {
        let pattern = pattern("x $s... x $s... g");
        assert_eq!(pattern.costly(), None);
        let work = |n: usize, tail: &str| {
            let (target, _) = build(&format!("[{}{tail}]", vec!["x"; n].join(" ")));
            let (found, work) = find_counting(&pattern, &target);
            (found.len(), work)
        };
        let ((found, small), (_, large)) = (work(100, " g"), work(200, " g"));
        assert_eq!(found, 49);
        assert!(large < 5 * small, "with g: {small} then {large}");
        let ((found, small), (_, large)) = (work(100, ""), work(200, ""));
        assert_eq!(found, 0);
        assert!(large < 3 * small, "with no g: {small} then {large}");
    }

    #[test]
    fn choices_in_lists_that_share_no_name_do_not_multiply() {
        // Each list is worked out once whatever the other chose.
        assert_eq!(
            pattern("(f (g $a... x $a...) (h $b... y $b...))").costly(),
            None
        );
        // A name bound last in its node binds one way, so the other list reads one binding.
        assert_eq!(
            pattern("(f (g $a... x $a... $v) (h $b... $v $b...))").costly(),
            None
        );
        // But a list that reads names another node binds is worked out again for each way it
        // binds them, here as many as the triples of its three stops.
        assert!(matches!(
            pattern("(f (g $a... $v $c... $w $e... $u $k...) (h $v $w $u))").costly(),
            Some(Costly::ThirdPlace(_))
        ));
        // Stops in one list multiply as ever: two in a node, which starts where it stands, are
        // the square, and three the cube.
        assert_eq!(
            pattern("(f w $a... x $b... y $a... x $b... z)").costly(),
            None
        );
        assert!(matches!(
            pattern("(f w $a... x $b... y $c... z $a... $b... $c...)").costly(),
            Some(Costly::ThirdPlace(_))
        ));
    }

    fn pattern_item(depth: u32) -> BoxedStrategy<String> {
        let leaf = prop_oneof![
            4 => prop_oneof![Just("x"), Just("y")].prop_map(str::to_owned),
            1 => prop_oneof![Just("$a"), Just("$b"), Just("$_")].prop_map(str::to_owned),
            2 => prop_oneof![Just("$p..."), Just("$q..."), Just("$...")].prop_map(str::to_owned),
            1 => Just(";".to_owned()),
        ];
        if depth == 0 {
            return leaf.boxed();
        }
        prop_oneof![
            4 => leaf,
            1 => prop::collection::vec(pattern_item(depth - 1), 1..5)
                .prop_map(|items| format!("(g {})", trimmed(&items))),
        ]
        .boxed()
    }

    fn target_item(depth: u32) -> BoxedStrategy<String> {
        let leaf = prop_oneof![
            4 => prop_oneof![Just("x"), Just("y")].prop_map(str::to_owned),
            1 => Just(";".to_owned()),
        ];
        if depth == 0 {
            return leaf.boxed();
        }
        prop_oneof![
            3 => leaf,
            1 => prop::collection::vec(target_item(depth - 1), 1..6)
                .prop_map(|items| format!("(g {})", trimmed(&items))),
            1 => prop::collection::vec(target_item(depth - 1), 1..6)
                .prop_map(|items| format!("[{}]", trimmed(&items))),
        ]
        .boxed()
    }

    /// What a name holds in [`Oracle`]: the code it matched.
    type Held<'a> = BTreeMap<String, Vec<Code<'a>>>;

    /// A matcher written for the tests alone, from the rules rather than from the matcher: every
    /// way of matching is tried, across nodes as within one, depth first with each sequence
    /// shortest first, and nothing is remembered or skipped.  Too slow for anything but small
    /// trees, and so the reference for them.
    struct Oracle<'a, 't> {
        pattern: &'a Pattern,
        target: &'a Arena<'t>,
    }

    impl<'a, 't> Oracle<'a, 't> {
        fn code(&self, of: &[Part<'_>]) -> Vec<Code<'a>> {
            let mut out = Vec::new();
            Same { arena: self.target }.code(of, &mut out);
            out
        }

        fn bind(&self, hole: &Hole, of: &[Part<'_>], held: &Held<'a>) -> Option<Held<'a>> {
            let Some(name) = &hole.name else {
                return Some(held.clone());
            };
            let code = self.code(of);
            if let Some(earlier) = held.get(name) {
                return (*earlier == code).then(|| held.clone());
            }
            let mut more = held.clone();
            more.insert(name.clone(), code);
            Some(more)
        }

        /// Calls `then` with each way `wanted` matches `node`, until it says to stop.
        fn node(
            &self,
            wanted: NodeId,
            node: NodeId,
            held: &Held<'a>,
            then: &mut dyn FnMut(&Held<'a>) -> bool,
        ) -> bool {
            let shape = self.pattern.arena();
            if let Some(hole) = self.pattern.hole(wanted) {
                return self
                    .bind(hole, &[Part::Child(node)], held)
                    .is_some_and(|held| then(&held));
            }
            if shape.kind(wanted) != self.target.kind(node) {
                return false;
            }
            // The trees built here carry no values, so none is compared.
            let wanted_parts = parts(shape, wanted);
            let node_parts = parts(self.target, node);
            if wanted_parts.is_empty() && node_parts.is_empty() {
                return shape.source(wanted) == self.target.source(node) && then(held);
            }
            let ends = list_ends(self.target, &node_parts);
            self.list(
                &wanted_parts,
                &node_parts,
                &ends,
                0,
                node_parts.len(),
                held,
                &mut |end, held| end == node_parts.len() && then(held),
            )
        }

        /// Calls `then` with where each way `wanted` matches `available` from `from` ends.
        #[expect(clippy::too_many_arguments, reason = "the state of one step")]
        fn list(
            &self,
            wanted: &[Part<'_>],
            available: &[Part<'t>],
            ends: &[usize],
            from: usize,
            limit: usize,
            held: &Held<'a>,
            then: &mut dyn FnMut(usize, &Held<'a>) -> bool,
        ) -> bool {
            let Some((first, rest)) = wanted.split_first() else {
                return then(from, held);
            };
            let Some(here) = available[..limit].get(from) else {
                return false;
            };
            match (first, here) {
                (Part::Token(token), Part::Token(other)) => {
                    self.pattern.arena().token_text(token) == self.target.token_text(other)
                        && self.list(rest, available, ends, from + 1, limit, held, then)
                }
                (Part::Child(child), Part::Child(node)) => match self.pattern.hole(*child) {
                    Some(hole) if hole.sequence => {
                        // One or more children of one list, from a child to a child.
                        let block = ends[from].min(limit);
                        (from..block)
                            .filter(|&stop| matches!(available[stop], Part::Child(_)))
                            .any(|stop| {
                                self.bind(hole, &available[from..=stop], held)
                                    .is_some_and(|held| {
                                        self.list(
                                            rest,
                                            available,
                                            ends,
                                            stop + 1,
                                            limit,
                                            &held,
                                            then,
                                        )
                                    })
                            })
                    }
                    _ => self.node(*child, *node, held, &mut |held| {
                        self.list(rest, available, ends, from + 1, limit, held, then)
                    }),
                },
                _ => false,
            }
        }

        /// Every match, as [`find`] reports them.
        fn find(&self) -> Vec<Match> {
            self.find_where(&mut |_| true)
        }

        /// The matches some way of which `accept` accepts, by what that way holds.
        fn find_where(&self, accept: &mut dyn FnMut(&Held<'a>) -> bool) -> Vec<Match> {
            let shape = self.pattern.arena();
            let content: Vec<NodeId> = shape.children(shape.root()).collect();
            let mut found = Vec::new();
            if let [only] = content.as_slice() {
                for node in self.target.nodes() {
                    if self.node(*only, node, &Held::new(), &mut |held| accept(held)) {
                        found.push(Match {
                            range: self.target.range(node),
                        });
                    }
                }
            } else {
                let wanted = between_first_and_last(&parts(shape, shape.root()));
                for node in self.target.nodes() {
                    let available = parts(self.target, node);
                    let ends = list_ends(self.target, &available);
                    for (start, first) in available.iter().enumerate() {
                        let Part::Child(first) = first else { continue };
                        if self.target.list(*first).is_none() {
                            continue;
                        }
                        let mut end = None;
                        self.list(
                            &wanted,
                            &available,
                            &ends,
                            start,
                            ends[start],
                            &Held::new(),
                            &mut |at, held| {
                                let kept = accept(held);
                                if kept {
                                    end = Some(at);
                                }
                                kept
                            },
                        );
                        if let Some(end) = end {
                            found.push(Match {
                                range: self.target.range(*first).start
                                    ..self.target.range(statement(&available, end - 1)).end,
                            });
                        }
                    }
                }
            }
            found.sort_by_key(|m| (m.range.start, m.range.end));
            found.dedup();
            found
        }
    }

    /// A `where` over a run of statements is judged by every way the run binds its names, as
    /// over one node, and a match it keeps covers the whole run.
    #[test]
    fn a_where_over_a_run_keeps_a_match_any_binding_satisfies() {
        let compiled = super::trees::pattern("(f $a... $x $b...) (g y)");
        let (built, _) = build("[(f p q r s) (g y)]");
        let kept = |wanted: &str| -> Vec<String> {
            find_where(&compiled, &built, &["x"], &mut |nodes| {
                built.source(nodes[0]) == wanted
            })
            .into_iter()
            .map(|m| built.text()[m.range].to_owned())
            .collect()
        };
        // Each sequence takes one or more, so `$x` is q, as the first way binds it, or r.
        for wanted in ["q", "r"] {
            assert_eq!(kept(wanted), ["p q r s y"], "{wanted}");
        }
        for wanted in ["p", "s"] {
            assert_eq!(kept(wanted), Vec::<String>::new(), "{wanted}");
        }
    }

    /// `matches_at` with `anywhere` looks inside the node it is given, never beside it.
    #[test]
    fn anywhere_is_inside_the_node_and_nowhere_else() {
        let compiled = super::trees::pattern("(f $x)");
        let (built, _) = build("(B (f y) (g x))");
        let block = built.children(built.root()).next().expect("B");
        let [f, g]: [NodeId; 2] = built
            .children(block)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert!(matches_at(&compiled, &built, f, true, &[]));
        assert!(matches_at(&compiled, &built, block, true, &[]));
        assert!(!matches_at(&compiled, &built, g, true, &[]));
    }

    /// A `where` whose pattern shares a name is matched at each bound node, once per binding,
    /// so each answer must cost what that node holds rather than what the file does: over a
    /// file four times as long, the same nodes are looked at.
    #[test]
    fn matching_at_a_node_looks_only_inside_it() {
        let compiled = super::trees::pattern("(g $x)");
        let examined = |n: usize| {
            let (built, _) = build(&format!("[{}]", vec!["(g a)"; n].join(" ")));
            let first = built.children(built.root()).next().expect("a call");
            let a = built.children(first).next().expect("its argument");
            crate::EXAMINED.with(|examined| examined.set(0));
            assert!(matches_at(&compiled, &built, first, false, &[("x", a)]));
            crate::EXAMINED.with(std::cell::Cell::get)
        };
        let (small, large) = (examined(16), examined(64));
        assert!(small > 0, "nothing was looked at");
        assert_eq!(small, large);
    }

    /// A nested node's match is not final: where a name it bound fails later, it is asked for
    /// its next way.  Here `f`'s `$a` must take `1, 2`, though `1` alone lets `f` itself match.
    #[test]
    fn a_node_is_asked_again_when_a_name_it_bound_fails_later() {
        for (pattern, target) in [
            ("(B (f $a... $b...) (g $a...))", "(B (f x y z) (g x y))"),
            // Two levels down, and across a run of statements.
            (
                "(B (h (f $a... $b...)) (g $a...))",
                "(B (h (f x y z)) (g x y))",
            ),
            ("(f $a... $b...) (g $a...)", "[(f x y z) (g x y)]"),
            // The node uses its name again itself, and reaches one place by more than one way.
            ("(B (f $a... y $a...) (g $a...))", "(B (f x y x) (g x))"),
            (
                "(B (f $a... y $... $... z) (g $a...))",
                "(B (f x y w w w z) (g x))",
            ),
        ] {
            let (fast, slow) = both(pattern, target);
            assert_eq!(fast, slow, "{pattern} over {target}");
            assert_eq!(slow.len(), 1, "{pattern} over {target}");
            let (built, _) = build(target);
            let compiled = super::trees::pattern(pattern);
            let oracle: Vec<String> = Oracle {
                pattern: &compiled,
                target: &built,
            }
            .find()
            .into_iter()
            .map(|m| built.text()[m.range].to_owned())
            .collect();
            assert_eq!(fast, oracle, "{pattern} over {target} against the oracle");
        }
    }

    /// Where fixed code could stand compares a node's parts in place: a token where the code has
    /// a child is not a place it stands.
    #[test]
    fn fixed_code_does_not_stand_where_a_token_meets_a_child() {
        let (fast, slow) = both("(B $... (k x ; y))", "(B w (k x y z) (k x ; y))");
        assert_eq!(fast, slow);
        assert_eq!(slow, ["w x y z x ; y"]);
    }

    /// A choice reported by more than one list - a segment's, and a node's that binds the same
    /// name used later - never conflicts with itself, so one such choice is accepted.
    #[test]
    fn a_choice_reported_by_more_than_one_list_does_not_conflict_with_itself() {
        assert_eq!(pattern("w $... (f $a... x) $... (g $a...)").costly(), None);
    }

    /// How deep a list stands never changes whether testing its names is too costly: a node's
    /// ways are one choice in every list enclosing it, however many names they move, and a name
    /// bound before any sequence, or after the last, binds one way.  Two sequences moving names
    /// of one node still make two choices, wherever the node is.
    #[test]
    fn testing_names_costs_the_same_however_deep_their_list_stands() {
        for (list, tested, costly) in [
            ("(g $b $q... $a)", &["a", "b"][..], false),
            ("(g $q... $a $b)", &["a", "b"][..], false),
            ("(g $b $q... $a)", &["b"][..], false),
            // The last sequence's stop is settled where its list ends, so `$b` binds one way.
            ("(g $q... $a $p... $b)", &["a", "b"][..], false),
            // Two names two stops move are every pair of them to try, the square, and three
            // the cube.
            ("(g $q... $a $p... $b $r...)", &["a", "b"][..], false),
            (
                "(g $q... $a $p... $b $r... $c $s...)",
                &["a", "b", "c"][..],
                true,
            ),
            // A node matches its whole list, so a name with no sequence after it is pinned to
            // the node's end and binds one way.
            ("(f (g $q... $a) (h $p... $b))", &["a", "b"][..], false),
            // Each node's name moves with its own stop: every pair of them, the square, and
            // with a third node every triple, the cube.
            (
                "(f (g $q... $a $r...) (h $p... $b $s...))",
                &["a", "b"][..],
                false,
            ),
            (
                "(f (g $q... $a $r...) (h $p... $b $s...) (k $t... $c $u...))",
                &["a", "b", "c"][..],
                true,
            ),
            // A list's own stop, and a node's beside it: each free only where something after
            // it can move, and two together the square.
            ("(f $q... $a (g $p... $b))", &["a", "b"][..], false),
            (
                "(f $q... $a (g $p... $b $s...) $r...)",
                &["a", "b"][..],
                false,
            ),
        ] {
            for wrapped in [
                list.to_owned(),
                format!("(D {list})"),
                format!("(D (D {list}))"),
            ] {
                assert_eq!(
                    pattern(&wrapped).costly_with(tested).is_some(),
                    costly,
                    "{wrapped} testing {tested:?}"
                );
            }
        }
        // Nor does a name bound first in a node make a choice where it is used again.
        assert_eq!(pattern("(g $a $q...) y $q... x x (g $a y)").costly(), None);
    }

    /// A name used twice repeats its code within one list: where the code it holds would run on
    /// into the next list of the node, it does not stand there, though the code reads alike.
    #[test]
    fn repeated_code_stays_in_the_list_it_starts_in() {
        let (fast, slow) = both("(D $a... x $a... y)", "(D [p q] x [p] [q] y)");
        assert_eq!(fast, Vec::<String>::new());
        assert_eq!(slow, Vec::<String>::new());
        // Where it fits in one list, it stands.
        let (fast, slow) = both("(D $a... x $a... y)", "(D [p q] x [p q] y)");
        assert_eq!(fast, ["p q x p q y"]);
        assert_eq!(slow, fast);
    }

    /// Each rule of the cost count, by a pattern whose verdict turns on it: with the search
    /// counted as the matcher makes it, each refused pattern makes some state depend on three of
    /// the places a search can go more than one way, and each accepted one on no more than two.
    #[test]
    fn the_cost_check_counts_the_states_the_matcher_makes() {
        for (text, tested, costly) in [
            // A sequence followed straight by another is not settled at the first place the code
            // after it stands, since there is none, so both try every stop.
            ("(D y $q... $... $... $q...)", &[][..], true),
            // A sequence followed by fixed code that binds nothing the rest reads is settled
            // there in a plain search, and tries no other stop.
            ("(D $p... $... y $... $p... x y)", &[][..], false),
            // But not where the fixed code after it is the last of a node's list, which only the
            // place that ends the list settles.
            ("(D $p... $... $q... (g $p...))", &[][..], true),
            // A sequence whose ways a `where` gathers shares them between the places that ask
            // only where more than one does: in a run, or after a free sequence in its list.
            ("x ; $p... (g (g $b $... $p...))", &["b"][..], false),
            // And only where nothing reads or carries what it takes.
            (
                "y (g y) $q... (g y $q... $a $p...) $b y",
                &["a", "b"][..],
                false,
            ),
            // A name bound inside a later part is read only after it is bound there.
            (
                "$b $... (g x $... $b $p...) (g $p...) $p... x",
                &["b"][..],
                false,
            ),
            (
                "(D (g x x) $q... $... $q... $p... $b (g (g x x) $p...))",
                &[][..],
                false,
            ),
            // A plain search matches a pattern of one node plainly, and only the `where` gathers
            // its ways, so the plain search's own states count too.
            (
                "(D (g (g $... $b $q...)) $p... $... x $b)",
                &["b"][..],
                true,
            ),
        ] {
            assert_eq!(
                pattern(text).costly_with(tested).is_some(),
                costly,
                "{text} testing {tested:?}"
            );
        }
    }

    /// A `where` over a run of statements tries each way of binding its name at each start, and
    /// what follows the name is worked out once whatever the name holds and wherever the run
    /// started, so the work grows with the square of the run's length: four times over twice as
    /// many statements, not eight.
    #[test]
    fn a_where_over_a_run_stays_quadratic_however_many_starts_it_has() {
        let compiled = pattern("x $... $a $... y");
        assert_eq!(compiled.costly_with(&["a"]), None);
        let work = |n: usize| {
            let (built, _) = build(&format!("[{}]", filled("x $... $a $... y", n)));
            WORK.with(|work| work.set(0));
            assert_eq!(
                find_where(&compiled, &built, &["a"], &mut |_| false),
                Vec::<Match>::new()
            );
            WORK.with(std::cell::Cell::get)
        };
        let (small, large) = (work(32), work(64));
        assert!(large < 6 * small, "{small} then {large}");
    }

    #[test]
    fn a_where_over_a_run_captures_a_name_inside_a_node_that_binds_a_tested_one() {
        // `$a` is tested, so the first statement is matched by its ways of binding `$a`, which
        // say nothing of `$p...` inside it; the capture must still be recorded.
        let compiled = pattern("(g (g $p...) $a) y");
        let (built, _) = build("[(g (g x) x) y]");
        let found = find_where_captured(&compiled, &built, &["a"], &mut |_| true, &["p", "a"]);
        let texts: Vec<Vec<&str>> = found
            .iter()
            .map(|found| {
                found
                    .captures
                    .iter()
                    .map(|r| &built.text()[r.clone()])
                    .collect()
            })
            .collect();
        assert_eq!(texts, [["x", "x"]]);
    }

    #[test]
    fn a_where_over_a_run_captures_the_way_it_accepted_inside_a_node() {
        // `$a` is tested, and the way the `where` accepts binds it to the fourth `x`; matched
        // again by code alone, it would bind the second, an equal `x`, and `$p...` and `$q...`
        // would be kept from that other way, overlapping `$a`.
        let compiled = pattern("$b (g $p... $a $q...)");
        let (built, _) = build("[(g x) [x] x [x (g x x) x x (g x x x x x)]]");
        let mut placed = |nodes: &[NodeId]| built.range(nodes[0]).start % 3 == 1;
        let check = capture_check(&compiled, &built, &["a"], Some(&mut placed));
        assert_eq!(check.holds(), Ok(()));
        let texts: Vec<Vec<std::ops::Range<usize>>> = check
            .fast
            .iter()
            .map(|found| found.captures.clone())
            .collect();
        // `$a`, `$b`, `$p...` and `$q...`, in that order.
        assert_eq!(texts, [[22..23, 14..15, 16..21, 24..25]]);
    }

    #[test]
    fn a_where_over_a_run_captures_a_name_compared_only_inside_a_node_it_tests() {
        // `$a` is tested, so the node is matched by its ways of binding `$a`; `$q...` is used
        // twice, but only inside the node, so no way binds it, and its capture must still be
        // recorded.
        let compiled = pattern("y (g $q... $q... $a)");
        let (built, _) = build("[y (g y y x)]");
        let found = find_where_captured(&compiled, &built, &["a"], &mut |_| true, &["q", "a"]);
        let texts: Vec<Vec<&str>> = found
            .iter()
            .map(|found| {
                found
                    .captures
                    .iter()
                    .map(|r| &built.text()[r.clone()])
                    .collect()
            })
            .collect();
        assert_eq!(texts, [["y", "x"]]);
    }

    #[test]
    fn a_where_over_a_run_that_tells_equal_code_apart_captures_the_way_it_accepted() {
        // Three `x`s; only the third passes.  Made again with `$x` bound by code, the run would
        // end two statements early, at the second `x`'s `y`.
        let compiled = pattern("w $... $x y");
        let (built, _) = build("[w x x y x y]");
        let third = built.text().rfind('x').expect("the target has an x");
        let found = find_where_captured(
            &compiled,
            &built,
            &["x"],
            &mut |nodes| built.range(nodes[0]).start == third,
            &["x"],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range, 0..built.text().len());
        assert_eq!(found[0].captures[0].start, third);
    }

    /// A sequence after another in a node's list is moved by both their stops, so it binds as many
    /// ways as the square of the list's length, and using it again after the node makes a third
    /// place the search depends on: refused, as searching it costs the cube, eight times the work
    /// over twice the code where no way matches.  With no sequence after it, it ends where the node
    /// does, and only one stop moves it.
    #[test]
    fn a_sequence_after_another_is_moved_by_both_their_stops() {
        let both = pattern("(D (g $... $q... $...) $... $q... x y)");
        assert!(matches!(both.costly(), Some(Costly::ThirdPlace(_))));
        let work = |n: usize| {
            let target = format!(
                "(D (g {}) {})",
                vec!["x"; n].join(" "),
                vec!["x y"; n].join(" ")
            );
            let (built, _) = build(&target);
            WORK.with(|work| work.set(0));
            assert_eq!(
                find_in(&both, &built, true, &[], None, None, &[], &[]),
                Vec::<Captured>::new()
            );
            WORK.with(std::cell::Cell::get)
        };
        let (small, large) = (work(16), work(32));
        assert!(large > 6 * small, "{small} then {large}");
        // Compared once after the node, its square of ways cost the square, as the node's own
        // search does; and with no sequence after it, it ends where the node does.
        assert_eq!(pattern("(D (g $... $q... $...) $q...)").costly(), None);
        assert_eq!(pattern("(D (g $... $q...) $q...)").costly(), None);
    }

    /// A node asked for its ways: each is one length of `$a`, and the one `g` needs is the
    /// longest, so every way is tried.  The work grows with the square of the arguments' number,
    /// four times over twice as many, not eight.
    #[test]
    fn a_node_asked_for_every_way_stays_quadratic() {
        let pattern = pattern("(B (f $a... $b...) (g $a...))");
        assert_eq!(pattern.costly(), None);
        let work = |n: usize| {
            let target = format!(
                "(B (f {}) (g {}))",
                vec!["x"; n].join(" "),
                vec!["x"; n - 1].join(" ")
            );
            let (target, _) = build(&target);
            let (found, work) = find_counting(&pattern, &target);
            assert_eq!(found.len(), 1);
            work
        };
        let (small, large) = (work(100), work(200));
        assert!(large < 5 * small, "{small} then {large}");
    }

    /// Items as a node's or list's children, with no token at either end and at least one child,
    /// as a parser makes them.
    fn trimmed(items: &[String]) -> String {
        let start = items.iter().position(|item| item != ";");
        let end = items.iter().rposition(|item| item != ";");
        match (start, end) {
            (Some(start), Some(end)) => items[start..=end].join(" "),
            _ => "x".to_owned(),
        }
    }

    proptest! {
        // About one generated run in five is assumed away - nothing but holes, a sequence at its
        // edge, or too costly - which the default limit on rejects leaves little room for.
        #![proptest_config(ProptestConfig {
            cases: 4000,
            max_global_rejects: 4096,
            ..ProptestConfig::default()
        })]

        #[test]
        fn remembering_finds_what_backtracking_finds_in_nested_trees(
            pattern in prop::collection::vec(pattern_item(2), 1..7),
            target in prop::collection::vec(target_item(2), 1..12),
        ) {
            let pattern = format!("(D {})", trimmed(&pattern));
            let target = format!("(D {})", trimmed(&target));
            let (fast, slow) = both(&pattern, &target);
            prop_assert_eq!(&fast, &slow, "{} over {}", pattern, target);
            let (built, _) = build(&target);
            let compiled = super::trees::pattern(&pattern);
            let oracle: Vec<String> = Oracle { pattern: &compiled, target: &built }
                .find()
                .into_iter()
                .map(|m| built.text()[m.range].to_owned())
                .collect();
            prop_assert_eq!(fast, oracle, "{} over {} against the oracle", pattern, target);
            capture_check(&compiled, &built, &[], None)
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{pattern} over {target}: {err}")))?;
        }

        /// A `where` keeps exactly the matches the oracle finds a way for whose code for the
        /// tested name passes - here, has a given length - so no way of binding it is missed.
        #[test]
        fn a_where_keeps_what_the_oracle_finds_a_passing_way_for(
            pattern in prop::collection::vec(pattern_item(2), 1..7),
            target in prop::collection::vec(target_item(2), 1..12),
            length in 1usize..5,
        ) {
            let pattern = format!("(D {})", trimmed(&pattern));
            let target = format!("(D {})", trimmed(&target));
            let compiled = super::trees::pattern(&pattern);
            let single = |name: &str| {
                compiled
                    .holes()
                    .any(|(_, hole)| hole.name.as_deref() == Some(name) && !hole.sequence)
                    && !compiled
                        .holes()
                        .any(|(_, hole)| hole.name.as_deref() == Some(name) && hole.sequence)
            };
            let Some(name) = ["a", "b"].into_iter().find(|name| single(name)) else {
                return Ok(());
            };
            prop_assume!(compiled.unbounded_sequence().is_none());
            prop_assume!(compiled.costly_with(&[name]).is_none());
            let (built, _) = build(&target);
            let same = Same { arena: &built };
            let kept: Vec<String> = find_where(&compiled, &built, &[name], &mut |nodes| {
                let mut code = Vec::new();
                same.code(&[Part::Child(nodes[0])], &mut code);
                code.len() == length
            })
            .into_iter()
            .map(|m| built.text()[m.range].to_owned())
            .collect();
            let oracle: Vec<String> = Oracle { pattern: &compiled, target: &built }
                .find_where(&mut |held| held.get(name).is_some_and(|code| code.len() == length))
                .into_iter()
                .map(|m| built.text()[m.range].to_owned())
                .collect();
            prop_assert_eq!(kept, oracle, "{} over {} testing ${}", pattern, target, name);
        }

        /// The same over a run of statements, where a match's end is a way's too.
        #[test]
        fn a_where_over_a_run_keeps_what_the_oracle_finds_a_passing_way_for(
            pattern in prop::collection::vec(pattern_item(2), 2..7),
            target in prop::collection::vec(target_item(2), 1..12),
            length in 1usize..5,
        ) {
            let pattern = trimmed(&pattern);
            // A pattern compiler refuses a pattern of nothing but holes.
            prop_assume!(pattern.split_whitespace().any(|word| !word.starts_with('$')));
            let target = format!("[{}]", trimmed(&target));
            let compiled = super::trees::pattern(&pattern);
            let single = |name: &str| {
                compiled
                    .holes()
                    .any(|(_, hole)| hole.name.as_deref() == Some(name) && !hole.sequence)
                    && !compiled
                        .holes()
                        .any(|(_, hole)| hole.name.as_deref() == Some(name) && hole.sequence)
            };
            let Some(name) = ["a", "b"].into_iter().find(|name| single(name)) else {
                return Ok(());
            };
            prop_assume!(compiled.unbounded_sequence().is_none());
            prop_assume!(compiled.costly_with(&[name]).is_none());
            let (built, _) = build(&target);
            let same = Same { arena: &built };
            let kept: Vec<String> = find_where(&compiled, &built, &[name], &mut |nodes| {
                let mut code = Vec::new();
                same.code(&[Part::Child(nodes[0])], &mut code);
                code.len() == length
            })
            .into_iter()
            .map(|m| built.text()[m.range].to_owned())
            .collect();
            let oracle: Vec<String> = Oracle { pattern: &compiled, target: &built }
                .find_where(&mut |held| held.get(name).is_some_and(|code| code.len() == length))
                .into_iter()
                .map(|m| built.text()[m.range].to_owned())
                .collect();
            prop_assert_eq!(kept, oracle, "{} over {} testing ${}", pattern, target, name);
            // The captures are a passing way's, whichever way the search went.
            let mut passes = |nodes: &[NodeId]| {
                let mut code = Vec::new();
                same.code(&[Part::Child(nodes[0])], &mut code);
                code.len() == length
            };
            let check = capture_check(&compiled, &built, &[name], Some(&mut passes));
            check
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{pattern} over {target}: {err}")))?;
            let mut names: Vec<&str> =
                compiled.holes().filter_map(|(_, hole)| hole.name.as_deref()).collect();
            names.sort_unstable();
            names.dedup();
            let at = names.iter().position(|n| *n == name).expect("the tested name is bound");
            for found in &check.fast {
                let node = built
                    .nodes_within(found.captures[at].clone())
                    .find(|&node| built.range(node) == found.captures[at])
                    .expect("a single hole captures a node");
                prop_assert!(passes(&[node]), "{} over {} testing ${}", pattern, target, name);
            }
            let mut placed = |nodes: &[NodeId]| built.range(nodes[0]).start % 3 == length % 3;
            let check = capture_check(&compiled, &built, &[name], Some(&mut placed));
            check
                .holds()
                .map_err(|err| TestCaseError::fail(format!("{pattern} over {target}: {err}")))?;
            for found in &check.fast {
                prop_assert_eq!(found.captures[at].start % 3, length % 3, "{} over {}", pattern, target);
            }
        }

        #[test]
        fn the_common_prefix_of_two_places_is_read_exactly(
            sequence in prop::collection::vec(0u32..3, 1..60),
            a in 0usize..60,
            b in 0usize..60,
        ) {
            let (a, b) = (a % sequence.len(), b % sequence.len());
            let plain = sequence[a..]
                .iter()
                .zip(&sequence[b..])
                .take_while(|(x, y)| x == y)
                .count();
            prop_assert_eq!(Common::new(&sequence).common(a, b), plain);
        }
    }

    /// `pattern` as code it matches: each hole filled with `x`, and each sequence with `length`
    /// of them, so that every sequence can stop in many places and every way is open.
    fn filled(pattern: &str, length: usize) -> String {
        let sequence = vec!["x"; length].join(" ");
        words(pattern)
            .iter()
            .map(|word| match word.strip_prefix('$') {
                Some(hole) if hole.ends_with("...") => sequence.as_str(),
                Some(_) => "x",
                None => word.as_str(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// [`filled`], with the last word the pattern itself spells changed to one it never does,
    /// so that the match fails only at its end, once every way before has been tried: a search
    /// that stops at the first way that matches is made to try them all.  None for a pattern
    /// that spells no word.
    fn spoiled(pattern: &str, length: usize) -> Option<String> {
        let mut out = Vec::new();
        let mut last = None;
        for word in words(pattern) {
            match word.strip_prefix('$') {
                Some(hole) if hole.ends_with("...") => out.extend(vec!["x".to_owned(); length]),
                Some(_) => out.push("x".to_owned()),
                None => {
                    if word == "x" || word == "y" {
                        last = Some(out.len());
                    }
                    out.push(word);
                }
            }
        }
        out[last?] = "z".to_owned();
        Some(out.join(" "))
    }

    /// The code [`growth`] measures a pattern over, in pairs of the same code at two lengths:
    /// the pattern [`filled`] with 8 and then 32 for each sequence, and the same [`spoiled`]
    /// where the pattern spells a word.  A run of statements is searched among a block's.
    fn growth_targets(text: &str) -> Vec<Arena<'static>> {
        let target = |code: String| {
            let code = if text.starts_with("(D ") {
                code
            } else {
                format!("[{code}]")
            };
            build(&code).0
        };
        let mut built: Vec<Arena<'static>> = [8, 32]
            .into_iter()
            .map(|length| target(filled(text, length)))
            .collect();
        if let (Some(small), Some(large)) = (spoiled(text, 8), spoiled(text, 32)) {
            built.extend([target(small), target(large)]);
        }
        built
    }

    /// The names a `where` could test in `compiled`, alone and together: those it binds to one
    /// node, of `a` and `b`, with testing nothing first.
    fn testable(compiled: &Pattern) -> Vec<&'static [&'static str]> {
        let single = |name: &str| {
            compiled
                .holes()
                .any(|(_, hole)| hole.name.as_deref() == Some(name) && !hole.sequence)
                && !compiled
                    .holes()
                    .any(|(_, hole)| hole.name.as_deref() == Some(name) && hole.sequence)
        };
        [&[][..], &["a"], &["b"], &["a", "b"]]
            .into_iter()
            .filter(|tested| tested.iter().all(|name| single(name)))
            .collect()
    }

    /// The matcher's work searching each of `built` for `compiled`, testing `tested`, with every
    /// way of binding them rejected so that all of them are tried.
    fn work_over(compiled: &Pattern, built: &[Arena<'static>], tested: &[&str]) -> Vec<usize> {
        built
            .iter()
            .map(|target| {
                WORK.with(|work| work.set(0));
                let mut reject = |_: &[NodeId]| false;
                let accept: Option<&mut Accept<'_>> = (!tested.is_empty()).then_some(&mut reject);
                let _ = find_in(compiled, target, true, tested, accept, None, &[], &[]);
                WORK.with(std::cell::Cell::get)
            })
            .collect()
    }

    /// Whether the work over the same code at two lengths, the second four times the first,
    /// grew faster than the square: more than thirty-two times, between the square's sixteen
    /// and the cube's sixty-four.
    fn steep(pair: &[usize]) -> bool {
        pair[1] > 32 * pair[0].max(1)
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 1000,
            max_global_rejects: 100_000,
            ..ProptestConfig::default()
        })]

        /// No pattern the cost check accepts makes the matcher's work grow faster than the
        /// square of the code it searches, measured as the corpus is
        /// ([`the_cost_check_gives_the_corpus_its_recorded_verdicts`]) but over patterns
        /// generated anew from each seed, deeper than the corpus's, so that a shape the corpus
        /// never holds is still met in time.
        #[test]
        fn no_accepted_pattern_costs_more_than_the_square(
            items in prop::collection::vec(pattern_item(3), 1..10),
            run in any::<bool>(),
        ) {
            let items = trimmed(&items);
            // A pattern compiler refuses a pattern of nothing but holes.
            prop_assume!(!run || items.split_whitespace().any(|word| !word.starts_with('$')));
            let text = if run { items } else { format!("(D {items})") };
            let compiled = super::trees::pattern(&text);
            prop_assume!(compiled.unbounded_sequence().is_none());
            let built = growth_targets(&text);
            for tested in testable(&compiled) {
                if compiled.costly_with(tested).is_some() {
                    continue;
                }
                let work = work_over(&compiled, &built, tested);
                for pair in work.chunks(2) {
                    prop_assert!(
                        !steep(pair),
                        "{} testing {:?}: {} then {}",
                        text,
                        tested,
                        pair[0],
                        pair[1]
                    );
                }
            }
        }
    }

    /// The cost check's verdict on a fixed corpus of generated patterns, with the work each
    /// search takes, as a snapshot, and no accepted pattern's work growing faster than the
    /// square of the code it searches: so that a change to the check - relaxing a limit, say -
    /// fails where it lets a costlier pattern through, and shows as a diff naming every pattern
    /// whose verdict moved, and what searching it cost.
    ///
    /// Each line is one pattern - a run of statements, or a node `D` over its children - then its
    /// verdict alone and with each name a `where` could test: `ok`, or why it is refused and at
    /// which hole.  A run with a sequence at its edge is left out, since it is refused before
    /// cost is asked.  The numbers after each verdict are the matcher's work over the pattern
    /// [`filled`] with 8 and then 32 for each sequence, then over the same [`spoiled`] where the
    /// pattern spells a word, searched whatever the verdict, with every way of binding the
    /// tested names rejected so that all of them are tried.  Four times the
    /// code is sixteen times the work where it grows with the square, and sixty-four with the
    /// cube; an accepted pattern's growing more than thirty-two times fails.  This is work on
    /// these targets, not a bound: they are not shown to be any pattern's worst case, so a
    /// pattern that passes is not shown to be cheap everywhere.
    ///
    /// The corpus comes from a fixed seed through proptest, whose version is pinned; a new
    /// version may generate other patterns, and the snapshot is then regenerated as a whole.
    #[test]
    fn the_cost_check_gives_the_corpus_its_recorded_verdicts() {
        use proptest::strategy::ValueTree;
        use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
        use std::fmt::Write as _;

        let mut runner = TestRunner::new_with_rng(
            Config::default(),
            TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]),
        );
        let mut patterns = BTreeSet::new();
        for _ in 0..1000 {
            let strategy = prop::collection::vec(pattern_item(2), 1..7);
            let items = trimmed(&strategy.new_tree(&mut runner).unwrap().current());
            patterns.insert(format!("(D {items})"));
            // A pattern compiler refuses a pattern of nothing but holes.
            if items.split_whitespace().any(|word| !word.starts_with('$')) {
                patterns.insert(items);
            }
        }

        let mut out = String::new();
        let mut steep_found = Vec::new();
        for text in &patterns {
            let compiled = super::trees::pattern(text);
            // A sequence at the edge of a run is refused before cost is asked, and has no search
            // to measure.
            if compiled.unbounded_sequence().is_some() {
                continue;
            }
            let built = growth_targets(text);
            out.push_str(text);
            for tested in testable(&compiled) {
                let verdict = match compiled.costly_with(tested) {
                    None => "ok".to_owned(),
                    Some(Costly::Together(hole)) => {
                        format!("together at {} {}", hole.0, compiled.arena().source(hole))
                    }
                    Some(Costly::ThirdPlace(hole)) => {
                        format!("third at {} {}", hole.0, compiled.arena().source(hole))
                    }
                };
                let work = work_over(&compiled, &built, tested);
                let tested = if tested.is_empty() {
                    "alone".to_owned()
                } else {
                    tested.join(" ")
                };
                for pair in work.chunks(2) {
                    if verdict == "ok" && steep(pair) {
                        steep_found.push(format!(
                            "{text} testing {tested}: {} then {}",
                            pair[0], pair[1]
                        ));
                    }
                }
                let work: Vec<String> = work.iter().map(usize::to_string).collect();
                write!(out, " | {tested}: {verdict} {}", work.join(" "))
                    .expect("a string takes it");
            }
            out.push('\n');
        }
        assert!(
            steep_found.is_empty(),
            "accepted, but its work grows faster than the square:\n{}",
            steep_found.join("\n")
        );
        insta::assert_snapshot!(out);
    }
}
