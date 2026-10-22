// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Python source parsed into a syntax arena, and the kinds of node a piece of source contains.

use std::collections::BTreeSet;
use std::fmt;

use quaffed_syntax::{Arena, Builder, Kind, Language, TooLong};
use ruff_python_ast::visitor::source_order::{
    SourceOrderVisitor, TraversalSignal, walk_interpolated_string_element, walk_node,
};
use ruff_python_ast::{AnyNodeRef, NodeKind};
use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::Ranged;

use crate::kinds::kind_of;

/// The language every arena this adapter builds carries.
pub const PYTHON: Language = Language::new("Python");

/// Why source could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Ruff refused the source: its message, verbatim, and the byte offset where the error
    /// starts.
    Invalid {
        /// What the parser said.
        message: String,
        /// The byte offset in the source.
        offset: usize,
    },
    /// The source is longer than an arena's offsets reach, so it was not parsed.
    TooLong(TooLong),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Invalid { message, offset } => write!(f, "at byte {offset}: {message}"),
            ParseError::TooLong(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Invalid { .. } => None,
            ParseError::TooLong(err) => Some(err),
        }
    }
}

impl From<ruff_python_parser::ParseError> for ParseError {
    fn from(err: ruff_python_parser::ParseError) -> Self {
        ParseError::Invalid {
            message: err.error.to_string(),
            offset: err.location.start().to_usize(),
        }
    }
}

impl From<TooLong> for ParseError {
    fn from(err: TooLong) -> Self {
        ParseError::TooLong(err)
    }
}

/// `source` parsed as a module: a file's worth of statements.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid module, or is too long to parse.
/// Nothing partial is returned.
pub fn parse_module(source: &str) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, source), source, Mode::Module)
}

/// `source` parsed as `IPython` source: statements that may include its escape commands -
/// `%timeit`, `!ls`, `?len` and the like - which are not Python and which Ruff parses only in
/// this mode.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not valid `IPython` source, or is too long.
pub fn parse_ipython(source: &str) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, source), source, Mode::Ipython)
}

/// `source` parsed as a single expression.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid expression, or is too long.
pub fn parse_expression(source: &str) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, source), source, Mode::Expression)
}

/// `source` parsed in `mode` into the arena `builder` starts, or the builder's refusal of text
/// too long for it, before Ruff sees the text.
fn parse_from<'text>(
    builder: Result<Builder<'text>, TooLong>,
    source: &'text str,
    mode: Mode,
) -> Result<Arena<'text>, ParseError> {
    let builder = builder?;
    let parsed = ruff_python_parser::parse(source, ParseOptions::from(mode))?;
    Ok(build(builder, AnyNodeRef::from(parsed.syntax())))
}

/// Every kind of node in `source`, parsed as a module.
///
/// # Errors
///
/// As [`parse_module`].  Nothing partial is returned, because kinds counted from a tree with
/// errors in it would claim coverage the corpus does not have.
pub fn kinds_in_module(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    parse_module(source).map(|arena| kinds_in(&arena))
}

/// Every kind of node in `source`, parsed as `IPython` source.
///
/// # Errors
///
/// As [`parse_ipython`].
pub fn kinds_in_ipython(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    parse_ipython(source).map(|arena| kinds_in(&arena))
}

/// Every kind of node in `source`, parsed as a single expression.
///
/// # Errors
///
/// As [`parse_expression`].
pub fn kinds_in_expression(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    parse_expression(source).map(|arena| kinds_in(&arena))
}

fn kinds_in(arena: &Arena<'_>) -> BTreeSet<Kind> {
    arena.nodes().map(|node| arena.kind(node)).collect()
}

/// The arena of `root` and every node beneath it, in one walk of Ruff's tree.
fn build<'text>(builder: Builder<'text>, root: AnyNodeRef<'_>) -> Arena<'text> {
    struct Build<'text>(Builder<'text>);

    impl<'a> SourceOrderVisitor<'a> for Build<'_> {
        fn enter_node(&mut self, node: AnyNodeRef<'a>) -> TraversalSignal {
            self.0.enter(kind_of(node.kind()), node.range().into());
            // Ruff's source-order walk visits a format spec's parts but never enters the spec
            // itself (`InterpolatedElement::visit_source_order`), so the spec would be missing
            // from the arena, its parts hung on the element.  The element's children are walked
            // here instead, with the spec entered around its parts, in the order Ruff's walk
            // uses: the expression, then the spec.
            if let AnyNodeRef::InterpolatedElement(element) = node
                && let Some(spec) = &element.format_spec
            {
                self.visit_expr(&element.expression);
                self.0.enter(
                    kind_of(NodeKind::InterpolatedStringFormatSpec),
                    spec.range().into(),
                );
                for part in &spec.elements {
                    walk_interpolated_string_element(self, part);
                }
                self.0.leave();
                return TraversalSignal::Skip;
            }
            TraversalSignal::Traverse
        }

        fn leave_node(&mut self, _node: AnyNodeRef<'a>) {
            self.0.leave();
        }
    }

    let mut build = Build(builder);
    walk_node(&mut build, root);
    build.0.finish()
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    fn names(kinds: &BTreeSet<Kind>) -> Vec<&'static str> {
        kinds.iter().map(|k| k.name()).collect()
    }

    /// Each node as its kind and its text, indented by depth, so a tree reads as it nests.
    fn outline(arena: &Arena<'_>) -> Vec<String> {
        fn visit(
            arena: &Arena<'_>,
            node: quaffed_syntax::NodeId,
            depth: usize,
            out: &mut Vec<String>,
        ) {
            out.push(format!(
                "{}{} {:?}",
                "  ".repeat(depth),
                arena.kind(node),
                arena.source(node)
            ));
            for child in arena.children(node) {
                visit(arena, child, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        visit(arena, arena.root(), 0, &mut out);
        out
    }

    #[test]
    fn a_module_is_an_arena_of_ruffs_nodes_in_source_order() {
        let arena = parse_module("def f(x):\n    return x\n").unwrap();
        assert_eq!(arena.language(), PYTHON);
        assert_eq!(
            outline(&arena),
            [
                "ModModule \"def f(x):\\n    return x\\n\"",
                "  StmtFunctionDef \"def f(x):\\n    return x\"",
                "    Identifier \"f\"",
                "    Parameters \"(x)\"",
                "      ParameterWithDefault \"x\"",
                "        Parameter \"x\"",
                "          Identifier \"x\"",
                "    StmtReturn \"return x\"",
                "      ExprName \"x\"",
            ]
        );
        // A module's root runs to the end of the text, its final newline included.
        assert_eq!(arena.leading(), "");
        assert_eq!(arena.trailing(), "");
    }

    /// Where Ruff's root starts and ends, as observed: a module's runs from the start of the
    /// text to its end, and an expression's from its first token to the end, so blank lines
    /// before an expression are the arena's leading text.
    #[test]
    fn ruffs_root_spans_the_text_but_blank_lines_before_an_expression() {
        type Parse = fn(&str) -> Result<Arena<'_>, ParseError>;
        let cases: [(&str, Parse, Range<usize>); 6] = [
            ("(a)  # c\n", parse_expression, 0..9),
            ("\n\n(a)\n\n", parse_expression, 2..7),
            ("# c\n\nx = 1  # d\n\n", parse_module, 0..17),
            ("", parse_module, 0..0),
            ("   \n", parse_module, 0..4),
            ("%ls\n# c\n", parse_ipython, 0..8),
        ];
        for (text, parse, root) in cases {
            let arena = parse(text).unwrap();
            assert_eq!(arena.range(arena.root()), root, "{text:?}");
            assert_eq!(
                [
                    arena.leading(),
                    arena.source(arena.root()),
                    arena.trailing()
                ]
                .concat(),
                text
            );
        }
    }

    #[test]
    fn a_module_reports_every_kind_in_it_and_no_others() {
        let found = kinds_in_module("def f(x):\n    return x\n").unwrap();
        assert_eq!(
            names(&found),
            [
                "ExprName",
                "Identifier",
                "ModModule",
                "Parameter",
                "ParameterWithDefault",
                "Parameters",
                "StmtFunctionDef",
                "StmtReturn",
            ]
        );
    }

    #[test]
    fn an_expression_is_parsed_as_one() {
        let found = kinds_in_expression("f(1)").unwrap();
        assert_eq!(
            names(&found),
            [
                "Arguments",
                "ExprCall",
                "ExprName",
                "ExprNumberLiteral",
                "ModExpression"
            ]
        );
        assert!(kinds_in_expression("x = 1").is_err());
    }

    #[test]
    fn ipython_escapes_parse_only_as_ipython() {
        let found = kinds_in_ipython("%timeit f()\nfiles = !ls\n").unwrap();
        assert!(names(&found).contains(&"StmtIpyEscapeCommand"), "{found:?}");
        assert!(names(&found).contains(&"ExprIpyEscapeCommand"), "{found:?}");
        assert!(kinds_in_module("%timeit f()\n").is_err());
    }

    #[test]
    fn a_format_spec_is_a_node_around_its_parts() {
        let arena = parse_expression("f\"{x:>{w}}\"").unwrap();
        assert_eq!(
            outline(&arena),
            [
                "ModExpression \"f\\\"{x:>{w}}\\\"\"",
                "  ExprFString \"f\\\"{x:>{w}}\\\"\"",
                "    FString \"f\\\"{x:>{w}}\\\"\"",
                "      InterpolatedElement \"{x:>{w}}\"",
                "        ExprName \"x\"",
                "        InterpolatedStringFormatSpec \">{w}\"",
                "          InterpolatedStringLiteralElement \">\"",
                "          InterpolatedElement \"{w}\"",
                "            ExprName \"w\"",
            ]
        );
        let without = kinds_in_expression("f\"{x}\"").unwrap();
        assert!(
            !names(&without).contains(&"InterpolatedStringFormatSpec"),
            "{without:?}"
        );
    }

    #[test]
    fn invalid_source_is_an_error_naming_where() {
        let err = kinds_in_module("def f(:\n").unwrap_err();
        assert_eq!(
            err,
            ParseError::Invalid {
                message: "Expected a parameter or the end of the parameter list".to_owned(),
                offset: 6
            }
        );
        assert_eq!(
            err.to_string(),
            "at byte 6: Expected a parameter or the end of the parameter list"
        );
    }

    #[test]
    fn text_too_long_for_an_arena_is_refused_before_ruff_sees_it() {
        // Invalid Python, so a parse would fail differently: the refusal comes first.
        let refused: Result<Builder<'_>, TooLong> = Err(TooLong { len: 6 });
        assert_eq!(
            parse_from(refused, "def (:", Mode::Module).unwrap_err(),
            ParseError::TooLong(TooLong { len: 6 })
        );
        let err = ParseError::from(TooLong { len: 5_000_000_000 });
        assert_eq!(
            err.to_string(),
            "the text is 5000000000 bytes, more than the 4294967295 a parsed file may hold"
        );
        assert!(std::error::Error::source(&err).is_some());
        let invalid = parse_module("def (:").unwrap_err();
        assert!(std::error::Error::source(&invalid).is_none());
    }

    #[test]
    fn invalid_ipython_is_an_error_too() {
        assert!(matches!(
            parse_ipython("%ls\ndef (:\n"),
            Err(ParseError::Invalid { .. })
        ));
    }
}
