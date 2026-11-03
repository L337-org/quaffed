// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A glob: literal text and the wildcards between it, as `in` and `where` compare against.
//!
//! A script writes the literal text in quotes, where nothing is special, and every wildcard as
//! a marker outside them, so `"src/"**".py"` is the text `src/`, any run, then `.py`.  Nothing
//! here says what a segment is: a path's segments are its directory names and a string's are its
//! lines, and the engine, which knows which it is comparing, decides that.

/// A glob, matched against the whole of a value.
///
/// Built from the pieces a script writes, as a [`TextPattern`](crate::program::TextPattern) is:
/// adjacent literal text is joined as it is built, so `"ab"` and `"a" "b"` are the same glob.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Glob {
    /// Pinned to the start of the value, as `^` writes it; only `contains` can be anything else.
    pub start_anchor: bool,
    /// The parts, in order, with no two literals adjacent.
    parts: Vec<GlobPart>,
    /// Pinned to the end of the value, as `$` writes it.
    pub end_anchor: bool,
}

/// One part of a [`Glob`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GlobPart {
    /// Text that must appear exactly.
    Literal(String),
    /// A line break: LF, CRLF or CR.
    LineBreak,
    /// `*`: any run of characters, none at all included, within one segment.
    Star,
    /// `**`: any run of characters across segments.
    DoubleStar,
    /// `?`: one character, never one that ends a segment.
    One,
    /// `[...]`: one character in a set, or not in it, never one that ends a segment.
    Class(Class),
    /// `{...}`: any one of the alternatives.
    Alternatives(Vec<Glob>),
}

/// A set of characters, written `[abc]`, `[a-z]` or `[!a]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Class {
    /// Whether the class matches a character not in the set, as a leading `!` writes it.
    pub negated: bool,
    /// The members, in the order written.
    pub items: Vec<ClassItem>,
}

/// One member of a [`Class`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassItem {
    /// One character.
    One(char),
    /// Every character from the first to the second, both included.
    Range(char, char),
}

impl Glob {
    /// A glob of one literal: exactly that text.
    pub fn literal(text: impl Into<String>) -> Self {
        let mut glob = Glob::default();
        glob.push_literal(text.into());
        glob
    }

    /// Appends literal text, joining it to a literal it follows.  Empty text adds nothing.
    pub fn push_literal(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(GlobPart::Literal(last)) = self.parts.last_mut() {
            last.push_str(&text);
        } else {
            self.parts.push(GlobPart::Literal(text));
        }
    }

    /// Appends a part, joining literal text to a literal it follows.
    pub fn push(&mut self, part: GlobPart) {
        match part {
            GlobPart::Literal(text) => self.push_literal(text),
            other => self.parts.push(other),
        }
    }

    /// The parts, in order.
    #[must_use]
    pub fn parts(&self) -> &[GlobPart] {
        &self.parts
    }

    /// The text the glob matches exactly, if it has no wildcard and no line break: an empty glob
    /// is the empty text.
    #[must_use]
    pub fn as_literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [] => Some(""),
            [GlobPart::Literal(text)] => Some(text),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_literals_join_and_a_marker_separates_them() {
        let mut joined = Glob::literal("a");
        joined.push(GlobPart::Literal("b".into()));
        joined.push_literal(String::new());
        assert_eq!(joined, Glob::literal("ab"));
        assert_eq!(joined.as_literal(), Some("ab"));
        let mut starred = Glob::literal("a");
        starred.push(GlobPart::Star);
        starred.push_literal("b".into());
        assert_eq!(
            starred.parts(),
            [
                GlobPart::Literal("a".into()),
                GlobPart::Star,
                GlobPart::Literal("b".into())
            ]
        );
        assert_eq!(starred.as_literal(), None);
        assert_eq!(Glob::default().as_literal(), Some(""));
    }
}
