# Textual search

What `quaff TEXT [PATH]`, `quaff -s TEXT [PATH]` and a script's textual `find` do, as built.
This page also carries the command line and the exit codes as far as they exist; the script
language itself is `script.md`.  Structural search and edits are not built; when they land,
their specifications join this one rather than replacing it.

## The command line

| form | means |
|---|---|
| `quaff TEXT` | search the project for `TEXT` |
| `quaff TEXT PATH` | the same, narrowed to `PATH` |
| `quaff -s TEXT [PATH]`, `--string TEXT`, `--string=TEXT` | the same, as an option |
| `quaff -e SCRIPT [PATH]`, `--expression SCRIPT`, `--expression=SCRIPT` | run the statements in `SCRIPT` |
| `quaff -f FILE [PATH]`, `--file FILE`, `--file=FILE` | run the script in `FILE`; `-f -` reads it from standard input |
| `quaff -h`, `quaff --help` | the help, on standard output, exit 0 |
| `quaff` | the help, on standard error, exit 3 |
| `--` | ends option parsing, so `quaff -- --verbose` searches for `--verbose` |

- **Sources compose in the order given.**  `-s`, `-e` and `-f` may each be repeated and mixed,
  and their statements make one program in command-line order; `-s TEXT` is `find` of that text,
  exactly.  `-f -` may be given once, because standard input can be read only once; a second is
  a usage error.  A script is read as UTF-8, and a script that is not is refused at the first
  byte that is not, exit 3.
- **The first positional is the query only when no `-s`, `-e` or `-f` was given**; otherwise it
  is the scope.
- **There is exactly one scope argument.**  More than one is refused, naming them and the
  shell's glob expansion as the likely cause: bash 3.2 expands `src/**/*.py` to one file of
  many and the search would look successful.
- **The query is literal.**  Nothing in it is special: `^`, `$`, `*` and `[` match themselves.
  Anchors and line breaks are a script construct, written outside an operand's quotes.
- **Options in the MVP whose stories have not landed** - `-p`, `-o`, `--dry-run`, `--version` -
  are refused as not built yet, rather than as unknown, which would be wrong.  Every other
  option is unknown.  Both exit 3.  A short option never takes `=VALUE`: `-s=x` is unknown.
- **Parsing is hand-written** (`src/cli.rs`).  The order of sources matters, and an argument
  library's own usage errors exit 2, which here means an assertion failed.

## What a search covers

1. **The project root** is the nearest directory at or above the current one that contains
   `.git`, tested for existence rather than type, so a submodule's `.git` file counts
   (`src/project.rs`).  `.gitignore` is not a marker.
2. **Without a scope, a search covers the whole project**, from wherever `quaff` runs.  With no
   project root it covers the current directory.
3. **A scope** is one file or directory path, relative to the current directory.  It must exist -
   otherwise exit 7, naming it - and must lie inside the project, because a command-line scope
   narrows and never widens - otherwise exit 3.  With no project root the current directory is
   the whole search, so it bounds the scope instead, again with exit 3.  **It is a path, not a
   glob**: a quoted glob that reaches quaff is reported as not found, with a note that globs are
   not expanded.  This is the MVP's behaviour and is expected to change.
4. **Discovery** (`src/discover.rs`) uses the `ignore` crate, ripgrep's walker:
   - `.gitignore`, `.ignore` and `.git/info/exclude` are honoured, `.gitignore` only inside a git
     repository, as git itself does;
   - no git configuration is read, so neither is a global excludes file;
   - hidden files are searched;
   - version-control metadata is skipped by name at any depth: `.git`, `.hg`, `.svn`, `.bzr`,
     `_darcs`, `.jj`, `.pijul` and `CVS`;
   - symbolic links are not followed, and the run says how many it met;
   - FIFOs, sockets and device files are not read - reading a FIFO waits for a writer - and the
     run says how many it met;
   - a scope that names a file or an ignored directory is searched even so - naming it is asking
     for it - and the ignore rules apply beneath it;
   - **a line of an ignore file that cannot be parsed** - `a{b`, which git reads as literal
     braces, or `[z-a]` - is skipped, and the rest of that file applies.  The run names the file
     and the line on standard error, with the glob parser's own words, and searches on.  Skipping
     a rule usually searches more files, but a skipped negation such as `!a{b` leaves ignored a
     file it would have brought back; either way the note says which line went;
   - anything else the walker cannot read - a directory that cannot be listed, an ignore file
     that cannot be opened - stops the run with exit 7 rather than searching the rest and
     answering short.  The message names the path relative to the current directory, quoted,
     with the system's own error.
5. Files are searched in path order, so output is the same on every run.

## Matching, in the file's own encoding

A query is matched in each file's encoding by encoding the query the way the file is, rather
than decoding the file (`src/encoding.rs`, `src/search.rs`).  The encoding is taken from what
the file declares, never guessed from statistics:

| the file | is | a query is matched |
|---|---|---|
| starts with a byte-order mark | UTF-8, UTF-16 or UTF-32, as the mark says | encoded to match; a match must start on a code unit |
| no mark, and a NUL in its first 8000 bytes | binary, by git's own test | not at all: the run says how many binary files it skipped |
| no mark, valid UTF-8 | UTF-8 | as its UTF-8 bytes |
| anything else | an unknown 8-bit encoding | if ASCII, as its bytes, which are the same in every ASCII-compatible encoding; otherwise not at all, and the run says how many files it skipped for that query |

The binary test is git's: `xdiff-interface.c` defines `FIRST_FEW_BYTES` as 8000, and
`buffer_is_binary` looks for a NUL within at most that many bytes.  A byte-order mark is
checked first, because UTF-16 and UTF-32 text is full of NULs; UTF-32's little-endian mark is
checked before UTF-16's, because it begins with it.

Matches are found left to right and do not overlap: `aa` occurs once in `aaa`.  Each file is
classified once, whatever the number of queries.

A script's textual operand adds line breaks and anchors (`script.md`).  **A line break** matches
LF, CRLF or a lone CR, whichever the file has there, preferring CRLF to its CR alone.  **A start
anchor** holds at the start of the file or just after a line ending, **an end anchor** at the
end of the file or just before one.  Both are spelled in the file's encoding like the text.

**Memory.**  A file is read whole, and positions are worked out in one pass over its
characters that keeps only the current line and column and stops at the last match, so a search
needs about the size of the largest file it reads.  No size limit is applied.  Python's
encoding declarations are not read yet; when the Python front end decodes them, textual search
should take a Python file's encoding from there too.

## Output

One line per match on standard output, textual and structural alike when structural arrives:

```
src/app.py:12:5-12:22: def connect(self):
```

- `path:line:column-endline:endcolumn: text`: the start and the last character of the match,
  both inclusive.
- **Lines** end at LF, CRLF or a lone CR.  **Columns** count characters from 1, in the file's
  encoding; a byte that does not decode counts as one, so a Latin-1 file counts as an editor
  showing it as Latin-1 would.
- **The path is relative to the current directory**, so a search from a subdirectory prints
  `../app.py`.
- **The text is the matched text**, folded onto one line: each line ending, with the
  indentation after it, becomes one space, never nothing, so `return` and `value` on two lines
  do not read as `returnvalue`.  It is printed in full.
- With several queries, each query's matches print together, in program order: a `find` with
  an `expect` prints its matches as a `find` without one does.  Every file is read once
  whatever the number of queries.
- **Notes go to standard error**, prefixed `quaff:`: no matches for a `find` without an
  `expect`, binary files not searched, files in an unknown encoding not searched for a
  non-ASCII query, ignore file lines skipped, special files not read, symbolic links not
  followed.  Anything not looked at is said, never silently left out.
- **A failed assertion** goes to standard error before the notes, as where it was written, the
  count expected and the count found:
  `quaff: checks.quaff:3:1: expected exactly 2 matches of "TODO", found 1`.
- A closed standard output - piping into `head` - stops the printing quietly.  Assertions are
  still evaluated, so the exit code is still the answer.

## Exit codes

| code | when |
|---|---|
| 0 | every assertion held; or, with no assertion, something was found |
| 1 | nothing was found, and there was no assertion; not an error |
| 2 | an assertion failed |
| 3 | a usage error, a malformed script, an edit with no `expect`, or a scope outside the project |
| 5 | a statement or construct this build does not have: outside the MVP, or in it and not built yet |
| 7 | an I/O error, naming the path and quoting the system's error |

**An assertion block** - a run of consecutive `find ... expect` statements - is evaluated to
the end: every assertion in it is checked and each that fails is reported.  If any failed, the
run stops after the block, exit 2, and nothing after it runs.

**Every source is read and parsed, and the whole program checked, before any file is looked
at**, so a script with an error anywhere does not run at all.  A script error names where it is
- `refactor.quaff:12:5`, `<stdin>:12:5`, or `-e expression 2, at character 10` - and what was
expected (`script.md`).  The constructs that parse but are not built yet - a backticked
pattern, an `in` or `where` clause, `replace` and `delete` - exit 5, named, at where they were
written.

A failure prints nothing on standard output, so a partial answer never passes for a whole one.
Every message says what was being done, quotes the path or argument it concerns, and says what
to do where there is something to do (`src/run.rs`).

## Checklist: changing what a search prints or how it exits

1. Change the code, and the unit tests in the module that owns the behaviour.
2. Update the end-to-end snapshot that shows it, reviewing the new `.snap` before accepting it;
   `verification.md` says how.  A new message or exit path gets its own snapshot.
3. Update this page in the same change.
4. If the dependency graph changed, name every new crate in `packaging/deb/copyright`;
   `crates/quaffed/tests/packaging.rs` fails until it is there.
