// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The help, printed by `-h` and `--help`, and the version, printed by `--version`.
//!
//! The help is written for a reader who has never seen `quaff`, an agent above all, and must be
//! enough on its own to drive it: every invocation form and every statement has a worked example
//! under _Examples_.  `tests/help_examples.rs` builds the project the examples show and runs every
//! `$ quaff` line in a fresh copy of it, so an example that stops working fails the build.  The
//! help's examples section of `architecture/testing.md` says how that test reads the help.

/// The help, for `-h` and `--help`, and on standard error when no arguments are given.
pub fn help() -> String {
    format!(
        "quaff {} - the QUAntiFied File EDitor\n{BODY}",
        env!("CARGO_PKG_VERSION")
    )
}

/// What `--version` prints: the version from the workspace manifest.
pub fn version() -> String {
    format!("quaff {}\n", env!("CARGO_PKG_VERSION"))
}

/// The help after its first line.  Raw, so that the examples read as they are typed.
const BODY: &str = r#"
Usage:
  quaff TEXT [PATH]        search the project for TEXT
  quaff -s TEXT [PATH]     the same, with TEXT given as an option
  quaff -p PATTERN [PATH]  search the project's Python files for code shaped like PATTERN
  quaff -e SCRIPT [PATH]   run the statements in SCRIPT
  quaff -f FILE [PATH]     run the script in FILE; -f - reads it from standard input
  quaff --dry-run ...      print each match an edit would change, but write nothing
  quaff -o text ...        print matches as text, the only output this build has
  quaff -h, --help         this help
  quaff --version          the version

-s, -p, -e and -f may each be repeated and mixed, and run in the order given, as one program.
With any of them, the first positional argument is PATH.  TEXT is literal: nothing in it is
special.  Put -- before TEXT that starts with a dash.  The long forms are --string, --pattern,
--expression, --file and --output, each taking its value as the next argument or after =.

A script is statements separated by newlines or ;, with # comments.  The statements are:

  find "TEXT"                                     find text; `find` may be left out
  find `PATTERN`                                  find code by its structure, as -p does
  replace string "TEXT" with "TEXT" expect COUNT  replace text
  replace `PATTERN` with `CODE` expect COUNT      replace code, keeping what $names matched
  delete `PATTERN` expect COUNT                   delete code, with its line if it is alone

and each takes, in this order, `in`, `where` and `expect`, each at most once:

  in "PATH"            look only at this path, or glob: wildcards go outside the quotes, as
                       in "src/"**".py"; a glob matches the whole path as printed
  where TEST           keep a match only if TEST holds: FILE or a $name, then `matches` or
                       `contains`, then quoted text, a glob or a `PATTERN`; join tests with
                       `and`, and turn one round with `not`.  There is no `or`.  FILE is the
                       path as printed, so FILE contains "x" is x anywhere in the path, not in
                       the file
  expect COUNT         an assertion: fail, exit 2, unless the number of matches is COUNT.
                       COUNT is N, at least N, at most N, N or none, none or any

A replace or a delete must say how many it changes, and changes nothing unless the count
holds.  Consecutive assertions are one block: every one is checked, each that fails is
reported, and the run stops after the block.  Every edit is written at the end of the run, into
each file in place, or none is - unless quaff is killed while writing, which can leave a file
half-written.

Text is double-quoted and a pattern backticked, and nothing inside either is escaped: to include
the delimiter, fence the operand with three or more, as """ say "hi" """ for say "hi": a fence
trims one space from each end.  Adjacent operands join, $^ between them is a line break, and
^ and $ outside the quotes anchor to the start and end of a line.

A search covers this directory.  PATH narrows it to one file or directory, relative to this
directory, or from the project root - the nearest directory at or above this one that holds
.git - when it begins :/; a path outside the project is searched by that project's rules.
Files ignored by .gitignore, .ignore or .git/info/exclude are skipped, and so is
version-control metadata.  Hidden files are searched.

A pattern is Python code in which $name matches any one node - an expression, a statement, an
argument, a parameter, a name - and $name... one or more items in a row; $_ and $... are the
same bound to no name.  A hole always matches something, and a name used twice must match the
same code each time.  Comments, layout and redundant parentheses take no part.  A pattern
applies to Python files only, and a Python file in scope quaff cannot parse refuses the run.
An edit that would leave a Python file unparseable is refused, and nothing is written.

Each match prints on its own line as path:line:column-endline:endcolumn: text, with columns
counted in characters.  Notes go to standard error, prefixed quaff:.  Binary files are not
searched, and the run says how many there were.

Examples.  Each runs from the root of this project, a git repository, and exits 0; an example
that edits is shown as if run on its own:

  app.py
  | import logging
  |
  |
  | def greet(name):
  |     print("hello", name)  # TODO: log instead
  |
  |
  | def main():
  |     greet("world")
  |     print("done")

  tests/test_app.py
  | from app import greet
  |
  |
  | def test_greet():
  |     greet("you")  # TODO: check the output

  checks.quaff
  | # A lint: run in CI with --dry-run, so that it never writes
  | find `breakpoint()` expect none

Searching:

  $ quaff TODO
      every TODO in the project: app.py:5:29-5:32: TODO, then the one in the test
  $ quaff TODO tests
      only those under tests/
  $ quaff -- --dry-run
      text that starts with a dash, after --: the line in checks.quaff
  $ quaff -s TODO -p 'greet($who)' tests
      two searches in one run, the second by structure: greet("you")
  $ quaff -p 'print($a, $b)' :/app.py
      every call of print with two arguments, in app.py from the project root
  $ quaff -o text -p 'print($args...)'
      both calls of print, whatever their arguments

Asserting, in a script:

  $ quaff -e 'find "TODO" expect 2'
      holds, so exits 0 and prints both; with expect 3 it fails, exit 2, saying so
  $ quaff -e 'find ^"def" expect 3; find `$x.$y` expect none; find "FIXME" expect at most 1'
      one block of three assertions, all checked: three lines start def, no code reads an
      attribute, and there is at most one FIXME
  $ quaff -e 'find "def greet(name):" $^ "    print" expect 1 or none'
      text across a line break, matching none or one
  $ quaff -e 'find """ "hello" """ expect any'
      text holding its own quotes, fenced
  $ quaff -e 'find "TODO" in "tests/"** expect at least 1'
      scoped by a glob
  $ quaff -e 'find `greet($who)` where not FILE matches "tests/"** expect 1'
      every call of greet outside the tests: greet("world")
  $ quaff -e 'find `print($x, $y)` where $y matches "name" and FILE contains "app" expect 1'
      a test of what $y matched, and of the path

Editing:

  $ quaff -e 'replace string "hello" with "hi" expect 1'
      prints the match it replaces, and says on standard error how many it replaced
  $ quaff -e 'replace `print($a, $b)` with `logging.info("%s %s", $a, $b)` expect 1'
      print("hello", name) becomes logging.info("%s %s", "hello", name)
  $ quaff -e 'delete `print("done")` expect 1'
      deletes the call, and its line with it
  $ quaff --dry-run -e 'delete `print($x)` expect 1'
      prints what it would delete, and writes nothing

Scripts from a file or standard input:

  $ quaff -f checks.quaff
      runs the lint, which holds
  $ echo 'find "TODO" expect 2' | quaff -f -
      the script from standard input
  $ quaff --version
      quaff and its version

Exit status: 0 found, or every assertion held; 1 nothing found, with no assertions; 2 an
assertion failed; 3 usage or script error; 5 a statement or construct this build does not
have; 6 a file to be edited changed after it was read, so nothing was written; 7 I/O error;
9 a Python file a structural search needs cannot be parsed, or an edit would break a Python
file or cannot be written in a file's encoding.
"#;
