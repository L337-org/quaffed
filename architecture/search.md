# Textual search

What `quaff TEXT [PATH]`, `quaff -s TEXT [PATH]` and a script's textual `find` do, and why.
This page also carries the command line and the exit codes as far as they exist; the script
language itself is `script.md`.  Specifications of further kinds of search join this page rather
than replacing it.

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
  a usage error.  A script is read as UTF-8, exit 3 if it is not: a file or standard input is
  refused at its first byte that is not, and an `-e` argument, which arrives whole, is refused
  naming which `-e` it was.  A leading UTF-8 byte-order mark on a file or standard input is
  skipped, as it is in a searched file, so locations count from the first visible character;
  anywhere else it is content inside an operand and an error outside one.  A program with no
  statements at all - every source empty, or only blank lines and comments, as `-e "$CHECKS"`
  with the variable unset gives - is refused too, exit 3, naming the sources, rather than
  searching for nothing and exiting 1.
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

1. **The project root** is found by the project's version control system, through the
   workspace (`src/workspace.rs`), which is the only part of quaff that knows any system.  For
   git it is the nearest directory at or above the current one that contains `.git`, tested for
   existence rather than type, so a submodule's `.git` file counts (`src/workspace/git.rs`).
   `.gitignore` is not a marker.
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
   - **no ignore file above the project root applies**, or above the current directory with no
     project, so that nothing outside the project changes which files are searched.  The walk
     starts at the root and is narrowed to the scope as it goes down, so the ignore files between
     the root and a scope still apply.  The `ignore` crate still opens the ignore files above
     where a walk starts, to find repository boundaries, but quaff applies none of them;
   - no git configuration is read, so neither is a global excludes file;
   - hidden files are searched;
   - version-control metadata is skipped by name at any depth: `.git`, `.hg`, `.svn`, `.bzr`,
     `_darcs`, `.jj`, `.pijul` and `CVS`;
   - symbolic links are not followed, and the run says how many it met;
   - FIFOs, sockets and device files are not read - reading a FIFO waits for a writer - and the
     run says how many it met;
   - **a scope that names a file or an ignored directory is searched even so** - naming it is
     asking for it - **but it crosses only that one boundary.**  Every ignore rule from the
     project root down still applies beneath it, so inside a named `build/` that the root ignores,
     a `*.log` file the root also ignores is not searched, and neither is a directory another
     rule ignores.  An ignored directory is never reached by the walk from the root, so it is
     walked from itself, and the ignore files between the root and it are read by
     `src/discover.rs` and applied beneath it, each directory's taking precedence over those
     above, as the walker's own do.  **Accepted limitation:** beneath such a scope, a rule in an
     ignore file inside it that un-ignores something a rule above it ignores, such as `!keep.log`
     against the root's `*.log`, is not honoured, since the walker applies the inner rules first
     and does not say when one of them matched;
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
than decoding the file (`crates/encoding` and `src/search.rs`).  The encoding comes from what
declares it, never from statistics, and is decided by `src/resolve.rs` as `encoding.md`
describes: a byte-order mark, a Python file's coding line, the version control system's
declaration, EditorConfig's `charset`, or the bytes being proven UTF-8 - the first that applies.
What is decided then settles how the file is searched:

| the file is | a query is matched |
|---|---|
| binary, by git's test: a NUL in its first 8000 bytes, unless a byte-order mark or a declaration says UTF-16 or UTF-32 | not at all: the run says how many binary files it skipped |
| in a known encoding | as its bytes in that encoding, each match starting and ending on a character, so bytes that match inside a multi-byte character, as in Shift-JIS, are not a match.  A character the encoding cannot hold matches nothing; one it spells more than one way leaves the file not searched for that query, and a note says so.  Every match is a hit |
| of unknown encoding: not proven UTF-8 and nothing declaring it, or declared in an encoding quaff does not read | an ASCII query as its bytes, which are the same in every ASCII-compatible encoding.  Where they are absent there is certainly no match.  Where present, each is a **possible match**: reported by line, never counted, and the file is never edited, since in some encodings those bytes end a character.  A non-ASCII query is not searched there, and a note says so.  A file declared UTF-16 or UTF-32 by a name quaff does not read, such as plain `UTF-16`, is not searched for any query, since ASCII is other bytes there, and a note says so |
| a Python file `CPython` refuses whatever it holds: an encoding name Python does not know, a codec not of text, the `undefined` codec, or a declaration contradicting a UTF-8 mark | not at all, for any query: a note names it and says why |

The binary test is git's: `xdiff-interface.c` defines `FIRST_FEW_BYTES` as 8000, and
`buffer_is_binary` looks for a NUL within at most that many bytes.  A byte-order mark is
checked first, because UTF-16 and UTF-32 text is full of NULs; UTF-32's little-endian mark is
checked before UTF-16's, because it begins with it.

Matches are found left to right and do not overlap: `aa` occurs once in `aaa`.  Each file is
classified once, whatever the number of queries.

A script's textual operand adds line breaks and anchors (`script.md`), both spelled in the
file's encoding like the text.  **A line break** matches LF, CRLF or a lone CR, whichever the
file has there, preferring CRLF to its CR alone.  **A start anchor** holds at the start of the
file or just after a line ending, **an end anchor** at the end of the file or just before one.
A CRLF is one line ending, so neither anchor holds between its CR and its LF: a pattern answers
a CRLF file as it answers the same file with LF endings.

**Memory.**  A file is read whole, and positions are worked out in one pass over its
characters that keeps only the current line and column and stops at the last match, so a search
needs about the size of the largest file it reads.  No size limit is applied.

## Output

One line per match on standard output:

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
  `expect`; possible matches, with their lines and why the file's encoding is unknown - naming
  the declaration and where it is, when one declared an encoding quaff does not read; files of
  unknown encoding not searched for a non-ASCII query; a file not searched for a query its
  encoding spells more than one way; a Python file `CPython` refuses and why; binary files not
  searched; ignore and attributes file lines skipped; special files not read; symbolic links not
  followed.  Anything not looked at, or looked at without certainty, is said, never silently left
  out.  When the user's locale names an encoding that files of unknown encoding read cleanly in,
  a note says so as a hint; the locale never decides anything, since it describes the user's
  terminal and not the project.
- **A failed assertion** goes to standard error before the notes, as where it was written, the
  count expected and the count found:
  `quaff: checks.quaff:3:1: expected exactly 2 matches of "TODO", found 1`.  One whose scope
  held files its query could not be searched in also names those files; the count is of hits
  only, never of possible matches.  A message names a pattern by its text quoted, a line break
  as `\n`, with any anchors outside the quotes as a script writes them: `^"x"$`.  Statements
  after a failed block do not run, so they add no notes.
- A closed standard output - piping into `head` - stops the printing quietly.  Assertions are
  still evaluated, so the exit code is still the answer.

## Exit codes

| code | when |
|---|---|
| 0 | every assertion held; or, with no assertion, something was found |
| 1 | nothing was found, and there was no assertion; not an error |
| 2 | an assertion failed |
| 3 | a usage error, a malformed script, a program with no statements, an edit with no `expect`, or a scope outside the project |
| 5 | a statement or construct this build does not have: outside the MVP, or in it and not built yet |
| 7 | an I/O error, naming the path and quoting the system's error |

**An assertion block** - a run of consecutive `find ... expect` statements - is evaluated to
the end: every assertion in it is checked and each that fails is reported.  If any failed, the
run stops after the block, exit 2, and nothing after it runs.

**An assertion is decided by hits alone.**  Possible matches, and files not searched for its
query, are noted but never decide it, so that a file quaff cannot read never blocks an answer
about the files it can, and a `find` and an edit with the same `expect` count the same matches.
The cost is that `expect none` can hold while a possible match exists in a file whose encoding
is not declared; the note says where, and declaring the file's encoding makes it count.  Binary
files never decide an assertion either: they are not text.

**Every source is read and parsed, and the whole program checked, before any file is looked
at**, so a script with an error anywhere does not run at all.  A script error names where it is
- `refactor.quaff:12:5`, `<stdin>:12:5`, or `-e expression 2, at character 10` - and what was
expected (`script.md`).  The constructs that parse but are not built yet - a backticked
pattern, an `in` or `where` clause, `replace` and `delete` - exit 5, named, at where they were
written.

A run that cannot give an answer - exit 3, 5 or 7 - prints nothing on standard output, so a
partial answer never passes for a whole one.
Every message says what was being done, quotes the path or argument it concerns, and says what
to do where there is something to do (`src/run.rs`).

## Checklist: changing what a search prints or how it exits

1. Change the code, and the unit tests in the module that owns the behaviour.
2. Update the end-to-end snapshot that shows it, reviewing the new `.snap` before accepting it;
   `verification.md` says how.  A new message or exit path gets its own snapshot.
3. Update this page in the same change.
4. If the dependency graph changed, name every new crate in `packaging/deb/copyright`;
   `crates/quaffed/tests/packaging.rs` fails until it is there.
