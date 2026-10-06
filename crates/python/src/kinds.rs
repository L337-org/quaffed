// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The kinds of node in Python's grammar, as Ruff names them, and which a piece of source
//! contains.

use std::collections::BTreeSet;
use std::fmt;

use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, TraversalSignal, walk_node};
use ruff_python_ast::{AnyNodeRef, NodeKind};

/// A kind of node in Python's grammar, named as Ruff's parser names it: `StmtFunctionDef`,
/// `ExprCall` and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind(&'static str);

impl Kind {
    /// The kind's name, as Ruff's parser gives it.
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

/// Lists every kind once, building [`ALL`] and the conversion from Ruff's `NodeKind`.
///
/// The conversion is a `match` with no catch-all arm, so the compiler holds the list to Ruff's
/// enum: a kind Ruff adds is a non-exhaustive match until it is listed here, a name Ruff does not
/// have does not resolve, and a name listed twice is an unreachable pattern, which the lints
/// refuse.  The names come from the same identifiers, so they are Ruff's own.  Ruff's `NodeKind`
/// offers no list of its variants of its own, which is why the list is written here at all.
macro_rules! node_kinds {
    ($($name:ident),+ $(,)?) => {
        /// Every kind in the grammar, in Ruff's order.
        const ALL: &[Kind] = &[$(Kind(stringify!($name))),+];

        /// The [`Kind`] of a Ruff node.
        fn kind_of(kind: NodeKind) -> Kind {
            match kind {
                $(NodeKind::$name => Kind(stringify!($name)),)+
            }
        }
    };
}

node_kinds! {
    ModModule, ModExpression, StmtFunctionDef, StmtClassDef, StmtReturn, StmtDelete,
    StmtTypeAlias, StmtAssign, StmtAugAssign, StmtAnnAssign, StmtFor, StmtWhile, StmtIf, StmtWith,
    StmtMatch, StmtRaise, StmtTry, StmtAssert, StmtImport, StmtImportFrom, StmtGlobal,
    StmtNonlocal, StmtExpr, StmtPass, StmtBreak, StmtContinue, StmtIpyEscapeCommand, ExprBoolOp,
    ExprNamed, ExprBinOp, ExprUnaryOp, ExprLambda, ExprIf, ExprDict, ExprSet, ExprListComp,
    ExprSetComp, ExprDictComp, ExprGenerator, ExprAwait, ExprYield, ExprYieldFrom, ExprCompare,
    ExprCall, ExprFString, ExprTString, ExprStringLiteral, ExprBytesLiteral, ExprNumberLiteral,
    ExprBooleanLiteral, ExprNoneLiteral, ExprEllipsisLiteral, ExprAttribute, ExprSubscript,
    ExprStarred, ExprName, ExprList, ExprTuple, ExprSlice, ExprIpyEscapeCommand,
    ExceptHandlerExceptHandler, InterpolatedElement, InterpolatedStringLiteralElement,
    PatternMatchValue, PatternMatchSingleton, PatternMatchSequence, PatternMatchMapping,
    PatternMatchClass, PatternMatchStar, PatternMatchAs, PatternMatchOr, TypeParamTypeVar,
    TypeParamTypeVarTuple, TypeParamParamSpec, InterpolatedStringFormatSpec, PatternArguments,
    PatternKeyword, Comprehension, Arguments, Parameters, Parameter, ParameterWithDefault, Keyword,
    Alias, WithItem, MatchCase, Decorator, ElifElseClause, TypeParams, FString, TString,
    StringLiteral, BytesLiteral, Identifier,
}

/// Every kind of node in Python's grammar, as the pinned Ruff knows it, each once.
#[must_use]
pub fn all_kinds() -> &'static [Kind] {
    ALL
}

/// Why source could not be parsed: Ruff's message and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What the parser said, verbatim.
    pub message: String,
    /// The byte offset in the source where the error starts.
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

impl From<ruff_python_parser::ParseError> for ParseError {
    fn from(err: ruff_python_parser::ParseError) -> Self {
        ParseError {
            message: err.error.to_string(),
            offset: err.location.start().to_usize(),
        }
    }
}

/// Every kind of node in `source`, parsed as a module: a file's worth of statements.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid module.  Nothing partial is
/// returned, because kinds counted from a tree with errors in it would claim coverage the corpus
/// does not have.
pub fn kinds_in_module(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    let parsed = ruff_python_parser::parse_module(source)?;
    Ok(kinds_under(AnyNodeRef::from(parsed.syntax())))
}

/// Every kind of node in `source`, parsed as `IPython` source: statements that may include its
/// escape commands - `%timeit`, `!ls`, `?len` and the like - which are not Python and which Ruff
/// parses only in this mode.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not valid `IPython` source.
pub fn kinds_in_ipython(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    let options = ruff_python_parser::ParseOptions::from(ruff_python_parser::Mode::Ipython);
    let parsed = ruff_python_parser::parse(source, options)?;
    Ok(kinds_under(AnyNodeRef::from(parsed.syntax())))
}

/// Every kind of node in `source`, parsed as a single expression.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid expression.
pub fn kinds_in_expression(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    let parsed = ruff_python_parser::parse_expression(source)?;
    Ok(kinds_under(AnyNodeRef::from(parsed.syntax())))
}

/// The kinds of `root` and every node beneath it.
fn kinds_under(root: AnyNodeRef<'_>) -> BTreeSet<Kind> {
    struct Collect(BTreeSet<Kind>);
    impl<'a> SourceOrderVisitor<'a> for Collect {
        fn enter_node(&mut self, node: AnyNodeRef<'a>) -> TraversalSignal {
            self.0.insert(kind_of(node.kind()));
            // Ruff's source-order walk visits a format spec's parts but never enters the spec
            // itself (`InterpolatedElement::visit_source_order` at 0.0.16), so the node is in
            // the tree and invisible to the walk.  It is counted where it is attached.
            if let AnyNodeRef::InterpolatedElement(element) = node
                && element.format_spec.is_some()
            {
                self.0
                    .insert(kind_of(NodeKind::InterpolatedStringFormatSpec));
            }
            TraversalSignal::Traverse
        }
    }
    let mut collect = Collect(BTreeSet::new());
    walk_node(&mut collect, root);
    collect.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(kinds: &BTreeSet<Kind>) -> Vec<&'static str> {
        kinds.iter().map(|k| k.name()).collect()
    }

    #[test]
    fn every_kind_is_listed_once_by_ruffs_name() {
        let all = all_kinds();
        let unique: BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "a kind is listed twice");
        // The count at ruff_python_ast 0.0.16; a Ruff upgrade that changes it is expected to
        // change this line too, deliberately.
        assert_eq!(all.len(), 94);
        assert_eq!(all[0].name(), "ModModule");
        assert_eq!(all[93].to_string(), "Identifier");
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
    fn a_format_spec_is_counted_though_the_walk_skips_it() {
        let with = kinds_in_expression("f\"{x:>10}\"").unwrap();
        assert!(
            names(&with).contains(&"InterpolatedStringFormatSpec"),
            "{with:?}"
        );
        let without = kinds_in_expression("f\"{x}\"").unwrap();
        assert!(
            !names(&without).contains(&"InterpolatedStringFormatSpec"),
            "{without:?}"
        );
    }

    /// Pins the quirk `kinds_under` works around.  If a Ruff upgrade makes its walk enter the
    /// format spec, this fails: delete the workaround, this test and the quirk in `python.md`.
    #[test]
    fn ruffs_walk_still_skips_the_format_spec() {
        struct Entered(Vec<NodeKind>);
        impl<'a> SourceOrderVisitor<'a> for Entered {
            fn enter_node(&mut self, node: AnyNodeRef<'a>) -> TraversalSignal {
                self.0.push(node.kind());
                TraversalSignal::Traverse
            }
        }
        let parsed = ruff_python_parser::parse_expression("f\"{x:>10}\"").unwrap();
        let mut entered = Entered(Vec::new());
        walk_node(&mut entered, AnyNodeRef::from(parsed.syntax()));
        assert!(
            entered.0.contains(&NodeKind::InterpolatedElement),
            "{:?}",
            entered.0
        );
        assert!(
            !entered.0.contains(&NodeKind::InterpolatedStringFormatSpec),
            "Ruff's walk now enters the format spec; the workaround in kinds_under is dead"
        );
    }

    #[test]
    fn invalid_source_is_an_error_naming_where() {
        let err = kinds_in_module("def f(:\n").unwrap_err();
        assert_eq!(err.offset, 6);
        assert_eq!(
            err.message,
            "Expected a parameter or the end of the parameter list"
        );
        assert_eq!(
            err.to_string(),
            "at byte 6: Expected a parameter or the end of the parameter list"
        );
    }
}
