# The Python adapter

`crates/python`, the `quaffed-python` crate: Python source through Ruff's parser, in quaff's own
terms.  **It is the only crate that may name a `ruff_*` type**, so that a Ruff upgrade, which
Ruff warns "will have frequent breaking changes", is absorbed here and reaches nothing else.
`crates/quaffed/tests/ruff_containment.rs` fails naming any other member that depends on a
`ruff_*` crate.

## What it offers

- **`parse_module(source)`**, **`parse_expression(source)`** and **`parse_ipython(source)`**:
  source parsed into a syntax arena (`syntax.md`), as a module, as a single expression, or as
  `IPython` source with its `%magic` and `!shell` escape commands.  Every arena carries the
  language `PYTHON`.  Source that does not parse is `ParseError::Invalid`, carrying Ruff's
  message verbatim and the byte offset; text too long for an arena's offsets is
  `ParseError::TooLong`, refused before Ruff sees it.  Never a partial arena.
- **`compile_pattern(text)`**: a structural pattern compiled into a `Pattern`, below.
- **`kinds_in_module(source)`**, **`kinds_in_expression(source)`** and
  **`kinds_in_ipython(source)`**: the kinds of node in the arena, which the synthetic corpus's
  coverage check reads.
- **`all_kinds()`**: every kind of node in Python's grammar as the pinned Ruff knows it, each
  once, in Ruff's order.  `every_kind_is_listed_once_by_ruffs_name` asserts how many.
- **`Kind`**, re-exported from `quaffed-syntax`: a kind by the name Ruff gives it -
  `StmtFunctionDef`, `ExprCall` - so nothing outside the adapter sees Ruff's own enum.

The arena is built in one walk of Ruff's tree, `enter` on the way in and `leave` on the way out.
A module's root runs from the start of the text to its end; an expression's starts at its first
token, so blank lines before it are the arena's leading text.
`ruffs_root_spans_the_text_but_blank_lines_before_an_expression` pins both.

It takes source as text.  Decoding a file is `quaffed-encoding`'s (`encoding.md`), so that the
adapter and a textual search read a file's bytes the same way, as CPython does.

## Patterns

`compile_pattern(text)` turns a structural pattern - `handle_request($a, $b)`, a `-p` argument or
a backticked operand - into a `Pattern` (`syntax.md`), keeping the rules for holes `script.md`
gives.  A hole is not Python, so it takes two stages:

1. **Claim before parsing.**  Every `$name`, `$_`, `$name...` and `$...` is replaced by a
   placeholder identifier, on a stem the pattern does not contain so a placeholder never meets
   a name the pattern wrote.  A `$` not followed by a name or `...` is left as written, so
   `${HOME}` is never claimed.  A name ends where Python's would, by the identifier rules
   Ruff's own lexer uses: `_` or `XID_Start`, then `XID_Continue`.
2. **Let the grammar decide.**  The result is parsed - as a single expression if it is one,
   otherwise as statements - and a placeholder is a hole when a node below the root spans
   exactly it.  Where several nodes do, the outermost is the hole, so a hole at statement level
   is the statement.  A placeholder spanned by none, or by a string's literal text - an
   f-string's literal part is a node of its own, so `f"$x"` has one spanning just the
   placeholder - lies inside a string or a comment: it is literal text, and is put back as
   written before the pattern is parsed again, which a string's or comment's content cannot
   change the shape of.  A hole in an f-string's field is in an expression, so it is a hole.
   A hole written against a name, `x$a`, merges with it so that no node spans it either; it is
   put back too, and the pattern fails to parse.

It refuses `$_...`, since a sequence that binds nothing is spelt `$...`; a pattern with no code,
only whitespace or comments; and a pattern that is only holes, which has no shape to match.  An
error that is Ruff's is placed in the pattern as written, not in the text with placeholders.

## The kind list is held to Ruff's enum by the compiler

Ruff's `NodeKind` offers no list of its variants, so the list is written once in
`src/kinds.rs`, in a macro that builds both the list and a `match` from `NodeKind` with no
catch-all arm.  The compiler then holds it to the enum: a kind Ruff adds is a non-exhaustive
match until it is listed, and a name Ruff does not have fails to resolve.  The names are the
same identifiers, so they are Ruff's own.

## Ruff quirks it absorbs

- **A format spec is invisible to Ruff's walk.**  `InterpolatedElement::visit_source_order`
  visits a format spec's parts but never enters the `InterpolatedStringFormatSpec` node, so the
  arena would hang the parts on the element with no spec around them.  The builder in
  `src/parse.rs` walks such an element's children itself - the expression, then the spec
  entered around its parts - and `a_format_spec_is_a_node_around_its_parts` checks the result.
  `ruffs_walk_still_skips_the_format_spec` pins the quirk: an upgrade that fixes it fails that
  test, which says to delete the workaround.

## Pins

`ruff_python_ast`, `ruff_python_parser` and `ruff_text_size`, which gives a node its range, are
pinned with `=` in this crate's `Cargo.toml`, and `Cargo.lock` holds them.  An upgrade is
deliberate work in this crate, never a lock-file refresh.

The three move together.  `ruff_python_parser` depends on `ruff_python_ast` at its own version, so
bumping one alone puts two copies of `ruff_python_ast` in the build, and the adapter fails to
compile with *"there are multiple different versions of crate `ruff_python_ast` in the
dependency graph"*.  `.github/dependabot.yml` keeps the `ruff_*` crates in a group of their own
for this reason; a pull request from that group is where the checklist below starts, not a merge
on green.

## Checklist: upgrading Ruff

1. Read Ruff's changelog for the three crates between the two versions.
2. Change all three `=` pins together.
3. Build.  A new or renamed node kind fails the kind list's `match`: list it in `src/kinds.rs`,
   and update the count in `every_kind_is_listed_once_by_ruffs_name`.
4. Run `cargo test -p quaffed-python`.  A new kind the synthetic corpus does not produce fails
   its coverage check; add Python that produces it, as `verification.md` describes.  A change
   in where Ruff's root starts or ends fails the root test above.
5. Run the round trip over the real-world corpus (`verification.md`): a change in Ruff's
   ranges that breaks the arena's invariants fails it, naming the file and the byte.
6. Each quirk above is pinned by a test that fails when it stops holding; delete the workaround
   that test names.  Record any new quirk here, with a test of its own.
7. Update this page in the same change.
