// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `quaff`, the command-line entry point.
//!
//! What is built so far is textual search: `quaff TEXT [PATH]` and `quaff -s TEXT [PATH]`
//! find every occurrence of the text across the project.  `architecture/search.md` specifies
//! it as built.

mod cli;
mod discover;
mod encoding;
mod output;
mod project;
mod run;
mod search;

use std::io::Write;
use std::process::ExitCode;

use cli::{Invocation, UsageError};
use run::Failure;

// The Linux package is a static musl binary, and musl's own allocator parsed far slower than
// mimalloc on the static-linking spike's workload; see architecture/release.md.  Other builds
// use the platform's.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The help, printed by `-h` and `--help`, and to standard error when no arguments are given.
fn help() -> String {
    format!(
        "quaff {} - the QUAntiFied File EDitor

Usage:
  quaff TEXT [PATH]        search the project for TEXT
  quaff -s TEXT [PATH]     the same, with TEXT given as an option; -s may be repeated
  quaff -h, --help         this help

TEXT is literal: nothing in it is special.  Put -- before TEXT that starts with a dash.

A search covers the whole project - the nearest directory at or above this one that holds
.git - or this directory when there is no project.  PATH narrows it to one file or directory
inside the project.  Files ignored by .gitignore, .ignore or .git/info/exclude are skipped,
and so is version-control metadata.  Hidden files are searched.

Each match prints on its own line as path:line:column-endline:endcolumn: text, with columns
counted in characters.  Binary files are not searched, and the run says how many there were.

Exit status: 0 found, 1 nothing found, 3 usage error, 7 I/O error.
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
        Invocation::Search { queries, scope } => {
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
            let mut out = std::io::stdout().lock();
            let mut notes = std::io::stderr().lock();
            let result = run::search(&queries, scope.as_ref(), &cwd, &mut out, &mut notes);
            // Flushed before any failure is reported, so the two streams stay in order.  Not
            // checked: every line was written through, so a failure here has nothing left to lose.
            let _ = out.flush();
            match result {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(1),
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
