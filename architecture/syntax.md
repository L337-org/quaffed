# The syntax arena

`crates/syntax`, the `quaffed-syntax` crate: a parsed file as node kinds and byte ranges over
its text, **the same shape for every language**.  Each language's adapter parses with the parser
chosen for that language and builds an arena.  The rule for the engine - matching, rewriting,
the invariant checks - is that it reads arenas and never a parser's own types.  This crate
depends on nothing, so a second language's adapter depends on it and not on the Python adapter
or its parser; `tests/no_dependencies.rs` fails on any dependency it does not name.

## The shape

- **Nodes in pre-order.**  An arena is one vector: each node holds its kind, its byte range and
  the index just past its subtree.  A node's first child is the next node, and its next sibling
  is found by skipping a subtree, so a walk is a scan of the vector.
- **A kind is the parser's own name** for it - `StmtFunctionDef` for Python - never a name
  copied by hand, so an adapter's kind list is held to its parser.  An arena carries its
  language once, and kinds are compared only within one language.
- **A node's identity is its index**, a `NodeId`, meaningful only in that arena.  Nothing
  survives an edit and a re-parse.
- **Tokens beside the nodes.**  The tokens a match compares - the parser's own, less what
  carries no meaning - are a flat array of their own, sorted by position, so the node layout
  does not depend on them.  Each is compared by its text, or by a fixed text the adapter gives
  it where its spelling takes no part: an f-string's opening, whatever its prefix and quotes.
  What is left out is the adapter's to decide, by the rule *Matching* below.
- **A value in place of children.**  A node may carry a value it compares by instead of its
  parts: a string literal's content, so that its quotes and prefix take no part.

## What the ranges mean

**Byte ranges index the text the parser was given**, which for a decoded file is its decoded
UTF-8 text, after any byte-order mark - not its bytes on disk.  Turning text back into a file's
bytes is the encoding layer's job (`encoding.md`), so no adapter maps offsets through a codec.

The ranges keep three invariants, and an adapter that breaks one is defective:

1. every node's range lies inside the text;
2. a node's children are in source order and do not overlap;
3. each child lies inside its parent.

**The root's range is whatever the parser gives**, which need not be the whole text.  The text
before and after it is the arena's `leading` and `trailing` text.  No node is invented to cover
the whole file, because that would be a kind no parser gives.

So **every byte of the text is in exactly one place**: the leading text, the trailing text, a
node with no children, or the text between a node's edges and its children.  The round trip in
`verification.md` prints an arena from those places and requires the file's bytes back, which
is what holds an adapter to the invariants.

## Offsets are 32-bit

Offsets are `u32`, as the parsers' own are.  `Builder::new` refuses text longer than that with
`TooLong`, naming its length, before the parser sees it, so a file too long to parse is refused
whole rather than partly searched or edited.  This limits structural work only: a textual search
never builds an arena.

## Patterns

A `Pattern` is a snippet of a language compiled into an arena of its own, with some of its
nodes marked as holes: each with the name it binds, if any, and whether it stands for one node
or a sequence.  A hole is a node of the pattern's arena where the hole was written, so the
matcher reads a pattern with the same walks as a file.  Compiling one is the language's job,
because how a hole survives the language's parser differs between languages; `python.md`
describes Python's.  A pattern owns its text, through `Arena::into_owned`, so it outlives the
text it was compiled from.

## Matching

`find(pattern, arena)` gives every place a pattern's shape occurs, in source order, a match
inside another match among them.  Two nodes match when they are of one kind and their **parts**
match in order, a part being a child or a token between children; a node with a value matches
by its value alone.  A hole matches any node; a sequence hole one or more consecutive children
of one node, with the tokens between them; and a name used twice matches only where both hold
equal code, by the same comparison.  A pattern of one node matches any node of that shape; one
of several - statements in a row - matches them standing consecutively among one node's
children.

**What takes no part is what the adapter leaves out of the tokens**: comments, whitespace, line
breaks, indentation, and parentheses that only wrap an expression, so `f((x))` matches `f($a)`
but `(a + b) * c` does not match `a + b * c`, whose tree differs.  The matcher compares what the
arena holds and knows no language; an adapter decides, for its language, which tokens carry
meaning.

## Building one

An adapter makes a `Builder` over the text, then walks its parser's tree once, calling `enter` with
a node's kind and range, then its children, then `leave`.  `finish` gives the arena.  A second root,
a `leave` with nothing open, or a node left open is a defect in the adapter and panics.  `value`
gives the node open now its value, and `token` adds a token, in source order.  The builder does not
check the range invariants, which would cost every parse; the round trip checks them over both
corpora.

## Adding a language

1. Choose the parser for that language on its merits; there is no common parsing substrate.
2. Give it its own adapter crate, the only one allowed to name that parser's types, and hold
   that with a containment test as `ruff_containment.rs` does for Ruff.
3. List the parser's kinds in the adapter, held to the parser's own enumeration by the
   compiler where it has one.
4. Build arenas with `Builder`, keeping the range invariants above, and check them with a round
   trip over a corpus for that language.
5. Keep the tokens that carry meaning in that language, give a string literal its content as
   its value, and compile patterns, as `python.md` does for Python; hold the result with
   matching tests and properties like Python's.
