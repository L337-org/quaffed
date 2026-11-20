// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Where Python's grammar has a list: the items of each, as byte ranges.
//!
//! A sequence hole stands for one or more items of a list, so the pattern compiler asks this
//! where the lists in a pattern are.  A place that holds one node - an assignment's value, a
//! subscript's slice, an `assert`'s test - is not a list, even where optional syntax follows
//! it, which a sequence there would take as well.  Which places are lists is a fact about
//! Python's grammar, read here from the fields of Ruff's tree, so the engine never needs it.

use std::ops::Range;

use ruff_python_ast::visitor::source_order::{
    SourceOrderVisitor, TraversalSignal, walk_body, walk_node,
};
use ruff_python_ast::{AnyNodeRef, Stmt};
use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::Ranged;

use crate::parse::ParseError;

/// Every list in `source`, parsed in `mode`, as the byte ranges of its items in source order.
/// The lists are those whose items can stand one after another with nothing between them but a
/// separator or an operator: a block's statements; a call's arguments, positional and keyword
/// together; each run of parameters a `/` or a `*` divides from the next; the elements of a
/// list, tuple or set; an assignment's targets but not its value; a `del`'s targets; an import's
/// names; the names of a `global` or `nonlocal`; a `with`'s items; the values joined by one
/// `and` or `or`; a comparison's operands, whatever operators join them; a comprehension's `for`
/// clauses, and each clause's `if` conditions; decorators; type parameters; and the patterns of
/// a sequence, class or alternative pattern.
///
/// A list is a place the tree holds a variable number of items in one node, as the tree has
/// it: `x < y == z` is one comparison of three operands, so its operands are a list, but
/// `x + y + z` is `(x + y) + z`, two operations of two operands each, so no list.
///
/// # Errors
///
/// As Ruff's parser reports them, which the pattern compiler has already met for the same text,
/// since it parses it the same way.
pub(crate) fn lists(source: &str, mode: Mode) -> Result<Vec<Vec<Range<usize>>>, ParseError> {
    let parsed = ruff_python_parser::parse(source, ParseOptions::from(mode))?;
    let mut found = Lists { lists: Vec::new() };
    walk_node(&mut found, AnyNodeRef::from(parsed.syntax()));
    Ok(found.lists)
}

struct Lists {
    lists: Vec<Vec<Range<usize>>>,
}

impl Lists {
    /// Records one list, of the items' ranges, in source order: a call's positional and keyword
    /// arguments are two fields of Ruff's tree, but one list as written.
    fn push<'a>(&mut self, items: impl IntoIterator<Item = &'a (dyn Ranged + 'a)>) {
        let mut ranges: Vec<Range<usize>> =
            items.into_iter().map(|item| item.range().into()).collect();
        if ranges.is_empty() {
            return;
        }
        ranges.sort_by_key(|range| range.start);
        self.lists.push(ranges);
    }
}

fn ranged<T: Ranged>(item: &T) -> &dyn Ranged {
    item
}

impl<'a> SourceOrderVisitor<'a> for Lists {
    fn enter_node(&mut self, node: AnyNodeRef<'a>) -> TraversalSignal {
        match node {
            AnyNodeRef::StmtFunctionDef(def) => self.push(def.decorator_list.iter().map(ranged)),
            AnyNodeRef::StmtClassDef(class) => {
                self.push(class.decorator_list.iter().map(ranged));
            }
            AnyNodeRef::StmtDelete(delete) => self.push(delete.targets.iter().map(ranged)),
            AnyNodeRef::StmtAssign(assign) => self.push(assign.targets.iter().map(ranged)),
            AnyNodeRef::StmtWith(with) => self.push(with.items.iter().map(ranged)),
            AnyNodeRef::StmtImport(import) => self.push(import.names.iter().map(ranged)),
            AnyNodeRef::StmtImportFrom(import) => self.push(import.names.iter().map(ranged)),
            AnyNodeRef::StmtGlobal(global) => self.push(global.names.iter().map(ranged)),
            AnyNodeRef::StmtNonlocal(nonlocal) => self.push(nonlocal.names.iter().map(ranged)),
            AnyNodeRef::ExprBoolOp(op) => self.push(op.values.iter().map(ranged)),
            AnyNodeRef::ExprCompare(compare) => self.push(compare.operands.iter().map(ranged)),
            AnyNodeRef::ExprSet(set) => self.push(set.elts.iter().map(ranged)),
            AnyNodeRef::ExprList(list) => self.push(list.elts.iter().map(ranged)),
            AnyNodeRef::ExprTuple(tuple) => self.push(tuple.elts.iter().map(ranged)),
            AnyNodeRef::ExprListComp(comp) => self.push(comp.generators.iter().map(ranged)),
            AnyNodeRef::ExprSetComp(comp) => self.push(comp.generators.iter().map(ranged)),
            AnyNodeRef::ExprDictComp(comp) => self.push(comp.generators.iter().map(ranged)),
            AnyNodeRef::ExprGenerator(comp) => self.push(comp.generators.iter().map(ranged)),
            AnyNodeRef::Comprehension(comprehension) => {
                self.push(comprehension.ifs.iter().map(ranged));
            }
            AnyNodeRef::Arguments(arguments) => self.push(
                arguments
                    .args
                    .iter()
                    .map(ranged)
                    .chain(arguments.keywords.iter().map(ranged)),
            ),
            AnyNodeRef::Parameters(parameters) => {
                // `/` and `*` divide the parameters, so a sequence on each side of one is bounded
                // by it.  `*args` and `**kwargs` hold one parameter each.
                self.push(parameters.posonlyargs.iter().map(ranged));
                self.push(parameters.args.iter().map(ranged));
                self.push(parameters.kwonlyargs.iter().map(ranged));
            }
            AnyNodeRef::TypeParams(params) => self.push(params.type_params.iter().map(ranged)),
            AnyNodeRef::PatternMatchSequence(sequence) => {
                self.push(sequence.patterns.iter().map(ranged));
            }
            AnyNodeRef::PatternArguments(arguments) => self.push(
                arguments
                    .patterns
                    .iter()
                    .map(ranged)
                    .chain(arguments.keywords.iter().map(ranged)),
            ),
            AnyNodeRef::PatternMatchOr(or) => self.push(or.patterns.iter().map(ranged)),
            _ => {}
        }
        TraversalSignal::Traverse
    }

    // Every block's statements - a module's, a definition's, a branch's, a handler's, a case's -
    // come through here, as the arena's builder marks them.
    fn visit_body(&mut self, body: &'a [Stmt]) {
        self.push(body.iter().map(ranged));
        walk_body(self, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lists in `source`, parsed as a module, each as its items' text.
    fn items(source: &str) -> Vec<Vec<&str>> {
        lists(source, Mode::Module)
            .unwrap()
            .into_iter()
            .map(|list| list.into_iter().map(|range| &source[range]).collect())
            .collect()
    }

    #[test]
    fn a_call_s_arguments_are_one_list_in_source_order() {
        assert_eq!(
            items("f(a, k=1, *b, **c)"),
            [vec!["f(a, k=1, *b, **c)"], vec!["a", "k=1", "*b", "**c"]]
        );
    }

    #[test]
    fn a_single_place_is_no_list() {
        assert_eq!(items("t = u = 1"), [vec!["t = u = 1"], vec!["t", "u"]]);
        assert_eq!(items("x: int = 1"), [vec!["x: int = 1"]]);
        assert_eq!(items("assert c, m"), [vec!["assert c, m"]]);
        assert_eq!(items("a[1:2]"), [vec!["a[1:2]"]]);
    }

    #[test]
    fn parameters_are_divided_by_a_slash_and_a_star() {
        assert_eq!(
            items("def f(a, b, /, c, *d, e, **g): pass"),
            [
                vec!["def f(a, b, /, c, *d, e, **g): pass"],
                vec!["a", "b"],
                vec!["c"],
                vec!["e"],
                vec!["pass"]
            ]
        );
    }

    #[test]
    fn a_comparison_s_operands_are_a_list_but_a_nested_operation_s_are_not() {
        assert_eq!(
            items("x < y == z"),
            [vec!["x < y == z"], vec!["x", "y", "z"]]
        );
        assert_eq!(items("x + y + z"), [vec!["x + y + z"]]);
    }

    #[test]
    fn a_lambda_s_parameters_are_a_list() {
        assert_eq!(
            items("lambda q, r=1: q"),
            [vec!["lambda q, r=1: q"], vec!["q", "r=1"]]
        );
    }
}
