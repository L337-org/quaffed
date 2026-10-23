# The quaff language

A reference to what `quaff` does, as built: the command line, scripts, textual and structural
search, what is searched, what is printed, and the edge cases, with examples.  `quaff -h` is the
short summary.

The examples run against this small project, a git repository with `src/` and `docs/`:

```
src/app.py
src/totals.py
docs/notes.md
```

```python
# src/app.py
import logging

log = logging.getLogger(__name__)


def handle_request(request, response):
    # TODO: check the session
    user = load_user(request)
    if user is None:
        return response.error(404)
    log.info("user %s", user.name)
    return response.ok(render(user, response))


def load_user(request):
    return lookup(request.args["id"],
                  default=None,)
```

```python
# src/totals.py
def total(items):
    subtotal = sum(item.price for item in items)
    tax = subtotal * 0.2
    return subtotal + tax


def discount(price, rate):
    return price - price * rate
```

```markdown
# docs/notes.md
# Notes

TODO: write the deployment guide.
handle_request(request, response) is the entry point.
```

Some examples use a `play/` directory beside them, with these two files:

```python
# play/nested.py
wrap(wrap(1))
same(a + b, a+b)
same(a, b)
x = ( (a + b) ) * c
y = a + b * c
s = "ab" "c"
t = 'abc'
u = f"{a}" f"{b}"
open_db()
a = 1
close_db()
```

```python
# play/more.py
call(  # the first argument
    first,
    second,
)
greet("$name")
for i in range(3):
    pass
else:
    done()
```

In each example, what `quaff` prints to standard output and standard error is shown together,
with the exit status after it.  Structural examples over the whole project also print the note
`1 text-tier file not searched for structural patterns, which apply to Python files only`, for
`docs/notes.md`; after the first, it is left out, and so is the exit status where it is 0.

## Contents

- [Running quaff](#running-quaff)
- [Textual search](#textual-search)
- [Scripts](#scripts)
- [Assertions](#assertions)
- [Structural search](#structural-search)
- [What is searched](#what-is-searched)
- [Encodings](#encodings)
- [Output](#output)
- [Exit codes](#exit-codes)
- [Not built yet](#not-built-yet)
- [Peculiarities](#peculiarities)

## Running quaff

```
quaff TEXT [PATH]          search the project for TEXT
quaff -s TEXT [PATH]       the same, with TEXT given as an option
quaff -p PATTERN [PATH]    search the project's Python files for code shaped like PATTERN
quaff -e SCRIPT [PATH]     run the statements in SCRIPT
quaff -f FILE [PATH]       run the script in FILE; -f - reads it from standard input
quaff -h, --help           the help
```

Long forms: `--string`, `--pattern`, `--expression` and `--file`, each taking its value as the
next argument or after `=`.  A short option never takes `=`: `-s=x` is an unknown option.

- **`-s`, `-p`, `-e` and `-f` can be repeated and mixed**, and run in the order given, as one
  program.
- **With any of them, the first positional argument is the scope**, `PATH`.  Without them, it is
  the text to search for.
- **There is one scope at most.**  More is refused, because the shell has usually expanded a glob:

  ```
  $ quaff a b c
  quaff: 2 scope arguments, "b", "c"; quaff takes one.  The shell has probably expanded a glob: name one file or directory instead
  [exit 3]
  ```

- **`--` ends the options**, so text starting with a dash can be searched for: `quaff -- -v`.
- **`quaff` with no arguments** prints the help to standard error and exits 3.

### The project, and the scope

- **The project** is the nearest directory at or above the current one that holds `.git`.  With
  no project, the current directory stands in for it.
- **A search covers the whole project by default**, wherever you run it from.
- **`PATH` narrows it** to one file or directory.  It must exist (exit 7 otherwise) and lie inside
  the project (exit 3 otherwise).  It is a path, not a glob: a quoted glob is reported as not
  found, with a note that globs are not expanded.
- **Paths in the output are relative to the current directory**, so from `src/`:

  ```
  $ quaff TODO
  ../docs/notes.md:3:1-3:4: TODO
  app.py:7:7-7:10: TODO
  [exit 0]
  ```

## Textual search

```
$ quaff TODO
docs/notes.md:3:1-3:4: TODO
src/app.py:7:7-7:10: TODO
[exit 0]

$ quaff TODO src
src/app.py:7:7-7:10: TODO
[exit 0]

$ quaff -s TODO -s FIXME
docs/notes.md:3:1-3:4: TODO
src/app.py:7:7-7:10: TODO
quaff: no matches for "FIXME"
[exit 0]
```

- **The text is literal.**  Nothing in it is special: `^`, `$`, `*`, `[` and `\` match
  themselves.
- **Matches do not overlap**, and are found left to right: `aa` occurs once in `aaa`.
- **With no assertion, the exit status says whether anything was found**: 0 if any query found
  something, 1 if none did.  A query that finds nothing is noted on standard error.

## Scripts

A script is statements separated by newlines or `;`, with `#` comments.  It comes from `-e`, from
a file with `-f`, or from standard input with `-f -`.

```
$ cat checks.quaff
find "TODO" expect 2
find `return $x` expect at least 1 # every function returns
```

### Statements

| statement | means |
| --- | --- |
| `find "TEXT"` | a textual search; `find` may be left out, so `"TEXT"` alone is the same |
| ``find `PATTERN` `` | a structural search, the same as `-p PATTERN` |
| `find ... expect COUNT` | an [assertion](#assertions) |

`replace` and `delete` parse but are [not built yet](#not-built-yet).  A misspelt statement is
refused, naming the ones there are:

```
$ quaff -e 'fnd "x"'
quaff: -e expression 1, at character 1: unknown statement: `fnd`; the statements in this build are find, replace and delete
[exit 5]
```

### Operands

**Text is double-quoted, and a structural pattern is backticked.  Nothing inside either is
escaped**: a backslash is always a backslash.

- **To include the delimiter, fence with three or more**: the operand closes at the next run of
  exactly that length.

  ```
  $ quaff -e 'find """ ["id"] """'
  src/app.py:16:31-16:36: ["id"]
  [exit 0]
  ```

- **A fence of three or more trims one space from each end** when the content starts and ends
  with one and is at least two characters long, so `""" "x" """` is `"x"`, while `""" """` is
  a single space.  A single-delimiter operand is never trimmed.
- **`""` is the empty operand**, so a run of two never opens a fence.
- **An operand may span lines.**

### Joining, line breaks and anchors

A textual operand can be built from pieces:

- **Adjacent quoted pieces join** with nothing between them: `"ab" "cd"` is `abcd`.
- **`$^` between two pieces is a line break**, matching LF, CRLF or a lone CR, whichever the
  file has:

  ```
  $ quaff -e 'find "(request):" $^ "    return"'
  src/app.py:15:14-16:10: (request): return
  [exit 0]
  ```

- **`^` before the first piece anchors to the start of a line, and `$` after the last to its
  end:**

  ```
  $ quaff -e 'find ^"def "'
  src/app.py:6:1-6:4: def 
  src/app.py:15:1-15:4: def 
  src/totals.py:1:1-1:4: def 
  src/totals.py:7:1-7:4: def 
  [exit 0]
  ```

  A CRLF is one line ending, so neither anchor holds between its CR and its LF.

- **None of these apply to a backticked pattern**, which is anchored by its shape.

## Assertions

A `find` with an `expect` is an assertion: it fails unless the count of matches holds.

```
$ quaff -e 'find "TODO" expect 2'
docs/notes.md:3:1-3:4: TODO
src/app.py:7:7-7:10: TODO
[exit 0]

$ quaff -e 'find "TODO" expect none'
docs/notes.md:3:1-3:4: TODO
src/app.py:7:7-7:10: TODO
quaff: -e expression 1, at character 1: expected no matches of "TODO", found 2
[exit 2]
```

**Counts:** `N`, `at least N`, `at most N`, `N or none`, `none` and `any`.

- **Consecutive assertions are one block.**  Every assertion in it is checked, each that fails
  is reported, and the run stops after the block, exit 2:

  ```
  $ quaff -e 'find "TODO" expect 2; find "FIXME" expect none; find "XXX" expect at least 1'
  docs/notes.md:3:1-3:4: TODO
  src/app.py:7:7-7:10: TODO
  quaff: -e expression 1, at character 49: expected at least 1 match of "XXX", found 0
  [exit 2]
  ```

- **A failure says where the assertion was written**, in the source's terms:
  `checks.quaff:1:1` for a file, `<stdin>:1:1` for standard input, `-e expression 1, at
  character 49` for an inline script.
- **Matches print as they would without the `expect`.**
- **An assertion counts certain matches only.**  Possible matches in a file of unknown encoding
  ([Encodings](#encodings)) are noted, but never counted, so `expect none` can hold while a
  possible match exists.  The note says where, and declaring that file's encoding makes it
  count.
- **An assertion about structure in a scope with no searchable Python fails**, whatever it asks,
  because a count that could not be taken is not a passed one.

## Structural search

A structural pattern is Python code with holes in it, matched by the shape of the code rather than
its text: `-p PATTERN`, or a backticked operand in a script.

```
$ quaff -p 'return $x'
src/app.py:10:9-10:34: return response.error(404)
src/app.py:12:5-12:46: return response.ok(render(user, response))
src/app.py:16:5-17:32: return lookup(request.args["id"], default=None,)
src/totals.py:4:5-4:25: return subtotal + tax
src/totals.py:8:5-8:31: return price - price * rate
quaff: 1 text-tier file not searched for structural patterns, which apply to Python files only
[exit 0]
```

### Holes

| hole | matches |
| --- | --- |
| `$name` | any one expression or statement, bound to `name` |
| `$_` | any one expression or statement, bound to nothing |
| `$name...` | one or more consecutive items - arguments, elements, statements - bound to `name` |
| `$...` | one or more consecutive items, bound to nothing |

**A hole always matches something, never nothing.**  A hole's name is spelt as a Python
identifier is.

```
$ quaff -p 'log.info($msg, $...)'
src/app.py:11:5-11:34: log.info("user %s", user.name)

$ quaff -p 'render($u, $_)'
src/app.py:12:24-12:45: render(user, response)

$ quaff -p '$f($args...)' src/totals.py
src/totals.py:2:16-2:48: sum(item.price for item in items)
```

**A name used twice must match the same code each time**, compared as code, so layout does not
matter:

```
$ quaff -p 'same($x, $x)' play
play/nested.py:2:1-2:16: same(a + b, a+b)
```

The same holds whatever role the name plays: `$x = $o.$x` matches `name = self.name`.

**A hole is only a hole in code.**  A `$name` inside a string literal or a comment in the pattern
is literal text, while one in an f-string's `{...}` field is a hole:

```
$ quaff -p 'greet("$name")' play/more.py
play/more.py:5:1-5:14: greet("$name")
```

That matches only the literal string `"$name"`.

**Refused patterns** are a script error, exit 3, before any file is read, placed where in the
pattern the fault is:

```
$ quaff -p '$x'
quaff: -p, at character 1: the pattern is only holes, so it has no shape to match: write the code around them, as in "handle_request($a, $b)"
[exit 3]

$ quaff -p 'f($_...)'
quaff: -p, at character 3: "$_..." is not a hole: a sequence that binds nothing is written "$..."
[exit 3]

$ quaff -p 'f($x, $x...)'
quaff: -p, at character 7: "$x" is used for one node and, here, for a sequence: a name is one or the other, so give the sequence a name of its own
[exit 3]

$ quaff -p 'f($a'
quaff: -p, at character 5: the pattern is not Python: unexpected EOF while parsing
[exit 3]
```

A pattern with no code at all, only whitespace or comments, is refused too, and so is one that
starts or ends a run of statements with a sequence hole ([Runs of
statements](#runs-of-statements)).

### What takes no part in a match

**Comments, whitespace, line breaks and indentation:**

```
$ quaff -p 'call(first, second)' play/more.py
play/more.py:1:1-4:1: call(  # the first argument first, second, )
```

**A trailing comma before a closing bracket**, the one formatters add when they split a call
over lines.  `lookup($key, default=None)` matches `lookup(request.args["id"],
default=None,)`.

**A semicolon between statements**: `x = 1` then `y = 2` on two lines in a pattern matches
`x = 1; y = 2`.

**Redundant parentheses**, those that only wrap an expression, but not parentheses that change
the meaning, since those change the tree:

```
$ quaff -p '($a + $b) * $c' play
play/nested.py:4:5-4:19: ( (a + b) ) * c

$ quaff -p '$a + $b * $c' play
play/nested.py:5:5-5:13: a + b * c
```

`$a + $b * $c` does not match `( (a + b) ) * c`, because there `a + b` is multiplied by `c`.

**Operators do take part.**  `$a + $b` matches `subtotal + tax` but not `price - price * rate`.
The same goes for `<` and `>`, and `not` and `-`.

### Strings

**A string matches by its content**, what lies between its quotes as written.  Its quote style
and prefix take no part, and the parts of an implicit concatenation are joined:

```
$ quaff -p '"abc"' play
play/nested.py:6:5-6:12: "ab" "c"
play/nested.py:7:5-7:9: 'abc'

$ quaff -p "log.info('user %s', \$v)"
src/app.py:11:5-11:34: log.info("user %s", user.name)
```

**Escapes are compared as written**, not decoded: `"\n"` matches `r"\n"`, both being a backslash
and an `n` in the file.

### Matches inside matches

**A match inside another match is reported as a match of its own**, so a search counts every
place the shape occurs:

```
$ quaff -p 'wrap($x)' play
play/nested.py:1:1-1:13: wrap(wrap(1))
play/nested.py:1:6-1:12: wrap(1)
```

### Runs of statements

**A pattern of several statements matches them standing together in one block.**  A sequence hole
between two fixed statements takes as few statements as it can, and never reaches into another
block - an `else:` clause, or a nested `if` body:

```
$ quaff -p 'open_db()
$body...
close_db()' play
play/nested.py:9:1-11:10: open_db() a = 1 close_db()
```

From each place a run can start it is matched separately, so with `w()` twice before `x()` twice,
`w()` then `$mid...` then `x()` gives two overlapping matches, and a search reports both.

**A sequence hole may not start or end a run of statements.**  Nothing would bound it on that
side, so it would reach the edge of its block - at a module's top level, most of the file - in a
single match that every count agreed with:

```
$ quaff -p 'open_db()
$rest...' play
quaff: -p, at character 11: "$rest..." ends the pattern's statements, so nothing bounds it and it would take every statement to the end of the block: put a statement after it, so that code stands on both sides, as "$body..." does between "open_db()" and "close_db()" on lines of their own
[exit 3]
```

A sequence inside one node is bounded by it, so `def $f():` with `$body...` as its body, or
`f($first, $rest...)`, is fine.

### Which files

**Structural patterns apply to Python files only**: `.py`, `.pyi` and `.pyw`, decided by the name
alone.

- **Any other file is not applicable**: it is never matched as text instead, and never an error.
  A note counts those not searched, as in every example above.
- **When nothing in scope is Python, the run says so** rather than reporting nothing found:

  ```
  $ quaff -p 'return response.ok($x)' docs
  quaff: no file in scope has structural support; 1 text-tier file not searched, so `return response.ok($x)` was not searched for
  [exit 1]
  ```

  When Python is in scope but none of it could be searched - binary, or in an encoding quaff does
  not read - it says `no Python file in scope could be searched`, and each file's note says why.
- **A Python file that does not parse refuses the whole run**, exit 9, before anything is printed,
  because a count that silently skipped a file could not be trusted:

  With a directory `broken/` beside `play/` holding only `bad.py`, which is `def broken(:`:

  ```
  $ quaff -p 'wrap($x)' broken
  quaff: refused: 1 Python file in scope does not parse, so a structural search cannot count matches in it:
    "broken/bad.py": line 1, column 12: Expected a parameter or the end of the parameter list
  To search without it, list it in .ignore, or give a narrower scope on the command line
  [exit 9]
  ```

  A textual search over the same files still runs, since it needs no parse.  A file Python
  itself refuses to read - a bad encoding declaration, or a byte its encoding does not have -
  does not parse either.  A binary Python file, one with a NUL byte in its first 8000 bytes, is
  skipped and counted with the binary files instead, as any binary file is.
- **A Python file is read as Python reads it**: in the encoding its coding line declares, or
  UTF-8.  An `.editorconfig` or git attribute that says otherwise does not change that, because it
  does not change what Python runs.  One in an encoding Python reads and quaff does not, such as
  GBK, is noted as not searched.

## What is searched

- **Files ignored by `.gitignore`, `.ignore` or `.git/info/exclude` are skipped.**  `.ignore`
  outranks `.gitignore`, which outranks `info/exclude`, and within each kind the nearest
  directory with a matching rule decides.  With no project, only `.ignore` applies.
- **No ignore file above the project root is read**, and no git configuration, so a global
  excludes file does not apply.
- **Naming an ignored file or directory as the scope searches it anyway**, but every ignore rule
  beneath it still applies.
- **Hidden files are searched.**  Version-control metadata is not: `.git`, `.hg`, `.svn`, `.bzr`,
  `_darcs`, `.jj`, `.pijul` and `CVS`, at any depth.
- **Binary files are skipped and counted.**  A file is binary if a NUL is in its first 8000
  bytes, git's test, unless a byte-order mark or a declaration says UTF-16 or UTF-32.
- **Symbolic links are not followed, and FIFOs, sockets and devices are not read**; the run
  says how many it met.
- **An ignore-file line that cannot be parsed is skipped**, with a note naming the file and line.
  Anything else that cannot be read stops the run, exit 7, rather than answering short.
- **Files are searched in path order**, so the output is the same on every run.

## Encodings

**A file is searched in its own encoding**, decided by what declares it, never guessed from the
bytes.  The first of these that applies decides:

1. a byte-order mark;
2. a Python file's coding line, as Python reads it;
3. git's `working-tree-encoding` attribute;
4. `.editorconfig`'s `charset`;
5. the bytes being valid UTF-8.

- **In a known encoding**, the text searched for is encoded the way the file is, and a match
  starts and ends on a character.  Bytes that match inside a multi-byte character, as can happen
  in Shift-JIS, are not a match.  A character the encoding cannot hold matches nothing.
- **In an unknown encoding** - not valid UTF-8 and nothing declaring it - ASCII text is matched by
  its bytes, and each match is a **possible match**: reported by line, never counted, and the file
  is never edited.  Non-ASCII text is not searched there, and a note says so.  Declaring the
  file's encoding makes its matches certain.
- **A note may suggest an encoding** when the files of unknown encoding read cleanly in your
  locale's.  The locale never decides anything, since it describes your terminal, not the
  project.

## Output

**One line per match** on standard output:

```
src/app.py:16:5-17:32: return lookup(request.args["id"], default=None,)
```

- **`path:line:column-endline:endcolumn: text`**, the first and last character of the match,
  both inclusive.
- **Lines end at LF, CRLF or a lone CR.  Columns count characters from 1**, not bytes, so a
  Latin-1 file counts as an editor showing it as Latin-1 would.
- **The text is folded onto one line**: each line ending, with the indentation after it, becomes
  one space, so `return` and `value` on two lines never read as `returnvalue`.  It is printed in
  full.
- **With several queries, each query's matches print together**, in the order of the program.
- **Notes go to standard error, prefixed `quaff:`**: no matches, possible matches, files not
  searched and why, binary files skipped, skipped ignore-file lines, links not followed.
  Anything not looked at, or looked at without certainty, is said.
- **A message names a pattern** quoted for text - `"TODO"`, with anchors outside the quotes as a
  script writes them, `^"x"$` - and in backticks for a structural pattern, `` `f($a)` ``.  In
  both, a line break is shown as `\n`, so a message stays on one line.
- **When standard output is closed**, as when piping into `head`, printing stops quietly, and
  assertions are still checked, so the exit status is still the answer.

## Exit codes

| code | when |
| --- | --- |
| 0 | every assertion held; or, with no assertion, something was found |
| 1 | nothing was found, and there was no assertion; not an error |
| 2 | an assertion failed |
| 3 | a usage error, a malformed script or pattern, an empty program, or a scope outside the project |
| 5 | a statement or construct this build does not have |
| 7 | an I/O error, naming the path and quoting the system's error |
| 9 | a structural search needs a Python file that does not parse |

**A run that cannot give an answer - exit 3, 5, 7 or 9 - prints nothing on standard output**, so
a partial answer never passes for a whole one.  The whole program is read and checked before any
file is looked at, so a script with an error anywhere does not run at all.

## Not built yet

These parse, and are refused with exit 5 at the place they were written, rather than being
ignored or treated as unknown:

- `replace` and `delete`;
- an `in` clause, and a `where` clause.

```
$ quaff -e 'find "x" in "src/**"'
quaff: -e expression 1, at character 13: an `in` clause is not built yet
[exit 5]
```

The options `-o`, `--dry-run` and `--version` are refused as not built yet too, with exit 3.

## Peculiarities

Edge cases and decisions that are easy to trip over, with the reason for each.

- **An empty program is an error, not an empty answer.**  `-e "$CHECKS"` with the variable unset
  exits 3, naming the sources, rather than searching for nothing and exiting 1.
- **A quoted glob as the scope is not expanded.**  It is reported as not found, with a note that
  globs are not expanded.
- **A script is UTF-8.**  Anything else is refused at its first byte that is not.  A leading
  byte-order mark is skipped, so locations count from the first visible character.
- **A possible match never decides an assertion**, so that a file quaff cannot read never blocks
  an answer about the files it can.
- **Structural matches inside matches are each reported**, so `f($x)` over `f(f(1))` is two
  matches.
- **A trailing comma and a semicolon between statements are layout**, so `f($a, $b)` matches a
  call a formatter has split over lines and ended with a comma.
- **Implicitly concatenated plain strings are joined, but f-strings are not**: `"abc"` matches
  `"ab" "c"`, while `f"{a}{b}"` does not match `f"{a}" f"{b}"`, which `f"{$x}" f"{$y}"` does.
- **A string's escapes are compared as written**, so `"\n"` and `r"\n"` match each other but not
  a string holding an actual newline.
- **A repeated hole name compares code, whatever role it plays**: an argument, a keyword, an
  attribute or a parameter.
- **`""" """` is a single space**: a fence trims its padding only from content of two characters
  or more, so a lone space survives.
- **A hole always matches at least one thing**, so `f($a...)` does not match `f()`.
- **A method call is a different shape from a plain call**: `handle_request($a, $b)` does not
  match `client.handle_request(a, b)`.
- **A run of statements never crosses into another block.**
- **A sequence hole may not start or end a run of statements**, because nothing would stop it
  short of the edge of its block, so one match would cover far more than the pattern spells.  Put
  fixed code on both sides of it.  Deleting every statement after a `return` with `return $x` then `$dead...`
  is not possible for that reason.
- **Structural patterns ignore non-Python files rather than falling back to text**, and say so.
- **A Python file Python cannot read stops a structural search** (exit 9), but not a textual one.
- **A Python file with no coding line is UTF-8 to a structural search, whatever `.editorconfig`
  or git attributes say**, since that is how Python reads it.  A textual search follows those
  declarations, as listed under [Encodings](#encodings), so with `charset = latin1` for `*.py`,
  `find "café"` misses a UTF-8 `café` that a structural search reads correctly.
