// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Evaluation: the steps walked in order over what the search found, deciding the outcome,
//! writing the edits, and printing the results.
//!
//! The fifth phase.  In: what was [`Searched`] for, the [`Plan`] and the [`Built`] program.  Out:
//! the results printed, the edits written, and what was [`Evaluated`], which reporting reads.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use super::Outcome;
use super::{
    execute::{Searched, parses},
    failure::{Broken, Failure, Unwritable},
    plan::{Plan, Step, Wanted},
    report::{count, display, expected, print, quoted},
    sources::Built,
};
use crate::changes;
use crate::cli;
use crate::output;
use crate::write;

/// `--debug-pause-before-write`: creates `marker` and waits until it is removed, so that a test
/// can change a file after it was read and before it is checked and written.  Gives up after a
/// minute, so a test that never removes it fails rather than hangs.
#[cfg(debug_assertions)]
fn pause(marker: &Path) -> Result<(), Failure> {
    let failed = |cause| Failure::Io {
        doing: "pausing before writing, at the marker",
        path: marker.to_path_buf(),
        cause,
    };
    fs::write(marker, b"").map_err(failed)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while fs::exists(marker).map_err(failed)? {
        if std::time::Instant::now() > deadline {
            return Err(failed(io::Error::new(
                io::ErrorKind::TimedOut,
                "still there after a minute",
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

/// What evaluating a run decided.
pub(super) struct Evaluated {
    /// How the run ended.
    pub(super) outcome: Outcome,
    /// Notes and failed assertions, for standard error once the results are out.
    pub(super) notes: Vec<String>,
    /// How many of the run's patterns, in order, it reached: all of them, unless an assertion
    /// failed and stopped it.
    pub(super) reached: usize,
}

/// Decides the run's outcome from what `searched` found for `plan`, planned from `built`, writes
/// its edits - unless `options` make it a dry run - and prints its results to `out`, in that
/// order.
///
/// The outcome is decided before anything is printed or written, so that a replace whose
/// replacement a file cannot hold, an edit that breaks a file's parse, or a file that cannot be
/// written refuses the whole run with nothing written and nothing printed: a run that gives no
/// answer prints nothing.  An assertion that fails is an answer, so its results print, and nothing
/// is written.
///
/// # Errors
///
/// Returns a [`Failure`] for a replace a file's encoding cannot write or that cannot be made as
/// written, an edit that would leave a file unparseable, a file that cannot be written, or results
/// that cannot be printed.
pub(super) fn evaluate(
    searched: &Searched,
    plan: &Plan,
    built: &Built,
    options: &cli::Options,
    out: &mut impl Write,
) -> Result<Evaluated, Failure> {
    let steps = &plan.steps;
    let dry_run = options.dry_run;
    let locations = replace_locations(steps, built);
    #[cfg(test)]
    HELD.with(|held| held.set(searched.held_for_printing()));
    #[cfg(test)]
    CHANGES_HELD.with(|held| {
        held.set(
            searched
                .changed
                .iter()
                .flat_map(|file| searched.held[file.held].file().1)
                .map(crate::changes::Changes::held)
                .sum(),
        );
    });
    let verdict =
        Evaluation::new(searched, false, locations.clone()).steps(steps, built, &mut io::sink())?;
    if verdict == Outcome::Success {
        refuse_broken(searched, &locations)?;
        if !dry_run {
            let edits: Vec<write::Edit> = searched
                .changed
                .iter()
                .map(|file| {
                    let (original, changes) = searched.held[file.held].file();
                    write::Edit {
                        path: &file.path,
                        shown: &file.shown,
                        identity: original.identity(),
                        original,
                        changes,
                    }
                })
                .collect();
            #[cfg(debug_assertions)]
            if let Some(marker) = &options.hooks.pause_before_write {
                pause(marker)?;
            }
            write::commit(&edits).map_err(Failure::Write)?;
        }
    }
    let mut evaluation = Evaluation::new(searched, true, locations);
    let outcome = evaluation.steps(steps, built, out)?;
    if outcome == Outcome::Success && evaluation.replaced + evaluation.deleted > 0 {
        let files = count(evaluation.replaced_in.len(), "file", "files");
        let matches = |n| count(n, "match", "matches");
        let (replaced, deleted) = (evaluation.replaced, evaluation.deleted);
        evaluation
            .notes
            .push(match (replaced > 0, deleted > 0, dry_run) {
                (true, true, true) => format!(
                    "dry run: {} would be replaced and {} deleted in {files}; nothing was written",
                    matches(replaced),
                    matches(deleted)
                ),
                (true, true, false) => format!(
                    "replaced {} and deleted {} in {files}",
                    matches(replaced),
                    matches(deleted)
                ),
                (false, _, true) => format!(
                    "dry run: {} in {files} would be deleted; nothing was written",
                    matches(deleted)
                ),
                (false, _, false) => format!("deleted {} in {files}", matches(deleted)),
                (true, false, true) => format!(
                    "dry run: {} in {files} would be replaced; nothing was written",
                    matches(replaced)
                ),
                (true, false, false) => format!("replaced {} in {files}", matches(replaced)),
            });
    }
    Ok(Evaluated {
        outcome,
        notes: evaluation.notes,
        reached: evaluation.next,
    })
}

#[cfg(test)]
thread_local! {
    /// How many bytes the changes the last run on this thread held, besides the files as read.
    pub(super) static CHANGES_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many bytes the last run on this thread held until it printed, by
    /// [`Searched::held_for_printing`].
    pub(super) static HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Refuses edits that would leave a file in a language quaff parses unparseable that parsed
/// before them.  A file
/// that did not parse before is not refused: the check protects a parse that existed, and a
/// textual edit needs no parse tree.
fn refuse_broken(searched: &Searched, locations: &[Option<String>]) -> Result<(), Failure> {
    let broken: Vec<Broken> = searched
        .changed
        .iter()
        .filter_map(|file| Some((file, file.language?)))
        .filter(|(file, language)| parses(*language, searched.held[file.held].file().0).is_ok())
        .filter_map(|(file, language)| {
            let (original, changes) = searched.held[file.held].file();
            parses(language, &changes::applied(original, changes))
                .err()
                .map(|why| Broken {
                    path: file.shown.clone(),
                    why,
                    by: located(&file.by, locations),
                })
        })
        .collect();
    if broken.is_empty() {
        Ok(())
    } else {
        Err(Failure::BrokenByEdit { files: broken })
    }
}

/// Where each replace, by its pattern's index, was written.
fn located(by: &[usize], locations: &[Option<String>]) -> Vec<String> {
    by.iter()
        .filter_map(|&index| locations.get(index).cloned().flatten())
        .collect()
}

/// For each of the run's patterns, in order, where the replace it belongs to was written, or
/// `None` for one only searched for.
fn replace_locations(steps: &[Step], built: &Built) -> Vec<Option<String>> {
    steps
        .iter()
        .flat_map(|step| match step {
            Step::Replace(replacing) => vec![Some(format!(
                "the {} at {}",
                replacing.verb(),
                built.locate(replacing.span, replacing.span.start)
            ))],
            _ => step.patterns().iter().map(|_| None).collect(),
        })
        .collect()
}

/// Walking the steps in order over what the search found.
struct Evaluation<'s> {
    pub(super) searched: &'s Searched,
    /// Whether the reader of the results is still there.
    pub(super) printing: bool,
    /// The index of the next pattern, in the order `Step::patterns` lists them.
    pub(super) next: usize,
    /// Notes and failed assertions, for standard error once the results are out.
    pub(super) notes: Vec<String>,
    /// How many matches the replaces replaced, how many those with no code deleted, and in which
    /// files.
    pub(super) replaced: usize,
    pub(super) deleted: usize,
    replaced_in: std::collections::BTreeSet<&'s Path>,
    /// For each pattern, where its replace was written, if it is a replace's.
    pub(super) locations: Vec<Option<String>>,
}

impl<'s> Evaluation<'s> {
    /// An evaluation of what `searched` found, printing as it goes or not.
    pub(super) fn new(
        searched: &'s Searched,
        printing: bool,
        locations: Vec<Option<String>>,
    ) -> Self {
        Evaluation {
            searched,
            printing,
            next: 0,
            notes: Vec::new(),
            replaced: 0,
            deleted: 0,
            replaced_in: std::collections::BTreeSet::new(),
            locations,
        }
    }

    pub(super) fn steps(
        &mut self,
        steps: &[Step],
        built: &Built,
        out: &mut impl Write,
    ) -> Result<Outcome, Failure> {
        let mut any = false;
        let mut asserted = false;
        for step in steps {
            match step {
                Step::Find(pattern, _) => {
                    self.refuse_broken_for(1)?;
                    let index = self.next;
                    let found = self.print_matches(*pattern, out)?;
                    if found == 0 {
                        self.notes
                            .push(match self.nothing_applicable(index, *pattern) {
                                Some(why) => {
                                    format!("{why}, so {} was not searched for", quoted(*pattern))
                                }
                                None => format!("no matches for {}", quoted(*pattern)),
                            });
                    }
                    any |= found > 0;
                }
                Step::Assert(block) => {
                    asserted = true;
                    self.refuse_broken_for(block.len())?;
                    let mut failed = Vec::new();
                    // An assertion is decided by hits alone.  Possible matches, and files not
                    // searched for its pattern, are noted but never decide it, so that a file
                    // quaff cannot read can never block an answer about the files it can.
                    for assertion in block {
                        let index = self.next;
                        let found = self.print_matches(assertion.pattern, out)?;
                        let location = built.locate(assertion.span, assertion.span.start);
                        // An assertion about the structure of files that have none is not a
                        // passed one, whatever it asks.
                        if let Some(why) = self.nothing_applicable(index, assertion.pattern) {
                            failed.push(format!(
                                "{location}: expected {} of {}, which cannot be checked: {why}",
                                expected(assertion.count),
                                quoted(assertion.pattern)
                            ));
                        } else if !assertion.count.holds(found) {
                            failed.push(format!(
                                "{location}: expected {} of {}, found {found}",
                                expected(assertion.count),
                                quoted(assertion.pattern)
                            ));
                        }
                    }
                    if !failed.is_empty() {
                        if self.replaced + self.deleted > 0 {
                            failed.push(
                                "nothing was written: the edits before the failed assertion are \
                                 made only if every assertion holds"
                                    .to_owned(),
                            );
                        }
                        // Before the other notes: they are about what was looked at, these
                        // are the answer.
                        failed.append(&mut self.notes);
                        self.notes = failed;
                        return Ok(Outcome::AssertionFailed);
                    }
                }
                Step::Replace(replacing) => {
                    asserted = true;
                    self.refuse_broken_for(1)?;
                    let index = self.next;
                    let pattern = replacing.wanted();
                    let found = self.print_matches(pattern, out)?;
                    // As for an assertion: a count over files that have no structure to search
                    // is not a held one, whatever it asks.
                    let unsearched = self.nothing_applicable(index, pattern);
                    if unsearched.is_some() || !replacing.count.holds(found) {
                        let location = built.locate(replacing.span, replacing.span.start);
                        let what = match unsearched {
                            Some(why) => format!("which cannot be checked: {why}"),
                            None => format!("found {found}"),
                        };
                        let mut failed = vec![format!(
                            "{location}: expected {} of {} to {}, {what}; nothing was \
                             written",
                            expected(replacing.count),
                            quoted(pattern),
                            replacing.verb()
                        )];
                        failed.append(&mut self.notes);
                        self.notes = failed;
                        return Ok(Outcome::AssertionFailed);
                    }
                    self.refuse_edits_of(index, replacing.verb())?;
                    let found = usize::try_from(found).unwrap_or(usize::MAX);
                    if replacing.deletes {
                        self.deleted += found;
                    } else {
                        self.replaced += found;
                    }
                    self.replaced_in.extend(
                        self.searched.matches[index]
                            .iter()
                            .map(|(path, _)| path.as_path()),
                    );
                }
            }
        }
        Ok(if asserted || any {
            Outcome::Success
        } else {
            Outcome::NothingFound
        })
    }

    /// Refuses the run if the replace at `index`, whose count holds, cannot be made: a file's
    /// encoding cannot write its replacement, or a structural replace cannot be made as written.
    fn refuse_edits_of(&self, index: usize, verb: &'static str) -> Result<(), Failure> {
        let unwritable = &self.searched.unwritable[index];
        if !unwritable.is_empty() {
            return Err(Failure::Unwritable {
                files: unwritable
                    .iter()
                    .map(|(path, encoding, text, ambiguous)| Unwritable {
                        path: path.clone(),
                        encoding,
                        text: text.clone(),
                        ambiguous: *ambiguous,
                    })
                    .collect(),
            });
        }
        let refused = &self.searched.edits_refused[index];
        if !refused.is_empty() {
            return Err(Failure::EditRefused {
                verb,
                files: refused.clone(),
            });
        }
        Ok(())
    }

    /// Refuses the run if the next `patterns` patterns need a file an earlier replace
    /// left unparseable: the edit broke it, so they cannot be searched for in it.
    fn refuse_broken_for(&self, patterns: usize) -> Result<(), Failure> {
        let mut files: Vec<Broken> = Vec::new();
        for broken in &self.searched.broken[self.next..self.next + patterns] {
            for (path, why, by) in broken {
                let file = Broken {
                    path: path.clone(),
                    why: why.clone(),
                    by: located(by, &self.locations),
                };
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        if files.is_empty() {
            Ok(())
        } else {
            Err(Failure::BrokenByEdit { files })
        }
    }

    /// Prints the next pattern's matches, `pattern`, and returns how many there were.
    fn print_matches(&mut self, pattern: Wanted, out: &mut impl Write) -> Result<u64, Failure> {
        // A textual match is the pattern's own text; a structural one is the code it covers.
        let text = match pattern {
            Wanted::Text(pattern) => display(pattern),
            Wanted::Structural { .. } => String::new(),
        };
        let mut count = 0;
        // The text a file's matches are being printed from, built once for all of them.
        let mut decoded: Option<((usize, usize), Cow<str>)> = None;
        for (path, in_file) in &self.searched.matches[self.next] {
            for hit in in_file {
                count += 1;
                if !self.printing {
                    continue;
                }
                let shown = match &hit.code {
                    None => text.as_str(),
                    Some(code) => {
                        let wanted = (code.held, code.version);
                        if decoded.as_ref().is_none_or(|(at, _)| *at != wanted) {
                            decoded =
                                Some((wanted, self.searched.held[code.held].text(code.version)));
                        }
                        let (_, version) = decoded.as_ref().expect("decoded just now");
                        &version[code.start..code.end]
                    }
                };
                self.printing = print(out, &output::line(path, &hit.at, shown))?;
            }
        }
        self.next += 1;
        Ok(count)
    }

    /// Why `pattern`, the run's pattern at `index`, could not be looked for anywhere, if that is
    /// so: a structural pattern whose scope holds no file in a language that reads it, or none
    /// that could be searched.
    fn nothing_applicable(&self, index: usize, pattern: Wanted) -> Option<String> {
        let searched = self.searched;
        match pattern {
            // With nothing in scope at all, as where a `where` leaves out every file, there are no
            // text-tier files to count, and "0 not searched" would say nothing.
            Wanted::Structural { .. } if searched.supported_for[index] == 0 => Some(
                match (
                    searched.unread_for[index],
                    searched.not_applicable_for[index],
                ) {
                    (0, 0) => "no file in scope has structural support".to_string(),
                    (0, n) => format!(
                        "no file in scope has structural support; {} not searched",
                        count(n, "text-tier file", "text-tier files")
                    ),
                    (unread, 0) => format!(
                        "no file in scope is in a language that reads it; {} not searched",
                        count(unread, "file", "files")
                    ),
                    (unread, n) => format!(
                        "no file in scope is in a language that reads it; {} and {} not \
                             searched",
                        count(
                            unread,
                            "file in another language",
                            "files in another language"
                        ),
                        count(n, "text-tier file", "text-tier files")
                    ),
                },
            ),
            // Every file in scope whose language reads it is binary, which the binary count says.
            Wanted::Structural { .. } if searched.structural_for[index] == 0 => Some(format!(
                "no file in scope with structural support could be searched; {} not searched",
                count(searched.supported_for[index], "such file", "such files")
            )),
            _ => None,
        }
    }
}
