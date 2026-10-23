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
- **Lists among children.**  A node's children may hold several lists - a function's name and
  parameters, then its body's statements - so an adapter marks each list, such as a block's
  statements, and a sequence hole or a run of statements stays inside one.
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

`find(pattern, arena)` gives every place a pattern's shape occurs, in source order, a match inside
another match among them.  Two nodes match when they are of one kind and their **parts** match in
order, a part being a child or a token between children; a node with a value matches by its value
alone, and one with nothing inside it at all by its text.  A hole matches any node; a sequence hole
one or more consecutive children of one node, in one list, with the tokens between them; and a name
used twice matches only where both hold equal code - the same tokens, values and texts in order,
nested the same way, whatever kinds of node hold them, so the same name matches as an argument, a
keyword, an attribute or a parameter.  The nesting is each child of more than one part marking
where it begins and ends: without it, once an adapter leaves a tuple's parentheses out, `[a, b]`
and `[a, (b, c)]`'s tokens would read alike to `[a, b, c]`'s.  A child of one part marks nothing,
so a name, which one node holds as an argument and another as an attribute, still compares
alike.  A pattern of one node matches any node of that shape; one of several - statements
in a row - matches them standing consecutively in one marked list.

**What takes no part is what the adapter leaves out of the tokens**: comments, whitespace, line
breaks, indentation, and parentheses that only wrap an expression, so `f((x))` matches `f($a)`
but `(a + b) * c` does not match `a + b * c`, whose tree differs.  The matcher compares what the
arena holds and knows no language; an adapter decides, for its language, which tokens carry
meaning.

A run of statements is matched once from each place it could start, as far as the pattern
goes, and every sequence hole takes the fewest children that let the rest match, and at least
one.  **A sequence hole may not start or end a run of statements**: nothing in the pattern
would bound it on that side, so it would reach the edge of its block - at a module's top level,
most of a file - in one match whose count every check would agree with, and an edit would
replace it all.  Taking the fewest at an edge would make it one statement, `$_` under another
name, and taking until the next match begins still reaches the edge when there is one
occurrence, so neither makes an edge hole both safe and useful.  `Pattern::unbounded_sequence`
names such a hole; every language's compiler refuses the pattern, and `find` panics on one as a
defect in its compiler.  A sequence inside one node - a call's arguments, a function's body - is
bounded by that node, and between two statements by them.

**A list of parts with a sequence in it is matched by remembering, not by trying every length
again** (`List` in `src/matching.rs`).  That is a node's parts and the run of statements a
pattern of several is, alike.  The parts are matched in order, a sequence taking the fewest parts
that let the rest match, as plain backtracking would.  The outcome of matching the rest of the
list from each pair of places - a part of the pattern, a part of the code - is remembered with
what it bound, so that no pair is worked out twice.  The outcome depends only on that pair and on
what the names the rest uses are bound to, so those are part of what it is remembered by.  A name
used once binds nothing, since nothing is compared with it.  Outcomes that depend on a binding
are forgotten at each start - each node a pattern of one node is tried at, each start of a run -
since another start binds the names to other code; the rest are kept for the whole search.

**A sequence stops only where the code after it could stand**: where the fixed parts up to the
next sequence could match whatever names are bound, found once per list, nearest first.  That is
judged as a match would judge it, but with every hole matching anything and a node holding a
sequence judged by its kind alone, since a name bound outside can change how much that sequence
takes; judging by a match with nothing bound would miss places a binding opens.  **A sequence
gives up at once when some later fixed code could stand nowhere after it.**  **And a failure
where the code stands settles the later places too**, since whatever stands after a later place
also stands after this one, unless the place changes what the rest is matched with.  It does
when the fixed parts bind a name used further on, when the sequence after them has a name used
elsewhere and so holds different code from a later place, when the list must end where those
parts do with no sequence after them to take the difference, when another list follows - an
`else` clause's - which a later place could let the next sequence start in, and when a token
follows the parts, which the next sequence cannot start on.  Only then are the later places
tried.

**A sequence whose name is already bound is not searched for**: it must hold the same code
again, so its end is where the code from its start first equals what it bound.  The code of a
node's parts is numbered piece by piece once, with running totals of lengths, so that end is a
binary search (`Codes`).  Within one node's parts, whether the code from there equals the bound
run is read in constant time, exactly, as the longest common prefix of the two places' code: a
suffix array, the common prefix of each suffix with the one before it, and a sparse table of
their minimums (`Common`), made only for a node a name used twice is compared within.  Between
two nodes' parts, a hash comparison comes first and a match is confirmed in full, so a collision
costs a comparison, never a wrong match.  A named sequence binds the run it takes as a range of
its node's parts, so binding copies nothing.

**So the cost is about linear in a list's length** when nothing a choice binds is used further
on, and quadratic with one **choice**: a place where a name is bound in a way that varies with
where an earlier sequence stopped and is used later in the same list, or where a sequence named
elsewhere in the list starts.  The fixed code before a list's first sequence makes no choice,
since where it stands is where the list, or the run, starts.  Each further choice that the first
can affect would multiply the cost by the list's length again, and with no limit on how many
there are the problem is that of matching a pattern with variables, which is NP-complete, so no
algorithm can bound it.  **So two choices may not affect each other, and in a run of statements
two sequences may not stand together**, since nothing then settles how the statements divide
between them.  Two choices affect each other when they are in one list, or when one list uses a
name whose binding the other's choice varies; a list that uses none of another's varying names
is remembered by bindings that choice does not change, and is worked out once whatever it chose,
so `f(g($a..., x, $a...), h($b..., y, $b...))` is accepted.  `Pattern::costly` names the hole
that breaks either rule; every language's compiler refuses the pattern, and `find` panics on one
as a defect in its compiler.  Every pattern a compiler accepts is matched in at most quadratic
time in the length of each list it is matched in, with tokens between a list's parts or not.

**Plain backtracking is kept as the reference** the remembering matcher is held to: it tries
every length of each sequence from every place, and compares code piece by piece.  Three
property tests run both and require the same matches: over runs of statements in blocks split
in two, over a node's parts with sequences at either end and a token between statements or not,
and over generated trees - nodes within nodes, names shared between lists, tokens anywhere and
marked lists in the code.  Named tests hold each case where a shortcut must not apply, and two
hold the work's growth: a body of two thousand statements matched in seconds, and the work of a
name used twice over repeated code growing with the square of the block's length, counted rather
than timed.

## Building one

An adapter makes a `Builder` over the text, then walks its parser's tree once, calling `enter` with
a node's kind and range, then its children, then `leave`.  `finish` gives the arena.  A second root,
a `leave` with nothing open, or a node left open is a defect in the adapter and panics.  `value`
gives the node open now its value, `token` adds a token, in source order, and `begin_list` and
`end_list` mark a list among the open node's children.  The builder does not
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
6. Refuse a pattern `Pattern::unbounded_sequence` or `Pattern::costly` names, with an error that
   says where the hole is, since `find` panics on one.
