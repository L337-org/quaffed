# The syntax arena

`crates/syntax`, the `quaffed-syntax` crate: a parsed file as node kinds and byte ranges over
its text, **the same shape for every language**, and the front end, **the one interface through
which the engine reaches a language**.  Each language's adapter parses with its own parser and
builds an arena; the engine - matching, rewriting, the invariant checks - reads arenas and never a
parser's own types.  So this crate depends on no parser, and a second language's adapter depends
on it rather than on the Python adapter or its parser; `tests/no_dependencies.rs` fails on any
dependency its `ALLOWED` does not name, which is where a dependency that is not a parser, such
as quaff's own reading of a file's bytes, is added deliberately.

## The shape

- **Nodes in pre-order.**  An arena is one vector; each node holds its kind, its byte range and
  the index just past its subtree, so a walk is a scan.
- **A kind is the parser's own name** - `StmtFunctionDef` for Python - so an adapter's kind list
  is held to its parser.  An arena carries its language, and kinds are compared only within one.
- **A node's identity is its index**, a `NodeId`, meaningful only in that arena.  Nothing
  survives an edit and a re-parse.
- **Tokens beside the nodes.**  The tokens a match compares are a flat array sorted by position,
  each compared by its text or by a fixed text the adapter gives it where its spelling takes no
  part, such as an f-string's opening.  Which tokens are kept is the adapter's choice, by the rule
  under *Matching*.
- **Lists among children.**  An adapter marks each list among a node's children, such as a
  block's statements, so that a sequence hole or a run of statements stays inside one.
- **A value in place of children.**  A node may compare by a value instead of its parts: a string
  literal's content, so its quotes and prefix take no part.

## What the ranges mean

**Byte ranges index the text the parser was given**: a decoded file's UTF-8 text, after any
byte-order mark, not its bytes on disk.  Turning text back into bytes is the encoding layer's job
(`encoding.md`), so no adapter maps offsets through a codec.

An adapter must keep three invariants:

1. every node's range lies inside the text;
2. a node's children are in source order and do not overlap;
3. each child lies inside its parent.

**The root's range is whatever the parser gives**, not necessarily the whole text; the text
before and after it is the arena's `leading` and `trailing` text, and no node is invented to
cover the file.  So every byte is in exactly one place - the leading or trailing text, a
childless node, or between a node's edges and its children - and the round trip
(`corpus.md`) prints an arena from those places and requires the file's bytes back, which
is what holds an adapter to the invariants.

## Offsets are 32-bit

Offsets are `u32`, as the parsers' own are.  `Builder::new` refuses longer text with `TooLong`
before the parser sees it, so a file too long is refused whole rather than partly searched or
edited.  A textual search never builds an arena, so is not limited.

## Patterns

A `Pattern` is a snippet compiled into an arena of its own, with some nodes marked as holes -
each with the name it binds, if any, and whether it stands for one node or a sequence - so the
matcher reads a pattern with the same walks as a file.  Compiling one is the language's job,
because how a hole survives a parser differs between languages (`python.md` for Python).  A
pattern owns its text (`Arena::into_owned`).  A node no text parses to on its own, such as a
decorator, is parsed inside the code it needs and lifted out of it (`Arena::lifted`): the lifted
nodes become the root's only children, in one list, with their subtrees, tokens and values.

## Matching

`find(pattern, arena)` gives every place a pattern's shape occurs, in source order, nested matches
included.  `src/matching.rs` holds the details; its module comment is the place to start.

- Two nodes match when they are of one kind and their **parts** - children, and tokens between
  them - match in order.  A node with a value matches by its value alone (compared with its kind,
  so a string and a bytes literal with the same content differ), and one with nothing inside it
  by its text.
- A hole matches any node; a sequence hole one or more consecutive children of one node, in one
  marked list, with the tokens between them.  A pattern of several statements matches them
  standing consecutively in one list.
- A name used twice matches only equal code: the same tokens, values and texts, nested the same
  way, whatever kinds of node hold them.  The nesting (`Code`) keeps apart trees whose tokens read
  alike once an adapter drops parentheses, such as `[a, (b, c)]` and `[a, b, c]`.
- **What takes no part is what the adapter leaves out of the tokens**: comments, whitespace,
  layout, and parentheses that only wrap an expression.  The matcher knows no language.

A sequence hole takes the fewest children that let the rest match, and at least one.  The
compiler enforces two limits, and `find` panics on a pattern that breaks either, as a defect in
its compiler:

- **A sequence hole may not start or end a run of statements** (`Pattern::unbounded_sequence`):
  nothing would bound it, so it would reach the edge of its block and an edit would replace it
  all.
- **Matching must stay at most quadratic in the code's length** (`Pattern::costly`): the matcher
  works out each state of a list's search once - a part of the pattern, a place in the code, and
  what the names the rest reads hold - so its work is the number of states.  `Cost` counts them
  as the matcher makes them: each depends on some of the places a search can go more than one
  way, a sequence's stop or where a run of statements starts, and a pattern is refused where a
  state depends on more than two.  It follows the matcher's own shortcuts: a node matches its
  whole list, so a sequence with nothing free after it ends where the node does; a plain search
  settles a sequence nothing reads at the first place the code after it stands; and a `where`
  carries every way of binding what it tests.  Two sequences standing together in a run of
  statements are refused too, since nothing settles how the statements divide between them; a
  language's compiler refuses them in every other list as well, where it knows which places are
  lists, but the engine does not.
  Unbounded choices would make this pattern matching with variables, which is NP-complete.  A
  fixed corpus of generated patterns records each verdict, and a property test holds what is
  accepted to quadratic growth over fresh ones: see the cost check's corpus in `testing.md`.

The matcher remembers outcomes rather than retrying every length (`List`), compares bound code
with hashes and a longest-common-prefix table (`Codes`, `Common`, and `Joint` for code in two
nodes), and gives every way a node can bind a name used after it (`Matcher::node_ways`).  **How
long each memory is kept is part of what the cost check counts**, so a change to either is a
change to both:

- A plain search keeps an outcome with no name bound for the whole search, and one with a name
  bound only for the start it was found at, so a run's states that read a name are worked out
  again from every start.
- The ways a node's or a run's rest can bind (`Matcher::ways`) are kept for every start among one
  node's parts.  That makes a `where` over a run quadratic in time where keeping them per start
  made it cubic, at the cost of memory quadratic in the block's length in the worst case, paid
  only by the patterns whose search would otherwise take the cube.

**A `where` over what a match binds** (`find_where`) uses the ways: the cost check counts the ways
it gathers of binding the names it tests (`Pattern::costly_with`), and a match is kept if any way
of binding them satisfies it, never only the way found first.  **Plain backtracking is kept as
the reference**: property tests (`remembered_against_backtracking` and `trees` in
`src/matching.rs`) require both to find the same matches, and the tree property also holds both
to an independent `Oracle`.  Named tests beside them hold each case where a shortcut must not
apply, and the work's growth, counted rather than timed.

**What a match captured** (`find_captured`, `find_where_captured`) is the code each named hole
bound there, for an edit to write back.  **A name asked for only for its capture never steers
the search**: only a name used more than once, or tested by a `where`, is compared, and one only
kept is recorded where the search went.  So asking for captures never changes the matches.  It
adds work in one place only, and only once a match is found: a node matched by its ways of
binding a name used after it (`Matcher::node_ways`) binds only that name, so it is matched once
more to record what it captured.  Where no name is compared the work is unchanged; elsewhere the
property tests above bound it by `CaptureCheck::holds`, and the cost check needs no change for
it.  Where a name could capture different code at one match, it captures what the search settles
on, each sequence taking the fewest nodes that let the rest match.  With a `where`, the captures
are those of the way it accepted: where that is not the first way found, the match is made again
from the same place by plain backtracking, each tested name first binding the very node that way
bound it to, not any with equal code, since a `where` can tell such nodes apart.

## Building one

An adapter makes a `Builder` over the text and walks its parser's tree once: `enter` with a
node's kind and range, its children, then `leave`; `finish` gives the arena.  `value`, `token`,
`begin_list` and `end_list` add the rest, tokens in source order, and `reserve` makes room up
front.  A second root, a `leave` with nothing open, or a node left open panics as a defect in the
adapter.  The builder does not check the range invariants, which would cost every parse; the round
trip checks them over both corpora.

## The front end

**A language is reached only through its `FrontEnd`** (`src/front.rs`), which its adapter
implements once, whatever tools it uses to answer: Python's uses Ruff, and quaff's own reading of
a Python file's bytes in `quaffed-encoding`.  It asks, of text in that language:

- which files are its own, by name alone (`claims`, from `extensions`);
- what encoding a file declares in its own text, and how its bytes are decoded, as the language's
  own reader would (`declared_encoding`, `decode`);
- to parse a file into an arena, and to compile a pattern and a rewrite (`parse`,
  `compile_pattern`, `compile_rewrite`);
- what a rewrite needs of the text beyond the arena: its comments, strings and brackets, its code
  tokens, how a value is written into a string, and what stands outside the construct it needs
  (`lex`, `code_tokens`, `token_kinds`, `escaped`, `misplaced`).

**Only quaff's own types cross it**, defined beside it or in `quaffed-encoding`: an arena, a
`Pattern`, a `Rewrite`, a `Lexed`, a decoded `Source`, a `Declared` encoding naming what the
language calls its declaration, and errors as a `ParseError`, a `CompileError` of message and
place, or a `Refused` in the language's own words, so the engine never sees an adapter's type and
never words a language's refusal for it.  Every question but `claims` takes text rather than a
file, so the same questions can be asked of code embedded in another language's file once a
scope can name it.

**A question added later has a default answer**, so every language already written compiles
unchanged.  A default is given only where an honest one exists, such as declaring no encoding,
and one that leaves something undone makes the engine refuse rather than guess: writing nothing
into a string refuses such an edit.  A language with only a parser implements the questions that
have no default and takes the rest.  A type an adapter builds is
built through a constructor where its fields may grow (`Rewrite::new`, `CompileError::new`);
every crate is in one workspace, so any other change to what crosses is a compile error in the
adapters, fixed in the same change.

**The binary names an adapter in one file**, `crates/quaffed/src/languages.rs`, whose `LANGUAGES`
lists each language's front end in the order the help gives them; `tests/adapter_containment.rs`
fails on any other file of the binary that names one.  A run finds each file's language there by
the file's name, and compiles every pattern for every language: `search.md` says what a pattern
means where files of several languages are in scope.

## Adding a language

1. Choose the parser for that language on its merits; there is no common parsing substrate.
2. Give it its own adapter crate, the only one allowed to name that parser's types, held with a
   containment test as `ruff_containment.rs` does for Ruff.
3. List the parser's kinds in the adapter, held to the parser's own enumeration by the compiler
   where it has one.
4. Build arenas with `Builder`, keeping the range invariants, and check them with a round trip
   over a corpus for that language.
5. Keep the tokens that carry meaning in that language, give a string literal its content as its
   value, and compile patterns, as `python.md` does for Python; hold the result with matching
   tests and properties like Python's.
6. Refuse a pattern `Pattern::unbounded_sequence` or `Pattern::costly` names, with an error that
   says where the hole is, since `find` panics on one.
7. Implement `FrontEnd` for it in the adapter, answering each question in the language's own
   terms.  Take a default only where it is the truth for the language.  Where the language
   declares its encoding in its own text, answer `declared_encoding` and `decode` as its own
   reader does, with refusals in its own words and its declaration named as its users know it,
   and describe the rule in `encoding.md` beside Python's.
8. Add its front end to `LANGUAGES` in `crates/quaffed/src/languages.rs`, its adapter to the
   binary's `Cargo.toml`, and the adapter's crate name to `ADAPTERS` in
   `crates/quaffed/tests/adapter_containment.rs`, which fails on a `quaffed-*` dependency of the
   binary that is in neither its `ADAPTERS` nor its `ENGINE`.  The test in `languages.rs` fails
   if a language claims a file named by another's extensions; a front end that overrides
   `claims` beyond its extensions needs a test of its own that it claims no other language's
   files.  Nothing else in the binary changes: every file is found among `LANGUAGES`, and every
   pattern is compiled for each.
9. List it, with the extensions that name its files, as a row of the table under _Which files_ in
   `docs/language.md`, with what only it does under a heading of its own there;
   `tests/language_guide.rs` fails until the table agrees with the help, which prints
   `LANGUAGES`.  Nothing else quaff says names a language, as `AGENTS.md` asks, so the rest of
   the help, the messages and the guide need no change.
10. Give it a corpus and an oracle for the edit invariants, by the checklist in `corpus.md`.
11. Check the rewriter's own rules against the language: where `src/rewrite.rs` or
    `src/rewrite/lines.rs` spells a kind name of one parser, or holds a rule about statements or
    blocks that the language does not share, the rule has to be asked of the front end before
    the language's files are edited.
