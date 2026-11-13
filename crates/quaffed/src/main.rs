// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `quaff`, the command-line entry point.
//!
//! It reads the command line, runs what it asks for, and exits with the run's status.  What it can
//! be asked is in the help, `src/help.rs`; `architecture/search.md` and `architecture/script.md`
//! describe its design.

mod cli;
mod contents;
mod discover;
mod editorconfig;
mod filter;
mod glob;
mod help;
mod identity;
mod output;
mod resolve;
mod rewrite;
mod run;
mod scope;
mod search;
mod structural;
mod workspace;
mod write;

use std::io::Write;
use std::process::ExitCode;

use cli::{Invocation, UsageError};
use run::Failure;

// The Linux package is a static musl binary, and musl's own allocator makes parsing markedly
// slower than mimalloc; see architecture/release.md.  Other builds use the platform's.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(UsageError::NoArguments) => {
            eprint!("{}", help::help());
            return ExitCode::from(3);
        }
        Err(err) => return fail(&Failure::Usage(err)),
    };
    match invocation {
        Invocation::Help => {
            print!("{}", help::help());
            ExitCode::SUCCESS
        }
        Invocation::Version => {
            print!("{}", help::version());
            ExitCode::SUCCESS
        }
        Invocation::Run {
            sources,
            scope,
            options,
        } => {
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
                &options,
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
