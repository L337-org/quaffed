// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A structural `replace` in one Python file's text: each match written over with its rewrite.
//!
//! A rewrite is built from two sources only.  A hole writes back the source its name captured,
//! exactly as written; everything else comes from the template, exactly as written.  What is
//! derived rather than written: the indentation of a template's later lines and of captured code
//! moved to another depth, the file's own line ending for the template's line breaks, and the
//! trailing commas a pattern matches whether or not they are there.  A pattern ignores comments
//! and trailing commas, so a template cannot say what becomes of them: each must survive the
//! edit, or the edit is refused.  So is one that rewrites overlapping matches, or changes a
//! literal's structure by interpolating into it; nothing in the file is changed.
//!
//! A capture written where Python reads it together with what surrounds it - `x + 1` into
//! `$a * 2` - is written as the template says, and the edit is questioned rather than refused:
//! the template is what was asked for, and a question can be answered by a further edit.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use quaffed_python::{Lexed, ParseError, Rewrite, lex, parse_module, token_kinds};
use quaffed_syntax::{Arena, Captured, NodeId, Pattern, find_captured, find_where_captured};

use crate::search::{Match, Position};

mod layout;

use crate::structural::{Found, Unparsed, Where, last_char, positions, unparsed};
use layout::{After, Before, WrittenSite};

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
    /// Each place the rewrite was written as asked but may have changed what the code means.
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
            text: text[m.range.clone()].to_owned(),
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
    let captured = &whole_brackets(text, captured);
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
        let done =
            rewritten(&current, level_arena, &level, names, rewrite).map_err(|(offset, why)| {
                Refused {
                    at: at(backward(&maps, offset)),
                    why,
                }
            })?;
        questioned.extend(done.questioned.into_iter().map(|(offset, why)| Questioned {
            at: at(backward(&maps, offset)),
            why,
        }));
        maps.push(done.edits);
        current = done.text;
    }
    questioned.sort_by_key(|q| (q.at.line, q.at.column));
    Ok(Edited {
        text: current,
        questioned,
    })
}

/// `captured` with each capture widened over any bracket it holds one end of.  A sequence
/// hole captures from its first node's start to its last's end, and a node's range leaves out
/// brackets that only wrap it, so a stretch ending in `(name == '_')` would hold the opening
/// bracket but not the closing one: written back, it would not be code.  Only such wrapping
/// brackets can be cut, since every other bracket belongs to a node.
fn whole_brackets(text: &str, captured: &[Captured]) -> Vec<Captured> {
    let brackets = lex(text).brackets;
    captured
        .iter()
        .map(|found| Captured {
            range: found.range.clone(),
            captures: found
                .captures
                .iter()
                .map(|capture| {
                    let mut capture = capture.clone();
                    loop {
                        let cut = brackets.iter().find(|b| {
                            let open = capture.contains(&b.open);
                            let close = capture.contains(&b.close);
                            open != close
                                && found.range.start <= b.open
                                && b.close < found.range.end
                        });
                        let Some(cut) = cut else { break };
                        capture = capture.start.min(cut.open)..capture.end.max(cut.close + 1);
                    }
                    capture
                })
                .collect(),
        })
        .collect()
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
    captured: &[Captured],
    names: &[&str],
    rewrite: &Rewrite,
) -> Result<Level, (usize, String)> {
    let lexed = lex(text);
    let before = Before {
        text,
        arena,
        lexed: &lexed,
        captured,
        names,
    };
    // A comment at the edge of one of the match's own brackets is carried with it, or refused,
    // once the rewrite is written; any other the rewrite does not write back is refused here.
    let edges = layout::edge_comments(&before);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    // Where each match's rewrite, and each code site in it, was written in `out`: checked once
    // the whole file is written.
    let mut written: Vec<Range<usize>> = Vec::with_capacity(captured.len());
    let mut written_sites: Vec<WrittenSite> = Vec::new();
    for (index, found) in captured.iter().enumerate() {
        if let Some(comment) = lexed.comments.iter().find(|comment| {
            found.range.start <= comment.start
                && comment.end <= found.range.end
                && !edges.contains(comment)
                && !found
                    .captures
                    .iter()
                    .take(rewrite_names(rewrite, names))
                    .any(|capture| capture.start <= comment.start && comment.end <= capture.end)
        }) {
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
        let (rendered, sites) =
            render(text, found, names, rewrite, &lexed).map_err(|why| (found.range.start, why))?;
        out.push_str(&text[copied..found.range.start]);
        let base = out.len();
        for (range, sequence, name) in sites {
            written_sites.push(WrittenSite {
                range: base + range.start..base + range.end,
                found: index,
                sequence,
                name,
            });
        }
        out.push_str(&rendered);
        written.push(base..out.len());
        copied = found.range.end;
    }
    out.push_str(&text[copied..]);
    let edits = |written: &[Range<usize>]| -> Vec<(Range<usize>, Range<usize>)> {
        captured
            .iter()
            .zip(written)
            .map(|(found, new)| (found.range.clone(), new.clone()))
            .collect()
    };
    // A file that no longer parses is left to the parse check, which refuses it.
    let Ok(out_arena) = parse_module(&out) else {
        return Ok(Level {
            edits: edits(&written),
            text: out,
            questioned: Vec::new(),
        });
    };
    let after = After {
        text: &out,
        arena: &out_arena,
        written: &written,
        sites: &written_sites,
    };
    Ok(match layout::carry(&before, &after)? {
        Some(carried) => {
            let arena =
                parse_module(&carried.text).expect("layout is carried only where it parses");
            Level {
                questioned: questioned(captured, &carried.text, &arena, &carried.sites),
                edits: edits(&carried.written),
                text: carried.text,
            }
        }
        None => Level {
            questioned: questioned(captured, &out, &out_arena, &written_sites),
            edits: edits(&written),
            text: out,
        },
    })
}

/// Each capture in `out`, the file's new text parsed as `arena`, written where Python reads it
/// with what surrounds it - `x + 1` into `$a * 2` - so that its meaning may have changed: each
/// code site should still be code of its own, one node, or for a sequence a run of them.  Each
/// is named at its match's start in the text the matches were found in.
fn questioned(
    captured: &[Captured],
    out: &str,
    arena: &Arena<'_>,
    sites: &[WrittenSite],
) -> Vec<(usize, String)> {
    let code = CodeIndex::of(arena);
    sites
        .iter()
        .filter(|site| !(code.whole(&site.range) || (site.sequence && code.run(&site.range))))
        .map(|site| {
            (
                captured[site.found].range.start,
                format!(
                    "what ${} captured, {:?}, is not code of its own where the rewrite writes \
                     it - Python reads it together with what stands around it - so its meaning \
                     may have changed: if that is not what was meant, put brackets round the \
                     hole in the rewrite, as in (${})",
                    site.name,
                    &out[site.range.clone()],
                    site.name
                ),
            )
        })
        .collect()
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
    ranges: HashSet<(usize, usize)>,
    starting: HashMap<usize, Vec<Option<NodeId>>>,
    ending: HashMap<usize, Vec<Option<NodeId>>>,
}

impl CodeIndex {
    fn of(arena: &Arena<'_>) -> CodeIndex {
        let mut index = CodeIndex {
            ranges: HashSet::new(),
            starting: HashMap::new(),
            ending: HashMap::new(),
        };
        let mut walk = vec![(arena.root(), None)];
        while let Some((node, parent)) = walk.pop() {
            let range = arena.range(node);
            index.ranges.insert((range.start, range.end));
            index.starting.entry(range.start).or_default().push(parent);
            index.ending.entry(range.end).or_default().push(parent);
            walk.extend(arena.children(node).map(|child| (child, Some(node))));
        }
        index
    }

    /// Whether `range` is exactly one node.
    fn whole(&self, range: &Range<usize>) -> bool {
        self.ranges.contains(&(range.start, range.end))
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
                code.push((
                    out.len()..out.len() + value.len(),
                    sequence,
                    site.name.clone(),
                ));
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

/// Where a code site was written in a rewrite, whether it is a sequence, and the name it writes.
type CodeSite = (Range<usize>, bool, String);

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
/// unless it starts inside a multi-line string, whose value it is part of, or inside a bracket
/// that opened before the capture.  Such a line continues code the capture is only part of, so
/// its indentation says nothing of depth, and it is left as written: a call's arguments one to
/// a line keep their indentation when the call is rewritten round them.
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
        replace(text, &pattern, None, &rewrite)
            .expect("the text parses")
            .outcome
            .map_err(|refused| {
                format!("{}:{}: {}", refused.at.line, refused.at.column, refused.why)
            })
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
            "g(x, y,)\n"
        );
        // And left alone where the rewrite already has one, or where there was none.
        assert_eq!(ok("f($a, $b)", "g($a, $b,)", "f(x, y,)\n"), "g(x, y,)\n");
        assert_eq!(ok("f($a, $b)", "g($a, $b)", "f(x, y)\n"), "g(x, y)\n");
    }

    #[test]
    fn a_trailing_comma_is_carried_to_the_bracket_holding_the_same_holes() {
        assert_eq!(
            ok("f($a, g($b))", "h($a, k($b))", "r = f(x, g(\n    y,\n))\n"),
            "r = h(x, k(y,))\n"
        );
        // In any order.
        assert_eq!(
            ok("f($a, $b)", "f($b, $a)", "f(\n    x,\n    y,\n)\n"),
            "f(y, x,)\n"
        );
        // A hole the rewrite drops is not counted, so the call still holds what it did.
        assert_eq!(
            ok("f($a, $b)", "f($a)", "f(\n    x,\n    y,\n)\n"),
            "f(x,)\n"
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
            "g(x,)\n"
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
    fn a_capture_python_would_read_with_its_surroundings_is_written_and_questioned() {
        let done = written("f($a)", "$a * 2", "y = f(x + 1)\n");
        assert_eq!(done.text, "y = x + 1 * 2\n");
        let [questioned] = done.questioned.as_slice() else {
            panic!("{:?}", done.questioned);
        };
        assert_eq!((questioned.at.line, questioned.at.column), (1, 5));
        assert!(
            questioned
                .why
                .starts_with("what $a captured, \"x + 1\", is not code of its own"),
            "{}",
            questioned.why
        );
        // Brackets in the rewrite keep it whole.
        let done = written("f($a)", "($a) * 2", "y = f(x + 1)\n");
        assert_eq!(done.text, "y = (x + 1) * 2\n");
        assert_eq!(done.questioned, []);
        // A tuple's own brackets are not part of it, so it becomes two arguments.
        let done = written(
            "return $x",
            "return wrap($x)",
            "def f():\n    return a, b\n",
        );
        assert_eq!(done.text, "def f():\n    return wrap(a, b)\n");
        assert_eq!(done.questioned.len(), 1);
        // A sequence is a run of code, so it may stand as several.
        let done = written("f($xs...)", "g($xs...)", "f(a, b)\n");
        assert_eq!(
            (done.text.as_str(), done.questioned.len()),
            ("g(a, b)\n", 0)
        );
        // But only as siblings: here the first becomes part of `1 + a`.
        let done = written("f($xs...)", "f(1 + $xs...)", "f(a, b)\n");
        assert_eq!(
            (done.text.as_str(), done.questioned.len()),
            ("f(1 + a, b)\n", 1)
        );
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
        // What the inner rewrite questions is named where it was.
        let done = written("f($a)", "$a * 2", "y = f(f(x + 1))\n");
        assert_eq!(done.text, "y = x + 1 * 2 * 2\n");
        let at: Vec<(usize, usize)> = done
            .questioned
            .iter()
            .map(|q| (q.at.line, q.at.column))
            .collect();
        assert_eq!(at, [(1, 5), (1, 7)], "{:?}", done.questioned);
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
            "g(  # first\n    x)\n"
        );
        assert_eq!(
            ok("f($a)", "g($a)", "f(\n    x  # last\n)\n"),
            "g(x  # last\n)\n"
        );
        assert_eq!(
            ok("f($a)", "g($a)", "f(\n    x,  # last\n)\n"),
            "g(x,  # last\n)\n"
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
