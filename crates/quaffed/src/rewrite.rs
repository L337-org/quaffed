// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A structural `replace` in one Python file's text: each match written over with its rewrite.
//!
//! A rewrite is built from two sources only.  A hole writes back the source its name captured,
//! with the brackets that only wrap it; everything else comes from the template, exactly as
//! written.  What is derived rather than written: the indentation of a template's later lines and
//! of captured statements moved to another depth, the file's own line ending for the template's
//! line breaks, and the layout a pattern ignores - trailing commas, comments, and line breaks
//! inside brackets, which survive the edit or refuse it.  A match inside another's hole is
//! rewritten first, in levels; matches that overlap otherwise are refused.  A value written into
//! a string is escaped so that the string holds it as text, and questioned; one the literal
//! cannot hold is refused.  A refused edit changes nothing in the file.
//!
//! A capture written where Python reads it together with what surrounds it - `x + 1` into
//! `$a * 2` - is refused: the code would mean something other than the template says, and adding
//! brackets would write something other than the template, so the message shows where brackets
//! round the hole make the template mean what it says.
//!
//! Statements written where the code around them shares their line - after a semicolon, or in a
//! block on its header's line - are given lines of their own where they need them, as
//! [`lines`] lays them out, so that each stays in the block it was in; an outcome that leaves one
//! in another block is refused.  So is an edit that leaves a `return`, `yield`, `await`, `break`
//! or `continue` outside the construct it needs, which Python parses but cannot compile.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use quaffed_python::{
    Bracket, Lexed, ParseError, Rewrite, code_tokens, lex, misplaced, parse_module, token_kinds,
};
use quaffed_syntax::{Arena, Captured, Kind, NodeId, Pattern, find_captured, find_where_captured};

use crate::search::{Match, Position};

mod layout;
mod lines;

use crate::structural::{Found, Unparsed, Where, last_char, positions, unparsed};
use layout::{After, Before, WrittenSite};
use lines::{LEFT_BLOCK, Needs, Placed, Slot};

/// What replacing a pattern in one file found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    /// Every match, in source order, as a search reports it.
    pub found: Vec<Found>,
    /// The file's new text, `None` if nothing matched; or why the edit is refused.
    pub outcome: Result<Option<Edited>, Refused>,
}

/// A file's text once every match is rewritten, and what in it may not mean what was meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    /// The new text.
    pub text: String,
    /// For each stretch of the text as it was that the rewrite wrote over, in order, where it was
    /// and where what was written over it stands in [`Edited::text`].  A match nested in another
    /// is inside the other's stretch.  Taken as a claim, to be checked, not as certain.
    pub over: Vec<(Range<usize>, Range<usize>)>,
    /// Each place a value was escaped to stay text in the string the rewrite writes it into:
    /// written as asked, but questioned, in case the code was meant to run.
    pub questioned: Vec<Questioned>,
}

/// A place an edit was made as written but may not mean what was meant, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Questioned {
    /// Where in the file as it was.
    pub at: Position,
    /// Why, and what to change if it is not what was meant.
    pub why: String,
}

/// Why an edit to a file is refused: where, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// Where in the file.
    pub at: Position,
    /// Why, and what to change.
    pub why: String,
}

/// What an unnamed hole is called once [`name_every_hole`] names it: `#` cannot start a name a
/// script spells, so it cannot meet one.
const UNNAMED: &str = "#";

/// `pattern` with each unnamed hole, `$_` or `$...`, given a name of its own that no script can
/// spell, so that what it matched is captured as a named hole's is.  A rewrite cannot write it
/// back, but it still needs to know where it was: a bracket inside what `$_` matched is the
/// hole's, dropped with it, not one the pattern spelled.  Each name is used once, so it compares
/// nothing and matches exactly what the unnamed hole did.
#[must_use]
pub fn name_every_hole(pattern: &Pattern) -> Pattern {
    let holes = pattern
        .holes()
        .enumerate()
        .map(|(i, (node, hole))| {
            let mut hole = hole.clone();
            hole.name.get_or_insert_with(|| format!("{UNNAMED}{i}"));
            (node, hole)
        })
        .collect();
    Pattern::new(pattern.arena().clone(), holes)
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
    // Every name the pattern binds is captured, not only those the rewrite writes: what a hole
    // matched is the hole's, written back or dropped as the rewrite says, and only what stands
    // outside every hole is the pattern's own.  An unnamed hole is captured only once
    // [`name_every_hole`] has named it.
    let mut names: Vec<&str> = Vec::new();
    let pattern_names = pattern.holes().filter_map(|(_, hole)| hole.name.as_deref());
    for name in rewrite
        .sites()
        .iter()
        .map(|site| site.name.as_str())
        .chain(pattern_names)
    {
        if !names.contains(&name) {
            names.push(name);
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
            range: m.range.clone(),
        })
        .collect();
    let outcome = if captured.is_empty() {
        Ok(None)
    } else {
        nested(text, &arena, &captured, &names, rewrite).map(Some)
    };
    Ok(Replaced { found, outcome })
}

/// `text`, parsed as `arena`, with every match in `captured` written over, each capture in the
/// order of `names`.  A match inside what another's hole captured is rewritten first, so that
/// the hole writes back the rewritten code: the matches are rewritten in levels, the most deeply
/// nested first, each level an ordinary rewrite of the text the one before left.  Matches that
/// overlap without one standing inside the other's capture cannot both be rewritten, and refuse
/// the edit.  Every place a refusal or a question names is where it was in `text`.
fn nested(
    text: &str,
    arena: &Arena<'_>,
    captured: &[Captured],
    names: &[&str],
    rewrite: &Rewrite,
) -> Result<Edited, Refused> {
    let at = |offset: usize| positions(text, &[offset])[&offset];
    // Lexed once for the first level, which widening the captures reads too.
    let first_lexed = lex(text);
    let captured = &whole_brackets(text, arena, &first_lexed, captured);
    let depths = depths(captured).map_err(|(later, earlier)| {
        let earlier = at(captured[earlier].range.start);
        Refused {
            at: at(captured[later].range.start),
            why: format!(
                "this match overlaps the one at line {}, column {}, though it stands in none of \
                 that one's holes, so rewriting one would change or remove the other: narrow the \
                 pattern so that its matches cannot overlap",
                earlier.line, earlier.column
            ),
        }
    })?;
    let deepest = depths.iter().copied().max().unwrap_or(0);
    let mut current = text.to_owned();
    let mut maps: Vec<Vec<(Range<usize>, Range<usize>)>> = Vec::new();
    let mut questioned = Vec::new();
    for depth in (0..=deepest).rev() {
        let level: Vec<Captured> = captured
            .iter()
            .zip(&depths)
            .filter(|(_, d)| **d == depth)
            .map(|(found, _)| Captured {
                range: forward(&maps, found.range.start)..forward(&maps, found.range.end),
                captures: found
                    .captures
                    .iter()
                    .map(|c| forward(&maps, c.start)..forward(&maps, c.end))
                    .collect(),
            })
            .collect::<Vec<_>>();
        let mut level = level;
        level.sort_by_key(|found| found.range.start);
        let parsed;
        let level_arena = if maps.is_empty() {
            arena
        } else {
            match parse_module(&current) {
                Ok(arena) => {
                    parsed = arena;
                    &parsed
                }
                Err(err) => {
                    // Named where the parse failed, mapped back to the file as it was, since the
                    // text between levels is never seen.
                    let (offset, message) = match err {
                        ParseError::Invalid { message, offset } => (offset, message),
                        ParseError::TooLong(err) => (level[0].range.start, err.to_string()),
                    };
                    return Err(Refused {
                        at: at(backward(&maps, offset)),
                        why: format!(
                            "once the matches inside another match here are rewritten, the code \
                             no longer parses, so the outer one cannot be rewritten round them: \
                             {message}"
                        ),
                    });
                }
            }
        };
        let later_lexed;
        let level_lexed = if maps.is_empty() {
            &first_lexed
        } else {
            later_lexed = lex(&current);
            &later_lexed
        };
        let done = rewritten(&current, level_arena, level_lexed, &level, names, rewrite).map_err(
            |(offset, why)| Refused {
                at: at(backward(&maps, offset)),
                why,
            },
        )?;
        questioned.extend(done.questioned.into_iter().map(|(offset, why)| Questioned {
            at: at(backward(&maps, offset)),
            why,
        }));
        maps.push(done.edits);
        current = done.text;
    }
    questioned.sort_by_key(|q| (q.at.line, q.at.column));
    let over = outermost_over(&maps);
    if let Some((offset, why)) = left_outside(text, &current, &over) {
        return Err(Refused {
            at: at(offset),
            why,
        });
    }
    Ok(Edited {
        text: current,
        over,
        questioned,
    })
}

/// What the levels in `maps` wrote over in the text as it was.  The outermost level's matches
/// hold every deeper one, so its stretches are all that changed, each mapped back through the
/// levels before it.
fn outermost_over(maps: &[Vec<(Range<usize>, Range<usize>)>]) -> Vec<(Range<usize>, Range<usize>)> {
    let (last, before) = maps.split_last().expect("a match makes at least one level");
    last.iter()
        .map(|(old, new)| {
            (
                backward(before, old.start)..backward(before, old.end),
                new.clone(),
            )
        })
        .collect()
}

/// Why an edit of `old` into `new` is refused for the first statement or expression it leaves
/// outside the construct it needs - a `return` outside a function, a `break` outside a loop -
/// with where the match that wrote it starts in `old`; or `None`.  Each parses anywhere, so
/// neither the parse check nor the matcher sees it.  Only what the edit wrote, the new side of
/// each of `over`, is judged, and only beyond as many of the same kind as stood outside in what
/// it wrote over: one already misplaced and carried through a hole is the file's, not the
/// edit's.  A new text that does not parse is left to the parse check.
fn left_outside(
    old: &str,
    new: &str,
    over: &[(Range<usize>, Range<usize>)],
) -> Option<(usize, String)> {
    const KEYWORDS: [&str; 6] = ["return", "yield", "await", "async", "break", "continue"];
    if !over
        .iter()
        .any(|(_, written)| KEYWORDS.iter().any(|k| new[written.clone()].contains(k)))
    {
        return None;
    }
    // Each found in a stretch the edit wrote over, on its new side or its old, with where that
    // stretch starts in `old`.
    let inside = |found: &[quaffed_python::Misplaced], new_side: bool| {
        found
            .iter()
            .filter_map(|m| {
                over.iter()
                    .find(|(was, now)| if new_side { now } else { was }.contains(&m.offset))
                    .map(|(was, _)| (was.start, m.what))
            })
            .collect::<Vec<_>>()
    };
    let now = inside(&misplaced(new).ok()?, true);
    if now.is_empty() {
        return None;
    }
    let was = inside(&misplaced(old).ok()?, false);
    let count = |found: &[(usize, &str)], what: &str| found.iter().filter(|f| f.1 == what).count();
    let &(offset, what) = now
        .iter()
        .find(|(_, what)| count(&now, what) > count(&was, what))?;
    let needs = match what {
        "return" | "yield" | "yield from" => "a function",
        "await" | "async with" | "async for" => "an async function",
        _ => "a loop",
    };
    Some((
        offset,
        format!(
            "the rewrite would leave `{what}` outside {needs}, which Python parses but refuses \
             to run, so the file would no longer compile: keep `{what}` inside {needs} in the \
             rewrite, or narrow the pattern so that it does not match here"
        ),
    ))
}

/// `captured` with each capture widened to the whole of the code it stands for.  A node's range
/// leaves out the brackets that only wrap it, which the matcher ignores, so `$y` over
/// `(a +\n b)` would capture `a +\n b` and write it back without them, and a sequence hole,
/// from its first node's start to its last's end, would cut such brackets in half.  So each
/// capture takes in any bracket it holds one end of, and then any brackets that wrap it with
/// nothing but layout between: what the pattern ignored survives with what it wraps.  A bracket
/// some node spans - a call's arguments, a tuple, a list - is that node's, never only a
/// wrapping.
fn whole_brackets(
    text: &str,
    arena: &Arena<'_>,
    lexed: &Lexed,
    captured: &[Captured],
) -> Vec<Captured> {
    let wrapping = Wrapping::new(text, arena, lexed);
    captured
        .iter()
        .map(|found| Captured {
            range: found.range.clone(),
            captures: found
                .captures
                .iter()
                .map(|capture| wrapping.widened(&found.range, capture))
                .collect(),
        })
        .collect()
}

/// What widens a stretch of code to the whole of what it stands for: the brackets it holds one
/// end of, and those that only wrap it.
struct Wrapping<'a> {
    text: &'a str,
    lexed: &'a Lexed,
    /// Every node's range, as `(start, end)`: a bracket some node spans is that node's.
    nodes: HashSet<(usize, usize)>,
    /// For each bracket, by its place in `lexed.brackets`, the one it stands directly inside.
    outer: Vec<Option<usize>>,
}

impl<'a> Wrapping<'a> {
    fn new(text: &'a str, arena: &Arena<'_>, lexed: &'a Lexed) -> Self {
        let nodes = arena
            .nodes()
            .map(|node| {
                let range = arena.range(node);
                (range.start, range.end)
            })
            .collect();
        // Brackets nest, and are in the order they open, so the one each stands inside is the
        // nearest before it still open.
        let mut outer = Vec::with_capacity(lexed.brackets.len());
        let mut open: Vec<usize> = Vec::new();
        for (index, bracket) in lexed.brackets.iter().enumerate() {
            while open
                .last()
                .is_some_and(|&last| lexed.brackets[last].close < bracket.open)
            {
                open.pop();
            }
            outer.push(open.last().copied());
            open.push(index);
        }
        Wrapping {
            text,
            lexed,
            nodes,
            outer,
        }
    }

    /// The brackets that hold `at`, from either end inclusive, innermost first: found from the
    /// last to open at or before it, through the brackets each stands inside, so that only
    /// those around `at` are looked at.
    fn around(&self, at: usize) -> impl Iterator<Item = &Bracket> {
        let brackets = &self.lexed.brackets;
        let last = brackets.partition_point(|b| b.open <= at).checked_sub(1);
        std::iter::successors(last, |&index| self.outer[index])
            .map(|index| &brackets[index])
            .filter(move |b| at <= b.close)
    }

    /// Whether `range` holds only whitespace and comments.
    fn layout(&self, range: Range<usize>) -> bool {
        let mut at = range.start;
        loop {
            let rest = &self.text[at..range.end];
            at += rest.len() - rest.trim_start().len();
            let comments = &self.lexed.comments;
            let index = comments.partition_point(|c| c.start < at);
            match comments
                .get(index)
                .filter(|c| c.start == at && c.end <= range.end)
            {
                Some(comment) => at = comment.end,
                None => return at == range.end,
            }
        }
    }

    /// `capture` taking in any bracket inside `within` it holds one end of, and then any that
    /// wrap it with nothing but layout between.
    fn widened(&self, within: &Range<usize>, capture: &Range<usize>) -> Range<usize> {
        let mut capture = capture.clone();
        if capture.is_empty() {
            return capture;
        }
        let wrapping = |b: &&Bracket| {
            self.text.as_bytes()[b.open] == b'(' && !self.nodes.contains(&(b.open, b.close + 1))
        };
        loop {
            let inside = |b: &Bracket| within.start <= b.open && b.close < within.end;
            // A bracket with one end in the capture, or wrapping it, holds its first or its last
            // byte, so only the brackets around those two are looked at, in the order they open.
            let mut near: Vec<&Bracket> = self
                .around(capture.start)
                .chain(self.around(capture.end - 1))
                .collect();
            near.sort_unstable_by_key(|b| b.open);
            near.dedup_by_key(|b| b.open);
            let cut = near
                .iter()
                .copied()
                .find(|b| inside(b) && capture.contains(&b.open) != capture.contains(&b.close));
            let wraps = near.iter().copied().filter(wrapping).find(|b| {
                inside(b)
                    && b.open < capture.start
                    && capture.end <= b.close
                    && self.layout(b.open + 1..capture.start)
                    && self.layout(capture.end..b.close)
            });
            match cut.or(wraps) {
                Some(b) => capture = capture.start.min(b.open)..capture.end.max(b.close + 1),
                None => return capture,
            }
        }
    }
}

/// How deeply each match stands inside the others' captures, or the two matches, later and
/// earlier, that overlap without one standing inside a capture of the other.
fn depths(captured: &[Captured]) -> Result<Vec<usize>, (usize, usize)> {
    let mut depths = vec![0; captured.len()];
    // Outermost first where matches start together, as `f(x)` and `f(x).g()` can.
    let mut order: Vec<usize> = (0..captured.len()).collect();
    order.sort_by_key(|&i| {
        (
            captured[i].range.start,
            std::cmp::Reverse(captured[i].range.end),
        )
    });
    // The matches still open where each one starts, outermost first.
    let mut open: Vec<usize> = Vec::new();
    for j in order {
        let found = &captured[j];
        while open
            .last()
            .is_some_and(|&i| captured[i].range.end <= found.range.start)
        {
            open.pop();
        }
        if let Some(&i) = open.last() {
            let held = captured[i]
                .captures
                .iter()
                .any(|c| c.start <= found.range.start && found.range.end <= c.end);
            if !held {
                return Err((j, i));
            }
            depths[j] = open.len();
        }
        open.push(j);
    }
    Ok(depths)
}

/// Where `offset` of the text before the levels in `maps` stands after them.  Each level maps
/// what it rewrote, an old range, to what it wrote, a new one; an offset at or after an old
/// range's end moves with the text after it.
fn forward(maps: &[Vec<(Range<usize>, Range<usize>)>], offset: usize) -> usize {
    maps.iter().fold(offset, |offset, level| {
        let mut moved = offset;
        for (old, new) in level {
            if old.end <= offset {
                moved = moved + new.len() - old.len();
            }
        }
        moved
    })
}

/// Where `offset` of the text after the levels in `maps` stood before them: inside what a level
/// wrote, where that match started.
fn backward(maps: &[Vec<(Range<usize>, Range<usize>)>], offset: usize) -> usize {
    maps.iter().rev().fold(offset, |offset, level| {
        let mut moved = offset;
        for (old, new) in level {
            if new.start <= offset && offset < new.end {
                return old.start;
            }
            if new.end <= offset {
                moved = moved + old.len() - new.len();
            }
        }
        moved
    })
}

/// One level's rewrite: the new text; each match's old range in the text it was given and the
/// range it was written to in the new; and what was questioned, at offsets of the old text.
struct Level {
    text: String,
    edits: Vec<(Range<usize>, Range<usize>)>,
    questioned: Vec<(usize, String)>,
}

/// `text`, parsed as `arena`, with every match in `captured`, none overlapping, written over,
/// each capture in the order of `names`; or why not, at an offset of `text`.
fn rewritten(
    text: &str,
    arena: &Arena<'_>,
    lexed: &Lexed,
    captured: &[Captured],
    names: &[&str],
    rewrite: &Rewrite,
) -> Result<Level, (usize, String)> {
    if !rewrite.has_code() {
        return deleted(text, arena, lexed, captured, names, rewrite);
    }
    let before = Before {
        text,
        arena,
        lexed,
        captured,
        names,
    };
    // A comment at the edge of one of the match's own brackets is carried with it, or refused,
    // once the rewrite is written; any other the rewrite does not write back is refused here.
    let edges = layout::edge_comments(&before);
    let Writing {
        out,
        written,
        written_sites,
        escapes,
        laid,
        slots,
    } = write_matches(&before, rewrite, &edges)?;
    let edits = |now: &[Range<usize>]| level_edits(captured, &written, now, &laid);
    // A file that no longer parses is left to the parse check, which refuses it.
    let Ok(out_arena) = parse_module(&out) else {
        return Ok(Level {
            edits: edits(&written),
            text: out,
            questioned: escapes,
        });
    };
    left_block(captured, &slots, &written, &edits(&written), &out_arena)?;
    let after = After {
        text: &out,
        arena: &out_arena,
        written: &written,
        sites: &written_sites,
    };
    let carried = layout::carry(&before, &after)?;
    let carried_arena;
    let (text, arena, sites, written) = if let Some(carried) = &carried {
        carried_arena =
            parse_module(&carried.text).expect("layout is carried only where it parses");
        (
            carried.text.as_str(),
            &carried_arena,
            carried.sites.as_slice(),
            carried.written.as_slice(),
        )
    } else {
        (
            out.as_str(),
            &out_arena,
            written_sites.as_slice(),
            written.as_slice(),
        )
    };
    if let Some(refused) = regrouped(&before, (text, arena), sites, rewrite) {
        return Err(refused);
    }
    let edits = edits(written);
    Ok(Level {
        questioned: escapes,
        edits,
        text: carried.map_or(out, |carried| carried.text),
    })
}

/// The file once every match is written over, as [`write_matches`] writes it.
struct Writing {
    out: String,
    /// Where each match's rewrite was written in `out`.
    written: Vec<Range<usize>>,
    /// Where each code site in each rewrite was written in `out`.
    written_sites: Vec<WrittenSite>,
    /// Values escaped to stay text in a string, at their match's start, to be questioned.
    escapes: Vec<(usize, String)>,
    /// Where each change laid out round the matches was made: its old range and its new.
    laid: Vec<(Range<usize>, Range<usize>)>,
    /// Where each match shares its line with other code, as [`laid_out`] found.
    slots: Vec<Option<Slot>>,
}

/// The text of `before` with each match written over with its rendering of `rewrite`, and the
/// text round them laid out as [`laid_out`] decides; or why not, at an offset of the text.  A
/// comment in `edges`, at the edge of one of the match's own brackets, is carried with it once
/// the whole file is written; any other the rewrite does not write back is refused here.
fn write_matches(
    before: &Before<'_>,
    rewrite: &Rewrite,
    edges: &[Range<usize>],
) -> Result<Writing, (usize, String)> {
    let Before {
        text,
        arena,
        lexed,
        captured,
        names,
    } = *before;
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    // Where each match's rewrite, and each code site in it, was written in `out`: checked once
    // the whole file is written.
    let mut written: Vec<Range<usize>> = Vec::with_capacity(captured.len());
    let mut written_sites: Vec<WrittenSite> = Vec::new();
    // Values escaped to stay text in a string, at their match's start, to be questioned.
    let mut escapes: Vec<(usize, String)> = Vec::new();
    let Laid {
        slots,
        bases,
        changes,
    } = laid_out(before, rewrite)?;
    let mut changes = changes.into_iter().peekable();
    // Where each change to the text around the matches was made: its old range and its new.
    let mut laid: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for (index, found) in captured.iter().enumerate() {
        let rendered = render(text, arena, found, names, rewrite, lexed, &bases[index])
            .map_err(|why| (found.range.start, why))?;
        if let Some(comment) = dropped_comment(lexed, found, edges, &rendered, rewrite, names) {
            return Err((
                comment.start,
                format!(
                    "the comment {:?} is in the match but in no capture the rewrite writes back, \
                     so the edit would drop it: capture it with a sequence hole, such as \
                     $args..., or narrow the pattern so that it does not match here",
                    &text[comment.clone()]
                ),
            ));
        }
        for name in &rendered.escaped {
            escapes.push((found.range.start, escaped_note(text, found, names, name)));
        }
        copied = write_changes(
            text,
            &mut out,
            copied,
            &mut changes,
            found.range.start,
            &mut laid,
        );
        out.push_str(&text[copied..found.range.start]);
        let base = out.len();
        for (range, sequence, name, site) in rendered.code {
            written_sites.push(WrittenSite {
                range: base + range.start..base + range.end,
                found: index,
                sequence,
                name,
                site,
            });
        }
        out.push_str(&rendered.text);
        written.push(base..out.len());
        copied = found.range.end;
    }
    copied = write_changes(text, &mut out, copied, &mut changes, text.len(), &mut laid);
    out.push_str(&text[copied..]);
    Ok(Writing {
        out,
        written,
        written_sites,
        escapes,
        laid,
        slots,
    })
}

/// Writes onto `out` the text from `copied` up to each of `changes` that starts at or before
/// `until`, and the change's text in place of its range, recording its old range and its new in
/// `laid`; returns where copying the text resumes.  A change at a match's start, the empty gap
/// after a header's colon with no space after it, goes before the match.
fn write_changes(
    text: &str,
    out: &mut String,
    mut copied: usize,
    changes: &mut std::iter::Peekable<std::vec::IntoIter<(Range<usize>, String)>>,
    until: usize,
    laid: &mut Vec<(Range<usize>, Range<usize>)>,
) -> usize {
    while let Some((range, line)) = changes.next_if(|(range, _)| range.start <= until) {
        out.push_str(&text[copied..range.start]);
        laid.push((range.clone(), out.len()..out.len() + line.len()));
        out.push_str(&line);
        copied = range.end;
    }
    copied
}

/// A level's edits, in order: each match in `captured` written at `written`, now at `now` once
/// layout is carried, and each change in `laid` round them.  Carrying layout changes text only
/// inside what a match's rewrite wrote, so a change after a match moves as that match's end
/// does.
fn level_edits(
    captured: &[Captured],
    written: &[Range<usize>],
    now: &[Range<usize>],
    laid: &[(Range<usize>, Range<usize>)],
) -> Vec<(Range<usize>, Range<usize>)> {
    let shift = |at: usize| -> isize {
        written
            .iter()
            .zip(now)
            .take_while(|(old, _)| old.end <= at)
            .last()
            .map_or(0, |(old, new)| {
                new.end.cast_signed() - old.end.cast_signed()
            })
    };
    let mut all: Vec<(Range<usize>, Range<usize>)> = captured
        .iter()
        .zip(now)
        .map(|(found, new)| (found.range.clone(), new.clone()))
        .chain(laid.iter().map(|(old, new)| {
            let moved = |at: usize| {
                at.checked_add_signed(shift(new.start))
                    .expect("carrying layout never moves text before its start")
            };
            (old.clone(), moved(new.start)..moved(new.end))
        }))
        .collect();
    // An empty change at a match's start was written before it.
    all.sort_by_key(|(old, _)| (old.start, old.end));
    all
}

/// Each match's layout, as [`laid_out`] decides it: where it shares its line with other code,
/// and the indentation of the block it is in, and the changes to the text around the matches
/// that give their rewrites lines of their own, in order.
struct Laid {
    slots: Vec<Option<Slot>>,
    bases: Vec<String>,
    changes: Vec<(Range<usize>, String)>,
}

/// The layout of every match in `before`, decided before any is written, since laying out a
/// block on its header's line changes text before the first match in it; or why a match cannot
/// be laid out, at its start.  A match that shares its line is rendered once at no depth, so
/// that it parses alone, to see what it needs of the line.
fn laid_out(before: &Before<'_>, rewrite: &Rewrite) -> Result<Laid, (usize, String)> {
    let Before {
        text,
        arena,
        lexed,
        captured,
        names,
    } = *before;
    let template_unit = lex(rewrite.text()).indent_unit;
    let unit = lexed.indent_unit.as_deref().or(template_unit.as_deref());
    let mut slots = Vec::with_capacity(captured.len());
    let mut bases = Vec::with_capacity(captured.len());
    let mut changes = Vec::new();
    for found in captured {
        let slot = Slot::find(text, arena, lexed, &found.range);
        let base = match &slot {
            None => line_indent(text, found.range.start),
            Some(slot) => {
                let alone = render(text, arena, found, names, rewrite, lexed, "")
                    .map_err(|why| (found.range.start, why))?;
                let ending = line_ending(text, found.range.start);
                let layout = slot
                    .lay_out(Needs::of_text(&alone.text), unit, ending)
                    .map_err(|why| (found.range.start, why.to_owned()))?;
                changes.extend(layout.changes);
                layout.indent
            }
        };
        slots.push(slot);
        bases.push(base);
    }
    Ok(Laid {
        slots,
        bases,
        changes: in_order(changes),
    })
}

/// The backstop for a layout nothing in [`lines`] foresaw: refuses, at the match's start, the
/// first match of `captured` with a slot whose rewrite, written at `written` in the new text
/// parsed as `arena`, leaves a statement it wrote at its top level, or one beside it on its
/// line, in a block other than the one the match stood in.  `edits` take an offset of the old
/// text to the new.  A statement beside it that another match rewrote is that match's to answer
/// for.
fn left_block(
    captured: &[Captured],
    slots: &[Option<Slot>],
    written: &[Range<usize>],
    edits: &[(Range<usize>, Range<usize>)],
    arena: &Arena<'_>,
) -> Result<(), (usize, String)> {
    let maps = [edits.to_vec()];
    let moved = |at: usize| forward(&maps, at);
    for ((found, slot), written) in captured.iter().zip(slots).zip(written) {
        let Some(slot) = slot else { continue };
        let beside = slot
            .beside
            .iter()
            .filter(|b| {
                !captured
                    .iter()
                    .any(|c| c.range.start < b.end && b.start < c.range.end)
            })
            .map(|b| moved(b.start))
            .collect();
        let placed = Placed {
            slot,
            written: written.clone(),
            beside,
        };
        if !placed.stays(arena, moved) {
            return Err((found.range.start, LEFT_BLOCK.to_owned()));
        }
    }
    Ok(())
}

/// `text`, parsed as `arena`, with every match in `captured`, none overlapping, deleted by a
/// rewrite with no code: each written over with the rewrite's comment, if it has one, or with
/// nothing.
///
/// A rewrite with no code leaves nothing to say where the code around a match goes, so the
/// match takes what would otherwise be left as debris: a statement or a list item alone on its
/// lines takes those lines whole, its trailing comment and line break with them, unless the
/// rewrite writes a comment there; otherwise the match takes one separator - a comma or a
/// semicolon - its parent holds next to it, the one after it unless that one is a trailing comma
/// and there is one before it.  A comma is taken across line breaks, since it separates the item
/// from its neighbour wherever they fall; a semicolon only from the match's own line.
/// Everything inside the match goes with it, comments included, since they describe code that
/// is gone.  A comment the rewrite writes may not have code after it on its line, which it would
/// swallow.  A result that does not parse is left to the parse check, which refuses it.
fn deleted(
    text: &str,
    arena: &Arena<'_>,
    lexed: &Lexed,
    captured: &[Captured],
    names: &[&str],
    rewrite: &Rewrite,
) -> Result<Level, (usize, String)> {
    let wrapping = Wrapping::new(text, arena, lexed);
    let parents: HashMap<NodeId, NodeId> = arena
        .nodes()
        .flat_map(|node| arena.children(node).map(move |child| (child, node)))
        .collect();
    let children = RefCell::new(HashMap::new());
    let mut taken: Vec<Range<usize>> = Vec::with_capacity(captured.len());
    let mut separated: Vec<bool> = Vec::with_capacity(captured.len());
    let mut writes: Vec<String> = Vec::with_capacity(captured.len());
    let mut escapes: Vec<(usize, String)> = Vec::new();
    for found in captured {
        let base = line_indent(text, found.range.start);
        let rendered = render(text, arena, found, names, rewrite, lexed, &base)
            .map_err(|why| (found.range.start, why))?;
        for name in &rendered.escaped {
            escapes.push((found.range.start, escaped_note(text, found, names, name)));
        }
        let comment = !rendered.text.trim().is_empty();
        let deletion = Deletion {
            text,
            arena,
            wrapping: &wrapping,
            taken: &taken,
            parents: &parents,
            children: &children,
        };
        let (range, took) = deletion.range(&found.range, !comment);
        // A match that takes neither its lines nor a separator leaves a hole where it stood.
        // A statement, or an item of a list, a tuple, or a call's or definition's own, can
        // simply be gone; anywhere else what is left can parse and mean something else -
        // `x = debug + a` becomes `x =  + a`, `return debug` becomes `return` - so it is refused.
        if !took {
            let matched = wrapping.widened(&(0..text.len()), &found.range);
            let parent = deletion.parent(&matched);
            // Every statement kind's name starts `Stmt`, as Ruff names them and
            // `quaffed_python`'s kind list keeps them: its test holds the list to Ruff's enum.
            let statement = arena
                .nodes_within(matched.clone())
                .next()
                .is_some_and(|node| arena.kind(node).name().starts_with("Stmt"));
            if !statement && !ITEMS.contains(&arena.kind(parent).name()) {
                return Err((
                    found.range.start,
                    "removing this match would leave what holds it without it - an operator, a \
                     call, a subscript, a slice, or a return, yield or raise - which may still \
                     parse but would mean something else: remove the whole of what holds it, \
                     or narrow the pattern so that it does not match here"
                        .to_owned(),
                ));
            }
        }
        // The last of a run of list items, each of which took the separator after it, has none
        // left to take, so the run takes the one before it, if its parent holds one.
        if !took {
            let mut first = taken.len();
            while first > 0 && taken[first - 1].end == range.start && separated[first - 1] {
                first -= 1;
            }
            if first < taken.len() {
                let floor = first.checked_sub(1).map_or(0, |before| taken[before].end);
                if let Some(start) =
                    deletion.before(&found.range, taken[first].start, floor, !comment)
                {
                    taken[first].start = start;
                }
            }
        }
        separated.push(took);
        if comment {
            let rest = &text[range.end..line_end(text, range.end)];
            let rest = rest.trim();
            if !rest.is_empty() && !rest.starts_with('#') {
                return Err((
                    found.range.start,
                    format!(
                        "the rewrite is a comment, and {rest:?} follows the match on its line, so \
                         the comment would swallow that code: narrow the pattern so that it does \
                         not match here, or delete the match instead"
                    ),
                ));
            }
        }
        taken.push(range);
        writes.push(if comment {
            rendered.text
        } else {
            String::new()
        });
    }
    // A match taking its whole line may start before an earlier one on that line ends; the
    // earlier one then takes in the start of the line, and the later one starts where it ends,
    // so that each is written once and the line goes whole.
    for index in (1..taken.len()).rev() {
        if taken[index].start < taken[index - 1].end {
            taken[index - 1].start = taken[index - 1].start.min(taken[index].start);
            taken[index].start = taken[index - 1].end.min(taken[index].end);
        }
    }
    // A tuple left with one item and no comma after it would be that item alone, bracketed or
    // not, so the comma that keeps it a tuple is written after it.
    let mut parts: Vec<(Range<usize>, String)> = taken.into_iter().zip(writes).collect();
    for at in one_item_tuples(text, arena, &wrapping, &parents, captured, &parts) {
        let index = parts.partition_point(|(range, _)| range.start < at);
        parts.insert(index, (at..at, ",".to_owned()));
    }
    Ok(Level {
        questioned: escapes,
        ..level(text, &parts)
    })
}

/// `changes` in order of where they start, each range once: two matches can ask for the same
/// change, as two statements on a block's header line both ask for the block to be given lines
/// of its own.
fn in_order(mut changes: Vec<(Range<usize>, String)>) -> Vec<(Range<usize>, String)> {
    changes.sort_by_key(|(range, _)| range.start);
    changes.dedup_by(|a, b| a.0 == b.0);
    changes
}

/// `text` with each of `parts` written over its range, which are in order and do not overlap, and
/// each range's old and new place; nothing questioned.
fn level(text: &str, parts: &[(Range<usize>, String)]) -> Level {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut edits = Vec::with_capacity(parts.len());
    for (range, written) in parts {
        out.push_str(&text[copied..range.start]);
        let base = out.len();
        out.push_str(written);
        edits.push((range.clone(), base..out.len()));
        copied = range.end;
    }
    out.push_str(&text[copied..]);
    Level {
        text: out,
        edits,
        questioned: Vec::new(),
    }
}

/// Where to write a comma so that each tuple the deletions in `parts` leave with one item stays a
/// tuple: after that item, unless a comma after it survives.  A tuple left with none is left to
/// the parse check where it has no brackets, and is the empty tuple where it has.
fn one_item_tuples(
    text: &str,
    arena: &Arena<'_>,
    wrapping: &Wrapping<'_>,
    parents: &HashMap<NodeId, NodeId>,
    captured: &[Captured],
    parts: &[(Range<usize>, String)],
) -> Vec<usize> {
    let whole = 0..text.len();
    // `parts` are in order and do not overlap, so the one that could hold `at` is found by search.
    let holding = |at: usize| {
        let index = parts.partition_point(|(range, _)| range.start <= at);
        index
            .checked_sub(1)
            .map(|index| &parts[index].0)
            .filter(|range| range.contains(&at))
    };
    let gone = |at: usize| holding(at).is_some();
    let deletion = Deletion {
        text,
        arena,
        wrapping,
        taken: &[],
        parents,
        children: &RefCell::new(HashMap::new()),
    };
    let mut tuples: Vec<NodeId> = captured
        .iter()
        .map(|found| deletion.parent(&wrapping.widened(&whole, &found.range)))
        .filter(|&parent| arena.kind(parent).name() == "ExprTuple")
        .collect();
    tuples.sort_unstable_by_key(|tuple| arena.range(*tuple).start);
    tuples.dedup();
    let mut commas = Vec::new();
    for tuple in tuples {
        let items: Vec<Range<usize>> = arena
            .children(tuple)
            .map(|item| wrapping.widened(&whole, &arena.range(item)))
            .collect();
        let left: Vec<&Range<usize>> = items.iter().filter(|item| !gone(item.start)).collect();
        let ([item], true) = (&left[..], items.len() > 1) else {
            continue;
        };
        // The next byte after the item that is neither whitespace nor deleted.
        let mut next = Some(item.end).filter(|&at| at < text.len());
        while let Some(at) = next {
            next = match holding(at) {
                Some(range) => Some(range.end),
                None if text.as_bytes()[at].is_ascii_whitespace() => Some(at + 1),
                None => break,
            }
            .filter(|&at| at < text.len());
        }
        if next.is_none_or(|at| text.as_bytes()[at] != b',') {
            commas.push(item.end);
        }
    }
    commas
}

/// The kinds whose children are items that a deletion taking no separator may simply leave out:
/// `[debug]` is `[]`, `f(debug)` is `f()`, `class A(debug)` is `class A()`.  A set is not one,
/// since `{}` is a dict.  Each is Ruff's name for the node kind, as `quaffed_python`'s kind list
/// gives it; a parameter is left out because no pattern of its own matches one.
const ITEMS: &[&str] = &["ExprList", "ExprTuple", "Arguments"];

/// Where the line holding `offset` ends: its line break, or the end of `text`.
fn line_end(text: &str, offset: usize) -> usize {
    text[offset..]
        .find('\n')
        .map_or(text.len(), |at| offset + at)
}

/// Whether a match's statements start the block they are in, and whether they end it.
#[derive(Clone, Copy)]
struct BlockEdges {
    starts: bool,
    ends: bool,
}

/// What one match of a rewrite with no code takes from the file, given what the matches before
/// it in the same file took.
struct Deletion<'a, 'text> {
    text: &'a str,
    arena: &'a Arena<'text>,
    wrapping: &'a Wrapping<'a>,
    /// What the matches before took, in order.
    taken: &'a [Range<usize>],
    /// Each node's parent.
    parents: &'a HashMap<NodeId, NodeId>,
    /// The ranges of each parent's children, in order, as [`holds`](Deletion::holds) needs them.
    children: &'a RefCell<HashMap<NodeId, Vec<Range<usize>>>>,
}

impl Deletion<'_, '_> {
    /// What deleting `found` takes: the match with the brackets that only wrap it, and its whole
    /// lines where `whole_lines` and it stands alone on them, or else one separator its parent
    /// holds next to it; and whether it took either.  `whole_lines` is false for a comment
    /// written in place of the match, which keeps its line and so may take no comma from
    /// another; [`before`](Deletion::before) calls the same choice `across_lines`.
    fn range(&self, found: &Range<usize>, whole_lines: bool) -> (Range<usize>, bool) {
        let text = self.text;
        let matched = self.wrapping.widened(&(0..text.len()), found);
        let floor = self.taken.last().map_or(0, |taken| taken.end);
        let parent = self.parent(&matched);
        // The separator after the match, if its parent holds one there: on the same line for a
        // semicolon, and for a comma anywhere before the next code where `whole_lines`, since it
        // separates the item from its neighbour wherever the line breaks fall.  A comment
        // written in place of the item takes none from another line, which it would join.
        let after = {
            let rest = &text[matched.end..];
            let at = matched.end + rest.len() - rest.trim_start().len();
            (at < text.len()
                && self.separator(parent, at)
                && (text.as_bytes()[at] == b',' && whole_lines
                    || !text[matched.end..at].contains('\n')))
            .then_some(at)
        };
        let line_start = text[..matched.start].rfind('\n').map_or(0, |at| at + 1);
        let end = line_end(text, matched.end);
        // A match whose comma starts the next line is not alone on its lines: taking them would
        // leave that comma with nothing before it.
        if whole_lines && after.is_none_or(|at| at < end) {
            let rest = text[after.map_or(matched.end, |at| at + 1)..end].trim();
            let alone_after = rest.is_empty() || rest.starts_with('#');
            // Only the matches taken last can reach back onto this line: their ends only grow.
            let near: Vec<&Range<usize>> = self
                .taken
                .iter()
                .rev()
                .take_while(|taken| taken.end > line_start)
                .collect();
            let alone_before = (line_start..matched.start).all(|at| {
                text.as_bytes()[at].is_ascii_whitespace()
                    || near.iter().any(|taken| taken.contains(&at))
            });
            if alone_before && alone_after {
                let lines = line_start..(end + 1).min(text.len());
                return (self.whole_lines(lines, self.block_edges(&matched)), true);
            }
        }
        let before = self.before(found, matched.start, floor, whole_lines);
        let trailing_comma = after.is_some_and(|at| {
            text.as_bytes()[at] == b','
                && matches!(
                    text[at + 1..].trim_start().as_bytes().first(),
                    Some(b')' | b']' | b'}')
                )
        });
        match (after, before) {
            // A comment written in place of the item stands before the trailing comma, which
            // ends its line; otherwise the item takes the comma before it, keeping the trailing
            // one as the list's layout.
            (Some(_), Some(start)) if trailing_comma && whole_lines => (start..matched.end, true),
            (Some(at), _) => {
                // The spaces after a separator go with it while code follows on the line.
                let rest = &text[at + 1..];
                let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
                let spaces = match rest[spaces..].chars().next() {
                    None | Some('\n' | '\r' | '#') => 0,
                    Some(_) => spaces,
                };
                (matched.start..at + 1 + spaces, true)
            }
            (None, Some(start)) => (start..matched.end, true),
            (None, None) => (matched, false),
        }
    }

    /// Where deleting from `start` back to the separator before it would start, if the parent of
    /// `found` holds a separator there after `floor`, which earlier matches took up to: on the
    /// same line for a semicolon, and for a comma unless `across_lines`.  The spaces before the
    /// separator go with it.
    fn before(
        &self,
        found: &Range<usize>,
        start: usize,
        floor: usize,
        across_lines: bool,
    ) -> Option<usize> {
        let text = self.text;
        let parent = self.parent(&self.wrapping.widened(&(0..text.len()), found));
        let floor = floor.min(start);
        let at = floor + text[floor..start].trim_end().len();
        // A comma in a comment is no separator, and one with a comment between it and the match
        // is not taken, since the comment would go with it.
        let separator = at.checked_sub(1).filter(|&at| {
            at >= floor
                && !self.in_comment(at)
                && self.separator(parent, at)
                && (text.as_bytes()[at] == b',' && across_lines || !text[at..start].contains('\n'))
        })?;
        Some(floor + text[floor..separator].trim_end_matches([' ', '\t']).len())
    }

    /// Whether `at` lies inside a comment.
    fn in_comment(&self, at: usize) -> bool {
        let comments = &self.wrapping.lexed.comments;
        let index = comments.partition_point(|comment| comment.start <= at);
        index > 0 && comments[index - 1].contains(&at)
    }

    /// Whether `at` is a comma or a semicolon `parent` holds between its children.
    fn separator(&self, parent: NodeId, at: usize) -> bool {
        matches!(self.text.as_bytes()[at], b',' | b';') && self.holds(parent, at)
    }

    /// `lines`, the whole lines a match stands alone on, with what goes with them: the comment
    /// lines after a block the match ends with, indented into it, which describe code that is
    /// gone; and the blank lines either side that would otherwise be left as a gap of their own.
    /// Between code, where blank lines stand both before and after, the narrower run goes, so the
    /// wider gap is left; at the start or the end of the file, the blank lines at that edge go.
    /// `edges` says whether the match starts its block and whether it ends it: a match that
    /// starts one takes the blank lines after it, which would otherwise open the block, and one
    /// that ends one takes those before it, which would otherwise close it, leaving those after,
    /// which part the block from what follows.
    fn whole_lines(&self, lines: Range<usize>, edges: BlockEdges) -> Range<usize> {
        let text = self.text;
        let indent = |at: usize| {
            let line = &text[at..line_end(text, at)];
            line.len() - line.trim_start_matches([' ', '\t']).len()
        };
        let blank = |at: usize| text[at..line_end(text, at)].trim().is_empty();
        let next_line = |at: usize| (line_end(text, at) + 1).min(text.len());
        let mut end = lines.end;
        // The lines without their final line break, which a last line may not have.
        let body = text[lines.clone()].trim_end_matches(['\n', '\r']);
        let last_line = lines.start + body.rfind('\n').map_or(0, |at| at + 1);
        let base = indent(lines.start);
        if indent(last_line) > base {
            let mut at = end;
            while at < text.len() {
                let line = text[at..line_end(text, at)].trim_start();
                if line.starts_with('#') && indent(at) > base {
                    end = next_line(at);
                } else if !line.is_empty() {
                    break;
                }
                at = next_line(at);
            }
        }
        // The blank lines either side: those before, back to where the line before them
        // starts, and those after, to where the line after them starts.
        let mut start = lines.start;
        let mut before = 0;
        while start > 0 {
            let previous = text[..start - 1].rfind('\n').map_or(0, |at| at + 1);
            if !blank(previous) {
                break;
            }
            (start, before) = (previous, before + 1);
        }
        let (mut past, mut after) = (end, 0);
        while past < text.len() && blank(past) {
            (past, after) = (next_line(past), after + 1);
        }
        // At the start or the end of the file, the blank lines at that edge go too, so that none
        // are left there.  Between code, the gap that is left is the wider of the two, so that the
        // file's own spacing stands: deleting a class's last method leaves the class's gap.
        if start == 0 || past == text.len() {
            start..past
        } else if edges.starts && !edges.ends {
            lines.start..past
        } else if edges.ends && !edges.starts {
            start..end
        } else if before > 0 && after > 0 && before < after {
            start..end
        } else if before > 0 && after > 0 {
            lines.start..past
        } else {
            lines.start..end
        }
    }

    /// Whether the statements `matched` holds start the block they are in and whether they end
    /// it, from the list of statements the adapter marks; neither where it holds no statement.
    fn block_edges(&self, matched: &Range<usize>) -> BlockEdges {
        let arena = self.arena;
        let none = BlockEdges {
            starts: false,
            ends: false,
        };
        // Pre-order, so the first node in a list is the outermost statement the match starts
        // with; the rest of the match's statements are its siblings in the same list.
        let Some(first) = arena
            .nodes_within(matched.clone())
            .find(|&node| arena.list(node).is_some())
        else {
            return none;
        };
        let list = arena.list(first);
        let Some(&parent) = self.parents.get(&first) else {
            return none;
        };
        let mut siblings = arena
            .children(parent)
            .filter(|&child| arena.list(child) == list)
            .map(|child| arena.range(child));
        let Some(opening) = siblings.next() else {
            return none;
        };
        let closing = siblings.last().unwrap_or_else(|| opening.clone());
        BlockEdges {
            starts: opening.start >= matched.start,
            ends: closing.end <= matched.end,
        }
    }

    /// The deepest node whose range holds `range` and is wider than it: from the first node
    /// inside `range`, found by search, up through the parents until one reaches beyond it.
    fn parent(&self, range: &Range<usize>) -> NodeId {
        let arena = self.arena;
        let Some(mut node) = arena.nodes_within(range.clone()).next() else {
            return arena.root();
        };
        while let Some(&parent) = self.parents.get(&node) {
            node = parent;
            let held = arena.range(node);
            if held.start < range.start || range.end < held.end {
                break;
            }
        }
        node
    }

    /// Whether `at` is a separator `parent` holds between its children: inside its range and
    /// inside none of theirs.
    fn holds(&self, parent: NodeId, at: usize) -> bool {
        let arena = self.arena;
        let range = arena.range(parent);
        if !(range.start <= at && at < range.end || parent == arena.root()) {
            return false;
        }
        // Each parent's children's ranges, gathered once and in order, so `at` is found among
        // them by search.
        let mut children = self.children.borrow_mut();
        let ranges = children
            .entry(parent)
            .or_insert_with(|| arena.children(parent).map(|c| arena.range(c)).collect());
        let index = ranges.partition_point(|child| child.start <= at);
        !index
            .checked_sub(1)
            .is_some_and(|index| ranges[index].contains(&at))
    }
}

/// The first comment in `found` that the rewrite drops: in no capture it writes back, not at
/// the edge of one of the match's own brackets, carried with it, and not in layout `rendered`
/// carried between two holes.
fn dropped_comment<'a>(
    lexed: &'a Lexed,
    found: &Captured,
    edges: &[Range<usize>],
    rendered: &Rendered,
    rewrite: &Rewrite,
    names: &[&str],
) -> Option<&'a Range<usize>> {
    let within = |outer: &Range<usize>, comment: &Range<usize>| {
        outer.start <= comment.start && comment.end <= outer.end
    };
    lexed.comments.iter().find(|comment| {
        within(&found.range, comment)
            && !edges.contains(comment)
            && !rendered.carried.iter().any(|gap| within(gap, comment))
            && !found
                .captures
                .iter()
                .take(rewrite_names(rewrite, names))
                .any(|capture| within(capture, comment))
    })
}

/// What a question says of `name`'s value in `found`, escaped to stay text in a string.
fn escaped_note(text: &str, found: &Captured, names: &[&str], name: &str) -> String {
    let index = names
        .iter()
        .position(|n| *n == name)
        .expect("every site's name was asked for");
    format!(
        "what ${name} captured, {:?}, holds what the string it is written into would read as its \
         end or as code - a quote, a backslash, a line break, or a brace in an f-string - so it \
         was escaped, and the string holds the text as captured: if the code was meant to run, \
         write the hole outside the string in the rewrite",
        &text[found.captures[index].clone()]
    )
}

/// The first capture in `after`, the file's new text, written where Python reads it with what
/// surrounds it - `x + 1` into `$a * 2` - so that the code would mean something other than the
/// template says: each code site should still be code of its own, one node, or for a sequence a
/// run of them.  It is refused, named at its match's start in `before`, the text the matches were
/// found in, with the template as it would work: brackets round the hole.  Brackets are not added
/// here, since that would write something other than the template.
fn regrouped(
    before: &Before<'_>,
    after: (&str, &Arena<'_>),
    sites: &[WrittenSite],
    rewrite: &Rewrite,
) -> Option<(usize, String)> {
    let (out, arena) = after;
    let code = CodeIndex::of(arena);
    let lexed = lex(out);
    let site = sites.iter().find(|site| {
        let whole = unwrapped(out, &lexed, &site.range)
            .iter()
            .any(|range| code.whole(range))
            || (site.sequence && is_run(out, &lexed, &code, &site.range));
        // A tuple's own brackets are part of it, so one the rewrite brackets, `wrap(($x))`, is
        // whole only with them.  Brackets that make some other node - a call's, in `wrap($x)` -
        // do not count, so the node they close must be the kind of code that was captured.
        !whole && !rebracketed(before, &code, out, &lexed, site)
    })?;
    // Brackets round a sequence make its items one tuple, which is not what it captured either,
    // so it is told to stand where its items stand apart instead.
    let remedy = if site.sequence {
        "write the hole where its items stand apart from what is around it, as among a call's \
         arguments, since brackets round it would make them one tuple"
            .to_owned()
    } else {
        format!(
            "put brackets round the hole in the rewrite, as in {:?}",
            bracketed(rewrite, site.site)
        )
    };
    Some((
        before.captured[site.found].range.start,
        format!(
            "what ${} captured, {:?}, would not be code of its own where the rewrite writes it - \
             it would be read together with what stands around it, so the code would mean \
             something other than the rewrite says: {remedy}",
            site.name,
            &out[site.range.clone()],
        ),
    ))
}

/// `range` of `text`, lexed as `lexed`, and each stretch inside the brackets at its ends, layout
/// inside them aside, in turn, outermost first: what a capture that took brackets wrapping it may
/// stand for.  Which brackets only wrap it is not known here - `([1, 2])` is a list in wrapping
/// brackets, whose own are not - so every layer is given, for the caller to find its node among.
fn unwrapped(text: &str, lexed: &Lexed, range: &Range<usize>) -> Vec<Range<usize>> {
    let mut layers = vec![range.clone()];
    while let Some(b) = lexed.brackets.iter().find(|b| {
        let range = &layers[layers.len() - 1];
        b.open == range.start && b.close + 1 == range.end
    }) {
        let start = layout::forward_over_layout(text, b.open + 1, b.close, &lexed.comments);
        let end = back_over_layout(text, start, b.close, &lexed.comments);
        layers.push(start..end);
    }
    layers
}

/// Whether `range` of `text`, lexed as `lexed` and indexed as `code`, is a run of sibling nodes.
/// Its first and last items may each have taken brackets that only wrap them, which no node's
/// range includes, so each place it could start and end, inside any such brackets, is tried.
fn is_run(text: &str, lexed: &Lexed, code: &CodeIndex, range: &Range<usize>) -> bool {
    let mut starts = vec![range.start];
    while let Some(b) = lexed
        .brackets
        .iter()
        .find(|b| b.open == starts[starts.len() - 1])
    {
        starts.push(layout::forward_over_layout(
            text,
            b.open + 1,
            b.close,
            &lexed.comments,
        ));
    }
    let mut ends = vec![range.end];
    while let Some(b) = lexed
        .brackets
        .iter()
        .find(|b| b.close + 1 == ends[ends.len() - 1])
    {
        ends.push(back_over_layout(text, b.open + 1, b.close, &lexed.comments));
    }
    starts.iter().any(|&start| {
        ends.iter()
            .any(|&end| start <= end && code.run(&(start..end)))
    })
}

/// Whether `site`, written into `out`, lexed as `lexed` and indexed as `code`, stands whole once
/// the brackets round it with only layout between are taken in: some such bracketed stretch is a
/// node of a kind the capture's code, in `before`, was too.
fn rebracketed(
    before: &Before<'_>,
    code: &CodeIndex,
    out: &str,
    lexed: &Lexed,
    site: &WrittenSite,
) -> bool {
    let mut range = site.range.clone();
    let mut wrapped = Vec::new();
    while let Some(b) = lexed.brackets.iter().find(|b| {
        b.open < range.start
            && range.end <= b.close
            && layout::forward_over_layout(out, b.open + 1, b.close, &lexed.comments) == range.start
            && back_over_layout(out, range.end, b.close, &lexed.comments) == range.end
    }) {
        range = b.open..b.close + 1;
        wrapped.push(range.clone());
    }
    if wrapped.is_empty() {
        return false;
    }
    let index = before
        .names
        .iter()
        .position(|name| *name == site.name)
        .expect("every site's name was asked for");
    let layers = unwrapped(
        before.text,
        before.lexed,
        &before.captured[site.found].captures[index],
    );
    let captured_kinds: Vec<Kind> = layers
        .iter()
        .flat_map(|layer| {
            before
                .arena
                .nodes_within(layer.clone())
                .filter(move |&node| before.arena.range(node) == *layer)
                .map(|node| before.arena.kind(node))
        })
        .collect();
    wrapped.iter().any(|range| {
        code.kinds(range)
            .iter()
            .any(|kind| captured_kinds.contains(kind))
    })
}

/// The line of `rewrite`'s template that holds its `site`th site, with brackets round the site,
/// without its indentation, which a message has no use for.
fn bracketed(rewrite: &Rewrite, site: usize) -> String {
    let text = rewrite.text();
    let written = &rewrite.sites()[site].written;
    let start = text[..written.start]
        .rfind(['\n', '\r'])
        .map_or(0, |i| i + 1);
    let end = text[written.end..]
        .find(['\n', '\r'])
        .map_or(text.len(), |i| written.end + i);
    format!(
        "{}({}){}",
        &text[start..written.start],
        &text[written.clone()],
        &text[written.end..end]
    )
    .trim()
    .to_owned()
}

/// How many of `names` the rewrite writes: they come first, the pattern's other names after.
fn rewrite_names(rewrite: &Rewrite, names: &[&str]) -> usize {
    names
        .iter()
        .take_while(|name| rewrite.sites().iter().any(|site| site.name == **name))
        .count()
}

/// Where a parsed file's code stands, gathered in one walk so that each written site is looked up
/// rather than searched for: every node's range, and for each offset the parents of the nodes
/// that start or end there.
struct CodeIndex {
    ranges: HashMap<(usize, usize), Vec<Kind>>,
    starting: HashMap<usize, Vec<Option<NodeId>>>,
    ending: HashMap<usize, Vec<Option<NodeId>>>,
}

impl CodeIndex {
    fn of(arena: &Arena<'_>) -> CodeIndex {
        let mut index = CodeIndex {
            ranges: HashMap::new(),
            starting: HashMap::new(),
            ending: HashMap::new(),
        };
        let mut walk = vec![(arena.root(), None)];
        while let Some((node, parent)) = walk.pop() {
            let range = arena.range(node);
            index
                .ranges
                .entry((range.start, range.end))
                .or_default()
                .push(arena.kind(node));
            index.starting.entry(range.start).or_default().push(parent);
            index.ending.entry(range.end).or_default().push(parent);
            walk.extend(arena.children(node).map(|child| (child, Some(node))));
        }
        index
    }

    /// Whether `range` is exactly one node.
    fn whole(&self, range: &Range<usize>) -> bool {
        self.ranges.contains_key(&(range.start, range.end))
    }

    /// The kinds of the nodes that are exactly `range`, none if no node is.
    fn kinds(&self, range: &Range<usize>) -> &[Kind] {
        self.ranges
            .get(&(range.start, range.end))
            .map_or(&[], Vec::as_slice)
    }

    /// Whether `range` is a run of sibling nodes: one starting where it starts and one ending
    /// where it ends, under the same parent, so that nothing but separators stands between them.
    fn run(&self, range: &Range<usize>) -> bool {
        let (Some(first), Some(last)) =
            (self.starting.get(&range.start), self.ending.get(&range.end))
        else {
            return false;
        };
        first.iter().any(|parent| last.contains(parent))
    }
}

/// The rewrite of one match: the template indented, and its line breaks spelled, for where the
/// match stands, each site written with what its name captured; where each code site was
/// written in it; the file's layout carried between two holes; and the names whose values were
/// escaped to stay text in a string.
///
/// Where the template writes two holes next to each other inside a bracket, with only layout and
/// the same code between them as the file has between their captures, the file's layout stands
/// there: inside a bracket a line break means nothing, so it is kept as written.  A value written
/// into a string is escaped as the string needs to hold it as text; one that cannot be, or that
/// still changes the literal's token structure, is refused, with why.
///
/// `base` is the indentation of the block the match is in, which its later lines take: the
/// match's line's, unless [`lines`] lays the match out on lines of its own.  A capture of
/// statements the template writes where they need lines of their own is given them in the
/// template, as [`lines`] lays them out, or refused where it cannot.
fn render(
    text: &str,
    arena: &Arena<'_>,
    found: &Captured,
    names: &[&str],
    rewrite: &Rewrite,
    lexed: &Lexed,
    base: &str,
) -> Result<Rendered, String> {
    let job = Job {
        text,
        arena,
        found,
        names,
        rewrite,
        lexed,
        base,
    };
    let marked = job.marked()?;
    let gaps = Gaps {
        between: job.gaps(&marked),
        items: job.item_gaps(&marked),
    };
    let mut escaped = Vec::new();
    let (written, code) = job.fill(&marked, &gaps, None, &mut escaped)?;
    if rewrite.sites().iter().any(|site| site.in_literal)
        && token_kinds(&written)
            != token_kinds(&job.fill(&marked, &gaps, Some("x"), &mut escaped)?.0)
    {
        let interpolated: Vec<String> = rewrite
            .sites()
            .iter()
            .filter(|site| site.in_literal)
            .map(|site| format!("${}", site.name))
            .collect();
        return Err(format!(
            "writing what {} captured here into a comment would end it - a line break - so \
             what follows would be read as code: narrow the pattern so that such values do not \
             match",
            interpolated.join(" and ")
        ));
    }
    Ok(Rendered {
        text: written,
        code,
        carried: gaps
            .between
            .into_iter()
            .flatten()
            .chain(gaps.items.into_iter().map(|(_, gap)| gap))
            .map(|gap| gap.range)
            .collect(),
        escaped,
    })
}

/// One match to render, with what rendering it reads.
struct Job<'a> {
    text: &'a str,
    arena: &'a Arena<'a>,
    found: &'a Captured,
    names: &'a [&'a str],
    rewrite: &'a Rewrite,
    lexed: &'a Lexed,
    /// The indentation of the block the match is in.
    base: &'a str,
}

/// The template with each site a marker that lexes as a name, indented for where the match
/// stands, so that its own strings, brackets and indentation can be read, and each site found
/// again: its text and lexing, each site's marker, and where each marker stands, in order.
struct Marked {
    indented: String,
    own: Lexed,
    markers: Vec<String>,
    at: Vec<(usize, usize)>,
}

impl Job<'_> {
    /// What the site named `name` captured.
    fn capture(&self, name: &str) -> Range<usize> {
        let index = self
            .names
            .iter()
            .position(|n| *n == name)
            .expect("every site's name was asked for");
        self.found.captures[index].clone()
    }

    fn marked(&self) -> Result<Marked, String> {
        let template = self.rewrite.text();
        let stem = marker_stem(template, self.text);
        let markers: Vec<String> = (0..self.rewrite.sites().len())
            .map(|i| format!("{stem}{i}_"))
            .collect();
        let mut with_markers = String::with_capacity(template.len());
        let mut copied = 0;
        for (site, marker) in self.rewrite.sites().iter().zip(&markers) {
            with_markers.push_str(&template[copied..site.written.start]);
            with_markers.push_str(marker);
            copied = site.written.end;
        }
        with_markers.push_str(&template[copied..]);
        let with_markers = self.laid_out(with_markers, &markers)?;
        let indented = indent_template(
            &with_markers,
            self.base,
            line_ending(self.text, self.found.range.start),
            self.lexed,
        );
        let mut at: Vec<(usize, usize)> = markers
            .iter()
            .enumerate()
            .map(|(i, marker)| {
                let place = indented.find(marker).expect("each marker is written once");
                (place, i)
            })
            .collect();
        at.sort_unstable();
        Ok(Marked {
            own: lex(&indented),
            indented,
            markers,
            at,
        })
    }

    /// `template`, with each site a marker from `markers`, laid out for the statements its code
    /// sites write: where what a site captured needs lines of its own and the template writes
    /// it beside other code on its line, the template is laid out round it as [`lines`] does a
    /// file, in the template's own indentation, or the file's where the template indents
    /// nothing.  A template that does not parse with its markers is left as it is, for the
    /// checks on the file to judge.
    fn laid_out(&self, template: String, markers: &[String]) -> Result<String, String> {
        let needs: Vec<(usize, Needs)> = self
            .rewrite
            .sites()
            .iter()
            .enumerate()
            .filter(|(_, site)| !site.in_literal)
            .filter_map(|(i, site)| {
                let needs = Needs::of(self.text, self.arena, self.lexed, &self.capture(&site.name));
                needs.any().then_some((i, needs))
            })
            .collect();
        if needs.is_empty() {
            return Ok(template);
        }
        let Ok(arena) = parse_module(&template) else {
            return Ok(template);
        };
        let lexed = lex(&template);
        let unit = lexed
            .indent_unit
            .as_deref()
            .or(self.lexed.indent_unit.as_deref());
        let mut changes = Vec::new();
        for (i, needs) in needs {
            let at = template
                .find(&markers[i])
                .expect("each marker is written once");
            let Some(slot) = Slot::find(&template, &arena, &lexed, &(at..at + markers[i].len()))
            else {
                continue;
            };
            let layout = slot.lay_out(needs, unit, "\n").map_err(|_| {
                format!(
                    "what ${} captured needs lines of its own where the rewrite writes it, in a \
                     block of the rewrite's written on its header's line, but nothing indents a \
                     block - neither the file nor the rewrite - so the depth to write it at \
                     cannot be derived: write that block of the rewrite on lines of its own, \
                     indented under its header",
                    self.rewrite.sites()[i].name
                )
            })?;
            changes.extend(layout.changes);
        }
        Ok(level(&template, &in_order(changes)).text)
    }

    /// For each site in order, the file's layout to write before it in place of the template's,
    /// where the site and the one before it are both code, next to each other in the template
    /// and in the file: inside a bracket, with the same code between them, the file's layout
    /// between them; and between statements, with no code or comment between them, the file's
    /// blank lines, before the template's own line break and indentation, since the statements
    /// may be written at another depth.
    fn gaps(&self, marked: &Marked) -> Vec<Option<Gap>> {
        let sites = self.rewrite.sites();
        let in_bracket = |brackets: &[Bracket], gap: &Range<usize>| {
            brackets
                .iter()
                .any(|b| b.open < gap.start && gap.end <= b.close)
        };
        marked
            .at
            .iter()
            .enumerate()
            .map(|(k, &(place, i))| {
                let &(before, j) = marked.at.get(k.checked_sub(1)?)?;
                let (one, other) = (&sites[j], &sites[i]);
                if one.in_literal || other.in_literal {
                    return None;
                }
                let template_gap = before + marked.markers[j].len()..place;
                let (a, b) = (self.capture(&one.name), self.capture(&other.name));
                if a.end > b.start {
                    return None;
                }
                let file_gap = a.end..b.start;
                let between_others = self
                    .found
                    .captures
                    .iter()
                    .any(|c| !c.is_empty() && c.start < file_gap.end && file_gap.start < c.end);
                let template_comment = marked
                    .own
                    .comments
                    .iter()
                    .any(|c| template_gap.start <= c.start && c.end <= template_gap.end);
                // The code is compared last, token by token, so that whitespace inside a string the
                // template changes counts, and only where the cheaper checks have passed.
                let same_code = || {
                    code_tokens(&marked.indented[template_gap.clone()])
                        == code_tokens(&self.text[file_gap.clone()])
                };
                if between_others || template_comment {
                    return None;
                }
                let in_template = &marked.indented[template_gap.clone()];
                let in_file = &self.text[file_gap.clone()];
                match (
                    in_bracket(&marked.own.brackets, &template_gap),
                    in_bracket(&self.lexed.brackets, &file_gap),
                ) {
                    (true, true) => same_code().then(|| Gap {
                        text: in_file.to_owned(),
                        range: file_gap,
                    }),
                    (false, false) => {
                        let file_comment = self
                            .lexed
                            .comments
                            .iter()
                            .any(|c| file_gap.start <= c.start && c.end <= file_gap.end);
                        // Each gap runs from the end of one statement's line, through any blank
                        // lines, to the next statement's indentation.
                        let (Some(file_last), Some(template_first)) =
                            (in_file.rfind('\n'), in_template.find('\n'))
                        else {
                            return None;
                        };
                        (!file_comment
                            && code_tokens(in_template).is_empty()
                            && code_tokens(in_file).is_empty())
                        .then(|| Gap {
                            text: format!(
                                "{}{}",
                                &in_file[..=file_last],
                                &in_template[template_first + 1..]
                            ),
                            range: file_gap,
                        })
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// Stretches of the template, in order, each with the file's layout to write in its place:
    /// the comma, and the layout round it, between a code site inside a bracket and an item the
    /// template writes beside it, where the file has a comma in the same place, between what the
    /// site captured and the code beside it.  The template's item stands where the file's did,
    /// so the line breaks round it are the file's.  Where the template has a comment there, or
    /// writes a site beside the site, [`gaps`](Job::gaps) decides instead.
    fn item_gaps(&self, marked: &Marked) -> Vec<(Range<usize>, Gap)> {
        let template = marked.indented.as_str();
        let in_bracket = |brackets: &[Bracket], gap: &Range<usize>| {
            brackets
                .iter()
                .any(|b| b.open < gap.start && gap.end <= b.close)
        };
        let markers: Vec<Range<usize>> = marked
            .at
            .iter()
            .map(|&(place, i)| place..place + marked.markers[i].len())
            .collect();
        let within = &self.found.range;
        let mut items = Vec::new();
        for (marker, &(_, i)) in markers.iter().zip(&marked.at) {
            let site = &self.rewrite.sites()[i];
            if site.in_literal {
                continue;
            }
            let capture = self.capture(&site.name);
            let after = (
                comma_after(template, marker.end, template.len(), &[]),
                comma_after(self.text, capture.end, within.end, &self.lexed.comments),
            );
            if let (Some(stretch), Some(file)) = after
                && !markers.iter().any(|m| m.start == stretch.end)
            {
                items.push((stretch, file));
            }
            let before = (
                comma_before(template, marker.start, 0, &[]),
                comma_before(self.text, capture.start, within.start, &self.lexed.comments),
            );
            if let (Some(stretch), Some(file)) = before
                && !markers.iter().any(|m| m.end == stretch.start)
            {
                items.push((stretch, file));
            }
        }
        items.retain(|(stretch, file)| {
            in_bracket(&marked.own.brackets, stretch)
                && in_bracket(&self.lexed.brackets, file)
                && !self
                    .found
                    .captures
                    .iter()
                    .any(|c| !c.is_empty() && c.start < file.end && file.start < c.end)
        });
        items.sort_unstable_by_key(|(stretch, _)| stretch.start);
        items.dedup_by_key(|(stretch, _)| stretch.start);
        items
            .into_iter()
            .map(|(stretch, file)| {
                let gap = Gap {
                    text: self.text[file.clone()].to_owned(),
                    range: file,
                };
                (stretch, gap)
            })
            .collect()
    }

    /// The template filled: each code site with its capture, moved to its depth, each literal
    /// site with `filler` if given, or else its capture escaped as its string needs, naming each
    /// escaped value's site in `escaped`; with where each code site was written.
    fn fill(
        &self,
        marked: &Marked,
        gaps: &Gaps,
        filler: Option<&str>,
        escaped: &mut Vec<String>,
    ) -> Result<(String, Vec<CodeSite>), String> {
        let mut out = String::with_capacity(marked.indented.len());
        let mut code = Vec::new();
        let mut copied = 0;
        for (k, &(place, i)) in marked.at.iter().enumerate() {
            let site = &self.rewrite.sites()[i];
            match &gaps.between[k] {
                Some(gap) => out.push_str(&gap.text),
                None => gaps.copy(&mut out, &marked.indented, copied..place),
            }
            let range = self.capture(&site.name);
            let value = if !site.in_literal {
                // On the template's first line, the line is the file's, up to the match, in the
                // block the match is in.
                let base = if out.contains(['\n', '\r']) {
                    line_indent(&out, out.len())
                } else {
                    self.base.to_owned()
                };
                moved(self.text, &range, &base, self.lexed)
            } else if let Some(filler) = filler {
                filler.to_owned()
            } else {
                let value = &self.text[range];
                let part = marked
                    .own
                    .strings
                    .iter()
                    .find(|part| part.range.start <= place && place < part.range.end);
                match part.map(|part| quaffed_python::escaped(value, part)) {
                    Some(Some(text)) => {
                        if text != value && !escaped.contains(&site.name) {
                            escaped.push(site.name.clone());
                        }
                        text
                    }
                    Some(None) => {
                        return Err(format!(
                            "what ${} captured, {value:?}, cannot be written into the string \
                             as text - a raw string cannot escape its own quote, a backslash \
                             before its closing quote or a line break on one line, and bytes \
                             hold ASCII only - so the edit would change more than the string's \
                             text: narrow the pattern so that such values do not match",
                            site.name
                        ));
                    }
                    // A comment: written as it is, for the structure check to refuse if it
                    // ends the comment.
                    None => value.to_owned(),
                }
            };
            if !site.in_literal {
                let sequence = site.written.len() > site.name.len() + 1
                    && self.rewrite.text()[site.written.clone()].ends_with("...");
                code.push((
                    out.len()..out.len() + value.len(),
                    sequence,
                    site.name.clone(),
                    i,
                ));
            }
            out.push_str(&value);
            copied = place + marked.markers[i].len();
        }
        gaps.copy(&mut out, &marked.indented, copied..marked.indented.len());
        Ok((out, code))
    }
}

/// The file's layout written between two sites in place of the template's: the stretch of the
/// file it comes from, and what is written.
struct Gap {
    range: Range<usize>,
    text: String,
}

/// The file's layout written in place of the template's: between each site and the one before
/// it, and, as stretches of the template, between a site and an item beside it.
struct Gaps {
    between: Vec<Option<Gap>>,
    items: Vec<(Range<usize>, Gap)>,
}

impl Gaps {
    /// `range` of the template onto `out`, with the file's layout in place of each stretch of it
    /// in `items`.
    fn copy(&self, out: &mut String, template: &str, range: Range<usize>) {
        let mut copied = range.start;
        for (stretch, gap) in &self.items {
            if range.start <= stretch.start && stretch.end <= range.end {
                out.push_str(&template[copied..stretch.start]);
                out.push_str(&gap.text);
                copied = stretch.end;
            }
        }
        out.push_str(&template[copied..range.end]);
    }
}

/// One match's rewrite, as [`render`] writes it.
struct Rendered {
    text: String,
    code: Vec<CodeSite>,
    /// The file's layout between holes, written in the template's place.
    carried: Vec<Range<usize>>,
    /// The names whose values were escaped to stay text in a string.
    escaped: Vec<String>,
}

/// Where a code site was written in a rewrite, whether it is a sequence, the name it writes, and
/// its index in the template's sites.
type CodeSite = (Range<usize>, bool, String, usize);

/// From `at` in `text`, the layout, one comma and the layout after it, up to the code that
/// follows, which must start before `ceiling` and be neither a closing bracket nor a comment:
/// the separator between an item ending at `at` and the next.  Comments are layout only where
/// listed in `comments`.
fn comma_after(
    text: &str,
    at: usize,
    ceiling: usize,
    comments: &[Range<usize>],
) -> Option<Range<usize>> {
    let comma = layout::forward_over_layout(text, at, ceiling, comments);
    if text.as_bytes().get(comma) != Some(&b',') {
        return None;
    }
    let next = layout::forward_over_layout(text, comma + 1, ceiling, comments);
    (next < ceiling && !matches!(text.as_bytes()[next], b')' | b']' | b'}' | b'#' | b','))
        .then_some(at..next)
}

/// Back from `at` in `text`, the layout, one comma and the layout before it, back to the code
/// before, which must end after `floor` and be neither an opening bracket nor another comma: the
/// separator between the item before and one starting at `at`.  Comments are layout only where
/// listed in `comments`.
fn comma_before(
    text: &str,
    at: usize,
    floor: usize,
    comments: &[Range<usize>],
) -> Option<Range<usize>> {
    let after_comma = back_over_layout(text, floor, at, comments);
    if after_comma <= floor || text.as_bytes()[after_comma - 1] != b',' {
        return None;
    }
    let previous = back_over_layout(text, floor, after_comma - 1, comments);
    (previous > floor && !matches!(text.as_bytes()[previous - 1], b'(' | b'[' | b'{' | b','))
        .then_some(previous..at)
}

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

/// The code `range` of `text` captured, moved to a line indented by `base`.  Only code that
/// starts a logical line - a statement, or a run of them - has a depth: each later line indented
/// at least as far as its first is shifted by the change, unless it starts inside a multi-line
/// string, whose value it is part of, or inside a bracket that opened before the capture.  Any
/// other capture continues a line, inside a bracket or after a backslash, so its indentation
/// says nothing of depth and it is written back as it was: a call's arguments one to a line, or
/// a value on the line after `x = \`, keep their layout wherever the rewrite puts them.
fn moved(text: &str, range: &Range<usize>, base: &str, lexed: &Lexed) -> String {
    let value = &text[range.clone()];
    // Only code that starts a logical line has a depth to move to: a value continued from the
    // line before, inside a bracket or after a backslash, keeps its lines as written.
    if !value.contains(['\n', '\r']) || lexed.logical_starts.binary_search(&range.start).is_err() {
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
        let continues = lexed
            .brackets
            .iter()
            .take_while(|b| b.open < range.start)
            .any(|b| at <= b.close);
        match line.strip_prefix(old.as_str()) {
            Some(rest) if i > 0 && !in_string && !continues => {
                out.push_str(base);
                out.push_str(rest);
            }
            _ => out.push_str(line),
        }
    }
    out
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
    use super::layout::ignored_by_matcher;
    use super::*;
    use quaffed_python::{compile_pattern, compile_rewrite};

    /// `text` with `pattern` replaced by `with`, and what was questioned, or why it was refused.
    fn edited(pattern: &str, with: &str, text: &str) -> Result<Option<Edited>, String> {
        let pattern = compile_pattern(pattern).expect("the pattern compiles");
        let rewrite = compile_rewrite(with, &pattern).expect("the rewrite compiles");
        // As a run does: once the rewrite is compiled.
        let pattern = name_every_hole(&pattern);
        let edited = replace(text, &pattern, None, &rewrite)
            .expect("the text parses")
            .outcome
            .map_err(|refused| {
                format!("{}:{}: {}", refused.at.line, refused.at.column, refused.why)
            })?;
        if let Some(edited) = &edited {
            // What it says it wrote over is all that changed: the text as it was, with each
            // stretch written over, is the new text.
            let mut rebuilt = String::new();
            let mut copied = 0;
            for (was, now) in &edited.over {
                rebuilt.push_str(&text[copied..was.start]);
                rebuilt.push_str(&edited.text[now.clone()]);
                copied = was.end;
            }
            rebuilt.push_str(&text[copied..]);
            assert_eq!(
                rebuilt, edited.text,
                "where the rewrite says it wrote, {:?}",
                edited.over
            );
        }
        Ok(edited)
    }

    /// `text` with `pattern` replaced by `with`, or why it was refused.
    fn replaced(pattern: &str, with: &str, text: &str) -> Result<Option<String>, String> {
        edited(pattern, with, text).map(|edited| edited.map(|edited| edited.text))
    }

    /// What replacing `pattern` by `with` in `text` wrote and questioned, where it was not refused.
    fn written(pattern: &str, with: &str, text: &str) -> Edited {
        edited(pattern, with, text)
            .unwrap_or_else(|why| panic!("{why}"))
            .expect("something matched")
    }

    fn ok(pattern: &str, with: &str, text: &str) -> String {
        replaced(pattern, with, text)
            .unwrap_or_else(|why| panic!("{why}"))
            .expect("something matched")
    }

    /// `text` with every match of `pattern` deleted, by a rewrite with no code.
    fn deleted(pattern: &str, text: &str) -> String {
        ok(pattern, "", text)
    }

    #[test]
    fn a_statement_alone_on_its_line_takes_the_line_and_its_comment() {
        assert_eq!(
            deleted(
                "print($x)",
                "import os\nprint(1)  # noqa\nx = 2\nif x:\n    print(x)\n    y = 3\n"
            ),
            "import os\nx = 2\nif x:\n    y = 3\n"
        );
        // Its own line ending, whatever it is; the line around it is left as it was.
        assert_eq!(
            deleted("import os", "a()\nimport os\r\nimport sys\r\n"),
            "a()\nimport sys\r\n"
        );
    }

    #[test]
    fn everything_inside_a_deleted_node_goes_with_it_but_not_the_comments_around_it() {
        assert_eq!(
            deleted(
                "if $c:\n    $b...\nelse:\n    $e...",
                "# above\nif x:  # head\n    a()\n    # inner\nelse:\n    b()  # tail\n# below\nz()\n"
            ),
            "# above\n# below\nz()\n"
        );
    }

    #[test]
    fn a_deleted_block_takes_the_comments_indented_into_it_after_its_last_line() {
        assert_eq!(
            deleted(
                "if $c:\n    $b...",
                "if x:\n    a()\n\n    # trailing\nz()\n"
            ),
            "z()\n"
        );
        // One at the block's own depth or less is not the block's.
        assert_eq!(
            deleted("if $c:\n    $b...", "if x:\n    a()\n# keep\nz()\n"),
            "# keep\nz()\n"
        );
    }

    #[test]
    fn a_deletion_with_blank_lines_either_side_takes_those_after_it() {
        let text = "def f():\n    pass\n\n\ndef g():\n    pass\n\n\ndef h():\n    pass\n";
        assert_eq!(
            deleted("def g():\n    pass", text),
            "def f():\n    pass\n\n\ndef h():\n    pass\n"
        );
        // At the start of the file, or its end, the blank lines at that edge go too.
        assert_eq!(
            deleted("def f():\n    pass", text),
            "def g():\n    pass\n\n\ndef h():\n    pass\n"
        );
        assert_eq!(
            deleted("def h():\n    pass", text),
            "def f():\n    pass\n\n\ndef g():\n    pass\n"
        );
        // Between code, the wider gap is left: a class's last method leaves the class's two.
        assert_eq!(
            deleted(
                "def g(self):\n    pass",
                "class A:\n    def f(self):\n        pass\n\n    def g(self):\n        pass\n\n\nx = 1\n"
            ),
            "class A:\n    def f(self):\n        pass\n\n\nx = 1\n"
        );
        // With blank lines on one side only, they stay.
        assert_eq!(deleted("b()", "a()\n\nb()\nc()\n"), "a()\n\nc()\n");
    }

    #[test]
    fn a_deletion_at_a_blocks_edge_takes_the_blank_lines_on_its_inner_side() {
        // The first statement takes the blank lines after it, which would open the block.
        assert_eq!(
            deleted("x = 1", "def f():\n    x = 1\n\n    y = 2\n"),
            "def f():\n    y = 2\n"
        );
        // The last takes those before it, and leaves those after, which part the block from
        // what follows.
        assert_eq!(
            deleted("x = 1", "def f():\n    a = 0\n\n    x = 1\n\n\nz = 3\n"),
            "def f():\n    a = 0\n\n\nz = 3\n"
        );
        assert_eq!(
            deleted("x = 1", "if c:\n    a = 0\n\n    x = 1\nelse:\n    b = 1\n"),
            "if c:\n    a = 0\nelse:\n    b = 1\n"
        );
        // A run of statements is judged by its first and its last.
        assert_eq!(
            deleted(
                "x = 1\ny = 2",
                "def f():\n    x = 1\n    y = 2\n\n    z = 3\n"
            ),
            "def f():\n    z = 3\n"
        );
        // Inside the block, the rule between code stands.
        assert_eq!(
            deleted("x = 1", "def f():\n    a = 0\n\n    x = 1\n\n    y = 2\n"),
            "def f():\n    a = 0\n\n    y = 2\n"
        );
    }

    #[test]
    fn a_decorator_takes_its_line() {
        assert_eq!(
            deleted(
                "@retry($n)",
                "class A:\n    @retry(3)  # why\n    @cache\n    def f(self):\n        pass\n"
            ),
            "class A:\n    @cache\n    def f(self):\n        pass\n"
        );
    }

    #[test]
    fn a_statement_sharing_its_line_takes_its_semicolon() {
        assert_eq!(deleted("a = 1", "a = 1; b = 2\n"), "b = 2\n");
        // The comment stays with the line, which stays.
        assert_eq!(deleted("b = 2", "a = 1; b = 2  # why\n"), "a = 1  # why\n");
        // Every statement on a line deleted takes the line, indentation and all.
        assert_eq!(
            deleted("$x = $y", "if t:\n    a = 1; b = 2\n    c()\n"),
            "if t:\n    c()\n"
        );
    }

    #[test]
    fn a_list_item_takes_one_comma_and_never_another_nodes() {
        assert_eq!(deleted("debug", "f(a, debug)\n"), "f(a)\n");
        assert_eq!(deleted("debug", "f(debug, a)\n"), "f(a)\n");
        // A comma across a line break, before or after.
        assert_eq!(deleted("debug", "f(a,\n  debug)\n"), "f(a)\n");
        assert_eq!(deleted("debug", "f(debug\n  , a)\n"), "f(a)\n");
        assert_eq!(
            deleted("debug", "f(\n    debug\n    , a)\n"),
            "f(\n    a)\n"
        );
        // A trailing comma stays as the list's layout.
        assert_eq!(deleted("debug", "x = [a, debug,]\n"), "x = [a,]\n");
        // Alone on its line, the item takes the line.
        assert_eq!(
            deleted("debug", "f(\n    a,\n    debug,  # remove\n)\n"),
            "f(\n    a,\n)\n"
        );
        // Runs of items, each taking one.
        assert_eq!(deleted("debug", "f(debug, debug)\n"), "f()\n");
        assert_eq!(deleted("debug", "f(debug, a, debug, debug)\n"), "f(a)\n");
        // With the brackets that only wrap it.
        assert_eq!(deleted("debug", "f(a, (debug))\n"), "f(a)\n");
        // A comma in a comment is no separator, and the comment line stays.
        assert_eq!(
            deleted(
                "\"debug\"",
                "x = [\n    \"a\",\n    # \"b\",\n    \"debug\"]\n"
            ),
            "x = [\n    \"a\",\n    # \"b\",\n    ]\n"
        );
        // A last line with no line break, ending in a character of more than one byte.
        assert_eq!(deleted("$x = $y", "a()\nx = caf\u{e9}"), "a()\n");
        // A tuple left with one item stays a tuple.
        assert_eq!(deleted("\"skip\"", "x = [(t, \"skip\")]\n"), "x = [(t,)]\n");
        assert_eq!(deleted("\"skip\"", "x = [(\"skip\", t)]\n"), "x = [(t,)]\n");
        assert_eq!(deleted("b", "x = a, b\nd[a, b]\n"), "x = a,\nd[a,]\n");
        assert_eq!(
            deleted("b", "x = (\n    a,\n    b\n)\n"),
            "x = (\n    a,\n)\n"
        );
        assert_eq!(
            deleted("a", "x = (\n    a,\n    b\n)\n"),
            "x = (\n    b,\n)\n"
        );
        assert_eq!(deleted("b", "x = (a, b, c)\n"), "x = (a, c)\n");
        // A list left with one item is still a list, so it needs no comma.
        assert_eq!(deleted("b", "x = [a, b]\n"), "x = [a]\n");
        // Anywhere but a statement or an item, a match that takes nothing is refused, since what
        // is left may parse and mean something else.
        for text in [
            "x = debug + a\n",
            "x = a - debug\n",
            "f(debug * args)\n",
            "x = not debug\n",
            "x = a and debug or b\n",
            "x = a < debug\n",
            "x = debug(1)\n",
            "x = debug[0]\n",
            "x = a[1:debug]\n",
            "x = {debug}\n",
            "def f():\n    return debug\n",
            "def f():\n    yield debug\n",
            "try:\n    pass\nexcept E:\n    raise debug\n",
        ] {
            let refused = replaced("debug", "", text).unwrap_err();
            assert!(
                refused.contains("mean something else"),
                "{text:?}: {refused}"
            );
        }
        // An item, the only one, simply goes.
        assert_eq!(
            deleted(
                "debug",
                "f(debug)\nx = [debug]\nclass A(debug):\n    pass\n"
            ),
            "f()\nx = []\nclass A():\n    pass\n"
        );
        // A comma the parent does not hold is never taken: this one is the call's, not the sum's,
        // so the match takes nothing, and is refused.  Taking the comma would have left
        // `g(a + c)`, which parses.
        assert!(
            replaced("b", "", "g(a + b, c)\n")
                .unwrap_err()
                .contains("mean something else")
        );
    }

    #[test]
    fn a_comment_rewrite_stands_in_place_of_the_code() {
        assert_eq!(
            ok(
                "print($x)",
                "# print($x)",
                "def f():\n    print(x)  # why\n    return\n"
            ),
            "def f():\n    # print(x)  # why\n    return\n"
        );
        assert_eq!(
            ok("debug", "# debug", "f(\n    a,\n    debug,\n)\n"),
            "f(\n    a,\n    # debug\n)\n"
        );
        // It takes no comma from another line, which would join it to that line.
        assert_eq!(
            ok("debug", "# debug", "f(\n    a,\n    debug\n)\n"),
            "f(\n    a,\n    # debug\n)\n"
        );
        // Where code follows on the line, the comment would swallow it.
        let refused = replaced("print($x)", "# print($x)", "print(x); y = 1\n").unwrap_err();
        assert!(
            refused.starts_with("1:1: the rewrite is a comment"),
            "{refused}"
        );
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
    fn a_capture_takes_the_brackets_that_only_wrap_it() {
        let text = "data = (\n    # header\n    b\"PK\"\n    + size\n)\n";
        let done = written("$x = $y", "$x = $y", text);
        assert_eq!((done.text.as_str(), done.questioned.len()), (text, 0));
        // So it keeps its meaning where the rewrite writes it, with nothing to question.
        let done = written("f($a)", "$a * 2", "y = f((x + 1))\n");
        assert_eq!(done.text, "y = (x + 1) * 2\n");
        assert_eq!(done.questioned, []);
        assert_eq!(
            ok("$f($x)", "$f($x)", "(lambda y: y)(2)\n"),
            "(lambda y: y)(2)\n"
        );
        // A call's own brackets are the call's, not a wrapping of its argument.
        assert_eq!(ok("f($a)", "g($a)", "f(x)\n"), "g(x)\n");
    }

    #[test]
    fn a_sequence_ending_in_wrapping_brackets_takes_both() {
        let text = "self.assertEqual(f(x), (name == '_'))\n";
        assert_eq!(
            ok(
                "self.assertEqual($args...)",
                "self.assertEqual($args...)",
                text
            ),
            text
        );
        let text = "self.assertEqual((a), b)\n";
        assert_eq!(
            ok("self.assertEqual($args...)", "check($args...)", text),
            "check((a), b)\n"
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
            "g(\n    x,\n    y,\n)\n"
        );
        // And left alone where the rewrite already has one, or where there was none.
        assert_eq!(ok("f($a, $b)", "g($a, $b,)", "f(x, y,)\n"), "g(x, y,)\n");
        assert_eq!(ok("f($a, $b)", "g($a, $b)", "f(x, y)\n"), "g(x, y)\n");
    }

    #[test]
    fn a_trailing_comma_is_carried_to_the_bracket_holding_the_same_holes() {
        assert_eq!(
            ok("f($a, g($b))", "h($a, k($b))", "r = f(x, g(\n    y,\n))\n"),
            "r = h(x, k(\n    y,\n))\n"
        );
        // In any order.
        assert_eq!(
            ok("f($a, $b)", "f($b, $a)", "f(\n    x,\n    y,\n)\n"),
            "f(\n    y, x,\n)\n"
        );
        // A hole the rewrite drops is not counted, so the call still holds what it did.
        assert_eq!(
            ok("f($a, $b)", "f($a)", "f(\n    x,\n    y,\n)\n"),
            "f(\n    x,\n)\n"
        );
        // Brackets holding no hole correspond by their order.
        assert_eq!(
            ok("f($a, [1, 2])", "g([1, 2], $a)", "f(x, [1, 2,])\n"),
            "g([1, 2,], x)\n"
        );
        // A bracket a capture wrote back is the capture's, written back as it was.
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
        // A bracket the rewrite leaves empty has nothing for a comma to trail.
        assert_eq!(ok("f(1, 2)", "g()", "f(1, 2,)\n"), "g()\n");
    }

    #[test]
    fn a_comma_that_is_not_ignored_is_the_patterns_to_match() {
        let text = "x = (a,)\ny = f(a,)\n";
        let arena = parse_module(text).unwrap();
        assert!(!ignored_by_matcher(&arena, text.find("a,)").unwrap() + 1));
        assert!(ignored_by_matcher(&arena, text.rfind("a,)").unwrap() + 1));
        // Every other kind of bracket the lexer lists: its trailing comma is layout to the
        // matcher, and so to the edit.
        let text = "l = [a,]\ns = {a,}\nd = {a: b,}\nv = x[a, b,]\nt = (a, b,)\n\
                    from m import (a,)\nclass C(B,):\n    def f(self, a,):\n        pass\n\
                    with (open(p), open(q),):\n    pass\nmatch v:\n    case [a,]:\n        \
                    pass\ndef g[T,](): pass\n";
        let arena = parse_module(text).unwrap();
        let ending: Vec<_> = lex(text)
            .brackets
            .into_iter()
            .filter_map(|b| b.trailing_comma.map(|comma| (comma, b)))
            .collect();
        assert_eq!(ending.len(), 11, "{ending:?}");
        for (comma, bracket) in ending {
            assert!(
                ignored_by_matcher(&arena, comma),
                "{:?}",
                &text[bracket.open..=bracket.close]
            );
        }
        // A one-element tuple's comma is the pattern's own, so the rewrite says what becomes of
        // it.
        assert_eq!(ok("f(($a,))", "g([$a])", "f((x,))\n"), "g([x])\n");
    }

    #[test]
    fn a_trailing_comma_goes_with_a_bracket_the_rewrite_deletes() {
        // Unwrapping a call deletes its brackets, and the comma that asked for its contents one
        // to a line goes with them.
        assert_eq!(ok("wrap($x)", "$x", "y = wrap(\n    a,\n)\n"), "y = a\n");
        // So does one split into two, and one only a dropped hole held.
        assert_eq!(
            ok(
                "grid[$i, $j]",
                "grid[$i][$j]",
                "v = grid[\n    i,\n    j,\n]\n"
            ),
            "v = grid[i][j]\n"
        );
        assert_eq!(ok("[$a, $b]", "$b", "v = [x, d[k],]\n"), "v = d[k]\n");
        assert_eq!(
            ok("f($a, g($b))", "h(k($a, $b))", "f(x, g(\n    y,\n))\n"),
            "h(k(x, y))\n"
        );
    }

    #[test]
    fn a_bracket_an_unnamed_hole_matched_is_the_holes() {
        // Dropped with what `$_` or `$...` matched, as a named hole's would be.
        assert_eq!(ok("f($_, $x)", "g($x)", "f([a,], b)\n"), "g(b)\n");
        // Not the pattern's, so not carried to a bracket of the rewrite's own that holds no hole
        // either.
        assert_eq!(ok("f($_, $x)", "g($x, [0])", "f([a,], b)\n"), "g(b, [0])\n");
        assert_eq!(
            ok(
                "f($a, $...)",
                "g($a)",
                "f(\n    x,\n    dict(\n        k=1,\n    ),\n)\n"
            ),
            // The call's own comma is carried; the one inside `dict(...)` went with it.
            "g(\n    x,\n)\n"
        );
    }

    #[test]
    fn a_trailing_comma_that_could_go_to_more_than_one_bracket_is_refused() {
        // Two brackets in the rewrite hold no hole, where the match has one.
        let why = replaced("f([1], $a)", "g([1], [2], $a)", "f([1,], x)\n")
            .expect_err("more than one counterpart");
        assert!(
            why.starts_with("1:5: the trailing comma before \"]\" is in the match"),
            "{why}"
        );
        assert!(
            why.contains("has 2 brackets holding no hole where the match has 1"),
            "{why}"
        );
    }

    #[test]
    fn a_trailing_comma_that_would_change_the_rewrites_meaning_is_refused() {
        // Brackets round an expression: a comma would make them a tuple.
        let why = replaced(
            "max($a, $b)",
            "($a if $a > $b else $b)",
            "m = max(\n    x,\n    y,\n)\n",
        )
        .expect_err("a tuple made");
        assert!(why.contains("it would change what the code means"), "{why}");
        assert!(
            replaced(
                "isinstance($x, $t)",
                "(type($x) is $t)",
                "if isinstance(\n    value,\n    int,\n):\n    pass\n"
            )
            .is_err()
        );
    }

    #[test]
    fn a_capture_python_would_read_with_its_surroundings_is_refused() {
        // Each place an operator, a sign, an attribute, a call or a subscript would take in part
        // of what the hole captured, with the rewrite that works.
        for (pattern, with, text, capture, works) in [
            ("dbl($a)", "$a * 2", "r = dbl(a + b)\n", "a + b", "($a) * 2"),
            ("dbl($a)", "$a ** 2", "r = dbl(-a)\n", "-a", "($a) ** 2"),
            // Python reads a leading minus as binding less tightly than `**`.
            ("dbl($a)", "$a ** 2", "r = dbl(-1)\n", "-1", "($a) ** 2"),
            (
                "dbl($a)",
                "$a.double()",
                "r = dbl(a + b)\n",
                "a + b",
                "($a).double()",
            ),
            ("neg($a)", "-$a", "r = neg(a - b)\n", "a - b", "-($a)"),
            (
                "one($f)",
                "$f()",
                "r = one(lambda: 0)\n",
                "lambda: 0",
                "($f)()",
            ),
            ("dbl($a)", "$a[0]", "r = dbl(a or b)\n", "a or b", "($a)[0]"),
        ] {
            let why = replaced(pattern, with, text).expect_err(with);
            assert!(
                why.starts_with(&format!(
                    "1:5: what ${} captured, {capture:?}, would not be code of its own",
                    &with[with.find('$').unwrap() + 1..][..1]
                )),
                "{why}"
            );
            assert!(why.ends_with(&format!("as in {works:?}")), "{why}");
            // And as the message says, it is written with the brackets, meaning what it says.
            let works = ok(pattern, works, text);
            assert!(works.contains(&format!("({capture})")), "{works}");
        }
        // A bare tuple written where one argument goes would be two.
        let why = replaced(
            "return $x",
            "return wrap($x)",
            "def f():\n    return a, b\n",
        )
        .expect_err("two arguments");
        assert!(why.starts_with("2:5: what $x captured, \"a, b\""), "{why}");
        assert!(why.ends_with("as in \"return wrap(($x))\""), "{why}");
        assert_eq!(
            ok(
                "return $x",
                "return wrap(($x))",
                "def f():\n    return a, b\n"
            ),
            "def f():\n    return wrap((a, b))\n"
        );
        // A sequence is a run of code, so it may stand as several, but only as siblings: here the
        // first would become part of `1 + a`.
        assert_eq!(ok("f($xs...)", "g($xs...)", "f(a, b)\n"), "g(a, b)\n");
        let why = replaced("f($xs...)", "f(1 + $xs...)", "f(a, b)\n").expect_err("regrouped");
        assert!(
            why.ends_with(
                "write the hole where its items stand apart from what is around it, as among a \
                 call's arguments, since brackets round it would make them one tuple"
            ),
            "{why}"
        );
        // Only the line of a longer template that holds the hole is shown, without its indent.
        let why = replaced("f($a)", "g(\n    $a * 2,\n)", "y = f(x + 1)\n").expect_err("regrouped");
        assert!(why.ends_with("as in \"($a) * 2,\""), "{why}");
        // Brackets that only wrap a capture are its own, but not those of the code inside them.
        let text = "mock_kalls = ([call(), call('foo')])\n";
        assert_eq!(ok("$x = $y", "$x = $y", text), text);
        assert_eq!(
            ok("f($a)", "$a * 2", "y = f(([a, b]))\n"),
            "y = ([a, b]) * 2\n"
        );
        // Nothing is said of a capture that is one piece of code wherever it goes - a name, a
        // call, a literal, or one in brackets of its own - and its text is written as it was.
        for (text, out) in [
            ("y = f(x)\n", "y = x * 2\n"),
            ("y = f(g(x))\n", "y = g(x) * 2\n"),
            ("y = f(1)\n", "y = 1 * 2\n"),
            ("y = f((x + 1))\n", "y = (x + 1) * 2\n"),
        ] {
            let done = written("f($a)", "$a * 2", text);
            assert_eq!((done.text.as_str(), done.questioned.len()), (out, 0));
        }
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
    fn a_match_inside_anothers_hole_is_rewritten_first() {
        // Two matches starting together, the outer holding the inner in its callee.
        assert_eq!(ok("$f($x)", "$f($x)", "a(b)(c)\n"), "a(b)(c)\n");
        assert_eq!(
            ok("$f($x)", "call($f, $x)", "a(b)(c)\n"),
            "call(call(a, b), c)\n"
        );
        // An outer call with two arguments is no match of `f($x)`; the calls in it are.
        assert_eq!(ok("f($x)", "g($x)", "f(f(1))\n"), "g(g(1))\n");
        assert_eq!(
            ok("f($x)", "g($x)", "y = f(f(f(1)), f(2))\n"),
            "y = f(g(g(1)), g(2))\n"
        );
        // A block unwrapped round one already unwrapped moves out again.
        assert_eq!(
            ok(
                "with $m:\n    $body...",
                "$body...",
                "def f():\n    with a:\n        with b:\n            x()\n        y()\n"
            ),
            "def f():\n    x()\n    y()\n"
        );
        // What the inner rewrite refuses is named where it was.
        let why = replaced("f($a)", "$a * 2", "y = f(f(x + 1))\n").expect_err("regrouped");
        assert!(why.starts_with("1:7: what $a captured, \"x + 1\""), "{why}");
    }

    #[test]
    fn matches_that_overlap_outside_each_others_holes_are_refused() {
        // Runs of statements tried from every start overlap without one being inside the other.
        let why = replaced("a()\na()", "b()", "a()\na()\na()\n").expect_err("overlapping runs");
        assert!(
            why.starts_with("2:1: this match overlaps the one at line 1, column 1"),
            "{why}"
        );
    }

    #[test]
    fn a_comment_at_a_brackets_edge_goes_with_the_bracket() {
        // Before the first item, and after the last; layout with no comment is the template's.
        assert_eq!(
            ok("f($a)", "g($a)", "f(  # first\n    x\n)\n"),
            "g(  # first\n    x\n)\n"
        );
        assert_eq!(
            ok("f($a)", "g($a)", "f(\n    x  # last\n)\n"),
            "g(\n    x  # last\n)\n"
        );
        assert_eq!(
            ok("f($a)", "g($a)", "f(\n    x,  # last\n)\n"),
            "g(\n    x,  # last\n)\n"
        );
        // So rewriting a call to itself changes nothing.
        let text = "self.assertEqual(  # inputs are iterators\n    fmean(a),\n    fmean(b))\n";
        assert_eq!(
            ok(
                "self.assertEqual($args...)",
                "self.assertEqual($args...)",
                text
            ),
            text
        );
        // A bracket the rewrite deletes cannot take a comment with it.
        let why = replaced("wrap($x)", "$x", "y = wrap(  # note\n    a\n)\n")
            .expect_err("a comment with nowhere to go");
        assert!(
            why.starts_with("1:12: the comment \"# note\" is in the match"),
            "{why}"
        );
        assert!(why.contains("is not in the rewrite"), "{why}");
        // Nor meet one the template wrote at the same edge.
        let why = replaced("f($a)", "g(  # mine\n    $a)", "f(  # theirs\n    x\n)\n")
            .expect_err("two comments at one edge");
        assert!(why.contains("which stands first cannot be told"), "{why}");
    }

    #[test]
    fn a_value_continued_from_the_line_before_keeps_its_lines() {
        // After a backslash the value's indentation says nothing of depth, so where the
        // template writes it on the line of `=`, its later lines stay as they were.
        let text = "expected = \\\n    [('a', 'b')] + \\\n    more\n";
        assert_eq!(
            ok("$x = $y", "$x = $y", text),
            "expected = [('a', 'b')] + \\\n    more\n"
        );
        let text = "x = \\\n    [1,\n     2]\n";
        assert_eq!(ok("$x = $y", "$x = $y", text), "x = [1,\n     2]\n");
    }

    #[test]
    fn layout_at_a_brackets_edge_is_the_files_where_the_template_writes_none() {
        let text = "self.assertEqual(\n    a,\n    b\n)\n";
        assert_eq!(
            ok(
                "self.assertEqual($args...)",
                "self.assertEqual($args...)",
                text
            ),
            text
        );
        assert_eq!(
            ok("self.assertEqual($args...)", "check($args...)", text),
            "check(\n    a,\n    b\n)\n"
        );
        // Where the template writes layout of its own there, it stands.
        assert_eq!(ok("f($a)", "g( $a )", "f(\n    x\n)\n"), "g( x )\n");
    }

    #[test]
    fn layout_between_two_holes_is_the_files_where_the_code_between_is_the_same() {
        // Inside a bracket a line break means nothing, so the file's stands, with its comment.
        assert_eq!(
            ok(
                "isinstance($x, $t)",
                "is_a($x, $t)",
                "isinstance(x,\n           T)\n"
            ),
            "is_a(x,\n           T)\n"
        );
        assert_eq!(
            ok("f($x, $y)", "g($x, $y)", "f(a,  # why\n  b)\n"),
            "g(a,  # why\n  b)\n"
        );
        // Reordered, there is no layout of the file's between them, so the template's stands.
        assert_eq!(ok("f($x, $y)", "g($y, $x)", "f(a,\n  b)\n"), "g(b, a)\n");
        // Nor where the code between differs.
        assert_eq!(ok("f($x, $y)", "g($x + $y)", "f(a,\n  b)\n"), "g(a + b)\n");
        // Whitespace inside a string between them is code, not layout: the template's stands.
        assert_eq!(
            ok(
                "fmt($a, \"%s:%s\", $b)",
                "fmt($a, \"%s: %s\", $b)",
                "fmt(x, \"%s:%s\", y)\n"
            ),
            "fmt(x, \"%s: %s\", y)\n"
        );
    }

    #[test]
    fn layout_between_a_hole_and_an_item_beside_it_is_the_files() {
        let text = "f(\n    1,\n    2,\n    3,\n)\n";
        assert_eq!(
            ok("f($a..., 3)", "f($a..., 4)", text),
            "f(\n    1,\n    2,\n    4,\n)\n"
        );
        assert_eq!(
            ok("f(1, $a...)", "f(0, $a...)", text),
            "f(\n    0,\n    2,\n    3,\n)\n"
        );
        // With a comment there, the file's stands with it.
        assert_eq!(
            ok("f($a, 3)", "g($a, 4)", "f(x,  # why\n  3)\n"),
            "g(x,  # why\n  4)\n"
        );
        // Where the template writes a comment there, its layout stands.
        assert_eq!(
            ok("f($a, 3)", "g($a,  # now\n  4)", "f(x,\n  3)\n"),
            "g(x,  # now\n  4)\n"
        );
        // Where the file has no item beside the capture, the template's layout stands.
        assert_eq!(
            ok("f($a)", "f($a, 4)", "f(\n    x\n)\n"),
            "f(\n    x, 4\n)\n"
        );
    }

    #[test]
    fn blank_lines_between_two_statement_holes_are_the_files() {
        let text = "class C:\n    def f(self):\n        pass\n\n    def g(self):\n        pass\n";
        assert_eq!(
            ok(
                "class C:\n    $a...\n    $b...",
                "class D:\n    $a...\n    $b...",
                text
            ),
            "class D:\n    def f(self):\n        pass\n\n    def g(self):\n        pass\n"
        );
        // Moved to another depth, the blank lines stand and the template's indentation is used.
        assert_eq!(
            ok(
                "def f():\n    $a...\n    $b...",
                "def f():\n    if c:\n        $a...\n        $b...",
                "def f():\n    x = 1\n\n\n    y = 2\n"
            ),
            "def f():\n    if c:\n        x = 1\n\n\n        y = 2\n"
        );
        // Where code stands between them in the template, its layout stands.
        assert_eq!(
            ok(
                "class C:\n    $a...\n    $b...",
                "class D:\n    $a...\n    z = 0\n    $b...",
                text
            ),
            "class D:\n    def f(self):\n        pass\n    z = 0\n    def g(self):\n        pass\n"
        );
    }

    #[test]
    fn a_comment_no_capture_writes_back_is_refused() {
        let why = replaced("f($x, $y)", "g($y, $x)", "f(a,  # why\n  b)\n")
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
    fn a_value_written_into_a_string_is_escaped_to_stay_text_and_questioned() {
        let done = written("f($v)", "\"pre $v\"", "f(\"\" + y + \"\")\n");
        assert_eq!(done.text, "\"pre \\\"\\\" + y + \\\"\\\"\"\n");
        let [questioned] = done.questioned.as_slice() else {
            panic!("{:?}", done.questioned);
        };
        assert!(
            questioned
                .why
                .starts_with("what $v captured, \"\\\"\\\" + y + \\\"\\\"\", holds"),
            "{}",
            questioned.why
        );
        // A line break in a one-line string, and braces in an f-string.
        assert_eq!(
            ok("f($v)", "\"pre $v\"", "f([1,\n   2])\n"),
            "\"pre [1,\\n   2]\"\n"
        );
        assert_eq!(
            ok("f($v)", "f\"value: $v\"", "f({y})\n"),
            "f\"value: {{y}}\"\n"
        );
        // Text that needs nothing escaped is written as it is, and questioned for nothing.
        let done = written("f($v)", "\"pre $v\"", "f('abc')\n");
        assert_eq!(
            (done.text.as_str(), done.questioned.len()),
            ("\"pre 'abc'\"\n", 0)
        );
    }

    #[test]
    fn a_value_a_string_cannot_hold_as_text_is_refused() {
        // A raw string cannot escape its own quote.
        let why = replaced("f($v)", "r\"pre $v\"", "f(\"a\")\n").expect_err("a raw string");
        assert!(
            why.starts_with(
                "1:1: what $v captured, \"\\\"a\\\"\", cannot be written into the string"
            ),
            "{why}"
        );
        // Nor can bytes hold other than ASCII.
        let why = replaced("f($v)", "b\"key-$v\"", "f(caf\u{e9})\n").expect_err("bytes");
        assert!(why.contains("cannot be written into the string"), "{why}");
        // A line break would end a comment.
        let why = replaced("f($v)", "g()  # was $v", "f([1,\n   2])\n").expect_err("a comment");
        assert!(why.contains("into a comment would end it"), "{why}");
    }

    #[test]
    fn code_that_no_longer_parses_between_levels_is_refused_where_it_was() {
        // The inner match's rewrite, `lambda 2: 0`, is not code the outer one can stand round.
        let why = replaced("f($x)", "lambda $x: 0", "y = f(f(2))\n").expect_err("between levels");
        assert!(
            why.starts_with("1:7: once the matches inside another match here are rewritten"),
            "{why}"
        );
    }

    /// A function indenting by two spaces, ahead of each case's own code, so the file has an
    /// indent unit to give a block on its header's line.
    const UNIT: &str = "def u():\n  pass\n";

    #[test]
    fn a_block_on_its_headers_line_gets_lines_of_its_own_for_several_statements() {
        // Under each header, both statements the rewrite writes, and the one after the match,
        // stay in the block; the line after it stays outside.
        for header in [
            "if x",
            "while x",
            "for i in r",
            "with x",
            "class C",
            "def f()",
        ] {
            let text = format!("{UNIT}{header}: a(); z()\ntail()\n");
            assert_eq!(
                ok("a()", "b()\nc()", &text),
                format!("{UNIT}{header}:\n  b()\n  c(); z()\ntail()\n"),
                "under {header:?}"
            );
        }
        let text = format!("{UNIT}if x:\n  pass\nelse: a(); z()\ntail()\n");
        assert_eq!(
            ok("a()", "b()\nc()", &text),
            format!("{UNIT}if x:\n  pass\nelse:\n  b()\n  c(); z()\ntail()\n")
        );
    }

    #[test]
    fn a_compound_statement_in_a_block_on_its_headers_line_gets_lines_and_ends_its_own() {
        // The template indents by four, the file by two: its block takes the file's unit.
        for header in [
            "if x",
            "while x",
            "for i in r",
            "with x",
            "class C",
            "def f()",
        ] {
            let text = format!("{UNIT}{header}: y(); a(); z()\ntail()\n");
            assert_eq!(
                ok("a()", "if c:\n    b()", &text),
                format!("{UNIT}{header}:\n  y()\n  if c:\n    b()\n  z()\ntail()\n"),
                "under {header:?}"
            );
        }
        let text = format!("{UNIT}if x:\n  pass\nelse: a(); z()\n");
        assert_eq!(
            ok("a()", "if c:\n    b()", &text),
            format!("{UNIT}if x:\n  pass\nelse:\n  if c:\n    b()\n  z()\n")
        );
    }

    #[test]
    fn a_header_continued_over_lines_indents_its_block_from_its_first() {
        let text = format!("{UNIT}if (x and\n        y): a()\n");
        assert_eq!(
            ok("a()", "b()\nc()", &text),
            format!("{UNIT}if (x and\n        y):\n  b()\n  c()\n")
        );
    }

    #[test]
    fn a_statement_after_the_match_stays_out_of_a_block_the_rewrite_ends_with() {
        // In an indented block too, not only one on its header's line.
        let text = "def f():\n    a(); z()\n";
        assert_eq!(
            ok("a()", "if c:\n    b()", text),
            "def f():\n    if c:\n        b()\n    z()\n"
        );
        assert_eq!(
            ok("a()", "if c: b()", text),
            "def f():\n    if c: b()\n    z()\n"
        );
        // A comment the rewrite ends with would swallow it.
        assert_eq!(
            ok("a()", "b()  # note", text),
            "def f():\n    b()  # note\n    z()\n"
        );
        // A rewrite that is one line of simple statements leaves the line as it is.
        assert_eq!(ok("a()", "b()", text), "def f():\n    b(); z()\n");
        assert_eq!(
            ok("a()", "b()\nc()", text),
            "def f():\n    b()\n    c(); z()\n"
        );
    }

    #[test]
    fn a_compound_statement_after_a_semicolon_starts_a_line_of_its_own() {
        let text = "def f():\n    y(); a()\n";
        assert_eq!(
            ok("a()", "if c:\n    b()", text),
            "def f():\n    y()\n    if c:\n        b()\n"
        );
    }

    #[test]
    fn matches_beside_each_other_share_the_layout() {
        let text = format!("{UNIT}if x: y(); a(); a(); z()\nelse: a()\n");
        assert_eq!(
            ok("a()", "b()\nc()", &text),
            format!("{UNIT}if x:\n  y(); b()\n  c(); b()\n  c(); z()\nelse:\n  b()\n  c()\n")
        );
        let text = "def f():\n    a(); a()\n";
        assert_eq!(
            ok("a()", "if c:\n    b()", text),
            "def f():\n    if c:\n        b()\n    if c:\n        b()\n"
        );
    }

    #[test]
    fn a_block_on_its_headers_line_is_refused_lines_where_nothing_indents_a_block() {
        let why = replaced("a()", "b()\nc()", "if x: a(); z()\ntail()\n").unwrap_err();
        assert!(
            why.starts_with("1:7: the match is in a block written on its header's line"),
            "{why}"
        );
        // The rewrite's own indentation is a unit to use.
        assert_eq!(
            ok("a()", "if c:\n    b()", "if x: a()\n"),
            "if x:\n    if c:\n        b()\n"
        );
        // One line of simple statements needs none.
        assert_eq!(ok("a()", "b(); c()", "if x: a()\n"), "if x: b(); c()\n");
    }

    #[test]
    fn statements_a_template_writes_after_a_colon_get_lines_of_their_own() {
        let text = "def f(p):\n  x = 1\n  return x\n";
        assert_eq!(
            ok("def f($p...): $b...", "def g($p...): $b...", text),
            "def g(p):\n  x = 1\n  return x\n"
        );
        // A compound statement after a semicolon there opens the whole block.
        let text = "def f():\n  if a:\n    b()\n";
        assert_eq!(
            ok("def f(): $b...", "def f(): x = 1; $b...", text),
            "def f():\n  x = 1\n  if a:\n    b()\n"
        );
        // In a block of the template's own, it starts a line of its own.
        assert_eq!(
            ok("def f(): $b...", "def f():\n    x = 1; $b...", text),
            "def f():\n  x = 1\n  if a:\n    b()\n"
        );
        // Captured from a one-line body, they fit on the template's line.
        assert_eq!(
            ok(
                "def f($p...): $b...",
                "def g($p...): $b...",
                "def f(p): x = 1; return x\n"
            ),
            "def g(p): x = 1; return x\n"
        );
    }

    #[test]
    fn an_edit_leaving_a_statement_outside_what_it_needs_is_refused() {
        for (text, what, needs) in [
            ("def f():\n    return 1\n", "return", "a function"),
            ("def f():\n    yield 1\n", "yield", "a function"),
            (
                "async def f():\n    await x\n",
                "await",
                "an async function",
            ),
        ] {
            let pattern = if what == "await" {
                "async def f(): $b..."
            } else {
                "def f(): $b..."
            };
            let why = replaced(pattern, "$b...", text).unwrap_err();
            assert_eq!(
                why,
                format!(
                    "1:1: the rewrite would leave `{what}` outside {needs}, which Python parses \
                     but refuses to run, so the file would no longer compile: keep `{what}` \
                     inside {needs} in the rewrite, or narrow the pattern so that it does not \
                     match here"
                )
            );
        }
        let text = "for i in r:\n    if x:\n        break\n    continue\n";
        let why = replaced("for $i in $r: $b...", "$b...", text).unwrap_err();
        assert!(
            why.starts_with("1:1: the rewrite would leave `break` outside a loop"),
            "{why}"
        );
        // Moved but still inside what it needs, it is accepted.
        assert_eq!(
            ok(
                "def f(): $b...",
                "def g():\n    $b...",
                "def f():\n    return 1\n"
            ),
            "def g():\n    return 1\n"
        );
    }

    #[test]
    fn a_statement_already_outside_what_it_needs_is_the_files_not_the_edits() {
        // A top-level await, as a notebook's exported script has, carried through a hole.
        assert_eq!(
            ok("print($x)", "log($x)", "print(await x)\n"),
            "log(await x)\n"
        );
    }

    #[test]
    fn a_statement_the_layout_cannot_keep_in_its_block_is_refused() {
        // After a backslash, the block on its header's line has no colon before it to lay out
        // from, so the rewrite's second line would leave the block.
        let text = format!("{UNIT}if x: \\\n    a(); z()\n");
        let why = replaced("a()", "b()\nc()", &text).unwrap_err();
        assert_eq!(why, format!("4:5: {LEFT_BLOCK}"));
    }

    #[test]
    fn a_block_with_no_space_after_its_headers_colon_is_laid_out() {
        assert_eq!(
            ok("a()", "b()\nc()", &format!("{UNIT}if x:a()\n")),
            format!("{UNIT}if x:\n  b()\n  c()\n")
        );
        assert_eq!(
            ok("a()", "if c:\n    b()", "if x:a(); z()\n"),
            "if x:\n    if c:\n        b()\n    z()\n"
        );
    }

    #[test]
    fn a_templates_own_block_that_nothing_indents_is_refused_with_its_own_reason() {
        let text = "x = 1\ny = 2\nv = 3\nw = 3\n";
        let why = replaced("x = 1\n$rest...\nw = 3", "if c: $rest...\nw = 3", text).unwrap_err();
        assert!(
            why.starts_with(
                "1:1: what $rest captured needs lines of its own where the rewrite writes it, \
                 in a block of the rewrite's written on its header's line"
            ),
            "{why}"
        );
    }

    #[test]
    fn async_with_and_for_left_outside_an_async_function_are_refused() {
        let text = "async def main():\n    async with lock:\n        count += 1\n";
        let why = replaced("async def main(): $b...", "$b...", text).unwrap_err();
        assert!(
            why.starts_with("1:1: the rewrite would leave `async with` outside an async function"),
            "{why}"
        );
        let text = "async def main():\n    async for a in b:\n        pass\n";
        let why = replaced("async def main(): $b...", "def main():\n    $b...", text).unwrap_err();
        assert!(
            why.starts_with("1:1: the rewrite would leave `async for` outside an async function"),
            "{why}"
        );
    }

    #[test]
    fn an_await_in_a_generator_expression_needs_no_async_function() {
        let text = "async def f():\n    return (await g(x) for x in y)\n";
        assert_eq!(
            ok("async def f(): $b...", "def f():\n    $b...", text),
            "def f():\n    return (await g(x) for x in y)\n"
        );
    }
}
