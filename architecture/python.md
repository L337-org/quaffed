# The Python adapter

`crates/python`, the `quaffed-python` crate: Python source through Ruff's parser, in quaff's own
terms.  **It is the only crate that may name a `ruff_*` type**, so that a Ruff upgrade, which
Ruff warns "will have frequent breaking changes", is absorbed here and reaches nothing else.
`crates/quaffed/tests/ruff_containment.rs` fails naming any other member that depends on a
`ruff_*` crate.

## What it offers

- **`Python`**: Python's front end, the `FrontEnd` (`syntax.md`) the engine reaches Python
  through, answering each question from what follows, and the coding line and decoding from
  `quaffed-encoding` (`src/front.rs`).  `crates/quaffed/src/languages.rs` is the only file of
  the binary that names it.  The types that cross it - `Lexed`, `StringPart`, `Bracket`,
  `Rewrite`, `Site`, `Misplaced` and `ParseError` - are `quaffed-syntax`'s, which the functions
  below return; `PatternError` and `RewriteError` stay here, and cross as a `CompileError` of
  message and place, and `quaffed-encoding`'s Python `Refusal` crosses as a `Refused` with its
  message, its coding line as a `Declared` named "coding line".
- **`parse_module`**, **`parse_expression`** and **`parse_ipython`**: source parsed into a syntax
  arena (`syntax.md`) of language `PYTHON`.  Source that does not parse is `ParseError::Invalid`,
  with Ruff's message verbatim and the byte offset; text too long for an arena is
  `ParseError::TooLong`.  Never a partial arena.
- **`compile_pattern`**: a structural pattern compiled into a `Pattern`, below.
- **`compile_rewrite`**: a structural `replace`'s rewrite template checked against its pattern,
  under *Rewrites*.
- **`lex`**, **`token_kinds`** and **`code_tokens`**: what a rewrite needs of Python text that
  the arena leaves out - its comments, its multi-line strings, its brackets with their trailing
  commas, its indent unit, where its logical lines start, its string parts with how each literal
  is written (`StringPart`), the kinds of its tokens and the text of its code tokens - from
  Ruff's lexer, so that the engine never names a token kind (`src/lex.rs`).
- **`escaped`**: a value written into a string part so that the literal holds it as text, or
  `None` where it cannot, as Python's string syntax decides (`src/lex.rs`).
- **`kinds_in_module`**, **`kinds_in_expression`**, **`kinds_in_ipython`** and **`all_kinds`**:
  the kinds an arena holds, and every kind the pinned Ruff knows, which the synthetic corpus's
  coverage check reads.
- **`Kind`**, re-exported from `quaffed-syntax`: a kind by Ruff's name for it, so nothing outside
  the adapter sees Ruff's own enum.

The arena is built in one walk of Ruff's tree (`src/parse.rs`).  It takes source as text:
decoding a file is `quaffed-encoding`'s (`encoding.md`), so the adapter and a textual search read
a file's bytes the same way.

## Patterns

`compile_pattern` turns a pattern into a `Pattern` (`syntax.md`), keeping the rules for holes
`script.md` gives.  A hole is not Python, so it takes two stages (`src/pattern.rs`):

1. **Claim before parsing.**  Every `$name`, `$_`, `$name...` and `$...` becomes a placeholder
   identifier on a stem the pattern does not contain.  A `$` not followed by a name or `...` is
   left as written, and a name ends where Python's would, by the identifier rules Ruff's lexer
   uses.
2. **Let the grammar decide.**  The result is parsed, as an expression if it is one and otherwise as
   statements, and a placeholder is a hole when a node spans exactly it, the outermost where several
   do.  A lambda's parameters have no brackets, so with one parameter the list and the parameter
   have one range; the list is passed over (`BARE_LIST`), so that one hole is one parameter, as in
   every list with brackets.  One that no node spans, or only a string's literal text, is inside a
   string or a comment: it is put back as written and the pattern parsed again.  A hole merged into
   a name, `x$a`, is put back too and fails to parse; one inside a dotted module name, `import
   a.$m`, is refused as `PatternError::InsideAName`.

**A decorator alone is not Python**, so a pattern that does not parse and starts with `@` is
parsed again with a definition after it to decorate (`DECORATED`), and its decorator is lifted
out of that definition to be the pattern's only content (`Arena::lifted`).  An error is still the
pattern's own.  Only one decorator: a definition's decorators are not marked as a list, so a run
of them could not be kept to one definition's, and several are refused
(`PatternError::SeveralDecorators`).  A rewrite template may be a decorator alone too, parsed the
same way for its sites.

**Which places are lists is Python's** (`src/lists.rs`): a block's statements, a call's arguments,
the parameters on each side of a `/` or `*`, and the rest that `lists`'s doc comment names, read
from the fields of Ruff's tree for the pattern.  A list follows the tree's shape, which is
CPython's: a comparison holds all its operands in one node, so they are a list and a sequence among
them takes the operators between, but a sum nests, `(x + y) + z`, so its operands are not.
Flattening a nested chain into a list would change what every pattern over it matches.  A sequence
hole must be one of a list's items, and is refused elsewhere (`PatternError::NotInAList`), since in
a place that holds one node it would also take whatever optional syntax follows it there.  Two
sequence holes that are consecutive items of one list are refused
(`PatternError::SequencesTogether`), since nothing says how many items each takes.  This is decided
as the pattern compiles, from a parse of its own text, so the arena and the matcher are unchanged;
the engine's own refusal of two sequences together in a run of statements (`syntax.md`) stays as its
guard.

`compile_pattern`'s doc comment lists what it refuses, including the two limits `syntax.md` sets
and one name used for both a node and a sequence.  An error from Ruff is placed in the pattern as
written, not in the text with placeholders.  Each error with a place carries it as a byte offset
into the pattern (`PatternError::offset`) and leaves it out of its message: the caller says where,
in the terms its reader knows.

## Rewrites

`compile_rewrite` (`src/rewrite.rs`) takes the `with` side of a structural `replace` and the
compiled pattern whose captures it writes back, and gives the template with each **site**: where a
captured name is written, and whether that is in code or in a literal.  A template with no code,
empty or only comments, compiles, and says so (`Rewrite::has_code`): it deletes what matched, and
what that takes from the file around a match is the edit's to decide.  It claims every `$` before
a name, `...` or a brace and substitutes placeholders exactly as `compile_pattern` does, so both
sides agree on where a `$` lands, but it reads **Ruff's tokens** to decide that, since comments
are not in the arena:

- **In code** - an identifier token that is exactly the placeholder, and a node of its own - a
  site writes back the captured code.  It must be a name the pattern binds, written as the pattern
  binds it, one node or a sequence.  `${name}` is accepted there too, though a name in code cannot
  run into text, so that one spelling means one thing everywhere.
- **In a string, an f-string's or t-string's literal part, or a comment**, `$name` and `${name}`
  interpolate the captured source text, the dots after `$name...` and a whole `$...` are text, and
  the name must be one the pattern binds.  A name runs to the end of the identifier, as on the
  match side, whatever is bound, so `$name_id` is the name `name_id` and `${name}_id` separates
  them: reading the longest bound name instead would let a typo or a forgotten binding write
  something silently.

Everything else after a `$` that is a name or a brace is an error, never literal text, so that a
typo or a value filter cannot reach a file as literal text, and a filter can be added
later without changing what any working script means.  A `$` before anything else is text.  What
is decided only once a value is known - whether interpolating it keeps the literal one token of
its kind, and whether the file still parses - is the edit's to check.

## Statements outside what they need

`misplaced` (`src/scope.rs`) finds each `return` or `yield` outside a function; `await`,
`async with`, `async for` or an asynchronous list, set or dict comprehension outside an `async`
one; and `break` or `continue` outside a loop's body.  It walks Ruff's tree with what each node
stands within.  Ruff's parser accepts all of them, and its own semantic checker needs a context
the caller keeps for it, so the walk keeps the little it needs itself.  Its doc comment lists
Python's rules, including where they bend: a generator expression may `await` in any function,
and a comprehension's first iterable, a definition's decorators, defaults and annotations, and
a loop's `else` are evaluated outside the construct.  The edit uses it to refuse code it would
leave with one of these out of place.  It covers only these: Python's compiler refuses other
code the parser accepts, such as a `nonlocal` with no binding, and those are not checked.

## What a match compares

**The arena keeps Ruff's tokens less those that carry no meaning in Python**, judged by whether
`CPython`'s tree records them: comments, line breaks, indentation, the end of the file,
semicolons between statements, a comma before a closing bracket or ending a tuple of several,
redundant parentheses, a tuple's or a generator's own parentheses, those around a `from` import's
names or a `with` statement's items, and a class's empty argument list.  Each has a function in
`src/parse.rs` whose doc comment gives the case, such as `redundant_parentheses`,
`own_parentheses`, `trailing_tuple_comma`, `statement_parentheses` and `empty_class_arguments`.
What stays is what changes the tree: an empty tuple's parentheses, and a one-element tuple's comma
(`tuple_comma`).

**A self-documenting field prints its own spacing**, so in `f"{v = }"` the spacing takes part
(`self_documenting`); in any other field it is layout.  An f-string's or t-string's opening and
closing compare by a fixed text, so prefix and quotes take no part.  A string or bytes literal's
value is its content as the script language defines it for comparing, an implicit concatenation
joined; an f-string's parts are compared one by one and not joined.

Each statement list Ruff's walk visits - a module's, a function's, a branch's, an `else` or a
`finally` - is marked as a list, so `$body...` never takes in an `else:` clause.

## The kind list is held to Ruff's enum by the compiler

Ruff's `NodeKind` offers no list of its variants, so `src/kinds.rs` writes it once in a macro that
builds both the list and a `match` from `NodeKind` with no catch-all arm.  A kind Ruff adds is then
a non-exhaustive match until it is listed, and a name Ruff does not have fails to resolve.

## Ruff quirks it absorbs

Each quirk is pinned by a test that fails when an upgrade fixes it, which says to delete the
workaround.

- **A format spec is invisible to Ruff's walk.**  `InterpolatedElement::visit_source_order` never
  enters the `InterpolatedStringFormatSpec` node, so the builder in `src/parse.rs` walks such an
  element's children itself.  `ruffs_walk_still_skips_the_format_spec` pins the quirk.

## Pins

`ruff_python_ast`, `ruff_python_parser` and `ruff_text_size` are pinned with `=` in this crate's
`Cargo.toml`, so an upgrade is deliberate work here, never a lock-file refresh.  **The three move
together**: `ruff_python_parser` depends on `ruff_python_ast` at its own version, so bumping one
alone fails to compile with two copies of `ruff_python_ast`.  `.github/dependabot.yml` groups them
for this reason; a pull request from that group starts the checklist below, not a merge on green.

## Checklist: upgrading Ruff

1. **Read the changelog** for the three crates between the two versions, for changes to node
   kinds, ranges, tokens and the parser's modes.
2. **Bump the pin.**  Change all three `=` pins together; `ruff_containment.rs` fails if any is
   not exact, or the three differ.
3. **Fix the adapter.**  A new or renamed kind fails the kind list's `match`: list it in
   `src/kinds.rs` and update the count in `every_kind_is_listed_once_by_ruffs_name`.  Run
   `cargo test -p quaffed-python`: a kind the synthetic corpus does not produce fails its coverage
   check, so add Python that produces it (`corpus.md`), and a change in where Ruff's root
   starts or ends fails `ruffs_root_spans_the_text_but_blank_lines_before_an_expression`.
   A new field holding a list of nodes - a `Vec`, `ThinVec` or boxed slice of them - goes in
   `src/lists.rs` if its items can stand one after another with only a separator between them,
   with a case in `a_sequence_stands_only_as_items_of_a_list` (`tests/matching.rs`); otherwise
   a sequence hole there is refused, which is the safe default.
4. **Run the round trip and the CPython oracle** over the real-world corpus, as the Real-world
   corpus CI job does (`corpus.md`): `scripts/fetch-corpus` with the oracle's Python, the
   round trip, then `scripts/count-structural` and `real_world_matches.rs`.
5. **Check the quirks.**  Delete the workaround a failing quirk test names.  Record any new quirk
   here, with a test of its own.
6. **Record what changed**: in the commit message, what moved in Ruff and what the adapter did;
   on this page, only what a later change must know.
