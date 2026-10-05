// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Carrying out a run: the program its sources make, the scope, the files in it, the matches,
//! the assertions, and what was not looked at.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use quaffed_representation::check;
use quaffed_representation::program::{
    ActionKind, Count, Counted, Expectation, Operand, Program, Query, Statement, TextPart,
    TextPattern,
};
use quaffed_representation::span::{Source, Span, Spanned};
use quaffed_script::{ErrorKind, location, parse_source};

use crate::cli::{self, SourceArg, UsageError};
use crate::discover;
use crate::encoding::{self, Content};
use crate::output;
use crate::project;
use crate::search::{self, Match};

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
            Failure::Io { .. } | Failure::Discovery(_) => 7,
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
        }
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Io { cause, .. } => Some(cause),
            Failure::Discovery(err) => Some(err),
            Failure::Usage(_)
            | Failure::Script { .. }
            | Failure::Empty { .. }
            | Failure::Unknown { .. }
            | Failure::OutsideProject { .. }
            | Failure::OutsideCurrentDirectory { .. } => None,
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
    cwd: &Path,
    stdin: &mut impl Read,
    out: &mut impl Write,
    notes: &mut impl Write,
) -> Result<Outcome, Failure> {
    let mut built = build(sources, stdin)?;
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
    let steps = plan(&built.program).map_err(|(construct, span)| Failure::Unknown {
        location: built.locate(span, span.start),
        message: format!("{construct} is not built yet"),
    })?;

    let cwd = fs::canonicalize(cwd).map_err(|cause| Failure::Io {
        doing: "reading the current directory",
        path: cwd.to_path_buf(),
        cause,
    })?;
    let scope = resolve_scope(built.program.narrow_to.as_deref(), &cwd)?;
    let found = discover::files(&scope, &cwd).map_err(Failure::Discovery)?;

    let searching: Vec<Searching> = steps.iter().flat_map(Step::patterns).collect();
    let patterns: Vec<&TextPattern> = searching.iter().map(|s| s.pattern).collect();
    let searched = search_files(&found.files, &cwd, &patterns)?;
    let mut evaluation = Evaluation {
        searched: &searched,
        printing: true,
        next: 0,
        notes: Vec::new(),
    };
    let outcome = evaluation.steps(&steps, &built, out)?;

    // Notes are best effort: a run that cannot write to standard error has nowhere to say so.
    let mut note = |text: &str| {
        let _ = writeln!(notes, "quaff: {text}");
    };
    for text in &evaluation.notes {
        note(text);
    }
    for text in skipped(&searched, &searching[..evaluation.next], &found) {
        note(&text);
    }
    Ok(outcome)
}

/// A program and the text of each of its sources, which locations are worked out from.
struct Built {
    program: Program,
    texts: Vec<String>,
}

impl Built {
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
fn build(sources: Vec<SourceArg>, stdin: &mut impl Read) -> Result<Built, Failure> {
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
                let text = script_text(fs::read(&path), &source)?;
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
    /// Adds a textual query from the command line as the `find` it means.
    fn push_query(&mut self, query: cli::Query) {
        let source = self.program.sources.len();
        self.program.sources.push(match query.source {
            cli::Source::Positional => Source::Positional,
            cli::Source::StringOption => Source::StringOption,
        });
        let span = Span {
            source,
            start: 0,
            end: query.text.len(),
        };
        let operand = Operand::Text(TextPattern::literal(query.text.clone()));
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

/// One step of a program this build can run: every query in it is textual and unscoped.
enum Step<'p> {
    /// Search, and print what was found.
    Find(&'p TextPattern),
    /// Search for each, print what was found, and check each count.
    Assert(Vec<Assertion<'p>>),
}

/// One assertion in a block this build can run.
struct Assertion<'p> {
    pattern: &'p TextPattern,
    count: Count,
    span: Span,
}

/// One pattern the run searches for, and whether an assertion counts it.
#[derive(Clone, Copy)]
struct Searching<'p> {
    pattern: &'p TextPattern,
    asserted: bool,
}

impl Step<'_> {
    /// The step's patterns in order, each with whether it is asserted.  One list carries both,
    /// so the search and the notes cannot disagree about which pattern is which.
    fn patterns(&self) -> Vec<Searching<'_>> {
        match self {
            Step::Find(pattern) => vec![Searching {
                pattern,
                asserted: false,
            }],
            Step::Assert(block) => block
                .iter()
                .map(|a| Searching {
                    pattern: a.pattern,
                    asserted: true,
                })
                .collect(),
        }
    }
}

/// The steps `program` runs as, or the first construct in it that is in the MVP but not
/// built yet, with where it was written.
fn plan(program: &Program) -> Result<Vec<Step<'_>>, (&'static str, Span)> {
    let mut steps = Vec::new();
    for statement in program.body() {
        steps.push(match &statement.node {
            Statement::Find(query) => Step::Find(buildable(query)?),
            Statement::Assert(block) => {
                Step::Assert(block.iter().map(assertion).collect::<Result<_, _>>()?)
            }
            Statement::Act(action) => {
                let construct = match action.kind {
                    ActionKind::Delete => "delete",
                    _ => "replace",
                };
                return Err((construct, statement.span));
            }
            _ => return Err(("this statement", statement.span)),
        });
    }
    Ok(steps)
}

fn assertion(expectation: &Spanned<Expectation>) -> Result<Assertion<'_>, (&'static str, Span)> {
    let Counted::Matches(query) = &expectation.node.counted else {
        return Err(("this assertion", expectation.span));
    };
    Ok(Assertion {
        pattern: buildable(query)?,
        count: expectation.node.count.node,
        span: expectation.span,
    })
}

/// The pattern of `query`, if this build can search for it.
fn buildable(query: &Query) -> Result<&TextPattern, (&'static str, Span)> {
    if let Some(glob) = query.scope.first() {
        return Err(("an `in` clause", glob.span));
    }
    if let Some(filter) = &query.filter {
        return Err(("a `where` clause", filter.span));
    }
    match &query.operand.node {
        Operand::Text(pattern) => Ok(pattern),
        _ => Err((
            "a structural pattern - a backticked operand -",
            query.operand.span,
        )),
    }
}

/// The directory or file a run covers: the project, or `cwd` with no project, narrowed to
/// `scope`.
fn resolve_scope(scope: Option<&Path>, cwd: &Path) -> Result<PathBuf, Failure> {
    let root = project::find_root(cwd);
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

/// What searching every file for every pattern found.
struct Searched {
    /// For each pattern, the files with matches, in discovery order.
    matches: Vec<Vec<(PathBuf, Vec<Match>)>>,
    /// For each pattern, the files in an unknown 8-bit encoding it has no spelling in, by the
    /// paths a message shows.
    unknown_encoding: Vec<Vec<PathBuf>>,
    /// Binary files, counted once whatever the number of patterns.
    binary: usize,
}

/// Searches each of `files` for each of `patterns`, reading each file once.
fn search_files(
    files: &[PathBuf],
    cwd: &Path,
    patterns: &[&TextPattern],
) -> Result<Searched, Failure> {
    let mut searched = Searched {
        matches: vec![Vec::new(); patterns.len()],
        unknown_encoding: vec![Vec::new(); patterns.len()],
        binary: 0,
    };
    for file in files {
        let shown = output::relative(file, cwd);
        let bytes = fs::read(file).map_err(|cause| Failure::Io {
            doing: "reading",
            path: shown.clone(),
            cause,
        })?;
        // Classified once, whatever the number of patterns.
        let (encoding, body) = match encoding::classify(&bytes) {
            Content::Binary => {
                searched.binary += 1;
                continue;
            }
            Content::Text { encoding, body } => (encoding, body),
        };
        for (i, pattern) in patterns.iter().enumerate() {
            match search::search(&bytes[body..], encoding, pattern) {
                None => searched.unknown_encoding[i].push(shown.clone()),
                Some(in_file) if in_file.is_empty() => {}
                Some(in_file) => searched.matches[i].push((shown.clone(), in_file)),
            }
        }
    }
    Ok(searched)
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
}

impl Evaluation<'_> {
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
                    let found = self.print_matches(pattern, out)?;
                    if found == 0 {
                        self.notes
                            .push(format!("no matches for {}", quoted(pattern)));
                    }
                    any |= found > 0;
                }
                Step::Assert(block) => {
                    asserted = true;
                    let mut failed = Vec::new();
                    for assertion in block {
                        let unsearched = &self.searched.unknown_encoding[self.next];
                        let found = self.print_matches(assertion.pattern, out)?;
                        let location = built.locate(assertion.span, assertion.span.start);
                        let wanted = expected(assertion.count);
                        let pattern = quoted(assertion.pattern);
                        if unsearched.is_empty() {
                            if !assertion.count.holds(found) {
                                failed.push(format!(
                                    "{location}: expected {wanted} of {pattern}, found {found}"
                                ));
                            }
                            continue;
                        }
                        // Unsearched files can only add matches, so a count is decided without
                        // them only where no number of further matches could change it.
                        // Otherwise a count over fewer files than the scope holds is not a
                        // passed assertion.
                        let one = unsearched.len() == 1;
                        let listed: Vec<String> =
                            unsearched.iter().map(|path| format!("{path:?}")).collect();
                        let files = format!(
                            "{} in an unknown 8-bit encoding, which a non-ASCII query has no one \
                             spelling in, {} not searched - {}",
                            count(unsearched.len(), "file", "files"),
                            if one { "was" } else { "were" },
                            listed.join(", ")
                        );
                        match settled(assertion.count, found) {
                            Some(true) => self.notes.push(format!(
                                "{location}: found {found} of {pattern}, enough for {wanted} \
                                 whatever the rest hold; {files}"
                            )),
                            Some(false) => failed.push(format!(
                                "{location}: expected {wanted} of {pattern}, found {found}, \
                                 and more can only be in the rest: {files}"
                            )),
                            None => failed.push(format!(
                                "{location}: cannot check for {wanted} of {pattern}: {files}.  \
                                 Found {found} in the files that were searched.  To check it, \
                                 re-encode {} as UTF-8, or leave {} out with the scope or an \
                                 ignore file",
                                if one { "the file" } else { "the files" },
                                if one { "it" } else { "them" }
                            )),
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
            }
        }
        Ok(if asserted || any {
            Outcome::Success
        } else {
            Outcome::NothingFound
        })
    }

    /// Prints the next pattern's matches, `pattern`, and returns how many there were.
    fn print_matches(
        &mut self,
        pattern: &TextPattern,
        out: &mut impl Write,
    ) -> Result<u64, Failure> {
        let text = display(pattern);
        let mut count = 0;
        for (path, in_file) in &self.searched.matches[self.next] {
            for found in in_file {
                count += 1;
                if self.printing {
                    self.printing = print(out, &output::line(path, found, &text))?;
                }
            }
        }
        self.next += 1;
        Ok(count)
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

/// `pattern` as a message names it: its text quoted, with any anchors outside the quotes as a
/// script writes them, so an anchored pattern is not mistaken for the bare text.
fn quoted(pattern: &TextPattern) -> String {
    format!(
        "{}{:?}{}",
        if pattern.start_anchor { "^" } else { "" },
        display(pattern),
        if pattern.end_anchor { "$" } else { "" }
    )
}

/// Whether `count` is decided by `found` whatever more matches are added: `Some(true)` if it
/// holds however many more there are, `Some(false)` if it fails however many, and `None` if
/// more could change the answer.
fn settled(count: Count, found: u64) -> Option<bool> {
    match count {
        Count::AtLeast(n) => (found >= n).then_some(true),
        Count::Any => Some(true),
        Count::Exactly(n) | Count::AtMost(n) | Count::ExactlyOrNone(n) => {
            (found > n).then_some(false)
        }
        Count::None => (found > 0).then_some(false),
        // A count added later is undecided until it says otherwise, which only fails safe.
        _ => None,
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

/// A note for everything not looked at, so that nothing is silently left out: per `find`
/// evaluated, the files it had no spelling in, then what discovery and classification passed
/// over.  An assertion's unsearched files are not noted: they failed it, and its failure names
/// them.
fn skipped(searched: &Searched, evaluated: &[Searching], found: &discover::Found) -> Vec<String> {
    let mut notes = Vec::new();
    for (Searching { pattern, .. }, unknown) in evaluated
        .iter()
        .zip(&searched.unknown_encoding)
        .filter(|(searching, _)| !searching.asserted)
    {
        if !unknown.is_empty() {
            notes.push(format!(
                "{} in an unknown 8-bit encoding not searched for {}, which is not ASCII and so \
                 has no one spelling there",
                count(unknown.len(), "file", "files"),
                quoted(pattern)
            ));
        }
    }
    if searched.binary > 0 {
        notes.push(format!(
            "{} not searched",
            count(searched.binary, "binary file", "binary files")
        ));
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
    fn a_count_is_settled_only_where_no_further_match_could_change_it() {
        // More matches can only be added, never taken away.
        assert_eq!(settled(Count::AtLeast(2), 2), Some(true));
        assert_eq!(settled(Count::AtLeast(2), 1), None);
        assert_eq!(settled(Count::Any, 0), Some(true));
        assert_eq!(settled(Count::Exactly(2), 3), Some(false));
        assert_eq!(settled(Count::Exactly(2), 2), None);
        assert_eq!(settled(Count::AtMost(2), 3), Some(false));
        assert_eq!(settled(Count::AtMost(2), 2), None);
        assert_eq!(settled(Count::ExactlyOrNone(2), 3), Some(false));
        assert_eq!(settled(Count::ExactlyOrNone(2), 0), None);
        assert_eq!(settled(Count::None, 1), Some(false));
        assert_eq!(settled(Count::None, 0), None);
        assert_eq!(expected(Count::Any), "any number of matches");
    }

    #[test]
    fn a_pattern_is_named_with_its_anchors_outside_the_quotes() {
        let mut pattern = TextPattern::literal("x");
        assert_eq!(quoted(&pattern), "\"x\"");
        pattern.start_anchor = true;
        pattern.push_line_break();
        pattern.push_literal("y".into());
        assert_eq!(quoted(&pattern), "^\"x\\ny\"");
        pattern.start_anchor = false;
        pattern.end_anchor = true;
        assert_eq!(quoted(&pattern), "\"x\\ny\"$");
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
    fn a_file_without_matches_is_not_listed_for_a_pattern() {
        let dir = std::env::temp_dir().join(format!("quaffed-run-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creating the directory");
        let with = dir.join("with.txt");
        let without = dir.join("without.txt");
        fs::write(&with, "TODO\n").expect("writing");
        fs::write(&without, "nothing\n").expect("writing");
        let pattern = TextPattern::literal("TODO");
        let searched = search_files(&[with, without], &dir, &[&pattern]);
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
