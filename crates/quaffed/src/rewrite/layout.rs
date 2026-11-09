// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The layout a pattern ignores at a bracket's edges, carried from a match's brackets to the
//! rewrite's: a trailing comma, and comments just inside the bracket, before its first item or
//! after its last.
//!
//! A pattern matches a bracket whatever layout it holds, so a template cannot say what becomes
//! of it.  A bracket of the match's own - not inside what any hole matched, whose brackets are
//! the hole's - corresponds to the rewrite's bracket holding the same holes written as code, in
//! any order, when there are as many such brackets on each side; brackets holding no hole
//! correspond by their order.  Its layout is carried to that bracket.  Where the rewrite has no
//! bracket holding what it did, it deleted or split it: a trailing comma, which only asks for
//! the contents one to a line, goes with it, but a comment is content and refuses the edit.
//! Where it has some, but not the same number, which one is meant cannot be told, and the edit
//! is refused.

use std::ops::Range;

use quaffed_python::{Bracket, Lexed, lex, parse_module};
use quaffed_syntax::{Arena, Captured};

use super::back_over_layout;

/// The file as it was: its text, tree and lexing, each match with what it captured, and the
/// names the captures are in the order of.
pub(super) struct Before<'a> {
    pub text: &'a str,
    pub arena: &'a Arena<'a>,
    pub lexed: &'a Lexed,
    pub captured: &'a [Captured],
    pub names: &'a [&'a str],
}

/// The file once each match is rewritten, before any layout is carried: its text and tree,
/// where each match's rewrite was written, and each code site in it.
pub(super) struct After<'a> {
    pub text: &'a str,
    pub arena: &'a Arena<'a>,
    pub written: &'a [Range<usize>],
    pub sites: &'a [WrittenSite],
}

/// Where a code site was written in the file's new text, the match it belongs to, whether it is
/// a sequence, and the name it writes.
#[derive(Debug, Clone)]
pub(super) struct WrittenSite {
    pub range: Range<usize>,
    pub found: usize,
    pub sequence: bool,
    pub name: String,
}

/// The new text with the layout carried, and where each match's rewrite and each code site now
/// stand in it.
pub(super) struct Carried {
    pub text: String,
    pub written: Vec<Range<usize>>,
    pub sites: Vec<WrittenSite>,
}

/// Why carrying layout refuses an edit: where in the old text, and why.
pub(super) type LayoutRefused = (usize, String);

/// The layout at one bracket's edges: a trailing comma the pattern ignores, and the text before
/// its first item and after its last, or after its comma, where either holds a comment.
struct Edges {
    comma: Option<usize>,
    leading: Option<Range<usize>>,
    trailing: Option<Range<usize>>,
    /// Whether each edge holds a comment, which must be carried, where layout alone is carried
    /// only into an edge the template left empty.
    leading_comment: bool,
    trailing_comment: bool,
}

impl Edges {
    fn is_empty(&self) -> bool {
        self.comma.is_none() && self.leading.is_none() && self.trailing.is_none()
    }

    /// Whether anything at these edges refuses the edit when it cannot be placed: a comma or a
    /// comment, never layout alone, which the template's then replaces.
    fn must_be_placed(&self) -> bool {
        self.comma.is_some() || self.leading_comment || self.trailing_comment
    }
}

/// The comments at the edges of each match's own brackets, which [`carry`] carries or refuses,
/// so that the check for comments no capture holds passes over them.
pub(super) fn edge_comments(before: &Before<'_>) -> Vec<Range<usize>> {
    let own = own_brackets(before);
    own.iter()
        .flatten()
        .flat_map(|bracket| {
            let edges = edges(before, bracket);
            [edges.leading, edges.trailing]
        })
        .flatten()
        .flat_map(|gap| {
            before
                .lexed
                .comments
                .iter()
                .filter(move |comment| gap.start <= comment.start && comment.end <= gap.end)
                .cloned()
        })
        .collect()
}

/// The new text with each match's bracket layout carried to the rewrite, and where everything
/// now stands; `None` if there was none to carry.  A comma or comment that cannot be carried
/// refuses the edit.
pub(super) fn carry(
    before: &Before<'_>,
    after: &After<'_>,
) -> Result<Option<Carried>, LayoutRefused> {
    let mut edits = edits(before, after)?;
    if edits.is_empty() {
        return Ok(None);
    }
    edits.sort_unstable_by_key(|edit| edit.range.start);
    let out = after.text;
    let with = |chosen: &[&Edit]| {
        let mut carried = String::with_capacity(out.len());
        let mut commas = Vec::new();
        let mut copied = 0;
        for edit in chosen {
            carried.push_str(&out[copied..edit.range.start]);
            if edit.comma.is_some() {
                commas.push(carried.len());
            }
            carried.push_str(&edit.text);
            copied = edit.range.end;
        }
        carried.push_str(&out[copied..]);
        (carried, commas)
    };
    let all: Vec<&Edit> = edits.iter().collect();
    let (carried, commas) = with(&all);
    // A carried comma must be one the matcher ignores where it lands: not a token of the new
    // tree, as a tuple's comma is; and comments change no tree.  Only where the result does not
    // parse at all is each tried alone, to name the one at fault.
    let culprit = match parse_module(&carried) {
        Ok(parsed) => edits
            .iter()
            .filter(|edit| edit.comma.is_some())
            .zip(&commas)
            .find(|(_, at)| !ignored_by_matcher(&parsed, **at))
            .map(|(edit, _)| edit)
            .or_else(|| (kinds(&parsed) != kinds(after.arena)).then(|| &edits[0])),
        Err(_) => Some(
            edits
                .iter()
                .find(|edit| parse_module(&with(&[*edit]).0).is_err())
                .unwrap_or(&edits[0]),
        ),
    };
    if let Some(culprit) = culprit {
        return Err((
            culprit.at,
            format!(
                "the {} before {:?} is in the match but no capture holds it, and in the \
                 rewrite's bracket holding {} it would change what the code means, so it can be \
                 neither kept nor dropped: narrow the pattern so that it does not match here, or \
                 take the comma out first",
                culprit.what, culprit.close, culprit.holding
            ),
        ));
    }
    let shifted = |offset: usize| -> usize {
        let mut moved = offset;
        for edit in &edits {
            let Range { start, end } = edit.range;
            // Text inserted at a leading edge stands before what starts there; at a trailing
            // edge, after what ends there.
            let before = if start == end {
                start < offset || (edit.leading && start == offset)
            } else {
                start < offset && end <= offset
            };
            if before {
                moved = moved + edit.text.len() - (end - start);
            }
        }
        moved
    };
    Ok(Some(Carried {
        text: carried,
        written: after
            .written
            .iter()
            .map(|range| shifted(range.start)..shifted(range.end))
            .collect(),
        sites: after
            .sites
            .iter()
            .map(|site| WrittenSite {
                range: shifted(site.range.start)..shifted(site.range.end),
                ..site.clone()
            })
            .collect(),
    }))
}

/// One change to the new text: `range` replaced by `text`.  `comma` is where a carried comma,
/// written first, was in the old text; `at`, `what`, `close` and `holding` say what was carried
/// for a message.
struct Edit {
    range: Range<usize>,
    text: String,
    /// Whether it is at a bracket's leading edge, before the first item.
    leading: bool,
    comma: Option<usize>,
    at: usize,
    what: &'static str,
    close: String,
    holding: String,
}

/// The changes that carry each match's bracket layout to the rewrite.
fn edits(before: &Before<'_>, after: &After<'_>) -> Result<Vec<Edit>, LayoutRefused> {
    let own = own_brackets(before);
    let edges: Vec<Vec<Edges>> = own
        .iter()
        .map(|brackets| brackets.iter().map(|b| edges(before, b)).collect())
        .collect();
    if edges.iter().flatten().all(Edges::is_empty) {
        return Ok(Vec::new());
    }
    let out_lexed = lex(after.text);
    let mut changes = Vec::new();
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
            let edges = &edges[index][i];
            if edges.is_empty() {
                continue;
            }
            let held = &file_held[i];
            let same = file_held.iter().filter(|h| *h == held).count();
            let counterparts: Vec<&Bracket> = rewrite_brackets
                .iter()
                .zip(&rewrite_held)
                .filter(|(_, h)| *h == held)
                .map(|(b, _)| *b)
                .collect();
            let carrying = Carrying {
                before,
                out: after.text,
                out_lexed: &out_lexed,
                bracket,
                edges,
                holding: holding(held),
            };
            if counterparts.is_empty() {
                // The rewrite deleted or split the bracket: a comma goes with it, a comment
                // cannot.
                if let Some(comment) = carrying.first_comment() {
                    return Err((comment.start, carrying.deleted(&comment)));
                }
                continue;
            }
            if counterparts.len() != same {
                if !edges.must_be_placed() {
                    continue;
                }
                return Err(carrying.no_one_place(counterparts.len(), same));
            }
            let nth = file_held[..i].iter().filter(|h| *h == held).count();
            changes.extend(carrying.to(counterparts[nth])?);
        }
    }
    Ok(changes)
}

/// One bracket's layout, on its way to its counterpart in the rewrite.
struct Carrying<'a> {
    before: &'a Before<'a>,
    out: &'a str,
    out_lexed: &'a Lexed,
    bracket: &'a Bracket,
    edges: &'a Edges,
    holding: String,
}

impl Carrying<'_> {
    fn close(&self) -> String {
        self.before.text[self.bracket.close..=self.bracket.close].to_owned()
    }

    /// The first comment at the bracket's edges, if any.
    fn first_comment(&self) -> Option<Range<usize>> {
        [&self.edges.leading, &self.edges.trailing]
            .into_iter()
            .flatten()
            .flat_map(|gap| {
                self.before
                    .lexed
                    .comments
                    .iter()
                    .filter(|c| gap.start <= c.start && c.end <= gap.end)
            })
            .next()
            .cloned()
    }

    /// Why a comment at the edge of a bracket the rewrite deleted refuses the edit.
    fn deleted(&self, comment: &Range<usize>) -> String {
        format!(
            "the comment {:?} is in the match but in no capture the rewrite writes back, and the \
             bracket it stands in, holding {}, is not in the rewrite, so the edit would drop it: \
             capture it with a hole, or narrow the pattern so that it does not match here",
            &self.before.text[comment.clone()],
            self.holding
        )
    }

    /// Why the bracket's layout cannot be placed: the rewrite has `found` brackets holding the
    /// same holes, where the match has `wanted`.
    fn no_one_place(&self, found: usize, wanted: usize) -> LayoutRefused {
        let (at, what) = match (self.edges.comma, self.first_comment()) {
            (Some(comma), _) => (comma, "trailing comma"),
            (None, Some(comment)) => (comment.start, "comment"),
            (None, None) => unreachable!("only a bracket with layout to carry is placed"),
        };
        (
            at,
            format!(
                "the {what} before {:?} is in the match but no capture holds it, and the \
                 rewrite has {found} bracket{} holding {} where the match has {wanted}, so it \
                 cannot be told where it belongs, and dropping it would change more than was \
                 asked: narrow the pattern so that it does not match here, or take it out first",
                self.close(),
                if found == 1 { "" } else { "s" },
                self.holding,
            ),
        )
    }

    /// The changes carrying the layout to `partner`, the rewrite's corresponding bracket.  A
    /// comment meets one the template wrote at the same edge only by refusing, since which
    /// should stand first cannot be told.
    fn to(&self, partner: &Bracket) -> Result<Vec<Edit>, LayoutRefused> {
        let text = self.before.text;
        let comments = &self.out_lexed.comments;
        let inner = partner.open + 1;
        let first = forward_over_layout(self.out, inner, partner.close, comments);
        let last = back_over_layout(self.out, inner, partner.close, comments);
        let mut edits = Vec::new();
        let template_comment = |gap: Range<usize>| {
            comments
                .iter()
                .any(|c| gap.start <= c.start && c.end <= gap.end)
        };
        let refuse = |at: usize| {
            (
                at,
                format!(
                    "the comment {:?} is in the match but no capture holds it, and the rewrite \
                     writes a comment of its own at the same edge of the bracket holding {}, so \
                     which stands first cannot be told: narrow the pattern so that it does not \
                     match here",
                    &text[self.first_comment().expect("a comment is carried")],
                    self.holding
                ),
            )
        };
        let empty = first == partner.close;
        let edges = self.edges;
        if empty {
            // A bracket the rewrite leaves empty takes the comments at both edges, one after the
            // other, and has nothing for a comma to trail.
            let mut carried = String::new();
            let mut at = None;
            for (gap, comment) in [
                (&edges.leading, edges.leading_comment),
                (&edges.trailing, edges.trailing_comment),
            ] {
                if let (Some(gap), true) = (gap, comment) {
                    at.get_or_insert(gap.start);
                    carried.push_str(&text[gap.clone()]);
                }
            }
            if let Some(at) = at {
                if template_comment(inner..first) {
                    return Err(refuse(at));
                }
                edits.push(self.edit(inner..first, carried, None, at, "comment"));
            }
            return Ok(edits);
        }
        if let Some(leading) = &edges.leading {
            if edges.leading_comment && template_comment(inner..first) {
                return Err(refuse(leading.start));
            }
            if edges.leading_comment || first == inner {
                let carried = text[leading.clone()].to_owned();
                edits.push(self.edit(inner..first, carried, None, leading.start, "comment"));
            }
        }
        let has_comma = partner.trailing_comma.is_some();
        let comma = edges.comma.filter(|_| !has_comma);
        // After the rewrite's last item, or after its own comma if it has one, which is code to
        // `back_over_layout`.
        let trailing = edges
            .trailing
            .as_ref()
            .filter(|_| edges.trailing_comment || last == partner.close);
        match (trailing, comma) {
            (None, None) => {}
            (None, Some(comma)) => {
                edits.push(self.edit(last..last, ",".into(), Some(comma), comma, "trailing comma"));
            }
            (Some(trailing), comma) => {
                if edges.trailing_comment && template_comment(last..partner.close) {
                    return Err(refuse(trailing.start));
                }
                let mut carried = String::new();
                if comma.is_some() {
                    carried.push(',');
                }
                carried.push_str(&text[trailing.clone()]);
                let what = if comma.is_some() {
                    "trailing comma"
                } else {
                    "comment"
                };
                let at = comma.unwrap_or(trailing.start);
                edits.push(self.edit(last..partner.close, carried, comma, at, what));
            }
        }
        Ok(edits)
    }

    /// Whether `range` is at the leading edge of the bracket it is in: it starts just inside an
    /// opening bracket.
    fn leading_edge(&self, range: &Range<usize>) -> bool {
        range.start > 0 && matches!(self.out.as_bytes()[range.start - 1], b'(' | b'[' | b'{')
    }

    fn edit(
        &self,
        range: Range<usize>,
        text: String,
        comma: Option<usize>,
        at: usize,
        what: &'static str,
    ) -> Edit {
        Edit {
            leading: self.leading_edge(&range),
            range,
            text,
            comma,
            at,
            what,
            close: self.close(),
            holding: self.holding.clone(),
        }
    }
}

/// The layout at `bracket`'s edges in the old text.  The leading edge runs from just inside the
/// opening bracket to the first item, and the trailing edge from the last item, or the trailing
/// comma after it, to the closing bracket; each is kept where it holds anything.  An empty
/// bracket has a leading edge only.
fn edges(before: &Before<'_>, bracket: &Bracket) -> Edges {
    let text = before.text;
    let comments = &before.lexed.comments;
    let inner = bracket.open + 1;
    let first = forward_over_layout(text, inner, bracket.close, comments);
    let holds_comment = |gap: &Range<usize>| {
        comments
            .iter()
            .any(|c| gap.start <= c.start && c.end <= gap.end)
    };
    let comma = bracket
        .trailing_comma
        .filter(|c| ignored_by_matcher(before.arena, *c));
    let leading = Some(inner..first).filter(|gap| !gap.is_empty());
    let trailing = if first == bracket.close {
        None
    } else {
        let last = match bracket.trailing_comma {
            Some(comma) => comma + 1,
            None => back_over_layout(text, inner, bracket.close, comments),
        };
        Some(last..bracket.close).filter(|gap| !gap.is_empty())
    };
    Edges {
        comma,
        leading_comment: leading.as_ref().is_some_and(holds_comment),
        trailing_comment: trailing.as_ref().is_some_and(holds_comment),
        leading,
        trailing,
    }
}

/// Forward from `at` in `text`, no further than `ceiling`, over whitespace and comments: where
/// the code after them starts.
pub(super) fn forward_over_layout(
    text: &str,
    at: usize,
    ceiling: usize,
    comments: &[Range<usize>],
) -> usize {
    let mut at = at;
    loop {
        let rest = &text[at..ceiling];
        at += rest.len() - rest.trim_start().len();
        match comments.iter().find(|comment| comment.start == at) {
            Some(comment) => at = comment.end,
            None => return at,
        }
    }
}

/// Each match's own brackets: not inside any capture, whose brackets are the capture's.
fn own_brackets<'a>(before: &Before<'a>) -> Vec<Vec<&'a Bracket>> {
    before
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
        .collect()
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

/// Whether the comma at `at` is one the matcher ignores: the arena keeps no token for it.  A
/// trailing comma is layout to the matcher, but a one-element tuple's is the tuple, and is kept.
pub(super) fn ignored_by_matcher(arena: &Arena<'_>, at: usize) -> bool {
    arena.tokens_in(at..at + 1).is_empty()
}

/// The kind of every node in `arena`, in order: what carried layout must leave as it was.
fn kinds(arena: &Arena<'_>) -> Vec<&'static str> {
    arena.nodes().map(|node| arena.kind(node).name()).collect()
}
