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
- [Paths and globs](#paths-and-globs)
- [Assertions](#assertions)
- [Replacing text](#replacing-text)
- [Structural search](#structural-search)
- [Replacing code](#replacing-code)
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
quaff --dry-run ...        work out every edit and print it, but write nothing
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
- **A search covers the current directory by default**, and `:/` names the whole project.
- **`PATH` narrows it** to one file or directory, by the [path rules](#paths-and-globs) `in` and
  `FILE` follow: relative to the current directory, or from the project root after `:/`.  It must
  exist (exit 7 otherwise).  It is a plain path, never a glob: a quoted glob is reported as not
  found, and one holding a double quote is refused (exit 3), pointing to `in`, which takes globs.
- **A path outside the project is searched**, since you named it, by that project's own ignore
  files and declarations; with no project there, the directory named stands in for one, as it
  would if quaff were run from inside it.
- **Paths in the output are relative to the current directory**, so from `src/`:

  ```
  $ quaff TODO
  app.py:7:7-7:10: TODO
  [exit 0]

  $ quaff TODO :/
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
| `replace string "TEXT" with "TEXT" expect COUNT` | a [textual replace](#replacing-text) |
| ``replace `PATTERN` with `CODE` expect COUNT`` | a [structural replace](#replacing-code) |

`delete` parses but is [not built yet](#not-built-yet).  A misspelt
statement is refused, naming the ones there are:

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

### Scope and filters: `in` and `where`

A statement's clauses follow its operand in the order `in`, `where`, `expect`, each at most once.

- **`in` says which files a statement looks at**: a path or a glob, by the
  [path rules](#paths-and-globs).

  ```
  $ quaff -e 'find "TODO" in "src"'
  src/app.py:7:7-7:10: TODO
  [exit 0]
  ```

- **`where` keeps what passes its tests**: `matches` and `contains`, of `FILE` or of a
  metavariable the pattern binds, joined with `and` and turned round with `not`.  There is no
  `or`: write a statement for each alternative.

  ```
  $ quaff -e 'find "TODO" where not FILE matches **".md"'
  src/app.py:7:7-7:10: TODO
  [exit 0]
  ```

- **A metavariable is compared by what its match bound**: a string literal by its content, as
  written between its quotes, with the quotes and any prefix taking no part, and any other code
  by its text.  A glob over a value takes the same markers as a path's, with a line in place of
  a directory name, so `*` stays within a line and `**` crosses lines:

  ```
  $ quaff -e 'find `log.info($s, $x)` where $s matches "user "*'
  src/app.py:11:5-11:34: log.info("user %s", user.name)

  $ quaff -e 'find `return $x` where not $x contains "("'
  src/totals.py:4:5-4:25: return subtotal + tax
  src/totals.py:8:5-8:31: return price - price * rate
  ```

- **With a backticked pattern, `matches` is the pattern matching the bound node itself, and
  `contains` it matching the node or anything inside it:**

  ```
  $ quaff -e 'find `return $x` where $x matches `response.$m($a)`'
  src/app.py:10:9-10:34: return response.error(404)
  src/app.py:12:5-12:46: return response.ok(render(user, response))

  $ quaff -e 'find `return $x` where $x contains `render($a, $b)`'
  src/app.py:12:5-12:46: return response.ok(render(user, response))
  ```

  A name the `where`'s pattern shares with the statement's is the same code in both, as a name
  used twice in one pattern is: `find `pair($m, $u)` where $m matches `format($u)`` keeps
  `pair(format(a), a)` but not `pair(format(a), b)`.  A shared name must be one node in both,
  never a sequence in either (exit 3).
- **A match is kept if any way it binds its names passes the whole `where`.**  In
  `connect($before..., $x, $after...)`, `$x` can be any argument but the first and last, so
  `where $x contains "password"` keeps a call with a password anywhere among them - and
  `where not $x contains "password"` keeps a call any of whose middle arguments has none, which
  is not the same as having no password at all.
- **A test of a metavariable must test one the pattern binds to one node**: a textual search binds
  none, `$_` binds nothing, and a sequence binds many, so each is refused (exit 3).  A test is
  also one more use of its name: where a pattern and its `where` together would make the search
  grow faster than the square of a list's length, as two names used twice can
  ([Runs of statements](#runs-of-statements)), the statement is refused the same way.
- **A file a statement leaves out by its path is never parsed for it**, so a Python file quaff
  cannot parse there cannot [refuse the run](#which-files): leave it out with `in`, or with
  `where not FILE matches`.
- **The scope on the command line narrows every statement and never widens one**: a statement
  with an `in` looks only at what is in both.  A note says so where the scope is deeper than the
  `in`, or shares nothing with it, so that the statement looks at nothing.

## Paths and globs

The scope on the command line, `in` and `FILE` follow one set of rules.

- **A path is relative to the current directory**, as the output prints it, so from `src/` a
  file in `docs/` is `../docs/notes.md`.  **One that begins `:/` starts at the project root**,
  or the current directory with no project: from `src/`, `in ":/docs"` is the root's `docs/`.
  Only a leading `:/` counts, and a directory really named `:` is written `./:`.  **An absolute
  path is where it says**, whatever the current directory: `in "/srv/app/"**".py"` is every
  Python file under `/srv/app`.
- **A glob's leading directory is a directory on disk**, as a path with no wildcard is: its
  text up to the last `/` before any wildcard.  So from `src/`, `"../"**` is every file under
  the parent, `src/` included, as `in ".."` is, though the output prints those as `app.py`
  rather than `../src/app.py`.  Through a symbolic link it is where the link leads: where
  `alias` links to `src`, `in "alias/"**` finds the files of `src`, as `in "alias"` does.  A
  glob with no leading directory, such as `**".py"`, is matched against the path as printed.
- **A path with no wildcard names a file or a directory**, and a directory stands for everything
  under it: `in "src"`, `where not FILE matches "src/app.py"`.
- **A glob is quoted text and wildcards, and text inside quotes is always literal.**  Each
  wildcard is a marker outside the quotes, joined to the text beside it, so nothing is ever
  escaped:

  | marker | matches |
  | --- | --- |
  | `*` | any run of characters within one directory name |
  | `**` | any run of characters, across directory names |
  | `?` | one character, never `/` |
  | `[abc]`, `[a-z]`, `[!a]` | one character in the set, or with `!` first one not in it, never `/` |
  | `{"src","tests"}` | any one of the alternatives, each quoted text and wildcards in turn |

  So `in "src/"**".py"` is every `.py` file under `src`, and `where not FILE matches
  **"/test_"*".py"` leaves out test files below the top directory.  In a class, a `]` that is a
  member goes first and a `-` first or last, and nothing else is special.
- **A glob matches the whole path**.  After its leading directory, `..` is an ordinary
  directory name to it.  `FILE contains "x"` is `x` anywhere in the path as printed, the same as
  `FILE matches **"x"**`, so `FILE contains "./"` is those two characters, not a start.
- **An `in` that names nothing there fails the run** (exit 7), as a missing scope does: a path
  that is not there, or a glob whose leading directory is not.  A glob written inside the quotes
  is literal, so `"**/*.md"` names a file of that name, and the message gives the marker
  spelling:

  ```
  $ quaff -e 'find "TODO" in "**/*.md"'
  quaff: -e expression 1, at character 16: `in` names "**/*.md", which is not there; text in quotes is literal, so to match a pattern write **"/"*".md"
  [exit 7]
  ```

- **A `FILE matches` path that names nothing there is only noted**, since it may be there to
  guard against a file that does not exist yet.

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

## Replacing text

`replace string "TEXT" with "TEXT" expect COUNT` replaces every match of the first text with the
second.  Each match prints as `find` prints it, and a note says what was replaced:

```
$ quaff --dry-run -e 'replace string "response" with "reply" expect 5'
docs/notes.md:4:25-4:32: response
src/app.py:6:29-6:36: response
src/app.py:10:16-10:23: response
src/app.py:12:12-12:19: response
src/app.py:12:37-12:44: response
quaff: dry run: 5 matches in 2 files would be replaced; nothing was written
[exit 0]
```

Without `--dry-run`, the files are written, and the note is `replaced 5 matches in 2 files`.

- **An edit must say how many it changes.**  `expect` is required, with any of the counts an
  [assertion](#assertions) takes; `expect any` opts out.  Zero is never implied: a replace that
  finds nothing fails unless its count allows none.
- **Nothing is written unless every count holds**, and the run stops at the first that does not,
  exit 2:

  ```
  $ quaff -e 'replace string "response" with "reply" expect 3'
  docs/notes.md:4:25-4:32: response
  src/app.py:6:29-6:36: response
  src/app.py:10:16-10:23: response
  src/app.py:12:12-12:19: response
  src/app.py:12:37-12:44: response
  quaff: -e expression 1, at character 1: expected exactly 3 matches of "response" to replace, found 5; nothing was written
  [exit 2]
  ```

- **Every edit is written at the end of the run, or none is.**  Each file is written in place, so
  its hard links, owner and permissions are kept, and every file is checked for writing before any
  is written: one that cannot be written stops the run with nothing changed, exit 7.  If a write
  fails partway, every file already written is put back as it was, and the message names any that
  could not be.  **Two gaps remain.**  If quaff is killed, or the machine loses power, while it
  is writing, a file can be left half-written.  And quaff does not yet flush what it writes to
  disk, so a power loss soon after a run that succeeded can lose its edits, all of them or only
  some files.
- **A later statement sees an earlier edit**: after `replace string "subtotal" with "net" expect
  3`, a `find "net"` counts the three new matches.
- **Every byte outside the matches is kept**, line endings included, mixed ones too.  The text is
  [joined, broken and anchored](#joining-line-breaks-and-anchors) as in a `find`, and a `$^` in
  the replacement writes the file's own line ending: the one ending the line the match starts on,
  the last one before it when that line has none, or LF in a file with none.  An anchor in the
  replacement is an error, since it matches rather than writes.
- **A file is written back in its own encoding**, a byte-order mark and a declared encoding
  included.  One of [unknown encoding](#encodings) is never edited: its possible matches are
  noted, and do not count towards the `expect`, so a replace counts what the same `find` would.
  If a file's encoding cannot hold the replacement, or spells it more than one way, the run is
  refused, exit 9, naming the file, and nothing is written.
- **A replace that would break a Python file that parsed is refused**, exit 9, naming the file
  and where it would no longer parse, and nothing is written.  A Python file that did not parse
  before is still edited, since a textual edit needs no parse.

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
| `$name` | any one node - an expression, a statement, an argument, a parameter, a name - bound to `name` |
| `$_` | any one node, bound to nothing |
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

The same holds whatever role the name plays: `$x = $o.$x` matches `name = self.name`.  And it
holds across nodes: `f($a..., $b...) + g($a...)` matches `f(1, 2, 3) + g(1, 2)`, `$a` taking
`1, 2` in `f` because `g` holds that, though `1` alone would let `f` match by itself.

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
starts or ends a run of statements with a sequence hole, puts two sequences together in one, or
whose search could grow faster than the square of a block's or list's length ([Runs of
statements](#runs-of-statements)).  So is a hole inside a name Python reads as one, such as a
dotted module name: `import $m` is a hole for the whole name, but `import a.$m` is refused, since
`a.m` is one name, not two.

**A pattern is parsed as a file is, so it starts at the left margin.**  Code copied from inside a
function keeps its indentation, which Python refuses at the start of a file, and so does quaff:
`-p '    f($x)'` is refused with `Unexpected indentation`.  Take the indentation off; a pattern
matches the same code at any depth.

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

**A tuple's or a generator's own parentheses** take no part either, since they are only how it
is written: `return $a, $b` matches `return (a, b)`, and `h(q for q in r)` matches
`h((q for q in r))`.  Where they change the meaning the shape changes too, so `f(($a, $b))`, a
call with one tuple argument, does not match `f(a, b)`, a call with two.  Nor do the parentheses
around a `from` import's names, `from x import (a, b)`, around a `with` statement's items, `with
(open(p), open(q)):`, or of a class with no bases, `class A():`, which Python records no more
than it does a tuple's.

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

**Two sequences may not stand together in a run of statements**, since nothing says how many
statements each takes:

```
$ quaff -p 'open_db()
$a...
$b...
close_db()' play
quaff: -p, at character 17: "$b..." follows another sequence with no statement between them, so nothing says how many statements each takes: use one sequence, or put a statement between them
[exit 3]
```

**A search may depend on two places at once where it can go more than one way, but not
three.**  Such a place is where a sequence stops, and, in a run of statements, where the run
starts.  A sequence named twice is fine: `w()`, `$a...`, `x()`, `$a...`, `y()` depends on where
the run starts and where the first `$a...` stops.  So is a name bound in one statement and used in
a later one.  Inside a node, which starts where it stands, `f($a..., $b...) + g($a..., x, $b...)`
is fine too, since one stop divides `f`'s arguments between `$a` and `$b`.  Places in lists that
share no name do not add up, since neither changes what the other is matched with: `f(g($a..., x,
$a...), h($b..., y, $b...))` is accepted.  Each place multiplies the work of a search by the
length of the block or list it stands in, so a third is refused rather than left to make a search
slow on a long file:

```
$ quaff -p 'open_db()
$a...
commit()
$b...
close_db()
$a...
commit()
$b...
close_db()' play
quaff: -p, at character 26: "$b..." makes a search depend on a third place at once where it can go more than one way - where a sequence stops, or where a run of statements starts - and each such place multiplies the work of a search by the length of the block or list it stands in, so a pattern may depend on two: give one of the names used twice a name of its own
[exit 3]
```

Every pattern that is accepted searches each block or list in time at most proportional to the
square of its length, and usually in proportion to its length, whether the sequences stand in a
run of statements or inside a node.

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

  When Python is in scope but none of it could be searched, because every such file is binary,
  it says `no Python file in scope could be searched`, and counts the binary files.
- **A Python file quaff cannot parse refuses the whole run**, exit 9, before anything is printed,
  because a count that silently skipped a file could not be trusted:

  With a directory `broken/` beside `play/` holding only `bad.py`, which is `def broken(:`:

  ```
  $ quaff -p 'wrap($x)' broken
  quaff: refused: 1 Python file in scope cannot be parsed, so a structural search cannot count matches in it:
    "broken/bad.py": line 1, column 12: Expected a parameter or the end of the parameter list
  To search without it, leave it out of the statement: with `in`, with `where not FILE matches`, such as where not FILE matches "broken/bad.py", or project-wide by listing it in .ignore
  [exit 9]
  ```

  A textual search over the same files still runs, since it needs no parse - but not in a run
  that also holds a structural pattern, since the refusal stops the whole run.  A file Python
  itself refuses to read - a bad encoding declaration, or a byte its encoding does not have -
  does not parse either, and nor, to quaff, does one in an encoding Python reads and quaff does
  not, such as GBK: quaff cannot count matches in it.  A binary Python file, one with a NUL byte
  in its first 8000 bytes, is skipped and counted with the binary files instead, as any binary
  file is.
- **A Python file is read as Python reads it**: in the encoding its coding line declares, or
  UTF-8.  An `.editorconfig` or git attribute that says otherwise does not change that, because it
  does not change what Python runs.  One in an encoding Python reads and quaff does not, such as
  GBK, refuses a structural search, as above.

## Replacing code

``replace `PATTERN` with `CODE` expect COUNT`` replaces every match of a structural pattern with
the code after `with`, in which the pattern's holes stand for what they captured.  Everything a
[textual replace](#replacing-text) promises holds: an edit says how many it changes, nothing is
written unless every count holds, every edit is written at the end of the run or none is,
`--dry-run` writes nothing, a file is written back in its own encoding, and a replace that would
break a Python file that parsed is refused, exit 9.

With `src/store.py` holding:

```python
def load(path):
    log("loading %s", path)
    with lock:
        data = read(path)
        check(data)
    return data


def save(path, data):
    log("saving %s to %s", data,
        path,)
    write(path, data)
```

```
$ quaff --dry-run -e 'replace `log($msg, $args...)` with `logger.info($msg, $args...)` expect 2'
src/store.py:2:5-2:27: log("loading %s", path)
src/store.py:10:5-11:14: log("saving %s to %s", data, path,)
quaff: dry run: 2 matches in 1 file would be replaced; nothing was written
[exit 0]
```

**A rewrite is built from two sources only.**  A hole writes back the source it captured,
exactly as written, the brackets that only wrap it included; everything else comes from the
template, exactly as written, its comments included.  Layout the template does not decide - line
breaks and comments inside a bracket, where they mean nothing - is the file's: `$args...` keeps
everything between the arguments it captured, and `$a, $b` keep the line break between the two
arguments where the template's `, ` stands, but not when the rewrite reorders them.

- **A name the pattern does not bind is an error**, exit 3, before any file is read: `$_`, `$...`,
  a typo, or a name the pattern spells as a sequence written as one node, or the other way round.
- **In a string, an f-string's or t-string's literal part, or a comment**, `$name` and `${name}`
  write the captured source text, quotes and all, as text: in a string, whatever it would
  otherwise read as its end or as code - a quote, a backslash, a line break in a one-line string,
  a brace in an f-string - is escaped, and a note says so.  A name runs to the end of the
  identifier, so `"$name_id"` is the name `name_id`; `"${name}_id"` separates them.  A `$`
  before anything but a name or a brace is text, as in `"costs $5"`, and a literal `$` before a
  name is written as Python writes it, `"\x24HOME"` or `'$' 'HOME'`:

  ```
  $ quaff -e 'replace `deprecated($name)` with `warn("$name is deprecated")` expect 1'
  app.py:5:1-5:20: deprecated(old_name)
  quaff: replaced 1 match in 1 file
  [exit 0]
  ```

  leaves `warn("old_name is deprecated")`.  Inside braces only a bound name may stand, so
  `${name|upper}` is refused as a value filter this build does not have, exit 5.
- **Indentation is derived, never written.**  A template's later lines take the indentation of the
  line the match starts on, in the file's own indent unit; captured code moved to another depth
  is shifted with it, except the lines of a multi-line string, which are part of its value, and
  lines inside a bracket that opened before the capture, whose indentation says nothing of
  depth: a call's arguments one to a line keep theirs when the call is rewritten round them.  So
  unwrapping a block is a replace:

  ```
  $ quaff -e 'replace `with lock:
      $body...` with `$body...` expect 1'
  src/store.py:3:5-5:19: with lock: data = read(path) check(data)
  quaff: replaced 1 match in 1 file
  [exit 0]
  ```

  leaves `data = read(path)` and `check(data)` at the function's own level.  Nothing else is
  reformatted: a project's formatter does the rest.
- **Line breaks are the file's own**: each line break the template writes is spelled as the
  file's line ending where the match stands, as `$^` writes one in a textual replace, except one
  inside a string, which is part of its value.
- **A trailing comma survives the edit, or goes with its bracket.**  A pattern matches a bracket
  whether or not it ends with a comma, so the template cannot say what becomes of one, and a
  formatter reads it as asking for one item to a line.  It is carried to the bracket in the rewrite
  that holds the same holes, in any order: ``replace `log($msg, $a, $b)` with `log($msg, $b, $a)`
  expect 1`` over `save` above leaves `log("saving %s to %s", path, data,)`, and ``replace
  `f($a, g($b))` with `h($a, k($b))` expect 1`` over `f(x, g(y,))` leaves `h(x, k(y,))`.  Brackets
  holding no hole correspond by their order.  Where the rewrite has no bracket holding what the
  comma's did, it deleted or split that bracket, and the comma goes with it: ``replace `wrap($x)`
  with `$x` expect 1`` over `wrap(a,)` leaves `a`.  A bracket a hole matched is the hole's, written
  back with its own comma or dropped with it, `$_` and `$...` included, and one the rewrite leaves
  empty has nothing for a comma to trail.
- **Layout at a bracket's edges is the file's** where the template writes none there: a call laid
  out one argument to a line stays so when it is renamed, and where the template writes layout of
  its own at an edge, it stands.
- **A comment at a bracket's edge goes with the bracket**: one just inside the opening bracket,
  before the first item, or after the last item and before the closing one, is carried to the
  rewrite's corresponding bracket as a trailing comma is, so rewriting
  `self.assertEqual(  # inputs are iterators` to itself changes nothing.  A comment is content,
  so where the rewrite deletes its bracket, or writes a comment of its own at the same edge, the
  edit is refused.
- **A match inside another's hole is rewritten first**, and the hole writes back the rewritten
  code: ``replace `f($x)` with `g($x)` expect 2`` over `f(f(1))` leaves `g(g(1))`, and unwrapping
  `with` blocks unwraps nested ones too.

**What cannot be done as written is refused**, exit 9, naming the file and where, and nothing is
written.  A replace whose count does not hold says so first, exit 2, as any edit does:

- **Matches that overlap outside each other's holes**: a run of statements tried from every start
  can overlap the next, and rewriting either would change the other, so a replace of them is
  refused, naming both.  Narrow the pattern so that its matches cannot overlap.
- **A trailing comma with no one place to go**: where the rewrite has some brackets holding the same
  holes as the comma's, but a different number from the match, more or fewer, so which one it
  belongs to is not known, or where the comma would change what the code means there, as in a
  bracketed expression, where it would make a tuple.  ``replace `max($a, $b)` with
  `($a if $a > $b else $b)` expect 1`` over a call to `max` that ends `y,` is refused.  Narrow the
  pattern so that it does not match there, or take the comma out first.
- **A comment in no capture the rewrite writes back**, which the edit would drop:

  ```
  $ quaff -e 'replace `load($p, $s)` with `load($p, strict=$s)` expect 1'
  quaff: refused: the replace cannot be made as written in 1 file, so nothing was written:
    "app.py": line 2, column 12: the comment "# the user's file" is in the match but in no capture the rewrite writes back, so the edit would drop it: capture it with a sequence hole, such as $args..., or narrow the pattern so that it does not match here
  [exit 9]
  ```

  where `app.py` holds `config = load(` then, each on a line of its own and indented by four
  spaces, `path,  # the user's file` and `strict,`, then `)`.  A comment on a compound
  statement's header, before the first statement of its body, or before an `elif`, `else` or
  `except` is in no hole's reach, so an edit that spells such a header is refused wherever one has
  such a comment.
- **A value a literal cannot hold as text**: a raw string cannot escape its own quote, a comment
  cannot hold a line break, and a bytes literal holds ASCII only, so writing such a value would
  change more than the literal's text.

**What may not mean what was meant is written, and questioned.**  A capture written where Python
reads it together with what surrounds it is written as the template says, since that is what was
asked for, and a note names the file, where, and the capture:

```
$ quaff -e 'replace `f($a)` with `$a * 2` expect 1'
calc.py:1:9-1:16: f(x + 1)
quaff: replaced 1 match in 1 file
quaff: "calc.py": line 1, column 9: what $a captured, "x + 1", is not code of its own where the rewrite writes it - Python reads it together with what stands around it - so its meaning may have changed: if that is not what was meant, put brackets round the hole in the rewrite, as in ($a)
[exit 0]
```

where `calc.py` holds `total = f(x + 1)`: it now holds `total = x + 1 * 2`, which Python reads as
`x + (1 * 2)`.  If that is not what was meant, a further replace can put it right; put brackets
round the hole in the rewrite, `($a) * 2`, and it is written as one piece of code.
`return wrap($x)` over `return a, b` is questioned the same way, since it calls `wrap` with two
arguments.

## What is searched

- **Files ignored by `.gitignore`, `.ignore` or `.git/info/exclude` are skipped.**  `.ignore`
  outranks `.gitignore`, which outranks `info/exclude`, and within each kind the nearest
  directory with a matching rule decides.  A `.gitignore` and `info/exclude` apply only inside
  a repository, so with no project only `.ignore` applies - until the search enters a
  repository, a submodule or one in a directory that is in no project, which keeps its own.
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
| 3 | a usage error, a malformed script or pattern, an empty program, or a scope holding a double quote |
| 5 | a statement or construct this build does not have |
| 7 | an I/O error, naming the path and quoting the system's error; an `in` that names nothing there; or edits that could not be written, saying what was left as it was |
| 9 | a structural search needs a Python file quaff cannot parse, an edit would break a Python file that parses, a file's encoding cannot write a replacement, or a structural replace cannot be made as written - its matches overlap outside each other's holes, it would drop a comment, a trailing comma has no one place to go, or a literal cannot hold a value as text: the script or the file must change first |

**A run that cannot give an answer - exit 3, 5, 7 or 9 - prints nothing on standard output**, so
a partial answer never passes for a whole one.  The whole program is read and checked before any
file is looked at, so a script with an error anywhere does not run at all.

## Not built yet

These parse, and are refused with exit 5 at the place they were written, rather than being
ignored or treated as unknown:

- `delete`;
- a structural `replace` whose rewrite has no code, which would delete what matched.

```
$ quaff -e 'find "a"; delete `a` expect 1'
quaff: -e expression 1, at character 11: delete is not built yet
[exit 5]
```

The options `-o` and `--version` are refused as not built yet too, with exit 3.

## Peculiarities

Edge cases and decisions that are easy to trip over, with the reason for each.

- **After a replace, positions are in the file as edited.**  A statement after a replace sees its
  edits, so the lines and columns it prints are where the text is after them, which is where it
  will be once written - but not where it is on disk, even with `--dry-run`.
- **A file's encoding is decided once, from disk.**  A replace that edits an `.editorconfig` or a
  `.gitattributes` does not change how the other files are read or written in the same run.
- **A match replaced by the same text counts as replaced**, towards the `expect` and the note,
  but changes no bytes, so nothing is written to its file.

- **An empty program is an error, not an empty answer.**  `-e "$CHECKS"` with the variable unset
  exits 3, naming the sources, rather than searching for nothing and exiting 1.
- **A quoted glob as the scope is not expanded.**  It is reported as not found, with a note that
  globs are not expanded; `in` takes globs.  A scope holding a double quote is refused, so that
  a pattern there can mean something later.
- **A glob's leading directory is where it is on disk, not text in the path as printed**: from
  `src/`, `"../"**` takes in `src/app.py`, printed `app.py`, and `"alias/"**` finds the files of
  `src` where `alias` links to it, printed under `src/`.  `FILE contains` is text anywhere in
  the path as printed, so `FILE contains "alias"` finds none of the files of `src`.
- **A star stays within one directory name**: `where not FILE matches *".txt"` leaves out only
  the `.txt` files at the top, and `**".txt"` every one.
- **The default scope is the current directory, not the project**: from a subdirectory, `:/`
  searches the whole project.
- **A script is UTF-8.**  Anything else is refused at its first byte that is not.  A leading
  byte-order mark is skipped, so locations count from the first visible character.
- **A possible match never decides an assertion**, so that a file quaff cannot read never blocks
  an answer about the files it can.
- **Structural matches inside matches are each reported**, so `f($x)` over `f(f(1))` is two
  matches.
- **A structural replace rewrites a match inside another's hole first**, but refuses, exit 9,
  matches that overlap otherwise, such as runs of statements that share some, though a `find` of
  the same pattern counts them, since rewriting one would change the other.
- **A rewrite drops what no hole captures**, layout and the template's spelling replacing it, but
  never a comment or a trailing comma: one at a bracket's edge is carried to the rewrite's matching
  bracket, a comma goes with a bracket the rewrite deletes, and any other comment in no capture
  refuses the edit.  A sequence hole captures everything between its first node and its last,
  and the whole of any bracket that only wraps one of them.
- **A trailing comma and a semicolon between statements are layout**, so `f($a, $b)` matches a
  call a formatter has split over lines and ended with a comma.
- **Implicitly concatenated plain strings are joined, but f-strings are not**: `"abc"` matches
  `"ab" "c"`, while `f"{a}{b}"` does not match `f"{a}" f"{b}"`, which `f"{$x}" f"{$y}"` does.
- **A tuple's parentheses are layout**, as CPython's tree has them: `a, b = f()` then
  `return (a, b)` returns the name it assigned.  So is a comma ending a tuple, `a, b,`, and so are
  the parentheses of `from x import (a, b)`, of `with (open(p), open(q)):` and of `class A():`.
  The comma of a one-element tuple is not, so `x[$a,]` matches `x[a,]` but not `x[a]`, and a
  name used twice still tells `a, (b, c)` from `(a, b), c`.
- **A self-documenting f-string field keeps its spacing**: `f"{v=}"` does not match `f"{v = }"`,
  since one prints `v=1` and the other `v = 1`.  Spacing in any other field takes no part.
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
  fixed code on both sides of it.  So `return $x` then `$dead...`, every statement after a
  `return`, cannot be written.
- **A search may depend on at most two places at once where it can go more than one way** -
  where a sequence stops, or where a run of statements starts - and two sequences may not stand
  together in a run of statements.  Matching patterns with names used more than once is, in
  general, a problem no algorithm solves quickly, so the pattern is refused where its search could
  grow faster than the square of a block's or list's length, rather than accepted and left to run
  for minutes on a long file.  A sequence named twice is fine; two sequences each named twice are
  fine inside a node, which starts where it stands, but not in a run of statements, which could
  start anywhere.  Two sequences together inside a node, `f($a..., $b...)`, are fine: the node
  bounds them.
- **Structural patterns ignore non-Python files rather than falling back to text**, and say so.
- **A Python file quaff cannot parse stops a structural search** (exit 9): one Python refuses to
  read, and one Python reads in an encoding quaff does not, such as GBK.  A textual query alone
  still searches it, but one in the same run as a structural pattern is stopped with it.
- **A Python file with no coding line is UTF-8 to a structural search, whatever `.editorconfig`
  or git attributes say**, since that is how Python reads it.  A textual search follows those
  declarations, as listed under [Encodings](#encodings), so with `charset = latin1` for `*.py`,
  `find "café"` misses a UTF-8 `café` that a structural search reads correctly.
