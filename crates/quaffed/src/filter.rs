// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A statement's `where`, decided for one match of a structural pattern: its `FILE` tests by the
//! file's path, and its tests of metavariables by what the match bound them to.
//!
//! A bound string literal is compared by its raw content - what lies between its delimiters, as
//! written - and any other node by its source text.  The matcher offers every way a match binds
//! the names tested, and the match is kept if any of them passes the whole `where`.

use quaffed_representation::program::{Filter as Written, Operand, Subject};
use quaffed_representation::span::{Span, Spanned};

use crate::glob::{Kind, Matcher};
use crate::scope::{self, PathGlob, Place, Roots};

/// A `where` that tests a metavariable, compiled.
pub struct Filter {
    /// The names it tests, without `$`, in the order first written.
    pub names: Vec<String>,
    tree: Tree,
}

enum Tree {
    And(Vec<Tree>),
    Not(Box<Tree>),
    File(PathGlob),
    /// The value bound to the name at this index of `names` matches.
    Value(usize, Matcher),
}

/// A part of a `where` this build cannot decide for a match yet, with where it was written.
pub struct NotBuilt(pub Span);

/// Why a `where` could not be compiled.
pub enum Error {
    /// A glob in it could not be: too large, or naming a path that could not be looked up.
    Glob(scope::Error, Span),
    /// It holds what this build does not have.
    NotBuilt(NotBuilt),
}

impl Filter {
    /// The `where` `filter` is, if it tests a metavariable; `None` if it tests `FILE` alone,
    /// which a file's path settles before it is parsed.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] for a glob that cannot be compiled, or a comparison with a
    /// backticked pattern, which is not built yet.
    pub fn new(filter: &Spanned<Written>, roots: Roots) -> Result<Option<Filter>, Error> {
        let mut names = Vec::new();
        let tree = tree(filter, roots, &mut names)?;
        Ok((!names.is_empty()).then_some(Filter { names, tree }))
    }

    /// Whether the match in the file at `place`, whose tested names are bound to `values` in the
    /// order of `names`, passes.
    #[must_use]
    pub fn holds(&self, place: &Place, values: &[&str]) -> bool {
        holds(&self.tree, place, values)
    }
}

fn tree(filter: &Spanned<Written>, roots: Roots, names: &mut Vec<String>) -> Result<Tree, Error> {
    Ok(match &filter.node {
        Written::And(parts) => Tree::And(
            parts
                .iter()
                .map(|part| tree(part, roots, names))
                .collect::<Result<_, _>>()?,
        ),
        Written::Not(part) => Tree::Not(Box::new(tree(part, roots, names)?)),
        Written::Matches(subject, operand) | Written::Contains(subject, operand) => {
            let anywhere = matches!(filter.node, Written::Contains(..));
            let Operand::Glob(glob) = &operand.node else {
                return Err(Error::NotBuilt(NotBuilt(operand.span)));
            };
            match subject {
                Subject::File => Tree::File(
                    PathGlob::new(glob, anywhere, roots)
                        .map_err(|err| Error::Glob(err, operand.span))?,
                ),
                Subject::Metavariable(name) => {
                    let index = names.iter().position(|n| n == name).unwrap_or_else(|| {
                        names.push(name.clone());
                        names.len() - 1
                    });
                    let matcher = Matcher::new(glob, Kind::Value, anywhere)
                        .map_err(|err| Error::Glob(scope::Error::TooLarge(err), operand.span))?;
                    Tree::Value(index, matcher)
                }
                // `Subject` is non-exhaustive; a subject this build does not know is not built.
                _ => return Err(Error::NotBuilt(NotBuilt(filter.span))),
            }
        }
        _ => return Err(Error::NotBuilt(NotBuilt(filter.span))),
    })
}

fn holds(tree: &Tree, place: &Place, values: &[&str]) -> bool {
    match tree {
        Tree::And(parts) => parts.iter().all(|part| holds(part, place, values)),
        Tree::Not(part) => !holds(part, place, values),
        Tree::File(glob) => glob.is_match(place),
        Tree::Value(index, matcher) => matcher.is_match(values[*index]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quaffed_representation::glob::{Glob, GlobPart};
    use std::path::Path;

    fn at(start: usize) -> Span {
        Span {
            source: 0,
            start,
            end: start + 1,
        }
    }

    fn test(name: &str, glob: Glob, contains: bool) -> Spanned<Written> {
        let operand = Spanned::new(Operand::Glob(glob), at(1));
        let subject = Subject::Metavariable(name.into());
        Spanned::new(
            if contains {
                Written::Contains(subject, operand)
            } else {
                Written::Matches(subject, operand)
            },
            at(0),
        )
    }

    #[test]
    fn a_value_test_compares_what_a_match_bound_and_not_turns_it_round() {
        let roots = Roots {
            cwd: Path::new("/p"),
            root: Path::new("/p"),
        };
        let place = Place {
            path: Path::new("/p/a.py"),
            from_cwd: "a.py".into(),
            from_root: Some("a.py".into()),
        };
        let mut https = Glob::literal("https");
        https.push(GlobPart::Star);
        let starts = Filter::new(&test("s", https, false), roots)
            .ok()
            .flatten()
            .unwrap();
        assert_eq!(starts.names, ["s"]);
        assert!(starts.holds(&place, &["https://x"]));
        assert!(!starts.holds(&place, &["http://x"]));
        // Contains is anywhere in the value, lines included.
        let password = test("x", Glob::literal("password"), true);
        let not = Spanned::new(Written::Not(Box::new(password)), at(0));
        let without = Filter::new(&not, roots).ok().flatten().unwrap();
        assert!(without.holds(&place, &["port"]));
        assert!(!without.holds(&place, &["a\npassword123"]));
        // Two tests of one name read one value.
        let both = Spanned::new(
            Written::And(vec![
                test("x", Glob::literal("a"), true),
                test("x", Glob::literal("b"), true),
            ]),
            at(0),
        );
        let both = Filter::new(&both, roots).ok().flatten().unwrap();
        assert_eq!(both.names, ["x"]);
        assert!(both.holds(&place, &["ab"]) && !both.holds(&place, &["a"]));
    }
}
