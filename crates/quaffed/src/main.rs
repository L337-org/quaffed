// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `quaff`, the command-line entry point.
//!
//! It runs textual and structural search and the script language's search subset:
//! `quaff TEXT`, `-s TEXT`, `-p PATTERN`, and scripts from `-e` and `-f` of `find` statements,
//! of text or backticked patterns, with or without an `expect`.  `architecture/search.md` and
//! `architecture/script.md` describe its design.

mod cli;
mod contents;
mod discover;
mod editorconfig;
mod output;
mod resolve;
mod run;
mod search;
mod structural;
mod workspace;

use std::io::Write;
use std::process::ExitCode;

use cli::{Invocation, UsageError};
use run::Failure;

// The Linux package is a static musl binary, and musl's own allocator makes parsing markedly
// slower than mimalloc; see architecture/release.md.  Other builds use the platform's.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The help, printed by `-h` and `--help`, and to standard error when no arguments are given.
fn help() -> String {
    format!(
        "quaff {} - the QUAntiFied File EDitor

Usage:
  quaff TEXT [PATH]        search the project for TEXT
  quaff -s TEXT [PATH]     the same, with TEXT given as an option
  quaff -p PATTERN [PATH]  search the project's Python files for code shaped like PATTERN
  quaff -e SCRIPT [PATH]   run the statements in SCRIPT
  quaff -f FILE [PATH]     run the script in FILE; -f - reads it from standard input
  quaff -h, --help         this help

-s, -p, -e and -f may each be repeated and mixed, and run in the order given.  With any of
them, the first positional argument is PATH.  TEXT is literal: nothing in it is special.  Put
-- before TEXT that starts with a dash.

A script is statements separated by newlines or ;, with # comments:

  find \"TODO\"                      print every match; `find` may be left out
  find \"TODO\" expect none          an assertion: fail, exit 2, unless the count holds
  find \"def\" $^ \"    pass\"         $^ is a line break; adjacent quoted text joins
  find ^\"x\"$                       ^ and $ anchor to the start and end of a line
  find `open($path)` expect 2      a backticked pattern is matched by structure, like -p

Consecutive assertions are one block: every one is checked, each that fails is reported,
and the run stops after the block.  Counts are N, at least N, at most N, N or none, none and
any.  Nothing in quoted text is escaped: to include a \", fence the text with three or more,
as \"\"\" say \"hi\" \"\"\".

A search covers the whole project - the nearest directory at or above this one that holds
.git - or this directory when there is no project.  PATH narrows it to one file or directory
inside the project.  Files ignored by .gitignore, .ignore or .git/info/exclude are skipped,
and so is version-control metadata.  Hidden files are searched.

A pattern is Python code in which $name matches any one node - an expression, a statement,
an argument, a parameter, a name - and $name... one or more in a row; a name used twice must
match the same code each time.  Comments, layout and redundant parentheses take no part.  A
pattern applies to Python files only, and a Python file in scope quaff cannot parse refuses
the run.

Each match prints on its own line as path:line:column-endline:endcolumn: text, with columns
counted in characters.  Binary files are not searched, and the run says how many there were.

Exit status: 0 found, or every assertion held; 1 nothing found, with no assertions; 2 an
assertion failed; 3 usage or script error; 5 a statement or construct this build does not
have; 7 I/O error; 9 a Python file a structural search needs does not parse.
",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(UsageError::NoArguments) => {
            eprint!("{}", help());
            return ExitCode::from(3);
        }
        Err(err) => return fail(&Failure::Usage(err)),
    };
    match invocation {
        Invocation::Help => {
            print!("{}", help());
            ExitCode::SUCCESS
        }
        Invocation::Run { sources, scope } => {
            let cwd = match std::env::current_dir() {
                Ok(cwd) => cwd,
                Err(cause) => {
                    return fail(&Failure::Io {
                        doing: "reading the current directory",
                        path: ".".into(),
                        cause,
                    });
                }
            };
            let mut stdin = std::io::stdin().lock();
            let mut out = std::io::stdout().lock();
            let mut notes = std::io::stderr().lock();
            let result = run::run(
                sources,
                scope.as_ref(),
                &cwd,
                &mut stdin,
                &mut out,
                &mut notes,
            );
            // Flushed before any failure is reported, so the two streams stay in order.  Not
            // checked: every line was written through, so a failure here has nothing left to lose.
            let _ = out.flush();
            match result {
                Ok(outcome) => ExitCode::from(outcome.exit_code()),
                Err(failure) => fail(&failure),
            }
        }
    }
}

/// Reports `failure` on standard error and returns its exit code.
fn fail(failure: &Failure) -> ExitCode {
    eprintln!("quaff: {failure}");
    ExitCode::from(failure.exit_code())
}
