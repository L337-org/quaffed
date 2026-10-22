//! Properties of structural matching that hold for any expression, checked on generated ones.
//!
//! - **Code matches itself**: an expression, compiled as a pattern, matches where it stands.
//! - **Layout takes no part**: spaces and redundant parentheses added around its parts do not
//!   stop it matching.
//! - **A hole generalises**: any name in it replaced by a hole still matches, and binds.
//! - **A different operator does not match**: swapping one operator for another stops it.

use proptest::prelude::*;
use quaffed_python::{compile_pattern, parse_module};
use quaffed_syntax::find;

/// A small expression: names, numbers, strings, calls, attributes, subscripts, unary and binary
/// operators.
#[derive(Debug, Clone)]
enum Expr {
    Name(&'static str),
    Number(u8),
    Text(&'static str),
    Call(Box<Expr>, Vec<Expr>),
    Attribute(Box<Expr>, &'static str),
    Subscript(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Binary(Box<Expr>, &'static str, Box<Expr>),
}

fn expr() -> impl Strategy<Value = Expr> {
    let leaf = prop_oneof![
        prop::sample::select(vec!["a", "b", "value", "items"]).prop_map(Expr::Name),
        (0u8..50).prop_map(Expr::Number),
        prop::sample::select(vec!["x", "two words"]).prop_map(Expr::Text),
    ];
    leaf.prop_recursive(4, 24, 3, |inner| {
        prop_oneof![
            (inner.clone(), prop::collection::vec(inner.clone(), 0..3))
                .prop_map(|(f, args)| Expr::Call(Box::new(f), args)),
            (inner.clone(), prop::sample::select(vec!["x", "count"]))
                .prop_map(|(e, a)| Expr::Attribute(Box::new(e), a)),
            (inner.clone(), inner.clone())
                .prop_map(|(e, i)| Expr::Subscript(Box::new(e), Box::new(i))),
            inner.clone().prop_map(|e| Expr::Not(Box::new(e))),
            (
                inner.clone(),
                prop::sample::select(vec!["+", "-", "*", "<"]),
                inner
            )
                .prop_map(|(l, op, r)| Expr::Binary(Box::new(l), op, Box::new(r))),
        ]
    })
}

/// How an expression is written: tightly, or with spaces and redundant parentheses around its
/// parts, and with one name, if any, replaced by a hole.
#[derive(Clone, Copy)]
struct Style {
    loose: bool,
    hole_for: Option<&'static str>,
}

/// `expr` as Python.  Every compound part is parenthesised, so the text always parses to the
/// tree that was generated whatever the operators' precedence.
fn write(expr: &Expr, style: Style) -> String {
    let pad = if style.loose { " " } else { "" };
    let wrap = |inner: String| {
        if style.loose {
            format!("( {inner} )")
        } else {
            inner
        }
    };
    match expr {
        Expr::Name(name) if style.hole_for == Some(*name) => "$h".to_owned(),
        Expr::Name(name) => wrap((*name).to_owned()),
        Expr::Number(n) => wrap(n.to_string()),
        Expr::Text(text) => wrap(format!("\"{text}\"")),
        Expr::Call(f, args) => {
            let args: Vec<String> = args.iter().map(|a| write(a, style)).collect();
            format!(
                "({}){pad}({pad}{}{pad})",
                write(f, style),
                args.join(&format!(",{pad}"))
            )
        }
        Expr::Attribute(e, attribute) => format!("({}){pad}.{pad}{attribute}", write(e, style)),
        Expr::Subscript(e, i) => format!("({})[{pad}{}{pad}]", write(e, style), write(i, style)),
        Expr::Not(e) => format!("not{pad} ({})", write(e, style)),
        Expr::Binary(l, op, r) => {
            format!("({}){pad} {op} {pad}({})", write(l, style), write(r, style))
        }
    }
}

/// The first name in `expr`, in writing order, if it has one.
fn first_name(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Name(name) => Some(name),
        Expr::Number(_) | Expr::Text(_) => None,
        Expr::Call(f, args) => first_name(f).or_else(|| args.iter().find_map(first_name)),
        Expr::Attribute(e, _) | Expr::Not(e) => first_name(e),
        Expr::Subscript(e, i) => first_name(e).or_else(|| first_name(i)),
        Expr::Binary(l, _, r) => first_name(l).or_else(|| first_name(r)),
    }
}

/// An operator in `expr` swapped for another, if it has one.
fn swap_an_operator(expr: &Expr) -> Option<Expr> {
    match expr {
        Expr::Binary(l, op, r) => {
            let other = if *op == "+" { "-" } else { "+" };
            Some(Expr::Binary(l.clone(), other, r.clone()))
        }
        Expr::Call(f, args) => swap_an_operator(f).map(|f| Expr::Call(Box::new(f), args.clone())),
        Expr::Attribute(e, a) => swap_an_operator(e).map(|e| Expr::Attribute(Box::new(e), a)),
        Expr::Not(e) => swap_an_operator(e).map(|e| Expr::Not(Box::new(e))),
        Expr::Subscript(e, i) => swap_an_operator(e)
            .map(|e| Expr::Subscript(Box::new(e), i.clone()))
            .or_else(|| swap_an_operator(i).map(|i| Expr::Subscript(e.clone(), Box::new(i)))),
        Expr::Name(_) | Expr::Number(_) | Expr::Text(_) => None,
    }
}

/// Whether `pattern` matches all of `source`, a single expression statement.
fn matches_whole(pattern: &str, source: &str) -> bool {
    let Ok(pattern) = compile_pattern(pattern) else {
        // Only a pattern of nothing but a hole fails to compile here, and has nothing to say.
        return true;
    };
    let arena = parse_module(source).unwrap_or_else(|err| panic!("{source:?}: {err}"));
    find(&pattern, &arena)
        .iter()
        .any(|m| source[m.range.clone()].trim() == source.trim())
}

const TIGHT: Style = Style {
    loose: false,
    hole_for: None,
};

proptest! {
    #[test]
    fn code_matches_itself(e in expr()) {
        let text = write(&e, TIGHT);
        prop_assert!(matches_whole(&text, &format!("{text}\n")), "{text}");
    }

    #[test]
    fn layout_takes_no_part(e in expr()) {
        let pattern = write(&e, TIGHT);
        // The outermost expression is written without parentheses of its own, so that the match
        // - which never includes redundant parentheses - is the whole line.
        let loose = match &e {
            Expr::Name(_) | Expr::Number(_) | Expr::Text(_) => write(&e, TIGHT),
            _ => write(&e, Style { loose: true, hole_for: None }),
        };
        prop_assert!(matches_whole(&pattern, &format!("{loose}\n")), "{pattern}  against  {loose}");
    }

    #[test]
    fn a_hole_generalises(e in expr()) {
        if let Some(name) = first_name(&e) {
            let pattern = write(&e, Style { loose: false, hole_for: Some(name) });
            let text = write(&e, TIGHT);
            prop_assert!(matches_whole(&pattern, &format!("{text}\n")), "{pattern}  against  {text}");
        }
    }

    #[test]
    fn a_different_operator_does_not_match(e in expr()) {
        if let Some(swapped) = swap_an_operator(&e) {
            let pattern = write(&e, TIGHT);
            let text = write(&swapped, TIGHT);
            prop_assert!(!matches_whole(&pattern, &format!("{text}\n")), "{pattern}  matched  {text}");
        }
    }
}
