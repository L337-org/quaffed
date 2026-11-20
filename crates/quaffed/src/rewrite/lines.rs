// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Statements given lines of their own where the code around them shares their line, in a
//! language whose blocks are decided by layout: one whose [`Editing`] names what ends a header
//! whose block may be written on the header's line.  For any other, nothing here applies.
//!
//! A statement starts its line, follows a statement separator, or follows the end of a header
//! whose block is written on the header's line, as in Python's `if x: a(); z()`.  The examples
//! below are Python's: a semicolon is the language's statement separator, a colon what ends such
//! a header, and a backslash what continues a line.  What a rewrite writes there,
//! or what a hole captured and a template writes there, may need lines of its own: it may be
//! more than one line, start with a compound statement, which can follow neither a semicolon nor
//! a colon, or end with one or with a comment, which nothing can follow on its line.  Its later
//! lines would otherwise be written at the depth of the line it shares, which for a block on its
//! header's line is the header's, and they would leave the block; a statement after it on its
//! line would join its last block or its comment.
//!
//! So the code around is laid out for it, each new line at the depth of the block the statement
//! is in: a block on its header's line is moved to lines of its own, indented under the header
//! by the indent unit; a semicolon before it becomes a line break where it starts with a compound
//! statement, and one after it where it ends with one or with a comment.  Nothing else on those
//! lines moves.  Where no indent unit can be found, the depth cannot be derived, and the edit is
//! refused.
//!
//! [`Placed`] then checks the outcome: every statement written at the top level of a rewrite,
//! and the statements beside it on its line, must stand in the block the match stood in, or the
//! edit is refused.  It is the backstop for a layout nothing here foresaw.
//!
//! Its tests are `rewrite`'s, which drive a whole rewrite, since a layout can only be judged in
//! the text it lands in.

use std::ops::Range;

use quaffed_syntax::{Arena, Editing, FrontEnd, Lexed, NodeId};

use super::line_indent;

/// Whether `node`, a statement, holds a block, and so ends its logical line with it: whether a
/// statement stands inside it, which only a block can hold.
fn compound(arena: &Arena<'_>, node: NodeId) -> bool {
    arena
        .nodes_within(arena.range(node))
        .any(|inner| inner != node && arena.list(inner).is_some())
}

/// What code written as statements needs of the line it is written on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Needs {
    /// It holds a line break that ends a statement or a comment: it is more than one line.
    lines: bool,
    /// It starts with a compound statement, which can follow neither a semicolon nor a colon.
    starts_block: bool,
    /// It ends with a compound statement or a comment, which nothing can follow on its line.
    ends_line: bool,
}

impl Needs {
    /// What `range` of `text`, parsed as `arena` and lexed as `lexed`, needs, by `editing`'s
    /// rules: its outermost statements, its comments, and its line breaks outside brackets,
    /// strings and continued lines.
    pub(super) fn of(
        text: &str,
        arena: &Arena<'_>,
        lexed: &Lexed,
        editing: &Editing,
        range: &Range<usize>,
    ) -> Needs {
        let statements = outermost(arena, range);
        let compound = |node: &NodeId| compound(arena, *node);
        let code = &text[range.clone()];
        let content_end = range.start + code.trim_end().len();
        let lines = code.match_indices(['\n', '\r']).any(|(i, _)| {
            let at = range.start + i;
            at < content_end
                && !lexed.brackets.iter().any(|b| b.open < at && at < b.close)
                && !lexed
                    .multi_line_strings
                    .iter()
                    .any(|s| s.start < at && at < s.end)
                && !editing
                    .line_continuation
                    .is_some_and(|c| text[..at].trim_end_matches('\r').ends_with(c))
        });
        let first_code = range.start + code.len() - code.trim_start().len();
        Needs {
            lines,
            starts_block: statements
                .first()
                .is_some_and(|node| arena.range(*node).start == first_code && compound(node)),
            ends_line: statements.last().is_some_and(compound)
                || lexed
                    .comments
                    .iter()
                    .any(|c| c.end == content_end && range.start <= c.start),
        }
    }

    /// What `text`, written as a whole, needs; nothing where it is not statements on its own.
    pub(super) fn of_text(language: &dyn FrontEnd, text: &str) -> Needs {
        match language.parse(text) {
            Ok(arena) => Needs::of(
                text,
                &arena,
                &language.lex(text),
                language.editing(),
                &(0..text.len()),
            ),
            Err(_) => Needs::default(),
        }
    }

    /// Whether it needs anything of its line.
    pub(super) fn any(self) -> bool {
        self.lines || self.starts_block || self.ends_line
    }
}

/// A run of statements in a text that shares its line with other code: where laying it out
/// would change that text, and what block it is in.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    /// The indentation of the logical line the run starts on.
    line_indent: String,
    /// Where its block is on its header's line: the gap after the header's colon, and the
    /// header's indentation.
    header: Option<(Range<usize>, String)>,
    /// The gap from the statement before it on its line, semicolon and all.
    before: Option<Range<usize>>,
    /// The gap to the statement after it on its line.
    after: Option<Range<usize>>,
    /// The block it is in, to check the outcome against.
    block: Block,
    /// The statements beside it on its line, before and after, to check the outcome against.
    pub(super) beside: Vec<Range<usize>>,
}

/// A block, as its holder's start - `None` for the file's top level - and which of its holder's
/// statement lists it is, counted from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    holder: Option<usize>,
    list: usize,
}

/// The layout a slot needs for what is written into it: the indentation of its block, and the
/// changes to the text around it, each a range written over with a line break and that
/// indentation.
#[derive(Debug, Clone)]
pub(super) struct Layout {
    pub(super) indent: String,
    pub(super) changes: Vec<(Range<usize>, String)>,
}

/// Why a layout cannot be derived.
pub(super) const NO_UNIT: &str = "the match is in a block written on its header's line, and \
    the rewrite needs lines of its own there, but nothing indents a block - neither the file \
    nor the rewrite - so the depth to write them at cannot be derived: write the block on lines \
    of its own, indented under its header, or write the rewrite on one line";

impl Slot {
    /// The slot `at` is in `text`, parsed as `arena` and lexed as `lexed`, if `at` is exactly a
    /// run of statements in one block and shares its line with other code; `None` otherwise,
    /// and always where `editing` names no end of a header whose block may share its line.
    /// Where the colon before a block on its header's line cannot be found, which only a line
    /// continued by a backslash after it can do, the block is not laid out, and what the
    /// rewrite writes is left to the backstop.
    pub(super) fn find(
        text: &str,
        arena: &Arena<'_>,
        lexed: &Lexed,
        editing: &Editing,
        at: &Range<usize>,
    ) -> Option<Slot> {
        let opener = editing.header_block_opener?;
        let logical = |offset: usize| {
            let index = lexed.logical_starts.partition_point(|&s| s <= offset);
            index.checked_sub(1).map_or(0, |i| lexed.logical_starts[i])
        };
        // Most statements start their line and end it: those are settled without the tree.
        let rest = &text[at.end..];
        let semicolon_after = editing
            .statement_separator
            .is_some_and(|c| rest.trim_start_matches([' ', '\t']).starts_with(c));
        if logical(at.start) == at.start && !semicolon_after {
            return None;
        }
        let statements = outermost(arena, at);
        let (&first, &last) = (statements.first()?, statements.last()?);
        if arena.range(first).start != at.start || arena.range(last).end != at.end {
            return None;
        }
        let holder = parent(arena, first);
        let block_list = arena.list(first)?;
        let siblings: Vec<NodeId> = arena
            .children(holder)
            .filter(|&child| arena.list(child) == Some(block_list))
            .collect();
        let i = siblings.iter().position(|&s| s == first)?;
        let j = siblings.iter().position(|&s| s == last)?;
        let line = logical(at.start);
        let last_line = logical(arena.range(last).start);
        let prev = i
            .checked_sub(1)
            .map(|i| arena.range(siblings[i]))
            .filter(|prev| logical(prev.start) == line);
        let next = siblings
            .get(j + 1)
            .map(|&next| arena.range(next))
            .filter(|next| logical(next.start) == last_line);
        let opening = arena.range(siblings[0]).start;
        let colon = text[..opening].trim_end();
        let header = (logical(opening) != opening && colon.ends_with(opener))
            .then(|| (colon.len()..opening, line_indent(text, logical(opening))));
        Some(Slot {
            line_indent: line_indent(text, line),
            header,
            before: prev.as_ref().map(|prev| prev.end..at.start),
            after: next.as_ref().map(|next| at.end..next.start),
            block: block_of(arena, first),
            beside: prev.into_iter().chain(next).collect(),
        })
    }

    /// The layout for code with `needs` written into the slot, its blocks indented by `unit`,
    /// each line break spelled `ending`.
    ///
    /// # Errors
    ///
    /// Returns [`NO_UNIT`] where a block on its header's line must be given lines of its own
    /// and there is no `unit`.
    pub(super) fn lay_out(
        &self,
        needs: Needs,
        unit: Option<&str>,
        ending: &str,
    ) -> Result<Layout, &'static str> {
        let mut changes = Vec::new();
        let indent = match &self.header {
            None => self.line_indent.clone(),
            Some((gap, header)) => {
                let open =
                    needs.lines || needs.starts_block || needs.ends_line && self.after.is_some();
                if !open {
                    // Written on the header's line, it has no later line for an indent to
                    // reach.
                    return Ok(Layout {
                        indent: header.clone(),
                        changes,
                    });
                }
                let indent = format!("{header}{}", unit.ok_or(NO_UNIT)?);
                changes.push((gap.clone(), format!("{ending}{indent}")));
                indent
            }
        };
        if needs.starts_block
            && let Some(gap) = &self.before
        {
            changes.push((gap.clone(), format!("{ending}{indent}")));
        }
        if needs.ends_line
            && let Some(gap) = &self.after
        {
            changes.push((gap.clone(), format!("{ending}{indent}")));
        }
        Ok(Layout { indent, changes })
    }
}

/// Where `slot`'s statements are once written: the rewrite's range in the new text, and where
/// each statement beside it that is checked now starts.
pub(super) struct Placed<'a> {
    pub(super) slot: &'a Slot,
    pub(super) written: Range<usize>,
    pub(super) beside: Vec<usize>,
}

/// Why an edit is refused when a statement would not stand in the block its match stood in.
pub(super) const LEFT_BLOCK: &str = "the rewrite here would leave a statement in a block other \
    than the one the match is in, so the code would mean something other than the rewrite says: \
    write this block on lines of its own, one statement to a line, and run the edit again";

impl Placed<'_> {
    /// Whether every statement at the top level of what was written, and each beside it on its
    /// line, stands in the block the match stood in, in `arena`, the new text's tree, with
    /// `moved` taking an offset of the old text to the new.
    pub(super) fn stays(&self, arena: &Arena<'_>, moved: impl Fn(usize) -> usize) -> bool {
        let expected = Block {
            holder: self.slot.block.holder.map(moved),
            list: self.slot.block.list,
        };
        let mut nodes = outermost(arena, &self.written);
        for &start in &self.beside {
            let found = arena
                .nodes_within(start..arena.text().len())
                .find(|&node| arena.range(node).start == start && arena.list(node).is_some());
            match found {
                Some(node) => nodes.push(node),
                None => return false,
            }
        }
        nodes.iter().all(|&node| block_of(arena, node) == expected)
    }
}

/// The block `node`, a statement, is in.
fn block_of(arena: &Arena<'_>, node: NodeId) -> Block {
    let holder = parent(arena, node);
    let mut lists: Vec<u32> = Vec::new();
    for child in arena.children(holder) {
        if let Some(list) = arena.list(child)
            && !lists.contains(&list)
        {
            lists.push(list);
        }
    }
    Block {
        holder: (holder != arena.root()).then(|| arena.range(holder).start),
        list: arena
            .list(node)
            .and_then(|l| lists.iter().position(|&x| x == l))
            .unwrap_or(0),
    }
}

/// The outermost statements inside `range`, in order: each in a block's list, and inside no
/// other in `range`.
fn outermost(arena: &Arena<'_>, range: &Range<usize>) -> Vec<NodeId> {
    let mut found: Vec<NodeId> = Vec::new();
    let mut end = range.start;
    for node in arena.nodes_within(range.clone()) {
        let held = arena.range(node);
        if arena.list(node).is_some() && (found.is_empty() || held.start >= end) {
            end = held.end;
            found.push(node);
        }
    }
    found
}

/// The node whose children hold `node`, found down from the root; the root for the root.
fn parent(arena: &Arena<'_>, node: NodeId) -> NodeId {
    let target = arena.range(node);
    let mut current = arena.root();
    'down: loop {
        for child in arena.children(current) {
            if child == node {
                return current;
            }
            let held = arena.range(child);
            if held.start <= target.start && target.end <= held.end {
                current = child;
                continue 'down;
            }
        }
        return current;
    }
}
