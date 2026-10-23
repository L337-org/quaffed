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
    // A tuple passed as one argument is still one argument: f((a, b)) is not f(a, b).
    assert_eq!(matches("f($x, $y)", "f((a, b))\n"), none());
    assert_eq!(matches("f(($x, $y))", "f(a, b)\n"), none());
}

#[test]
fn a_tuples_or_generators_own_parentheses_take_no_part() {
    // One tree to CPython, so one shape: the parentheses are only how it is written.
    assert_eq!(
        matches("return $a, $b", "return a, b\nreturn (a, b)\n"),
        ["return a, b", "return (a, b)"]
    );
    assert_eq!(
        matches(
            "for $x in ($a, $b):\n    pass\n",
            "for i in a, b:\n    pass\n"
        ),
        ["for i in a, b:\n    pass"]
    );
    assert_eq!(
        matches("h($q for $q in $r)", "h(q for q in r)\nh((q for q in r))\n"),
        ["h(q for q in r)", "h((q for q in r))"]
    );
    // And a name bound to one compares equal to the other, as a tuple assigned and returned.
    assert_eq!(
        matches("$x = $y\nreturn $x\n", "a, b = f()\nreturn (a, b)\n"),
        ["a, b = f()\nreturn (a, b)"]
    );
    // An empty tuple's parentheses are all there is of it.
    assert_eq!(matches("f(())", "f(())\nf(x)\n"), ["f(())"]);
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
    // Only the run it took, not what follows it: here the first binding stops before `, 0`.
    assert_eq!(
        matches(
            "f($xs..., 0) + g($xs...)",
            "f(1, 0) + g(1)\nf(1, 2, 0) + g(1, 2)\nf(1, 0) + g(1, 0)\n"
        ),
        ["f(1, 0) + g(1)", "f(1, 2, 0) + g(1, 2)"]
    );
    // A name bound inside a statement - here a call's arguments - and used again as statements
    // in the run compares their code: `x` as an argument and as a statement.
    assert_eq!(
        matches(
            "f($a...)\nh()\n$a...\ng()\n",
            "f(x)\nh()\nx\ng()\nf(x)\nh()\ny\ng()\n"
        ),
        ["f(x)\nh()\nx\ng()"]
    );
    // And between statements, where the run is cut from a block: the second statement is
    // reached only once a one-statement try has failed.
    assert_eq!(
        matches(
            "a()\n$body...\nb()\n$body...\nc()\n",
            "a()\nx\ny\nb()\nx\ny\nc()\na()\nx\ny\nb()\nx\nc()\n"
        ),
        ["a()\nx\ny\nb()\nx\ny\nc()"]
    );
}

#[test]
fn a_name_used_twice_tells_apart_trees_whose_tokens_read_alike() {
    let source = "\
same([(a, b)], [a, b])
same((a + b) * c, a + b * c)
same(x[a,], x[a])
same(f((x,)), f(x))
same({(q for q in r)}, {q for q in r})
same([(q for q in r)], [q for q in r])
same([a, b], [a, (b)])
";
    // Only the last holds the same code twice, `(b)`'s parentheses being redundant.
    assert_eq!(matches("same($x, $x)", source), ["same([a, b], [a, (b)])"]);
    // Nor across statements: a tuple of a tuple and a name is not a name and a tuple, though
    // each is still the same tuple written without its parentheses.
    assert_eq!(
        matches(
            "$x = $y\nreturn $y\n",
            "def f():\n    t = (a, b), c\n    return a, (b, c)\n\
             def g():\n    t = (a, b), c\n    return ((a, b), c)\n"
        ),
        ["t = (a, b), c\n    return ((a, b), c)"]
    );
    // A one-element tuple's comma is the tuple, so it stays in its shape too.
    assert_eq!(matches("x[$a,]", "x[a,]\nx[a]\n"), ["x[a,]"]);
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
fn a_run_whose_search_would_grow_faster_than_quadratic_is_refused() {
    // Two sequences together: nothing settles how many statements each takes.
    match compile_pattern("w()\n$a...\n$b...\nx()\n") {
        Err(PatternError::SequencesTogether { name, offset }) => {
            assert_eq!((name.as_deref(), offset), (Some("b"), 10));
        }
        other => panic!("{other:?}"),
    }
    // Two places where the statement a sequence stops at decides later code: `$b` after `x()`,
    // and the second `$a` after `y()`, which is named.
    match compile_pattern("w()\n$a...\nx()\n$b...\ny()\n$a...\nx()\n$b...\nz()\n") {
        Err(PatternError::SecondChoice { spelled, offset }) => {
            assert_eq!((spelled.as_str(), offset), ("$a...", 24));
        }
        other => panic!("{other:?}"),
    }
    // One such place is allowed: a sequence named twice, or a name used across statements.
    assert!(compile_pattern("w()\n$a...\nx()\n$a...\ny()\n").is_ok());
    assert!(compile_pattern("f($v)\n$...\ng($v)\n$...\nh($v)\n").is_ok());
    assert!(compile_pattern("w()\n$...\nx()\n$...\ny()\n").is_ok());
    // Two sequences together inside one node are not a run of statements, and cost nothing.
    assert!(compile_pattern("f($a..., $b...)").is_ok());
    // But choices inside a node count as they do in a run: `$b` after `x`, then `$a` again.
    match compile_pattern("f(w, $a..., x, $b..., y, $a..., x, $b..., z)") {
        Err(PatternError::SecondChoice { spelled, offset }) => {
            assert_eq!((spelled.as_str(), offset), ("$a...", 25));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        compile_pattern("def $f():\n    w()\n    $a...\n    x()\n    $a...\n    y()\n").is_ok()
    );
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

/// Whether `a` as a pattern matches the whole of `b`, and the other way round.
fn alike(a: &str, b: &str) -> (bool, bool) {
    let whole = |pattern: &str, source: &str| matches(pattern, source).iter().any(|m| m == source);
    (whole(a, b), whole(b, a))
}

#[test]
fn parentheses_and_commas_cpythons_tree_does_not_record_take_no_part() {
    // Each pair is one tree to CPython 3.14.8, so each matches the other.
    for (a, b) in [
        ("from x import (a, b)", "from x import a, b"),
        ("from x import (a, b,)", "from x import a, b"),
        (
            "with (open(p), open(q)):\n    pass",
            "with open(p), open(q):\n    pass",
        ),
        (
            "with (open(p), open(q),):\n    pass",
            "with open(p), open(q):\n    pass",
        ),
        ("class A():\n    pass", "class A:\n    pass"),
        ("x = (a, b,)", "x = a, b"),
        ("x = a, b,", "x = a, b"),
    ] {
        assert_eq!(alike(a, b), (true, true), "{a:?} and {b:?}");
    }
    // And each of these is two trees to CPython, so neither matches the other.
    for (a, b) in [
        ("with (a, b) as c:\n    pass", "with a, b as c:\n    pass"),
        ("class A(B):\n    pass", "class A:\n    pass"),
        ("x = (a,)", "x = a"),
    ] {
        assert_eq!(alike(a, b), (false, false), "{a:?} and {b:?}");
    }
}

#[test]
fn the_spacing_a_self_documenting_field_prints_takes_part() {
    // `f"{v = }"` prints `v = 1` where `f"{v=}"` prints `v=1`, and CPython's trees differ.
    for (a, b) in [
        ("print(f\"{v=}\")", "print(f\"{v = }\")"),
        ("print(f\"{ v=}\")", "print(f\"{v=}\")"),
        ("print(f\"{v=!r}\")", "print(f\"{v = !r}\")"),
    ] {
        assert_eq!(alike(a, b), (false, false), "{a:?} and {b:?}");
    }
    // Spacing in a field that does not print itself is still layout.
    assert_eq!(alike("print(f\"{v}\")", "print(f\"{ v }\")"), (true, true));
    assert_eq!(
        matches("print(f\"{$x = }\")", "print(f\"{v = }\")\n"),
        ["print(f\"{v = }\")"]
    );
}

#[test]
fn implicitly_concatenated_f_strings_are_not_joined() {
    // One tree to CPython, but quaff keeps an f-string's parts apart, as decided for the MVP.
    let source = "f\"{a}\" f\"{b}\"\n";
    assert_eq!(matches("f\"{$x}{$y}\"", source), none());
    assert_eq!(
        matches("f\"{$x}\" f\"{$y}\"", source),
        ["f\"{a}\" f\"{b}\""]
    );
}

#[test]
fn a_hole_inside_a_dotted_name_is_refused_saying_why() {
    for (pattern, offset) in [("import a.$m", 9), ("from a.$m import x", 7)] {
        match compile_pattern(pattern) {
            Err(err @ PatternError::InsideAName { .. }) => {
                assert_eq!(err.offset(), Some(offset), "{pattern}");
                assert!(
                    err.to_string()
                        .contains("stands inside a name Python reads as one")
                );
            }
            other => panic!("{pattern}: {other:?}"),
        }
    }
    // A hole for the whole name, or in a string or comment, is not inside a name.
    assert!(compile_pattern("import $m").is_ok());
    assert!(compile_pattern("from $m import x").is_ok());
    assert!(compile_pattern("f(\"a.$m\")  # a.$m").is_ok());
}
