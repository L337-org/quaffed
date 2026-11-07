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

use quaffed_python::{Bracket, Lexed, Rewrite, lex, parse_module, token_kinds};
use quaffed_syntax::{Arena, Captured, NodeId, Pattern, find_captured, find_where_captured};

use crate::search::{Match, Position};
use crate::structural::{Found, Unparsed, Where, last_char, positions, unparsed};

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
    // outside every hole is the pattern's own.
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
        rewritten(text, &arena, &captured, &names, rewrite).map(Some)
    };
    Ok(Replaced { found, outcome })
}

/// `text`, parsed as `arena`, with every match in `captured` written over, each capture in the
/// order of `names`.
fn rewritten(
    text: &str,
    arena: &Arena<'_>,
    captured: &[Captured],
    names: &[&str],
    rewrite: &Rewrite,
) -> Result<Edited, Refused> {
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
    // Where each match's rewrite, and each code site in it, was written in `out`: checked once
    // the whole file is written.
    let mut written: Vec<Range<usize>> = Vec::with_capacity(captured.len());
    let mut written_sites: Vec<WrittenSite> = Vec::new();
    for (index, found) in captured.iter().enumerate() {
        if let Some(comment) = lexed.comments.iter().find(|comment| {
            found.range.start <= comment.start
                && comment.end <= found.range.end
                && !found
                    .captures
                    .iter()
                    .take(rewrite_names(rewrite, names))
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
        let (rendered, sites) = render(text, found, names, rewrite, &lexed)
            .map_err(|why| refused(found.range.start, why))?;
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
    // A file that no longer parses is left to the parse check, which refuses it.
    let Ok(out_arena) = parse_module(&out) else {
        return Ok(Edited {
            text: out,
            questioned: Vec::new(),
        });
    };
    let before = Before {
        text,
        arena,
        lexed: &lexed,
        captured,
        names,
    };
    let after = After {
        text: &out,
        arena: &out_arena,
        written: &written,
        sites: &written_sites,
    };
    let carried = carry_trailing_commas(&before, &after).map_err(|(at, why)| refused(at, why))?;
    let (out, written_sites) = match carried {
        Some(carried) => carried,
        None => (out, written_sites),
    };
    let questioned = questioned(text, captured, &out, &written_sites);
    Ok(Edited {
        text: out,
        questioned,
    })
}

/// Each capture in `out`, the file's new text, written where Python reads it with what surrounds
/// it - `x + 1` into `$a * 2` - so that its meaning may have changed: each code site should still
/// be code of its own, one node, or for a sequence a run of them.
fn questioned(
    text: &str,
    captured: &[Captured],
    out: &str,
    sites: &[WrittenSite],
) -> Vec<Questioned> {
    let arena = parse_module(out).expect("a carried comma is kept only where it parses");
    let code = CodeIndex::of(&arena);
    sites
        .iter()
        .filter(|site| !(code.whole(&site.range) || (site.sequence && code.run(&site.range))))
        .map(|site| {
            let at = captured[site.found].range.start;
            Questioned {
                at: positions(text, &[at])[&at],
                why: format!(
                    "what ${} captured, {:?}, is not code of its own where the rewrite writes \
                     it - Python reads it together with what stands around it - so its meaning \
                     may have changed: if that is not what was meant, put brackets round the \
                     hole in the rewrite, as in (${})",
                    site.name,
                    &out[site.range.clone()],
                    site.name
                ),
            }
        })
        .collect()
}

/// The file as it was: its text, tree and lexing, each match with what it captured, and the
/// names the captures are in the order of.
struct Before<'a> {
    text: &'a str,
    arena: &'a Arena<'a>,
    lexed: &'a Lexed,
    captured: &'a [Captured],
    names: &'a [&'a str],
}

/// The file once each match is rewritten, before any trailing comma is carried: its text and
/// tree, where each match's rewrite was written, and each code site in it.
struct After<'a> {
    text: &'a str,
    arena: &'a Arena<'a>,
    written: &'a [Range<usize>],
    sites: &'a [WrittenSite],
}

/// How many of `names` the rewrite writes: they come first, the pattern's other names after.
fn rewrite_names(rewrite: &Rewrite, names: &[&str]) -> usize {
    names
        .iter()
        .take_while(|name| rewrite.sites().iter().any(|site| site.name == **name))
        .count()
}

/// Where a code site was written in the file's new text, the match it belongs to, whether it is
/// a sequence, and the name it writes.
#[derive(Debug, Clone)]
struct WrittenSite {
    range: Range<usize>,
    found: usize,
    sequence: bool,
    name: String,
}

/// The new text with each trailing comma a match had inside a bracket of the pattern's own
/// carried to the bracket in the rewrite that corresponds to it, and the code sites moved to
/// suit; `None` if there were none to carry.  A comma that would change what the code means
/// there refuses the edit, with where it is in the old text and why.
fn carry_trailing_commas(
    before: &Before<'_>,
    after: &After<'_>,
) -> Result<Option<(String, Vec<WrittenSite>)>, CommaRefused> {
    let mut insertions = commas_to_carry(before, after)?;
    if insertions.is_empty() {
        return Ok(None);
    }
    insertions.sort_unstable_by_key(|insertion| insertion.at);
    let out = after.text;
    let with = |chosen: &[&Insertion]| {
        let mut carried = String::with_capacity(out.len() + chosen.len());
        let mut copied = 0;
        for &&Insertion { at, .. } in chosen {
            carried.push_str(&out[copied..at]);
            carried.push(',');
            copied = at;
        }
        carried.push_str(&out[copied..]);
        carried
    };
    let same_code = |carried: &str| {
        parse_module(carried).is_ok_and(|parsed| kinds(&parsed) == kinds(after.arena))
    };
    let all: Vec<&Insertion> = insertions.iter().collect();
    let carried = with(&all);
    if !same_code(&carried) {
        let culprit = insertions
            .iter()
            .find(|insertion| !same_code(&with(&[*insertion])))
            .unwrap_or(&insertions[0]);
        return Err((
            culprit.comma,
            format!(
                "the trailing comma before {:?} is in the match but no capture holds it, and in \
                 the rewrite's bracket holding {} it would change what the code means, so it can \
                 be neither kept nor dropped: narrow the pattern so that it does not match here, \
                 or take the comma out first",
                culprit.close, culprit.holding
            ),
        ));
    }
    let shifted = |offset: usize| offset + insertions.iter().filter(|i| i.at < offset).count();
    let sites = after
        .sites
        .iter()
        .map(|site| WrittenSite {
            range: shifted(site.range.start)..shifted(site.range.end),
            ..site.clone()
        })
        .collect();
    Ok(Some((carried, sites)))
}

/// Where in the new text each trailing comma the pattern ignored goes: before the closing bracket
/// of the rewrite's bracket that corresponds to the comma's.  Brackets correspond when they hold
/// the same holes written as code, in any order, and are as many on each side; brackets holding
/// none correspond by their order.  A comma whose bracket has no one counterpart refuses the
/// edit, with where it is in the old text and why.
fn commas_to_carry(before: &Before<'_>, after: &After<'_>) -> Result<Vec<Insertion>, CommaRefused> {
    // Each match's own brackets: not inside any capture, whose brackets are the capture's.
    let own: Vec<Vec<&Bracket>> = before
        .captured
        .iter()
        .map(|found| {
            before
                .lexed
                .brackets
                .iter()
                .filter(|b| inside(b, &found.range))
                .filter(|b| !found.captures.iter().any(|capture| inside(b, capture)))
                .collect()
        })
        .collect();
    let commas: Vec<usize> = own
        .iter()
        .flatten()
        .filter_map(|b| b.trailing_comma)
        .collect();
    if commas.is_empty() {
        return Ok(Vec::new());
    }
    let ignored = ignored_commas(before.text, before.arena, &commas);
    let out_lexed = lex(after.text);
    let mut insertions = Vec::new();
    for (index, found) in before.captured.iter().enumerate() {
        let sites: Vec<&WrittenSite> = after
            .sites
            .iter()
            .filter(|site| site.found == index)
            .collect();
        let mut captures: Vec<(usize, &str)> = before
            .names
            .iter()
            .zip(&found.captures)
            .filter(|(name, _)| sites.iter().any(|site| site.name == **name))
            .map(|(name, capture)| (capture.start, *name))
            .collect();
        captures.sort_unstable();
        let rewrite_brackets: Vec<&Bracket> = out_lexed
            .brackets
            .iter()
            .filter(|b| inside(b, &after.written[index]))
            .filter(|b| !sites.iter().any(|site| inside(b, &site.range)))
            .collect();
        let file_held: Vec<Vec<&str>> = own[index]
            .iter()
            .map(|b| held_in(b, captures.iter().copied()))
            .collect();
        let rewrite_held: Vec<Vec<&str>> = rewrite_brackets
            .iter()
            .map(|b| {
                held_in(
                    b,
                    sites
                        .iter()
                        .map(|site| (site.range.start, site.name.as_str())),
                )
            })
            .collect();
        for (i, bracket) in own[index].iter().enumerate() {
            let Some(comma) = bracket.trailing_comma.filter(|c| ignored.contains(c)) else {
                continue;
            };
            let held = &file_held[i];
            let same: Vec<usize> = (0..own[index].len())
                .filter(|j| file_held[*j] == *held)
                .collect();
            let counterparts: Vec<&Bracket> = rewrite_brackets
                .iter()
                .zip(&rewrite_held)
                .filter(|(_, h)| *h == held)
                .map(|(b, _)| *b)
                .collect();
            let holding = holding(held);
            let close = &before.text[bracket.close..=bracket.close];
            if counterparts.len() != same.len() {
                return Err((
                    comma,
                    no_counterpart(close, &holding, counterparts.len(), same.len()),
                ));
            }
            let partner = counterparts[same.iter().position(|j| *j == i).expect("one of them")];
            let at = back_over_layout(
                after.text,
                partner.open + 1,
                partner.close,
                &out_lexed.comments,
            );
            // A bracket the rewrite leaves empty has nothing for a comma to trail.
            if partner.trailing_comma.is_none() && at > partner.open + 1 {
                insertions.push(Insertion {
                    at,
                    comma,
                    close: close.to_owned(),
                    holding,
                });
            }
        }
    }
    Ok(insertions)
}

/// Why a trailing comma before `close`, whose bracket holds `holding`, has no one bracket to go
/// to: the rewrite has `found` brackets holding the same, where the match has `wanted`.
fn no_counterpart(close: &str, holding: &str, found: usize, wanted: usize) -> String {
    format!(
        "the trailing comma before {close:?} is in the match but no capture holds it, and the \
         rewrite has {found} bracket{} holding {holding} where the match has {wanted}, so it \
         cannot be told where the comma belongs, and dropping it would change more than was \
         asked: narrow the pattern so that it does not match here, or take the comma out first",
        if found == 1 { "" } else { "s" },
    )
}

/// Whether `bracket` stands wholly inside `range`.
fn inside(bracket: &Bracket, range: &Range<usize>) -> bool {
    range.start <= bracket.open && bracket.close < range.end
}

/// The holes a bracket holds, as a message names them.
fn holding(held: &[&str]) -> String {
    if held.is_empty() {
        return "no hole".to_owned();
    }
    let held: Vec<String> = held.iter().map(|name| format!("${name}")).collect();
    held.join(", ")
}

/// The names of the holes `at`, each with where it starts, that `bracket` holds, each once and
/// sorted: a rewrite may reorder what a bracket holds, as `f($b, $a)` does, and it is still the
/// same bracket.
fn held_in<'a>(bracket: &Bracket, at: impl IntoIterator<Item = (usize, &'a str)>) -> Vec<&'a str> {
    let mut held: Vec<&str> = Vec::new();
    for (start, name) in at {
        if bracket.open < start && start <= bracket.close && !held.contains(&name) {
            held.push(name);
        }
    }
    held.sort_unstable();
    held
}

/// Why a trailing comma refuses an edit: where it is in the old text, and why.
type CommaRefused = (usize, String);

/// A trailing comma to carry: where in the new text, where it was in the old, the bracket it
/// stood before, and the holes that bracket holds, as a message names them.
struct Insertion {
    at: usize,
    comma: usize,
    close: String,
    holding: String,
}

/// Which of `commas`, trailing commas in `text`, the pattern ignores: those the file parses the
/// same without.  Tried all at once first, since that is the common case, then one by one.
fn ignored_commas(text: &str, arena: &Arena<'_>, commas: &[usize]) -> HashSet<usize> {
    let before = kinds(arena);
    let without = |chosen: &[usize]| {
        let mut sorted = chosen.to_vec();
        sorted.sort_unstable();
        let mut left = String::with_capacity(text.len());
        let mut copied = 0;
        for at in sorted {
            left.push_str(&text[copied..at]);
            copied = at + 1;
        }
        left.push_str(&text[copied..]);
        parse_module(&left).is_ok_and(|parsed| kinds(&parsed) == before)
    };
    if without(commas) {
        return commas.iter().copied().collect();
    }
    commas
        .iter()
        .copied()
        .filter(|comma| without(&[*comma]))
        .collect()
}

/// The kind of every node in `arena`, in order: what a comma that changes nothing leaves alone.
fn kinds(arena: &Arena<'_>) -> Vec<&'static str> {
    arena.nodes().map(|node| arena.kind(node).name()).collect()
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

    /// `text` with `pattern` replaced by `with`, and what was questioned, or why it was refused.
    fn edited(pattern: &str, with: &str, text: &str) -> Result<Option<Edited>, String> {
        let pattern = compile_pattern(pattern).expect("the pattern compiles");
        let rewrite = compile_rewrite(with, &pattern).expect("the rewrite compiles");
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
        let commas = [
            text.find("a,)").unwrap() + 1,
            text.rfind("a,)").unwrap() + 1,
        ];
        assert_eq!(
            ignored_commas(text, &arena, &commas),
            HashSet::from([commas[1]])
        );
        // A one-element tuple's comma is the pattern's own, so the rewrite says what becomes of
        // it.
        assert_eq!(ok("f(($a,))", "g([$a])", "f((x,))\n"), "g([x])\n");
    }

    #[test]
    fn a_trailing_comma_with_no_one_counterpart_is_refused() {
        // The rewrite holds `$b` in no bracket of its own.
        let why = replaced("f($a, g($b))", "h(k($a, $b))", "f(x, g(\n    y,\n))\n")
            .expect_err("no counterpart");
        assert!(
            why.starts_with("2:6: the trailing comma before \")\" is in the match"),
            "{why}"
        );
        assert!(
            why.contains("has 0 brackets holding $b where the match has 1"),
            "{why}"
        );
        // Nor where the bracket is gone.
        assert!(
            replaced(
                "grid[$i, $j]",
                "grid[$i][$j]",
                "v = grid[\n    i,\n    j,\n]\n"
            )
            .is_err()
        );
        assert!(replaced("[$a, $b]", "$b", "v = [x, d[k],]\n").is_err());
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
