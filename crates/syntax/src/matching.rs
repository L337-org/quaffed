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
        // A sequence hole at either edge of the pattern takes as much of the block as it can: a
        // leading one from the block's first statement, a trailing one to its last.
        let edge = |part: Option<&Part<'_>>| match part {
            Some(Part::Child(child)) => pattern.hole(*child).is_some_and(|hole| hole.sequence),
            _ => false,
        };
        let leading = edge(wanted.first());
        let mut greedy = Vec::new();
        for part in [wanted.first(), wanted.last()] {
            if edge(part)
                && let Some(Part::Child(child)) = part
            {
                greedy.push(*child);
            }
        }
        for node in target.nodes() {
            let available = parts(target, node);
            let limits = list_ends(target, &available);
            let mut previous = None;
            for (start, first) in available.iter().enumerate() {
                let Part::Child(first) = first else { continue };
                let list = target.list(*first);
                let begins_list = previous != Some(list);
                previous = Some(list);
                // A run of statements stays in one list - one block's statements - so it never
                // takes in a header, a clause, or another block.
                if list.is_none() || (leading && !begins_list) {
                    continue;
                }
                let limit = limits[start];
                // Matched from this start as far as the pattern goes: one try per start, whatever
                // the run's length.
                let mut bindings = Bindings::new();
                if let Some(taken) = Matcher::new(pattern, target).greedy(&greedy).consume(
                    &wanted,
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
    }
    found.sort_by_key(|m| (m.range.start, m.range.end));
    found.dedup();
    found
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
/// a name are always of the same form.  A run shares the parts it was cut from, so trying a
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
    /// The sequence holes that take the longest run they can rather than the shortest: those at
    /// the edges of a pattern of several statements.
    greedy: Vec<NodeId>,
}

impl<'p, 't> Matcher<'p, 't> {
    fn new(pattern: &'p Pattern, target: &'t Arena<'_>) -> Matcher<'p, 't> {
        Matcher {
            shape: pattern.arena(),
            pattern,
            target,
            greedy: Vec::new(),
        }
    }

    fn greedy(mut self, holes: &[NodeId]) -> Self {
        self.greedy = holes.to_vec();
        self
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
    /// match took, the shortest way; with `whole`, only a match that takes them all counts.
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
                    // One or more of the available parts, from a child to a child, shortest
                    // first; whichever split matches first is kept.
                    let Some(Part::Child(first)) = available.first() else {
                        return None;
                    };
                    // A run stays in one list, so `$body...` never takes in a clause or a header.
                    let block = self.target.list(*first);
                    let shared: Rc<[Part<'t>]> = available.into();
                    // Within a run's own list, so the longest a greedy hole tries is the rest of
                    // the block.
                    let within = available
                        .iter()
                        .position(|p| matches!(p, Part::Child(c) if self.target.list(*c) != block))
                        .unwrap_or(available.len());
                    let ends: Box<dyn Iterator<Item = usize>> = if self.greedy.contains(child) {
                        Box::new((0..within).rev())
                    } else {
                        Box::new(0..within)
                    };
                    for end in ends {
                        if !matches!(available[end], Part::Child(_)) {
                            continue;
                        }
                        let mut attempt = bindings.clone();
                        let run = Bound::Run(Rc::clone(&shared), 0..end + 1);
                        if bind(self.target, hole, run, &mut attempt)
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
#[derive(Debug, PartialEq, Eq)]
enum Code<'a> {
    Token(&'a str),
    Value(&'a str),
    Text(&'a str),
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
