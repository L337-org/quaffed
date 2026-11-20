// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Carrying out a run: the program its sources make, the scope, the files in it, the matches,
//! the assertions, and what was not looked at.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;

use quaffed_syntax::FrontEnd;

use crate::cli::{self, SourceArg};
use crate::contents::Contents;
use crate::languages;

mod compile;
mod evaluate;
mod execute;
mod failure;
mod plan;
mod report;
mod scoping;
mod sources;
#[cfg(test)]
mod tests;

pub use self::failure::Failure;

/// How a run that gave an answer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every assertion held, or, with none, something was found.  Exit 0.
    Success,
    /// There were no assertions, and nothing was found.  Exit 1.
    NothingFound,
    /// An assertion block had an assertion that did not hold.  Exit 2.
    AssertionFailed,
}

impl Outcome {
    /// The exit code this outcome ends the run with.
    pub fn exit_code(self) -> u8 {
        match self {
            Outcome::Success => 0,
            Outcome::NothingFound => 1,
            Outcome::AssertionFailed => 2,
        }
    }
}

/// Runs `sources` in the order given, printing matches to `out` and notes and failed
/// assertions to `notes`.  `stdin` is read for `-f -`.
///
/// Every source is read and parsed, and the whole program checked, before any file is looked at, so
/// a script that cannot run in full does not run at all.  The search covers `cwd`, or `scope` when
/// one is given, relative to `cwd` or after `:/` to the project root found by walking up from it,
/// and each statement's `in` within that.  Every file is read once, whatever the number of
/// statements; the matches print statement by statement, in program order.  An assertion block is
/// evaluated to the end, and if any of its assertions does not hold, each one that failed is
/// reported and the run stops there.
///
/// # Errors
///
/// Returns a [`Failure`] when the run cannot be answered in full: a script that is malformed or
/// uses what this build does not have, a scope holding a double quote, an `in` that names nothing
/// there, or something that cannot be read.  Nothing is printed for a run that fails, so a partial
/// answer never passes for a whole one.
pub fn run(
    sources: Vec<SourceArg>,
    scope: Option<&OsString>,
    options: &cli::Options,
    cwd: &Path,
    stdin: &mut impl Read,
    out: &mut impl Write,
    notes: &mut impl Write,
) -> Result<Outcome, Failure> {
    run_in(
        languages::LANGUAGES,
        sources,
        scope,
        options,
        cwd,
        stdin,
        out,
        notes,
    )
}

/// [`run`], with `languages` the languages quaff parses, in the order a pattern is compiled for
/// them: the registry's for a run, and others for a test.
#[expect(
    clippy::too_many_arguments,
    reason = "a run's arguments, and the languages it parses"
)]
fn run_in(
    languages: &[&'static dyn FrontEnd],
    sources: Vec<SourceArg>,
    scope: Option<&OsString>,
    options: &cli::Options,
    cwd: &Path,
    stdin: &mut impl Read,
    out: &mut impl Write,
    notes: &mut impl Write,
) -> Result<Outcome, Failure> {
    // One store for the whole run, so a file read before the search or for its rules, and
    // searched as well, is read once.
    let contents = Contents::default();
    let built = sources::read(sources, scope, stdin, &contents)?;
    let mut plan = plan::plan(languages, &built)?;
    let mut files = scoping::scope(&mut plan, &built, cwd, &contents)?;
    let searched = execute::execute(&plan, &mut files)?;
    let evaluated = evaluate::evaluate(&searched, &plan, &built, options, out)?;
    report::notes(notes, &evaluated, &files, &searched, &plan);
    Ok(evaluated.outcome)
}
