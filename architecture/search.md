# Textual search

How `quaff` searches, textually and structurally, and why it is built that way.  This page also
carries the command line and the exit codes; the script language is `script.md`, and what a user
sees, with examples, is `docs/language.md`.  Further kinds of search join this page.

## The command line

The forms and options are in `docs/language.md`.  What the parser (`src/cli.rs`) holds to:

- **Parsing is hand-written**, because the order of sources matters and an argument library's
  own usage errors exit 2, which here means an assertion failed.
- **Sources compose in the order given**: `-s`, `-p`, `-e` and `-f` make one program in
  command-line order.  `-s TEXT` is `find` of that text exactly, and `-p PATTERN` is `find` of a
  backticked operand.  `-f -` may be given once, since standard input can be read only once.
- **A script is UTF-8**, exit 3 otherwise, naming the first bad byte, or for `-e` which
  expression.  A leading byte-order mark on a file or standard input is skipped, as in a
  searched file; anywhere else it is content inside an operand and an error outside one.
- **A program with no statements is refused**, exit 3, naming the sources, rather than
  searching for nothing and exiting 1.
- **The first positional is the query only when no `-s`, `-p`, `-e` or `-f` was given**;
  otherwise it is the scope.  More than one scope is refused, naming the shell's glob expansion
  as the likely cause, since the search would otherwise look successful over one file.
- **The query is literal**: anchors and line breaks are script constructs, outside the quotes.
- **An option in the MVP that is not built** - `-o`, `--dry-run`, `--version` - is refused as
  not built, not as unknown.  Both exit 3.  A short option never takes `=VALUE`.

## Discovery

What a user sees is in `docs/language.md`, _What is searched_.  How it is built:

1. **The project root** comes from the workspace (`src/workspace.rs`), the only part of quaff
   that knows any version control system.  For git it is the nearest directory at or above the
   current one containing `.git`, tested for existence rather than type, so a submodule's `.git`
   file counts.  With no root, the current directory stands in.
2. **A scope** is one path, never a glob.  It must exist (exit 7) and lie inside the project
   (exit 3), because a scope narrows and never widens.
3. **quaff walks the project itself** (`src/discover.rs`), with the standard library's directory
   listing and the `ignore` crate's pattern matcher - ripgrep's - but not its walker, because:
   - **no ignore file above the project root may be read**, so that nothing outside the project
     changes what is searched, and the crate's walker cannot be told to stop there;
   - **a directory is listed only once it is to be entered**, so an ignored `node_modules/`
     costs nothing; a walker that filters what it has listed reads every name first.  A test
     counts the directories listed.
4. **Ignore files rank as the `ignore` crate's walker ranks them**, and a repository's own stop at
   its root.  Which files are listed is held to git's own answer by a test replaying what
   `scripts/generate-ignore-cases` recorded; to pin a behaviour, add a case there and run it
   again.
5. **The walk starts at the root and narrows to the scope as it goes down**, so the ignore files
   between them still apply.  A scope naming an ignored file or directory is searched, crossing
   only the boundaries on the way to it; every rule beneath it still applies.
6. **Every file is read once, through `src/contents.rs`**, whatever reads it: an ignore file,
   `.gitattributes`, `.editorconfig` or a `-f` script may also be in the scope and searched.
   Ignore files are parsed from those bytes as the `ignore` crate parses a file it opens, and a
   test holds the two to the same answers.  A test fails if any other source file of the binary
   reads a file itself.
7. **No git configuration is read**, so neither is a global excludes file.  Symbolic links are
   not followed, and FIFOs, sockets and devices are not read - reading a FIFO waits for a
   writer - each counted in a note.
8. **An ignore-file line that cannot be parsed is skipped**, with a note naming the file, the
   line and the glob parser's words.  Anything else that cannot be read stops the run, exit 7,
   rather than answering short.
9. Files are searched in path order, so output is the same on every run.

## Matching, in the file's own encoding

**A query is matched by encoding it as the file is encoded**, never by decoding the file
(`crates/encoding`, `src/search.rs`).  `src/resolve.rs` decides the encoding as `encoding.md`
describes, and that settles how the file is searched:

| the file is | a query is matched |
|---|---|
| binary: a NUL in its first 8000 bytes (git's test), unless a byte-order mark or a declaration says UTF-16 or UTF-32 | not at all; the run counts binary files skipped |
| in a known encoding | as its bytes in that encoding, each match starting and ending on a character.  A character the encoding cannot hold matches nothing; one it spells more than one way leaves the file not searched for that query, with a note |
| of unknown encoding, or declared in one quaff does not read | an ASCII query by its bytes, since they are the same in every ASCII-compatible encoding.  Each match is a **possible match**: reported by line, never counted, and **the file is never edited**, since in some encodings those bytes end a character.  A non-ASCII query is not searched, with a note.  A file declared UTF-16 or UTF-32 by a name quaff does not read is not searched at all |
| a Python file `CPython` refuses to read | not at all, with a note saying why |

The byte-order mark is checked before the binary test, since UTF-16 and UTF-32 text is full of
NULs, and UTF-32's little-endian mark before UTF-16's, which it begins with.  Each file is
classified once, whatever the number of queries.

Line breaks and anchors (`script.md`) are spelt in the file's encoding like the text; a line
break prefers CRLF to its CR alone, so a pattern answers a CRLF file as it answers one with LF.

**A file is read whole**, and positions are worked out in one pass that stops at the last
match, so a search needs about the size of the largest file.  No size limit is applied.

## Structural search

A structural pattern is matched by shape against Python files, through the syntax arena
(`syntax.md`) and the Python adapter (`python.md`).  **Which files it applies to follows the units
rule, a unit being a whole file** (`src/structural.rs`, and the file loop in `src/run.rs`):

- **A Python file**, by name alone as `resolve::is_python` decides - `.py`, `.pyi`, `.pyw` - is
  decoded as `CPython` decodes it (`python::Source::decode`), parsed and searched.  A declaration
  that gives the textual tier another encoding does not change the text Ruff is given, since it
  does not change what Python runs.
- **Any other file is not applicable**: never matched as text instead, never an error, and
  counted in a note.  **When nothing in scope could be searched structurally, the run says so**:
  a `find` exits 1, and a `find ... expect` fails, exit 2, whatever it asks, since an assertion
  about structure that could not be looked for has not passed.
- **A Python file quaff cannot parse refuses the whole run**, exit 9, before anything is
  printed, because a count that skipped it could not be trusted.  That includes a file `CPython`
  refuses to read and one in an encoding quaff does not read.  A run with no structural pattern
  still searches such files as text.
- **A binary Python file** is skipped and counted, as any binary file is.

**A pattern is compiled before any file is read**, so a pattern the compiler refuses
(`python.md`, `syntax.md`) is a script error, exit 3, and nothing is searched.  The error is
placed in its source's terms - `-p, at character 6`, or a script's line and column - not as a byte
offset into the pattern.

## Output

The format is in `docs/language.md`, _Output_.  The rules behind it:

- **Columns count characters in the file's encoding**, a byte that does not decode counting as
  one.  **Paths are relative to the current directory.**  The matched text is folded onto one
  line, each line ending and the indentation after it becoming one space, never nothing.
- **Anything not looked at, or looked at without certainty, is said** in a note on standard
  error, never silently left out.  The user's locale may prompt a hint but never decides
  anything, since it describes the terminal, not the project.
- **A failed assertion** goes to standard error before the notes, with where it was written, the
  counts expected and found, hits only, and any files its query could not be searched in.
- **A closed standard output stops the printing quietly**, but assertions are still evaluated,
  so the exit code is still the answer.

## Exit codes

| code | when |
|---|---|
| 0 | every assertion held; or, with no assertion, something was found |
| 1 | nothing was found, and there was no assertion; not an error |
| 2 | an assertion failed |
| 3 | a usage error, a malformed script, a program with no statements, an edit with no `expect`, or a scope outside the project |
| 5 | a statement or construct this build does not have: outside the MVP, or in it and not built |
| 7 | an I/O error, naming the path and quoting the system's error |
| 9 | a structural search needs a Python file quaff cannot parse, naming each and how to leave it out |

**An assertion block** - consecutive `find ... expect` statements - is evaluated to the end, and
if any assertion failed the run stops after it, exit 2.

**An assertion is decided by hits alone.**  Possible matches, files not searched for its query
and binary files never decide it, so that a file quaff cannot read never blocks an answer about
those it can, and a `find` and an edit with the same `expect` count the same matches.

**The whole program is read, parsed and checked before any file is looked at**, so a script with
an error anywhere does not run.  The constructs that parse but are not built - an `in` or `where`
clause, `replace` and `delete` - exit 5, named, where they were written.

**A run that cannot give an answer - exit 3, 5, 7 or 9 - prints nothing on standard output**, so
a partial answer never passes for a whole one.  Every message says what was being done, quotes
the path or argument it concerns, and says what to do where there is something to do
(`src/run.rs`).

## Checklist: changing what a search prints or how it exits

1. Change the code, and the unit tests in the module that owns the behaviour.
2. Update the end-to-end snapshot that shows it, reviewing the new `.snap` before accepting it;
   `testing.md` says how.  A new message or exit path gets its own snapshot.
3. Update `docs/language.md`, and this page where the design changes, in the same change.
4. If the dependency graph changed, cover every new crate in `packaging/deb/copyright`, as
   `release.md` says; `crates/quaffed/tests/packaging.rs` fails until it is there.
