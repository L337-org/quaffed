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
- **`-o` takes only `text`**, the one rendering.  Any other value is refused with exit 5, as a
  construct this build does not have, rather than exit 3, because a later build may have it.
  A short option never takes `=VALUE`.
- **The help (`src/help.rs`) is a reference that teaches by example.**  It is written for an
  agent that has never seen `quaff` and has only the help: every invocation form and every
  statement has a `$ quaff` example, run from the root of a small project the help shows.
  `tests/help_examples.rs` runs each one and requires it to exit 0, and lists the forms and
  statements that must each have one; a form or statement this build gains joins that list
  and gets an example in the same change.

## A run's phases

A run (`src/run.rs`) is a pipeline of phases, each a module of its own under `src/run/` with
one entry point, taking what the phase before gave and giving what the phases after read.  A
statement kind is added by extending these types, not by threading a new value from one end of
the run to the other.

| Phase | Module | In | Out |
|---|---|---|---|
| Reading | `sources.rs` | the sources and the command-line scope | `Built`: the program, checked, and each source's text |
| Planning | `plan.rs`, `compile.rs` | `Built`, and the languages quaff parses | `Plan`: the steps, the run's patterns in order, and each compiled for each language |
| Scoping | `scoping.rs` | `Plan`, `Built` and the current directory | `Files`: the files found, each with which patterns look at it; each `where` compiled into the `Plan` |
| Execution | `execute.rs`, `parallel.rs` | `Plan`, `Files`, and how many files to search at once | `Searched`: every match, and every edit held, not written |
| Evaluation | `evaluate.rs` | `Searched`, `Plan` and `Built` | the results printed, the edits written, and `Evaluated`: the outcome and its notes |
| Reporting | `report.rs` | `Evaluated`, `Searched`, `Files` and `Plan` | the notes, on standard error |

The failures every phase can end a run with, and how each is worded, are `failure.rs`.  The order
of the phases is what keeps a run's promises: every pattern and `where` is compiled before
discovery looks at a file, so a script error is one however large the tree; and nothing is printed
or written until evaluation has decided the outcome, so a run that fails prints nothing.

**What a run knows about each of its patterns is one record in each phase**, not lists matched
up by a pattern's index: planning's `Searching` holds the pattern, its query and what it is
replaced with, and execution's `Found` holds what it found - its matches, its counts, and the
files it could not be searched for, written in or made in.  A pattern's index is its place among
the run's patterns, in program order, and is the same in both.

### The unit of work

**Execution's unit of work is a file**: it is read once and taken through the run's patterns in
batches, in program order, as the first rule under _Editing_ says.  Only the file's own patterns -
those whose scope holds it - are looked at.  What a file gives is
a `Searched` of its own, which is gathered into the run's in discovery order: each pattern's
`Found` takes the file's matches, counts and notes after those of the files before it, and a
file the run holds is renumbered as it joins the files held so far.  Nothing a file's search does
depends on another file's, so files can be worked on in any order, and the order of the results
depends on discovery alone.

### Sharing work between threads

**Files are searched on threads of the run's own** (`src/run/parallel.rs`), as many as `-j`
says or, without it, as the machine runs at once.  Each thread takes the next file in discovery
order from a shared counter, searches it, and sends what it gave to the run's own thread, which
gathers the results strictly in discovery order, holding any that finish early until those
before them are in.  A thread starts no file more than twice the number of threads ahead of the
next to be gathered, so what waits stays bounded however long one file takes, and memory stays
in line with the files being read at once.  The first failure in discovery order ends the run, as it
would one file at a time: files after it may have been read, but nothing they gave is used.  With
one job no thread is started, and each file is searched and gathered before the next is read.
Where the system will start fewer threads than asked for, the run goes on with those it started,
or on its own thread with none, and says so in a note: a container's limit on processes must not
stop a search that could be answered more slowly.

**What the threads share is read-only, or locked for one question at a time.**  The plan and its
compiled patterns are only read.  The store files are read through (`src/contents.rs`) is locked
to look a file up or keep it, and a file it keeps - a rules file, a declaring file, a script - is
read with the lock held, so two threads asking for one at once read it once.  Each project's
encoding declarations, git's attributes (`src/workspace/git.rs`) and EditorConfig's
(`src/editorconfig.rs`), keep what they have read behind a lock held for the whole question.  A
lock per declaring file would let more threads decide at once, but one per project is simpler,
and is chosen on the reasoning that deciding an encoding costs little next to searching the file;
the benchmarks are where that would show otherwise.

**Nothing a thread does decides the order of the output.**  Matches, counts and notes come from
the gathered results.  So do the notes on lines a declaring file holds that cannot be used: a
file's search lists each declaring file it consulted with such lines, whether it read that file or
another thread did, and gathering keeps a declaring file's lines the first time a file names it,
which is where a search of one file after another meets them.  A new note that comes from state
the threads share must be gathered the same way, or it will come out in a different order from
run to run.

**A statement that works across files does not fit this unit**: one that follows references from
one file into others, or binds a set a later statement ranges over, needs what other files gave
before it can work on a file, and a file's search has nothing but the file.

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

Which files each statement looks at (`src/scope.rs`, and `src/run/scoping.rs`).  The path
rules a user sees are `docs/language.md`'s _Paths and globs_.

- **Every pattern of the run has its own scope**: the base its files come from, its `in`, and
  the part of its `where` a path alone can decide.  A file no pattern looks at is not read.
- **A path is matched as printed**, relative to the current directory with `/` between its
  segments, relative to the root for a glob that begins `:/`, and as the absolute path for one
  that is absolute.  A glob's leading directory, where it is there, is the exception: it is
  resolved on disk and the glob matched as an absolute one (`scope::on_disk`), so it names the
  directory a path with no wildcard would - `"../"**` from `src/` takes in `src/` as `in ".."`
  does - and follows a symbolic link as discovery does.  `contains` reads none of those
  prefixes: it is text anywhere in the path as printed.  A glob is compiled to a regular
  expression (`src/glob.rs`), with `regex-automata`, so a match takes time linear in the path
  however the glob is written.  `*`, `?` and a class are kept within a segment - for a path a
  directory name, for a string a line - and `**` crosses them.
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
match, so a search needs about the size of the largest files being searched at once, one for
each thread.  No size limit is applied.

## Structural search

A structural pattern is matched by shape against files in a language quaff parses, through the
syntax arena and that language's front end (`syntax.md`, and `python.md` for Python).  Such a file
has structural support.  **Each file's language is identified, and the pattern is applied as that
language reads it.**  Which files it applies to follows the units rule, a unit being a whole file
(`src/structural.rs`, and the file loop in `src/run/execute.rs`):

- **A file in a language quaff parses**, by name alone, by the extensions its front end gives in
  `languages::LANGUAGES`, is decoded as the language's own reader decodes it - for Python, as
  `CPython` does - parsed and searched.  A declaration that gives the textual tier another
  encoding does not change the text the parser is given, since it does not change what the
  language runs.
- **A file whose language cannot read the pattern is not applicable to it**: it cannot hold a
  match, so it is not searched for that pattern, never an error, and counted in a note of its own.
  Where no file in scope is in a language that reads the pattern, the run says so.
- **Any other file is not applicable**: never matched as text instead, never an error, and
  counted in a note.  **When nothing in scope could be searched structurally, the run says so**:
  a `find` exits 1, and a `find ... expect` fails, exit 2, whatever it asks, since an assertion
  about structure that could not be looked for has not passed.
- **A file with structural support that quaff cannot parse refuses the whole run**, exit 9,
  before anything is printed, because a count that skipped it could not be trusted.  That
  includes a file its language refuses to read - for Python, one `CPython` refuses - and one in
  an encoding quaff does not read.  A run with no structural pattern still searches such files
  as text.
- **A binary file with structural support** is skipped and counted, as any binary file is.

**A pattern is compiled before any file is read, once for every language quaff parses**, with
its rewrite and its `where`.  **A pattern no language reads** (`python.md`, `syntax.md`) is a
script error, exit 3, and nothing is searched; with more than one language the message gives what
each said, named.  A `where` a language cannot read leaves the pattern unread in that language, and
one that no language reading the pattern reads is a script error.  **A rewrite a language reads
the pattern but not the rewrite in** refuses the edit in each file of that language where the
pattern matches, as an edit refused, exit 9, so nothing is written; one that no language reading
its pattern can read is a script error.  The error is
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
- **Every match is held until the run prints, so a match holds where its code is, not the code**
  (`Hit` and `Code` in `src/run/execute.rs`).  However files are searched, each statement's
  matches print together, so nothing can print until the last file is done; structural matches
  can overlap, as `x = $v` and a sequence to the end of a block do from every statement in it, and
  copies of their code would hold about the square of the block's length.  So a structural match
  holds which file it was found in, which version of it - as read, or as a replace left it - and
  where in that version's decoded text.  **Of each file a structural match was found in, the run
  holds the smaller of the file and the code its matches cover** (`Held`): a few short matches in
  a large file, as most searches find, keep a copy of their code, one after another, and nothing
  else; matches that cover more than the file keep the file, and when they print, the version they
  were found in is built and decoded again, once for all of them.  So a run never holds more for a
  file than a copy of every match would.  A file a replace changed is held whole whatever its
  matches, since it is checked and written from it.  `Hit` is `Copy`, so a field that owns memory,
  such as a copy of the code, does not compile.  A textual match prints the pattern's own text, so
  it holds only where it is.

## Exit codes

| code | when |
|---|---|
| 0 | every assertion held; or, with no assertion, something was found |
| 1 | nothing was found, and there was no assertion; not an error |
| 2 | an assertion failed |
| 3 | a usage error, a malformed script, a program with no statements, an edit with no `expect`, or a command-line scope holding a double quote |
| 5 | a statement or construct this build does not have: outside the MVP, or in it and not built |
| 6 | a file to be edited changed after it was read, or was removed or renamed, naming it; nothing is written |
| 7 | an I/O error, naming the path and quoting the system's error; an `in` that names nothing there; for edits that could not be written, saying what was left |
| 9 | the script or a file must change first: a structural search needs a file quaff cannot parse, naming each and how to leave it out; an edit would break a file that parsed; a file's encoding cannot write a replacement; or a structural replace cannot be made as written |

**An assertion block** - consecutive `find ... expect` statements - is evaluated to the end, and
if any assertion failed the run stops after it, exit 2.

**An assertion is decided by hits alone.**  Possible matches, files not searched for its query
and binary files never decide it, so that a file quaff cannot read never blocks an answer about
those it can, and a `find` and an edit with the same `expect` count the same matches.

**The whole program is read, parsed and checked before any file is looked at**, so a script with
an error anywhere does not run.  A construct that parses but is not built exits 5, named, where
it was written.

**A run that cannot give an answer - exit 3, 5, 6, 7 or 9 - prints nothing on standard output**, so
a partial answer never passes for a whole one.  Every message says what was being done, quotes
the path or argument it concerns, and says what to do where there is something to do
(`src/run/failure.rs`).

## Editing

`replace string` edits text (`src/run/execute.rs`, `src/search.rs`, `src/write.rs`), and a
structural `replace` edits code (`src/rewrite.rs`), through the same pipeline.  `delete` is a
structural `replace` whose rewrite is empty.

- **Each file is taken through the program's statements in order** and read once: the patterns
  between two replaces are searched in one pass over the file as it then is, and a replace changes
  it for the statements after.  With no replace, that is one pass, as for any search.  The
  encoding is decided once, from the file on disk.
- **Edits are held in memory to the end of the run**, with every changed file's original bytes,
  so an edit holds every file it changes until then.  **A changed file is held as its bytes as
  read and the changes each replace made** (`src/changes.rs`), not as a whole copy per edit: the
  stretches each wrote over and what it wrote there.  The whole file after a replace exists only
  while the run is on that file, for the statements after it to read; the parse check, the writer
  and the printing of a match found after a replace build the version they need again, one file
  at a time.  Each replace says what it wrote
  over - a textual one its matches, a structural one its outermost matches, which hold any nested
  in them - and `Changes::between` checks that everything else is unchanged, taking any stretch
  that is not as changed, so a wrong report can make the changes larger but never wrong.
- **The outcome is decided before anything is printed or written.**  A silent first pass over the
  results finds, statement by statement, whether each count holds, and for a replace whose count
  holds, whether any file's encoding cannot write its replacement and whether a structural
  replace cannot be made as written; then every changed file with structural support that
  parsed must still parse; then the files are written, unless `--dry-run`; and only then are the
  results printed.  So a count that does not hold is told first, exit 2, whatever else is wrong
  with the edit, and a run refused with exit 6, 7 or 9 prints nothing and writes nothing.  Only a
  file with structural support that cannot be parsed refuses the run before any count, since a
  structural count over it cannot be trusted.
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
- **A file changed since it was read is never written over: optimistic checking, not locking.**
  Every read records the file's identity (`src/identity.rs`) from the open file before its bytes
  are read, so a write landing during the read is seen later.  Before any file is opened for
  writing, each is checked (`contents::unchanged`): if its identity now vouches for it, it is
  written without being read again; otherwise its bytes are read again and compared with the
  original the run holds, and only a difference, or the file no longer being there or a file,
  refuses the run, exit 6, with nothing written.  Comparing the held bytes stands in for a
  content hash, and needs none.  Nothing is locked, so nothing waits on another process and
  nothing is left behind by a crash; the cost is that a write landing between the check and the
  write is lost, which only a lock held across those moments would close.
- **An identity vouches only for what its system can show**, so each system has its own
  `platform` module in `identity.rs`.  On Unix it is the device, inode, size, and modification
  and change times: the inode sees an editor's save by rename, and the change time, which nothing
  sets back, sees an edit whose modification time was restored.  **A timestamp is only as fine as
  the clock that set it**, so, as git does for its racily clean files, a file whose newest
  timestamp is within two seconds of its read is never vouched for, and has its bytes compared.
- **A structural replace edits a file's decoded text** and encodes the result back as the file is
  encoded (`Source::encode`), which gives a file's own bytes back exactly, so every byte outside
  the matches is kept.  A character the encoding cannot hold is refused as for a
  textual replace.  Each match is written over with its rewrite: what each hole captured, from the
  matcher, put where the template's sites say (`FrontEnd::compile_rewrite`), each capture
  widened over brackets that only wrap it (`whole_brackets`).  **Layout the template does not
  decide is the file's, by one rule** (`diffed`): each stretch of the template between two
  anchors - its holes, and the match's start and end - is compared with the file's stretch
  between the same anchors, token by token, by their longest common run.  The tokens both share
  keep the file's layout between them, and only what differs is written from the template, with
  its spacing within a line beside a changed token; a line break at either end of what differs
  stays the file's, and where as many tokens differ on each side, they are paired one for one.
  A line break of the file's is kept only where it is safe where it is written - inside a
  bracket, counting the template's own and one round the whole match, or continued by a
  backslash with no comment before it - judged by the template's brackets where it is written,
  so the brackets themselves may change; a line break deleted code leaves with no code after it
  goes with that code.  The layout just inside a bracket a stretch closes first or opens last,
  in the template or the file, is that bracket's edge, carried with it (`rewrite/layout.rs`),
  unless the template keeps that bracket beside it.  A stretch
  is held to the rule between holes (`Job::gaps`) and at the match's ends (`Job::edge_gaps`),
  and checked to lex as the template's would beside the code either side, so a word is never
  run into a name (`Job::diffed_gap`).  **Accepted limitation:** a stretch is compared only
  where its tokens in the file and in the template multiply to no more than `MOST_COMPARED`,
  since the table grows with that product; a longer stretch between two anchors takes the
  template's layout, and may join lines it did not change.  The stretches are paired by place,
  not by hole (`Job::positions`): a rewrite that reorders holes keeps the file's layout between
  the places they fill, but not a comment there, written about the code beside it, which
  refuses the edit.
  Where captures overlap, as one hole written twice does, there are no places to pair, and the
  template's layout stands; a hole whose capture is empty has no place of its own, so the
  template's layout stands beside it alone.  Where the template ends a
  statement between two holes, outside any bracket, the file's blank lines stand, before the
  template's own line break and indentation, since the statements may move to another depth
  (`Job::statement_gap`);
  a bracket of the match's that no node owns, round some of a node's children, as round a Python
  `from` import's names or a tuple, is put round the children in the same places, counted from
  either end, of the template's node of that kind holding the same holes, unless a bracket of
  that node's own holds them, as a subscript's does, before the template is filled
  (`Job::rebracketed`), so that its layout is carried as any bracket's is; and a value written
  into a string is escaped as
  the language's literal needs (`FrontEnd::escaped`), and questioned.  **What the user did not
  write is derived, never guessed**: the indentation of the template's later lines and of
  captured code moved to another depth, from the file's indent unit; the file's own line ending for
  the template's line breaks; and the trailing commas a pattern ignores.  **Statements that share
  a line are given lines of their own where they need them** (`src/rewrite/lines.rs`), in a
  language whose blocks layout decides, where its `Editing` names what ends a header whose block
  may share its line - in Python, after a semicolon, or in a block written on its header's line
  after the colon.  A match that is a run of statements there, or a statement
  capture the template writes there, may need more than one line, or start or end with a
  statement holding a block, or end with a comment.  Then the code round it is laid out: the
  block is moved to lines of its own under its header, and a separator before or after the
  match becomes a line break, each line at the depth of the block the statement is in, and the
  template's later lines take that depth, not the depth of the line the match shares.  Every
  match's layout is decided before any is written, since opening a block changes text before the
  first match in it, and the changes are reported as stretches written over as the matches are.
  The indent unit is the file's, else the rewrite's; with neither, the depth is not guessed and
  the edit is refused.  **A backstop then checks the outcome**: every statement the rewrite
  writes at its top level, and each beside the match on its line, must stand in the block the
  match stood in, or the edit is refused; it is what stops a layout nobody foresaw, as a Python
  header continued by a backslash.  **What the pattern ignored must survive or the edit is
  refused**, exit 9, naming the file and where: a comment no capture writes back, unless it
  stands at the edge of one of the match's own brackets and is carried with it, and a trailing
  comma whose place in the rewrite cannot be told.  Both are carried in
  `src/rewrite/layout.rs`.  A trailing comma the pattern ignores is one the arena keeps no token
  for, so the matcher and the edit agree on it (`ignored_by_matcher`); it is carried to the bracket
  in the rewrite that holds the same holes as its own, brackets paired from the lexer's
  (`Lexed::brackets`), goes with its bracket where the rewrite has none that does,
  goes to the outermost where the match has one and the rewrite has several nested one inside the
  next, since the outermost stands where the match's did, and is refused where the rewrite has some
  but a different number from the match otherwise, or where the comma would be a token where it
  lands, as a tuple's is.  A bracket inside what a hole matched is
  the hole's, so a replace's unnamed holes are given names no script can spell (`name_every_hole`)
  and captured as named ones are.  **Matches nested in one another's holes are rewritten in
  levels**, the most deeply nested first, each level an ordinary rewrite of the text the one before
  left, with every place a message names mapped back to the file as it was (`nested`).  **Also
  refused**: matches that overlap outside each other's holes, and an interpolation that changes the
  token structure of the literal it is written into, found by lexing the rewrite with the values and
  with a filler, because each would change something the script did not ask for.  **A capture that
  is no longer one node**, or a run of them, once the file is written, is refused too
  (`regrouped`): the code would mean something other than the template says, and adding brackets
  would write something other than the template, so the message shows the template's line with
  brackets round the hole.  Brackets that only wrap an item belong to no node, so a sequence's
  first or last item may stand inside them; and a tuple's are its own, so a capture of a bare tuple
  stands whole in the template's brackets only where they make a node of the captured kind, never a
  call's.  A sequence is told to stand where its items stand apart instead, since brackets round
  it make a tuple.  **An edit that leaves code outside the construct the language requires of
  it** is refused too, once every level is written: code the parser accepts anywhere but the
  language will not run there, so neither the parse check nor the matcher would see it.  For
  Python, `FrontEnd::misplaced` finds what it covers, listed in `python.md`; only what the
  edit wrote is judged, and only beyond as many of the same kind as were outside in what it wrote
  over, so one already in the file and carried through a hole is the file's.  A text search for
  the keywords involved skips the extra parse for an edit that writes none of them.  A refusal
  is decided in the silent first pass, once its replace's count holds.
- **A rewrite with no code deletes** (`deleted`), and the rules above about what the pattern ignored
  do not apply to it: everything inside a match goes with it, comments included, since they describe
  code that is gone.  Nothing in such a template says where the code around a match goes, so each
  match takes what would otherwise be left as debris, and nothing else: its whole lines where it
  stands alone on them, with a trailing comment and the line break, the comment lines indented into
  a block it ends with, and of the blank lines either side, the narrower run, or those at the start
  or end of the file, or those on the inner side of a block's start or end, which the statement
  lists the adapter marks tell (`Deletion::block_edges`), unless a comment stands at that edge
  (`Deletion::whole_lines`); or else one separator its parent node holds next to it, as the
  language's `Editing` names them: an item separator, Python's comma, across line breaks, or a
  statement separator, Python's semicolon, on its own line (`Deletion`).  Asking the parent is
  what keeps a comma belonging to a neighbouring expression from being taken, and a separator in a
  comment, or with a comment between it and the match, is never taken.  A run of list items, each
  taking the separator after it, leaves the last with none, so the run takes the one before it.  A
  list the language needs a separator after a lone item in, as Python's tuple, left with one item
  and no separator after it would be that item alone, so one is written after it (`lone_items`).
  A rewrite that is only a comment is written in place of the code, takes no separator from
  another line, and never takes the line; it is refused where code follows it on its line, which
  it would swallow.  An empty block left behind is the language's parse check to judge, and
  Python's refuses it.  A match that takes nothing is refused unless it is a statement, which the
  parse check judges, or an item the language lets its list simply lose
  (`Editing::droppable_items`): anywhere else what holds it is left without it, which can parse
  and mean something else, as Python's `x =  + a` and a bare `return` do.  It is an allowlist, not
  a list of the kinds known to go wrong, so a position nobody thought of is refused rather than
  let through.  An empty rewrite is reported as a delete however it is spelled
  (`Replacing::deletes`); one that is only a comment is reported as the replace it is.
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
