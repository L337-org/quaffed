# The Python adapter

`crates/python`, the `quaffed-python` crate: Python source through Ruff's parser, in quaff's own
terms.  **It is the only crate that may name a `ruff_*` type**, so that a Ruff upgrade, which
Ruff warns "will have frequent breaking changes", is absorbed here and reaches nothing else.
Today it offers what the synthetic corpus's coverage check needs; structural search extends it
with the node arena and the matcher.

## What it offers

- **`all_kinds()`**: every kind of node in Python's grammar as the pinned Ruff knows it, each
  once, in Ruff's order - 94 at `ruff_python_ast` 0.0.16.
- **`kinds_in_module(source)`**, **`kinds_in_expression(source)`** and
  **`kinds_in_ipython(source)`**: the kinds of node a piece of source contains, parsed as a
  module, as a single expression, or as `IPython` source with its `%magic` and `!shell` escape
  commands.  Source that does not parse
  is a `ParseError` carrying Ruff's message verbatim and the byte offset, never a partial
  answer.
- **`Kind`**: a kind, by the name Ruff gives it - `StmtFunctionDef`, `ExprCall` - so nothing
  outside the adapter sees Ruff's own enum.

## The kind list is held to Ruff's enum by the compiler

Ruff's `NodeKind` offers no list of its variants, so the list is written once in
`src/kinds.rs`, in a macro that builds both the list and a `match` from `NodeKind` with no
catch-all arm.  The compiler then holds it to the enum: a kind Ruff adds is a non-exhaustive
match until it is listed, and a name Ruff does not have fails to resolve.  Both were shown to
fail the build when the slice landed.  The names are the same identifiers, so they are Ruff's
own.

## Ruff quirks it absorbs

- **A format spec is invisible to Ruff's walk.**  `InterpolatedElement::visit_source_order`
  visits a format spec's parts but never enters the `InterpolatedStringFormatSpec` node, so the
  adapter records that kind wherever an element has a spec.

## Pins

`ruff_python_ast` and `ruff_python_parser` are pinned with `=` in this crate's `Cargo.toml`, and
`Cargo.lock` holds them.  An upgrade is deliberate work in this crate, never a lock-file refresh.

## Checklist: upgrading Ruff

1. Read Ruff's changelog for both crates between the two versions.
2. Change both `=` pins together.
3. Build.  A new or renamed node kind fails the kind list's `match`: list it in `src/kinds.rs`,
   and update the count in `every_kind_is_listed_once_by_ruffs_name`.
4. Run `cargo test -p quaffed-python`.  A new kind the synthetic corpus does not produce fails
   its coverage check; add Python that produces it, as `verification.md` describes.
5. Check each quirk above still holds, and record any new one here.
6. Update this page in the same change.
