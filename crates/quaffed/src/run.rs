// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Carrying out a run: the program its sources make, the scope, the files in it, the matches,
//! the assertions, and what was not looked at.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use quaffed_encoding::{self as encoding, Encoding, Named};
use quaffed_representation::check;
use quaffed_representation::glob::Glob;
use quaffed_representation::program::{
    Action, ActionKind, Count, Counted, Expectation, Filter, Operand, Program, Query, Statement,
    Subject, TextPart, TextPattern,
};
use quaffed_representation::span::{Source, Span, Spanned};
use quaffed_script::{ErrorKind, lexer, location, parse_source};
use quaffed_syntax::{CompileError, FrontEnd, Pattern, Refused, Rewrite};

use crate::changes::{self, Changes};
use crate::cli::{self, SourceArg, UsageError};
use crate::contents::Contents;
use crate::discover;
use crate::editorconfig::EditorConfig;
use crate::filter;
use crate::languages;
use crate::output;
use crate::resolve::{self, Declarations, Resolution, Unknown};
use crate::rewrite;
use crate::scope;
use crate::search::{self, Match};
use crate::structural;
use crate::workspace::{self, Workspace};
use crate::write;

/// Why a run could not give an answer.  Each kind maps to one exit code, because each asks the
/// caller for a different action.
#[derive(Debug)]
pub enum Failure {
    /// The command line cannot be acted on: fix it.  Exit as [`UsageError::exit_code`] says.
    Usage(UsageError),
    /// A script that must be fixed: malformed, or breaking a rule every program keeps.  Exit 3.
    Script {
        /// Where, as `location::describe` names it.
        location: String,
        /// What is wrong.
        message: String,
    },
    /// Sources that hold no statements at all - only blank lines and comments - so there is
    /// nothing to run.  Usually a script that was not what was meant, such as `-e "$CHECKS"`
    /// with the variable unset.  Exit 3.
    Empty {
        /// Each source, as a diagnostic names it.
        sources: Vec<String>,
    },
    /// A statement or construct this build does not have: outside the MVP, or in it and not
    /// built yet.  Exit 5.
    Unknown {
        /// Where, as `location::describe` names it.
        location: String,
        /// What it is.
        message: String,
    },
    /// The command-line scope holds a double quote: it is a plain path, and a pattern is
    /// written with `in`.  Refused rather than read as a path, so that an expression there later
    /// only adds to what a scope may be.  Exit 3.
    QuotedScope {
        /// The scope as given.
        scope: OsString,
    },
    /// An `in` names a path that is not there: asking for what is not there is a mistake, and
    /// whatever ran instead would not be what was meant.  Exit 7, as for a missing scope.
    NotThere {
        /// Where the `in` was written, as `location::describe` names it.
        location: String,
        /// What it names, and how to write it if it reads as a glob.
        message: String,
    },
    /// Something could not be read.  Exit 7.
    Io {
        /// What was being done, in the user's terms.
        doing: &'static str,
        /// The path, as the user would recognise it.
        path: PathBuf,
        /// The system's error, verbatim.
        cause: io::Error,
    },
    /// The files to search could not all be found.  Exit 7.
    Discovery(discover::Error),
    /// A structural search needs files quaff cannot parse - ones that do not parse in their
    /// language, and ones in an encoding their language reads and quaff does not - so the run is
    /// refused: a count that left them out could not be trusted.  Exit 9.
    Unparseable {
        /// Each such file, as the user would recognise it, and why it cannot be parsed.
        files: Vec<(PathBuf, String)>,
    },
    /// A replace matched in files whose encoding cannot write its replacement, so the run is
    /// refused and nothing written.  Exit 9: the script or the file must change first.
    Unwritable {
        /// Each such file, and what it cannot write.
        files: Vec<Unwritable>,
    },
    /// The edits would leave files that parsed in their language unparseable, so the run is
    /// refused and nothing written.  Exit 9.
    BrokenByEdit {
        /// Each such file.
        files: Vec<Broken>,
    },
    /// A structural replace cannot be made as written in some files - its matches overlap, it
    /// would drop a comment, or it would change a literal's structure - so the run is refused
    /// and nothing written.  Exit 9.
    EditRefused {
        /// The statement, as a message names it: `replace` or `delete`.
        verb: &'static str,
        /// Each such file, as the user would recognise it, with where and why.
        files: Vec<(PathBuf, String)>,
    },
    /// The edits could not all be written.  Exit 7.
    Write(write::Failure),
}

/// A file an edit would leave unparseable in its language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    /// The file, as the user would recognise it.
    path: PathBuf,
    /// Where and why it would not parse.
    why: String,
    /// Each statement that changed it, and where it was written: "the replace at" a place.
    by: Vec<String>,
}

/// A file whose encoding cannot write a replacement.
#[derive(Debug)]
pub struct Unwritable {
    /// The file, as the user would recognise it.
    path: PathBuf,
    /// Its encoding, as `CPython` names the codec.
    encoding: &'static str,
    /// The text of the replacement it cannot write.
    text: String,
    /// Whether the encoding spells the text more than one way, rather than not at all.
    ambiguous: bool,
}

impl Failure {
    /// The exit code this failure ends the run with.
    pub fn exit_code(&self) -> u8 {
        match self {
            Failure::Usage(err) => err.exit_code(),
            Failure::Script { .. } | Failure::Empty { .. } | Failure::QuotedScope { .. } => 3,
            Failure::Unknown { .. } => 5,
            Failure::NotThere { .. } | Failure::Io { .. } | Failure::Discovery(_) => 7,
            Failure::Write(err) => err.exit_code(),
            Failure::Unparseable { .. }
            | Failure::Unwritable { .. }
            | Failure::BrokenByEdit { .. }
            | Failure::EditRefused { .. } => 9,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Usage(err) => err.fmt(f),
            Failure::Script { location, message }
            | Failure::Unknown { location, message }
            | Failure::NotThere { location, message } => write!(f, "{location}: {message}"),
            Failure::Empty { sources } => write!(
                f,
                "nothing to run: {} {} no statements, only blank lines and comments",
                sources.join(", "),
                if sources.len() == 1 { "has" } else { "have" }
            ),
            Failure::QuotedScope { scope } => write!(
                f,
                "the scope {scope:?} holds a double quote: the scope on the command line is a \
                 plain path.  To scope by a pattern, write it with `in` in the script, as -e \
                 'find \"x\" in \"src/\"**'"
            ),
            Failure::Io { doing, path, cause } => {
                write!(f, "{doing} {path:?}: {cause}")?;
                let looks_like_a_glob = path.to_string_lossy().contains(['*', '?', '[']);
                if cause.kind() == io::ErrorKind::NotFound && looks_like_a_glob {
                    write!(
                        f,
                        ".  quaff does not expand a glob given as the scope; name one file or \
                         directory, or write the pattern with `in` in the script"
                    )?;
                }
                Ok(())
            }
            Failure::Discovery(err) => err.fmt(f),
            Failure::Unparseable { files } => {
                write!(
                    f,
                    "refused: {} in scope cannot be parsed, so a structural search cannot count \
                     matches in {}:",
                    count(files.len(), "file", "files"),
                    if files.len() == 1 { "it" } else { "them" }
                )?;
                for (path, why) in files {
                    write!(f, "\n  {path:?}: {why}")?;
                }
                let them = if files.len() == 1 { "it" } else { "them" };
                write!(
                    f,
                    "\nTo search without {them}, leave {them} out of the statement: {}, or \
                     project-wide by listing {them} in .ignore",
                    leave_out(files.first().map(|(path, _)| path.as_path()))
                )
            }
            Failure::Unwritable { files } => unwritable(f, files),
            Failure::BrokenByEdit { files } => broken(f, files),
            Failure::EditRefused { verb, files } => {
                write!(
                    f,
                    "refused: the {verb} cannot be made as written in {}, so nothing was written:",
                    count(files.len(), "file", "files")
                )?;
                for (path, why) in files {
                    write!(f, "\n  {path:?}: {why}")?;
                }
                Ok(())
            }
            Failure::Write(err) => err.fmt(f),
        }
    }
}

/// The refusal of a replacement files' encodings cannot write.
fn unwritable(f: &mut fmt::Formatter<'_>, files: &[Unwritable]) -> fmt::Result {
    write!(
        f,
        "refused: the replacement cannot be written in the encoding of {}, so nothing \
             was written:",
        count(files.len(), "file", "files")
    )?;
    for file in files {
        if file.ambiguous {
            write!(
                f,
                "\n  {:?}: {} spells {:?} more than one way",
                file.path, file.encoding, file.text
            )?;
        } else {
            write!(
                f,
                "\n  {:?}: {} cannot hold {:?}",
                file.path, file.encoding, file.text
            )?;
        }
    }
    write!(
        f,
        "\nUse a replacement {} can hold, or leave {} out of the statement: {}, or with a \
         narrower scope on the command line",
        if files.len() == 1 {
            "its encoding"
        } else {
            "their encodings"
        },
        if files.len() == 1 { "it" } else { "them" },
        leave_out(files.first().map(|file| file.path.as_path()))
    )
}

/// The ways a statement can leave out files it refuses, as a refusal offers them: `in`, and
/// `where not FILE matches` with the first of them, `first`, as the example.
fn leave_out(first: Option<&Path>) -> String {
    // A path as printed is one `FILE` names, so the clause can be copied as it is.
    let example = first.map_or_else(String::new, |path| {
        format!(
            ", such as where not FILE matches {}",
            crate::scope::quote(&path.to_string_lossy())
        )
    });
    format!("with `in`, with `where not FILE matches`{example}")
}

/// The refusal of edits that would leave files unparseable in their language.
fn broken(f: &mut fmt::Formatter<'_>, files: &[Broken]) -> fmt::Result {
    write!(
        f,
        "refused: the edit would leave {} that {} unparseable, so nothing was written:",
        count(files.len(), "file", "files"),
        if files.len() == 1 { "parses" } else { "parse" }
    )?;
    for file in files {
        write!(f, "\n  {:?}: {}", file.path, file.why)?;
        if !file.by.is_empty() {
            write!(f, "; changed by {}", file.by.join(" and by "))?;
        }
    }
    // Each `by` names its statement, "the delete at" or "the replace at".
    let deleting = files
        .iter()
        .flat_map(|file| &file.by)
        .all(|by| by.starts_with("the delete "));
    write!(
        f,
        "\nChange the {} so that {} still {}, or leave {} out of the statement: {}, \
         or with a narrower scope on the command line",
        if deleting { "deletion" } else { "replacement" },
        if files.len() == 1 {
            "the file"
        } else {
            "the files"
        },
        if files.len() == 1 { "parses" } else { "parse" },
        if files.len() == 1 { "it" } else { "them" },
        leave_out(files.first().map(|file| file.path.as_path()))
    )
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Io { cause, .. } => Some(cause),
            Failure::Discovery(err) => Some(err),
            Failure::Write(err) => Some(err),
            Failure::Usage(_)
            | Failure::Script { .. }
            | Failure::Empty { .. }
            | Failure::Unknown { .. }
            | Failure::QuotedScope { .. }
            | Failure::NotThere { .. }
            | Failure::Unparseable { .. }
            | Failure::Unwritable { .. }
            | Failure::BrokenByEdit { .. }
            | Failure::EditRefused { .. } => None,
        }
    }
}

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
    let mut built = build(sources, stdin, &contents)?;
    built.program.narrow_to = scope.map(PathBuf::from);
    if built.program.body().is_empty() {
        return Err(Failure::Empty {
            sources: built.program.sources.iter().map(name).collect(),
        });
    }
    check::check(&built.program).map_err(|violation| {
        let location = built.locate(violation.span, violation.span.start);
        let message = violation.to_string();
        if violation.is_unknown_statement() {
            Failure::Unknown { location, message }
        } else {
            Failure::Script { location, message }
        }
    })?;
    let (mut steps, structural, planned_rewrites) =
        plan(&built.program).map_err(|(construct, span)| Failure::Unknown {
            location: built.locate(span, span.start),
            message: format!("{construct} is not built yet"),
        })?;
    let mut compiled = compile(languages, &structural, &planned_rewrites, &built)?;
    for step in &mut steps {
        if let Step::Replace(replacing) = step
            && let ReplaceKind::Structural { rewrite, .. } = replacing.kind
        {
            // A rewrite that writes a comment replaces the code with it; only one that writes
            // nothing deletes.
            replacing.deletes = planned_rewrites[rewrite].0.trim().is_empty();
        }
    }

    let cwd = fs::canonicalize(cwd).map_err(|cause| Failure::Io {
        doing: "reading the current directory",
        path: cwd.to_path_buf(),
        cause,
    })?;
    let mut projects = Projects::new(&cwd, &contents);
    let root = projects.list[0].boundary.clone();
    let roots = scope::Roots {
        cwd: &cwd,
        root: &root,
    };
    let narrow = narrowing(built.program.narrow_to.as_deref(), roots)?;

    let searching: Vec<Searching> = steps.iter().flat_map(Step::patterns).collect();
    let patterns: Vec<Wanted> = searching.iter().map(|s| s.pattern).collect();
    let replacements: Vec<Option<Replacement>> =
        steps.iter().flat_map(Step::replacements).collect();
    // Every `where` is compiled and checked, as every pattern is, before discovery looks at a
    // file, so that a script error is reported as one however large or unreadable the tree.
    filters(&searching, &mut compiled, roots, &built)?;
    rewrites_read(&compiled, &planned_rewrites, &built)?;
    let mut scoping = Scoping::new(narrow.as_deref(), &cwd);
    let scoped: Vec<Scoped> = searching
        .iter()
        .map(|s| scoping.scoped(s.query, roots, &built))
        .collect::<Result<_, _>>()?;
    let (files, found) = discover_bases(&scoping.bases, &mut projects, &cwd)?;
    let to_search = to_search(files, &scoped, roots);
    let locale = locale();
    let searched = search_files(
        &to_search,
        roots,
        &patterns,
        &replacements,
        &compiled,
        &mut projects,
        locale.as_ref(),
    )?;
    // A structural search over files it cannot parse would give a count that cannot be trusted,
    // whether their language could parse them or not, so the whole run is refused, naming each,
    // before anything is printed.
    if !searched.unparsed.is_empty() {
        return Err(Failure::Unparseable {
            files: searched.unparsed.clone(),
        });
    }
    let (outcome, evaluation) = conclude(&searched, &steps, &built, options, out)?;

    // Notes are best effort: a run that cannot write to standard error has nowhere to say so.
    let mut note = |text: &str| {
        let _ = writeln!(notes, "quaff: {text}");
    };
    for text in &evaluation.notes {
        note(text);
    }
    for text in &scoping.notes {
        note(text);
    }
    let declared_skipped: Vec<String> = projects
        .list
        .iter()
        .filter_map(|p| p.declarations.workspace.as_ref())
        .flat_map(|w| w.skipped().iter().cloned())
        .collect();
    for text in skipped(
        &searched,
        &searching[..evaluation.next],
        &found,
        &declared_skipped,
    ) {
        note(&text);
    }
    Ok(outcome)
}

/// The run's structural patterns, their rewrites and their `where`s, compiled for one language,
/// each where the language reads it: a file is searched with its own language's.
struct Compiled {
    /// The language.
    language: &'static dyn FrontEnd,
    /// Each structural pattern, by its place among them, if the language reads it and its
    /// `where`.
    patterns: Vec<Option<Pattern>>,
    /// Each rewrite template, by its place among them, where the language reads its pattern:
    /// compiled, or why the language cannot read it, which refuses the edit in a file of the
    /// language where the pattern matches.
    rewrites: Vec<Option<Result<Rewrite, CompileError>>>,
    /// Each of the run's patterns' `where`, by the pattern's index among all of them, where it
    /// tests a metavariable and the language reads the pattern.
    filters: Vec<Option<filter::Filter>>,
}

/// A script error for text no language could compile, at the place of the first language's
/// error: what it said, or with more than one language, what each said, named.
fn unread(errors: &[(&dyn FrontEnd, CompileError)], span: Span, built: &Built) -> Failure {
    let (_, first) = &errors[0];
    let message = match errors {
        [(_, only)] => only.to_string(),
        _ => errors
            .iter()
            .map(|(language, err)| format!("as {}: {err}", language.language()))
            .collect::<Vec<_>>()
            .join("; "),
    };
    Failure::Script {
        location: built.locate(
            span,
            first
                .offset()
                .map_or(span.start, |at| built.content_start(span) + at),
        ),
        message,
    }
}

/// Each rewrite, checked against the languages that read its pattern: one that none of them reads
/// stops the run, with what each said.  Checked when the rewrites are compiled, and again once
/// the `where`s have left a pattern unread in the languages that cannot read them.
fn rewrites_read(
    compiled: &[Compiled],
    planned_rewrites: &[PlannedRewrite],
    built: &Built,
) -> Result<(), Failure> {
    for (index, (_, span, compiled_at)) in planned_rewrites.iter().enumerate() {
        let reading: Vec<&Compiled> = compiled
            .iter()
            .filter(|c| c.patterns[*compiled_at].is_some())
            .collect();
        let errors: Vec<(&dyn FrontEnd, CompileError)> = reading
            .iter()
            .filter_map(|c| match &c.rewrites[index] {
                Some(Err(err)) => Some((c.language, err.clone())),
                _ => None,
            })
            .collect();
        if !reading.is_empty() && errors.len() == reading.len() {
            return Err(unread(&errors, *span, built));
        }
    }
    Ok(())
}

/// Every structural pattern compiled for each of `languages`, and every rewrite template against
/// the pattern whose captures it writes back, before any file is read.  A pattern no language
/// reads stops the run before it looks at anything, as does a rewrite no language that reads its
/// pattern reads.
fn compile(
    languages: &[&'static dyn FrontEnd],
    structural: &[(&str, Span)],
    planned_rewrites: &[PlannedRewrite],
    built: &Built,
) -> Result<Vec<Compiled>, Failure> {
    let mut compiled: Vec<Compiled> = languages
        .iter()
        .map(|&language| Compiled {
            language,
            patterns: Vec::with_capacity(structural.len()),
            rewrites: Vec::with_capacity(planned_rewrites.len()),
            filters: Vec::new(),
        })
        .collect();
    for (written, span) in structural {
        let mut errors = Vec::new();
        for for_language in &mut compiled {
            let language = for_language.language;
            match language.compile_pattern(written) {
                Ok(pattern) => for_language.patterns.push(Some(pattern)),
                Err(err) => {
                    for_language.patterns.push(None);
                    errors.push((language, err));
                }
            }
        }
        if errors.len() == languages.len() {
            return Err(unread(&errors, *span, built));
        }
    }
    for (written, _, compiled_at) in planned_rewrites {
        for for_language in &mut compiled {
            let language = for_language.language;
            let rewrite = for_language.patterns[*compiled_at]
                .as_ref()
                .map(|pattern| language.compile_rewrite(written, pattern));
            for_language.rewrites.push(rewrite);
        }
    }
    rewrites_read(&compiled, planned_rewrites, built)?;
    // A replace's unnamed holes are named once its rewrite is compiled, which reports `$_` in a
    // rewrite as the unnamed hole it is, so that each match says where they matched too.
    for for_language in &mut compiled {
        for (_, _, compiled_at) in planned_rewrites {
            if let Some(pattern) = &mut for_language.patterns[*compiled_at] {
                *pattern = rewrite::name_every_hole(pattern);
            }
        }
    }
    Ok(compiled)
}

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

/// Decides the run's outcome from what the search found, writes its edits - unless it is a dry
/// run - and prints its results, in that order.
///
/// The outcome is decided before anything is printed or written, so that a replace whose
/// replacement a file cannot hold, an edit that breaks a file's parse, or a file that cannot be
/// written refuses the whole run with nothing written and nothing printed: a run that gives no
/// answer prints nothing.  An assertion that fails is an answer, so its results print, and nothing
/// is written.
fn conclude<'s>(
    searched: &'s Searched,
    steps: &[Step],
    built: &Built,
    options: &cli::Options,
    out: &mut impl Write,
) -> Result<(Outcome, Evaluation<'s>), Failure> {
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
                .map(Changes::held)
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
    Ok((outcome, evaluation))
}

#[cfg(test)]
thread_local! {
    /// How many bytes the changes the last run on this thread held, besides the files as read.
    static CHANGES_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many bytes the last run on this thread held until it printed, by
    /// [`Searched::held_for_printing`].
    static HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

/// Stretches an edit wrote over, each where it was in the bytes before and where what was written
/// stands in those after, as [`Changes::between`] takes them.
type Over = Vec<(Range<usize>, Range<usize>)>;

/// `over`, stretches a structural rewrite gave in decoded text - from `source`'s text to `edited` -
/// as the same stretches in the file's bytes before and after.  Every encoding quaff decodes a
/// file in spells each character on its own, so a stretch's bytes are its characters' spellings,
/// after the byte-order mark if there is one.  A stretch that does not fall on characters in
/// either text is not one a rewrite gives; then nothing is claimed, and the changes are one
/// stretch from the first byte that differs to the last - exact, but holding about a whole copy of
/// the file for the rest of the run, as a run did before changes were held as stretches.
fn in_bytes(
    source: &encoding::Source,
    edited: &str,
    over: &[(Range<usize>, Range<usize>)],
) -> Over {
    let plain = encoding::Source {
        text: String::new(),
        encoding: source.encoding,
        byte_order_mark: false,
    };
    // Encoding nothing gives the byte-order mark alone, where the file has one.
    let mark = source.encode("").map_or(0, |bytes| bytes.len());
    // Each offset's place in the bytes, found by spelling the text up to it, piece by piece, in
    // order: a sorted list of pairs, looked up by binary search.
    let placed = |text: &str, mut offsets: Vec<usize>| -> Option<Vec<(usize, usize)>> {
        offsets.sort_unstable();
        offsets.dedup();
        let mut placed = Vec::with_capacity(offsets.len());
        let (mut from, mut at) = (0, mark);
        for offset in offsets {
            at += plain.encode(text.get(from..offset)?).ok()?.len();
            placed.push((offset, at));
            from = offset;
        }
        Some(placed)
    };
    let old = placed(
        &source.text,
        over.iter()
            .flat_map(|(was, _)| [was.start, was.end])
            .collect(),
    );
    let new = placed(
        edited,
        over.iter()
            .flat_map(|(_, now)| [now.start, now.end])
            .collect(),
    );
    let (Some(old), Some(new)) = (old, new) else {
        return Vec::new();
    };
    let at = |placed: &[(usize, usize)], offset: usize| {
        let index = placed
            .binary_search_by_key(&offset, |&(offset, _)| offset)
            .expect("every offset was placed");
        placed[index].1
    };
    over.iter()
        .map(|(was, now)| {
            (
                at(&old, was.start)..at(&old, was.end),
                at(&new, now.start)..at(&new, now.end),
            )
        })
        .collect()
}

/// Whether a file's bytes parse in `language`, decoded as the language decodes them; where and why
/// not.
fn parses(language: &dyn FrontEnd, bytes: &[u8]) -> Result<(), String> {
    let source = language
        .decode(bytes)
        .map_err(|refusal| refusal.to_string())?;
    structural::search(language, &source.text, &[])
        .map(|_| ())
        .map_err(|unparsed| unparsed_at(&unparsed))
}

/// The encoding the user's locale names, if it names one other than UTF-8 that quaff reads, from
/// `LC_ALL`, `LC_CTYPE` or `LANG`, the first that is set, as `setlocale` reads them.
///
/// Used only for a hint in a message, never to decide anything: a locale describes the user's
/// terminal, not the project, and the same command must give the same answer on every machine.
fn locale() -> Option<(String, Encoding)> {
    let value = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))?;
    let codeset = value.split_once('.')?.1;
    let codeset = codeset.split('@').next().unwrap_or(codeset);
    match encoding::named(codeset) {
        Named::Read(Encoding::Utf8) | Named::Unread(_) | Named::Unknown => None,
        Named::Read(encoding) => Some((codeset.to_owned(), encoding)),
    }
}

/// A program and the text of each of its sources, which locations are worked out from.
struct Built {
    program: Program,
    texts: Vec<String>,
}

impl Built {
    /// Where in its source the pattern operand at `span` starts its content: past the backtick
    /// fence and any padding space in a script, as the lexer cuts it, and at its start for `-p`,
    /// whose source is the pattern alone.
    fn content_start(&self, span: Span) -> usize {
        if matches!(self.program.sources[span.source], Source::PatternOption) {
            return span.start;
        }
        span.start + lexer::content_start(&self.texts[span.source][span.start..span.end])
    }

    /// Where byte `offset` of the source `span` is in, as a diagnostic names it.
    fn locate(&self, span: Span, offset: usize) -> String {
        location::describe(
            &self.program.sources[span.source],
            &self.texts[span.source],
            offset,
        )
    }
}

/// A script source as a diagnostic names it, with no position.  A query always holds a
/// statement, so only a script can be empty.
fn name(source: &Source) -> String {
    match source {
        Source::File(path) => format!("{path:?}"),
        Source::Stdin => "<stdin>".into(),
        Source::Expression(n) => format!("-e expression {n}"),
        // Unreachable while only scripts can be empty; `Source` is non-exhaustive, so a source
        // added later still gets a name rather than a compile error here.
        _ => "a source".into(),
    }
}

/// Reads and parses each source in turn into one program.
fn build(
    sources: Vec<SourceArg>,
    stdin: &mut impl Read,
    contents: &Contents,
) -> Result<Built, Failure> {
    let mut built = Built {
        program: Program::default(),
        texts: Vec::new(),
    };
    for source in sources {
        let (source, text) = match source {
            SourceArg::Query(query) => {
                built.push_query(query);
                continue;
            }
            SourceArg::Expression { number, text } => (Source::Expression(number), text),
            SourceArg::File(path) => {
                let path = PathBuf::from(path);
                let source = Source::File(path.clone());
                // Kept under its canonical path, as the walker names files, so that a script in
                // the scope is not read again when it is searched.  A path that cannot be
                // canonicalised is read as given, and fails with the system's own error.
                let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                contents.keep_path(&key);
                let read = contents.read(&key).map(|bytes| bytes.to_vec());
                let text = script_text(read, &source)?;
                (source, text)
            }
            SourceArg::Stdin => {
                let mut bytes = Vec::new();
                let read = stdin.read_to_end(&mut bytes).map(|_| bytes);
                (Source::Stdin, script_text(read, &Source::Stdin)?)
            }
        };
        let parsed = parse_source(&mut built.program, source, &text);
        built.texts.push(text);
        if let Err(err) = parsed {
            let span = Span {
                source: built.texts.len() - 1,
                start: err.start,
                end: err.end,
            };
            let location = built.locate(span, err.start);
            let message = err.message;
            return Err(match err.kind {
                ErrorKind::Unknown => Failure::Unknown { location, message },
                ErrorKind::Malformed => Failure::Script { location, message },
            });
        }
    }
    Ok(built)
}

impl Built {
    /// Adds a query from the command line as the `find` it means: `-p` a structural one, and
    /// `-s` or the bare query a textual one.
    fn push_query(&mut self, query: cli::Query) {
        let source = self.program.sources.len();
        self.program.sources.push(match query.source {
            cli::Source::Positional => Source::Positional,
            cli::Source::StringOption => Source::StringOption,
            cli::Source::PatternOption => Source::PatternOption,
        });
        let span = Span {
            source,
            start: 0,
            end: query.text.len(),
        };
        let operand = match query.source {
            cli::Source::PatternOption => Operand::Pattern(query.text.clone()),
            cli::Source::Positional | cli::Source::StringOption => {
                Operand::Text(TextPattern::literal(query.text.clone()))
            }
        };
        let find = Statement::Find(Query::new(Spanned::new(operand, span)));
        self.program.push(Spanned::new(find, span));
        self.texts.push(query.text);
    }
}

/// What `delete` replaces each match with: a rewrite with no code.
static NOTHING: Operand = Operand::Pattern(String::new());

/// The UTF-8 byte-order mark.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// The text of a script that `read` read from `source`, which must be UTF-8, without a leading
/// byte-order mark.
fn script_text(read: io::Result<Vec<u8>>, source: &Source) -> Result<String, Failure> {
    let bytes = read.map_err(|cause| Failure::Io {
        doing: "reading the script",
        path: match source {
            Source::File(path) => path.clone(),
            _ => PathBuf::from("<stdin>"),
        },
        cause,
    })?;
    // A leading byte-order mark is skipped, as it is in a searched file: some editors write
    // one, and nothing else can mean anything there.  It goes before decoding, so a location
    // counts from the first visible character.
    let bytes = match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) => rest.to_vec(),
        None => bytes,
    };
    String::from_utf8(bytes).map_err(|err| {
        let valid_up_to = err.utf8_error().valid_up_to();
        let text = String::from_utf8_lossy(&err.into_bytes()[..valid_up_to]).into_owned();
        Failure::Script {
            location: location::describe(source, &text, valid_up_to),
            message: "the script is not valid UTF-8 here; a script is read as UTF-8".into(),
        }
    })
}

/// One step of a program this build can run, with the query of each pattern in it, whose scope
/// and filter decide which files it looks at.
enum Step<'p> {
    /// Search, and print what was found.
    Find(Wanted<'p>, &'p Query),
    /// Search for each, print what was found, and check each count.
    Assert(Vec<Assertion<'p>>),
    /// Replace text, print what was replaced, and check its count.
    Replace(Replacing<'p>),
}

/// A replace this build can run.
struct Replacing<'p> {
    /// What it replaces, and with what.
    kind: ReplaceKind<'p>,
    /// The count its `expect` requires.
    count: Count,
    /// Where the statement was written.
    span: Span,
    /// Its query.
    query: &'p Query,
    /// Whether its rewrite is empty, so that it deletes what matched, however it is spelled: set
    /// once the rewrite is compiled, and only for how its outcome is worded.  One that is only a
    /// comment is a replace.
    deletes: bool,
}

impl Replacing<'_> {
    /// What the statement does, as a message names it.
    fn verb(&self) -> &'static str {
        if self.deletes { "delete" } else { "replace" }
    }
}

/// What a replace replaces, and with what.
#[derive(Clone, Copy)]
enum ReplaceKind<'p> {
    /// Text, with text.
    Text {
        pattern: &'p TextPattern,
        with: &'p TextPattern,
    },
    /// A structural pattern, with a rewrite template: the pattern as a search finds it, and the
    /// template's index among the run's rewrites.
    Structural { pattern: Wanted<'p>, rewrite: usize },
}

impl Replacing<'_> {
    /// What it replaces, as a search finds it.
    fn wanted(&self) -> Wanted<'_> {
        match self.kind {
            ReplaceKind::Text { pattern, .. } => Wanted::Text(pattern),
            ReplaceKind::Structural { pattern, .. } => pattern,
        }
    }
}

/// What one of the run's patterns is replaced with, if it is replaced.
#[derive(Clone, Copy)]
enum Replacement<'p> {
    /// Text.
    Text(&'p TextPattern),
    /// A rewrite template, by its index among the run's rewrites.
    Structural(usize),
}

/// A structural replace's rewrite template as written: its text, where it was written, and the
/// index of the compiled pattern whose captures it writes back.
type PlannedRewrite<'p> = (&'p str, Span, usize);

/// A program's steps, each structural pattern in it with where it was written, and each rewrite
/// template.
type Planned<'p> = (Vec<Step<'p>>, Vec<(&'p str, Span)>, Vec<PlannedRewrite<'p>>);

/// What a query searches for: text, or a structural pattern.
#[derive(Clone, Copy)]
enum Wanted<'p> {
    /// A textual pattern, matched in every text file.
    Text(&'p TextPattern),
    /// A structural pattern, matched in every file whose language reads it: as written, and
    /// where its compiled forms are in the run's list of them.
    Structural { written: &'p str, compiled: usize },
}

/// One assertion in a block this build can run.
struct Assertion<'p> {
    pattern: Wanted<'p>,
    count: Count,
    span: Span,
    query: &'p Query,
}

/// One pattern the run searches for, and the query it is from.
#[derive(Clone, Copy)]
struct Searching<'p> {
    pattern: Wanted<'p>,
    query: &'p Query,
}

impl Step<'_> {
    /// The step's patterns in order.  One list serves the search and the notes, so they cannot
    /// disagree about which pattern is which.
    fn patterns(&self) -> Vec<Searching<'_>> {
        match self {
            Step::Find(pattern, query) => vec![Searching {
                pattern: *pattern,
                query,
            }],
            Step::Assert(block) => block
                .iter()
                .map(|a| Searching {
                    pattern: a.pattern,
                    query: a.query,
                })
                .collect(),
            Step::Replace(replacing) => vec![Searching {
                pattern: replacing.wanted(),
                query: replacing.query,
            }],
        }
    }

    /// The replacement each of the step's patterns is replaced with, in the order
    /// [`Step::patterns`] lists them: `None` for a pattern only searched for.
    fn replacements(&self) -> Vec<Option<Replacement<'_>>> {
        match self {
            Step::Find(..) => vec![None],
            Step::Assert(block) => block.iter().map(|_| None).collect(),
            Step::Replace(replacing) => vec![Some(match replacing.kind {
                ReplaceKind::Text { with, .. } => Replacement::Text(with),
                ReplaceKind::Structural { rewrite, .. } => Replacement::Structural(rewrite),
            })],
        }
    }
}

/// The steps `program` runs as, and each structural pattern in it in order with where it was
/// written; or the first construct in it that is in the MVP but not built yet, with where it was
/// written.
fn plan(program: &Program) -> Result<Planned<'_>, (&'static str, Span)> {
    let mut steps = Vec::new();
    let mut structural = Vec::new();
    let mut rewrites = Vec::new();
    for statement in program.body() {
        steps.push(match &statement.node {
            Statement::Find(query) => Step::Find(buildable(query, &mut structural)?, query),
            Statement::Assert(block) => Step::Assert(
                block
                    .iter()
                    .map(|a| assertion(a, &mut structural))
                    .collect::<Result<_, _>>()?,
            ),
            Statement::Act(action) => Step::Replace(replacing(
                action,
                statement.span,
                &mut structural,
                &mut rewrites,
            )?),
            _ => return Err(("this statement", statement.span)),
        });
    }
    Ok((steps, structural, rewrites))
}

/// The replace `action` is, if this build can run it.  A structural one's pattern joins
/// `structural` and its template `rewrites`, to be compiled before any file is read.
fn replacing<'p>(
    action: &'p Action,
    span: Span,
    structural: &mut Vec<(&'p str, Span)>,
    rewrites: &mut Vec<PlannedRewrite<'p>>,
) -> Result<Replacing<'p>, (&'static str, Span)> {
    // `delete X` is `replace X with ``: a rewrite with no code, whose matches go with their
    // separator or their line.  It has no behaviour of its own.
    let (with, with_span) = match &action.kind {
        ActionKind::Replace(with) => (&with.node, with.span),
        ActionKind::Delete => (&NOTHING, span),
        _ => return Err(("this kind of edit", span)),
    };
    let kind = match (&action.query.operand.node, with) {
        (Operand::Text(pattern), Operand::Text(with)) => ReplaceKind::Text { pattern, with },
        (Operand::Pattern(_), Operand::Pattern(written)) => {
            let pattern = buildable(&action.query, structural)?;
            let Wanted::Structural { compiled, .. } = pattern else {
                unreachable!("a backticked operand is a structural pattern");
            };
            rewrites.push((written.as_str(), with_span, compiled));
            ReplaceKind::Structural {
                pattern,
                rewrite: rewrites.len() - 1,
            }
        }
        _ => return Err(("this kind of replace", span)),
    };
    // The checker has refused an edit with no `expect`, and the parser takes at most one.
    let Some(count) = action.expect.first() else {
        return Err(("a replace with no `expect`", span));
    };
    Ok(Replacing {
        kind,
        count: count.node,
        span,
        query: &action.query,
        deletes: false,
    })
}

fn assertion<'p>(
    expectation: &'p Spanned<Expectation>,
    structural: &mut Vec<(&'p str, Span)>,
) -> Result<Assertion<'p>, (&'static str, Span)> {
    let Counted::Matches(query) = &expectation.node.counted else {
        return Err(("this assertion", expectation.span));
    };
    Ok(Assertion {
        pattern: buildable(query, structural)?,
        count: expectation.node.count.node,
        span: expectation.span,
        query,
    })
}

/// The pattern of `query`, if this build can search for it.  A structural one joins
/// `structural`, to be compiled with the rest before any file is read.
fn buildable<'p>(
    query: &'p Query,
    structural: &mut Vec<(&'p str, Span)>,
) -> Result<Wanted<'p>, (&'static str, Span)> {
    match &query.operand.node {
        Operand::Text(pattern) => Ok(Wanted::Text(pattern)),
        Operand::Pattern(written) => {
            structural.push((written.as_str(), query.operand.span));
            Ok(Wanted::Structural {
                written,
                compiled: structural.len() - 1,
            })
        }
        _ => Err(("this kind of operand", query.operand.span)),
    }
}

/// Each of the run's patterns' `where`, compiled for each language that reads the pattern, where
/// it tests a metavariable: checked first against the pattern, which must be structural, must
/// bind each name it tests to one node, and with the names it tests must not be too costly to
/// search.  A language that cannot read the `where` does not read the pattern either; one that
/// no language reads stops the run, with the first language's error.
fn filters(
    searching: &[Searching],
    compiled: &mut [Compiled],
    roots: scope::Roots,
    built: &Built,
) -> Result<(), Failure> {
    for for_language in compiled.iter_mut() {
        for_language.filters = searching.iter().map(|_| None).collect();
    }
    for (index, searching) in searching.iter().enumerate() {
        let Some(written) = &searching.query.filter else {
            continue;
        };
        let mut failures = Vec::new();
        let mut reading = 0;
        for for_language in compiled.iter_mut() {
            let pattern = match searching.pattern {
                Wanted::Structural { compiled: at, .. } => match &for_language.patterns[at] {
                    Some(pattern) => Some(pattern),
                    None => continue,
                },
                Wanted::Text(_) => None,
            };
            reading += 1;
            match filter_for(for_language.language, written, pattern, roots, built) {
                Ok(filter) => for_language.filters[index] = filter,
                Err(failure @ (Failure::Unknown { .. } | Failure::Io { .. })) => {
                    return Err(failure);
                }
                Err(failure) => {
                    if let Wanted::Structural { compiled: at, .. } = searching.pattern {
                        for_language.patterns[at] = None;
                    }
                    failures.push(failure);
                }
            }
        }
        if failures.len() == reading
            && let Some(first) = failures.into_iter().next()
        {
            return Err(first);
        }
    }
    Ok(())
}

/// The `where` written, compiled in `language`, where it tests a metavariable, and checked
/// against `pattern`, the statement's in that language, or `None` for a textual statement.
fn filter_for(
    language: &dyn FrontEnd,
    written: &Spanned<Filter>,
    pattern: Option<&Pattern>,
    roots: scope::Roots,
    built: &Built,
) -> Result<Option<filter::Filter>, Failure> {
    let script = |span: Span, message: String| Failure::Script {
        location: built.locate(span, span.start),
        message,
    };
    let outer: Vec<&str> = pattern.map_or_default(|pattern| {
        pattern
            .holes()
            .filter_map(|(_, hole)| hole.name.as_deref())
            .collect()
    });
    let compiled_filter =
        filter::Filter::new(language, written, roots, &outer).map_err(|err| match err {
            filter::Error::NotBuilt(filter::NotBuilt(span)) => Failure::Unknown {
                location: built.locate(span, span.start),
                message: "this `where` test is not built yet".into(),
            },
            filter::Error::Pattern(err, span) => unread(&[(language, err)], span, built),
            filter::Error::Glob(scope::Error::TooLarge(err), span) => script(span, err.to_string()),
            filter::Error::Glob(scope::Error::Io { path, cause }, _) => Failure::Io {
                doing: "looking for",
                path,
                cause,
            },
        })?;
    let Some(compiled_filter) = compiled_filter else {
        return Ok(None);
    };
    let span = metavariable_test(written).unwrap_or(written.span);
    let Some(pattern) = pattern else {
        return Err(script(
            span,
            "a textual search binds no metavariables, so there is nothing for `where` to test \
             here: test FILE, or search for a backticked pattern"
                .into(),
        ));
    };
    check_filter(&compiled_filter, pattern, span, built)?;
    Ok(Some(compiled_filter))
}

/// Checks a compiled `where` against `pattern`, the statement's: each name it tests must be one
/// the pattern binds to one node, its own patterns must share no name with it, and together
/// they must not be too costly to search.  `span` is where its first test of a metavariable
/// was written.
fn check_filter(
    compiled_filter: &filter::Filter,
    pattern: &Pattern,
    span: Span,
    built: &Built,
) -> Result<(), Failure> {
    let script = |span: Span, message: String| Failure::Script {
        location: built.locate(span, span.start),
        message,
    };
    for name in &compiled_filter.names {
        let holes: Vec<bool> = pattern
            .holes()
            .filter(|(_, hole)| hole.name.as_deref() == Some(name.as_str()))
            .map(|(_, hole)| hole.sequence)
            .collect();
        if name == "_" {
            return Err(script(
                span,
                "`$_` binds nothing, so `where` cannot test it".into(),
            ));
        }
        if holes.is_empty() {
            return Err(script(
                span,
                format!("`${name}` is not bound by this statement's pattern"),
            ));
        }
        if holes.contains(&true) {
            // A name the `where`'s pattern shares is tested because it is shared, so say that.
            let sharing = compiled_filter.shapes.iter().find(|(shape, _)| {
                shape
                    .holes()
                    .any(|(_, hole)| hole.name.as_deref() == Some(name.as_str()))
            });
            return Err(match sharing {
                Some((_, at)) => script(
                    *at,
                    format!(
                        "`${name}...` is a sequence in the statement's pattern and one node in \
                         the `where`'s, and a name shared between them is one or the other: \
                         name it differently in one of them"
                    ),
                ),
                None => script(
                    span,
                    format!("`${name}` binds a sequence, and `where` tests what one node holds"),
                ),
            });
        }
    }
    // A name the `where`'s pattern shares with the statement's is bound there to what the
    // statement's match bound it to, one node, so it must be one node in both.
    for (shape, at) in &compiled_filter.shapes {
        if let Some(shared) = shape
            .holes()
            .filter(|(_, hole)| hole.sequence)
            .filter_map(|(_, hole)| hole.name.as_deref())
            .find(|name| compiled_filter.names.iter().any(|tested| tested == name))
        {
            return Err(script(
                *at,
                format!(
                    "`${shared}...` is a sequence in the `where`'s pattern and one node in the \
                     statement's, and a name shared between them is one or the other: name it \
                     differently in one of them"
                ),
            ));
        }
    }
    let names: Vec<&str> = compiled_filter.names.iter().map(String::as_str).collect();
    if pattern.costly_with(&names).is_some() {
        return Err(script(
            span,
            format!(
                "testing {} in `where` makes a search depend on a third place at once where \
                 it can go more than one way - where a sequence stops, or where a run of \
                 statements starts - and each such place multiplies the work of a search by \
                 the length of the block or list it stands in, so a pattern and its `where` \
                 may depend on two between them: test fewer of its names, or give the \
                 pattern more fixed code",
                names
                    .iter()
                    .map(|name| format!("`${name}`"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        ));
    }
    Ok(())
}

/// Where the first test of a metavariable in `filter` was written, if it has one.
fn metavariable_test(filter: &Spanned<Filter>) -> Option<Span> {
    match &filter.node {
        Filter::And(parts) => parts.iter().find_map(metavariable_test),
        Filter::Not(part) => metavariable_test(part),
        Filter::Matches(Subject::Metavariable(_), _)
        | Filter::Contains(Subject::Metavariable(_), _) => Some(filter.span),
        _ => None,
    }
}

/// The command-line scope, if one was given: a plain path relative to the current directory,
/// or from the project root when it begins `:/`, as `in` and `FILE` are.  It may lie outside the
/// project: naming a path is asking for it.
fn narrowing(scope: Option<&Path>, roots: scope::Roots) -> Result<Option<PathBuf>, Failure> {
    let Some(given) = scope else {
        return Ok(None);
    };
    let bytes = given.as_os_str().as_encoded_bytes();
    if bytes.contains(&b'"') {
        return Err(Failure::QuotedScope {
            scope: given.as_os_str().to_owned(),
        });
    }
    // `:` alone is a path's first segment, so the root is only after `:/`.
    let joined = match given.strip_prefix(":") {
        Ok(rest) if bytes.starts_with(b":/") => roots.root.join(rest),
        _ => roots.cwd.join(given),
    };
    fs::canonicalize(joined)
        .map(Some)
        .map_err(|cause| Failure::Io {
            doing: "reading the scope",
            path: given.to_path_buf(),
            cause,
        })
}

/// The projects a run's files are in, each with the declarations that decide its files'
/// encodings.  The first is the current directory's, which `:/` names.
struct Projects {
    list: Vec<Project>,
    contents: Contents,
    /// The project each directory a file was found in belongs to, where finding it meant walking
    /// up from the directory.
    by_dir: std::collections::HashMap<PathBuf, usize>,
}

/// A project, or the directory that stands in for one where there is none.
struct Project {
    /// Its root, above which nothing is read.
    boundary: PathBuf,
    declarations: Declarations,
}

impl Projects {
    /// The projects of a run from `cwd`: to begin with, the one it is in, or `cwd` standing in.
    fn new(cwd: &Path, contents: &Contents) -> Self {
        let mut projects = Projects {
            list: Vec::new(),
            contents: contents.clone(),
            by_dir: std::collections::HashMap::new(),
        };
        projects.add(workspace::find(cwd, contents), cwd, cwd);
        projects
    }

    fn add(&mut self, workspace: Option<Box<dyn Workspace>>, standing_in: &Path, cwd: &Path) {
        let boundary = workspace
            .as_ref()
            .map_or_else(|| standing_in.to_path_buf(), |w| w.root().to_path_buf());
        // EditorConfig is read no higher than the boundary, so that nothing outside a project
        // changes an answer about its files.
        let declarations = Declarations {
            workspace,
            editorconfig: EditorConfig::new(&boundary, &self.contents),
            cwd: cwd.to_path_buf(),
            contents: self.contents.clone(),
        };
        self.list.push(Project {
            boundary,
            declarations,
        });
    }

    /// The project `base`, a path discovery starts from, is in: the current directory's when it
    /// lies under its root; otherwise the one found by walking up from it, or with none the
    /// directory it names, which stands in for one.
    fn of(&mut self, base: &Path, cwd: &Path) -> usize {
        if base.starts_with(&self.list[0].boundary) {
            return 0;
        }
        let dir = if base.is_dir() {
            base
        } else {
            base.parent().unwrap_or(base)
        };
        let workspace = workspace::find(dir, &self.contents);
        let boundary = workspace.as_ref().map_or(dir, |w| w.root());
        if let Some(found) = self.list.iter().position(|p| p.boundary == boundary) {
            return found;
        }
        self.add(workspace, dir, cwd);
        self.list.len() - 1
    }
}

impl Projects {
    /// The deepest of the projects so far whose boundary holds `path`.  The current directory's
    /// holds every file a run finds under it, and each base's project holds that base's.
    fn deepest(&self, path: &Path) -> usize {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, project)| path.starts_with(&project.boundary))
            .max_by_key(|(_, project)| project.boundary.components().count())
            .map_or(0, |(index, _)| index)
    }

    /// The project a file found from `base`'s project is in: that one, unless it is no project
    /// and the file is in a repository below it, which then decides the file's encoding as a
    /// nested repository does inside a project.
    fn of_file(&mut self, file: &Path, base: usize, cwd: &Path) -> usize {
        if self.list[base].declarations.workspace.is_some() {
            return base;
        }
        let Some(dir) = file.parent() else {
            return base;
        };
        if let Some(&known) = self.by_dir.get(dir) {
            return known;
        }
        let owner = match workspace::find(dir, &self.contents) {
            Some(found) => {
                let root = found.root().to_path_buf();
                if let Some(known) = self.list.iter().position(|p| p.boundary == root) {
                    known
                } else {
                    self.add(Some(found), &root, cwd);
                    self.list.len() - 1
                }
            }
            None => base,
        };
        self.by_dir.insert(dir.to_path_buf(), owner);
        owner
    }
}

/// Which files each of a run's patterns looks at, worked out from their queries, with the
/// places discovery starts from and what it notes along the way.
struct Scoping<'a> {
    /// The command-line scope, which every pattern's files lie under.
    narrow: Option<&'a Path>,
    cwd: &'a Path,
    /// The places discovery starts from: each distinct base a pattern needs.
    bases: Vec<PathBuf>,
    /// Notes on scopes that name nothing, or that the command line narrowed.
    notes: Vec<String>,
}

/// Which files one of the run's patterns looks at.
struct Scoped {
    /// The base its files are discovered from, or none when its `in` and the command-line scope
    /// have nothing in common.
    base: Option<usize>,
    /// Its `in`, which its files must match.
    within: Option<scope::PathGlob>,
    /// Its `where`, as far as a file's path alone can decide it.
    test: Option<scope::FileTest>,
}

impl Scoped {
    /// Whether the pattern looks at the file at `place`, which the bases marked in `found_by`
    /// found.  A file its `where` rules out whatever a match binds is not looked at, so it is
    /// never parsed for this pattern and cannot refuse the run.
    fn applies(&self, found_by: &[bool], place: &scope::Place) -> bool {
        self.base.is_some_and(|base| found_by[base])
            && self
                .within
                .as_ref()
                .is_none_or(|within| within.is_match(place))
            && self
                .test
                .as_ref()
                .is_none_or(|test| test.holds(place) != Some(false))
    }
}

impl<'a> Scoping<'a> {
    fn new(narrow: Option<&'a Path>, cwd: &'a Path) -> Self {
        Scoping {
            narrow,
            cwd,
            bases: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The scope of `query`'s pattern.
    fn scoped(
        &mut self,
        query: &Query,
        roots: scope::Roots,
        built: &Built,
    ) -> Result<Scoped, Failure> {
        let compile_error = |err: scope::Error, span: Span| match err {
            scope::Error::TooLarge(err) => Failure::Script {
                location: built.locate(span, span.start),
                message: err.to_string(),
            },
            scope::Error::Io { path, cause } => Failure::Io {
                doing: "looking for",
                path,
                cause,
            },
        };
        let within = match query.scope.first() {
            Some(glob) => Some((
                scope::PathGlob::new(&glob.node, false, roots)
                    .map_err(|err| compile_error(err, glob.span))?,
                glob,
            )),
            None => None,
        };
        let test = match &query.filter {
            Some(filter) => Some(
                scope::FileTest::new(filter, roots)
                    .map_err(|(err, span)| compile_error(err, span))?,
            ),
            None => None,
        };
        if let Some(filter) = &query.filter {
            self.note_missing_files(filter, roots, built);
        }
        // An `in` names its own base, and fails the run if that is not there, whether or not
        // the command line narrows it.
        let named = match &within {
            Some((compiled, glob)) => {
                let base = match compiled.names() {
                    scope::Names::There(path) => Some(path.to_path_buf()),
                    scope::Names::NotThere => None,
                    scope::Names::Glob => scope::base(&glob.node, roots)
                        .map_err(|err| compile_error(err, glob.span))?,
                };
                Some(base.ok_or_else(|| not_there(glob, built))?)
            }
            None => None,
        };
        let base = match (self.narrow, named) {
            // The deeper of the two is where both look; with neither inside the other, the
            // statement looks at nothing.  So the same `in` finds the same files whether or not
            // a scope that contains it is given, which crosses no boundary the `in` would not.
            (Some(narrow), Some(named)) => {
                let glob = &within.as_ref().expect("an `in` named it").1;
                // Said only where it changes what the statement looks at.
                let said = if named.starts_with(narrow) {
                    None
                } else if narrow.starts_with(&named) {
                    Some("narrows this statement's `in`")
                } else {
                    Some("shares nothing with this statement's `in`, so it looks at nothing")
                };
                if let Some(said) = said {
                    self.notes.push(format!(
                        "{}: the scope {:?} given on the command line {said}",
                        built.locate(glob.span, glob.span.start),
                        output::relative(narrow, self.cwd)
                    ));
                }
                if named.starts_with(narrow) {
                    Some(named)
                } else if narrow.starts_with(&named) {
                    Some(narrow.to_path_buf())
                } else {
                    None
                }
            }
            (Some(narrow), None) => Some(narrow.to_path_buf()),
            (None, Some(named)) => Some(named),
            (None, None) => Some(self.cwd.to_path_buf()),
        };
        let base = base.map(|base| self.base(base));
        Ok(Scoped {
            base,
            within: within.map(|(compiled, _)| compiled),
            test,
        })
    }

    /// The index of `base` among the places discovery starts from.
    fn base(&mut self, base: PathBuf) -> usize {
        if let Some(found) = self.bases.iter().position(|b| *b == base) {
            return found;
        }
        self.bases.push(base);
        self.bases.len() - 1
    }

    /// Notes each `FILE matches` of a path with no wildcard that names nothing that is there,
    /// with how to write it as a glob if that is what it reads as.  A note, not a failure: such a
    /// test may guard against a file that does not exist yet.
    fn note_missing_files(&mut self, filter: &Spanned<Filter>, roots: scope::Roots, built: &Built) {
        match &filter.node {
            Filter::And(parts) => {
                for part in parts {
                    self.note_missing_files(part, roots, built);
                }
            }
            Filter::Not(part) => self.note_missing_files(part, roots, built),
            Filter::Matches(Subject::File, operand) => {
                let Operand::Glob(glob) = &operand.node else {
                    return;
                };
                let Some(path) = glob.as_literal() else {
                    return;
                };
                // A glob that cannot be compiled is refused where its test is built instead.
                let there = scope::PathGlob::new(glob, false, roots)
                    .is_ok_and(|compiled| matches!(compiled.names(), scope::Names::There(_)));
                if !there {
                    let mut text = format!(
                        "{}: FILE matches {path:?}, which names nothing that is there",
                        built.locate(operand.span, operand.span.start),
                    );
                    if let Some(markers) = scope::as_markers(path) {
                        text.push_str("; text in quotes is literal, so to match a pattern write ");
                        text.push_str(&markers);
                    }
                    self.notes.push(text);
                }
            }
            _ => {}
        }
    }
}

/// The failure for an `in` that names nothing there: its path, or the directory its glob's
/// leading text names, with how to write it as a glob if that is what it reads as.
fn not_there(glob: &Spanned<Glob>, built: &Built) -> Failure {
    let mut message = match glob.node.as_literal() {
        Some(path) => format!("`in` names {path:?}, which is not there"),
        None => format!(
            "`in` looks for files under {:?}, which is not there",
            scope::leading_directory(&glob.node)
        ),
    };
    if let Some(markers) = glob.node.as_literal().and_then(scope::as_markers) {
        message.push_str("; text in quotes is literal, so to match a pattern write ");
        message.push_str(&markers);
    }
    Failure::NotThere {
        location: built.locate(glob.span, glob.span.start),
        message,
    }
}

/// Each of `files`, with which of the run's patterns, scoped as `scoped`, look at it.
fn to_search(files: Discovered, scoped: &[Scoped], roots: scope::Roots) -> Vec<ToSearch> {
    // A file's path as a glob sees it is worked out only where some `in` or `where` reads it: a
    // run with neither pays nothing for scoping beyond the base each file is in.
    let reads_paths = scoped
        .iter()
        .any(|scoped| scoped.within.is_some() || scoped.test.is_some());
    files
        .into_iter()
        .map(|(path, project, bases)| {
            let applies = if reads_paths {
                let place = scope::Place::of(&path, roots.cwd, roots.root);
                scoped
                    .iter()
                    .map(|scoped| scoped.applies(&bases, &place))
                    .collect()
            } else {
                scoped
                    .iter()
                    .map(|scoped| scoped.base.is_some_and(|base| bases[base]))
                    .collect()
            };
            ToSearch {
                path,
                project,
                applies,
            }
        })
        .collect()
}

/// A file to search, the project it is in, and which of the run's patterns look at it.
struct ToSearch {
    path: PathBuf,
    project: usize,
    applies: Vec<bool>,
}

/// Every file found under a run's bases, in path order, with the project it is in and which
/// bases found it.
type Discovered = Vec<(PathBuf, usize, Vec<bool>)>;

/// Every file under each of `bases`, each discovered by the rules of the project it is in, with
/// the project of each file and which bases found it, and what discovery passed over.
fn discover_bases(
    bases: &[PathBuf],
    projects: &mut Projects,
    cwd: &Path,
) -> Result<(Discovered, discover::Found), Failure> {
    // With one base, discovery's own order is the run's, so nothing need be merged.
    if let [base] = bases {
        let project = projects.of(base, cwd);
        let mut found = discover_base(base, project, projects, cwd)?;
        let files = std::mem::take(&mut found.files)
            .into_iter()
            .map(|path| {
                let owner = projects.of_file(&path, project, cwd);
                (path, owner, vec![true])
            })
            .collect();
        return Ok((files, found));
    }
    let mut files: std::collections::BTreeMap<PathBuf, (usize, Vec<bool>)> =
        std::collections::BTreeMap::new();
    let mut all = discover::Found::default();
    let mut links = std::collections::BTreeSet::new();
    let mut specials = std::collections::BTreeSet::new();
    for (index, base) in bases.iter().enumerate() {
        let project = projects.of(base, cwd);
        let found = discover_base(base, project, projects, cwd)?;
        for path in found.files {
            files
                .entry(path)
                .or_insert_with(|| (project, vec![false; bases.len()]))
                .1[index] = true;
        }
        // Bases overlap when one statement's lies under another's, so each is kept once.
        links.extend(found.links_not_followed);
        specials.extend(found.special_files);
        for rule in found.skipped_rules {
            if !all.skipped_rules.contains(&rule) {
                all.skipped_rules.push(rule);
            }
        }
    }
    all.links_not_followed = links.into_iter().collect();
    all.special_files = specials.into_iter().collect();
    let files = files
        .into_iter()
        .map(|(path, (_, found_by))| {
            // The deepest project holding the file, whichever base found it first, so that its
            // encoding never depends on the order the statements were written in.
            let deepest = projects.deepest(&path);
            let owner = projects.of_file(&path, deepest, cwd);
            (path, owner, found_by)
        })
        .collect();
    Ok((files, all))
}

/// The files under `base`, discovered by the rules of `project`, the one it is in.
fn discover_base(
    base: &Path,
    project: usize,
    projects: &Projects,
    cwd: &Path,
) -> Result<discover::Found, Failure> {
    let project = &projects.list[project];
    // With no project, a repository the walk enters still has its own rules.
    let system = match &project.declarations.workspace {
        Some(workspace) => workspace.ignore_files(),
        None => workspace::ignore_files_outside(&projects.contents),
    };
    discover::files(
        base,
        &project.boundary,
        Some(system),
        cwd,
        &projects.contents,
    )
    .map_err(Failure::Discovery)
}

/// One match a search found, and for a structural one where the code it covers is, which is
/// printed in place of the pattern's text.
///
/// `Copy`, so that it cannot own memory: a run holds every match until it prints, and a copy of
/// each match's code would make that the square of a block's length where the matches overlap.
#[derive(Clone, Copy)]
struct Hit {
    at: Match,
    code: Option<Code>,
}

// A field that owns memory, such as a `String` of the matched code, would not be `Copy`: what a
// match covers is read from the file it was found in when it is printed.
const _: () = {
    const fn holds_no_memory<T: Copy>() {}
    holds_no_memory::<Hit>();
};

/// Where a structural match's code is: in which of the files the run holds, in which version of
/// it - as read, or as a replace left it - and where in that version's decoded text.  Held rather
/// than a copy of the code, since many long matches can overlap, and copies of them all would
/// hold far more than the code itself.
#[derive(Clone, Copy)]
struct Code {
    held: usize,
    version: usize,
    start: usize,
    end: usize,
}

/// What the run holds of a file until it ends, because a replace changed it or a structural match
/// was found in it: whichever is smaller of the file and the code its matches cover.
enum Held {
    /// The whole file: its bytes as read, and what each replace that changed it changed, in
    /// order, from which any version of it is built.  A file a replace changed is always held
    /// so, since it is checked and written from these.
    File {
        original: crate::contents::Bytes,
        changes: Vec<Changes>,
        /// Its language, if quaff parses it, which decodes it for a structural match's code.
        language: Option<&'static dyn FrontEnd>,
    },
    /// The code the structural matches found in a file no replace changed cover, one match's
    /// after another, where that is less than the file: a few short matches in a large file, as
    /// most searches find.  Each match's [`Code`] is where its code stands in this.
    Copied(String),
}

impl Held {
    /// The file's bytes as read and the changes made to it, for a file held whole, as every file
    /// a replace changed is.
    fn file(&self) -> (&crate::contents::Bytes, &[Changes]) {
        match self {
            Held::File {
                original, changes, ..
            } => (original, changes),
            Held::Copied(_) => unreachable!("a file a replace changed is held whole"),
        }
    }

    /// The decoded text a match's [`Code`] places it in: the version it was found in, built and
    /// decoded again, for a file held whole, and the copies for one whose matches were copied.
    fn text(&self, version: usize) -> Cow<'_, str> {
        match self {
            Held::File {
                original,
                changes,
                language,
            } => {
                let bytes = if version == 0 {
                    Cow::Borrowed(&original[..])
                } else {
                    Cow::Owned(changes::applied(original, &changes[..version]))
                };
                let source = language
                    .expect("a file a structural match was found in has a language")
                    .decode(&bytes)
                    .expect("a version a match was found in decoded then");
                Cow::Owned(source.text)
            }
            Held::Copied(copies) => Cow::Borrowed(copies),
        }
    }

    /// How many bytes it holds.
    #[cfg(test)]
    fn size(&self) -> usize {
        match self {
            Held::File {
                original, changes, ..
            } => original.len() + changes.iter().map(Changes::held).sum::<usize>(),
            Held::Copied(copies) => copies.len(),
        }
    }
}

/// What searching every file for every pattern found.
struct Searched {
    /// For each pattern, the files with matches, in discovery order.
    matches: Vec<Vec<(PathBuf, Vec<Hit>)>>,
    /// Files a structural search needs that quaff cannot parse, by the paths a message shows,
    /// with where and why.
    unparsed: Vec<(PathBuf, String)>,
    /// Files in a language quaff parses in some structural pattern's scope, binary ones included.
    supported: usize,
    /// Other files in some structural pattern's scope, which have no structure quaff reads.
    not_applicable: usize,
    /// Files in a language quaff parses in the scope of some structural pattern the language
    /// cannot read, binary ones aside, so not searched for it.
    unread: usize,
    /// For each pattern, the files in a language quaff parses in its scope, binary ones included:
    /// none for a textual one.
    supported_for: Vec<usize>,
    /// For each pattern, the files in a language quaff parses searched for it.
    structural_for: Vec<usize>,
    /// For each pattern, the other files in its scope, which it does not apply to.
    not_applicable_for: Vec<usize>,
    /// For each pattern, the files in its scope in a language quaff parses that cannot read it,
    /// binary ones aside.
    unread_for: Vec<usize>,
    /// For each pattern, the files of unknown encoding holding bytes it would match, by the
    /// paths a message shows, with the lines they are on and why the encoding is unknown.  Never
    /// counted, and never edited.
    possible: Vec<Vec<(PathBuf, Vec<usize>, Unknown)>>,
    /// For each pattern, the files not searched for it because it has no one spelling there,
    /// by the paths a message shows, and why.
    unspelled: Vec<Vec<(PathBuf, Unspelled)>>,
    /// For each replaced pattern, the files whose encoding cannot write its replacement, by the
    /// paths a message shows, with the encoding as `CPython` names its codec, the text it cannot
    /// write, and whether it spells that text more than one way rather than not at all.
    unwritable: Vec<Vec<(PathBuf, &'static str, String, bool)>>,
    /// For each structural replace, the files it cannot be made in as written, by the paths a
    /// message shows, with where and why: refused once its count holds, as an edit its files'
    /// encodings cannot write is, so that a count that does not hold is told first.
    edits_refused: Vec<Vec<(PathBuf, String)>>,
    /// For each structural replace, the places it was made as written but may have changed what
    /// the code means, by the paths a message shows, with where and why: noted only for the
    /// statements the run reached.
    questioned: Vec<Vec<(PathBuf, String)>>,
    /// Every file a replace changed, held until the run ends.
    changed: Vec<Changed>,
    /// Every file the run holds until it ends, which a [`Changed`] or a [`Code`] names by its place
    /// here.
    held: Vec<Held>,
    /// The patterns, by their index, that structural matches were found for in the file being
    /// searched, whose last entry in [`Searched::matches`] is that file's: it must be held.
    cited: Vec<usize>,
    /// For each structural pattern, the files an earlier replace left unparseable, though
    /// they parse on disk, with where and why: the pattern cannot be searched for in them, and
    /// the edit is refused.
    broken: Vec<Vec<(PathBuf, String, Vec<usize>)>>,
    /// Files whose own declared encoding their language's reader refuses, whatever they hold, so
    /// are searched for nothing, by the paths a message shows, with why.
    refused: Vec<(PathBuf, Refused)>,
    /// Files of unknown encoding that read cleanly in the locale's encoding, which a message
    /// mentions as a hint, and that encoding's name as the locale spells it.
    hinted: Vec<PathBuf>,
    hint: Option<String>,
    /// Where the project's version control system declares encodings, for the hint.
    declares_in: Option<&'static str>,
    /// Binary files, counted once whatever the number of patterns.
    binary: usize,
}

/// A file a replace changed, held in memory until the run is over: by its place among the files
/// held, whose new bytes are built when they are checked and written, one file at a time.
struct Changed {
    /// Where it is, to write it.
    path: PathBuf,
    /// The path a message shows.
    shown: PathBuf,
    /// Its language, if quaff parses it: then it must still parse if it parsed before.
    language: Option<&'static dyn FrontEnd>,
    /// Its place in [`Searched::held`].
    held: usize,
    /// The replaces that changed it, by their pattern's index.
    by: Vec<usize>,
}

/// Why a file was not searched for a pattern.
#[derive(Clone)]
enum Unspelled {
    /// Its encoding is known and spells a character of the pattern more than one way.
    Ambiguous(Encoding),
    /// Its encoding is unknown and the pattern is not ASCII.
    Unknown(Unknown),
}

/// The run's patterns in the batches a file is searched in: the patterns between two replaces are
/// searched in one pass over the file as it then is, and each replace changes it for the
/// patterns after.  With no replace, every pattern is in one batch.
enum Batch<'p> {
    /// Search for these, by their index among all the run's patterns: the textual ones, and the
    /// structural ones with their compiled pattern's index.
    Search {
        textual: Vec<(usize, &'p TextPattern)>,
        structural: Vec<(usize, usize)>,
    },
    /// Replace the pattern at this index with this text.
    Replace {
        index: usize,
        pattern: &'p TextPattern,
        with: &'p TextPattern,
    },
    /// Replace the structural pattern at this index, compiled at `compiled`, with the rewrite at
    /// `rewrite`.
    Rewrite {
        index: usize,
        compiled: usize,
        rewrite: usize,
    },
}

/// `patterns` in batches, `replacements` naming each one's replacement, if it is replaced.
fn batches<'p>(
    patterns: &[Wanted<'p>],
    replacements: &[Option<Replacement<'p>>],
) -> Vec<Batch<'p>> {
    let mut batches = Vec::new();
    let mut textual = Vec::new();
    let mut structural = Vec::new();
    for (index, (wanted, with)) in patterns.iter().zip(replacements).enumerate() {
        if with.is_some() && (!textual.is_empty() || !structural.is_empty()) {
            batches.push(Batch::Search {
                textual: std::mem::take(&mut textual),
                structural: std::mem::take(&mut structural),
            });
        }
        match (wanted, with) {
            (Wanted::Text(pattern), Some(Replacement::Text(with))) => {
                batches.push(Batch::Replace {
                    index,
                    pattern,
                    with,
                });
            }
            (Wanted::Structural { compiled, .. }, Some(Replacement::Structural(rewrite))) => {
                batches.push(Batch::Rewrite {
                    index,
                    compiled: *compiled,
                    rewrite: *rewrite,
                });
            }
            (Wanted::Text(pattern), None) => textual.push((index, *pattern)),
            (Wanted::Structural { compiled, .. }, None) => structural.push((index, *compiled)),
            _ => unreachable!("a replace replaces text with text, and a pattern with a pattern"),
        }
    }
    if !textual.is_empty() || !structural.is_empty() {
        batches.push(Batch::Search {
            textual,
            structural,
        });
    }
    batches
}

/// A file as it was read for the run: where it is, the path a message shows, its bytes on disk,
/// how its encoding was decided, what the run's patterns compile to in its language, and whether
/// it is binary.
struct ReadFile<'a> {
    file: &'a Path,
    shown: &'a Path,
    bytes: &'a [u8],
    resolution: &'a Resolution,
    /// What the run's patterns compile to in its language, if quaff parses it.
    compiled: Option<&'a Compiled>,
    binary: bool,
}

/// A structural replace as a file is edited by it: its pattern's index among the run's patterns,
/// the compiled pattern, and its rewrite.
struct Rewriting<'a> {
    index: usize,
    pattern: &'a Pattern,
    /// The rewrite, or why the file's language cannot read it.
    rewrite: &'a Result<Rewrite, CompileError>,
}

/// What earlier replaces did to a file: its bytes on disk if they changed it, which did, by
/// their pattern's index, and so which version of it is being searched, for a match to say where
/// it was found.
struct Earlier<'a> {
    on_disk: Option<&'a [u8]>,
    by: &'a [usize],
    version: Version,
}

/// A version of a file: the place among the files held it is given if it is held, and how many
/// replaces changed it before it.
#[derive(Clone, Copy)]
struct Version {
    held: usize,
    number: usize,
}

/// What a file is searched for: the batches of patterns, the compiled structural patterns, and
/// whether there is any.
struct Plan<'p> {
    batches: Vec<Batch<'p>>,
    /// The structural patterns, rewrites and `where`s, compiled for each language.
    compiled: &'p [Compiled],
    /// The structural patterns, by their index among all the run's patterns, with their place
    /// among the compiled ones.
    structural: Vec<(usize, usize)>,
    /// Where a file's path, as a `FILE` test sees it, is worked out from.
    roots: scope::Roots<'p>,
}

/// Searches each of `files` for each of `patterns`, in order, replacing as `replacements` says,
/// reading each file once and deciding its encoding from what declares it.  A pattern after a
/// replace is searched for in the file as the replace left it; the edits are held, not written.
fn search_files(
    files: &[ToSearch],
    roots: scope::Roots,
    patterns: &[Wanted],
    replacements: &[Option<Replacement>],
    compiled: &[Compiled],
    projects: &mut Projects,
    locale: Option<&(String, Encoding)>,
) -> Result<Searched, Failure> {
    let cwd = roots.cwd;
    let batches = batches(patterns, replacements);
    let structural = patterns
        .iter()
        .enumerate()
        .filter_map(|(i, wanted)| match wanted {
            Wanted::Structural { compiled, .. } => Some((i, *compiled)),
            Wanted::Text(_) => None,
        })
        .collect();
    let mut searched = Searched {
        matches: (0..patterns.len()).map(|_| Vec::new()).collect(),
        unparsed: Vec::new(),
        supported: 0,
        not_applicable: 0,
        unread: 0,
        supported_for: vec![0; patterns.len()],
        structural_for: vec![0; patterns.len()],
        not_applicable_for: vec![0; patterns.len()],
        unread_for: vec![0; patterns.len()],
        possible: vec![Vec::new(); patterns.len()],
        unspelled: vec![Vec::new(); patterns.len()],
        unwritable: vec![Vec::new(); patterns.len()],
        edits_refused: vec![Vec::new(); patterns.len()],
        questioned: vec![Vec::new(); patterns.len()],
        changed: Vec::new(),
        held: Vec::new(),
        cited: Vec::new(),
        broken: vec![Vec::new(); patterns.len()],
        refused: Vec::new(),
        hinted: Vec::new(),
        hint: locale.map(|(name, _)| name.clone()),
        declares_in: projects.list[0]
            .declarations
            .workspace
            .as_ref()
            .map(|w| w.declares_encodings_in()),
        binary: 0,
    };
    let plan = Plan {
        batches,
        compiled,
        structural,
        roots,
    };
    for file in files {
        let declarations = &mut projects.list[file.project].declarations;
        searched.file(&file.path, cwd, &plan, declarations, locale, &file.applies)?;
    }
    Ok(searched)
}

impl Searched {
    /// How many bytes what the run holds until it prints comes to: every match, each a fixed
    /// size, and every file held, as read and as each replace changed it.  The code a match covers
    /// is not among them, since it is read from the file when the match is printed.
    #[cfg(test)]
    fn held_for_printing(&self) -> usize {
        let hits: usize = self
            .matches
            .iter()
            .flatten()
            .map(|(_, hits)| hits.len())
            .sum();
        let files: usize = self.held.iter().map(Held::size).sum();
        hits * std::mem::size_of::<Hit>() + files
    }

    /// Searches one file for the run's patterns that look at it, as `applies` says by their
    /// index, batch by batch, holding its edits.  A file no pattern looks at is not read.
    fn file(
        &mut self,
        file: &Path,
        cwd: &Path,
        plan: &Plan,
        declarations: &mut Declarations,
        locale: Option<&(String, Encoding)>,
        applies: &[bool],
    ) -> Result<(), Failure> {
        if !applies.contains(&true) {
            return Ok(());
        }
        let shown = output::relative(file, cwd);
        let bytes = declarations
            .contents
            .read(file)
            .map_err(|cause| Failure::Io {
                doing: "reading",
                path: shown.clone(),
                cause,
            })?;
        // The file's language, by its name, and its patterns, rewrites and `where`s compiled for
        // it.
        let compiled = plan.compiled.iter().find(|c| c.language.claims(file));
        let language = compiled.map(|c| c.language);
        // Decided once, from the file as it is on disk, whatever the number of patterns.
        let resolution = resolve::resolve(
            &resolve::File {
                path: file,
                bytes: &bytes,
                language,
            },
            declarations,
        )
        .map_err(|err| Failure::Io {
            doing: "reading the encoding declarations in",
            path: output::relative(&err.path, cwd),
            cause: err.cause,
        })?;
        let binary = matches!(resolution, Resolution::Binary);
        self.count_for_structure(plan, applies, compiled, binary);
        let read = ReadFile {
            file,
            shown: &shown,
            bytes: &bytes,
            resolution: &resolution,
            compiled,
            binary,
        };
        self.cited.clear();
        let (edited, by, changes) = self.through_batches(&read, plan, applies);
        self.tally(&shown, resolution, &bytes, locale);
        // Only the changes are kept, not the bytes they made: those are built again when needed.
        let differs = edited.is_some_and(|edited| edited[..] != bytes[..]);
        if differs {
            self.changed.push(Changed {
                path: file.to_path_buf(),
                shown,
                language,
                held: self.held.len(),
                by,
            });
        }
        // The place a match found here was given is this one, taken only if something needs it.
        if differs || (!self.cited.is_empty() && !changes.is_empty()) {
            self.held.push(Held::File {
                original: bytes,
                changes,
                language,
            });
        } else if !self.cited.is_empty() {
            let held = self.copied_or_whole(bytes, language);
            self.held.push(held);
        }
        Ok(())
    }

    /// Takes a file through the run's batches in order, as `applies` says which patterns look at
    /// it: each search on the file as the replaces before it left it.  Returns the file's bytes
    /// once a replace has changed them, the replaces that did, by their pattern's index, and what
    /// each changed.
    fn through_batches(
        &mut self,
        read: &ReadFile,
        plan: &Plan,
        applies: &[bool],
    ) -> (Option<Vec<u8>>, Vec<usize>, Vec<Changes>) {
        // The file as the replaces so far have left it, once one has changed it: whole while the
        // run is on this file, for the statements after each replace to read.
        let mut edited: Option<Vec<u8>> = None;
        // The replaces that changed it so far, by their pattern's index, and what each changed.
        let mut by: Vec<usize> = Vec::new();
        let mut changes: Vec<Changes> = Vec::new();
        let mut unparsed = false;
        for batch in &plan.batches {
            let text: &[u8] = edited.as_deref().unwrap_or(read.bytes);
            // On disk, if an edit has changed it, to tell a file that never parsed from one an
            // edit broke.
            let earlier = Earlier {
                on_disk: edited.is_some().then_some(read.bytes),
                by: &by,
                version: Version {
                    held: self.held.len(),
                    number: changes.len(),
                },
            };
            match batch {
                Batch::Search {
                    textual,
                    structural,
                } => {
                    let textual = applying(textual, applies);
                    let structural = reads(read.compiled, applying(structural, applies));
                    if let Some(compiled) = read.compiled
                        && !structural.is_empty()
                        && !read.binary
                    {
                        self.structural_batch(
                            compiled,
                            read.shown,
                            text,
                            &earlier,
                            &structural,
                            plan,
                            read.file,
                            &mut unparsed,
                        );
                    }
                    match read.resolution {
                        Resolution::Known { encoding, body, .. } => {
                            self.known(read.shown, &text[*body..], *encoding, &textual);
                        }
                        Resolution::Unknown(reason) => {
                            self.unknown(read.shown, text, reason, &textual);
                        }
                        Resolution::Binary | Resolution::Refused(_) => {}
                    }
                }
                Batch::Replace {
                    index,
                    pattern,
                    with,
                } => {
                    if !applies[*index] {
                        continue;
                    }
                    if let Some((whole, over)) =
                        self.replace_in(read.shown, text, read.resolution, *index, pattern, with)
                    {
                        changes.push(Changes::between(text, &whole, &over));
                        edited = Some(whole);
                        by.push(*index);
                    }
                }
                Batch::Rewrite {
                    index,
                    compiled: at,
                    rewrite,
                } => {
                    let Some(compiled) = read
                        .compiled
                        .filter(|c| applies[*index] && !read.binary && c.patterns[*at].is_some())
                    else {
                        continue;
                    };
                    let edit = Rewriting {
                        index: *index,
                        pattern: compiled.patterns[*at]
                            .as_ref()
                            .expect("rewritten only where the language reads the pattern"),
                        rewrite: compiled.rewrites[*rewrite]
                            .as_ref()
                            .expect("a rewrite is compiled where its pattern is"),
                    };
                    if let Some((whole, over)) = self.rewrite_in(
                        compiled,
                        read.shown,
                        text,
                        &earlier,
                        &edit,
                        plan,
                        read.file,
                        &mut unparsed,
                    ) {
                        changes.push(Changes::between(text, &whole, &over));
                        edited = Some(whole);
                        by.push(*index);
                    }
                }
            }
        }
        (edited, by, changes)
    }

    /// Counts a file for each structural pattern that looks at it, as `applies` says: as one with
    /// structural support where its language reads the pattern, a binary one too, since it is in
    /// scope and the run must not say there is none, though it is counted with the binary files
    /// rather than searched; as one whose language cannot read the pattern; or as a text-tier
    /// file, to which no pattern applies.
    fn count_for_structure(
        &mut self,
        plan: &Plan,
        applies: &[bool],
        compiled: Option<&Compiled>,
        binary: bool,
    ) {
        let (mut supported, mut not_applicable, mut unread) = (false, false, false);
        for &(i, at) in plan.structural.iter().filter(|&&(i, _)| applies[i]) {
            match compiled.map(|c| c.patterns[at].is_some()) {
                Some(true) => {
                    self.supported_for[i] += 1;
                    supported = true;
                }
                _ if binary => {}
                Some(false) => {
                    self.unread_for[i] += 1;
                    unread = true;
                }
                None => {
                    self.not_applicable_for[i] += 1;
                    not_applicable = true;
                }
            }
        }
        self.supported += usize::from(supported);
        self.not_applicable += usize::from(not_applicable);
        self.unread += usize::from(unread);
    }

    /// Searches a file in a known encoding, `text` its body after any byte-order mark, for each
    /// textual pattern, by its index among all the run's patterns.
    fn known(
        &mut self,
        shown: &Path,
        text: &[u8],
        encoding: Encoding,
        textual: &[(usize, &TextPattern)],
    ) {
        for &(i, pattern) in textual {
            match search::search(text, encoding, pattern) {
                None => self.unspelled[i].push((shown.to_owned(), Unspelled::Ambiguous(encoding))),
                Some(in_file) if in_file.is_empty() => {}
                Some(in_file) => self.matches[i].push((
                    shown.to_owned(),
                    in_file
                        .into_iter()
                        .map(|at| Hit { at, code: None })
                        .collect(),
                )),
            }
        }
    }

    /// Searches a file's decoded `text`, in `language`, for each structural pattern, `shapes` in
    /// the order of `structural`, which pairs each pattern's index among all the run's patterns
    /// with its compiled one.  A file that does not parse is searched for nothing, and where and
    /// why is returned.
    fn parsed(
        &mut self,
        language: &dyn FrontEnd,
        shown: &Path,
        text: &str,
        version: Version,
        structural: &[(usize, usize)],
        shapes: &[(&Pattern, Option<&structural::Where>)],
    ) -> Result<(), String> {
        match structural::search(language, text, shapes) {
            Err(unparsed) => Err(unparsed_at(&unparsed)),
            Ok(found) => {
                for (&(i, _), in_file) in structural.iter().zip(found) {
                    self.cite(i, shown, version, in_file);
                }
                Ok(())
            }
        }
    }

    /// What to hold of a file no replace changed, `bytes`, that structural matches were found in:
    /// the code they cover, copied, if that is less than the file, as for a few short matches in a
    /// large file, with each match's [`Code`] moved to where its copy stands; and otherwise the
    /// file, as for matches that overlap, each to the end of a long block.  So a run holds no more
    /// of a file than the smaller of the two.
    fn copied_or_whole(
        &mut self,
        bytes: crate::contents::Bytes,
        language: Option<&'static dyn FrontEnd>,
    ) -> Held {
        let cited = std::mem::take(&mut self.cited);
        let codes = |matches: &[(PathBuf, Vec<Hit>)]| -> Vec<Code> {
            let (_, hits) = matches
                .last()
                .expect("a pattern cited here has this file's matches");
            hits.iter().filter_map(|hit| hit.code).collect()
        };
        let covered: usize = cited
            .iter()
            .flat_map(|&index| codes(&self.matches[index]))
            .map(|code| code.end - code.start)
            .sum();
        if covered >= bytes.len() {
            return Held::File {
                original: bytes,
                changes: Vec::new(),
                language,
            };
        }
        let text = language
            .expect("a file a structural match was found in has a language")
            .decode(&bytes)
            .expect("a file a structural match was found in decoded then")
            .text;
        let mut copies = String::with_capacity(covered);
        for index in cited {
            let (_, hits) = self.matches[index]
                .last_mut()
                .expect("a pattern cited here has this file's matches");
            for code in hits.iter_mut().filter_map(|hit| hit.code.as_mut()) {
                let start = copies.len();
                copies.push_str(&text[code.start..code.end]);
                code.start = start;
                code.end = copies.len();
            }
        }
        Held::Copied(copies)
    }

    /// Records the structural matches `found` of the pattern at `index` in a file, as where each
    /// is in the version of it searched, holding the file if there is any.
    fn cite(
        &mut self,
        index: usize,
        shown: &Path,
        version: Version,
        found: Vec<structural::Found>,
    ) {
        if found.is_empty() {
            return;
        }
        self.cited.push(index);
        self.matches[index].push((
            shown.to_owned(),
            found
                .into_iter()
                .map(|f| Hit {
                    at: f.at,
                    code: Some(Code {
                        held: version.held,
                        version: version.number,
                        start: f.range.start,
                        end: f.range.end,
                    }),
                })
                .collect(),
        ));
    }

    /// Searches a file's current bytes, `text`, for a batch's structural patterns, as `compiled`
    /// has them for its language.
    /// `unparsed` says whether an earlier batch found the file unparseable, so that it is counted
    /// as such once however many batches search it.  `on_disk` is the file as it is on
    /// disk when an earlier replace has changed it: if that parses and `text` does not, the edit
    /// broke it, which refuses the run as an edit rather than as a file that cannot be parsed.
    #[expect(
        clippy::too_many_arguments,
        reason = "the file, its batch and what earlier batches found, as a search has them"
    )]
    fn structural_batch(
        &mut self,
        compiled: &Compiled,
        shown: &Path,
        text: &[u8],
        earlier: &Earlier,
        structural: &[(usize, usize)],
        plan: &Plan,
        file: &Path,
        unparsed: &mut bool,
    ) {
        // The file's path as a glob sees it, worked out only if some `where` may read it.
        let language = compiled.language;
        let place = structural
            .iter()
            .any(|&(i, _)| compiled.filters[i].is_some())
            .then(|| scope::Place::of(file, plan.roots.cwd, plan.roots.root));
        // Each pattern's `where`, decided for this file's path and for what a match binds.
        let filters: Vec<Option<structural::Where>> = structural
            .iter()
            .map(|&(i, _)| {
                compiled.filters[i].as_ref().map(|filter| {
                    deciding(
                        filter,
                        place
                            .as_ref()
                            .expect("worked out for a pattern with a `where`"),
                    )
                })
            })
            .collect();
        let shapes: Vec<(&Pattern, Option<&structural::Where>)> = structural
            .iter()
            .zip(&filters)
            .map(|(&(_, at), filter)| {
                let pattern = compiled.patterns[at]
                    .as_ref()
                    .expect("searched only where the language reads the pattern");
                (pattern, filter.as_ref())
            })
            .collect();
        // The text parsed is the text the language's own reader would read, decoded as it
        // decodes it, whatever else declares the file's encoding for the textual tier.
        let outcome = match language.decode(text) {
            Ok(source) => self.parsed(
                language,
                shown,
                &source.text,
                earlier.version,
                structural,
                &shapes,
            ),
            // A file its language's reader refuses does not parse, whatever it holds, and one in
            // an encoding quaff does not read cannot be parsed by quaff: either way a count
            // without it could not be trusted.
            Err(refusal) => Err(refusal.to_string()),
        };
        if outcome.is_ok() {
            for &(i, _) in structural {
                self.structural_for[i] += 1;
            }
        }
        let Err(why) = outcome else {
            return;
        };
        let indices: Vec<usize> = structural.iter().map(|&(i, _)| i).collect();
        self.not_parsed(language, shown, why, earlier, &indices, unparsed);
    }

    /// Records that a file could not be parsed in `language` for the patterns at `indices`, `why`:
    /// as broken by an earlier replace, if it parses on disk and a replace changed it, and
    /// otherwise as a file that cannot be parsed, once however many batches find it so.
    fn not_parsed(
        &mut self,
        language: &dyn FrontEnd,
        shown: &Path,
        why: String,
        earlier: &Earlier,
        indices: &[usize],
        unparsed: &mut bool,
    ) {
        if earlier
            .on_disk
            .is_some_and(|bytes| parses(language, bytes).is_ok())
        {
            for &i in indices {
                self.broken[i].push((shown.to_owned(), why.clone(), earlier.by.to_vec()));
            }
        } else if !*unparsed {
            self.unparsed.push((shown.to_owned(), why));
            *unparsed = true;
        }
    }

    /// Replaces a structural pattern in a file's current bytes, `text`, as `edit` says, and
    /// returns the file's new bytes if that changed it, with each stretch of `text` it wrote over
    /// and where it went in them, as [`Changes::between`] takes.  The text is decoded as the
    /// file's language decodes it and written back in the same encoding, so that every byte
    /// outside the matches is as it was.  A file that does not parse is recorded as a search
    /// records it; an edit that cannot be made as written, or written in the file's encoding, is
    /// recorded for the run to refuse.
    #[expect(
        clippy::too_many_arguments,
        reason = "the file, the edit and what earlier batches found, as a search has them"
    )]
    fn rewrite_in(
        &mut self,
        compiled: &Compiled,
        shown: &Path,
        text: &[u8],
        earlier: &Earlier,
        edit: &Rewriting,
        plan: &Plan,
        file: &Path,
        unparsed: &mut bool,
    ) -> Option<(Vec<u8>, Over)> {
        let index = edit.index;
        let language = compiled.language;
        // The file's path as a glob sees it, worked out only if a `where` may read it.
        let placed = compiled.filters[index].as_ref().map(|filter| {
            let place = scope::Place::of(file, plan.roots.cwd, plan.roots.root);
            (filter, place)
        });
        let filter = placed
            .as_ref()
            .map(|(filter, place)| deciding(filter, place));
        let source = match language.decode(text) {
            Ok(source) => source,
            Err(refusal) => {
                self.not_parsed(
                    language,
                    shown,
                    refusal.to_string(),
                    earlier,
                    &[index],
                    unparsed,
                );
                return None;
            }
        };
        let rewrite = match edit.rewrite {
            Ok(rewrite) => rewrite,
            Err(err) => {
                self.unwritten(
                    language,
                    shown,
                    &source.text,
                    earlier,
                    edit,
                    filter.as_ref(),
                    err,
                    unparsed,
                );
                return None;
            }
        };
        let replaced = match rewrite::replace(
            language,
            &source.text,
            edit.pattern,
            filter.as_ref(),
            rewrite,
        ) {
            Ok(replaced) => replaced,
            Err(err) => {
                self.not_parsed(
                    language,
                    shown,
                    unparsed_at(&err),
                    earlier,
                    &[index],
                    unparsed,
                );
                return None;
            }
        };
        self.structural_for[index] += 1;
        self.cite(index, shown, earlier.version, replaced.found);
        match replaced.outcome {
            Ok(None) => None,
            Ok(Some(edited)) => match source.encode(&edited.text) {
                Ok(bytes) => {
                    for questioned in edited.questioned {
                        self.questioned[index].push((
                            shown.to_owned(),
                            format!(
                                "line {}, column {}: {}",
                                questioned.at.line, questioned.at.column, questioned.why
                            ),
                        ));
                    }
                    let over = in_bytes(&source, &edited.text, &edited.over);
                    Some((bytes, over))
                }
                Err(unencodable) => {
                    self.unwritable[index].push((
                        shown.to_owned(),
                        unencodable.encoding,
                        unencodable.character.to_string(),
                        unencodable.ambiguous,
                    ));
                    None
                }
            },
            Err(refused) => {
                self.edits_refused[index].push((
                    shown.to_owned(),
                    format!(
                        "line {}, column {}: {}",
                        refused.at.line, refused.at.column, refused.why
                    ),
                ));
                None
            }
        }
    }

    /// Searches a file whose language reads a replace's pattern but not its rewrite: each match
    /// is counted and cited, and the first refuses the edit, since it cannot be written there.
    #[expect(
        clippy::too_many_arguments,
        reason = "the file, the edit and why its rewrite cannot be compiled"
    )]
    fn unwritten(
        &mut self,
        language: &dyn FrontEnd,
        shown: &Path,
        text: &str,
        earlier: &Earlier,
        edit: &Rewriting,
        filter: Option<&structural::Where>,
        err: &CompileError,
        unparsed: &mut bool,
    ) {
        let index = edit.index;
        let found = match structural::search(language, text, &[(edit.pattern, filter)]) {
            Ok(mut found) => found.swap_remove(0),
            Err(why) => {
                self.not_parsed(
                    language,
                    shown,
                    unparsed_at(&why),
                    earlier,
                    &[index],
                    unparsed,
                );
                return;
            }
        };
        self.structural_for[index] += 1;
        if let Some(first) = found.first() {
            self.edits_refused[index].push((
                shown.to_owned(),
                format!(
                    "line {}, column {}: the rewrite cannot be written in {}: {err}",
                    first.at.start.line,
                    first.at.start.column,
                    language.language()
                ),
            ));
        }
        self.cite(index, shown, earlier.version, found);
    }

    /// Counts a file once by what it is: binary, refused by its language's reader, or of unknown
    /// encoding that reads cleanly in the locale's, for the hint.
    fn tally(
        &mut self,
        shown: &Path,
        resolution: Resolution,
        bytes: &[u8],
        locale: Option<&(String, Encoding)>,
    ) {
        match resolution {
            Resolution::Binary => self.binary += 1,
            Resolution::Refused(refusal) => self.refused.push((shown.to_owned(), refusal)),
            Resolution::Unknown(reason) => {
                if reason == Unknown::NotUtf8
                    && locale.is_some_and(|(_, encoding)| encoding.reads_cleanly(bytes))
                {
                    self.hinted.push(shown.to_owned());
                }
            }
            Resolution::Known { .. } => {}
        }
    }

    /// Replaces the pattern at `index` with `with` in a file's current bytes, `text`, decided as
    /// `resolution` says, and returns its new bytes if that changed it, with where each
    /// replacement went, as [`Changes::between`] takes.  A file of unknown encoding is never
    /// edited: its matches are only possible, and reported as in a search.
    fn replace_in(
        &mut self,
        shown: &Path,
        text: &[u8],
        resolution: &Resolution,
        index: usize,
        pattern: &TextPattern,
        with: &TextPattern,
    ) -> Option<(Vec<u8>, Over)> {
        match resolution {
            Resolution::Known { encoding, body, .. } => {
                let replaced = search::replace(&text[*body..], *encoding, pattern, with);
                let written = self.replaced(shown, index, *encoding, replaced)?;
                let mut whole = text[..*body].to_vec();
                whole.extend_from_slice(&written.bytes);
                // The replace saw the text after any byte-order mark, which is before it in both.
                let over = written
                    .over
                    .into_iter()
                    .map(|(was, now)| {
                        (
                            was.start + body..was.end + body,
                            now.start + body..now.end + body,
                        )
                    })
                    .collect();
                Some((whole, over))
            }
            Resolution::Unknown(reason) => {
                self.unknown(shown, text, reason, &[(index, pattern)]);
                None
            }
            Resolution::Binary | Resolution::Refused(_) => None,
        }
    }

    /// Records what replacing the pattern at `index` in a file in `encoding` found, and returns
    /// the file's new text if the replace changed it.
    fn replaced(
        &mut self,
        shown: &Path,
        index: usize,
        encoding: Encoding,
        replaced: search::Replaced,
    ) -> Option<search::Written> {
        let hits = |matches: Vec<Match>| -> Vec<Hit> {
            matches
                .into_iter()
                .map(|at| Hit { at, code: None })
                .collect()
        };
        match replaced {
            search::Replaced::NotSearched => {
                self.unspelled[index].push((shown.to_owned(), Unspelled::Ambiguous(encoding)));
                None
            }
            search::Replaced::Edited { matches, text } => {
                if !matches.is_empty() {
                    self.matches[index].push((shown.to_owned(), hits(matches)));
                }
                text
            }
            search::Replaced::Unwritable {
                matches,
                text,
                ambiguous,
            } => {
                self.matches[index].push((shown.to_owned(), hits(matches)));
                self.unwritable[index].push((shown.to_owned(), name_of(encoding), text, ambiguous));
                None
            }
        }
    }

    /// Searches a file of unknown encoding for each textual pattern.  An ASCII query is the
    /// same bytes in every ASCII-compatible encoding, so where its bytes are absent there is
    /// certainly no match; where present, a match is only possible, since in some encodings those
    /// bytes end a character.  In a declared UTF-16 or UTF-32 it is other bytes, so no query can
    /// be searched.
    fn unknown(
        &mut self,
        shown: &Path,
        bytes: &[u8],
        reason: &Unknown,
        textual: &[(usize, &TextPattern)],
    ) {
        for &(i, pattern) in textual {
            let in_file = if reason.ascii_compatible() {
                search::search(bytes, Encoding::Unknown8Bit, pattern)
            } else {
                None
            };
            match in_file {
                None => {
                    self.unspelled[i].push((shown.to_owned(), Unspelled::Unknown(reason.clone())));
                }
                Some(in_file) if in_file.is_empty() => {}
                Some(in_file) => {
                    let mut lines: Vec<usize> = in_file.iter().map(|m| m.start.line).collect();
                    lines.dedup();
                    self.possible[i].push((shown.to_owned(), lines, reason.clone()));
                }
            }
        }
    }
}

/// `filter`, a statement's `where`, decided for a file at `place` and for what a match binds.
fn deciding<'f>(filter: &'f filter::Filter, place: &'f scope::Place) -> structural::Where<'f> {
    let found = filter::Found::default();
    structural::Where {
        names: filter.names.iter().map(String::as_str).collect(),
        holds: Box::new(move |arena, nodes| filter.holds(place, arena, nodes, &found)),
    }
}

/// The structural patterns of `batch` that `compiled`'s language reads: the rest do not apply to
/// a file of it.  All of them where it reads them all, as one language reads every pattern.
fn reads<'b>(
    compiled: Option<&Compiled>,
    batch: Cow<'b, [(usize, usize)]>,
) -> Cow<'b, [(usize, usize)]> {
    match compiled {
        Some(compiled) if !batch.iter().all(|&(_, at)| compiled.patterns[at].is_some()) => {
            Cow::Owned(
                batch
                    .iter()
                    .copied()
                    .filter(|&(_, at)| compiled.patterns[at].is_some())
                    .collect(),
            )
        }
        _ => batch,
    }
}

/// The patterns in `batch`, each by its index among all the run's patterns, that look at a file,
/// as `applies` says by that index: the batch itself, uncopied, when they all do.
fn applying<'b, T: Copy>(batch: &'b [(usize, T)], applies: &[bool]) -> Cow<'b, [(usize, T)]> {
    if batch.iter().all(|&(i, _)| applies[i]) {
        Cow::Borrowed(batch)
    } else {
        Cow::Owned(batch.iter().copied().filter(|&(i, _)| applies[i]).collect())
    }
}

/// Where and why a file does not parse in its language, as a message gives it.
fn unparsed_at(unparsed: &structural::Unparsed) -> String {
    format!(
        "line {}, column {}: {}",
        unparsed.at.line, unparsed.at.column, unparsed.message
    )
}

/// Walking the steps in order over what the search found.
struct Evaluation<'s> {
    searched: &'s Searched,
    /// Whether the reader of the results is still there.
    printing: bool,
    /// The index of the next pattern, in the order `Step::patterns` lists them.
    next: usize,
    /// Notes and failed assertions, for standard error once the results are out.
    notes: Vec<String>,
    /// How many matches the replaces replaced, how many those with no code deleted, and in which
    /// files.
    replaced: usize,
    deleted: usize,
    replaced_in: std::collections::BTreeSet<&'s Path>,
    /// For each pattern, where its replace was written, if it is a replace's.
    locations: Vec<Option<String>>,
}

impl<'s> Evaluation<'s> {
    /// An evaluation of what `searched` found, printing as it goes or not.
    fn new(searched: &'s Searched, printing: bool, locations: Vec<Option<String>>) -> Self {
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

    fn steps(
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

/// The text a pattern matches, for display: each line break as LF, which `output::fold`
/// shows as a space, as it would the file's own line ending.
fn display(pattern: &TextPattern) -> String {
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
fn quoted(pattern: Wanted) -> String {
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
fn expected(count: Count) -> String {
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
fn skipped(
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
fn name_of(encoding: Encoding) -> &'static str {
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
fn lines_of(lines: &[usize]) -> String {
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
fn print(out: &mut impl Write, line: &str) -> Result<bool, Failure> {
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

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operand_this_build_cannot_search_for_is_not_built_yet_where_it_was_written() {
        // The MVP check refuses a bound operand before planning, so no script reaches this;
        // the planner still says what it cannot build rather than searching for nothing.
        let span = Span {
            source: 0,
            start: 4,
            end: 5,
        };
        let query = Query::new(Spanned::new(Operand::Bound("f".into()), span));
        let mut structural = Vec::new();
        let refused = buildable(&query, &mut structural).err();
        assert_eq!(refused, Some(("this kind of operand", span)));
        assert_eq!(structural.len(), 0, "{structural:?}");
    }

    #[test]
    fn each_failure_has_the_exit_code_for_its_remedy() {
        assert_eq!(Failure::Usage(UsageError::NoQuery).exit_code(), 3);
        let quoted = Failure::QuotedScope {
            scope: "\"x\"".into(),
        };
        assert_eq!(quoted.exit_code(), 3);
        let io = Failure::Io {
            doing: "reading",
            path: "a".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(io.exit_code(), 7);
    }

    #[test]
    fn a_missing_scope_that_looks_like_a_glob_says_globs_are_not_expanded() {
        let failure = Failure::Io {
            doing: "reading the scope",
            path: "src/**/*.py".into(),
            cause: io::Error::from(io::ErrorKind::NotFound),
        };
        let message = failure.to_string();
        assert!(
            message.starts_with("reading the scope \"src/**/*.py\": "),
            "{message}"
        );
        assert!(
            message.ends_with(
                "quaff does not expand a glob given as the scope; name one file or directory, or \
                 write the pattern with `in` in the script"
            ),
            "{message}"
        );
    }

    /// A standard output whose reader has gone.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_pipe_ends_the_run_quietly() {
        assert!(!print(&mut Closed, "x").unwrap());
    }

    #[test]
    fn with_the_reader_gone_assertions_still_decide_the_outcome() {
        let dir = std::env::temp_dir().join(format!("quaffed-closed-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creating the directory");
        fs::write(dir.join("a.txt"), "TODO\nTODO\n").expect("writing");
        let outcome = |script: &str| {
            let sources = vec![SourceArg::Expression {
                number: 1,
                text: script.to_owned(),
            }];
            run(
                sources,
                None,
                &cli::Options::default(),
                &dir,
                &mut io::empty(),
                &mut Closed,
                &mut io::sink(),
            )
        };
        let failed = outcome("find \"TODO\" expect 1");
        let held = outcome("find \"TODO\" expect 2");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(failed.expect("running"), Outcome::AssertionFailed);
        assert_eq!(held.expect("running"), Outcome::Success);
    }

    #[test]
    fn every_encoding_is_named_as_cpython_names_its_codec() {
        let sjis = quaffed_encoding::MultiByteCodec::named("shift_jis").unwrap();
        assert_eq!(name_of(Encoding::MultiByte(sjis)), "shift_jis");
        assert_eq!(name_of(Encoding::Utf8), "utf_8");
        assert_eq!(name_of(Encoding::Utf16Le), "utf_16_le");
        assert_eq!(name_of(Encoding::Utf16Be), "utf_16_be");
        assert_eq!(name_of(Encoding::Utf32Le), "utf_32_le");
        assert_eq!(name_of(Encoding::Utf32Be), "utf_32_be");
        assert_eq!(name_of(Encoding::Unknown8Bit), "unknown");
    }

    #[test]
    fn line_numbers_are_worded_for_one_and_for_many() {
        assert_eq!(lines_of(&[3]), "line 3");
        assert_eq!(lines_of(&[3, 7, 12]), "lines 3, 7, 12");
        assert_eq!(expected(Count::Any), "any number of matches");
    }

    #[test]
    fn a_pattern_is_named_with_its_anchors_outside_the_quotes() {
        let mut pattern = TextPattern::literal("x");
        assert_eq!(quoted(Wanted::Text(&pattern)), "\"x\"");
        pattern.start_anchor = true;
        pattern.push_line_break();
        pattern.push_literal("y".into());
        assert_eq!(quoted(Wanted::Text(&pattern)), "^\"x\\ny\"");
        pattern.start_anchor = false;
        pattern.end_anchor = true;
        assert_eq!(quoted(Wanted::Text(&pattern)), "\"x\\ny\"$");
        // A structural pattern is named in backticks, as a script writes it.
        let structural = Wanted::Structural {
            written: "f($a)",
            compiled: 0,
        };
        assert_eq!(quoted(structural), "`f($a)`");
    }

    #[test]
    fn any_other_write_failure_is_an_io_error_naming_standard_output() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::StorageFull))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let failure = print(&mut Full, "x").unwrap_err();
        assert_eq!(failure.exit_code(), 7);
        assert!(
            failure
                .to_string()
                .starts_with("writing the results to \"standard output\": "),
            "{failure}"
        );
    }

    #[test]
    fn the_underlying_error_stays_on_the_chain() {
        use std::error::Error as _;
        let io = Failure::Io {
            doing: "reading",
            path: "a".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            io.source().map(ToString::to_string),
            Some(io::Error::from(io::ErrorKind::PermissionDenied).to_string())
        );
        assert!(Failure::Usage(UsageError::NoQuery).source().is_none());
    }

    #[test]
    fn a_failed_count_is_worded_as_the_script_wrote_it() {
        assert_eq!(expected(Count::Exactly(1)), "exactly 1 match");
        assert_eq!(expected(Count::Exactly(3)), "exactly 3 matches");
        assert_eq!(expected(Count::AtLeast(2)), "at least 2 matches");
        assert_eq!(expected(Count::AtMost(1)), "at most 1 match");
        assert_eq!(
            expected(Count::ExactlyOrNone(4)),
            "exactly 4 matches or none"
        );
        assert_eq!(expected(Count::None), "no matches");
    }

    #[test]
    fn every_file_is_read_from_disk_once_whatever_reads_it() {
        // Each rules file is in the scope as well, so it is both read for its rules - by
        // discovery or a declaration reader - and searched for the query.
        let dir = std::env::temp_dir().join(format!("quaffed-run-once-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (path, text) in [
            (".git/info/attributes", "*.md text\n"),
            (".gitignore", "*.log\n"),
            (".ignore", "*.tmp\n"),
            (".gitattributes", "*.c text\n"),
            (".editorconfig", "root = true\n"),
            ("sub/.gitignore", "*.bak\n"),
            ("sub/.editorconfig", "[*]\n"),
            ("sub/a.txt", "TODO\n"),
            ("b.py", "TODO = 1\n"),
        ] {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let dir = fs::canonicalize(&dir).unwrap();
        crate::contents::DISK_READS.with(|reads| reads.borrow_mut().clear());
        let outcome = run(
            vec![SourceArg::Query(cli::Query {
                text: "TODO".into(),
                source: cli::Source::Positional,
            })],
            None,
            &cli::Options::default(),
            &dir,
            &mut io::empty(),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let reads = crate::contents::DISK_READS.with(|reads| reads.borrow().clone());
        let _ = fs::remove_dir_all(&dir);
        assert!(outcome.is_ok(), "{outcome:?}");
        let twice: Vec<_> = reads
            .iter()
            .filter(|path| reads.iter().filter(|read| read == path).count() > 1)
            .collect();
        assert!(twice.is_empty(), "read more than once: {twice:?}");
        for rules in [
            ".gitignore",
            ".ignore",
            ".gitattributes",
            ".editorconfig",
            "sub/.gitignore",
        ] {
            assert!(
                reads.contains(&dir.join(rules)),
                "{rules} never read: {reads:?}"
            );
        }
    }

    #[test]
    fn a_git_pointer_file_is_read_from_disk_once() {
        // A linked worktree's .git is a file naming its own git directory, whose commondir names
        // the main one, where info/ is; both are read for the repository's info/exclude and
        // again for its info/attributes.
        let dir = std::env::temp_dir().join(format!("quaffed-run-pointer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let gitdir = dir.join("gitdir");
        let project = dir.join("project");
        for (path, text) in [
            ("gitdir/info/attributes", "*.md text\n"),
            ("gitdir/info/exclude", "*.tmp\n"),
            ("gitdir/worktrees/project/commondir", "../..\n"),
            ("project/a.txt", "TODO\n"),
        ] {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let worktree = gitdir.join("worktrees/project");
        fs::write(
            project.join(".git"),
            format!("gitdir: {}\n", worktree.display()),
        )
        .unwrap();
        let project = fs::canonicalize(&project).unwrap();
        crate::contents::DISK_READS.with(|reads| reads.borrow_mut().clear());
        let outcome = run(
            vec![SourceArg::Query(cli::Query {
                text: "TODO".into(),
                source: cli::Source::Positional,
            })],
            None,
            &cli::Options::default(),
            &project,
            &mut io::empty(),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let reads = crate::contents::DISK_READS.with(|reads| reads.borrow().clone());
        let _ = fs::remove_dir_all(&dir);
        assert!(outcome.is_ok(), "{outcome:?}");
        for pointer in [project.join(".git"), worktree.join("commondir")] {
            let count = reads.iter().filter(|read| **read == pointer).count();
            assert_eq!(
                count,
                1,
                "{} read {count} times: {reads:?}",
                pointer.display()
            );
        }
    }

    /// A stretch of `text`'s bytes, in `encoding`'s spelling, rewritten: `old` in the file as
    /// decoded becomes `new`, and the claim is checked to be exactly what changed.
    fn claimed_in_bytes(file: &[u8], old: &str, new: &str) {
        let source = crate::languages::PYTHON.decode(file).expect("decodes");
        let at = source.text.find(old).expect("the old text is in the file");
        let edited = source.text.replacen(old, new, 1);
        let over = [(at..at + old.len(), at..at + new.len())];
        let bytes = source.encode(&edited).expect("encodes");
        let claimed = in_bytes(&source, &edited, &over);
        assert_eq!(claimed.len(), 1, "{claimed:?}");
        let (was, now) = &claimed[0];
        let mut rebuilt = file[..was.start].to_vec();
        rebuilt.extend_from_slice(&bytes[now.clone()]);
        rebuilt.extend_from_slice(&file[was.end..]);
        assert_eq!(rebuilt, bytes, "{claimed:?}");
        let changes = Changes::between(file, &bytes, &claimed);
        assert_eq!(changes.held(), now.len());
    }

    #[test]
    fn a_rewrites_stretches_are_found_in_the_files_bytes_whatever_its_encoding() {
        // After a byte-order mark, which comes before every stretch.
        claimed_in_bytes(
            b"\xef\xbb\xbfx = 'caf\xc3\xa9'\ny = 2\n",
            "y = 2",
            "y = (2)",
        );
        // Single-byte: each character one byte, so a decoded offset past "é" is not a byte one.
        claimed_in_bytes(
            b"# -*- coding: latin-1 -*-\nx = 'caf\xe9'\ny = 2\n",
            "y = 2",
            "y = 'na\u{ef}ve'",
        );
        // Multi-byte: "日本" is four bytes in Shift-JIS and six in UTF-8.
        claimed_in_bytes(
            b"# -*- coding: shift_jis -*-\nx = '\x93\xfa\x96\x7b'\ny = 2\n",
            "y = 2",
            "y = '\u{65e5}'",
        );
    }

    #[test]
    fn a_stretch_off_a_character_boundary_claims_nothing() {
        let source = crate::languages::PYTHON
            .decode("x = 'é'\n".as_bytes())
            .unwrap();
        // Byte 6 is inside "é".
        assert_eq!(in_bytes(&source, "x = 'é'\n", &[(6..7, 6..7)]), Vec::new());
    }

    #[test]
    fn a_search_holds_the_code_its_matches_cover_not_the_file_they_are_in() {
        let mut text = "a = 1\n".repeat(5000);
        text.push_str("TODO = 2\nTODO = 33\n");
        let (held, printed) = held_and_printed_in(&text, "find `TODO = $v` expect 2");
        assert!(printed > 0);
        // The two matches, each a fixed size, and their code copied: none of the rest.
        let code = "TODO = 2".len() + "TODO = 33".len();
        assert_eq!(held, 2 * std::mem::size_of::<Hit>() + code);
        // Matches covering more than the file hold the file instead, never more than it.
        let (held, printed) = held_and_printed(50, "find `x = $v\n$mid...\nend()` expect any");
        let block: usize = (0..50).map(|i| format!("x = {i}\n").len()).sum::<usize>() + 6;
        assert!(printed > block);
        assert_eq!(held, 49 * std::mem::size_of::<Hit>() + block);
    }

    #[test]
    fn an_edit_holds_what_it_wrote_not_a_copy_of_the_file() {
        let dir = std::env::temp_dir().join(format!("quaffed-run-held-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        let mut text = "a = 1\n".repeat(5000);
        text.push_str("TODO = 2\n");
        fs::write(dir.join("big.py"), &text).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let held = |script: &str| {
            let outcome = run(
                vec![SourceArg::Expression {
                    number: 1,
                    text: script.to_owned(),
                }],
                None,
                &cli::Options {
                    dry_run: true,
                    ..cli::Options::default()
                },
                &dir,
                &mut io::empty(),
                &mut Vec::new(),
                &mut Vec::new(),
            );
            assert!(outcome.is_ok(), "{script}: {outcome:?}");
            CHANGES_HELD.with(std::cell::Cell::get)
        };
        let textual = held(r#"replace string "TODO" with "DONE" expect 1"#);
        let structural = held("replace `TODO = $v` with `DONE = $v` expect 1");
        let both = held(
            "replace string \"TODO\" with \"DONE\" expect 1\nreplace `DONE = $v` with `DONE = ($v)` expect 1",
        );
        let deleted = held("delete `TODO = $v` expect 1");
        // A match nested in another's hole is rewritten inside the outer one's stretch.
        fs::write(dir.join("big.py"), text.clone() + "f(f(1))\n").unwrap();
        let nested = held("replace `f($x)` with `g($x)` expect 2");
        let _ = fs::remove_dir_all(&dir);
        // Each edit holds the bytes it wrote - "DONE", then the match rewritten whole - and none
        // of the thousands it left as they were.
        assert_eq!(textual, "DONE".len());
        assert_eq!(structural, "DONE = 2".len());
        assert_eq!(both, "DONE".len() + "DONE = (2)".len());
        // A deletion writes nothing over its line, and holds nothing.
        assert_eq!(deleted, 0);
        assert_eq!(nested, "g(g(1))".len());
    }

    /// What a run on a block of `n` statements, each followed by every later one to the block's
    /// end, holds until it prints, and how much it prints: the matches overlap, so what they
    /// cover is about the square of the block's length.
    fn held_and_printed(n: usize, script: &str) -> (usize, usize) {
        let mut block = String::new();
        for i in 0..n {
            std::fmt::Write::write_fmt(&mut block, format_args!("x = {i}\n"))
                .expect("a String takes any text");
        }
        block.push_str("end()\n");
        held_and_printed_in(&block, script)
    }

    /// What a run of `script` over a project holding one Python file, `text`, holds until it
    /// prints, by [`Searched::held_for_printing`], and how many bytes it prints.
    fn held_and_printed_in(text: &str, script: &str) -> (usize, usize) {
        // One project per call: tests run on several threads of one process at once.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("quaffed-run-block-{}-{call}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("block.py"), text).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let mut out = Vec::new();
        let outcome = run(
            vec![SourceArg::Expression {
                number: 1,
                text: script.to_owned(),
            }],
            None,
            &cli::Options {
                dry_run: true,
                ..cli::Options::default()
            },
            &dir,
            &mut io::empty(),
            &mut out,
            &mut Vec::new(),
        );
        let _ = fs::remove_dir_all(&dir);
        assert!(outcome.is_ok(), "{script}: {outcome:?}");
        (HELD.with(std::cell::Cell::get), out.len())
    }

    /// The matches a run holds, each a fixed size, and the files it holds grow with the code
    /// while what it prints grows with the square.  What this cannot see is memory a match owns,
    /// which `Hit` being `Copy` rules out where it is declared.
    #[test]
    fn the_matches_and_files_a_run_holds_grow_with_the_code_not_with_what_it_prints() {
        let block = |end: &str| format!("find `x = $v\n$mid...\n{end}` expect any");
        for script in [
            block("end()"),
            format!(
                "{}\nreplace `end()` with `finish()` expect 1",
                block("end()")
            ),
            format!(
                "replace `end()` with `finish()` expect 1\n{}",
                block("finish()")
            ),
            format!("{}\ndelete `end()` expect 1", block("end()")),
            format!(
                "replace string \"end()\" with \"done()\" expect 1\n{}\n\
                 replace `done()` with `end()` expect 1\n{}",
                block("done()"),
                block("end()")
            ),
        ] {
            let (held, printed) = held_and_printed(100, &script);
            let (held_twice, printed_twice) = held_and_printed(200, &script);
            // Twice the code prints about four times as much, and holds about twice as much.
            assert!(
                printed_twice > 3 * printed,
                "{script}: printed {printed} then {printed_twice}"
            );
            assert!(
                held_twice < 3 * held,
                "{script}: held {held} then {held_twice}"
            );
            assert!(held < printed, "{script}: held {held}, printed {printed}");
        }
    }

    #[test]
    fn a_script_in_the_scope_is_read_from_disk_once() {
        // `quaff -f scripts/fix.quaff` from the project's root: the script is read to run it,
        // and is in the scope, so searched as well.
        let dir = std::env::temp_dir().join(format!("quaffed-run-script-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/fix.quaff"), "find \"TODO\"\n").unwrap();
        fs::write(dir.join("a.txt"), "TODO\n").unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let script = dir.join("scripts/fix.quaff");
        crate::contents::DISK_READS.with(|reads| reads.borrow_mut().clear());
        let mut out = Vec::new();
        let outcome = run(
            vec![SourceArg::File(script.clone().into_os_string())],
            None,
            &cli::Options::default(),
            &dir,
            &mut io::empty(),
            &mut out,
            &mut Vec::new(),
        );
        let reads = crate::contents::DISK_READS.with(|reads| reads.borrow().clone());
        let _ = fs::remove_dir_all(&dir);
        assert!(outcome.is_ok(), "{outcome:?}");
        // The script holds "TODO" too, so it was searched as well as run.
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("fix.quaff"), "{out}");
        let count = reads.iter().filter(|read| **read == script).count();
        assert_eq!(count, 1, "the script read {count} times: {reads:?}");
    }

    #[test]
    fn a_file_without_matches_is_not_listed_for_a_pattern() {
        let dir = std::env::temp_dir().join(format!("quaffed-run-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creating the directory");
        let with = dir.join("with.txt");
        let without = dir.join("without.txt");
        fs::write(&with, "TODO\n").expect("writing");
        fs::write(&without, "nothing\n").expect("writing");
        let pattern = TextPattern::literal("TODO");
        let mut projects = Projects::new(&dir, &Contents::default());
        let file = |path| ToSearch {
            path,
            project: 0,
            applies: vec![true],
        };
        let searched = search_files(
            &[file(with), file(without)],
            scope::Roots {
                cwd: &dir,
                root: &dir,
            },
            &[Wanted::Text(&pattern)],
            &[None],
            &[],
            &mut projects,
            None,
        );
        let _ = fs::remove_dir_all(&dir);
        let searched = searched.expect("searching");
        let listed: Vec<&PathBuf> = searched.matches[0].iter().map(|(path, _)| path).collect();
        assert_eq!(listed, [&PathBuf::from("with.txt")]);
    }

    #[test]
    fn a_replace_or_a_delete_is_planned_and_one_this_build_cannot_run_is_named_where_it_was_written()
     {
        let span = |start| Span {
            source: 0,
            start,
            end: start + 1,
        };
        let text = |t: &str| Spanned::new(Operand::Text(TextPattern::literal(t)), span(0));
        let count = vec![Spanned::new(Count::Any, span(9))];
        let replace = |query: Query, expect: Vec<Spanned<Count>>| {
            Action::new(ActionKind::Replace(text("b")), query, expect)
        };
        let scoped = Query::with(
            text("a"),
            vec![Spanned::new(Glob::literal("x"), span(3))],
            None,
        );
        assert!(
            replacing(
                &replace(scoped, count.clone()),
                span(1),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .is_ok()
        );
        // A test of a metavariable needs what a match binds.
        let of_x = Spanned::new(
            Filter::Matches(
                Subject::Metavariable("x".into()),
                Spanned::new(Operand::Glob(Glob::literal("y")), span(7)),
            ),
            span(5),
        );
        let filter = Spanned::new(Filter::Not(Box::new(of_x)), span(4));
        let filtered = Query::with(text("a"), Vec::new(), Some(filter));
        // A test of a metavariable is planned like any other: the run checks it against the
        // pattern once that is compiled.
        assert!(
            replacing(
                &replace(filtered, count.clone()),
                span(1),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .is_ok()
        );
        // The checker refuses an edit with no `expect` first; the planner still names it.
        assert_eq!(
            replacing(
                &replace(Query::new(text("a")), Vec::new()),
                span(1),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .err(),
            Some(("a replace with no `expect`", span(1)))
        );
        // A delete is a structural replace with an empty rewrite, written where it was.
        let backticked = Spanned::new(Operand::Pattern("f()".into()), span(0));
        let delete = Action::new(ActionKind::Delete, Query::new(backticked), count.clone());
        let (mut patterns, mut rewrites) = (Vec::new(), Vec::new());
        assert!(replacing(&delete, span(1), &mut patterns, &mut rewrites).is_ok());
        assert_eq!(rewrites.len(), 1);
        assert_eq!((rewrites[0].0, rewrites[0].1), ("", span(1)));
        let pattern = Spanned::new(Operand::Pattern("f()".into()), span(0));
        let structural = Action::new(
            ActionKind::Replace(pattern.clone()),
            Query::new(pattern),
            count,
        );
        // A structural one is planned too, its pattern and its rewrite to be compiled.
        let (mut patterns, mut rewrites) = (Vec::new(), Vec::new());
        assert!(replacing(&structural, span(1), &mut patterns, &mut rewrites).is_ok());
        assert_eq!(patterns.len(), 1);
        assert_eq!(rewrites, [("f()", span(0), 0)]);
        // Text replaced with a pattern, or the other way round, the parser never builds.
        let mixed = Action::new(
            ActionKind::Replace(text("b")),
            Query::new(Spanned::new(Operand::Pattern("f()".into()), span(0))),
            vec![Spanned::new(Count::Any, span(9))],
        );
        assert_eq!(
            replacing(&mixed, span(1), &mut Vec::new(), &mut Vec::new()).err(),
            Some(("this kind of replace", span(1)))
        );
    }

    #[test]
    fn the_refusals_of_an_edit_are_worded_for_one_file_and_for_many() {
        let unwritable = |path: &str, ambiguous| Unwritable {
            path: path.into(),
            encoding: "mac_arabic",
            text: "a b".into(),
            ambiguous,
        };
        let one = Failure::Unwritable {
            files: vec![unwritable("a.txt", true)],
        }
        .to_string();
        assert!(
            one.contains("\"a.txt\": mac_arabic spells \"a b\" more than one way"),
            "{one}"
        );
        assert!(
            one.ends_with(
                "Use a replacement its encoding can hold, or leave it out of the statement: with \
                 `in`, with `where not FILE matches`, such as where not FILE matches \"a.txt\", \
                 or with a narrower scope on the command line"
            ),
            "{one}"
        );
        let many = Failure::Unwritable {
            files: vec![unwritable("a.txt", false), unwritable("b.txt", false)],
        }
        .to_string();
        assert!(many.contains("the encoding of 2 files"), "{many}");
        assert!(
            many.contains("a replacement their encodings can hold"),
            "{many}"
        );
        assert!(
            many.contains("or leave them out of the statement"),
            "{many}"
        );
        let broken = Failure::BrokenByEdit {
            files: vec![
                Broken {
                    path: "a.py".into(),
                    why: "line 1, column 1: x".into(),
                    by: vec![
                        "the replace at -e expression 1, at character 1".into(),
                        "the delete at -e expression 2, at character 1".into(),
                    ],
                },
                Broken {
                    path: "b.py".into(),
                    why: "line 2, column 1: y".into(),
                    by: Vec::new(),
                },
            ],
        };
        let message = broken.to_string();
        assert_eq!(broken.exit_code(), 9);
        assert!(
            message.contains("2 files that parse unparseable"),
            "{message}"
        );
        assert!(
            message.contains(
                "x; changed by the replace at -e expression 1, at character 1 and by the delete \
                 at -e expression 2, at character 1"
            ),
            "{message}"
        );
        assert!(
            message.contains("\"b.py\": line 2, column 1: y\n"),
            "{message}"
        );
        // One of the statements replaced, so it is the replacement that must change.
        assert!(
            message.contains("\nChange the replacement so that"),
            "{message}"
        );
        assert!(
            message.ends_with(
                "so that the files still parse, or leave them out of the statement: with `in`, \
                 with `where not FILE matches`, such as where not FILE matches \"a.py\", or \
                 with a narrower scope on the command line"
            ),
            "{message}"
        );
        for failure in [
            &broken,
            &Failure::Unwritable {
                files: vec![unwritable("a.txt", true), unwritable("b.txt", false)],
            },
        ] {
            refuses_every_write(failure);
        }
    }

    /// A destination refusing one write, the `refused`th from 0, and accepting every other, so
    /// that a message dropping the refusal goes on to succeed.
    struct Refusing {
        writes: usize,
        refused: Option<usize>,
    }

    impl fmt::Write for Refusing {
        fn write_str(&mut self, _: &str) -> fmt::Result {
            self.writes += 1;
            if Some(self.writes - 1) == self.refused {
                return Err(fmt::Error);
            }
            Ok(())
        }
    }

    /// Displays `failure` into destinations that refuse its first write, then its second, and so
    /// on to its last, and requires each refusal to end the message as an error, not be dropped.
    fn refuses_every_write(failure: &dyn fmt::Display) {
        let display = |refused| {
            let mut destination = Refusing { writes: 0, refused };
            fmt::Write::write_fmt(&mut destination, format_args!("{failure}"))
                .map(|()| destination.writes)
        };
        let writes = display(None).unwrap();
        assert!(writes > 1, "{failure} was written in {writes} writes");
        for refused in 0..writes {
            assert!(
                display(Some(refused)).is_err(),
                "a refusal of write {} of {writes} was dropped: {failure}",
                refused + 1
            );
        }
    }

    #[test]
    fn a_write_failure_exits_7_with_the_systems_error_on_the_chain() {
        use std::error::Error as _;
        let failure = Failure::Write(write::Failure::Open {
            path: "a.txt".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        });
        assert_eq!(failure.exit_code(), 7);
        let cause = failure
            .source()
            .and_then(|err| err.source())
            .map(ToString::to_string);
        assert_eq!(
            cause,
            Some(io::Error::from(io::ErrorKind::PermissionDenied).to_string())
        );
        refuses_every_write(&Failure::Write(write::Failure::Write {
            path: "a.txt".into(),
            cause: io::Error::from(io::ErrorKind::StorageFull),
            unrestored: vec![
                (
                    "a.txt".into(),
                    io::Error::from(io::ErrorKind::PermissionDenied),
                ),
                (
                    "b.txt".into(),
                    io::Error::from(io::ErrorKind::PermissionDenied),
                ),
            ],
            unflushed: Vec::new(),
        }));
    }

    #[test]
    fn counts_are_worded_for_one_and_for_many() {
        assert_eq!(count(1, "binary file", "binary files"), "1 binary file");
        assert_eq!(count(3, "binary file", "binary files"), "3 binary files");
    }

    /// What a run over two languages, Python and the test language Toy, gave: its outcome, what
    /// it printed, its notes, and each file's text afterwards.
    struct TwoLanguages {
        outcome: Result<Outcome, Failure>,
        out: String,
        notes: String,
        after: Vec<String>,
    }

    /// Runs `script` over `files`, each a path and its text, in a fresh directory, as a run that
    /// parses Python and Toy.
    fn in_two_languages(name: &str, script: &str, files: &[(&str, &str)]) -> TwoLanguages {
        use crate::languages::{PYTHON, TOY};
        in_languages(&[PYTHON, TOY], name, script, files)
    }

    /// Runs `script` over `files`, each a path and its text, in a fresh directory, as a run that
    /// parses `languages`.
    fn in_languages(
        languages: &[&'static dyn FrontEnd],
        name: &str,
        script: &str,
        files: &[(&str, &str)],
    ) -> TwoLanguages {
        let dir = std::env::temp_dir().join(format!(
            "quaffed-two-languages-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        for (path, text) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let dir = fs::canonicalize(&dir).unwrap();
        let (mut out, mut notes) = (Vec::new(), Vec::new());
        let outcome = run_in(
            languages,
            vec![SourceArg::Expression {
                number: 1,
                text: script.to_owned(),
            }],
            None,
            &cli::Options::default(),
            &dir,
            &mut io::empty(),
            &mut out,
            &mut notes,
        );
        let after = files
            .iter()
            .map(|(path, _)| fs::read_to_string(dir.join(path)).unwrap())
            .collect();
        let _ = fs::remove_dir_all(&dir);
        TwoLanguages {
            outcome,
            out: String::from_utf8(out).unwrap(),
            notes: String::from_utf8(notes).unwrap(),
            after,
        }
    }

    #[test]
    fn a_binary_file_is_never_searched_whatever_its_language() {
        let ran = in_two_languages(
            "binary",
            "find `f($x)`",
            &[
                ("a.py", "f(1)\n"),
                ("b.toy", "f(\0)\n"),
                ("c.bin", "f(\0)\n"),
            ],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert!(
            !ran.out.contains("b.toy") && !ran.out.contains("c.bin"),
            "{}",
            ran.out
        );
    }

    #[test]
    fn a_rewrite_both_languages_read_edits_each_language_s_files() {
        let ran = in_two_languages(
            "rewrite-both",
            "replace `f($x)` with `g($x)` expect 2",
            &[("a.py", "f(1)\n"), ("b.toy", "f(2)\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert_eq!(ran.after, ["g(1)\n", "g(2)\n"]);
    }

    #[test]
    fn a_file_its_language_cannot_decode_or_parse_refuses_a_replace() {
        // Toy decodes as Python does, so an encoding Python does not know is refused.
        for (name, script, text) in [
            (
                "undecoded",
                "replace `f($x)` with `g($x)` expect 1",
                "# coding: nonsense\nf(1)\n",
            ),
            (
                "unwritten-unparsed",
                "replace `f($x)` with `lambda: $x` expect 1",
                "f(1\n",
            ),
        ] {
            let ran = in_two_languages(name, script, &[("a.py", "f(1)\n"), ("b.toy", text)]);
            assert!(
                matches!(&ran.outcome, Err(Failure::Unparseable { files }) if files.len() == 1),
                "{name}: {:?}",
                ran.outcome
            );
            assert_eq!(ran.after[0], "f(1)\n", "{name}: nothing is written");
        }
    }

    #[test]
    fn a_pattern_both_languages_read_is_matched_in_each_language_s_files() {
        let ran = in_two_languages(
            "both",
            "find `f($x)`",
            &[("a.py", "f(1)\n"), ("b.toy", "f(2)\n"), ("c.txt", "f(3)\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert!(
            ran.out.contains("a.py") && ran.out.contains("b.toy"),
            "{}",
            ran.out
        );
        assert!(!ran.out.contains("c.txt"), "{}", ran.out);
    }

    #[test]
    fn a_file_whose_language_cannot_read_the_pattern_is_not_applicable_and_counted() {
        let ran = in_two_languages(
            "one-reads",
            "find `g = lambda: f($x)`",
            &[
                ("a.py", "g = lambda: f(1)\n"),
                ("b.toy", "g = lambda: f(2)\n"),
            ],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert!(ran.out.contains("a.py"), "{}", ran.out);
        assert!(!ran.out.contains("b.toy"), "{}", ran.out);
        assert!(
            ran.notes
                .contains("1 file not searched for a structural pattern its language cannot read"),
            "{}",
            ran.notes
        );
        let ran = in_two_languages(
            "one-reads-of-two",
            "find `g = lambda: f($x)`",
            &[
                ("a.py", "g = lambda: f(1)\n"),
                ("b.toy", "g = lambda: f(2)\n"),
                ("c.toy", "g = lambda: f(3)\n"),
            ],
        );
        assert!(
            ran.notes.contains(
                "2 files not searched for structural patterns their language cannot read"
            ),
            "{}",
            ran.notes
        );
    }

    #[test]
    fn a_pattern_no_file_s_language_reads_says_so_rather_than_nothing_found() {
        let ran = in_two_languages(
            "unread-only",
            "find `g = lambda: f($x)`",
            &[("b.toy", "g = lambda: f(2)\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::NothingFound)),
            "{:?}",
            ran.outcome
        );
        assert!(
            ran.notes
                .contains("no file in scope is in a language that reads it; 1 file not searched"),
            "{}",
            ran.notes
        );
        let ran = in_two_languages(
            "unread-and-text",
            "find `g = lambda: f($x)`",
            &[("b.toy", "g = lambda: f(2)\n"), ("c.txt", "g\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::NothingFound)),
            "{:?}",
            ran.outcome
        );
        assert!(
            ran.notes.contains(
                "no file in scope is in a language that reads it; 1 file in another language \
                 and 1 text-tier file not searched"
            ),
            "{}",
            ran.notes
        );
    }

    #[test]
    fn a_pattern_no_language_reads_is_refused_with_what_each_said() {
        let ran = in_two_languages("none-reads", "find `lambda f(`", &[("a.py", "x\n")]);
        let Err(Failure::Script { message, .. }) = &ran.outcome else {
            panic!("{:?}", ran.outcome);
        };
        assert!(message.starts_with("as Python: "), "{message}");
        assert!(
            message.contains("; as Toy: `lambda` is not Toy"),
            "{message}"
        );
    }

    #[test]
    fn a_rewrite_one_language_cannot_write_refuses_the_run_where_it_matches_there() {
        let files = [("a.py", "f(1)\n"), ("b.toy", "f(2)\n")];
        let ran = in_two_languages(
            "unwritten",
            "replace `f($x)` with `lambda: $x` expect 2",
            &files,
        );
        let Err(Failure::EditRefused { files: refused, .. }) = &ran.outcome else {
            panic!("{:?}", ran.outcome);
        };
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].0.ends_with("b.toy"), "{refused:?}");
        assert!(
            refused[0].1.contains(
                "line 1, column 1: the rewrite cannot be written in Toy: `lambda` is not Toy"
            ),
            "{refused:?}"
        );
        assert_eq!(ran.after, ["f(1)\n", "f(2)\n"], "nothing is written");
        // Where no file of the language that cannot write it matches, the edit is made.
        let ran = in_two_languages(
            "unwritten-unmatched",
            "replace `f($x)` with `lambda: $x` expect 1",
            &[("a.py", "f(1)\n"), ("b.toy", "g(2)\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert_eq!(ran.after, ["lambda: 1\n", "g(2)\n"]);
    }

    #[test]
    fn a_rewrite_no_language_that_reads_its_pattern_can_write_is_refused() {
        let ran = in_two_languages(
            "rewrite-none",
            "replace `f($x)` with `lambda $x:` expect 1",
            &[("a.py", "f(1)\n")],
        );
        let Err(Failure::Script { message, .. }) = &ran.outcome else {
            panic!("{:?}", ran.outcome);
        };
        assert!(message.starts_with("as Python: "), "{message}");
        assert!(message.contains("; as Toy: "), "{message}");
        assert_eq!(ran.after, ["f(1)\n"]);
    }

    #[test]
    fn a_where_one_language_cannot_read_leaves_that_language_s_files_not_applicable() {
        let ran = in_two_languages(
            "where",
            "find `g = $v` where $v matches `lambda: $b`",
            &[("a.py", "g = lambda: 1\n"), ("b.toy", "g = lambda: 2\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert!(ran.out.contains("a.py"), "{}", ran.out);
        assert!(!ran.out.contains("b.toy"), "{}", ran.out);
        // Nor is a `where` compiled for a language that does not read the statement's pattern.
        let ran = in_two_languages(
            "where-unread-pattern",
            "find `g = lambda: $b` where $b matches `1`",
            &[("a.py", "g = lambda: 1\n"), ("b.toy", "g = lambda: 1\n")],
        );
        assert!(
            matches!(ran.outcome, Ok(Outcome::Success)),
            "{:?}",
            ran.outcome
        );
        assert!(
            ran.out.contains("a.py") && !ran.out.contains("b.toy"),
            "{}",
            ran.out
        );
    }

    #[test]
    fn a_where_no_language_reading_the_pattern_can_read_is_refused() {
        let ran = in_two_languages(
            "where-none",
            "find `f($x)` where $x matches `g(`",
            &[("a.py", "f(1)\n")],
        );
        assert!(
            matches!(
                &ran.outcome,
                Err(Failure::Script { message, .. }) if !message.contains("as Toy")
            ),
            "the first language's error alone: {:?}",
            ran.outcome
        );
    }

    #[test]
    fn a_rewrite_is_checked_again_once_a_where_leaves_its_pattern_unread() {
        use crate::languages::{PLAIN, TOY};
        // Toy reads the pattern and the `where` but not the rewrite; Plain reads the pattern and
        // the rewrite but not the `where`.  Once the `where` leaves Plain out, no language that
        // reads the pattern reads the rewrite, so the run stops before reading any file.
        let ran = in_languages(
            &[TOY, PLAIN],
            "rewrite-after-where",
            "replace `f($x)` with `lambda: $x` where $x matches `yielded` expect 1",
            &[("a.toy", "f(yielded)\n"), ("b.plain", "f(yielded)\n")],
        );
        assert!(
            matches!(
                &ran.outcome,
                Err(Failure::Script { message, .. }) if message.contains("`lambda` is not Toy")
            ),
            "{:?}",
            ran.outcome
        );
        assert_eq!(ran.after, ["f(yielded)\n", "f(yielded)\n"]);
    }

    #[test]
    fn a_file_unparseable_for_a_search_and_an_unwritable_rewrite_is_named_once() {
        let ran = in_two_languages(
            "unparsed-once",
            "find `f($x)`\nreplace `f($x)` with `lambda: $x` expect any",
            &[("a.py", "f(1)\n"), ("b.toy", "f(2\n")],
        );
        assert!(
            matches!(&ran.outcome, Err(Failure::Unparseable { files }) if files.len() == 1),
            "{:?}",
            ran.outcome
        );
    }
}
