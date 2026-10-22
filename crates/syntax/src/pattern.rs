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
