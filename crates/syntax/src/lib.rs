// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The syntax arena: a parsed file as node kinds and byte ranges over its text, the same for
//! every language.
//!
//! Each language's adapter parses with its own parser and builds an [`Arena`] with a
//! [`Builder`].  The engine is to read arenas and never a parser's own types, which is why this
//! crate depends on nothing: `tests/no_dependencies.rs` holds that.  `architecture/syntax.md`
//! describes it.

mod matching;
mod pattern;

pub use matching::{Match, find};
pub use pattern::{Hole, Pattern};

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

/// A kind of node, by the name its language's parser gives it - `StmtFunctionDef` for Python.
///
/// Kinds are compared only within one language, so two languages may use the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind(&'static str);

impl Kind {
    /// The kind called `name`.  An adapter takes its names from its parser, never by hand.
    #[must_use]
    pub const fn new(name: &'static str) -> Kind {
        Kind(name)
    }

    /// The parser's name for this kind.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The language an arena was built from, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Language(&'static str);

impl Language {
    /// The language called `name`.
    #[must_use]
    pub const fn new(name: &'static str) -> Language {
        Language(name)
    }

    /// Its name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A node, by its place in one arena.  It means nothing in any other arena, and nothing after
/// the text is edited and parsed again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

/// One node: its kind, its byte range in the text, and the index just past its subtree.
#[derive(Debug, Clone, Copy)]
struct Node {
    kind: Kind,
    start: u32,
    end: u32,
    after: u32,
    /// The list among its parent's children it belongs to, if the adapter marked one - a
    /// block's statements - so that a run of children never crosses from one into another.
    list: u32,
}

/// A parsed file: every node in pre-order, over the text the parser was given.
///
/// Byte ranges index that text, which for a decoded file is its decoded text rather than its
/// bytes on disk.  A node's children are in source order, do not overlap and lie inside it;
/// the root's range is the parser's, so text may lie before and after it.
#[derive(Debug, Clone)]
pub struct Arena<'text> {
    language: Language,
    text: Cow<'text, str>,
    nodes: Vec<Node>,
    tokens: Vec<Token>,
    values: Vec<(NodeId, String)>,
}

/// One token the matcher compares: the parser's tokens that carry meaning, with comments,
/// whitespace, line breaks, indentation and redundant parentheses left out by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    start: u32,
    end: u32,
    compare: TokenText,
}

/// What a token is compared by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenText {
    /// Its text in the source.
    Source,
    /// A fixed text standing for the token whatever its source says: the opening of an
    /// f-string compares the same whatever its prefix and quotes, as a string's content does.
    Fixed(&'static str),
}

impl<'text> Arena<'text> {
    /// The language whose parser built it.
    #[must_use]
    pub fn language(&self) -> Language {
        self.language
    }

    /// The whole text the parser was given.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many nodes it holds; at least one, the root.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false: an arena has a root.  Present because clippy expects it beside `len`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The root node.
    #[must_use]
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Every node, in pre-order: each node before its children, and children in source order.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + use<'_, 'text> {
        (0..narrow(self.nodes.len())).map(NodeId)
    }

    /// The kind of `node`.
    #[must_use]
    pub fn kind(&self, node: NodeId) -> Kind {
        self.node(node).kind
    }

    /// The list among its parent's children `node` belongs to, a block's statements, or `None`
    /// if the adapter marked none.  Two siblings in one list share it.
    #[must_use]
    pub fn list(&self, node: NodeId) -> Option<u32> {
        match self.node(node).list {
            0 => None,
            list => Some(list),
        }
    }

    /// The byte range of `node` in the text.
    #[must_use]
    pub fn range(&self, node: NodeId) -> Range<usize> {
        let node = self.node(node);
        widen(node.start)..widen(node.end)
    }

    /// The text of `node`.
    #[must_use]
    pub fn source(&self, node: NodeId) -> &str {
        &self.text[self.range(node)]
    }

    /// The children of `node`, in source order.
    #[must_use]
    pub fn children(&self, node: NodeId) -> Children<'_, 'text> {
        Children {
            arena: self,
            next: node.0 + 1,
            end: self.node(node).after,
        }
    }

    /// The text before the root.
    #[must_use]
    pub fn leading(&self) -> &str {
        &self.text[..self.range(self.root()).start]
    }

    /// The text after the root.
    #[must_use]
    pub fn trailing(&self) -> &str {
        &self.text[self.range(self.root()).end..]
    }

    /// The same arena, owning a copy of its text, so that it can outlive the text it was
    /// built over.  A pattern keeps its arena this way.
    #[must_use]
    pub fn into_owned(self) -> Arena<'static> {
        Arena {
            language: self.language,
            text: Cow::Owned(self.text.into_owned()),
            nodes: self.nodes,
            tokens: self.tokens,
            values: self.values,
        }
    }

    /// The tokens the adapter kept that lie inside `range`, in source order.
    #[must_use]
    pub fn tokens_in(&self, range: Range<usize>) -> &[Token] {
        let first = self
            .tokens
            .partition_point(|t| widen(t.start) < range.start);
        let past = self.tokens.partition_point(|t| widen(t.start) < range.end);
        let past = past.max(first);
        let kept = &self.tokens[first..past];
        let inside = kept.partition_point(|t| widen(t.end) <= range.end);
        &kept[..inside]
    }

    /// The byte range of `token` in the text.
    #[must_use]
    pub fn token_range(&self, token: &Token) -> Range<usize> {
        widen(token.start)..widen(token.end)
    }

    /// What `token` is compared by: its source text, or the fixed text the adapter gave it.
    #[must_use]
    pub fn token_text(&self, token: &Token) -> &str {
        match token.compare {
            TokenText::Source => &self.text[self.token_range(token)],
            TokenText::Fixed(text) => text,
        }
    }

    /// The value `node` compares by in place of its children, if the adapter gave it one: a
    /// string literal's content, its quotes and prefix taking no part.
    #[must_use]
    pub fn value(&self, node: NodeId) -> Option<&str> {
        self.values
            .binary_search_by_key(&node, |(n, _)| *n)
            .ok()
            .map(|at| self.values[at].1.as_str())
    }

    fn node(&self, node: NodeId) -> &Node {
        &self.nodes[widen(node.0)]
    }
}

/// The children of one node, in source order.
#[derive(Debug, Clone)]
pub struct Children<'arena, 'text> {
    arena: &'arena Arena<'text>,
    next: u32,
    end: u32,
}

impl Iterator for Children<'_, '_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.next >= self.end {
            return None;
        }
        let child = NodeId(self.next);
        // The next sibling starts just past this child's subtree.
        self.next = self.arena.node(child).after;
        Some(child)
    }
}

/// Text too long for an arena's offsets, which are 32-bit as the parsers' are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLong {
    /// The text's length in bytes.
    pub len: usize,
}

impl fmt::Display for TooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the text is {} bytes, more than the {} a parsed file may hold",
            self.len,
            u32::MAX
        )
    }
}

impl std::error::Error for TooLong {}

/// Builds an arena in one pass over a parser's tree: [`enter`](Builder::enter) a node,
/// then its children, then [`leave`](Builder::leave) it.
#[derive(Debug)]
pub struct Builder<'text> {
    language: Language,
    text: &'text str,
    nodes: Vec<Node>,
    open: Vec<usize>,
    tokens: Vec<Token>,
    values: Vec<(NodeId, String)>,
    /// The lists begun and not yet ended: each one's number, and how many nodes were open when
    /// it began, so that only the open node's children join it.
    lists: Vec<(u32, usize)>,
    next_list: u32,
}

impl<'text> Builder<'text> {
    /// A builder for an arena over `text`.  Called before parsing, so that text the arena
    /// cannot index is refused before the parser sees it.
    ///
    /// # Errors
    ///
    /// Returns [`TooLong`] if `text` is longer than a 32-bit offset can reach.
    pub fn new(language: Language, text: &'text str) -> Result<Builder<'text>, TooLong> {
        Builder::within(language, text, u32::MAX)
    }

    /// As [`new`](Builder::new), with the longest text allowed as a parameter, so the refusal
    /// can be tested without a 4 GiB text.
    fn within(language: Language, text: &'text str, limit: u32) -> Result<Builder<'text>, TooLong> {
        if u64::try_from(text.len()).map_or(true, |len| len > u64::from(limit)) {
            return Err(TooLong { len: text.len() });
        }
        Ok(Builder {
            language,
            text,
            nodes: Vec::new(),
            open: Vec::new(),
            tokens: Vec::new(),
            values: Vec::new(),
            lists: Vec::new(),
            next_list: 1,
        })
    }

    /// Starts a node of `kind` covering `range` of the text, as a child of the node open now.
    ///
    /// # Panics
    ///
    /// If a second root is entered, which is a defect in the adapter.
    pub fn enter(&mut self, kind: Kind, range: Range<usize>) {
        assert!(
            !self.open.is_empty() || self.nodes.is_empty(),
            "an adapter entered a second root"
        );
        let list = match self.lists.last() {
            Some(&(list, depth)) if depth == self.open.len() => list,
            _ => 0,
        };
        self.open.push(self.nodes.len());
        self.nodes.push(Node {
            kind,
            start: narrow(range.start),
            end: narrow(range.end),
            after: 0,
            list,
        });
    }

    /// Begins a list among the open node's children - a block's statements - which every child
    /// entered until [`end_list`](Builder::end_list) belongs to.
    pub fn begin_list(&mut self) {
        self.lists.push((self.next_list, self.open.len()));
        self.next_list += 1;
    }

    /// Ends the list begun last.
    ///
    /// # Panics
    ///
    /// If no list was begun, which is a defect in the adapter.
    pub fn end_list(&mut self) {
        self.lists
            .pop()
            .expect("an adapter ended a list it never began");
    }

    /// Ends the node entered last.
    ///
    /// # Panics
    ///
    /// If no node is open, which is a defect in the adapter.
    pub fn leave(&mut self) {
        let index = self
            .open
            .pop()
            .expect("an adapter left more nodes than it entered");
        self.nodes[index].after = narrow(self.nodes.len());
    }

    /// The arena.
    ///
    /// # Panics
    ///
    /// If no root was entered or a node is still open, which is a defect in the adapter.
    #[must_use]
    pub fn finish(mut self) -> Arena<'text> {
        assert!(self.open.is_empty(), "an adapter left nodes open");
        assert!(!self.nodes.is_empty(), "an adapter entered no root");
        self.values.sort_by_key(|(node, _)| *node);
        Arena {
            language: self.language,
            text: Cow::Borrowed(self.text),
            nodes: self.nodes,
            tokens: self.tokens,
            values: self.values,
        }
    }

    /// Gives the node open now a value it compares by in place of its children.
    ///
    /// # Panics
    ///
    /// If no node is open, which is a defect in the adapter.
    pub fn value(&mut self, value: String) {
        let open = *self
            .open
            .last()
            .expect("an adapter gave a value with no node open");
        self.values.push((NodeId(narrow(open)), value));
    }

    /// Adds a token the matcher compares, covering `range`.  Tokens are added in source order.
    ///
    /// # Panics
    ///
    /// If `range` starts before the end of the token added last, which is a defect in the
    /// adapter.
    pub fn token(&mut self, range: Range<usize>, compare: TokenText) {
        assert!(
            self.tokens
                .last()
                .is_none_or(|last| widen(last.end) <= range.start),
            "an adapter added a token out of order"
        );
        self.tokens.push(Token {
            start: narrow(range.start),
            end: narrow(range.end),
            compare,
        });
    }
}

/// An offset, which fits in 32 bits because the builder refused longer text, or a node index.
/// A parser gives far fewer nodes than 2^32 for text that short, but a node may be empty, so
/// that is a bound rather than a proof.
fn narrow(value: usize) -> u32 {
    u32::try_from(value).expect("an offset or node index past 32 bits")
}

fn widen(value: u32) -> usize {
    usize::try_from(value).expect("usize holds a u32 on every platform quaff builds for")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOY: Language = Language::new("toy");

    /// `a + b` with a comment after it: a sum whose operands are two names.
    fn sum(text: &str) -> Arena<'_> {
        let mut builder = Builder::new(TOY, text).unwrap();
        builder.enter(Kind::new("Sum"), 1..6);
        builder.enter(Kind::new("Name"), 1..2);
        builder.leave();
        builder.enter(Kind::new("Name"), 5..6);
        builder.leave();
        builder.leave();
        builder.finish()
    }

    #[test]
    fn nodes_are_in_pre_order_with_their_kinds_ranges_and_text() {
        let arena = sum(" a + b  # c\n");
        let seen: Vec<_> = arena
            .nodes()
            .map(|n| (arena.kind(n).name(), arena.range(n), arena.source(n)))
            .collect();
        assert_eq!(
            seen,
            [
                ("Sum", 1..6, "a + b"),
                ("Name", 1..2, "a"),
                ("Name", 5..6, "b")
            ]
        );
        assert_eq!(arena.len(), 3);
        assert!(!arena.is_empty());
        assert_eq!(arena.language().to_string(), "toy");
        assert_eq!(Language::new("toy").name(), "toy");
    }

    #[test]
    fn children_skip_each_others_subtrees() {
        let text = "f(g(x), y)";
        let mut builder = Builder::new(TOY, text).unwrap();
        builder.enter(Kind::new("Call"), 0..10);
        builder.enter(Kind::new("Name"), 0..1);
        builder.leave();
        builder.enter(Kind::new("Call"), 2..6);
        builder.enter(Kind::new("Name"), 2..3);
        builder.leave();
        builder.enter(Kind::new("Name"), 4..5);
        builder.leave();
        builder.leave();
        builder.enter(Kind::new("Name"), 8..9);
        builder.leave();
        builder.leave();
        let arena = builder.finish();
        let texts = |n| -> Vec<&str> { arena.children(n).map(|c| arena.source(c)).collect() };
        assert_eq!(texts(arena.root()), ["f", "g(x)", "y"]);
        let inner = arena.children(arena.root()).nth(1).unwrap();
        assert_eq!(texts(inner), ["g", "x"]);
        assert_eq!(texts(arena.children(inner).next().unwrap()), [""; 0]);
    }

    #[test]
    fn the_text_around_the_root_is_leading_and_trailing() {
        let arena = sum(" a + b  # c\n");
        assert_eq!(arena.leading(), " ");
        assert_eq!(arena.trailing(), "  # c\n");
        assert_eq!(arena.text(), " a + b  # c\n");
    }

    #[test]
    fn text_past_a_32_bit_offset_is_refused_naming_its_length() {
        assert!(Builder::within(TOY, "abc", 3).is_ok());
        assert_eq!(
            Builder::within(TOY, "abcd", 3).unwrap_err(),
            TooLong { len: 4 }
        );
        assert_eq!(
            TooLong { len: 5_000_000_000 }.to_string(),
            "the text is 5000000000 bytes, more than the 4294967295 a parsed file may hold"
        );
        assert!(Builder::new(TOY, "short").is_ok());
    }

    #[test]
    #[should_panic(expected = "an adapter entered a second root")]
    fn a_second_root_is_a_defect() {
        let mut builder = Builder::new(TOY, "ab").unwrap();
        builder.enter(Kind::new("A"), 0..1);
        builder.leave();
        builder.enter(Kind::new("B"), 1..2);
    }

    #[test]
    #[should_panic(expected = "an adapter left nodes open")]
    fn an_open_node_is_a_defect() {
        let mut builder = Builder::new(TOY, "a").unwrap();
        builder.enter(Kind::new("A"), 0..1);
        let _ = builder.finish();
    }

    #[test]
    #[should_panic(expected = "an adapter entered no root")]
    fn no_root_is_a_defect() {
        let _ = Builder::new(TOY, "").unwrap().finish();
    }

    #[test]
    #[should_panic(expected = "an adapter added a token out of order")]
    fn a_token_out_of_order_is_a_defect() {
        let mut builder = Builder::new(TOY, "ab").unwrap();
        builder.token(1..2, TokenText::Source);
        builder.token(0..1, TokenText::Source);
    }

    #[test]
    #[should_panic(expected = "an adapter gave a value with no node open")]
    fn a_value_with_no_node_open_is_a_defect() {
        Builder::new(TOY, "").unwrap().value(String::new());
    }

    #[test]
    #[should_panic(expected = "an adapter ended a list it never began")]
    fn ending_a_list_never_begun_is_a_defect() {
        Builder::new(TOY, "").unwrap().end_list();
    }

    #[test]
    #[should_panic(expected = "an adapter left more nodes than it entered")]
    fn leaving_too_often_is_a_defect() {
        Builder::new(TOY, "").unwrap().leave();
    }
}
