// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Statements and expressions outside the construct they need: a `return` outside a function, a
//! `yield` outside one, an `await` outside an `async` one, a `break` or `continue` outside a
//! loop.
//!
//! Each parses anywhere a statement or an expression may stand, so the parser accepts a file
//! holding one in the wrong place, and only Python's compiler refuses it, as a `SyntaxError`.  An
//! edit can put one there by moving code out of what held it, so the edit asks.  The rules are
//! Python's:
//!
//! - `return` needs a function, and a class body between it and the function is not one.
//! - `yield` and `yield from` need a function or a lambda; a class body or a comprehension
//!   between is not one, though a comprehension's first iterable is evaluated outside it.
//! - `await` needs an `async` function, through any comprehension; a lambda, a class body or a
//!   plain function is not one.
//! - `break` and `continue` need the body of a `for` or `while` loop - not its `else` - with no
//!   function, lambda or class between.
//!
//! A function's decorators, defaults and annotations, and a class's bases and decorators, are
//! evaluated where the definition stands, so they are judged there.

use ruff_python_ast::visitor::{Visitor, walk_comprehension, walk_expr, walk_stmt};
use ruff_python_ast::{Comprehension, Expr, Stmt};
use ruff_text_size::Ranged;

use crate::ParseError;

/// A statement or expression outside the construct it needs: what it is, as Python spells it,
/// and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misplaced {
    /// `return`, `yield`, `yield from`, `await`, `break` or `continue`.
    pub what: &'static str,
    /// The byte offset in the source where it starts.
    pub offset: usize,
}

/// Every statement and expression in `source`, parsed as a module, that stands outside the
/// construct it needs, in source order.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid module.  Nothing partial is
/// returned.
pub fn misplaced(source: &str) -> Result<Vec<Misplaced>, ParseError> {
    let parsed = ruff_python_parser::parse_module(source)?;
    let mut walk = Walk {
        within: Vec::new(),
        found: Vec::new(),
    };
    walk.visit_body(&parsed.syntax().body);
    walk.found.sort_by_key(|m| m.offset);
    Ok(walk.found)
}

/// What a node stands within, innermost last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Within {
    Function { is_async: bool },
    Lambda,
    Class,
    Comprehension,
    Loop,
}

struct Walk {
    within: Vec<Within>,
    found: Vec<Misplaced>,
}

impl Walk {
    fn found(&mut self, what: &'static str, node: &impl Ranged) {
        self.found.push(Misplaced {
            what,
            offset: node.start().to_usize(),
        });
    }

    /// Whether the innermost of the constructs that decide `what` is one that allows it, looking
    /// out past those that do not decide it.
    fn allowed(&self, what: &str) -> bool {
        for within in self.within.iter().rev() {
            let decided = match (what, within) {
                ("return", Within::Function { .. })
                | ("yield", Within::Function { .. } | Within::Lambda)
                | ("loop", Within::Loop) => Some(true),
                ("return", Within::Class)
                | ("yield", Within::Class | Within::Comprehension)
                | ("await", Within::Lambda | Within::Class)
                | ("loop", Within::Function { .. } | Within::Lambda | Within::Class) => Some(false),
                ("await", Within::Function { is_async }) => Some(*is_async),
                _ => None,
            };
            if let Some(decided) = decided {
                return decided;
            }
        }
        false
    }

    fn within(&mut self, within: Within, walk: impl FnOnce(&mut Self)) {
        self.within.push(within);
        walk(self);
        self.within.pop();
    }

    /// A comprehension's generators and elements: the first iterable where the comprehension
    /// stands, the rest within it.
    fn comprehension<'a>(&mut self, generators: &'a [Comprehension], elements: &[&'a Expr]) {
        let Some((first, rest)) = generators.split_first() else {
            return;
        };
        self.visit_expr(&first.iter);
        self.within(Within::Comprehension, |walk| {
            walk.visit_expr(&first.target);
            for condition in &first.ifs {
                walk.visit_expr(condition);
            }
            for generator in rest {
                walk_comprehension(walk, generator);
            }
            for element in elements {
                walk.visit_expr(element);
            }
        });
    }
}

impl<'a> Visitor<'a> for Walk {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(def) => {
                for decorator in &def.decorator_list {
                    self.visit_decorator(decorator);
                }
                if let Some(type_params) = &def.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_parameters(&def.parameters);
                if let Some(returns) = &def.returns {
                    self.visit_annotation(returns);
                }
                self.within(
                    Within::Function {
                        is_async: def.is_async,
                    },
                    |walk| walk.visit_body(&def.body),
                );
            }
            Stmt::ClassDef(class) => {
                for decorator in &class.decorator_list {
                    self.visit_decorator(decorator);
                }
                if let Some(type_params) = &class.type_params {
                    self.visit_type_params(type_params);
                }
                if let Some(arguments) = &class.arguments {
                    self.visit_arguments(arguments);
                }
                self.within(Within::Class, |walk| walk.visit_body(&class.body));
            }
            Stmt::For(each) => {
                self.visit_expr(&each.target);
                self.visit_expr(&each.iter);
                self.within(Within::Loop, |walk| walk.visit_body(&each.body));
                self.visit_body(&each.orelse);
            }
            Stmt::While(each) => {
                self.visit_expr(&each.test);
                self.within(Within::Loop, |walk| walk.visit_body(&each.body));
                self.visit_body(&each.orelse);
            }
            Stmt::Return(_) => {
                if !self.allowed("return") {
                    self.found("return", stmt);
                }
                walk_stmt(self, stmt);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {
                if !self.allowed("loop") {
                    let what = if matches!(stmt, Stmt::Break(_)) {
                        "break"
                    } else {
                        "continue"
                    };
                    self.found(what, stmt);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameters(parameters);
                }
                self.within(Within::Lambda, |walk| walk.visit_expr(&lambda.body));
            }
            Expr::ListComp(comp) => self.comprehension(&comp.generators, &[&comp.elt]),
            Expr::SetComp(comp) => self.comprehension(&comp.generators, &[&comp.elt]),
            Expr::Generator(comp) => self.comprehension(&comp.generators, &[&comp.elt]),
            Expr::DictComp(comp) => {
                let mut elements: Vec<&Expr> = comp.key.iter().map(AsRef::as_ref).collect();
                elements.push(&comp.value);
                self.comprehension(&comp.generators, &elements);
            }
            Expr::Yield(_) | Expr::YieldFrom(_) => {
                if !self.allowed("yield") {
                    let what = if matches!(expr, Expr::Yield(_)) {
                        "yield"
                    } else {
                        "yield from"
                    };
                    self.found(what, expr);
                }
                walk_expr(self, expr);
            }
            Expr::Await(_) => {
                if !self.allowed("await") {
                    self.found("await", expr);
                }
                walk_expr(self, expr);
            }
            _ => walk_expr(self, expr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whats(source: &str) -> Vec<&'static str> {
        misplaced(source)
            .unwrap()
            .into_iter()
            .map(|m| m.what)
            .collect()
    }

    #[test]
    fn each_is_allowed_inside_what_it_needs() {
        let source = "\
def f():
    return 1
def g():
    yield 1
    yield from x
    lambda: (yield)
async def h():
    await x
    [await y for y in z]
    (await y for y in z)
for i in r:
    break
while x:
    if y:
        continue
x = [y for y in (yield)] if 0 else 0
";
        // The last line's yield is outside a function: its first iterable is the module's.
        assert_eq!(whats(source), ["yield"]);
    }

    #[test]
    fn each_is_found_outside_what_it_needs() {
        assert_eq!(whats("return 1\n"), ["return"]);
        assert_eq!(whats("yield 1\n"), ["yield"]);
        assert_eq!(whats("x = yield from y\n"), ["yield from"]);
        assert_eq!(whats("await x\n"), ["await"]);
        assert_eq!(whats("break\n"), ["break"]);
        assert_eq!(whats("continue\n"), ["continue"]);
    }

    #[test]
    fn a_class_body_between_is_not_the_function() {
        let source = "\
def f():
    class C:
        return 1
        yield 2
for i in r:
    class D:
        break
async def g():
    class E:
        await x
";
        assert_eq!(whats(source), ["return", "yield", "break", "await"]);
    }

    #[test]
    fn a_function_lambda_or_comprehension_between_decides_await_and_loops() {
        let source = "\
async def f():
    def g():
        await x
    lambda: await y
for i in r:
    def h():
        break
    lambda: [z for z in w]
def k():
    [(yield) for z in w]
";
        assert_eq!(whats(source), ["await", "await", "break", "yield"]);
    }

    #[test]
    fn a_loops_else_and_a_definitions_defaults_are_outside_it() {
        let source = "\
for i in r:
    pass
else:
    break
def f(x=(yield)):
    pass
";
        assert_eq!(whats(source), ["break", "yield"]);
    }
}
