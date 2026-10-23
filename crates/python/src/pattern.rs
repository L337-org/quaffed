// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A structural pattern in Python - `handle_request($a, $b)` - compiled into an arena with holes.
//!
//! A pattern cannot be parsed as written, because a hole is not Python.  So compiling it takes
//! two stages.  First every `$name`, `$_`, `$name...` and `$...` is replaced by a placeholder
//! identifier, before anything is parsed.  Then the result is parsed as ordinary Python, and the
//! grammar decides: a placeholder that became a node of its own is a hole, and one that landed
//! inside a string literal or a comment is literal text, restored as it was written.
//! `${HOME}` is never claimed, since `${` is not the sigil followed by a name.

use std::fmt::{self, Write as _};
use std::ops::Range;

use quaffed_syntax::{Arena, Builder, Costly, Hole, NodeId, Pattern};
use ruff_python_parser::Mode;

use crate::parse::{PYTHON, ParseError, parse_from};

/// Why a pattern could not be compiled.  Each that has a place in the pattern carries it, as a
/// byte offset into the pattern as written, and [`PatternError::offset`] gives it; the message
/// leaves it out, for the caller to place in terms its reader knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// The pattern, with its holes, is not Python.
    Invalid {
        /// What the parser said, verbatim.
        message: String,
        /// Where in the pattern the error starts.
        offset: usize,
    },
    /// `$_...`, which is not a hole: an unbound sequence is written `$...`, so that each
    /// meaning has one spelling.
    UnboundNamedSequence {
        /// Where in the pattern it is.
        offset: usize,
    },
    /// Every part of the pattern is a hole, so there is no shape to match: `$x` alone would
    /// match every node in every file.
    NoShape,
    /// The pattern has no code at all: it is empty, or only whitespace and comments.
    Empty,
    /// One name is both a hole for one node and a sequence, `$x` and `$x...`, which cannot
    /// both hold.
    MixedHole {
        /// The name.
        name: String,
        /// Where in the pattern its second spelling is.
        offset: usize,
        /// Whether the spelling at `offset` is the sequence, `$x...`, or the single node, `$x`.
        sequence: bool,
    },
    /// A sequence hole starts or ends the pattern's statements, so nothing in the pattern bounds
    /// it on that side and it would take every statement to that edge of the block.
    UnboundedSequence {
        /// The name it binds, or `None` for `$...`.
        name: Option<String>,
        /// Where in the pattern it is.
        offset: usize,
        /// Whether it starts the statements, rather than ending them.
        leading: bool,
    },
    /// Two sequence holes stand together in a run of statements, so nothing settles how many
    /// statements each takes, and a search would try every division.
    SequencesTogether {
        /// The second sequence's name, or `None` for `$...`.
        name: Option<String>,
        /// Where in the pattern the second sequence is.
        offset: usize,
    },
    /// A second place in a run of statements where the statement a sequence stops at decides
    /// what later code must match, each such place multiplying a search's work by the length of
    /// the block.
    SecondChoice {
        /// The hole that makes it so, as written: `$x` or `$x...`.
        spelled: String,
        /// Where in the pattern it is.
        offset: usize,
    },
    /// The pattern is longer than an arena's offsets reach.
    TooLong(quaffed_syntax::TooLong),
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Invalid { message, .. } => {
                write!(f, "the pattern is not Python: {message}")
            }
            PatternError::UnboundNamedSequence { .. } => f.write_str(
                "\"$_...\" is not a hole: a sequence that binds nothing is written \"$...\"",
            ),
            PatternError::NoShape => f.write_str(
                "the pattern is only holes, so it has no shape to match: write the code around \
                 them, as in \"handle_request($a, $b)\"",
            ),
            PatternError::MixedHole { name, sequence, .. } => {
                let (first, here) = if *sequence {
                    ("one node", "a sequence")
                } else {
                    ("a sequence", "one node")
                };
                write!(
                    f,
                    "\"${name}\" is used for {first} and, here, for {here}: a name is one or the \
                     other, so give the sequence a name of its own"
                )
            }
            PatternError::Empty => {
                f.write_str("the pattern has no code in it, only whitespace or comments")
            }
            PatternError::UnboundedSequence { name, leading, .. } => {
                let spelled = format!("${}...", name.as_deref().unwrap_or(""));
                let (role, edge, put) = if *leading {
                    ("starts", "the start", "before")
                } else {
                    ("ends", "the end", "after")
                };
                write!(
                    f,
                    "\"{spelled}\" {role} the pattern's statements, so nothing bounds it and it would \
                     take every statement to {edge} of the block: put a statement {put} it, so that \
                     code stands on both sides, as \"$body...\" does between \"open_db()\" and \
                     \"close_db()\" on lines of their own"
                )
            }
            PatternError::SequencesTogether { name, .. } => write!(
                f,
                "\"${}...\" follows another sequence with no statement between them, so nothing \
                 says how many statements each takes: use one sequence, or put a statement \
                 between them",
                name.as_deref().unwrap_or("")
            ),
            PatternError::SecondChoice { spelled, .. } => write!(
                f,
                "\"{spelled}\" makes a second place in this run of statements where the statement a \
                 sequence stops at decides what later code must match, and each such place \
                 multiplies the work of a search by the length of the block, so a pattern may have \
                 one: give one of the names used twice a name of its own"
            ),
            PatternError::TooLong(err) => err.fmt(f),
        }
    }
}

impl PatternError {
    /// Where in the pattern as written the error is, as a byte offset, if it has a place.
    #[must_use]
    pub fn offset(&self) -> Option<usize> {
        match self {
            PatternError::Invalid { offset, .. }
            | PatternError::UnboundNamedSequence { offset }
            | PatternError::MixedHole { offset, .. }
            | PatternError::UnboundedSequence { offset, .. }
            | PatternError::SequencesTogether { offset, .. }
            | PatternError::SecondChoice { offset, .. } => Some(*offset),
            PatternError::NoShape | PatternError::Empty | PatternError::TooLong(_) => None,
        }
    }
}

impl std::error::Error for PatternError {}

/// One stretch of the pattern the substitution claimed: where it was written, and the hole it
/// asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Claim {
    written: Range<usize>,
    hole: Hole,
}

/// `text`, a structural pattern in Python, compiled.  It is parsed as a single expression if it
/// is one, and otherwise as statements.
///
/// # Errors
///
/// Returns a [`PatternError`] if the pattern is not Python, spells a hole `$_...`, is nothing
/// but holes, starts or ends a run of statements with a sequence hole, has a run too costly to
/// search ([`Pattern::costly`]), or is too long.  Nothing partial is returned.
///
/// # Panics
///
/// If putting literal text back inside a string or comment moved a hole, which would mean the
/// grammar let a string's content change the pattern's shape: a defect, not an input.
pub fn compile_pattern(text: &str) -> Result<Pattern, PatternError> {
    let claims = claim(text)?;
    let placeholder = placeholder_stem(text);
    // First parse: every claim a placeholder, to learn which of them the grammar makes nodes.
    let first = substitute(text, &claims, |_| true, &placeholder);
    let (mode, arena) = parse_either(&first.text).map_err(|err| invalid(err, &first))?;
    let is_hole: Vec<bool> = first
        .placeholders
        .iter()
        .map(|range| node_spanning(&arena, range).is_some())
        .collect();
    // Second parse: a literal claim restored as written, so the pattern matches that text.  A
    // string or comment's content takes no part in the structure, so the shape is unchanged.
    let second = substitute(text, &claims, |i| is_hole[i], &placeholder);
    let arena = parse_as(&second.text, mode).map_err(|err| invalid(err, &second))?;
    let placed: Vec<(NodeId, &Claim)> = claims
        .iter()
        .zip(&is_hole)
        .filter(|(_, hole)| **hole)
        .zip(&second.placeholders)
        .map(|((claim, _), range)| {
            let node = node_spanning(&arena, range)
                .expect("restoring literal text inside a string or comment moved a hole");
            (node, claim)
        })
        .collect();
    let holes: Vec<(NodeId, Hole)> = placed
        .iter()
        .map(|(node, claim)| (*node, claim.hole.clone()))
        .collect();
    let content: Vec<NodeId> = arena.children(arena.root()).collect();
    if content.is_empty() {
        return Err(PatternError::Empty);
    }
    if content
        .iter()
        .all(|node| holes.iter().any(|(hole, _)| hole == node))
    {
        return Err(PatternError::NoShape);
    }
    let first = content[0];
    let pattern = Pattern::new(arena.into_owned(), holes);
    if let Some(node) = pattern.unbounded_sequence() {
        let (_, claim) = placed
            .iter()
            .find(|(hole, _)| *hole == node)
            .expect("an unbounded sequence is one of the pattern's holes");
        return Err(PatternError::UnboundedSequence {
            name: claim.hole.name.clone(),
            offset: claim.written.start,
            leading: node == first,
        });
    }
    if let Some(costly) = pattern.costly() {
        let node = match costly {
            Costly::Together(node) | Costly::SecondChoice(node) => node,
        };
        let (_, claim) = placed
            .iter()
            .find(|(hole, _)| *hole == node)
            .expect("a costly run is named by one of the pattern's holes");
        let offset = claim.written.start;
        return Err(match costly {
            Costly::Together(_) => PatternError::SequencesTogether {
                name: claim.hole.name.clone(),
                offset,
            },
            Costly::SecondChoice(_) => PatternError::SecondChoice {
                spelled: text[claim.written.clone()].to_owned(),
                offset,
            },
        });
    }
    Ok(pattern)
}

/// Every hole the pattern spells, in order, before anything is parsed.
fn claim(text: &str) -> Result<Vec<Claim>, PatternError> {
    let mut claims = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find('$') {
        let start = at + found;
        let mut end = start + 1;
        let name = if text[end..].starts_with("...") {
            None
        } else {
            let length = identifier_len(&text[end..]);
            if length == 0 {
                // Not the sigil followed by a name or `...` - a bare `$`, or `${` - so it is
                // left for the parser, as written.
                at = end;
                continue;
            }
            let name = &text[end..end + length];
            end += length;
            Some(name)
        };
        let sequence = text[end..].starts_with("...");
        if sequence {
            end += 3;
        }
        if name == Some("_") && sequence {
            return Err(PatternError::UnboundNamedSequence { offset: start });
        }
        if let Some(name) = name.filter(|n| *n != "_")
            && claims.iter().any(|c: &Claim| {
                c.hole.name.as_deref() == Some(name) && c.hole.sequence != sequence
            })
        {
            return Err(PatternError::MixedHole {
                name: name.to_owned(),
                offset: start,
                sequence,
            });
        }
        claims.push(Claim {
            written: start..end,
            hole: Hole {
                name: name.filter(|n| *n != "_").map(str::to_owned),
                sequence,
            },
        });
        at = end;
    }
    Ok(claims)
}

/// The length in bytes of the Python identifier `text` starts with, by the rules Ruff's lexer
/// uses: `_` or `XID_Start`, then `XID_Continue`.
fn identifier_len(text: &str) -> usize {
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, c)) if c == '_' || unicode_ident::is_xid_start(c) => {}
        _ => return 0,
    }
    chars
        .find(|(_, c)| !unicode_ident::is_xid_continue(*c))
        .map_or(text.len(), |(i, _)| i)
}

/// A stem for placeholders that the pattern does not already contain, so a placeholder can
/// never be confused with a name the pattern wrote.
fn placeholder_stem(text: &str) -> String {
    let mut stem = String::from("__quaff_hole");
    while text.contains(&stem) {
        stem.push('_');
    }
    stem
}

/// The pattern with some claims replaced by placeholders, and where each placeholder landed.
struct Substituted {
    text: String,
    /// The placeholders' ranges in `text`, in the order of the claims they replaced.
    placeholders: Vec<Range<usize>>,
    /// For each claim replaced, its range written and its range in `text`, to map an offset
    /// in `text` back to the pattern as written.
    moves: Vec<(Range<usize>, Range<usize>)>,
}

fn substitute(
    text: &str,
    claims: &[Claim],
    replace: impl Fn(usize) -> bool,
    stem: &str,
) -> Substituted {
    let mut out = String::with_capacity(text.len() + claims.len() * 16);
    let mut placeholders = Vec::new();
    let mut moves = Vec::new();
    let mut copied = 0;
    for (i, claim) in claims.iter().enumerate() {
        if !replace(i) {
            continue;
        }
        out.push_str(&text[copied..claim.written.start]);
        let start = out.len();
        write!(out, "{stem}{i}").expect("writing to a String cannot fail");
        placeholders.push(start..out.len());
        moves.push((claim.written.clone(), start..out.len()));
        copied = claim.written.end;
    }
    out.push_str(&text[copied..]);
    Substituted {
        text: out,
        placeholders,
        moves,
    }
}

impl Substituted {
    /// The offset in the pattern as written of `offset` in the substituted text: the start of
    /// the hole it falls inside, if it does.
    fn written_offset(&self, offset: usize) -> usize {
        let mut shift: isize = 0;
        for (written, placed) in &self.moves {
            if offset < placed.start {
                break;
            }
            if offset < placed.end {
                return written.start;
            }
            shift = to_isize(written.end) - to_isize(placed.end);
        }
        usize::try_from(to_isize(offset) + shift).expect("an offset inside the pattern")
    }
}

fn to_isize(value: usize) -> isize {
    isize::try_from(value).expect("a pattern shorter than isize::MAX")
}

fn invalid(err: ParseError, substituted: &Substituted) -> PatternError {
    match err {
        ParseError::Invalid { message, offset } => PatternError::Invalid {
            message,
            offset: substituted.written_offset(offset),
        },
        ParseError::TooLong(err) => PatternError::TooLong(err),
    }
}

/// The text parsed as a single expression if it is one, and otherwise as statements, with the
/// mode that worked.  When neither works, the statements' error is the one reported, since a
/// pattern that is not an expression was most likely meant as statements.
fn parse_either(text: &str) -> Result<(Mode, Arena<'_>), ParseError> {
    parse_as(text, Mode::Expression)
        .map(|arena| (Mode::Expression, arena))
        .or_else(|_| parse_as(text, Mode::Module).map(|arena| (Mode::Module, arena)))
}

fn parse_as(text: &str, mode: Mode) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, text), text, mode)
}

/// The kinds whose text is a string's literal text, whatever their range.  Ruff makes an
/// f-string's literal parts, and so a format spec's, nodes of their own, so a placeholder that
/// is a whole literal part - `f"$x"`, `f"{a:$w}"` - is spanned by one; it is still literal.
const LITERAL_TEXT: &[&str] = &[
    "InterpolatedStringLiteralElement",
    "StringLiteral",
    "BytesLiteral",
];

/// The hole a placeholder spanning `range` is: the outermost node below the root whose range is
/// exactly `range`, unless one of those nodes is a string's literal text, in which case it is
/// no hole at all.  The root never is the hole: a pattern that is one placeholder parses to a
/// root spanning just that, and its content, not the root, is the hole.
fn node_spanning(arena: &Arena<'_>, range: &Range<usize>) -> Option<NodeId> {
    let mut spanning = arena
        .nodes()
        .skip(1)
        .filter(|&node| arena.range(node) == *range)
        .peekable();
    let outermost = *spanning.peek()?;
    spanning
        .all(|node| !LITERAL_TEXT.contains(&arena.kind(node).name()))
        .then_some(outermost)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each hole as its name (or `_`), `...` if a sequence, and the kind and text of its node.
    fn holes(pattern: &Pattern) -> Vec<String> {
        let arena = pattern.arena();
        pattern
            .holes()
            .map(|(node, hole)| {
                format!(
                    "{}{} {} {:?}",
                    hole.name.as_deref().unwrap_or("_"),
                    if hole.sequence { "..." } else { "" },
                    arena.kind(node),
                    arena.source(node)
                )
            })
            .collect()
    }

    fn compiled(text: &str) -> Pattern {
        compile_pattern(text).unwrap_or_else(|err| panic!("{text:?}: {err}"))
    }

    #[test]
    fn each_kind_of_hole_is_claimed_as_written() {
        let pattern = compiled("f($a, $_, $rest...)");
        let found = holes(&pattern);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("a ExprName"), "{found:?}");
        assert!(found[1].starts_with("_ ExprName"), "{found:?}");
        assert!(found[2].starts_with("rest... ExprName"), "{found:?}");
        let unbound = compiled("g($...)");
        assert!(holes(&unbound)[0].starts_with("_... ExprName"));
    }

    #[test]
    fn an_expression_is_parsed_as_one_and_statements_as_statements() {
        let call = compiled("handle_request($a, $b)");
        assert_eq!(
            call.arena().kind(call.arena().root()).name(),
            "ModExpression"
        );
        let def = compiled("def $f($args...):\n    $body...\n");
        assert_eq!(def.arena().kind(def.arena().root()).name(), "ModModule");
        let found = holes(&def);
        assert!(found[0].starts_with("f Identifier"), "{found:?}");
        assert!(
            found[1].starts_with("args... ParameterWithDefault"),
            "{found:?}"
        );
        // At statement level the hole is the whole statement, the outermost node there.
        assert!(found[2].starts_with("body... StmtExpr"), "{found:?}");
    }

    #[test]
    fn a_hole_in_a_string_or_a_comment_is_literal_text() {
        for text in [
            "f(\"$x\")",
            "f('''$doc''')",
            "f(b\"$x\")",
            "f(r'$x')",
            "f(f\"cost $x\")",
            "f(f\"$x\")",
            "f(f\"$x{y}\")",
            "f(f\"{y}$x\")",
            "f(f\"{a:$w}\")",
            "f(t\"$x\")",
            "f(x)  # $x",
        ] {
            let pattern = compiled(text);
            assert_eq!(holes(&pattern), Vec::<String>::new(), "{text:?}");
            // Restored as written, so the pattern matches that text.
            assert_eq!(pattern.arena().text(), text);
        }
    }

    #[test]
    fn a_hole_joined_to_the_name_beside_it_is_not_python() {
        // The placeholder merges with the name before it, so no node spans it; it is put back as
        // written, and `x$a` is not Python.
        for text in ["f(x$a)", "f($a$b)"] {
            assert!(
                matches!(compile_pattern(text), Err(PatternError::Invalid { .. })),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_hole_in_an_f_string_field_is_a_hole() {
        let pattern = compiled("f\"value is {$x}\"");
        assert_eq!(holes(&pattern).len(), 1);
        assert!(holes(&pattern)[0].starts_with("x ExprName"));
        let mixed = compiled("f\"{$x} costs $y\"");
        let found = holes(&mixed);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(mixed.arena().text().contains("costs $y"));
    }

    #[test]
    fn a_brace_or_a_bare_sigil_is_never_claimed() {
        assert_eq!(claim("echo ${HOME} $ $1 $"), Ok(vec![]));
        let pattern = compiled("f(\"${HOME}\", $a)");
        assert_eq!(holes(&pattern).len(), 1);
        assert!(pattern.arena().text().starts_with("f(\"${HOME}\""));
    }

    #[test]
    fn names_follow_pythons_identifiers() {
        let claims = claim("$a1 $_x $é $9 $a² $e\u{301}").unwrap();
        let names: Vec<_> = claims.iter().map(|c| c.hole.name.clone()).collect();
        // A superscript digit is not XID_Continue; a combining accent is.
        assert_eq!(
            names,
            [
                Some("a1".into()),
                Some("_x".into()),
                Some("é".into()),
                Some("a".into()),
                Some("e\u{301}".into())
            ]
        );
        assert_eq!(identifier_len("abc def"), 3);
        assert_eq!(identifier_len("abc"), 3);
        assert_eq!(identifier_len("1"), 0);
    }

    #[test]
    fn an_unbound_named_sequence_is_refused_pointing_to_the_one_spelling() {
        let err = compile_pattern("f(a, $_...)").unwrap_err();
        assert_eq!(err, PatternError::UnboundNamedSequence { offset: 5 });
        assert_eq!(err.offset(), Some(5));
        assert_eq!(
            err.to_string(),
            "\"$_...\" is not a hole: a sequence that binds nothing is written \"$...\""
        );
    }

    #[test]
    fn one_name_as_both_a_node_and_a_sequence_is_refused() {
        let err = compile_pattern("f($x, g($x...))").unwrap_err();
        assert_eq!(
            err,
            PatternError::MixedHole {
                name: "x".into(),
                offset: 8,
                sequence: true
            }
        );
        assert!(
            err.to_string()
                .starts_with("\"$x\" is used for one node and, here, for a sequence")
        );
        // The other order names the use at the offset.
        let err = compile_pattern("f($x..., $x)").unwrap_err();
        assert_eq!(err.offset(), Some(9));
        assert_eq!(
            err.to_string(),
            "\"$x\" is used for a sequence and, here, for one node: a name is one or the other, \
             so give the sequence a name of its own"
        );
        assert!(compile_pattern("f($x, $x)").is_ok());
        assert!(compile_pattern("f($x..., g($x...))").is_ok());
        assert!(compile_pattern("f($_, $...)").is_ok());
    }

    #[test]
    fn a_sequence_at_the_edge_of_statements_names_the_side_to_bound_it_on() {
        let err = compile_pattern("$pre...\nx()\n").unwrap_err();
        assert_eq!(err.offset(), Some(0));
        assert!(
            err.to_string()
                .starts_with("\"$pre...\" starts the pattern's statements"),
            "{err}"
        );
        assert!(
            err.to_string().contains("put a statement before it"),
            "{err}"
        );
        let err = compile_pattern("x()\n$...\n").unwrap_err();
        assert_eq!(err.offset(), Some(4));
        assert!(
            err.to_string()
                .starts_with("\"$...\" ends the pattern's statements"),
            "{err}"
        );
        assert!(
            err.to_string().contains("put a statement after it"),
            "{err}"
        );
    }

    #[test]
    fn a_pattern_of_nothing_but_holes_is_refused() {
        for text in ["$x", "$_", "$x...", "$...", "$a\n$b\n"] {
            assert_eq!(
                compile_pattern(text).unwrap_err(),
                PatternError::NoShape,
                "{text:?}"
            );
        }
        assert!(
            PatternError::NoShape
                .to_string()
                .starts_with("the pattern is only holes")
        );
        assert_eq!(PatternError::NoShape.offset(), None);
        assert!(compile_pattern("$a\nx = 1\n").is_ok());
        assert!(compile_pattern("$a + 1").is_ok());
    }

    #[test]
    fn a_pattern_with_no_code_says_so_rather_than_blaming_holes() {
        for text in ["", "   ", "# $x", "\n# only a comment\n"] {
            assert_eq!(
                compile_pattern(text).unwrap_err(),
                PatternError::Empty,
                "{text:?}"
            );
        }
        assert_eq!(
            PatternError::Empty.to_string(),
            "the pattern has no code in it, only whitespace or comments"
        );
    }

    #[test]
    fn an_invalid_pattern_names_its_place_in_the_pattern_as_written() {
        // The error is after two holes, whose placeholders are longer than `$a` and `$b`.
        let err = compile_pattern("f($a, $b))").unwrap_err();
        let PatternError::Invalid { offset, message } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(*offset, 9, "{message}");
        assert_eq!(err.offset(), Some(9));
        assert_eq!(
            err.to_string(),
            format!("the pattern is not Python: {message}")
        );
        // An error inside a placeholder points at the start of the hole.
        let sub = substitute("$abc", &claim("$abc").unwrap(), |_| true, "__h");
        assert_eq!(sub.written_offset(2), 0);
        assert_eq!(sub.written_offset(sub.text.len()), 4);
    }

    #[test]
    fn a_placeholder_never_collides_with_a_name_in_the_pattern() {
        let pattern = compiled("__quaff_hole0($a)");
        let found = holes(&pattern);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("a ExprName"), "{found:?}");
        assert_eq!(placeholder_stem("x __quaff_hole y"), "__quaff_hole_");
    }

    #[test]
    fn text_too_long_is_its_own_error() {
        let err = invalid(
            ParseError::TooLong(quaffed_syntax::TooLong { len: 9 }),
            &substitute("", &[], |_| true, "__h"),
        );
        assert!(matches!(err, PatternError::TooLong(_)));
        assert!(err.to_string().starts_with("the text is 9 bytes"));
    }
}
