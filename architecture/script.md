# The script language

`crates/script`, the `quaffed-script` crate: the quaff script language for the MVP subset, from
text to the operation representation (`representation.md`).  `parse_source` reads one source -
a script file, standard input, or one `-e` expression - and appends its statements to a
`Program`.  Sources compose in the order they are parsed, and inline text goes through the same
parser as a file, never split on `;` first, because an operand may contain one.

## Operands (`src/lexer.rs`)

A double-quoted operand is text; a backticked one is a pattern in the target language.
**Nothing inside either is escaped:**

- A backslash is always a backslash.
- An operand opens with a run of one delimiter, or of three or more, and closes at the next run
  of exactly that length; a run of another length inside is content.
- Two delimiters in a row are the empty operand, so a run of two never opens a fence.
- In an operand fenced with three or more, content that both starts and ends with a space, and
  is at least two characters long, loses one space from each end.  A single-delimiter operand
  is never trimmed.
- Only `"` and `` ` `` are delimiters.  An operand may span lines.

The shortest spelling of any content is the shortest fence whose length does not occur as a run
in it, padded with a space at each end when the content starts or ends with the delimiter.
The unit tests check every spelling the design lists, and a property test checks that any
content read back from its shortest spelling is unchanged.

## Statements (`src/parser.rs`)

```
find "TODO"                                   # search; `find` may be omitted
find "TODO" in "src/**" where not FILE matches "*_test.py" expect none
replace string "old" with "new" expect 3      # a textual edit
replace `f($a)` with `g($a)` expect 1         # a structural edit
delete `breakpoint()` expect any
```

- **Statements** are separated by a newline or `;`.  **Comments** run from `#` to the end of
  the line, outside operands; a `#` inside an operand is content.
- **A `find` with an `expect` is an assertion.**  Consecutive ones form one assertion block,
  as `Program::push` groups them.  There is no standalone `expect`.
- **Clauses come in one order** - `in`, `where`, `expect` - each at most once; a clause out of
  order or repeated is an error naming it.  `in` takes a quoted glob; `where` takes conditions
  joined by `and`, each `not`-able, testing a metavariable or `FILE` with `matches` or
  `contains`.  There is no `or`.
- **Counts:** `N`, `at least N`, `at most N`, `N or none`, `none`, `any`.
- **Textual operations are spelled textually.**  `replace string` takes quoted operands and
  `replace` backticked ones; a mismatch is an error naming it, never a guess.
- **An edit without an `expect` parses**, and the representation's checker refuses it, so the
  rule holds however a program was built.

## Joining, line breaks and anchors

A textual operand is built from **pieces**: adjacent quoted operands join with nothing between
them, `$^` between two pieces is a line break, `^` before the first piece and `$` after the last
anchor it.  Whitespace may sit between pieces and markers.  `$` followed by a name or `_` is a
metavariable, `$^` a line break, and any other `$` the end anchor.

- On a backticked operand, joining, `$^` and anchors are errors: a pattern is already anchored
  by its shape.
- In a replacement, anchors are errors - they match a position, and a replacement only writes
  text - and an empty operand is allowed, to delete text.  An empty search is refused.

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

**Locations** (`src/location.rs`) name the source: `refactor.quaff:12:5` for a file,
`<stdin>:12:5` for standard input, `-e expression 2, at character 10` for an inline script.
Lines and columns count from 1, columns in characters.

## Properties

`tests/properties.rs` checks, over generated input, that any content reads back from its
shortest spelling, that the parser never panics, and that layout and comments never change the
program a script parses to.

## Checklist: adding a statement or clause

1. Add it to the representation first (`representation.md`'s checklist).
2. Parse it in `src/parser.rs`, removing it from `NOT_IN_THIS_BUILD` or the matching list.
3. Add unit tests for the form and for each way it can be malformed, with what each says.
4. Extend the statement generator in `tests/properties.rs` so layout invariance covers it.
5. Update this page in the same change.
