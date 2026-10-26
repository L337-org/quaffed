// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Python source parsed into a syntax arena, and the kinds of node a piece of source contains.

use std::collections::BTreeSet;
use std::fmt;

use quaffed_syntax::{Arena, Builder, Kind, Language, TokenText, TooLong};
use ruff_python_ast::token::{Token, TokenKind, Tokens, parentheses_iterator};
use ruff_python_ast::visitor::source_order::{
    SourceOrderVisitor, TraversalSignal, walk_body, walk_interpolated_string_element, walk_node,
};
use ruff_python_ast::{AnyNodeRef, ExprRef, NodeKind, Stmt, StringFlags};
use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::kinds::kind_of;

/// The language every arena this adapter builds carries.
pub const PYTHON: Language = Language::new("Python");

/// Why source could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Ruff refused the source: its message, verbatim, and the byte offset where the error
    /// starts.
    Invalid {
        /// What the parser said.
        message: String,
        /// The byte offset in the source.
        offset: usize,
    },
    /// The source is longer than an arena's offsets reach, so it was not parsed.
    TooLong(TooLong),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Invalid { message, offset } => write!(f, "at byte {offset}: {message}"),
            ParseError::TooLong(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Invalid { .. } => None,
            ParseError::TooLong(err) => Some(err),
        }
    }
}

impl From<ruff_python_parser::ParseError> for ParseError {
    fn from(err: ruff_python_parser::ParseError) -> Self {
        ParseError::Invalid {
            message: err.error.to_string(),
            offset: err.location.start().to_usize(),
        }
    }
}

impl From<TooLong> for ParseError {
    fn from(err: TooLong) -> Self {
        ParseError::TooLong(err)
    }
}

/// `source` parsed as a module: a file's worth of statements.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid module, or is too long to parse.
/// Nothing partial is returned.
pub fn parse_module(source: &str) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, source), source, Mode::Module)
}

/// `source` parsed as `IPython` source: statements that may include its escape commands -
/// `%timeit`, `!ls`, `?len` and the like - which are not Python and which Ruff parses only in
/// this mode.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not valid `IPython` source, or is too long.
pub fn parse_ipython(source: &str) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, source), source, Mode::Ipython)
}

/// `source` parsed as a single expression.
///
/// # Errors
///
/// Returns the first [`ParseError`] if `source` is not a valid expression, or is too long.
pub fn parse_expression(source: &str) -> Result<Arena<'_>, ParseError> {
    parse_from(Builder::new(PYTHON, source), source, Mode::Expression)
}

/// `source` parsed in `mode` into the arena `builder` starts, or the builder's refusal of text
/// too long for it, before Ruff sees the text.
pub(crate) fn parse_from<'text>(
    builder: Result<Builder<'text>, TooLong>,
    source: &'text str,
    mode: Mode,
) -> Result<Arena<'text>, ParseError> {
    let builder = builder?;
    let parsed = ruff_python_parser::parse(source, ParseOptions::from(mode))?;
    Ok(build(
        builder,
        AnyNodeRef::from(parsed.syntax()),
        parsed.tokens(),
        source,
    ))
}

/// Every kind of node in `source`, parsed as a module.
///
/// # Errors
///
/// As [`parse_module`].  Nothing partial is returned, because kinds counted from a tree with
/// errors in it would claim coverage the corpus does not have.
pub fn kinds_in_module(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    parse_module(source).map(|arena| kinds_in(&arena))
}

/// Every kind of node in `source`, parsed as `IPython` source.
///
/// # Errors
///
/// As [`parse_ipython`].
pub fn kinds_in_ipython(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    parse_ipython(source).map(|arena| kinds_in(&arena))
}

/// Every kind of node in `source`, parsed as a single expression.
///
/// # Errors
///
/// As [`parse_expression`].
pub fn kinds_in_expression(source: &str) -> Result<BTreeSet<Kind>, ParseError> {
    parse_expression(source).map(|arena| kinds_in(&arena))
}

fn kinds_in(arena: &Arena<'_>) -> BTreeSet<Kind> {
    arena.nodes().map(|node| arena.kind(node)).collect()
}

/// Where the comma of `node` starts, if it is a one-element tuple: the first comma after its
/// element, since the element's own commas lie inside it.
fn tuple_comma(node: AnyNodeRef<'_>, tokens: &Tokens) -> Option<TextSize> {
    let AnyNodeRef::ExprTuple(tuple) = node else {
        return None;
    };
    let [element] = tuple.elts.as_slice() else {
        return None;
    };
    tokens
        .after(element.range().end())
        .iter()
        .take_while(|token| token.start() < tuple.range().end())
        .find(|token| token.kind() == TokenKind::Comma)
        .map(Token::start)
}

/// Where the comma ending a tuple of two or more elements starts, if it has one: `a, b,` is the
/// tuple `a, b` to `CPython`, so the comma is layout, as one before a closing bracket is.
fn trailing_tuple_comma(node: AnyNodeRef<'_>, tokens: &Tokens) -> Option<TextSize> {
    let AnyNodeRef::ExprTuple(tuple) = node else {
        return None;
    };
    let [_, .., last] = tuple.elts.as_slice() else {
        return None;
    };
    tokens
        .after(last.range().end())
        .iter()
        .take_while(|token| token.start() < tuple.range().end())
        .find(|token| token.kind() == TokenKind::Comma)
        .map(Token::start)
}

/// Whether `node` is the empty argument list of a class, `class A():`, which `CPython`'s tree
/// does not record: `class A():` and `class A:` are one class, so the list is left out of the
/// arena and its parentheses take no part.
fn empty_class_arguments(node: AnyNodeRef<'_>, parent: Option<AnyNodeRef<'_>>) -> bool {
    matches!(
        (node, parent),
        (AnyNodeRef::Arguments(arguments), Some(AnyNodeRef::StmtClassDef(_)))
            if arguments.args.is_empty() && arguments.keywords.is_empty()
    )
}

/// Where the parentheses around a statement's list start, an opening one and a closing one,
/// where `CPython`'s tree does not record them: `from x import (a, b)` imports what `from x
/// import a, b` does, and `with (open(p), open(q)):` opens what the same without them does.
/// Neither is around an expression, so neither is found as one that only wraps an expression.
fn statement_parentheses(node: AnyNodeRef<'_>, tokens: &Tokens) -> Option<[TextSize; 2]> {
    let (first, last) = match node {
        AnyNodeRef::StmtImportFrom(import) => match (import.names.first(), import.names.last()) {
            (Some(first), Some(last)) => (first.range(), last.range()),
            _ => return None,
        },
        AnyNodeRef::StmtWith(with) => match (with.items.first(), with.items.last()) {
            (Some(first), Some(last)) => (first.range(), last.range()),
            _ => return None,
        },
        _ => return None,
    };
    let open = tokens
        .after(node.range().start())
        .iter()
        .take_while(|token| token.start() < first.start())
        .find(|token| token.kind() == TokenKind::Lpar);
    // After the last name or item, up to the end of the import or the `:` of the `with`.
    let close = tokens
        .after(last.end())
        .iter()
        .take_while(|token| token.start() < node.range().end() && token.kind() != TokenKind::Colon)
        .find(|token| token.kind() == TokenKind::Rpar);
    Some([open?.start(), close?.start()])
}

/// For an f-string or t-string field that prints itself, `{v = }`, the ranges its `{` and `=`
/// tokens take in, so that the spacing around the expression, which is printed, takes part in a
/// match: the `{` with the text before the expression, and the `=` with the text from the
/// expression to the conversion, the format spec or the `}`.
fn self_documenting(
    node: AnyNodeRef<'_>,
    tokens: &Tokens,
    widened: &mut Vec<(TextSize, TextRange)>,
) {
    let AnyNodeRef::InterpolatedElement(element) = node else {
        return;
    };
    let Some(debug) = &element.debug_text else {
        return;
    };
    let open = element.range().start();
    let after = element.expression.range().end();
    widened.push((
        open,
        TextRange::at(open, TextSize::of(debug.leading()) + TextSize::new(1)),
    ));
    if let Some(equal) = tokens
        .after(after)
        .iter()
        .take_while(|token| token.start() < element.range().end())
        .find(|token| token.kind() == TokenKind::Equal)
    {
        widened.push((
            equal.start(),
            TextRange::at(after, TextSize::of(debug.trailing())),
        ));
    }
}

/// Where each parenthesis around `expr` that takes no part in a match starts, an opening one
/// and a closing one.
///
/// Parentheses around an expression that its parent's syntax does not need - `f((x))`'s inner
/// pair - change nothing.  Where they change the meaning, the tree changes too - `f((a, b))`
/// passes one argument and `f(a, b)` two.  A tuple's or a generator's own are
/// [`own_parentheses`].
fn redundant_parentheses(
    expr: ExprRef<'_>,
    parent: AnyNodeRef<'_>,
    tokens: &Tokens,
    found: &mut Vec<TextSize>,
) {
    for around in parentheses_iterator(expr, Some(parent), tokens) {
        found.extend([around.start(), around.end() - TextSize::new(1)]);
    }
}

/// Where a tuple's or a generator's own parentheses start, which take no part in a match: Ruff
/// counts them among the node's tokens rather than the redundant, but `a, b` and `(a, b)` are
/// one tree to `CPython`, as are `f(x for x in y)` and `f((x for x in y))`.  An empty tuple's are
/// its only spelling, and stay.
fn own_parentheses(node: AnyNodeRef<'_>) -> Option<[TextSize; 2]> {
    let wears_its_own = match node {
        AnyNodeRef::ExprTuple(tuple) => tuple.parenthesized && !tuple.elts.is_empty(),
        AnyNodeRef::ExprGenerator(generator) => generator.parenthesized,
        _ => false,
    };
    wears_its_own.then(|| [node.range().start(), node.range().end() - TextSize::new(1)])
}

/// Where each token that a `(` comes straight before starts, read with a cursor.
struct Opened {
    starts: Vec<TextSize>,
    next: usize,
}

impl Opened {
    /// Whether a `(` comes straight before the token starting at `start`.  The walk asks in
    /// source order, so the cursor only moves forward; asked out of order, it searches again.
    fn before(&mut self, start: TextSize) -> bool {
        if self.next > 0 && self.starts[self.next - 1] >= start {
            self.next = self.starts.partition_point(|s| *s < start);
        }
        while self.starts.get(self.next).is_some_and(|s| *s < start) {
            self.next += 1;
        }
        self.starts.get(self.next) == Some(&start)
    }
}

/// Where each token that a `(` comes straight before starts, comments and line breaks inside
/// brackets aside, as Ruff's [`parentheses_iterator`] reads them: the only places an expression
/// can start inside parentheses of its own.  In source order.
fn after_opening_parentheses(tokens: &Tokens) -> Vec<TextSize> {
    let mut opened = Vec::new();
    let mut after_open = false;
    for token in tokens.iter().filter(|token| !token.kind().is_trivia()) {
        if after_open {
            opened.push(token.start());
        }
        after_open = token.kind() == TokenKind::Lpar;
    }
    opened
}

/// The walk [`build`] makes of Ruff's tree, building the arena and noting the layout among
/// the tokens as it goes.
struct Build<'text, 'a> {
    builder: Builder<'text>,
    tokens: &'a Tokens,
    source: &'a str,
    /// The nodes entered and not yet left, so each node knows its parent.
    open: Vec<AnyNodeRef<'a>>,
    /// Where each token straight after a `(` starts.
    opened: Opened,
    /// Where each redundant parenthesis starts: an opening one, and a closing one.
    redundant: Vec<TextSize>,
    /// Where each one-element tuple's comma starts, which is the tuple, not layout.
    tuple_commas: Vec<TextSize>,
    /// Where each comma that ends a tuple of several elements starts, which is layout.
    trailing_commas: Vec<TextSize>,
    /// Tokens that take in the text around them, by where each starts.
    widened: Vec<(TextSize, TextRange)>,
}

impl<'a> Build<'_, 'a> {
    /// Notes the parentheses, commas and widened tokens in or around `node` that a match
    /// treats as layout or reads differently from Ruff.
    fn note_layout(&mut self, node: AnyNodeRef<'a>, parent: Option<AnyNodeRef<'a>>) {
        // Ruff's search for the parentheses around an expression costs three binary
        // searches over the file's tokens, and finds nothing unless a `(` comes straight
        // before it, which few do.
        if let (Some(expr), Some(parent)) = (node.as_expr_ref(), parent)
            && self.opened.before(expr.start())
        {
            redundant_parentheses(expr, parent, self.tokens, &mut self.redundant);
        }
        // Each of the rest applies to one or two kinds of node, so only those ask.
        match node {
            AnyNodeRef::ExprTuple(_) => {
                self.redundant
                    .extend(own_parentheses(node).into_iter().flatten());
                self.tuple_commas.extend(tuple_comma(node, self.tokens));
                self.trailing_commas
                    .extend(trailing_tuple_comma(node, self.tokens));
            }
            AnyNodeRef::ExprGenerator(_) => {
                self.redundant
                    .extend(own_parentheses(node).into_iter().flatten());
            }
            AnyNodeRef::StmtImportFrom(_) | AnyNodeRef::StmtWith(_) => {
                self.redundant.extend(
                    statement_parentheses(node, self.tokens)
                        .into_iter()
                        .flatten(),
                );
            }
            AnyNodeRef::InterpolatedElement(_) => {
                self_documenting(node, self.tokens, &mut self.widened);
            }
            _ => {}
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for Build<'_, 'a> {
    fn enter_node(&mut self, node: AnyNodeRef<'a>) -> TraversalSignal {
        let parent = self.open.last().copied();
        if empty_class_arguments(node, parent) {
            self.redundant
                .extend([node.range().start(), node.range().end() - TextSize::new(1)]);
            return TraversalSignal::Skip;
        }
        self.builder
            .enter(kind_of(node.kind()), node.range().into());
        self.note_layout(node, parent);
        if let Some(content) = string_content(node, self.source) {
            self.builder.value(content);
        }
        self.open.push(node);
        // Ruff's source-order walk visits a format spec's parts but never enters the spec
        // itself (`InterpolatedElement::visit_source_order`), so the spec would be missing
        // from the arena, its parts hung on the element.  The element's children are walked
        // here instead, with the spec entered around its parts, in the order Ruff's walk
        // uses: the expression, then the spec.
        if let AnyNodeRef::InterpolatedElement(element) = node
            && let Some(spec) = &element.format_spec
        {
            self.visit_expr(&element.expression);
            self.builder.enter(
                kind_of(NodeKind::InterpolatedStringFormatSpec),
                spec.range().into(),
            );
            for part in &spec.elements {
                walk_interpolated_string_element(self, part);
            }
            self.builder.leave();
            return TraversalSignal::Skip;
        }
        TraversalSignal::Traverse
    }

    fn leave_node(&mut self, node: AnyNodeRef<'a>) {
        // Ruff's walk leaves a node it was told to skip, which was never entered.
        if empty_class_arguments(node, self.open.last().copied()) {
            return;
        }
        self.open.pop();
        self.builder.leave();
    }

    // Each statement list - a module's, a function's, a branch's, an else or finally - is
    // marked, so that a run of statements or a sequence hole stays in one block.
    fn visit_body(&mut self, body: &'a [Stmt]) {
        self.builder.begin_list();
        walk_body(self, body);
        self.builder.end_list();
    }
}

/// The arena of `root` and every node beneath it, in one walk of Ruff's tree, with the tokens
/// a match compares: Ruff's own, less comments, line breaks, indentation and the parentheses
/// that only wrap an expression or that a tuple or generator wears.
fn build<'text>(
    mut builder: Builder<'text>,
    root: AnyNodeRef<'_>,
    tokens: &Tokens,
    source: &str,
) -> Arena<'text> {
    // The tokens kept are some of Ruff's, and a file seldom has more nodes than tokens, so this
    // is room enough without growing as the walk goes; where a file has more, the arena grows.
    builder.reserve(tokens.len(), tokens.len());
    let mut build = Build {
        builder,
        tokens,
        source,
        open: Vec::new(),
        opened: Opened {
            starts: after_opening_parentheses(tokens),
            next: 0,
        },
        redundant: Vec::new(),
        tuple_commas: Vec::new(),
        trailing_commas: Vec::new(),
        widened: Vec::new(),
    };
    walk_node(&mut build, root);
    let Build {
        mut builder,
        redundant,
        tuple_commas,
        trailing_commas,
        widened,
        ..
    } = build;
    let layout = Layout {
        redundant: sorted(redundant),
        tuple_commas: sorted(tuple_commas),
        trailing_commas: sorted(trailing_commas),
        widened: {
            let mut widened = widened;
            widened.sort_unstable_by_key(|(start, _)| *start);
            widened
        },
    };
    add_tokens(&mut builder, tokens, &layout);
    builder.finish()
}

/// `offsets` in order, each once, to be searched.  Collected into a list and sorted once
/// rather than kept in a set as they are found, which costs more for the handful a file has.
fn sorted(mut offsets: Vec<TextSize>) -> Vec<TextSize> {
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

/// What [`build`] found of a file's layout among its tokens, each list in order of where the
/// token starts.
struct Layout {
    /// Where each parenthesis that takes no part starts.
    redundant: Vec<TextSize>,
    /// Where each one-element tuple's comma starts, which is the tuple.
    tuple_commas: Vec<TextSize>,
    /// Where each comma ending a tuple of several elements starts, which is layout.
    trailing_commas: Vec<TextSize>,
    /// Tokens that take in the text around them, by where each starts.
    widened: Vec<(TextSize, TextRange)>,
}

/// Adds to `builder` the tokens a match compares: Ruff's own, less comments, line breaks,
/// indentation, the end of the file, semicolons, the parentheses that take no part, a comma
/// just before a closing bracket unless it is a one-element tuple's, and a comma ending a tuple.
fn add_tokens(builder: &mut Builder<'_>, tokens: &Tokens, layout: &Layout) {
    let mut meaningful = tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind(),
                TokenKind::Comment
                    | TokenKind::Newline
                    | TokenKind::NonLogicalNewline
                    | TokenKind::Indent
                    | TokenKind::Dedent
                    | TokenKind::EndOfFile
                    // A semicolon only separates statements, as a line break does.
                    | TokenKind::Semi
            )
        })
        .peekable();
    while let Some(token) = meaningful.next() {
        let kind = token.kind();
        if matches!(kind, TokenKind::Lpar | TokenKind::Rpar)
            && layout.redundant.binary_search(&token.start()).is_ok()
        {
            continue;
        }
        // A comma just before a closing bracket - `f(x, y,)`, the comma Black adds to a call it
        // splits - changes nothing, whether or not the bracket itself takes part, as a tuple's
        // own does not.  A one-element tuple's comma is the tuple, so it stays: without it
        // `x[a,]` would read as `x[a]`.  A comma ending a tuple of several, `a, b,`, is layout.
        if kind == TokenKind::Comma
            && layout.tuple_commas.binary_search(&token.start()).is_err()
            && (layout.trailing_commas.binary_search(&token.start()).is_ok()
                || meaningful.peek().is_some_and(|next| {
                    matches!(
                        next.kind(),
                        TokenKind::Rpar | TokenKind::Rsqb | TokenKind::Rbrace
                    )
                }))
        {
            continue;
        }
        // An f-string's or t-string's opening holds its prefix and quotes, and its closing its
        // quotes; neither takes part, as a plain string's quotes take no part in its content.
        let compare = match kind {
            TokenKind::FStringStart => TokenText::Fixed("f-string start"),
            TokenKind::FStringEnd => TokenText::Fixed("f-string end"),
            TokenKind::TStringStart => TokenText::Fixed("t-string start"),
            TokenKind::TStringEnd => TokenText::Fixed("t-string end"),
            _ => TokenText::Source,
        };
        // Most files have no field that prints itself, so most never search.
        let range = if layout.widened.is_empty() {
            token.range()
        } else {
            layout
                .widened
                .binary_search_by_key(&token.start(), |(start, _)| *start)
                .map_or(token.range(), |at| layout.widened[at].1)
        };
        builder.token(range.into(), compare);
    }
}

/// A string or bytes literal's content, as the script language defines it for comparing: what
/// lies between each part's delimiters exactly as written, the parts of an implicit
/// concatenation joined.  The quotes and the prefix take no part.
fn string_content(node: AnyNodeRef<'_>, source: &str) -> Option<String> {
    let between = |range: TextRange, flags: &dyn Fn() -> (TextSize, TextSize)| {
        let (opener, closer) = flags();
        &source[usize::from(range.start() + opener)..usize::from(range.end() - closer)]
    };
    match node {
        AnyNodeRef::ExprStringLiteral(literal) => Some(
            literal
                .value
                .iter()
                .map(|part| {
                    between(part.range, &|| {
                        (part.flags.opener_len(), part.flags.closer_len())
                    })
                })
                .collect(),
        ),
        // A plain part of an f-string's concatenation compares by its content too; inside a
        // plain string or bytes literal the whole literal's value is compared first.
        AnyNodeRef::StringLiteral(part) => Some(
            between(part.range, &|| {
                (part.flags.opener_len(), part.flags.closer_len())
            })
            .to_owned(),
        ),
        AnyNodeRef::BytesLiteral(part) => Some(
            between(part.range, &|| {
                (part.flags.opener_len(), part.flags.closer_len())
            })
            .to_owned(),
        ),
        AnyNodeRef::ExprBytesLiteral(literal) => Some(
            literal
                .value
                .iter()
                .map(|part| {
                    between(part.range, &|| {
                        (part.flags.opener_len(), part.flags.closer_len())
                    })
                })
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    fn names(kinds: &BTreeSet<Kind>) -> Vec<&'static str> {
        kinds.iter().map(|k| k.name()).collect()
    }

    /// Each node as its kind and its text, indented by depth, so a tree reads as it nests.
    fn outline(arena: &Arena<'_>) -> Vec<String> {
        fn visit(
            arena: &Arena<'_>,
            node: quaffed_syntax::NodeId,
            depth: usize,
            out: &mut Vec<String>,
        ) {
            out.push(format!(
                "{}{} {:?}",
                "  ".repeat(depth),
                arena.kind(node),
                arena.source(node)
            ));
            for child in arena.children(node) {
                visit(arena, child, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        visit(arena, arena.root(), 0, &mut out);
        out
    }

    #[test]
    fn a_module_is_an_arena_of_ruffs_nodes_in_source_order() {
        let arena = parse_module("def f(x):\n    return x\n").unwrap();
        assert_eq!(arena.language(), PYTHON);
        assert_eq!(
            outline(&arena),
            [
                "ModModule \"def f(x):\\n    return x\\n\"",
                "  StmtFunctionDef \"def f(x):\\n    return x\"",
                "    Identifier \"f\"",
                "    Parameters \"(x)\"",
                "      ParameterWithDefault \"x\"",
                "        Parameter \"x\"",
                "          Identifier \"x\"",
                "    StmtReturn \"return x\"",
                "      ExprName \"x\"",
            ]
        );
        // A module's root runs to the end of the text, its final newline included.
        assert_eq!(arena.leading(), "");
        assert_eq!(arena.trailing(), "");
    }

    /// Where Ruff's root starts and ends, as observed: a module's runs from the start of the
    /// text to its end, and an expression's from its first token to the end, so blank lines
    /// before an expression are the arena's leading text.
    #[test]
    fn ruffs_root_spans_the_text_but_blank_lines_before_an_expression() {
        type Parse = fn(&str) -> Result<Arena<'_>, ParseError>;
        let cases: [(&str, Parse, Range<usize>); 6] = [
            ("(a)  # c\n", parse_expression, 0..9),
            ("\n\n(a)\n\n", parse_expression, 2..7),
            ("# c\n\nx = 1  # d\n\n", parse_module, 0..17),
            ("", parse_module, 0..0),
            ("   \n", parse_module, 0..4),
            ("%ls\n# c\n", parse_ipython, 0..8),
        ];
        for (text, parse, root) in cases {
            let arena = parse(text).unwrap();
            assert_eq!(arena.range(arena.root()), root, "{text:?}");
            assert_eq!(
                [
                    arena.leading(),
                    arena.source(arena.root()),
                    arena.trailing()
                ]
                .concat(),
                text
            );
        }
    }

    #[test]
    fn a_module_reports_every_kind_in_it_and_no_others() {
        let found = kinds_in_module("def f(x):\n    return x\n").unwrap();
        assert_eq!(
            names(&found),
            [
                "ExprName",
                "Identifier",
                "ModModule",
                "Parameter",
                "ParameterWithDefault",
                "Parameters",
                "StmtFunctionDef",
                "StmtReturn",
            ]
        );
    }

    #[test]
    fn an_expression_is_parsed_as_one() {
        let found = kinds_in_expression("f(1)").unwrap();
        assert_eq!(
            names(&found),
            [
                "Arguments",
                "ExprCall",
                "ExprName",
                "ExprNumberLiteral",
                "ModExpression"
            ]
        );
        assert!(kinds_in_expression("x = 1").is_err());
    }

    #[test]
    fn ipython_escapes_parse_only_as_ipython() {
        let found = kinds_in_ipython("%timeit f()\nfiles = !ls\n").unwrap();
        assert!(names(&found).contains(&"StmtIpyEscapeCommand"), "{found:?}");
        assert!(names(&found).contains(&"ExprIpyEscapeCommand"), "{found:?}");
        assert!(kinds_in_module("%timeit f()\n").is_err());
    }

    #[test]
    fn a_format_spec_is_a_node_around_its_parts() {
        let arena = parse_expression("f\"{x:>{w}}\"").unwrap();
        assert_eq!(
            outline(&arena),
            [
                "ModExpression \"f\\\"{x:>{w}}\\\"\"",
                "  ExprFString \"f\\\"{x:>{w}}\\\"\"",
                "    FString \"f\\\"{x:>{w}}\\\"\"",
                "      InterpolatedElement \"{x:>{w}}\"",
                "        ExprName \"x\"",
                "        InterpolatedStringFormatSpec \">{w}\"",
                "          InterpolatedStringLiteralElement \">\"",
                "          InterpolatedElement \"{w}\"",
                "            ExprName \"w\"",
            ]
        );
        let without = kinds_in_expression("f\"{x}\"").unwrap();
        assert!(
            !names(&without).contains(&"InterpolatedStringFormatSpec"),
            "{without:?}"
        );
    }

    #[test]
    fn invalid_source_is_an_error_naming_where() {
        let err = kinds_in_module("def f(:\n").unwrap_err();
        assert_eq!(
            err,
            ParseError::Invalid {
                message: "Expected a parameter or the end of the parameter list".to_owned(),
                offset: 6
            }
        );
        assert_eq!(
            err.to_string(),
            "at byte 6: Expected a parameter or the end of the parameter list"
        );
    }

    #[test]
    fn text_too_long_for_an_arena_is_refused_before_ruff_sees_it() {
        // Invalid Python, so a parse would fail differently: the refusal comes first.
        let refused: Result<Builder<'_>, TooLong> = Err(TooLong { len: 6 });
        assert_eq!(
            parse_from(refused, "def (:", Mode::Module).unwrap_err(),
            ParseError::TooLong(TooLong { len: 6 })
        );
        let err = ParseError::from(TooLong { len: 5_000_000_000 });
        assert_eq!(
            err.to_string(),
            "the text is 5000000000 bytes, more than the 4294967295 a parsed file may hold"
        );
        assert!(std::error::Error::source(&err).is_some());
        let invalid = parse_module("def (:").unwrap_err();
        assert!(std::error::Error::source(&invalid).is_none());
    }

    #[test]
    fn invalid_ipython_is_an_error_too() {
        assert!(matches!(
            parse_ipython("%ls\ndef (:\n"),
            Err(ParseError::Invalid { .. })
        ));
    }
}
