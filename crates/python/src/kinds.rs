// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The kinds of node in Python's grammar, named as Ruff names them: `StmtFunctionDef`,
//! `ExprCall` and so on.

use quaffed_syntax::Kind;
use ruff_python_ast::NodeKind;

macro_rules! node_kinds {
    ($($name:ident),+ $(,)?) => {
        /// Every kind in the grammar, in Ruff's order.
        const ALL: &[Kind] = &[$(Kind::new(stringify!($name))),+];

        /// The [`Kind`] of a Ruff node.
        pub(crate) fn kind_of(kind: NodeKind) -> Kind {
            match kind {
                $(NodeKind::$name => Kind::new(stringify!($name)),)+
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ruff_python_ast::AnyNodeRef;
    use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, TraversalSignal, walk_node};

    use super::*;

    #[test]
    fn every_kind_is_listed_once_by_ruffs_name() {
        let all = all_kinds();
        let unique: BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "a kind is listed twice");
        // The count of the pinned Ruff's enum; an upgrade that changes it changes this line too,
        // deliberately.
        assert_eq!(all.len(), 94);
        assert_eq!(all[0].name(), "ModModule");
        assert_eq!(all[93].to_string(), "Identifier");
    }

    /// Pins the quirk the arena builder in `parse.rs` works around.  If a Ruff upgrade makes its
    /// walk enter the format spec, this fails: delete the workaround, this test and the quirk in
    /// `python.md`.
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
            "Ruff's walk now enters the format spec; the workaround in parse.rs is dead"
        );
    }
}
