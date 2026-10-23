// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A structural pattern: a snippet of a language parsed into an arena, with holes.

use crate::{Arena, NodeId};

/// One hole in a pattern: what it binds, and whether it stands for one node or a sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hole {
    /// The name it binds, or `None` for `$_` and `$...`, which bind nothing.
    pub name: Option<String>,
    /// Whether it stands for a sequence of one or more nodes, `$name...` and `$...`, rather
    /// than one node.
    pub sequence: bool,
}

/// A pattern: its own arena, and the nodes in it that are holes.
///
/// A hole is a node of the arena.  The node stands where the hole was written; what it is -
/// a name, a statement - is whatever the language's parser made of the placeholder the hole
/// was replaced by, and the matcher does not look inside it.
#[derive(Debug, Clone)]
pub struct Pattern {
    arena: Arena<'static>,
    holes: Vec<(NodeId, Hole)>,
}

impl Pattern {
    /// A pattern from its arena and its holes, each a node of that arena.
    ///
    /// # Panics
    ///
    /// If a hole names a node the arena does not have, or the same node twice, which is a
    /// defect in the language's pattern compiler.
    #[must_use]
    pub fn new(arena: Arena<'static>, mut holes: Vec<(NodeId, Hole)>) -> Pattern {
        holes.sort_by_key(|(node, _)| *node);
        assert!(
            holes
                .iter()
                .all(|(node, _)| arena.nodes().any(|n| n == *node)),
            "a pattern compiler named a hole outside its arena"
        );
        assert!(
            holes.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "a pattern compiler named one node as two holes"
        );
        Pattern { arena, holes }
    }

    /// The pattern's arena, over the text its language's parser was given.
    #[must_use]
    pub fn arena(&self) -> &Arena<'static> {
        &self.arena
    }

    /// The hole `node` is, if it is one.
    #[must_use]
    pub fn hole(&self, node: NodeId) -> Option<&Hole> {
        self.holes
            .binary_search_by_key(&node, |(n, _)| *n)
            .ok()
            .map(|at| &self.holes[at].1)
    }

    /// Every hole, in the arena's pre-order.
    pub fn holes(&self) -> impl Iterator<Item = (NodeId, &Hole)> {
        self.holes.iter().map(|(node, hole)| (*node, hole))
    }

    /// A sequence hole at the start or end of a pattern of several statements in a row, if the
    /// pattern has one, the leading one first: a hole nothing in the pattern bounds on that
    /// side.  Every language's compiler refuses such a pattern, and [`crate::find`] treats one
    /// as a defect in the compiler.
    ///
    /// A run of statements is matched wherever it can start, so such a hole would reach to the
    /// edge of the block it stands in - at a module's top level, most of a file - and a match
    /// that far beyond what the pattern spells is one an edit would destroy code with.  A
    /// sequence with fixed code on both sides, or inside a node such as a call or a function
    /// body, is bounded by that code and is not one of these.
    #[must_use]
    pub fn unbounded_sequence(&self) -> Option<NodeId> {
        let content: Vec<NodeId> = self.arena.children(self.arena.root()).collect();
        if content.len() < 2 {
            return None;
        }
        [content[0], content[content.len() - 1]]
            .into_iter()
            .find(|node| self.hole(*node).is_some_and(|hole| hole.sequence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Builder, Kind, Language};

    fn call() -> Arena<'static> {
        let mut builder = Builder::new(Language::new("toy"), "f(x)").unwrap();
        builder.enter(Kind::new("Call"), 0..4);
        builder.enter(Kind::new("Name"), 0..1);
        builder.leave();
        builder.enter(Kind::new("Name"), 2..3);
        builder.leave();
        builder.leave();
        builder.finish().into_owned()
    }

    fn bound(name: &str) -> Hole {
        Hole {
            name: Some(name.to_owned()),
            sequence: false,
        }
    }

    #[test]
    fn a_hole_is_found_by_its_node_and_the_rest_are_not_holes() {
        let arena = call();
        let nodes: Vec<_> = arena.nodes().collect();
        let pattern = Pattern::new(arena, vec![(nodes[2], bound("x"))]);
        assert_eq!(pattern.hole(nodes[2]), Some(&bound("x")));
        assert_eq!(pattern.hole(nodes[0]), None);
        assert_eq!(pattern.hole(nodes[1]), None);
        assert_eq!(pattern.holes().count(), 1);
        assert_eq!(pattern.arena().source(nodes[2]), "x");
    }

    #[test]
    fn holes_are_kept_in_pre_order_whatever_order_they_are_given_in() {
        let arena = call();
        let nodes: Vec<_> = arena.nodes().collect();
        let pattern = Pattern::new(arena, vec![(nodes[2], bound("b")), (nodes[1], bound("a"))]);
        let names: Vec<_> = pattern
            .holes()
            .map(|(_, h)| h.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    /// Two statements, `a` then `b`, as children of a module, with the given nodes as sequence
    /// holes: 1 is `a`, 2 is `b`.
    fn run_of_two(sequences: &[usize]) -> Pattern {
        let mut builder = Builder::new(Language::new("toy"), "a\nb").unwrap();
        builder.enter(Kind::new("Module"), 0..3);
        builder.enter(Kind::new("Name"), 0..1);
        builder.leave();
        builder.enter(Kind::new("Name"), 2..3);
        builder.leave();
        builder.leave();
        let arena = builder.finish().into_owned();
        let nodes: Vec<_> = arena.nodes().collect();
        let holes = sequences
            .iter()
            .map(|&i| {
                (
                    nodes[i],
                    Hole {
                        name: None,
                        sequence: true,
                    },
                )
            })
            .collect();
        Pattern::new(arena, holes)
    }

    #[test]
    fn a_sequence_at_either_end_of_a_run_is_unbounded_and_the_leading_one_is_named_first() {
        let nodes: Vec<_> = run_of_two(&[]).arena().nodes().collect();
        assert_eq!(run_of_two(&[1]).unbounded_sequence(), Some(nodes[1]));
        assert_eq!(run_of_two(&[2]).unbounded_sequence(), Some(nodes[2]));
        assert_eq!(run_of_two(&[1, 2]).unbounded_sequence(), Some(nodes[1]));
        assert_eq!(run_of_two(&[]).unbounded_sequence(), None);
    }

    #[test]
    fn a_sequence_inside_one_node_is_bounded_by_it() {
        // `f(x)` as the one statement of a module, with `x` a sequence: the call bounds it.
        let mut builder = Builder::new(Language::new("toy"), "f(x)").unwrap();
        builder.enter(Kind::new("Module"), 0..4);
        builder.enter(Kind::new("Call"), 0..4);
        builder.enter(Kind::new("Name"), 0..1);
        builder.leave();
        builder.enter(Kind::new("Name"), 2..3);
        builder.leave();
        builder.leave();
        builder.leave();
        let arena = builder.finish().into_owned();
        let x = arena.nodes().nth(3).unwrap();
        let pattern = Pattern::new(
            arena,
            vec![(
                x,
                Hole {
                    name: None,
                    sequence: true,
                },
            )],
        );
        assert_eq!(pattern.unbounded_sequence(), None);
    }

    #[test]
    #[should_panic(expected = "a pattern compiler let through a sequence hole at the edge")]
    fn matching_a_pattern_with_an_unbounded_sequence_is_a_defect() {
        let pattern = run_of_two(&[1]);
        let target = run_of_two(&[]).arena().clone();
        let _ = crate::find(&pattern, &target);
    }

    #[test]
    #[should_panic(expected = "a pattern compiler named one node as two holes")]
    fn one_node_named_twice_is_a_defect() {
        let arena = call();
        let node = arena.nodes().nth(1).unwrap();
        let _ = Pattern::new(arena, vec![(node, bound("a")), (node, bound("b"))]);
    }

    #[test]
    #[should_panic(expected = "a pattern compiler named a hole outside its arena")]
    fn a_hole_outside_the_arena_is_a_defect() {
        let _ = Pattern::new(call(), vec![(NodeId(7), bound("a"))]);
    }
}
