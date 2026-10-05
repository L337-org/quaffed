# The operation representation

`crates/representation`, the `quaffed-representation` crate: what a quaff program means, as
operations, with no syntax left in it.  The DSL parser and the command line both produce a
`Program`; the engine consumes nothing else.  That is what lets the surface syntax change
without the engine noticing, and keeps a script's meaning in one place.

## The shape

- **`Program`** (`src/program.rs`): the sources it was built from, in command-line order; its
  statements in source order; and the one positional scope from the command line.
- **`Statement`**:
  - `Find(Query)` - search and report;
  - `Act(Action)` - replace or delete, carrying its own `expect` clauses;
  - `Assert(Vec<Expectation>)` - a block of assertions.
- **`Query`**: an operand, the `in` globs and the `where` filter.  **The operand's kind is the
  matching mode** - `Text` for a quoted operand, `Pattern` for a backticked one - so textual
  against structural is never a separate flag that could disagree with it.  A structural
  pattern is carried as written; the engine compiles it through the front end for each file's
  language, so this crate depends on no parser.
- **`TextPattern`**: literal text and line breaks, optionally anchored at either end, built
  from the pieces a script writes.  Adjacent literals join as it is built, so `"ab"` and
  `"a" "b"` are the same pattern.
- **`Expectation`**: a `Count` - exactly, at least, at most, exactly-or-none, none, any - of
  something counted, in the MVP a query's matches.
- **`Span`** and **`Spanned<T>`** (`src/span.rs`): every node carries the source and byte
  range it was written at.

## The rules it carries

- **Identity ignores position.**  `Spanned`'s equality and hashing look only at the node, so
  two statements that parse the same are the same statement wherever they were written.
- **Assertion blocks.**  `Program::push` joins an `Assert` to an `Assert` it follows, so a run
  of asserting statements is one block, and an action between two runs keeps them apart.
  Within a block an expectation is kept once, whether an equal one is already there or two
  arrive together.  `Program`'s statements are private, read through `body()`, so nothing can
  append one any other way.  A block is evaluated to the end before a failure stops the run.
- **The checker** (`src/check.rs`) refuses an edit with no `expect`, and an `expect` on an edit
  whose target is a node already bound.  `first_outside_mvp` is the one answer to whether a
  program is inside the MVP subset; what it finds is an unknown statement, which the command
  line exits with a code of its own, as its exit-code table says.
- **Determinism.**  Every collection is a `Vec` in source order.  `clippy.toml` forbids
  `HashMap` and `HashSet` in this crate, and `tests/disallowed_types.rs` shows the lint firing:
  it builds a throwaway crate with this `clippy.toml` and checks that clippy refuses a hash map
  and a hash set and accepts the same crate without them.
- **Additive only.**  Every enum is `non_exhaustive`, so a construct outside the MVP joins as a
  new variant.  `Operand::Bound` is already here - the checker's bound-target rule needs it -
  but nothing in the MVP's syntax can produce it, and `first_outside_mvp` reports it.

## Checklist: adding a construct

1. Add the variant or field here, `non_exhaustive` if it is an enum or a struct others build.
2. Decide whether it is inside the MVP subset, and if not, make `first_outside_mvp` report it.
3. Add any rule it brings to `check`, with a test of the refusal and its span.
4. Update this page in the same change.
