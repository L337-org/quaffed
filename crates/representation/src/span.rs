// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Where something was written: the source it came from and the bytes it occupies there.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Where a piece of a program came from.
///
/// Sources compose in the order the command line gave them, so a diagnostic always names one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// A script file, from `-f path`, as given.
    File(PathBuf),
    /// A script on standard input, from `-f -`.
    Stdin,
    /// The `n`th `-e` expression, counted from 1.
    Expression(usize),
    /// A `-s` or `--string` option.
    StringOption,
    /// The bare query: the first positional argument.
    Positional,
}

/// A byte range in one of a program's sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// The index of the source in `Program::sources`.
    pub source: usize,
    /// The first byte.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

impl Span {
    /// The span from the start of `self` to the end of `last`: a statement's span from its
    /// first and last parts.
    ///
    /// When `last` lies in another source - sources compose, but a span cannot cross from one
    /// to the next - the result is `self` alone, so it still names a real place.
    #[must_use]
    pub fn through(self, last: Span) -> Span {
        Span {
            end: if last.source == self.source {
                last.end
            } else {
                self.end
            },
            ..self
        }
    }
}

/// A node with the span it was written at.
///
/// **The span takes no part in equality or hashing**, so two nodes that parse the same are the
/// same node whatever their layout or position.  That is what de-duplication of assertions
/// rests on: identity is the parsed statement, never where it was written.
#[derive(Debug, Clone)]
pub struct Spanned<T> {
    /// The node.
    pub node: T,
    /// Where it was written.
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Wraps `node` with the span it was written at.
    pub fn new(node: T, span: Span) -> Self {
        Spanned { node, span }
    }
}

impl<T: PartialEq> PartialEq for Spanned<T> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<T: Eq> Eq for Spanned<T> {}

impl<T: Hash> Hash for Spanned<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.node.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::DefaultHasher;

    fn hash_of<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn position_takes_no_part_in_identity() {
        let here = Spanned::new(
            "x",
            Span {
                source: 0,
                start: 0,
                end: 1,
            },
        );
        let there = Spanned::new(
            "x",
            Span {
                source: 3,
                start: 40,
                end: 41,
            },
        );
        assert_eq!(here, there);
        assert_eq!(hash_of(&here), hash_of(&there));
    }

    #[test]
    fn a_span_through_another_covers_both_within_one_source() {
        let first = Span {
            source: 0,
            start: 3,
            end: 5,
        };
        let last = Span {
            source: 0,
            start: 9,
            end: 12,
        };
        assert_eq!(
            first.through(last),
            Span {
                source: 0,
                start: 3,
                end: 12
            }
        );
        let elsewhere = Span {
            source: 1,
            start: 0,
            end: 2,
        };
        assert_eq!(first.through(elsewhere), first);
    }

    #[test]
    fn different_nodes_are_different_wherever_they_are() {
        let span = Span {
            source: 0,
            start: 0,
            end: 1,
        };
        assert_ne!(Spanned::new("x", span), Spanned::new("y", span));
        assert_ne!(
            hash_of(&Spanned::new("x", span)),
            hash_of(&Spanned::new("y", span))
        );
    }
}
