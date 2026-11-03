// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A statement's `where`, decided for one match of a structural pattern: its `FILE` tests by the
//! file's path, and its tests of metavariables by what the match bound them to.
//!
//! With a quoted glob, a bound string literal is compared by its raw content - what lies between
//! its delimiters, as written - and any other node by its source text.  With a backticked
//! pattern, `matches` is the pattern matching the bound node itself, and `contains` the pattern
//! matching it or anything inside it.  The matcher offers every way a match binds the names
//! tested, and the match is kept if any of them passes the whole `where`.

use std::cell::RefCell;
use std::ops::Range;

use quaffed_python::{PatternError, compile_pattern};
use quaffed_representation::program::{Filter as Written, Operand, Subject};
use quaffed_representation::span::{Span, Spanned};
use quaffed_syntax::{Arena, NodeId, Pattern, find};

use crate::glob::{Kind, Matcher};
use crate::scope::{self, PathGlob, Place, Roots};

/// A `where` that tests a metavariable, compiled.
pub struct Filter {
    /// The names it tests, without `$`, in the order first written.
    pub names: Vec<String>,
    /// The backticked patterns it compares with, compiled, with where each was written.
    pub shapes: Vec<(Pattern, Span)>,
    tree: Tree,
}

enum Tree {
    And(Vec<Tree>),
    Not(Box<Tree>),
    File(PathGlob),
    /// The value bound to the name at this index of `names` matches.
    Value(usize, Matcher),
    /// The pattern at the second index of `shapes` matches the node bound to the name at the
    /// first: the node itself, or with `anywhere` the node or anything inside it.
    Shape {
        name: usize,
        shape: usize,
        anywhere: bool,
    },
}

/// What each of a filter's patterns matched in one file, found the first time it is asked.
pub type Found = RefCell<Vec<Option<Vec<Range<usize>>>>>;

/// A part of a `where` this build cannot decide for a match yet, with where it was written.
pub struct NotBuilt(pub Span);

/// Why a `where` could not be compiled.
pub enum Error {
    /// A glob in it could not be: too large, or naming a path that could not be looked up.
    Glob(scope::Error, Span),
    /// A backticked pattern in it is not a pattern, saying where in it.
    Pattern(PatternError, Span),
    /// It holds what this build does not have.
    NotBuilt(NotBuilt),
}

impl Filter {
    /// The `where` `filter` is, if it tests a metavariable; `None` if it tests `FILE` alone,
    /// which a file's path settles before it is parsed.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] for a glob or a pattern that cannot be compiled, or for what this
    /// build does not have.
    pub fn new(filter: &Spanned<Written>, roots: Roots) -> Result<Option<Filter>, Error> {
        let mut built = Filter {
            names: Vec::new(),
            shapes: Vec::new(),
            tree: Tree::And(Vec::new()),
        };
        built.tree = tree(filter, roots, &mut built)?;
        Ok((!built.names.is_empty()).then_some(built))
    }

    /// A store for what this filter's patterns match in one file, for [`Filter::holds`].
    #[must_use]
    pub fn found(&self) -> Found {
        RefCell::new(vec![None; self.shapes.len()])
    }

    /// Whether the match in the file at `place`, parsed into `arena`, whose tested names are
    /// bound to `nodes` in the order of `names`, passes.  `found` is [`Filter::found`]'s store
    /// for this file, so that each pattern is searched for in it once.
    #[must_use]
    pub fn holds(&self, place: &Place, arena: &Arena<'_>, nodes: &[NodeId], found: &Found) -> bool {
        holds(self, &self.tree, place, arena, nodes, found)
    }

    /// The index of `name` among the names tested, adding it.
    fn name(&mut self, name: &str) -> usize {
        self.names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| {
                self.names.push(name.to_owned());
                self.names.len() - 1
            })
    }
}

fn tree(filter: &Spanned<Written>, roots: Roots, built: &mut Filter) -> Result<Tree, Error> {
    Ok(match &filter.node {
        Written::And(parts) => Tree::And(
            parts
                .iter()
                .map(|part| tree(part, roots, built))
                .collect::<Result<_, _>>()?,
        ),
        Written::Not(part) => Tree::Not(Box::new(tree(part, roots, built)?)),
        Written::Matches(subject, operand) | Written::Contains(subject, operand) => {
            let anywhere = matches!(filter.node, Written::Contains(..));
            if let (Subject::Metavariable(name), Operand::Pattern(written)) =
                (subject, &operand.node)
            {
                let pattern =
                    compile_pattern(written).map_err(|err| Error::Pattern(err, operand.span))?;
                built.shapes.push((pattern, operand.span));
                return Ok(Tree::Shape {
                    name: built.name(name),
                    shape: built.shapes.len() - 1,
                    anywhere,
                });
            }
            let Operand::Glob(glob) = &operand.node else {
                return Err(Error::NotBuilt(NotBuilt(operand.span)));
            };
            match subject {
                Subject::File => Tree::File(
                    PathGlob::new(glob, anywhere, roots)
                        .map_err(|err| Error::Glob(err, operand.span))?,
                ),
                Subject::Metavariable(name) => {
                    let index = built.name(name);
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

fn holds(
    filter: &Filter,
    tree: &Tree,
    place: &Place,
    arena: &Arena<'_>,
    nodes: &[NodeId],
    found: &Found,
) -> bool {
    let holds = |part| holds(filter, part, place, arena, nodes, found);
    match tree {
        Tree::And(parts) => parts.iter().all(holds),
        Tree::Not(part) => !holds(part),
        Tree::File(glob) => glob.is_match(place),
        Tree::Value(index, matcher) => {
            let node = nodes[*index];
            matcher.is_match(arena.value(node).unwrap_or_else(|| arena.source(node)))
        }
        Tree::Shape {
            name,
            shape,
            anywhere,
        } => {
            let bound = arena.range(nodes[*name]);
            let mut found = found.borrow_mut();
            let ranges = found[*shape].get_or_insert_with(|| {
                find(&filter.shapes[*shape].0, arena)
                    .into_iter()
                    .map(|m| m.range)
                    .collect()
            });
            ranges.iter().any(|range| {
                if *anywhere {
                    bound.start <= range.start && range.end <= bound.end
                } else {
                    *range == bound
                }
            })
        }
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

    /// The first node of `arena` whose source is `text`.
    fn node(arena: &Arena<'_>, text: &str) -> NodeId {
        arena
            .nodes()
            .find(|&node| arena.source(node) == text)
            .unwrap_or_else(|| panic!("no node {text:?}"))
    }

    fn roots() -> Roots<'static> {
        Roots {
            cwd: Path::new("/p"),
            root: Path::new("/p"),
        }
    }

    fn place() -> Place<'static> {
        Place {
            path: Path::new("/p/a.py"),
            from_cwd: "a.py".into(),
            from_root: Some("a.py".into()),
        }
    }

    #[test]
    fn a_value_test_compares_what_a_match_bound_and_not_turns_it_round() {
        let arena = quaffed_python::parse_module(
            "a = 'https://x'\nb = r\"http://x\"\nc = port\nd = \"\"\"a\npassword123\"\"\"\n",
        )
        .unwrap();
        let holds = |filter: &Filter, nodes: &[NodeId]| {
            filter.holds(&place(), &arena, nodes, &filter.found())
        };
        let mut https = Glob::literal("https");
        https.push(GlobPart::Star);
        let starts = Filter::new(&test("s", https, false), roots())
            .ok()
            .flatten()
            .unwrap();
        assert_eq!(starts.names, ["s"]);
        // A string by its content, whatever its quotes and prefix.
        assert!(holds(&starts, &[node(&arena, "'https://x'")]));
        assert!(!holds(&starts, &[node(&arena, "r\"http://x\"")]));
        // Contains is anywhere in the value, lines included; any other node is its text.
        let password = test("x", Glob::literal("password"), true);
        let not = Spanned::new(Written::Not(Box::new(password)), at(0));
        let without = Filter::new(&not, roots()).ok().flatten().unwrap();
        assert!(holds(&without, &[node(&arena, "port")]));
        assert!(!holds(
            &without,
            &[node(&arena, "\"\"\"a\npassword123\"\"\"")]
        ));
        // Two tests of one name read one value.
        let both = Spanned::new(
            Written::And(vec![
                test("x", Glob::literal("ht"), true),
                test("x", Glob::literal("s:"), true),
            ]),
            at(0),
        );
        let both = Filter::new(&both, roots()).ok().flatten().unwrap();
        assert_eq!(both.names, ["x"]);
        assert!(holds(&both, &[node(&arena, "'https://x'")]));
        assert!(!holds(&both, &[node(&arena, "r\"http://x\"")]));
    }

    fn shape(name: &str, pattern: &str, contains: bool) -> Filter {
        let operand = Spanned::new(Operand::Pattern(pattern.into()), at(1));
        let subject = Subject::Metavariable(name.into());
        let written = Spanned::new(
            if contains {
                Written::Contains(subject, operand)
            } else {
                Written::Matches(subject, operand)
            },
            at(0),
        );
        Filter::new(&written, roots()).ok().flatten().unwrap()
    }

    #[test]
    fn a_pattern_matches_the_bound_node_itself_and_contains_anything_inside_it() {
        let arena = quaffed_python::parse_module("x = f(g(1), 2)\n").unwrap();
        let call = node(&arena, "f(g(1), 2)");
        let holds = |filter: &Filter, bound: NodeId| {
            filter.holds(&place(), &arena, &[bound], &filter.found())
        };
        assert!(holds(&shape("v", "f($a, $b)", false), call));
        // `matches` is the whole node: g(1) is inside it, not it.
        assert!(!holds(&shape("v", "g($a)", false), call));
        assert!(holds(&shape("v", "g($a)", true), call));
        assert!(!holds(&shape("v", "h($a)", true), call));
        // What a pattern finds in the file is worked out once, however many bindings ask.
        let filter = shape("v", "g($a)", true);
        let found = filter.found();
        for _ in 0..3 {
            assert!(filter.holds(&place(), &arena, &[call], &found));
        }
        let ranges = found.borrow()[0].clone().expect("searched for once asked");
        assert_eq!((ranges.len(), ranges[0].clone()), (1, 6..10));
    }
}
