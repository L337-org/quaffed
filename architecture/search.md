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
- **An option in the MVP that is not built** - `-o`, `--version` - is refused as not built, not
  as unknown.  Both exit 3.  A short option never takes `=VALUE`.

## Discovery

What a user sees is in `docs/language.md`, _What is searched_.  How it is built:

1. **The project root** comes from the workspace (`src/workspace.rs`), the only part of quaff
   that knows any version control system.  For git it is the nearest directory at or above the
   current one containing `.git`, tested for existence rather than type, so a submodule's `.git`
   file counts.  With no root, the current directory stands in.
2. **Discovery starts from a base**, a path that exists: the scope the command line gives, or,
   for a statement with an `in` and none, the directory its glob's leading text names, or the
   current directory (_Scope and filters_ below).  Each distinct base is walked once, by **the
   rules of the project it is in**: the current directory's under its root, otherwise the one
   `workspace::find` finds above the base, or with none the base itself standing in, as it would
   if quaff ran there.  Each project keeps its own declarations, for its files' encodings.
3. **quaff walks the project itself** (`src/discover.rs`), with the standard library's directory
   listing and the `ignore` crate's pattern matcher - ripgrep's - but not its walker, because:
   - **no ignore file above the project root may be read**, so that nothing outside the project
     changes what is searched, and the crate's walker cannot be told to stop there;
   - **a directory is listed only once it is to be entered**, so an ignored `node_modules/`
     costs nothing; a walker that filters what it has listed reads every name first.  A test
     counts the directories listed.
4. **Ignore files rank as the `ignore` crate's walker ranks them**, and a repository's own stop at
   its root and apply only within a repository, as git's do.  So with no project the walk is still
   given git's ignore files (`workspace::ignore_files_outside`), and a repository it enters keeps
   its own rules, as a submodule does inside a project; a file in one takes its declared encoding
   from it too (`Projects::of_file`).  Which files are listed is held to git's own answer by a test
   replaying what `scripts/generate-ignore-cases` recorded; to pin a behaviour, add a case there and
   run it again.
5. **The walk starts at the boundary and narrows to the base as it goes down**, so the ignore files
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

## Scope and filters

Which files each statement looks at (`src/scope.rs`, and `Scoping` in `src/run.rs`).  The path
rules a user sees are `docs/language.md`'s _Paths and globs_.

- **Every pattern of the run has its own scope**: the base its files come from, its `in`, and
  the part of its `where` a path alone can decide.  A file no pattern looks at is not read.
- **A path is matched as printed**, relative to the current directory with `/` between its
  segments, relative to the root for a glob that begins `:/`, and as the absolute path for one
  that is absolute.  A glob's leading directory, where it is there, is the exception: it is
  resolved on disk and the glob matched as an absolute one (`scope::on_disk`), so it names the
  directory a path with no wildcard would - `"../"**` from `src/` takes in `src/` as `in ".."`
  does - and follows a symbolic link as discovery does.  `contains` reads none of those prefixes: it is text anywhere in the path as
  printed.  A glob is compiled to a regular expression (`src/glob.rs`), with `regex-automata`,
  so a match takes time linear in the path however the glob is written.  `*`, `?` and a
  class are kept within a segment - for a path a directory name, for a string a line - and `**`
  crosses them.
- **A path with no wildcard names what is on disk**: it is resolved and compared by where it is,
  so `in "src"` and `in "../src"` from inside `src/` agree, as a scope on the command line would.
- **`where` is decided as far as it can be before a file is parsed** (`FileTest`), with every
  test of a metavariable unknown until a match binds it.  A file whose `where` is false whatever
  the unknowns are is not needed by that statement: it is never parsed for it, and so cannot
  refuse the run with exit 9.  This is the units rule, and it is why excluding by `FILE` and by
  `in` behave alike.
- **A test of a metavariable is decided per match** (`src/filter.rs`): `find_where` in
  `quaffed-syntax` offers every way a match binds the tested names, and the match is kept if any
  way passes the whole `where`.  A quoted glob compares a string literal's content or other code's
  text; a backticked pattern is compiled before any file is read, searched for once per file, and
  matches the bound node when it has the node's range, or is inside it for `contains`.  A pattern
  that shares a name with the statement's is instead matched at the bound node with that name bound
  in advance (`quaffed_syntax::matches_at`), once per binding, remembered, and the shared name is
  tested too, so the cost check counts it.  Each tested name counts as one more use of it, read once
  the match is complete, so `Pattern::costly_with` bounds the search as it bounds a name used twice,
  and a pattern that with its `where` would be too costly is refused before any file is read.
- **The command-line scope narrows every statement**: a statement's `in` still names its own
  base, and fails the run if that is not there; the deeper of the two is walked, and a
  statement whose `in` and scope share nothing looks at nothing.  So an `in` finds the same
  files whether or not a scope holding it is given.  A note says so only where the scope
  changes what the statement looks at: where it is deeper than the `in`, or shares nothing
  with it.
- **A path is only computed where something reads it**: a run with no `in` and no `where`
  works out no file's path as a glob would see it, and a structural search works it out for a
  file only where one of its patterns has a `where`.
- **What names nothing is said**: an `in` whose path or leading directory is not there fails the
  run, exit 7, as a missing scope does, and a `FILE matches` path that is not there is noted.
  Either gives the marker spelling of a path that reads as a glob (`scope::as_markers`).

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
| 3 | a usage error, a malformed script, a program with no statements, an edit with no `expect`, or a command-line scope holding a double quote |
| 5 | a statement or construct this build does not have: outside the MVP, or in it and not built |
| 7 | an I/O error, naming the path and quoting the system's error; an `in` that names nothing there; for edits that could not be written, saying what was left |
| 9 | the script or a file must change first: a structural search needs a Python file quaff cannot parse, naming each and how to leave it out; an edit would break a Python file that parsed; a file's encoding cannot write a replacement; or a structural replace cannot be made as written |

**An assertion block** - consecutive `find ... expect` statements - is evaluated to the end, and
if any assertion failed the run stops after it, exit 2.

**An assertion is decided by hits alone.**  Possible matches, files not searched for its query
and binary files never decide it, so that a file quaff cannot read never blocks an answer about
those it can, and a `find` and an edit with the same `expect` count the same matches.

**The whole program is read, parsed and checked before any file is looked at**, so a script with
an error anywhere does not run.  A construct that parses but is not built exits 5, named, where
it was written.

**A run that cannot give an answer - exit 3, 5, 7 or 9 - prints nothing on standard output**, so
a partial answer never passes for a whole one.  Every message says what was being done, quotes
the path or argument it concerns, and says what to do where there is something to do
(`src/run.rs`).

## Editing

`replace string` edits text (`src/run.rs`, `src/search.rs`, `src/write.rs`), and a structural
`replace` edits code (`src/rewrite.rs`), through the same pipeline.  `delete` is a structural
`replace` whose rewrite is empty.

- **Each file is taken through the program's statements in order** and read once: the patterns
  between two replaces are searched in one pass over the file as it then is, and a replace changes
  it for the statements after.  With no replace, that is one pass, as for any search.  The
  encoding is decided once, from the file on disk.
- **Edits are held in memory to the end of the run**, with every changed file's original bytes,
  so an edit holds every file it changes until then.
- **The outcome is decided before anything is printed or written.**  A silent first pass over the
  results finds, statement by statement, whether each count holds, and for a replace whose count
  holds, whether any file's encoding cannot write its replacement and whether a structural
  replace cannot be made as written; then every changed Python file that parsed must still
  parse; then the files are written, unless `--dry-run`; and only then are the results printed.
  So a count that does not hold is told first, exit 2, whatever else is wrong with the edit, and
  a run refused with exit 7 or 9 prints nothing and writes nothing.  Only a Python file that
  cannot be parsed refuses the run before any count, since a structural count over it cannot be
  trusted.
- **Only a file of known encoding is edited**, its replacement spelled in that encoding, a line
  break as the file's own line ending (`search::replace`).  A file of unknown encoding is reported
  as in a search and never touched, so a replace counts what the same `find` counts.
- **Files are written in place**, into the file itself, never by renaming a new file over it,
  which would break its hard links and change its owner.  Every file is opened for writing, and
  closed, before any is written, then opened again only while it is written, so no limit on open
  files is reached; a failed write puts back every file already written from its original bytes.
  **Accepted limitation:** a process killed, or a machine losing power, during the writes can
  leave a file half-written.  **Nothing is flushed to disk yet**, deliberately, while how to make
  writes durable is undecided, so a power loss soon after an edit can lose it too; `write.rs` has
  a hook for each system after each file is written and after them all, each a no-op, where a
  flush goes once decided.  `src/write.rs` is the only file besides `src/contents.rs` that
  opens a project's files, and only to write; `contents_containment.rs` holds both.
- **A structural replace edits a Python file's decoded text** and encodes the result back as the
  file is encoded (`python::Source::encode`), which gives a file's own bytes back exactly, so every
  byte outside the matches is kept.  A character the encoding cannot hold is refused as for a
  textual replace.  Each match is written over with its rewrite: what each hole captured, from the
  matcher, put where the template's sites say (`quaffed_python::compile_rewrite`), each capture
  widened over brackets that only wrap it (`whole_brackets`); layout inside a bracket is the file's
  where the template writes none, at a bracket's edges (`rewrite/layout.rs`) and between two holes
  written next to each other over the same code (`Job::gaps`); and a value written into a string is
  escaped as Python's literal needs (`quaffed_python::escaped`), and questioned.  **What the user
  did not write is derived, never guessed**: the indentation of the template's later lines and of
  captured code moved to another depth, from the file's indent unit; the file's own line ending for
  the template's line breaks; and the trailing commas a pattern ignores.  **What the pattern ignored
  must survive or the edit is refused**, exit 9, naming the file and where: a comment no capture
  writes back, unless it stands at the edge of one of the match's own brackets and is carried with
  it, and a trailing comma whose place in the rewrite cannot be told.  Both are carried in
  `src/rewrite/layout.rs`.  A trailing comma the pattern ignores is one the arena keeps no token
  for, so the matcher and the edit agree on it (`ignored_by_matcher`); it is carried to the bracket
  in the rewrite that holds the same holes as its own, brackets paired from the lexer's
  (`quaffed_python::Lexed::brackets`), goes with its bracket where the rewrite has none that does,
  and is refused where the rewrite has some but a different number from the match, or where the
  comma would be a token where it lands, as a tuple's is.  A bracket inside what a hole matched is
  the hole's, so a replace's unnamed holes are given names no script can spell (`name_every_hole`)
  and captured as named ones are.  **Matches nested in one another's holes are rewritten in
  levels**, the most deeply nested first, each level an ordinary rewrite of the text the one before
  left, with every place a message names mapped back to the file as it was (`nested`).  **Also
  refused**: matches that overlap outside each other's holes, and an interpolation that changes the
  token structure of the literal it is written into, found by lexing the rewrite with the values and
  with a filler, because each would change something the script did not ask for.  **A capture that
  is no longer one node**, or a run of them, once the file is written, is written as the template
  says and questioned in a note, since the template is what was asked for.  A refusal is decided in
  the silent first pass, once its replace's count holds.
- **A rewrite with no code deletes** (`deleted`), and the rules above about what the pattern
  ignored do not apply to it: everything inside a match goes with it, comments included, since
  they describe code that is gone.  Nothing in such a template says where the code around a match
  goes, so each match takes what would otherwise be left as debris, and nothing else: its whole
  lines where it stands alone on them, with a trailing comment and the line break, the comment
  lines indented into a block it ends with, and of the blank lines either side, the narrower run,
  or those at the start or end of the file (`Deletion::whole_lines`); or else one comma its parent node holds next to it,
  across line breaks, or one semicolon on its own line (`Deletion`).  Asking the parent is what
  keeps a comma belonging to a neighbouring expression from being taken, and a comma in a
  comment, or with a comment between it and the match, is never taken.  A run of list items,
  each taking the separator after it, leaves the last with none, so the run takes the one before
  it.  A tuple left with one item and no comma after it would be that item alone, so a comma is
  written after it (`one_item_tuples`).  A rewrite that is only a comment is written in place of
  the code, takes no comma from another line, and never takes the line; it is refused where code
  follows it on its line, which it would swallow.  An empty block left behind does not parse,
  and the parse check refuses it.  A match that takes nothing and is an operand is refused
  (`OPERATIONS`): its operator would be left applied to what is left, which can parse and mean
  something else, as `x =  + a` does.  An empty rewrite is reported as a delete however it is
  spelled (`Replacing::deletes`); one that is only a comment is reported as the replace it is.
- **A file an earlier replace broke is refused as an edit** when a later structural statement
  needs it, naming the replaces that changed it, not as a file that cannot be parsed, since on
  disk it parses.

## Checklist: changing what a search prints or how it exits

1. Change the code, and the unit tests in the module that owns the behaviour.
2. Update the end-to-end snapshot that shows it, reviewing the new `.snap` before accepting it;
   `testing.md` says how.  A new message or exit path gets its own snapshot.
3. Update `docs/language.md`, and this page where the design changes, in the same change.
4. If the dependency graph changed, cover every new crate in `packaging/deb/copyright`, as
   `release.md` says; `crates/quaffed/tests/packaging.rs` fails until it is there.
