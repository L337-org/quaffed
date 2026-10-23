//! Structural matching of Python patterns against Python source.
//!
//! Each test is a pattern, some source, and the text of every match in source order, so a case
//! reads as what it asserts: operators that differ only in the token between children, comments
//! and whitespace that take no part, holes inside strings, each kind of hole, a name used twice,
//! a match inside a match, a string's content, redundant parentheses, and runs of statements
//! that stay in one block.

use quaffed_python::{PatternError, compile_pattern, parse_module};
use quaffed_syntax::find;

/// The text of every match of `pattern` in `source`, parsed as a module.
fn matches(pattern: &str, source: &str) -> Vec<String> {
    let pattern = compile_pattern(pattern).unwrap_or_else(|err| panic!("{pattern:?}: {err}"));
    let arena = parse_module(source).unwrap_or_else(|err| panic!("{source:?}: {err}"));
    find(&pattern, &arena)
        .into_iter()
        .map(|m| source[m.range].to_owned())
        .collect()
}

fn none() -> Vec<String> {
    Vec::new()
}

#[test]
fn an_operator_is_part_of_the_shape() {
    assert_eq!(matches("$a + $b", "x - y\n"), none());
    assert_eq!(matches("$a + $b", "x + y\n"), ["x + y"]);
    assert_eq!(matches("$a < $b", "a > b\n"), none());
    assert_eq!(matches("$a < $b", "a < b\n"), ["a < b"]);
    assert_eq!(matches("not $a", "-a\n"), none());
    assert_eq!(matches("not $a", "not a\n"), ["not a"]);
}

#[test]
fn comments_and_whitespace_between_children_take_no_part() {
    assert_eq!(
        matches("f($a, $b)", "f( x ,  # why\n    y )\n"),
        ["f( x ,  # why\n    y )"]
    );
    assert_eq!(matches("x+1", "x  +  1\n"), ["x  +  1"]);
    assert_eq!(
        matches("if $c:\n    $b\n", "if ready:  # c\n\n    go()\n"),
        ["if ready:  # c\n\n    go()"]
    );
}

#[test]
fn redundant_parentheses_take_no_part_but_structural_ones_do() {
    assert_eq!(matches("f($a)", "f((x))\n"), ["f((x))"]);
    assert_eq!(matches("f(x)", "f(((x)))\n"), ["f(((x)))"]);
    assert_eq!(matches("$a + $b", "(x) + (y)\n"), ["(x) + (y)"]);
    // (a + b) * c and a + b * c are different trees.
    assert_eq!(matches("($a + $b) * $c", "a + b * c\n"), none());
    assert_eq!(matches("($a + $b) * $c", "(a + b) * c\n"), ["(a + b) * c"]);
    // A call's own parentheses are its arguments', never redundant.
    assert_eq!(matches("f()", "f()\n"), ["f()"]);
    assert_eq!(matches("f()", "f(x)\n"), none());
    // A tuple's parentheses stay: f((a, b)) is one argument, f(a, b) two.
    assert_eq!(matches("f($x, $y)", "f((a, b))\n"), none());
}

#[test]
fn a_string_matches_by_its_content_not_its_quotes_or_prefix() {
    let source = "f(\"x\")\nf('x')\nf(r\"x\")\nf('''x''')\nf(\"y\")\n";
    assert_eq!(
        matches("f(\"x\")", source),
        ["f(\"x\")", "f('x')", "f(r\"x\")", "f('''x''')"]
    );
    // Escapes are compared as written, and implicitly concatenated parts joined.
    assert_eq!(
        matches("f(\"a\\nb\")", "f('a\\nb')\nf('a\\tb')\n"),
        ["f('a\\nb')"]
    );
    assert_eq!(matches("f(\"ab\")", "f(\"a\" 'b')\n"), ["f(\"a\" 'b')"]);
    // A bytes literal is not a string.
    assert_eq!(matches("f(\"x\")", "f(b\"x\")\n"), none());
    // A plain part of an f-string's concatenation matches by its content too.
    assert_eq!(
        matches("f(\"a\" f\"{$x}\")", "f('a' f\"{y}\")\n"),
        ["f('a' f\"{y}\")"]
    );
    // An f-string's quotes and prefix take no part either; its literal text does.
    assert_eq!(matches("f\"a{$x}\"", "f'a{y}'\nf\"b{y}\"\n"), ["f'a{y}'"]);
}

#[test]
fn a_hole_inside_a_string_or_a_comment_is_literal_text() {
    assert_eq!(matches("f(\"$x\")", "f(\"$x\")\nf(\"a\")\n"), ["f(\"$x\")"]);
    assert_eq!(
        matches("f(\"cost $x\")", "f('cost $x')\nf('cost 5')\n"),
        ["f('cost $x')"]
    );
    // In an f-string's field it is a hole.
    assert_eq!(
        matches("f\"value is {$x}\"", "f\"value is {a + 1}\"\n"),
        ["f\"value is {a + 1}\""]
    );
    // In an f-string's literal text it is text.
    assert_eq!(matches("f\"$x\"", "f\"$x\"\nf\"y\"\n"), ["f\"$x\""]);
}

#[test]
fn each_kind_of_hole_matches_what_it_stands_for() {
    let source = "handle_request(req, resp)\nhandle_request(req)\n";
    assert_eq!(
        matches("handle_request($a, $b)", source),
        ["handle_request(req, resp)"]
    );
    assert_eq!(
        matches("handle_request($_)", source),
        ["handle_request(req)"]
    );
    assert_eq!(
        matches("handle_request($args...)", source),
        ["handle_request(req, resp)", "handle_request(req)"]
    );
    assert_eq!(
        matches("handle_request($...)", source),
        ["handle_request(req, resp)", "handle_request(req)"]
    );
    // A hole never matches nothing.
    assert_eq!(matches("g($...)", "g()\n"), none());
    assert_eq!(
        matches("g($first, $rest...)", "g(1)\ng(1, 2, 3)\n"),
        ["g(1, 2, 3)"]
    );
}

#[test]
fn a_name_used_twice_matches_equal_code_both_times() {
    let source = "f(a + b, a  +  b)\nf(a, b)\nf(g(x), g( x ))\n";
    assert_eq!(
        matches("f($x, $x)", source),
        ["f(a + b, a  +  b)", "f(g(x), g( x ))"]
    );
    // Different kinds, different strings, and the same parts in another order are not equal.
    assert_eq!(
        matches("f($x, $x)", "f(a, 1)\nf(\"a\", 'b')\nf(\"a\", 'a')\n"),
        ["f(\"a\", 'a')"]
    );
    assert_eq!(
        matches("f($x, $x)", "f(a[1:], a[:1])\nf(a[:1], a[ : 1])\n"),
        ["f(a[:1], a[ : 1])"]
    );
    // A sequence named twice matches the same run both times.
    assert_eq!(
        matches(
            "f($xs...) + g($xs...)",
            "f(1, 2) + g(1, 2)\nf(1, 2) + g(1, 3)\n"
        ),
        ["f(1, 2) + g(1, 2)"]
    );
}

#[test]
fn a_name_used_twice_matches_the_same_code_whatever_its_role() {
    // An argument and a keyword, a name and an attribute, a parameter and a name.
    assert_eq!(matches("f($k=$k)", "f(a=a)\nf(a=b)\n"), ["f(a=a)"]);
    assert_eq!(
        matches("$x = $o.$x", "name = self.name\nname = self.other\n"),
        ["name = self.name"]
    );
    assert_eq!(
        matches(
            "def $f($p):\n    return $p\n",
            "def g(a):\n    return a\n\ndef h(a):\n    return b\n"
        ),
        ["def g(a):\n    return a"]
    );
}

#[test]
fn a_trailing_comma_or_a_semicolon_takes_no_part() {
    assert_eq!(matches("f($a, $b)", "f(x, y,)\n"), ["f(x, y,)"]);
    assert_eq!(
        matches(
            "handle_request($args...)",
            "handle_request(\n    a,\n    b,\n)\n"
        ),
        ["handle_request(\n    a,\n    b,\n)"]
    );
    assert_eq!(
        matches("def $f($a, $b):\n    pass\n", "def g(a, b,):\n    pass\n").len(),
        1
    );
    assert_eq!(matches("[1, 2]", "[1, 2,]\n"), ["[1, 2,]"]);
    assert_eq!(
        matches("x = 1\ny = 2\n", "x = 1; y = 2\n"),
        ["x = 1; y = 2"]
    );
    assert_eq!(
        matches("return 1", "def f():\n    return 1;\n"),
        ["return 1"]
    );
    // A one-element tuple stays a tuple, and a tuple index stays apart from a name index.
    assert_eq!(matches("($a,)", "(x,)\n(x)\n"), ["(x,)"]);
    assert_eq!(matches("$s[1]", "a[1,]\na[1]\n"), ["a[1]"]);
}

#[test]
fn a_sequence_at_the_edge_of_statements_is_refused_rather_than_taking_the_block() {
    // Nothing would bound it on that side, so it would reach the edge of its block.
    let refused = |pattern: &str| match compile_pattern(pattern) {
        Err(PatternError::UnboundedSequence {
            name,
            offset,
            leading,
        }) => (name, offset, leading),
        other => panic!("{pattern:?}: {other:?}"),
    };
    assert_eq!(refused("$pre...\nx()\n"), (Some("pre".into()), 0, true));
    assert_eq!(
        refused("x = 1\n$rest...\n"),
        (Some("rest".into()), 6, false)
    );
    assert_eq!(refused("x()\n$...\n"), (None, 4, false));
    // At both edges, the leading one is named.
    assert_eq!(refused("$a...\nx()\n$b...\n"), (Some("a".into()), 0, true));
    // Bounded by code on both sides, or by the node it stands in, a sequence is allowed.
    assert!(compile_pattern("w()\n$mid...\nx()\n").is_ok());
    assert!(compile_pattern("def $f():\n    $body...\n").is_ok());
    assert!(compile_pattern("if $c:\n    x()\n    $rest...\n").is_ok());
    assert!(compile_pattern("f($first, $rest...)").is_ok());
}

#[test]
fn a_sequence_between_statements_takes_as_few_as_it_can() {
    let source = "open_db()\na = 1\nclose_db()\nb = 2\nopen_db()\nc = 3\nclose_db()\n";
    // One match per pair, never one from the first open to the last close.
    assert_eq!(
        matches("open_db()\n$body...\nclose_db()\n", source),
        [
            "open_db()\na = 1\nclose_db()",
            "open_db()\nc = 3\nclose_db()"
        ]
    );
    // From each place it can start, as far as the pattern needs, and at least one statement.
    let source = "w()\nw()\nx()\nx()\n";
    assert_eq!(
        matches("w()\n$mid...\nx()\n", source),
        ["w()\nw()\nx()", "w()\nx()\nx()"]
    );
    assert_eq!(matches("w()\n$mid...\nw()\n", source), none());
}

#[test]
fn a_match_inside_a_match_is_its_own_match() {
    assert_eq!(matches("f($x)", "f(f(1))\n"), ["f(f(1))", "f(1)"]);
}

#[test]
fn statements_in_a_row_match_where_they_stand_together_in_one_block() {
    let source = "def g():\n    x = 1\n    y = 2\n    z = 3\n\nx = 1\nq = 0\ny = 2\n";
    assert_eq!(matches("x = 1\ny = 2\n", source), ["x = 1\n    y = 2"]);
    assert_eq!(
        matches("def $f():\n    $body...\n", source),
        ["def g():\n    x = 1\n    y = 2\n    z = 3"]
    );
    assert_eq!(
        matches(
            "def $f($args...):\n    return $v\n",
            "def h(a, b):\n    return a\n"
        ),
        ["def h(a, b):\n    return a"]
    );
}

#[test]
fn a_run_of_statements_or_a_sequence_stays_in_one_block() {
    // Within a function's body, but never out of it.
    assert_eq!(
        matches(
            "y = 1\n$mid...\nreturn $x\n",
            "def f(a):\n    y = 1\n    g()\n    return a\n"
        ),
        ["y = 1\n    g()\n    return a"]
    );
    assert_eq!(
        matches(
            "w()\n$mid...\nx()\n",
            "if c:\n    w()\nelse:\n    v()\n    x()\n"
        ),
        none()
    );
    // Not from a branch into its else clause.
    assert_eq!(
        matches("if $c:\n    $body...\n", "if a:\n    x()\nelse:\n    y()\n"),
        none()
    );
    assert_eq!(
        matches(
            "x = 1\n$rest...\nz()\n",
            "if c:\n    x = 1\nelse:\n    y()\n    z()\n"
        ),
        none()
    );
    // Not across a for's else, nor from a try's body over its handlers.
    assert_eq!(
        matches(
            "for $x in $y:\n    $body...\n",
            "for i in j:\n    p()\nelse:\n    q()\n"
        ),
        none()
    );
    assert_eq!(
        matches(
            "try:\n    $body...\nfinally:\n    $f...\n",
            "try:\n    a()\nexcept E:\n    b()\nfinally:\n    c()\n"
        ),
        none()
    );
    // Within its own block it still takes every statement.
    assert_eq!(
        matches(
            "for $x in $y:\n    $body...\n",
            "for i in j:\n    p()\n    q()\n"
        ),
        ["for i in j:\n    p()\n    q()"]
    );
}

#[test]
fn a_statement_pattern_matches_a_statement_of_that_shape() {
    assert_eq!(
        matches(
            "return $x",
            "def f():\n    return 1\n\ndef g():\n    return\n"
        ),
        ["return 1"]
    );
    assert_eq!(matches("$x = 0", "a = 0\nb = 1\n"), ["a = 0"]);
}
