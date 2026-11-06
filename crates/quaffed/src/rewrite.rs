// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A structural `replace` in one Python file's text: each match written over with its rewrite.
//!
//! A rewrite is built from two sources only.  A hole writes back the source its name captured,
//! exactly as written; everything else comes from the template, exactly as written.  What is
//! derived rather than written: the indentation of a template's later lines and of captured code
//! moved to another depth, the file's own line ending for the template's line breaks, and a
//! trailing comma the match had before its own closing bracket.  An edit that would do anything
//! else - rewrite overlapping matches, drop a comment no capture writes back, change a literal's
//! structure by interpolating into it, or write a capture where Python would read it together
//! with what surrounds it - is refused, and nothing in the file is changed.

use std::ops::Range;

use quaffed_python::{Lexed, Rewrite, lex, parse_module, token_kinds};
use quaffed_syntax::{Captured, Pattern, find_captured, find_where_captured};

use crate::search::{Match, Position};
use crate::structural::{Found, Unparsed, Where, last_char, positions, unparsed};

/// What replacing a pattern in one file found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    /// Every match, in source order, as a search reports it.
    pub found: Vec<Found>,
    /// The file's new text, `None` if nothing matched; or why the edit is refused.
    pub outcome: Result<Option<String>, Refused>,
}

/// Why an edit to a file is refused: where, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// Where in the file.
    pub at: Position,
    /// Why, and what to change.
    pub why: String,
}

/// Every match of `pattern` in `text`, a Python file's decoded text, kept by `filter` where there
/// is one, each written over with `rewrite`.
///
/// # Errors
///
/// Returns [`Unparsed`] if the text is not Python; nothing is searched or replaced in it.
pub fn replace(
    text: &str,
    pattern: &Pattern,
    filter: Option<&Where>,
    rewrite: &Rewrite,
) -> Result<Replaced, Unparsed> {
    let arena = parse_module(text).map_err(|err| unparsed(text, err))?;
    let mut names: Vec<&str> = Vec::new();
    for site in rewrite.sites() {
        if !names.contains(&site.name.as_str()) {
            names.push(&site.name);
        }
    }
    let captured = match filter {
        None => find_captured(pattern, &arena, &names),
        Some(filter) => find_where_captured(
            pattern,
            &arena,
            &filter.names,
            &mut |nodes| (filter.holds)(&arena, nodes),
            &names,
        ),
    };
    let wanted: Vec<usize> = captured
        .iter()
        .flat_map(|m| [m.range.start, last_char(text, &m.range)])
        .collect();
    let placed = positions(text, &wanted);
    let found = captured
        .iter()
        .map(|m| Found {
            at: Match {
                start: placed[&m.range.start],
                end: placed[&last_char(text, &m.range)],
            },
            text: text[m.range.clone()].to_owned(),
        })
        .collect();
    let outcome = if captured.is_empty() {
        Ok(None)
    } else {
        let shape_arena = pattern.arena();
        let content: Vec<_> = shape_arena.children(shape_arena.root()).collect();
        let shape = match content.as_slice() {
            [only] => Some(shape_arena.kind(*only).name()),
            _ => None,
        };
        rewritten(text, &captured, &names, rewrite, shape).map(Some)
    };
    Ok(Replaced { found, outcome })
}

/// `text` with every match in `captured` written over, each capture in the order of `names`.
/// `shape` is the kind of node the pattern is, if it is one node.
fn rewritten(
    text: &str,
    captured: &[Captured],
    names: &[&str],
    rewrite: &Rewrite,
    shape: Option<&str>,
) -> Result<String, Refused> {
    let refused = |at: usize, why: String| Refused {
        at: positions(text, &[at])[&at],
        why,
    };
    for pair in captured.windows(2) {
        if pair[1].range.start < pair[0].range.end {
            let earlier = positions(text, &[pair[0].range.start])[&pair[0].range.start];
            return Err(refused(
                pair[1].range.start,
                format!(
                    "this match overlaps the one at line {}, column {}, so rewriting one would \
                     change or remove the other: narrow the pattern so that its matches cannot \
                     overlap",
                    earlier.line, earlier.column
                ),
            ));
        }
    }
    let lexed = lex(text);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    // Where each code site was written in `out`, with the match it belongs to and whether it is
    // a sequence: checked once the whole file is written.
    let mut written_sites: Vec<(Range<usize>, usize, bool)> = Vec::new();
    for found in captured {
        if let Some(comment) = lexed.comments.iter().find(|comment| {
            found.range.start <= comment.start
                && comment.end <= found.range.end
                && !found
                    .captures
                    .iter()
                    .any(|capture| capture.start <= comment.start && comment.end <= capture.end)
        }) {
            return Err(refused(
                comment.start,
                format!(
                    "the comment {:?} is in the match but in no capture the rewrite writes back, \
                     so the edit would drop it: capture it with a sequence hole, such as \
                     $args..., or narrow the pattern so that it does not match here",
                    &text[comment.clone()]
                ),
            ));
        }
        let (mut written, sites) = render(text, found, names, rewrite, &lexed)
            .map_err(|why| refused(found.range.start, why))?;
        keep_trailing_comma(text, found, &lexed, shape, &sites, &mut written);
        out.push_str(&text[copied..found.range.start]);
        let base = out.len();
        for (range, sequence) in sites {
            written_sites.push((
                base + range.start..base + range.end,
                found.range.start,
                sequence,
            ));
        }
        out.push_str(&written);
        copied = found.range.end;
    }
    out.push_str(&text[copied..]);
    // A capture written where Python reads it with what surrounds it - `x + 1` into `$a * 2` -
    // would change its meaning.  Each code site must still be code of its own: one node, or for
    // a sequence a run of them.  A file that no longer parses is left to the parse check.
    if let Ok(arena) = parse_module(&out) {
        for (range, match_start, sequence) in &written_sites {
            let whole = arena.nodes().any(|node| arena.range(node) == *range);
            let run = *sequence
                && arena
                    .nodes()
                    .any(|node| arena.range(node).start == range.start)
                && arena.nodes().any(|node| arena.range(node).end == range.end);
            if !whole && !run {
                return Err(refused(
                    *match_start,
                    format!(
                        "what a hole captured, {:?}, would no longer be code of its own where the \
                         rewrite writes it - Python would read it together with what stands \
                         around it - so the edit would change its meaning: put brackets round \
                         the hole in the rewrite, as in ($name)",
                        &out[range.clone()]
                    ),
                ));
            }
        }
    }
    Ok(out)
}

/// The rewrite of one match: the template indented, and its line breaks spelled, for where the
/// match stands, each site written with what its name captured; and where each code site was
/// written in it, with whether it is a sequence.  An interpolation that changes the token
/// structure of the literal it is written into is refused, with why.
fn render(
    text: &str,
    found: &Captured,
    names: &[&str],
    rewrite: &Rewrite,
    lexed: &Lexed,
) -> Result<(String, Vec<CodeSite>), String> {
    let template = rewrite.text();
    let stem = marker_stem(template, text);
    let marker = |i: usize| format!("{stem}{i}_");
    // The template with each site a marker that lexes as a name, so that its own strings and
    // indentation can be read, and each site found again once it is indented.
    let mut with_markers = String::with_capacity(template.len());
    let mut copied = 0;
    for (i, site) in rewrite.sites().iter().enumerate() {
        with_markers.push_str(&template[copied..site.written.start]);
        with_markers.push_str(&marker(i));
        copied = site.written.end;
    }
    with_markers.push_str(&template[copied..]);
    let indented = indent_template(
        &with_markers,
        &line_indent(text, found.range.start),
        line_ending(text, found.range.start),
        lexed,
    );
    // Each site's marker, in the order the indented template holds them.
    let mut at: Vec<(usize, usize)> = (0..rewrite.sites().len())
        .map(|i| {
            let place = indented
                .find(&marker(i))
                .expect("each marker is written once");
            (place, i)
        })
        .collect();
    at.sort_unstable();
    let capture = |name: &str| -> Range<usize> {
        let index = names
            .iter()
            .position(|n| *n == name)
            .expect("every site's name was asked for");
        found.captures[index].clone()
    };
    let fill = |filler: Option<&str>| -> (String, Vec<CodeSite>) {
        let mut out = String::with_capacity(indented.len());
        let mut code = Vec::new();
        let mut copied = 0;
        for &(place, i) in &at {
            let site = &rewrite.sites()[i];
            out.push_str(&indented[copied..place]);
            let range = capture(&site.name);
            let value = if site.in_literal {
                filler.map_or_else(|| text[range].to_owned(), str::to_owned)
            } else {
                // On the template's first line, the line is the file's, up to the match.
                let base = if out.contains(['\n', '\r']) {
                    line_indent(&out, out.len())
                } else {
                    line_indent(text, found.range.start)
                };
                moved(text, &range, &base, lexed)
            };
            if !site.in_literal {
                let sequence = site.written.len() > site.name.len() + 1
                    && rewrite.text()[site.written.clone()].ends_with("...");
                code.push((out.len()..out.len() + value.len(), sequence));
            }
            out.push_str(&value);
            copied = place + marker(i).len();
        }
        out.push_str(&indented[copied..]);
        (out, code)
    };
    let (written, code) = fill(None);
    if rewrite.sites().iter().any(|site| site.in_literal)
        && token_kinds(&written) != token_kinds(&fill(Some("x")).0)
    {
        let interpolated: Vec<String> = rewrite
            .sites()
            .iter()
            .filter(|site| site.in_literal)
            .map(|site| format!("${}", site.name))
            .collect();
        return Err(format!(
            "writing what {} captured here into the literal changes its structure - a value \
             closes the string, or opens an f-string field - so the edit would change more \
             than the literal's text: narrow the pattern so that such values do not match",
            interpolated.join(" and ")
        ));
    }
    Ok((written, code))
}

/// Where a code site was written in a rewrite, and whether it is a sequence.
type CodeSite = (Range<usize>, bool);

/// A stem for site markers that neither the template nor the file contains.
fn marker_stem(template: &str, text: &str) -> String {
    let mut stem = String::from("__quaff_site");
    while template.contains(&stem) || text.contains(&stem) {
        stem.push('_');
    }
    stem
}

/// `marked` with each line after the first indented for a match whose line is indented by
/// `base`, the line's own indentation kept relative to it in the file's indent unit, and each
/// line break spelled `ending`, the file's own.  A line that starts inside one of the template's
/// multi-line strings is part of its value and left alone, and so is the line break before it.
fn indent_template(marked: &str, base: &str, ending: &str, file: &Lexed) -> String {
    let own = lex(marked);
    let inside = |at: usize| {
        own.multi_line_strings
            .iter()
            .any(|s| s.start < at && at < s.end)
    };
    let mut out = String::with_capacity(marked.len() + base.len() * 4);
    for (i, (start, line)) in lines_with_starts(marked).into_iter().enumerate() {
        let body = line.trim_end_matches(['\n', '\r']);
        let break_after = &line[body.len()..];
        let in_string = inside(start);
        let rest = body.trim_start_matches([' ', '\t']);
        if i == 0 || in_string || rest.is_empty() {
            out.push_str(body);
        } else {
            out.push_str(base);
            out.push_str(&in_unit(
                &body[..body.len() - rest.len()],
                own.indent_unit.as_deref(),
                file.indent_unit.as_deref(),
            ));
            out.push_str(rest);
        }
        if !break_after.is_empty() {
            // A break inside a string is part of its value.
            out.push_str(if inside(start + body.len()) {
                break_after
            } else {
                ending
            });
        }
    }
    out
}

/// The file's own line ending where a match at `offset` stands, as `$^` writes it in a textual
/// replace: the one ending the match's line, the last before it where that line has none, or LF.
fn line_ending(text: &str, offset: usize) -> &'static str {
    let ending_at = |i: usize| -> &'static str {
        match text.as_bytes()[i] {
            b'\r' if text.as_bytes().get(i + 1) == Some(&b'\n') => "\r\n",
            b'\r' => "\r",
            _ => "\n",
        }
    };
    if let Some(i) = text[offset..].find(['\n', '\r']) {
        return ending_at(offset + i);
    }
    match text[..offset].rfind(['\n', '\r']) {
        // The LF of a CRLF is found last; the ending it closes starts at its CR.
        Some(i) if text.as_bytes()[i] == b'\n' && i > 0 && text.as_bytes()[i - 1] == b'\r' => {
            "\r\n"
        }
        Some(i) => ending_at(i),
        None => "\n",
    }
}

/// `leading`, a template line's indentation in the template's unit `from`, written in the file's
/// unit `to`: each whole level converted, anything left over kept.  Left as it is when either
/// unit is unknown.
fn in_unit(leading: &str, from: Option<&str>, to: Option<&str>) -> String {
    let (Some(from), Some(to)) = (from, to) else {
        return leading.to_owned();
    };
    let mut levels = 0;
    let mut rest = leading;
    while let Some(after) = rest.strip_prefix(from) {
        levels += 1;
        rest = after;
    }
    format!("{}{rest}", to.repeat(levels))
}

/// The code `range` of `text` captured, moved to a line indented by `base`: each later line
/// that is indented at least as far as the line the capture starts on is shifted by the change,
/// unless it starts inside a multi-line string, whose value it is part of.
fn moved(text: &str, range: &Range<usize>, base: &str, lexed: &Lexed) -> String {
    let value = &text[range.clone()];
    if !value.contains(['\n', '\r']) {
        return value.to_owned();
    }
    let old = line_indent(text, range.start);
    let mut out = String::with_capacity(value.len());
    for (i, (start, line)) in lines_with_starts(value).into_iter().enumerate() {
        let at = range.start + start;
        let in_string = lexed
            .multi_line_strings
            .iter()
            .any(|s| s.start < at && at < s.end);
        match line.strip_prefix(old.as_str()) {
            Some(rest) if i > 0 && !in_string => {
                out.push_str(base);
                out.push_str(rest);
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// The kinds of node whose own closing bracket a trailing comma can stand before without
/// changing what they mean: a call's arguments and a list, set or dict display.  Not a tuple,
/// whose brackets are layout, nor a subscript, where the comma makes a tuple of the index.
const COMMA_KEEPING: &[&str] = &["ExprCall", "ExprList", "ExprSet", "ExprDict"];

/// Keeps the trailing comma the match had before its own closing bracket, where the rewrite is
/// the same kind of node, ends with its own bracket rather than a capture's, and does not say
/// otherwise: a formatter reads that comma as asking for the bracket's contents one to a line,
/// so losing it would change more than was asked.  The comma is only ever added where the
/// result still parses as that kind of node.
fn keep_trailing_comma(
    text: &str,
    found: &Captured,
    lexed: &Lexed,
    shape: Option<&str>,
    sites: &[CodeSite],
    written: &mut String,
) {
    let Some(shape) = shape.filter(|shape| COMMA_KEEPING.contains(shape)) else {
        return;
    };
    let range = &found.range;
    let close_at = range.end - 1;
    // The match's closing bracket is its own, not a capture's.
    if found
        .captures
        .iter()
        .any(|capture| capture.start <= close_at && close_at < capture.end)
    {
        return;
    }
    let before_close = back_over_layout(text, range.start, close_at, &lexed.comments);
    if !text[range.start..before_close].ends_with(',') {
        return;
    }
    // The rewrite ends with its own bracket: the same kind of node, ending where it does, not
    // with what a capture wrote back.
    let trimmed = written.trim_end().len();
    if trimmed == 0 || sites.iter().any(|(site, _)| site.end >= trimmed) {
        return;
    }
    let ends_as_shape = |code: &str| {
        quaffed_python::parse_expression(code.trim()).is_ok_and(|arena| {
            let content: Vec<_> = arena.children(arena.root()).collect();
            matches!(content.as_slice(), [only] if arena.kind(*only).name() == shape
                && arena.range(*only).end == code.trim().len())
        })
    };
    if !ends_as_shape(written) {
        return;
    }
    let own = lex(written);
    let at = back_over_layout(written, 0, trimmed - 1, &own.comments);
    let open = match &written[trimmed - 1..trimmed] {
        ")" => "(",
        "]" => "[",
        _ => "{",
    };
    if written[..at].ends_with(',') || written[..at].ends_with(open) {
        return;
    }
    let mut kept = written.clone();
    kept.insert(at, ',');
    if ends_as_shape(&kept) {
        *written = kept;
    }
}

/// Back from `at` in `text`, no further than `floor`, over whitespace and comments: where the
/// code before them ends.
fn back_over_layout(text: &str, floor: usize, at: usize, comments: &[Range<usize>]) -> usize {
    let mut at = at;
    loop {
        at = floor + text[floor..at].trim_end().len();
        match comments.iter().find(|comment| comment.end == at) {
            Some(comment) => at = comment.start,
            None => return at,
        }
    }
}

/// The whitespace the line holding `offset` starts with.
fn line_indent(text: &str, offset: usize) -> String {
    let start = text[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    text[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// `text` split into lines with each one's offset, every line keeping its own line ending.
fn lines_with_starts(text: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let end = match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => Some(i + 2),
            b'\r' | b'\n' => Some(i + 1),
            _ => None,
        };
        if let Some(end) = end {
            lines.push((start, &text[start..end]));
            start = end;
            i = end;
        } else {
            i += 1;
        }
    }
    if start < text.len() {
        lines.push((start, &text[start..]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use quaffed_python::{compile_pattern, compile_rewrite};

    /// `text` with `pattern` replaced by `with`, or why it was refused.
    fn replaced(pattern: &str, with: &str, text: &str) -> Result<Option<String>, String> {
        let pattern = compile_pattern(pattern).expect("the pattern compiles");
        let rewrite = compile_rewrite(with, &pattern).expect("the rewrite compiles");
        replace(text, &pattern, None, &rewrite)
            .expect("the text parses")
            .outcome
            .map_err(|refused| {
                format!("{}:{}: {}", refused.at.line, refused.at.column, refused.why)
            })
    }

    fn ok(pattern: &str, with: &str, text: &str) -> String {
        replaced(pattern, with, text)
            .unwrap_or_else(|why| panic!("{why}"))
            .expect("something matched")
    }

    #[test]
    fn captures_are_written_back_and_the_rest_comes_from_the_template() {
        assert_eq!(
            ok("f($a, $b)", "g($b, $a)", "x = f(1, two)\n"),
            "x = g(two, 1)\n"
        );
    }

    #[test]
    fn nothing_matched_changes_nothing() {
        assert_eq!(replaced("f($a)", "g($a)", "h(1)\n"), Ok(None));
    }

    #[test]
    fn a_sequence_writes_back_its_stretch_with_what_stands_between() {
        assert_eq!(
            ok(
                "log($msg, $args...)",
                "logger.info($msg, $args...)",
                "log(\"x=%s y=%s\", x,  # the old one\n    y)\n"
            ),
            "logger.info(\"x=%s y=%s\", x,  # the old one\n    y)\n"
        );
    }

    #[test]
    fn a_name_interpolates_into_a_string_and_a_comment() {
        assert_eq!(
            ok(
                "def $f(): $body...",
                "def $f():  # was $f\n    \"\"\"TODO: document $f.\"\"\"\n    $body...",
                "def go():\n    run()\n"
            ),
            "def go():  # was go\n    \"\"\"TODO: document go.\"\"\"\n    run()\n"
        );
    }

    #[test]
    fn a_later_template_line_takes_the_matchs_indentation_in_the_files_unit() {
        // The file indents by two; the template by four.
        let text = "if x:\n  f(a)\n";
        assert_eq!(
            ok("f($a)", "if $a:\n    g()", text),
            "if x:\n  if a:\n    g()\n"
        );
    }

    #[test]
    fn captured_code_moved_to_another_depth_is_re_indented() {
        let text = "if c:\n    with x:\n        a()\n        b()\n";
        // Unwrapping the `with` moves its body one level out.
        assert_eq!(
            ok("with $m:\n    $body...", "$body...", text),
            "if c:\n    a()\n    b()\n"
        );
    }

    #[test]
    fn a_line_inside_a_multi_line_string_is_never_re_indented() {
        let text = "if c:\n    with x:\n        s = '''a\n  b'''\n        t()\n";
        assert_eq!(
            ok("with $m:\n    $body...", "$body...", text),
            "if c:\n    s = '''a\n  b'''\n    t()\n"
        );
    }

    #[test]
    fn a_trailing_comma_inside_the_match_is_kept() {
        assert_eq!(
            ok("f($a, $b)", "g($a, $b)", "f(\n    x,\n    y,\n)\n"),
            "g(x, y,)\n"
        );
        // And left alone where the rewrite already has one, or where there was none.
        assert_eq!(ok("f($a, $b)", "g($a, $b,)", "f(x, y,)\n"), "g(x, y,)\n");
        assert_eq!(ok("f($a, $b)", "g($a, $b)", "f(x, y)\n"), "g(x, y)\n");
    }

    #[test]
    fn a_trailing_comma_is_kept_only_before_the_same_kind_of_brackets_own() {
        let split = "if isinstance(\n    value,\n    int,\n):\n    pass\n";
        // A parenthesised expression is not an argument list: a comma would make a tuple.
        assert_eq!(
            ok("isinstance($x, $t)", "(type($x) is $t)", split),
            "if (type(value) is int):\n    pass\n"
        );
        assert_eq!(
            ok(
                "max($a, $b)",
                "($a if $a > $b else $b)",
                "m = max(\n    x,\n    y,\n)\n"
            ),
            "m = (x if x > y else y)\n"
        );
        // A subscript's comma makes a tuple of the index.
        assert_eq!(
            ok(
                "grid[$i, $j]",
                "grid[$i][$j]",
                "v = grid[\n    i,\n    j,\n]\n"
            ),
            "v = grid[i][j]\n"
        );
        // A bracket a capture wrote back is the capture's, written back as it was.
        assert_eq!(ok("[$a, $b]", "$b", "v = [x, d[k],]\n"), "v = d[k]\n");
        assert_eq!(
            ok(
                "return $x",
                "return wrap($x)",
                "def f():\n    return foo(a, b,)\n"
            ),
            "def f():\n    return wrap(foo(a, b,))\n"
        );
        // Before a comment the template ends its last line with, not after it.
        assert_eq!(
            ok("f($a)", "g(\n    $a  # last\n)", "f(\n    x,\n)\n"),
            "g(\n    x,  # last\n)\n"
        );
    }

    #[test]
    fn a_capture_python_would_read_with_its_surroundings_is_refused() {
        let why = replaced("f($a)", "$a * 2", "y = f(x + 1)\n").expect_err("a changed meaning");
        assert!(
            why.contains("\"x + 1\", would no longer be code of its own"),
            "{why}"
        );
        // Brackets in the rewrite keep it whole.
        assert_eq!(
            ok("f($a)", "($a) * 2", "y = f(x + 1)\n"),
            "y = (x + 1) * 2\n"
        );
        // A tuple's own brackets are not part of it, so it would become two arguments.
        assert!(
            replaced(
                "return $x",
                "return wrap($x)",
                "def f():\n    return a, b\n"
            )
            .is_err()
        );
        // A sequence is a run of code, so it may stand as several.
        assert_eq!(ok("f($xs...)", "g($xs...)", "f(a, b)\n"), "g(a, b)\n");
    }

    #[test]
    fn a_rewrites_line_breaks_are_the_files_own() {
        assert_eq!(
            ok("f($a)", "if $a:\n    g()", "if x:\r\n    f(a)\r\n"),
            "if x:\r\n    if a:\r\n        g()\r\n"
        );
        // A break inside one of the template's strings is part of its value.
        assert_eq!(
            ok("f($a)", "g('''x\ny''', $a)", "f(a)\r\n"),
            "g('''x\ny''', a)\r\n"
        );
    }

    #[test]
    fn overlapping_matches_are_refused_naming_both() {
        let why = replaced("f($x)", "g($x)", "f(f(1))\n").expect_err("overlapping matches");
        assert!(
            why.starts_with("1:3: this match overlaps the one at line 1, column 1"),
            "{why}"
        );
        // Runs of statements tried from every start overlap without one being inside the other.
        let why = replaced("a()\na()", "b()", "a()\na()\na()\n").expect_err("overlapping runs");
        assert!(
            why.starts_with("2:1: this match overlaps the one at line 1, column 1"),
            "{why}"
        );
    }

    #[test]
    fn a_comment_no_capture_writes_back_is_refused() {
        let why = replaced("f($x, $y)", "g($x, $y)", "f(a,  # why\n  b)\n")
            .expect_err("a dropped comment");
        assert!(why.starts_with("1:7: the comment \"# why\""), "{why}");
        // In a capture that is not written back, it would be dropped too.
        assert!(replaced("f($x, $y)", "g($x)", "f(a, [b,  # why\n  c])\n").is_err());
    }

    #[test]
    fn an_ascii_value_interpolates_into_bytes() {
        assert_eq!(ok("f($v)", "b\"key-$v\"", "f(ab)\n"), "b\"key-ab\"\n");
    }

    #[test]
    fn a_value_spanning_lines_in_a_one_line_string_is_refused() {
        let why =
            replaced("f($v)", "\"pre $v\"", "f([1,\n   2])\n").expect_err("a value spanning lines");
        assert!(why.contains("changes its structure"), "{why}");
    }

    #[test]
    fn an_interpolation_that_changes_a_literals_structure_is_refused() {
        // Into an f-string, a value with braces would open a field.
        let why = replaced("f($v)", "f\"value: $v\"", "f({y})\n").expect_err("a field opened");
        assert!(why.contains("changes its structure"), "{why}");
        // A value with the string's quote would close it.
        assert!(replaced("f($v)", "\"pre $v\"", "f(\"\" + y + \"\")\n").is_err());
        // One with the other quote is only text.
        assert_eq!(ok("f($v)", "\"pre $v\"", "f('abc')\n"), "\"pre 'abc'\"\n");
    }
}
