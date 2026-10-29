# The script language

`crates/script`, the `quaffed-script` crate: the quaff script language for the MVP subset, from
text to the operation representation (`representation.md`).  `parse_source` reads one source -
a script file, standard input, or one `-e` expression - and appends its statements to a
`Program`.  Sources compose in the order they are parsed, and inline text goes through the same
parser as a file, never split on `;` first, because an operand may contain one.  What each
construct means to a user is in `docs/language.md`; this page is how the parser is built.

## Operands (`src/lexer.rs`)

A double-quoted operand is text; a backticked one is a pattern in the target language.
**Nothing inside either is escaped**: a backslash is always a backslash, and an operand that
holds its own delimiter is fenced with a longer run of it instead.  The fencing and trimming
rules are `docs/language.md`'s _Operands_.

**Holes in a pattern.**  Inside a backticked operand, `$name` binds one node, `$_` one node bound
to nothing, `$name...` a sequence of one or more nodes, and `$...` such a sequence bound to
nothing; a hole never matches nothing.  Whether a `$name` is a hole is the target language's
grammar's to decide, not the script parser's: one that lands in code is a hole, and one inside a
string literal or a comment is literal text.  Where they are holes, these are refused:

- `$_...`, since `$...` already spells it;
- one name used both for one node and for a sequence;
- a sequence hole at the start or end of a run of statements, which nothing would bound;
- a pattern whose search could grow faster than the square of a block's or list's length
  (`syntax.md` says which);
- a pattern with no code, or with nothing but holes.

The script parser carries the operand as written; compiling it is the language adapter's, which
`python.md` describes for Python.

**A string literal in a pattern matches by its content** as written between its delimiters, so
that a script says what a string says without spelling any one language's quoting.  It is the
same content a `where` comparison uses.  A string's decoded value is never compared: escapes
would become characters that are not in the file, and an interpolated string cannot be decoded.

## Statements (`src/parser.rs`)

```
find "TODO"                                   # search; `find` may be omitted
find "TODO" in "src/**" where not FILE matches "*_test.py" expect none
replace string "old" with "new" expect 3      # a textual edit
replace `f($a)` with `g($a)` expect 1         # a structural edit
delete `breakpoint()` expect any
```

The grammar a user sees is `docs/language.md`'s _Statements_.  The parser's own rules:

- **A `find` with an `expect` is an assertion**, and consecutive ones form one assertion block,
  as `Program::push` groups them.
- **Clauses come in one order** - `in`, `where`, `expect` - each at most once; a clause out of
  order or repeated is an error naming it.
- **Textual operations are spelled textually.**  `replace string` takes quoted operands and
  `replace` backticked ones; a mismatch is an error naming it, never a guess.  `delete` takes
  only a backticked pattern.
- **An edit without an `expect` parses**, and the representation's checker refuses it, so the
  rule holds however a program was built.

## Joining, line breaks and anchors

A textual operand is built from **pieces** joined, with `$^` for a line break and `^` and `$`
anchoring it, as `docs/language.md` describes.  On a backticked operand all three are errors,
since a pattern is already anchored by its shape; in a replacement anchors are errors, since a
replacement only writes text, and an empty operand is allowed, to delete text.

## Errors

A `ParseError` is either **malformed** - the script must be fixed - or **unknown**: a statement
or construct this build does not have, named.  The command line gives them different exit
codes.  Nothing outside the MVP is parsed and ignored:

| unknown here | why |
| --- | --- |
| any word in statement position other than `find`, `replace`, `delete` | a statement this build does not have, a misspelt one included |
| a standalone `expect` | assertions go on the `find` they count |
| a metavariable between quoted pieces | textual capture |
| `${name\|...}` in a backticked replacement | a value filter |
| `as`, `reject`, `in $set`, `ENCLOSING`, `LANGUAGE`, `where ... in [...]`, `expect any <set>`, `expect applicable`, `expect no` | constructs outside the MVP |

**Text straight after an operand's closing delimiter** is reported at that delimiter, with the
advice to use a longer fence, because it is almost always an operand that held its own
delimiter.

**Locations** (`src/location.rs`) name the source: a file or `<stdin>` by line and column, an
`-e` expression or a `-p` pattern by character.  Lines and columns count from 1, columns in
characters.  The command line (`crates/quaffed/src/run.rs`) drops a leading byte-order mark
before parsing, so the parser never sees one and line 1, column 1 is the first visible
character.

## Running a script

The command line (`crates/quaffed/src/run.rs`) parses every source into one `Program`, runs the
checker, then refuses anything this build cannot yet run - an `in` or `where` clause, a
structural `replace`, and `delete` - each exit 5 at where it was written.  It compiles every
backticked pattern, so one that is not a pattern is exit 3 there too.  Only then does it look at
a file.  Every file is read once, and taken through the statements in order, so a statement sees
the edits before it; the statements are then evaluated in order over the results.  Editing,
output, notes, assertion failures and exit codes are in `search.md`.

## Properties

`tests/properties.rs` checks, over generated input, that any content reads back from its
shortest spelling, that the parser never panics, and that layout and comments never change the
program a script parses to.

## Checklist: adding a statement or clause

1. Add it to the representation first (`representation.md`'s checklist).
2. Parse it in `src/parser.rs`, removing it from `NOT_IN_THIS_BUILD` or the matching list.
3. Add unit tests for the form and for each way it can be malformed, with what each says.
4. Extend the statement generator in `tests/properties.rs` so layout invariance covers it.
5. When it becomes runnable, remove its refusal from `plan` in `crates/quaffed/src/run.rs`, and
   replace its end-to-end snapshot in `each_construct_not_built_yet_exits_5_naming_it` with one
   that runs it.
6. Update this page, `search.md` where it changes the output or the exit codes, and
   `docs/language.md`, in the same change.
