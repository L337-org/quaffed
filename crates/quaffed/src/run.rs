// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Carrying out a run: the program its sources make, the scope, the files in it, the matches,
//! the assertions, and what was not looked at.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use quaffed_encoding::{self as encoding, Encoding, Named, Refusal, python};
use quaffed_python::compile_pattern;
use quaffed_representation::check;
use quaffed_representation::program::{
    Action, ActionKind, Count, Counted, Expectation, Operand, Program, Query, Statement, TextPart,
    TextPattern,
};
use quaffed_representation::span::{Source, Span, Spanned};
use quaffed_script::{ErrorKind, lexer, location, parse_source};
use quaffed_syntax::Pattern;

use crate::cli::{self, SourceArg, UsageError};
use crate::contents::Contents;
use crate::discover;
use crate::editorconfig::EditorConfig;
use crate::output;
use crate::resolve::{self, Declarations, Resolution, Unknown};
use crate::search::{self, Match};
use crate::structural;
use crate::workspace::{self, Workspace};
use crate::write;

/// Why a run could not give an answer.  Each kind maps to one exit code, because each asks the
/// caller for a different action.
#[derive(Debug)]
pub enum Failure {
    /// The command line cannot be acted on: fix it.  Exit 3.
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
    /// The scope lies outside the project, and a command-line scope narrows, never widens.
    /// Exit 3.
    OutsideProject {
        /// The scope as given.
        scope: OsString,
        /// The project root it lies outside.
        root: PathBuf,
    },
    /// With no project, the scope lies outside the current directory, which is then all a
    /// search covers; a scope narrows, never widens.  Exit 3.
    OutsideCurrentDirectory {
        /// The scope as given.
        scope: OsString,
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
    /// A structural search needs Python files quaff cannot parse - ones that do not parse, and
    /// ones in an encoding Python reads and quaff does not - so the run is refused: a count that
    /// left them out could not be trusted.  Exit 9.
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
    /// The edits would leave Python files that parsed unparseable, so the run is refused and
    /// nothing written.  Exit 9.
    BrokenByEdit {
        /// Each such file, as the user would recognise it, and where and why it would not parse.
        files: Vec<(PathBuf, String)>,
    },
    /// The edits could not all be written.  Exit 7.
    Write(write::Failure),
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
            Failure::Usage(_)
            | Failure::Script { .. }
            | Failure::Empty { .. }
            | Failure::OutsideProject { .. }
            | Failure::OutsideCurrentDirectory { .. } => 3,
            Failure::Unknown { .. } => 5,
            Failure::Io { .. } | Failure::Discovery(_) | Failure::Write(_) => 7,
            Failure::Unparseable { .. }
            | Failure::Unwritable { .. }
            | Failure::BrokenByEdit { .. } => 9,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Usage(err) => err.fmt(f),
            Failure::Script { location, message } | Failure::Unknown { location, message } => {
                write!(f, "{location}: {message}")
            }
            Failure::Empty { sources } => write!(
                f,
                "nothing to run: {} {} no statements, only blank lines and comments",
                sources.join(", "),
                if sources.len() == 1 { "has" } else { "have" }
            ),
            Failure::OutsideProject { scope, root } => write!(
                f,
                "the scope {scope:?} is outside the project at {root:?}, and a scope can only \
                 narrow a search; run quaff from inside the other project instead"
            ),
            Failure::OutsideCurrentDirectory { scope } => write!(
                f,
                "the scope {scope:?} is outside the current directory; with no project - no .git \
                 here or above - a search covers only this directory, and a scope can only \
                 narrow it.  Run quaff from the directory you want searched"
            ),
            Failure::Io { doing, path, cause } => {
                write!(f, "{doing} {path:?}: {cause}")?;
                let looks_like_a_glob = path.to_string_lossy().contains(['*', '?', '[']);
                if cause.kind() == io::ErrorKind::NotFound && looks_like_a_glob {
                    write!(
                        f,
                        ".  quaff does not expand a glob given as the scope; name one file or \
                         directory"
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
                    count(files.len(), "Python file", "Python files"),
                    if files.len() == 1 { "it" } else { "them" }
                )?;
                for (path, why) in files {
                    write!(f, "\n  {path:?}: {why}")?;
                }
                write!(
                    f,
                    "\nTo search without {}, list {} in .ignore, or give a narrower scope on \
                     the command line",
                    if files.len() == 1 { "it" } else { "them" },
                    if files.len() == 1 { "it" } else { "them" }
                )
            }
            Failure::Unwritable { files } => {
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
                Ok(())
            }
            Failure::BrokenByEdit { files } => {
                write!(
                    f,
                    "refused: the edit would leave {} that {} unparseable, so nothing was written:",
                    count(files.len(), "Python file", "Python files"),
                    if files.len() == 1 { "parses" } else { "parse" }
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
            | Failure::OutsideProject { .. }
            | Failure::OutsideCurrentDirectory { .. }
            | Failure::Unparseable { .. }
            | Failure::Unwritable { .. }
            | Failure::BrokenByEdit { .. } => None,
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
/// Every source is read and parsed, and the whole program checked, before any file is looked
/// at, so a script that cannot run in full does not run at all.  The search covers the
/// project - found by walking up from `cwd` - or `cwd` when there is no project, narrowed to
/// `scope` when one is given.  Every file is read once, whatever the number of statements; the
/// matches print statement by statement, in program order.  An assertion block is evaluated
/// to the end, and if any of its assertions does not hold, each one that failed is reported
/// and the run stops there.
///
/// # Errors
///
/// Returns a [`Failure`] when the run cannot be answered in full: a script that is malformed
/// or uses what this build does not have, a scope outside the project, or something that
/// cannot be read.  Nothing is printed for a run that fails, so a partial answer never passes
/// for a whole one.
pub fn run(
    sources: Vec<SourceArg>,
    scope: Option<&OsString>,
    dry_run: bool,
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
    let (steps, structural) =
        plan(&built.program).map_err(|(construct, span)| Failure::Unknown {
            location: built.locate(span, span.start),
            message: format!("{construct} is not built yet"),
        })?;
    // Every structural pattern is compiled before any file is read, so one that cannot be
    // compiled stops the run before it looks at anything.
    let compiled: Vec<Pattern> = structural
        .iter()
        .map(|(written, span)| {
            compile_pattern(written).map_err(|err| Failure::Script {
                location: built.locate(
                    *span,
                    err.offset()
                        .map_or(span.start, |at| built.content_start(*span) + at),
                ),
                message: err.to_string(),
            })
        })
        .collect::<Result<_, _>>()?;

    let cwd = fs::canonicalize(cwd).map_err(|cause| Failure::Io {
        doing: "reading the current directory",
        path: cwd.to_path_buf(),
        cause,
    })?;
    let workspace = workspace::find(&cwd, &contents);
    let scope = resolve_scope(
        built.program.narrow_to.as_deref(),
        &cwd,
        workspace.as_deref(),
    )?;
    let found = discover::files(&scope, &cwd, workspace.as_deref(), &contents)
        .map_err(Failure::Discovery)?;

    let searching: Vec<Searching> = steps.iter().flat_map(Step::patterns).collect();
    let patterns: Vec<Wanted> = searching.iter().map(|s| s.pattern).collect();
    let replacements: Vec<Option<&TextPattern>> =
        steps.iter().flat_map(Step::replacements).collect();
    // EditorConfig is read no higher than the project root, or where quaff runs with no
    // project, so that nothing outside the project changes an answer.
    let boundary = workspace
        .as_ref()
        .map_or_else(|| cwd.clone(), |w| w.root().to_path_buf());
    let mut declarations = Declarations {
        workspace,
        editorconfig: EditorConfig::new(&boundary, &contents),
        cwd: cwd.clone(),
        contents,
    };
    let locale = locale();
    let searched = search_files(
        &found.files,
        &cwd,
        &patterns,
        &replacements,
        &compiled,
        &mut declarations,
        locale.as_ref(),
    )?;
    // A structural search over files it cannot parse would give a count that cannot be trusted,
    // whether Python could parse them or not, so the whole run is refused, naming each, before
    // anything is printed.
    if !searched.unparsed.is_empty() {
        return Err(Failure::Unparseable {
            files: searched.unparsed.clone(),
        });
    }
    let (outcome, evaluation) = conclude(&searched, &steps, &built, dry_run, out)?;

    // Notes are best effort: a run that cannot write to standard error has nowhere to say so.
    let mut note = |text: &str| {
        let _ = writeln!(notes, "quaff: {text}");
    };
    for text in &evaluation.notes {
        note(text);
    }
    let declared_skipped = declarations
        .workspace
        .as_ref()
        .map_or(&[][..], |w| w.skipped());
    for text in skipped(
        &searched,
        &searching[..evaluation.next],
        &found,
        declared_skipped,
    ) {
        note(&text);
    }
    Ok(outcome)
}

/// Decides the run's outcome from what the search found, writes its edits - unless it is a dry
/// run - and prints its results, in that order.
///
/// The outcome is decided before anything is printed or written, so that a replace whose
/// replacement a file cannot hold, an edit that breaks a Python file, or a file that cannot be
/// written refuses the whole run with nothing written and nothing printed: a run that gives no
/// answer prints nothing.  An assertion that fails is an answer, so its results print, and nothing
/// is written.
fn conclude<'s>(
    searched: &'s Searched,
    steps: &[Step],
    built: &Built,
    dry_run: bool,
    out: &mut impl Write,
) -> Result<(Outcome, Evaluation<'s>), Failure> {
    let verdict = Evaluation::new(searched, false).steps(steps, built, &mut io::sink())?;
    if verdict == Outcome::Success {
        refuse_broken(&searched.changed)?;
        if !dry_run {
            let edits: Vec<write::Edit> = searched
                .changed
                .iter()
                .map(|file| write::Edit {
                    path: &file.path,
                    shown: &file.shown,
                    original: &file.original,
                    edited: &file.edited,
                })
                .collect();
            write::commit(&edits).map_err(Failure::Write)?;
        }
    }
    let mut evaluation = Evaluation::new(searched, true);
    let outcome = evaluation.steps(steps, built, out)?;
    if outcome == Outcome::Success && evaluation.replaced > 0 {
        let files = count(evaluation.replaced_in.len(), "file", "files");
        let matches = count(evaluation.replaced, "match", "matches");
        evaluation.notes.push(if dry_run {
            format!("dry run: {matches} in {files} would be replaced; nothing was written")
        } else {
            format!("replaced {matches} in {files}")
        });
    }
    Ok((outcome, evaluation))
}

/// Refuses edits that would leave a Python file unparseable that parsed before them.  A file
/// that did not parse before is not refused: the check protects a parse that existed, and a
/// textual edit needs no parse tree.
fn refuse_broken(changed: &[Changed]) -> Result<(), Failure> {
    let broken: Vec<(PathBuf, String)> = changed
        .iter()
        .filter(|file| file.python && parses(&file.original).is_ok())
        .filter_map(|file| {
            parses(&file.edited)
                .err()
                .map(|why| (file.shown.clone(), why))
        })
        .collect();
    if broken.is_empty() {
        Ok(())
    } else {
        Err(Failure::BrokenByEdit { files: broken })
    }
}

/// Whether a Python file's bytes parse, decoded as `CPython` decodes them; where and why not.
fn parses(bytes: &[u8]) -> Result<(), String> {
    let source = python::Source::decode(bytes).map_err(|refusal| refusal.to_string())?;
    structural::search(&source.text, &[])
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

/// One step of a program this build can run: every query in it is unscoped.
enum Step<'p> {
    /// Search, and print what was found.
    Find(Wanted<'p>),
    /// Search for each, print what was found, and check each count.
    Assert(Vec<Assertion<'p>>),
    /// Replace text, print what was replaced, and check its count.
    Replace(Replacing<'p>),
}

/// A textual replace this build can run.
struct Replacing<'p> {
    /// What it replaces.
    pattern: &'p TextPattern,
    /// What it writes in its place.
    with: &'p TextPattern,
    /// The count its `expect` requires.
    count: Count,
    /// Where the statement was written.
    span: Span,
}

/// A program's steps, and each structural pattern in it with where it was written.
type Planned<'p> = (Vec<Step<'p>>, Vec<(&'p str, Span)>);

/// What a query searches for: text, or a structural pattern.
#[derive(Clone, Copy)]
enum Wanted<'p> {
    /// A textual pattern, matched in every text file.
    Text(&'p TextPattern),
    /// A structural pattern, matched in every Python file: as written, and where its compiled
    /// form is in the run's list of them.
    Structural { written: &'p str, compiled: usize },
}

/// One assertion in a block this build can run.
struct Assertion<'p> {
    pattern: Wanted<'p>,
    count: Count,
    span: Span,
}

/// One pattern the run searches for.
#[derive(Clone, Copy)]
struct Searching<'p> {
    pattern: Wanted<'p>,
}

impl Step<'_> {
    /// The step's patterns in order.  One list serves the search and the notes, so they cannot
    /// disagree about which pattern is which.
    fn patterns(&self) -> Vec<Searching<'_>> {
        match self {
            Step::Find(pattern) => vec![Searching { pattern: *pattern }],
            Step::Assert(block) => block
                .iter()
                .map(|a| Searching { pattern: a.pattern })
                .collect(),
            Step::Replace(replacing) => vec![Searching {
                pattern: Wanted::Text(replacing.pattern),
            }],
        }
    }

    /// The replacement each of the step's patterns is replaced with, in the order
    /// [`Step::patterns`] lists them: `None` for a pattern only searched for.
    fn replacements(&self) -> Vec<Option<&TextPattern>> {
        match self {
            Step::Find(_) => vec![None],
            Step::Assert(block) => block.iter().map(|_| None).collect(),
            Step::Replace(replacing) => vec![Some(replacing.with)],
        }
    }
}

/// The steps `program` runs as, and each structural pattern in it in order with where it was
/// written; or the first construct in it that is in the MVP but not built yet, with where it was
/// written.
fn plan(program: &Program) -> Result<Planned<'_>, (&'static str, Span)> {
    let mut steps = Vec::new();
    let mut structural = Vec::new();
    for statement in program.body() {
        steps.push(match &statement.node {
            Statement::Find(query) => Step::Find(buildable(query, &mut structural)?),
            Statement::Assert(block) => Step::Assert(
                block
                    .iter()
                    .map(|a| assertion(a, &mut structural))
                    .collect::<Result<_, _>>()?,
            ),
            Statement::Act(action) => Step::Replace(replacing(action, statement.span)?),
            _ => return Err(("this statement", statement.span)),
        });
    }
    Ok((steps, structural))
}

/// The textual replace `action` is, if this build can run it.
fn replacing(action: &Action, span: Span) -> Result<Replacing<'_>, (&'static str, Span)> {
    let ActionKind::Replace(with) = &action.kind else {
        return Err(("delete", span));
    };
    let (Operand::Text(pattern), Operand::Text(with)) = (&action.query.operand.node, &with.node)
    else {
        return Err(("a structural replace", span));
    };
    if let Some(glob) = action.query.scope.first() {
        return Err(("an `in` clause", glob.span));
    }
    if let Some(filter) = &action.query.filter {
        return Err(("a `where` clause", filter.span));
    }
    // The checker has refused an edit with no `expect`, and the parser takes at most one.
    let Some(count) = action.expect.first() else {
        return Err(("a replace with no `expect`", span));
    };
    Ok(Replacing {
        pattern,
        with,
        count: count.node,
        span,
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
    })
}

/// The pattern of `query`, if this build can search for it.  A structural one joins
/// `structural`, to be compiled with the rest before any file is read.
fn buildable<'p>(
    query: &'p Query,
    structural: &mut Vec<(&'p str, Span)>,
) -> Result<Wanted<'p>, (&'static str, Span)> {
    if let Some(glob) = query.scope.first() {
        return Err(("an `in` clause", glob.span));
    }
    if let Some(filter) = &query.filter {
        return Err(("a `where` clause", filter.span));
    }
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

/// The directory or file a run covers: the project, or `cwd` with no project, narrowed to
/// `scope`.
fn resolve_scope(
    scope: Option<&Path>,
    cwd: &Path,
    workspace: Option<&dyn Workspace>,
) -> Result<PathBuf, Failure> {
    let root = workspace.map(|w| w.root().to_path_buf());
    let Some(given) = scope else {
        return Ok(root.unwrap_or_else(|| cwd.to_path_buf()));
    };
    let scope = fs::canonicalize(cwd.join(given)).map_err(|cause| Failure::Io {
        doing: "reading the scope",
        path: given.to_path_buf(),
        cause,
    })?;
    match &root {
        Some(root) if !scope.starts_with(root) => Err(Failure::OutsideProject {
            scope: given.as_os_str().to_owned(),
            root: output::relative(root, cwd),
        }),
        // With no project the current directory is the whole search, so it bounds the scope as
        // a project root would.
        None if !scope.starts_with(cwd) => Err(Failure::OutsideCurrentDirectory {
            scope: given.as_os_str().to_owned(),
        }),
        _ => Ok(scope),
    }
}

/// One match a search found, and for a structural one the code it covers, which is printed in
/// place of the pattern's text.
struct Hit {
    at: Match,
    text: Option<String>,
}

/// What searching every file for every pattern found.
struct Searched {
    /// For each pattern, the files with matches, in discovery order.
    matches: Vec<Vec<(PathBuf, Vec<Hit>)>>,
    /// Python files a structural search needs that quaff cannot parse, by the paths a message
    /// shows, with where and why.
    unparsed: Vec<(PathBuf, String)>,
    /// Python files in scope, binary ones included, when a structural pattern is wanted.
    python: usize,
    /// Python files searched for the structural patterns.
    structural: usize,
    /// Other files, which have no structure quaff reads, so no structural pattern applies.
    not_applicable: usize,
    /// For each pattern, the files of unknown encoding holding bytes it would match, by the
    /// paths a message shows, with the lines they are on and why the encoding is unknown.  Never
    /// counted, and never edited.
    possible: Vec<Vec<(PathBuf, Vec<usize>, Unknown)>>,
    /// For each pattern, the files not searched for it because it has no one spelling there,
    /// by the paths a message shows, and why.
    unspelled: Vec<Vec<(PathBuf, Unspelled)>>,
    /// For each replaced pattern, the files whose encoding cannot write its replacement, by the
    /// paths a message shows, with the encoding, the text it cannot write, and whether it spells
    /// that text more than one way rather than not at all.
    unwritable: Vec<Vec<(PathBuf, Encoding, String, bool)>>,
    /// Every file a replace changed, with its bytes before and after, held until the run ends.
    changed: Vec<Changed>,
    /// Python files `CPython` refuses to read whatever they hold, so are searched for nothing, by
    /// the paths a message shows, with why.
    refused: Vec<(PathBuf, Refusal)>,
    /// Files of unknown encoding that read cleanly in the locale's encoding, which a message
    /// mentions as a hint, and that encoding's name as the locale spells it.
    hinted: Vec<PathBuf>,
    hint: Option<String>,
    /// Where the project's version control system declares encodings, for the hint.
    declares_in: Option<&'static str>,
    /// Binary files, counted once whatever the number of patterns.
    binary: usize,
}

/// A file a replace changed, held in memory until the run is over.
struct Changed {
    /// Where it is, to write it.
    path: PathBuf,
    /// The path a message shows.
    shown: PathBuf,
    /// Whether it is a Python file, which must still parse if it parsed before.
    python: bool,
    /// Its bytes as they were read, to write back if writing fails.
    original: crate::contents::Bytes,
    /// Its bytes after every replace.
    edited: Vec<u8>,
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
}

/// `patterns` in batches, `replacements` naming each one's replacement, if it is replaced.
fn batches<'p>(
    patterns: &[Wanted<'p>],
    replacements: &[Option<&'p TextPattern>],
) -> Vec<Batch<'p>> {
    let mut batches = Vec::new();
    let mut textual = Vec::new();
    let mut structural = Vec::new();
    for (index, (wanted, with)) in patterns.iter().zip(replacements).enumerate() {
        match (wanted, with) {
            (Wanted::Text(pattern), Some(with)) => {
                if !textual.is_empty() || !structural.is_empty() {
                    batches.push(Batch::Search {
                        textual: std::mem::take(&mut textual),
                        structural: std::mem::take(&mut structural),
                    });
                }
                batches.push(Batch::Replace {
                    index,
                    pattern,
                    with,
                });
            }
            (Wanted::Text(pattern), None) => textual.push((index, *pattern)),
            (Wanted::Structural { compiled, .. }, _) => structural.push((index, *compiled)),
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

/// What a file is searched for: the batches of patterns, the compiled structural patterns, and
/// whether there is any.
struct Plan<'p> {
    batches: Vec<Batch<'p>>,
    compiled: &'p [Pattern],
    any_structural: bool,
}

/// Searches each of `files` for each of `patterns`, in order, replacing as `replacements` says,
/// reading each file once and deciding its encoding from what declares it.  A pattern after a
/// replace is searched for in the file as the replace left it; the edits are held, not written.
fn search_files(
    files: &[PathBuf],
    cwd: &Path,
    patterns: &[Wanted],
    replacements: &[Option<&TextPattern>],
    compiled: &[Pattern],
    declarations: &mut Declarations,
    locale: Option<&(String, Encoding)>,
) -> Result<Searched, Failure> {
    let batches = batches(patterns, replacements);
    let any_structural = patterns
        .iter()
        .any(|wanted| matches!(wanted, Wanted::Structural { .. }));
    let mut searched = Searched {
        matches: (0..patterns.len()).map(|_| Vec::new()).collect(),
        unparsed: Vec::new(),
        python: 0,
        structural: 0,
        not_applicable: 0,
        possible: vec![Vec::new(); patterns.len()],
        unspelled: vec![Vec::new(); patterns.len()],
        unwritable: vec![Vec::new(); patterns.len()],
        changed: Vec::new(),
        refused: Vec::new(),
        hinted: Vec::new(),
        hint: locale.map(|(name, _)| name.clone()),
        declares_in: declarations
            .workspace
            .as_ref()
            .map(|w| w.declares_encodings_in()),
        binary: 0,
    };
    let plan = Plan {
        batches,
        compiled,
        any_structural,
    };
    for file in files {
        searched.file(file, cwd, &plan, declarations, locale)?;
    }
    Ok(searched)
}

impl Searched {
    /// Searches one file for the run's patterns, batch by batch, holding its edits.
    fn file(
        &mut self,
        file: &Path,
        cwd: &Path,
        plan: &Plan,
        declarations: &mut Declarations,
        locale: Option<&(String, Encoding)>,
    ) -> Result<(), Failure> {
        let shown = output::relative(file, cwd);
        let bytes = declarations
            .contents
            .read(file)
            .map_err(|cause| Failure::Io {
                doing: "reading",
                path: shown.clone(),
                cause,
            })?;
        // Decided once, from the file as it is on disk, whatever the number of patterns.
        let resolution = resolve::resolve(
            &resolve::File {
                path: file,
                bytes: &bytes,
            },
            declarations,
        )
        .map_err(|err| Failure::Io {
            doing: "reading the encoding declarations in",
            path: output::relative(&err.path, cwd),
            cause: err.cause,
        })?;
        let binary = matches!(resolution, Resolution::Binary);
        let python = resolve::is_python(file);
        if plan.any_structural {
            if python {
                // A binary one too: it is in scope, so the run does not say there is no Python,
                // though it is counted with the binary files rather than searched.
                self.python += 1;
            } else if !binary {
                self.not_applicable += 1;
            }
        }
        // The file as the replaces so far have left it, once one has changed it.
        let mut edited: Option<Vec<u8>> = None;
        let mut parsed = None;
        for batch in &plan.batches {
            let text: &[u8] = edited.as_deref().unwrap_or(&bytes);
            match batch {
                Batch::Search {
                    textual,
                    structural,
                } => {
                    if !structural.is_empty() && python && !binary {
                        self.structural_batch(&shown, text, structural, plan, &mut parsed);
                    }
                    match &resolution {
                        Resolution::Known { encoding, body, .. } => {
                            self.known(&shown, &text[*body..], *encoding, textual);
                        }
                        Resolution::Unknown(reason) => {
                            self.unknown(&shown, text, reason, textual);
                        }
                        Resolution::Binary | Resolution::Refused(_) => {}
                    }
                }
                Batch::Replace {
                    index,
                    pattern,
                    with,
                } => match &resolution {
                    Resolution::Known { encoding, body, .. } => {
                        let replaced = search::replace(&text[*body..], *encoding, pattern, with);
                        if let Some(text) = self.replaced(&shown, *index, *encoding, replaced) {
                            let mut whole = edited.as_deref().unwrap_or(&bytes)[..*body].to_vec();
                            whole.extend_from_slice(&text);
                            edited = Some(whole);
                        }
                    }
                    // Never edited: its matches are only possible, and reported as in a search.
                    Resolution::Unknown(reason) => {
                        self.unknown(&shown, text, reason, &[(*index, *pattern)]);
                    }
                    Resolution::Binary | Resolution::Refused(_) => {}
                },
            }
        }
        match resolution {
            Resolution::Binary => self.binary += 1,
            Resolution::Refused(refusal) => self.refused.push((shown.clone(), refusal)),
            Resolution::Unknown(reason) => {
                if reason == Unknown::NotUtf8
                    && locale.is_some_and(|(_, encoding)| encoding.reads_cleanly(&bytes))
                {
                    self.hinted.push(shown.clone());
                }
            }
            Resolution::Known { .. } => {}
        }
        if let Some(edited) = edited.filter(|edited| edited[..] != bytes[..]) {
            self.changed.push(Changed {
                path: file.to_path_buf(),
                shown,
                python,
                original: bytes,
                edited,
            });
        }
        Ok(())
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
                        .map(|at| Hit { at, text: None })
                        .collect(),
                )),
            }
        }
    }

    /// Searches a Python file's decoded `text` for each structural pattern, `shapes` in the
    /// order of `structural`, which pairs each pattern's index among all the run's patterns with
    /// its compiled one.  A file that does not parse is searched for nothing, and where and why
    /// is returned.
    fn python(
        &mut self,
        shown: &Path,
        text: &str,
        structural: &[(usize, usize)],
        shapes: &[&Pattern],
    ) -> Result<(), String> {
        match structural::search(text, shapes) {
            Err(unparsed) => Err(unparsed_at(&unparsed)),
            Ok(found) => {
                for (&(i, _), in_file) in structural.iter().zip(found) {
                    if !in_file.is_empty() {
                        self.matches[i].push((
                            shown.to_owned(),
                            in_file
                                .into_iter()
                                .map(|f| Hit {
                                    at: f.at,
                                    text: Some(f.text),
                                })
                                .collect(),
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    /// Searches a Python file's current bytes, `text`, for a batch's structural patterns.
    /// `parsed` says whether an earlier batch parsed the file, so that it is counted as searched,
    /// or as unparseable, once however many batches search it.
    fn structural_batch(
        &mut self,
        shown: &Path,
        text: &[u8],
        structural: &[(usize, usize)],
        plan: &Plan,
        parsed: &mut Option<bool>,
    ) {
        let shapes: Vec<&Pattern> = structural.iter().map(|&(_, c)| &plan.compiled[c]).collect();
        // The text Ruff parses is the text CPython would run, decoded as CPython decodes it,
        // whatever else declares the file's encoding for the textual tier.
        let outcome = match python::Source::decode(text) {
            Ok(source) => self.python(shown, &source.text, structural, &shapes),
            // A Python file CPython refuses to read does not parse, whatever it holds, and one in
            // an encoding quaff does not read cannot be parsed by quaff: either way a count
            // without it could not be trusted.
            Err(refusal) => Err(refusal.to_string()),
        };
        match (*parsed, outcome) {
            (None, Ok(())) => {
                self.structural += 1;
                *parsed = Some(true);
            }
            (None | Some(true), Err(why)) => {
                self.unparsed.push((shown.to_owned(), why));
                *parsed = Some(false);
            }
            _ => {}
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
    ) -> Option<Vec<u8>> {
        let hits = |matches: Vec<Match>| -> Vec<Hit> {
            matches
                .into_iter()
                .map(|at| Hit { at, text: None })
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
                self.unwritable[index].push((shown.to_owned(), encoding, text, ambiguous));
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

/// Where and why a Python file does not parse, as a message gives it.
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
    /// How many matches the replaces replaced, and in which files.
    replaced: usize,
    replaced_in: std::collections::BTreeSet<&'s Path>,
}

impl<'s> Evaluation<'s> {
    /// An evaluation of what `searched` found, printing as it goes or not.
    fn new(searched: &'s Searched, printing: bool) -> Self {
        Evaluation {
            searched,
            printing,
            next: 0,
            notes: Vec::new(),
            replaced: 0,
            replaced_in: std::collections::BTreeSet::new(),
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
                Step::Find(pattern) => {
                    let found = self.print_matches(*pattern, out)?;
                    if found == 0 {
                        self.notes.push(match self.nothing_applicable(*pattern) {
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
                    let mut failed = Vec::new();
                    // An assertion is decided by hits alone.  Possible matches, and files not
                    // searched for its pattern, are noted but never decide it, so that a file
                    // quaff cannot read can never block an answer about the files it can.
                    for assertion in block {
                        let found = self.print_matches(assertion.pattern, out)?;
                        let location = built.locate(assertion.span, assertion.span.start);
                        // An assertion about the structure of files that have none is not a
                        // passed one, whatever it asks.
                        if let Some(why) = self.nothing_applicable(assertion.pattern) {
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
                        // Before the other notes: they are about what was looked at, these
                        // are the answer.
                        failed.append(&mut self.notes);
                        self.notes = failed;
                        return Ok(Outcome::AssertionFailed);
                    }
                }
                Step::Replace(replacing) => {
                    asserted = true;
                    let index = self.next;
                    let pattern = Wanted::Text(replacing.pattern);
                    let found = self.print_matches(pattern, out)?;
                    if !replacing.count.holds(found) {
                        let location = built.locate(replacing.span, replacing.span.start);
                        let mut failed = vec![format!(
                            "{location}: expected {} of {} to replace, found {found}; nothing \
                             was written",
                            expected(replacing.count),
                            quoted(pattern)
                        )];
                        failed.append(&mut self.notes);
                        self.notes = failed;
                        return Ok(Outcome::AssertionFailed);
                    }
                    let unwritable = &self.searched.unwritable[index];
                    if !unwritable.is_empty() {
                        return Err(Failure::Unwritable {
                            files: unwritable
                                .iter()
                                .map(|(path, encoding, text, ambiguous)| Unwritable {
                                    path: path.clone(),
                                    encoding: name_of(*encoding),
                                    text: text.clone(),
                                    ambiguous: *ambiguous,
                                })
                                .collect(),
                        });
                    }
                    self.replaced += usize::try_from(found).unwrap_or(usize::MAX);
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

    /// Prints the next pattern's matches, `pattern`, and returns how many there were.
    fn print_matches(&mut self, pattern: Wanted, out: &mut impl Write) -> Result<u64, Failure> {
        // A textual match is the pattern's own text; a structural one is the code it covers.
        let text = match pattern {
            Wanted::Text(pattern) => display(pattern),
            Wanted::Structural { .. } => String::new(),
        };
        let mut count = 0;
        for (path, in_file) in &self.searched.matches[self.next] {
            for hit in in_file {
                count += 1;
                if self.printing {
                    let shown = hit.text.as_deref().unwrap_or(&text);
                    self.printing = print(out, &output::line(path, &hit.at, shown))?;
                }
            }
        }
        self.next += 1;
        Ok(count)
    }

    /// Why `pattern` could not be looked for anywhere, if that is so: a structural pattern in a
    /// scope with no Python file, or none that could be searched.
    fn nothing_applicable(&self, pattern: Wanted) -> Option<String> {
        let searched = self.searched;
        match pattern {
            Wanted::Structural { .. } if searched.python == 0 => Some(format!(
                "no file in scope has structural support; {} not searched",
                count(searched.not_applicable, "text-tier file", "text-tier files")
            )),
            // Every Python file in scope is binary, which the binary count says.
            Wanted::Structural { .. } if searched.structural == 0 => Some(format!(
                "no Python file in scope could be searched; {} not searched",
                count(searched.python, "Python file", "Python files")
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
/// matches; then the Python files `CPython` refuses, the files that read cleanly in the locale's
/// encoding, and what discovery, classification and the project's declarations passed over.
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
    if found.special_files > 0 {
        notes.push(format!(
            "{} - FIFOs, sockets or devices - not searched",
            count(found.special_files, "special file", "special files")
        ));
    }
    if found.links_not_followed > 0 {
        notes.push(format!(
            "{} not followed",
            count(found.links_not_followed, "symbolic link", "symbolic links")
        ));
    }
    notes
}

/// The notes for structural patterns, once for the run however many there are: the text-tier
/// files they do not apply to, where some Python is in scope - with none, the search or the
/// assertion already counted them.
fn structural_notes(searched: &Searched, evaluated: &[Searching]) -> Vec<String> {
    let mut notes = Vec::new();
    if !evaluated
        .iter()
        .any(|s| matches!(s.pattern, Wanted::Structural { .. }))
    {
        return notes;
    }
    // With no Python in scope, the run's own message counts these instead.
    if searched.python > 0 && searched.not_applicable > 0 {
        notes.push(format!(
            "{} not searched for structural patterns, which apply to Python files only",
            count(searched.not_applicable, "text-tier file", "text-tier files")
        ));
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
        let outside = Failure::OutsideProject {
            scope: "/x".into(),
            root: ".".into(),
        };
        assert_eq!(outside.exit_code(), 3);
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
                "quaff does not expand a glob given as the scope; name one file or directory"
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
                false,
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
            false,
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
            false,
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
            false,
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
        let mut declarations = Declarations {
            workspace: None,
            editorconfig: EditorConfig::new(&dir, &Contents::default()),
            cwd: dir.clone(),
            contents: Contents::default(),
        };
        let searched = search_files(
            &[with, without],
            &dir,
            &[Wanted::Text(&pattern)],
            &[None],
            &[],
            &mut declarations,
            None,
        );
        let _ = fs::remove_dir_all(&dir);
        let searched = searched.expect("searching");
        let listed: Vec<&PathBuf> = searched.matches[0].iter().map(|(path, _)| path).collect();
        assert_eq!(listed, [&PathBuf::from("with.txt")]);
    }

    #[test]
    fn counts_are_worded_for_one_and_for_many() {
        assert_eq!(count(1, "binary file", "binary files"), "1 binary file");
        assert_eq!(count(3, "binary file", "binary files"), "3 binary files");
    }
}
