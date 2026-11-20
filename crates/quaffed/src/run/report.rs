// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Reporting: how a run words what it found and what it did not look at.
//!
//! The last phase.  In: what was [`Evaluated`], what was [`Searched`] for, the [`Files`] and the
//! [`Plan`].  Out: the run's notes, written to standard error once its results are out.

use std::io::{self, Write};
use std::path::PathBuf;

use quaffed_encoding::Encoding;
use quaffed_representation::program::{Count, TextPart, TextPattern};

use super::{
    evaluate::Evaluated,
    execute::{Searched, Unspelled},
    failure::Failure,
    plan::{Plan, Searching, Wanted},
    scoping::Files,
};
use crate::discover;
use crate::resolve::Unknown;

/// Writes the run's notes to `notes`: what `evaluated` noted, then what scoping `files` noted,
/// then everything that was not looked at, or was looked at without certainty, for the patterns
/// of `plan` the run reached.
pub(super) fn notes(
    notes: &mut impl Write,
    evaluated: &Evaluated,
    files: &Files,
    searched: &Searched,
    plan: &Plan,
) {
    // Notes are best effort: a run that cannot write to standard error has nowhere to say so.
    let mut note = |text: &str| {
        let _ = writeln!(notes, "quaff: {text}");
    };
    for text in &evaluated.notes {
        note(text);
    }
    for text in &files.notes {
        note(text);
    }
    let declared_skipped: Vec<String> = files
        .projects
        .list
        .iter()
        .filter_map(|p| p.declarations.workspace.as_ref())
        .flat_map(|w| w.skipped().iter().cloned())
        .collect();
    for text in skipped(
        searched,
        &plan.searching[..evaluated.reached],
        &files.found,
        &declared_skipped,
    ) {
        note(&text);
    }
}

/// The text a pattern matches, for display: each line break as LF, which `output::fold`
/// shows as a space, as it would the file's own line ending.
pub(super) fn display(pattern: &TextPattern) -> String {
    pattern
        .parts()
        .iter()
        .map(|part| match part {
            TextPart::Literal(text) => text.as_str(),
            _ => "\n",
        })
        .collect()
}

/// `pattern` as a message names it, in the delimiters a script writes it in: a textual one
/// quoted, with any anchors outside the quotes, so an anchored pattern is not mistaken for the
/// bare text; a structural one in backticks.
pub(super) fn quoted(pattern: Wanted) -> String {
    match pattern {
        Wanted::Text(pattern) => format!(
            "{}{:?}{}",
            if pattern.start_anchor { "^" } else { "" },
            display(pattern),
            if pattern.end_anchor { "$" } else { "" }
        ),
        // A line break as `\n`, as for text, so that a note stays on one line.
        Wanted::Structural { written, .. } => {
            format!("`{}`", written.replace('\r', "\\r").replace('\n', "\\n"))
        }
    }
}

/// What `count` requires, worded for a failed assertion.
pub(super) fn expected(count: Count) -> String {
    let matches = |n: u64| if n == 1 { "match" } else { "matches" };
    match count {
        Count::Exactly(n) => format!("exactly {n} {}", matches(n)),
        Count::AtLeast(n) => format!("at least {n} {}", matches(n)),
        Count::AtMost(n) => format!("at most {n} {}", matches(n)),
        Count::ExactlyOrNone(n) => format!("exactly {n} {} or none", matches(n)),
        Count::None => "no matches".into(),
        Count::Any => "any number of matches".into(),
        _ => format!("{count:?}"),
    }
}

/// A note for everything not looked at, or looked at without certainty, so that nothing is
/// silently left out: per pattern evaluated, the files it was not searched in and its possible
/// matches; then the files their language's reader refuses, the files that read cleanly in the
/// locale's encoding, and what discovery, classification and the project's declarations passed
/// over.
pub(super) fn skipped(
    searched: &Searched,
    evaluated: &[Searching],
    found: &discover::Found,
    declared_skipped: &[String],
) -> Vec<String> {
    let mut notes = structural_notes(searched, evaluated);
    for (i, Searching { pattern, .. }) in evaluated.iter().enumerate() {
        let pattern = quoted(*pattern);
        let unknown = searched.unspelled[i]
            .iter()
            .filter(|(_, why)| {
                matches!(
                    why,
                    Unspelled::Unknown(Unknown::NotUtf8 | Unknown::Iso2022Switch)
                )
            })
            .count();
        if unknown > 0 {
            notes.push(format!(
                "{} of unknown encoding not searched for {pattern}, which is not ASCII and so \
                 has no one spelling there",
                count(unknown, "file", "files"),
            ));
        }
        for (path, why) in &searched.unspelled[i] {
            match why {
                Unspelled::Ambiguous(encoding) => notes.push(format!(
                    "{path:?} not searched for {pattern}: it is in {:?}, which spells a \
                     character of it more than one way",
                    name_of(*encoding)
                )),
                Unspelled::Unknown(reason @ Unknown::Unreadable { .. }) => notes.push(format!(
                    "{path:?} not searched for {pattern}{}: {reason}",
                    if reason.ascii_compatible() {
                        ", which is not ASCII"
                    } else {
                        ""
                    }
                )),
                Unspelled::Unknown(_) => {}
            }
        }
        for (path, lines, reason) in &searched.possible[i] {
            notes.push(format!(
                "{path:?}: possible {} of {pattern} on {}, not counted, since its encoding is \
                 unknown: {reason}",
                if lines.len() == 1 { "match" } else { "matches" },
                lines_of(lines)
            ));
        }
    }
    for (path, refusal) in &searched.refused {
        notes.push(format!("{path:?} not searched: {refusal}"));
    }
    if let (Some(name), false) = (&searched.hint, searched.hinted.is_empty()) {
        let paths: Vec<String> = searched.hinted.iter().map(|p| format!("{p:?}")).collect();
        let places = match searched.declares_in {
            Some(system) => format!("a coding line, {system} or .editorconfig"),
            None => "a coding line or .editorconfig".to_owned(),
        };
        notes.push(format!(
            "{} of unknown encoding {} cleanly in {name:?}, the encoding of your locale: {}.  If \
             that is the encoding, declare it - with {places} - for quaff to read {}",
            count(paths.len(), "file", "files"),
            if paths.len() == 1 { "reads" } else { "read" },
            paths.join(", "),
            if paths.len() == 1 { "it" } else { "them" }
        ));
    }
    if searched.binary > 0 {
        notes.push(format!(
            "{} not searched",
            count(searched.binary, "binary file", "binary files")
        ));
    }
    for rule in declared_skipped {
        notes.push(format!("attributes file {rule}; that line is skipped"));
    }
    for rule in &found.skipped_rules {
        notes.push(format!(
            "ignore file {rule}; that line is skipped and the rest of the file applies"
        ));
    }
    if !found.special_files.is_empty() {
        notes.push(format!(
            "{} - FIFOs, sockets or devices - not searched",
            count(found.special_files.len(), "special file", "special files")
        ));
    }
    if !found.links_not_followed.is_empty() {
        notes.push(format!(
            "{} not followed",
            count(
                found.links_not_followed.len(),
                "symbolic link",
                "symbolic links"
            )
        ));
    }
    notes
}

/// The notes for structural patterns, once for the run however many there are, where some file
/// in scope has structural support for one - with none, the search or the assertion already
/// counted them: the text-tier files they do not apply to, and the files whose language cannot
/// read one of them.
fn structural_notes(searched: &Searched, evaluated: &[Searching]) -> Vec<String> {
    let mut notes = Vec::new();
    if !evaluated
        .iter()
        .any(|s| matches!(s.pattern, Wanted::Structural { .. }))
    {
        return notes;
    }
    for (path, why) in searched.questioned[..evaluated.len()].iter().flatten() {
        notes.push(format!("{path:?}: {why}"));
    }
    // With nothing in scope with structural support, the run's own message counts these instead.
    if searched.supported > 0 && searched.not_applicable > 0 {
        notes.push(format!(
            "{} not searched for structural patterns, which apply only to files with \
             structural support",
            count(searched.not_applicable, "text-tier file", "text-tier files")
        ));
    }
    if searched.supported > 0 && searched.unread > 0 {
        notes.push(match searched.unread {
            1 => "1 file not searched for a structural pattern its language cannot read".to_owned(),
            n => {
                format!("{n} files not searched for structural patterns their language cannot read")
            }
        });
    }
    notes
}

/// An encoding's name, as `CPython` calls the codec.
pub(super) fn name_of(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::SingleByte(codec) => codec.name(),
        Encoding::MultiByte(codec) => codec.name(),
        Encoding::Utf8 => "utf_8",
        Encoding::Utf16Le => "utf_16_le",
        Encoding::Utf16Be => "utf_16_be",
        Encoding::Utf32Le => "utf_32_le",
        Encoding::Utf32Be => "utf_32_be",
        Encoding::Unknown8Bit => "unknown",
    }
}

/// Line numbers as a message gives them: `line 3`, or `lines 3, 7, 12`.
pub(super) fn lines_of(lines: &[usize]) -> String {
    let numbers: Vec<String> = lines.iter().map(ToString::to_string).collect();
    format!(
        "{} {}",
        if lines.len() == 1 { "line" } else { "lines" },
        numbers.join(", ")
    )
}

/// Writes one line of results.  Returns `false` if the reader has gone, which stops the
/// printing quietly, as it does for any tool whose output is piped into `head`; assertions are
/// still evaluated, so the exit code is still the answer.
pub(super) fn print(out: &mut impl Write, line: &str) -> Result<bool, Failure> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(cause) => Err(Failure::Io {
            doing: "writing the results to",
            path: PathBuf::from("standard output"),
            cause,
        }),
    }
}

pub(super) fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
