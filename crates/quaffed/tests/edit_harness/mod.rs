//! What the edit-invariant harness does with a registered case: runs it with the real `quaff` on
//! fresh copies of a language's files, and judges each run against the invariants.
//!
//! Nothing here knows an operation or a language: the tables in `edit_invariants.rs` give both,
//! and a language's oracle is an external command.  The checks are tested below on small
//! projects, with the real `quaff` and with runs broken on purpose, so that the harness is shown
//! to catch each fault before it is trusted with the corpus.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use quaffed_encoding::python::declared_encoding;
use quaffed_encoding::{Encoding, byte_order_mark, proven_utf8};

/// A project's files: each path, relative and with `/` between parts, and its bytes.
pub type Files = BTreeMap<String, Vec<u8>>;

/// One registered case: a statement that edits, and the `find` that counts what it edits.
pub struct Case {
    /// The operation, as a script names it: `replace string`.
    pub operation: &'static str,
    /// What this case exercises, so that two cases of one operation are told apart.
    pub exercises: &'static str,
    /// The `find` statement whose matches are exactly what the edit changes, with no `expect`.
    pub find: &'static str,
    /// The editing statement, with no `expect`: the harness adds every count.
    pub edit: &'static str,
    /// The languages, by name in the language table, whose files it runs over.
    pub languages: &'static [&'static str],
    /// What quaff should make of it.
    pub outcome: Outcome,
    /// Whether it runs only over the files the language's oracle accepts.  A structural search
    /// refuses a run with a file in scope it cannot parse, by design, so a structural case over
    /// a corpus holding deliberately broken files would only ever be refused.
    pub parsed_only: bool,
}

/// What quaff should make of a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It edits, and every invariant is checked, on its own output again too.
    Edits,
    /// It cannot be made as written - it would leave a file unparseable, say, or rewrite matches
    /// that overlap - so quaff refuses it with exit 9 and writes nothing, saying why in words
    /// that hold this text: a refusal for another reason does not exercise what the case does.
    Refused(&'static str),
}

/// A language the corpus holds.
pub struct Language {
    /// Its name, as cases name it.
    pub name: &'static str,
    /// Whether a file is this language's, by its path in the corpus.
    pub claims: fn(&str) -> bool,
    /// What judges whether its files parse.
    pub judge: Judge,
}

/// What judges whether a language's files parse.
pub enum Judge {
    /// Nothing: it has no structure, so still-parses does not apply, and is reported so.
    NotApplicable,
    /// This oracle.
    Oracle(Oracle),
    /// It has structure, but its oracle cannot run here, for this reason.
    Unavailable(String),
}

/// An external command that is given files and prints, one line each, the ones it refuses: the
/// path as given, a tab and its reason.  It prints nothing for a file it accepts, and exits 0
/// only once it has judged every file.
pub struct Oracle {
    /// The program it runs.
    pub program: PathBuf,
    /// The arguments before the files.
    pub args: Vec<OsString>,
}

impl Oracle {
    /// The files of `paths`, under `dir`, that the oracle refuses, with its reasons.
    ///
    /// # Errors
    ///
    /// If the oracle cannot run, exits other than 0, or prints a line that is not a refusal of
    /// one of the files: an oracle that failed has judged nothing, so nothing counts as accepted.
    pub fn refused(
        &self,
        dir: &Path,
        paths: &[&String],
    ) -> Result<BTreeMap<String, String>, String> {
        if paths.is_empty() {
            return Ok(BTreeMap::new());
        }
        let given: BTreeMap<String, &String> = paths
            .iter()
            .map(|path| {
                (
                    dir.join(path.as_str()).to_string_lossy().into_owned(),
                    *path,
                )
            })
            .collect();
        let output = Command::new(&self.program)
            .args(&self.args)
            .args(given.keys())
            .output()
            .map_err(|err| format!("running the oracle {:?}: {err}", self.program))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            return Err(format!(
                "the oracle {:?} {:?} exited with {}: {}",
                self.program,
                self.args,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        stdout
            .lines()
            .map(|line| {
                let (path, reason) = line
                    .split_once('\t')
                    .ok_or_else(|| format!("the oracle printed {line:?}, which has no tab"))?;
                let file = given.get(path).ok_or_else(|| {
                    format!("the oracle refused {path:?}, which it was not given")
                })?;
                Ok(((*file).clone(), reason.to_owned()))
            })
            .collect()
    }
}

/// What one run of `quaff` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Its exit code, or `None` if a signal ended it.
    pub code: Option<i32>,
    /// Its standard output.
    pub stdout: String,
    /// Its standard error.
    pub stderr: String,
}

/// Something that runs a script in a project as `quaff -e` does.
pub trait Quaff: Sync {
    /// Runs `script` with `project` as the current directory.
    fn run(&self, project: &Path, script: &str) -> Run;
}

/// The built `quaff`.
pub struct Binary;

impl Quaff for Binary {
    /// The environment is cleared, as for the end-to-end tests, but for the coverage profile's
    /// path: without it an instrumented binary writes its profile into the project, which would
    /// read as a file the edit created.
    fn run(&self, project: &Path, script: &str) -> Run {
        let binary = Path::new(env!("CARGO_BIN_EXE_quaff"));
        let mut command = Command::new(binary);
        command
            .args(["-e", script])
            .current_dir(project)
            .env_clear();
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let output = command.output().unwrap_or_else(|err| {
            panic!(
                "running {} in {}: {err}",
                binary.display(),
                project.display()
            )
        });
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// A throwaway project holding a copy of some files, removed when it is dropped.
pub struct Scratch {
    /// Where it is.
    pub dir: PathBuf,
}

impl Scratch {
    /// A new project holding exactly `files`.
    pub fn new(files: &Files) -> Scratch {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-edit-invariants-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier run whose process ID this one reuses would start the
        // project with stale files, so anything but "not there" stops the test.
        match fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                panic!("clearing {} before the run: {err}", dir.display())
            }
            _ => {}
        }
        fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("creating {}: {err}", dir.display()));
        for (path, bytes) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap_or_else(|err| {
                panic!("creating the directory of {}: {err}", path.display())
            });
            fs::write(&path, bytes)
                .unwrap_or_else(|err| panic!("writing {}: {err}", path.display()));
        }
        Scratch { dir }
    }

    /// Every file in it now.
    pub fn files(&self) -> Files {
        read_tree(&self.dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Not checked: a panic here, during the unwinding of a failed test, would abort the run
        // and hide that failure.  A directory left behind is cleared by the next Scratch that
        // reuses its name.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Every file under `root`, by its path relative to `root`.
pub fn read_tree(root: &Path) -> Files {
    let mut files = Files::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries =
            fs::read_dir(&dir).unwrap_or_else(|err| panic!("listing {}: {err}", dir.display()));
        for entry in entries {
            let path = entry.expect("reading a directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).expect("under the root");
                let parts: Vec<_> = relative.iter().map(|p| p.to_string_lossy()).collect();
                let bytes = fs::read(&path)
                    .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
                files.insert(parts.join("/"), bytes);
            }
        }
    }
    files
}

/// An invariant, or the harness's own check that it checked something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invariant {
    /// A count that does not hold leaves every file byte-identical, exit 2.
    Atomicity,
    /// Every byte outside the matches `find` reports survives, in order.
    MinimalDiff,
    /// Every line outside the lines `find` reports survives, in order: a second view of minimal
    /// diff that does not use the harness's column arithmetic.
    MinimalDiffLines,
    /// Two runs give the same exit, output and files.
    Determinism,
    /// A file the oracle accepted before is accepted after, or the edit is refused with exit 9.
    StillParses,
    /// An edit whose count holds runs and exits 0.
    Applies,
    /// The harness could not check what it should have: a guard, never a pass.
    Harness,
}

impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Invariant::Atomicity => "atomicity",
            Invariant::MinimalDiff => "minimal diff",
            Invariant::MinimalDiffLines => "minimal diff, by lines",
            Invariant::Determinism => "determinism",
            Invariant::StillParses => "still parses",
            Invariant::Applies => "applies",
            Invariant::Harness => "the harness",
        })
    }
}

/// One way a case broke an invariant, or the harness could not check it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The operation and what the case exercises, or `-` for a failure of no case.
    pub case: String,
    /// The language, or `-`.
    pub language: String,
    /// What broke.
    pub invariant: Invariant,
    /// Whether it broke on re-applying the edit to its own output: idempotency.
    pub reapplying: bool,
    /// The file, if one is to blame.
    pub file: Option<String>,
    /// What happened.
    pub what: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, {}: {}", self.case, self.language, self.invariant)?;
        if self.reapplying {
            f.write_str(" on re-applying the edit to its own output (idempotency)")?;
        }
        if let Some(file) = &self.file {
            write!(f, ", {file:?}")?;
        }
        write!(f, ": {}", self.what)
    }
}

/// A failure of the harness itself rather than of an edit: what it could not check.
fn guard(what: String) -> Failure {
    Failure {
        case: "-".into(),
        language: "-".into(),
        invariant: Invariant::Harness,
        reapplying: false,
        file: None,
        what,
    }
}

/// The failures the corpus's files give before any edit: a declaration of encodings the harness
/// does not read, and a file claimed by no language or by more than one.
fn corpus_guards(languages: &[Language], corpus: &Files) -> Vec<Failure> {
    let mut failures = Vec::new();
    // quaff reads an encoding from these, and the harness places matches without them, so a
    // corpus holding one is reported rather than mis-judged.
    for path in corpus.keys() {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name == ".gitattributes" || name == ".editorconfig" {
            failures.push(guard(format!(
                "{path:?} may declare encodings, which the harness does not read, so it cannot \
                 place matches in the files it covers; remove it from the corpus or teach the \
                 harness to read it"
            )));
        }
    }
    // Each file is one language's, so none goes unchecked and none is judged by two sets of rules.
    for path in corpus.keys() {
        let claimed: Vec<&str> = languages
            .iter()
            .filter(|language| (language.claims)(path))
            .map(|language| language.name)
            .collect();
        if claimed.is_empty() {
            failures.push(guard(format!(
                "{path:?} is claimed by no language, so no edit of it is checked"
            )));
        } else if claimed.len() > 1 {
            failures.push(guard(format!(
                "{path:?} is claimed by more than one language, {claimed:?}, so it would be judged \
                 by each one's rules; a file must be one language's"
            )));
        }
    }
    failures
}

/// Runs every case over every language it names, in `corpus`, and returns every failure.
///
/// It fails rather than passes when there is nothing to check: no case, a language with no
/// files, a file claimed by no language or by more than one, a case naming a language the table
/// lacks, a structural language whose oracle cannot run, a case matching or editing nothing, or a
/// declaration of encoding in the corpus that the harness does not read.
pub fn check(
    cases: &[Case],
    languages: &[Language],
    corpus: &Files,
    quaff: &dyn Quaff,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    if cases.is_empty() {
        failures.push(guard(
            "no operation is registered, so nothing was checked".into(),
        ));
    }
    failures.extend(corpus_guards(languages, corpus));
    for language in languages {
        if !corpus.keys().any(|path| (language.claims)(path)) {
            failures.push(Failure {
                language: language.name.into(),
                ..guard("the corpus has no file of this language, so nothing was checked".into())
            });
        }
        if let Judge::Unavailable(why) = &language.judge {
            failures.push(Failure {
                language: language.name.into(),
                invariant: Invariant::StillParses,
                ..guard(format!(
                    "its oracle cannot run, so no file was judged: {why}"
                ))
            });
        }
    }
    let mut jobs: Vec<(&Case, &Language)> = Vec::new();
    for case in cases {
        for name in case.languages {
            match languages.iter().find(|language| language.name == *name) {
                Some(language) => jobs.push((case, language)),
                None => failures.push(Failure {
                    case: format!("{} ({})", case.operation, case.exercises),
                    ..guard(format!(
                        "it names the language {name:?}, which the table does not have"
                    ))
                }),
            }
        }
    }
    let by_language: BTreeMap<&str, Files> = languages
        .iter()
        .map(|language| {
            let files = corpus
                .iter()
                .filter(|(path, _)| (language.claims)(path))
                .map(|(path, bytes)| (path.clone(), bytes.clone()))
                .collect();
            (language.name, files)
        })
        .collect();
    failures.extend(concurrently(&jobs, |(case, language)| {
        let began = Instant::now();
        let found = Judging {
            case,
            language,
            quaff,
        }
        .judge(&by_language[language.name]);
        // Seen with --nocapture, so that a language or a case whose run grows is visible.
        eprintln!(
            "edit invariants: {} ({}) over {}: {:.1}s",
            case.operation,
            case.exercises,
            language.name,
            began.elapsed().as_secs_f64()
        );
        found
    }));
    failures
}

/// Every job's failures, in the order of `jobs`, judged as many at a time as the machine has
/// cores: each case over each language is independent, so the run grows with the slowest
/// rather than with the number of languages and cases.
fn concurrently<J: Sync>(jobs: &[J], judge: impl Fn(&J) -> Vec<Failure> + Sync) -> Vec<Failure> {
    let workers = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(jobs.len());
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, Vec<Failure>)> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(job) = jobs.get(at) else {
                            return done;
                        };
                        done.push((at, judge(job)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            // A worker's panic is a failed test, so it is passed on as it is.
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    done.sort_by_key(|(at, _)| *at);
    done.into_iter()
        .flat_map(|(_, failures)| failures)
        .collect()
}

/// The files of `files` the oracle `accepted`, or `None` if no oracle judged them.
fn parsed_only(files: &Files, accepted: Option<&BTreeSet<String>>) -> Option<Files> {
    let accepted = accepted?;
    Some(
        files
            .iter()
            .filter(|(path, _)| accepted.contains(*path))
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect(),
    )
}

/// One case, judged over one language's files.
struct Judging<'a> {
    case: &'a Case,
    language: &'a Language,
    quaff: &'a dyn Quaff,
}

/// What one application of an edit left: the files, those the oracle accepts in them, and a
/// project holding them, which the next application starts from.
struct Applied {
    files: Files,
    accepted: Option<BTreeSet<String>>,
    project: Scratch,
}

impl Judging<'_> {
    fn failure(
        &self,
        invariant: Invariant,
        reapplying: bool,
        file: Option<&str>,
        what: String,
    ) -> Failure {
        Failure {
            case: format!("{} ({})", self.case.operation, self.case.exercises),
            language: self.language.name.into(),
            invariant,
            reapplying,
            file: file.map(str::to_owned),
            what,
        }
    }

    /// Applies the case to `files`, then to its own output, and returns every failure.
    fn judge(&self, files: &Files) -> Vec<Failure> {
        let mut failures = Vec::new();
        if files.is_empty() {
            return failures;
        }
        let project = Scratch::new(files);
        let accepted = match self.accepted(&project.dir, files) {
            Ok(accepted) => accepted,
            Err(why) => {
                failures.push(self.failure(Invariant::Harness, false, None, why));
                None
            }
        };
        let no_oracle = "the case runs only over files that parse, but no oracle judged which do";
        let (files, project) = if self.case.parsed_only {
            let Some(parsed) = parsed_only(files, accepted.as_ref()) else {
                let failure = self.failure(Invariant::Harness, false, None, no_oracle.into());
                failures.push(failure);
                return failures;
            };
            let project = Scratch::new(&parsed);
            (parsed, project)
        } else {
            (files.clone(), project)
        };
        let before = &files;
        let start = Applied {
            files: files.clone(),
            accepted,
            project,
        };
        if let Some(edited) = self.apply(start, false, &mut failures) {
            if edited.files == *before {
                failures.push(self.failure(
                    Invariant::Harness,
                    false,
                    None,
                    "the edit changed no file, so the invariants were checked against nothing; \
                     change the case so that it edits this language's files"
                        .into(),
                ));
            }
            self.apply(edited, true, &mut failures);
        }
        failures
    }

    /// The files of `files`, written under `dir`, the language's oracle accepts, or `None` if
    /// it has none.
    fn accepted(&self, dir: &Path, files: &Files) -> Result<Option<BTreeSet<String>>, String> {
        let Judge::Oracle(oracle) = &self.language.judge else {
            return Ok(None);
        };
        accepted_in(oracle, dir, files).map(Some)
    }

    /// Applies the edit once to `start`, checking every invariant, and returns what it left, or
    /// `None` if nothing more can be checked.
    ///
    /// The find, the edit whose count fails and the edit itself share `start`'s project, since
    /// the first two must leave it as it was; when the second did not, which atomicity reports,
    /// the edit gets a fresh copy.  Only the second run of the edit needs one of its own.
    fn apply(&self, start: Applied, again: bool, failures: &mut Vec<Failure>) -> Option<Applied> {
        let matches = self.matches(&start.project.dir, &start.files, again, failures)?;
        let count: usize = matches.values().map(Vec::len).sum();
        let applied = if self.atomicity(&start.project.dir, &start.files, count, again, failures) {
            start.project
        } else {
            Scratch::new(&start.files)
        };

        let script = format!("{} expect {count}", self.case.edit);
        let run = self.quaff.run(&applied.dir, &script);
        let edited = applied.files();
        self.determinism(&start.files, &script, (&run, &edited), again, failures);

        if let (Outcome::Refused(why), false) = (self.case.outcome, again) {
            self.refusal(why, &start.files, (&run, &edited), failures);
            return None;
        }
        if run.code != Some(0) {
            failures.push(self.failure(
                Invariant::Applies,
                again,
                None,
                format!("`{script}` exited {:?}: {}", run.code, run.stderr.trim()),
            ));
            return None;
        }
        for (path, (invariant, what)) in minimal_diff(&start.files, &matches, &edited) {
            failures.push(self.failure(invariant, again, Some(&path), what));
        }
        let accepted = self.still_parses(
            start.accepted.as_ref(),
            &applied.dir,
            &edited,
            again,
            failures,
        );
        Some(Applied {
            files: edited,
            accepted,
            project: applied,
        })
    }

    /// What `find` reports in `files`, written under `dir`, by file, or `None`, with the
    /// failure, if it ran wrong or matched nothing the first time.
    fn matches(
        &self,
        dir: &Path,
        files: &Files,
        again: bool,
        failures: &mut Vec<Failure>,
    ) -> Option<BTreeMap<String, Vec<Span>>> {
        let found = self
            .quaff
            .run(dir, &format!("{} expect any", self.case.find));
        let why = if found.code == Some(0) {
            let paths: BTreeSet<String> = files.keys().cloned().collect();
            match parse_matches(&found.stdout, &paths) {
                Ok(matches) if again || !matches.is_empty() => return Some(matches),
                Ok(_) => "the find matches nothing, so nothing was checked".into(),
                Err(why) => why,
            }
        } else {
            format!("the find exited {:?}: {}", found.code, found.stderr.trim())
        };
        failures.push(self.failure(Invariant::Harness, again, None, why));
        None
    }

    /// The edit, expecting one more match than there are, exits 2 and leaves `files`, written
    /// under `dir`, as they were; returns whether it did leave them.
    fn atomicity(
        &self,
        dir: &Path,
        files: &Files,
        count: usize,
        again: bool,
        failures: &mut Vec<Failure>,
    ) -> bool {
        let run = self
            .quaff
            .run(dir, &format!("{} expect {}", self.case.edit, count + 1));
        if run.code != Some(2) {
            failures.push(self.failure(
                Invariant::Atomicity,
                again,
                None,
                format!(
                    "expecting {} of {count} matches exited {:?}, not 2: {}",
                    count + 1,
                    run.code,
                    run.stderr.trim()
                ),
            ));
        }
        let changed = differing(files, &read_tree(dir));
        for path in &changed {
            failures.push(self.failure(
                Invariant::Atomicity,
                again,
                Some(path),
                "it changed although the count did not hold".into(),
            ));
        }
        changed.is_empty()
    }

    /// `script`, run again on a fresh copy of `files`, does what `first` did.
    fn determinism(
        &self,
        files: &Files,
        script: &str,
        first: (&Run, &Files),
        again: bool,
        failures: &mut Vec<Failure>,
    ) {
        let project = Scratch::new(files);
        let run = self.quaff.run(&project.dir, script);
        if *first.0 != run {
            failures.push(self.failure(
                Invariant::Determinism,
                again,
                None,
                format!(
                    "two runs differ in what they printed or how they exited: {:?} and {run:?}",
                    first.0
                ),
            ));
        }
        for path in differing(first.1, &project.files()) {
            failures.push(self.failure(
                Invariant::Determinism,
                again,
                Some(&path),
                "it differs between two runs".into(),
            ));
        }
    }

    /// An edit that cannot be made as written exited 9, saying `why`, and wrote nothing.
    fn refusal(
        &self,
        why: &str,
        files: &Files,
        applied: (&Run, &Files),
        failures: &mut Vec<Failure>,
    ) {
        let (run, edited) = applied;
        if run.code != Some(9) {
            failures.push(self.failure(
                Invariant::StillParses,
                false,
                None,
                format!(
                    "an edit that cannot be made as written exited {:?}, not 9: {}",
                    run.code,
                    run.stderr.trim()
                ),
            ));
        } else if !run.stderr.contains(why) {
            failures.push(self.failure(
                Invariant::StillParses,
                false,
                None,
                format!(
                    "the edit was refused, but not saying {why:?}, so not for what the case \
                     exercises: {}",
                    run.stderr.trim()
                ),
            ));
        }
        for path in differing(files, edited) {
            failures.push(self.failure(
                Invariant::StillParses,
                false,
                Some(&path),
                "it was written although the edit was refused".into(),
            ));
        }
    }

    /// Every file the oracle `accepted` before it still accepts in `edited`, written under
    /// `dir`; returns the files it accepts there, or `None` if the language has no oracle or it
    /// could not judge them.
    fn still_parses(
        &self,
        accepted: Option<&BTreeSet<String>>,
        dir: &Path,
        edited: &Files,
        again: bool,
        failures: &mut Vec<Failure>,
    ) -> Option<BTreeSet<String>> {
        let (Judge::Oracle(oracle), Some(before)) = (&self.language.judge, accepted) else {
            return None;
        };
        let refused = match oracle.refused(dir, &edited.keys().collect::<Vec<_>>()) {
            Ok(refused) => refused,
            Err(why) => {
                failures.push(self.failure(Invariant::Harness, again, None, why));
                return None;
            }
        };
        for path in before {
            if let Some(reason) = refused.get(path) {
                failures.push(self.failure(
                    Invariant::StillParses,
                    again,
                    Some(path),
                    format!(
                        "the oracle accepted it before the edit and refuses it after: {reason}"
                    ),
                ));
            }
        }
        Some(
            edited
                .keys()
                .filter(|path| !refused.contains_key(*path))
                .cloned()
                .collect(),
        )
    }
}

/// The files the oracle accepts of `files`, written under `dir`.
fn accepted_in(oracle: &Oracle, dir: &Path, files: &Files) -> Result<BTreeSet<String>, String> {
    let refused = oracle.refused(dir, &files.keys().collect::<Vec<_>>())?;
    Ok(files
        .keys()
        .filter(|path| !refused.contains_key(*path))
        .cloned()
        .collect())
}

/// The paths whose bytes differ between `before` and `after`, a file in only one included.
pub fn differing(before: &Files, after: &Files) -> Vec<String> {
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    paths
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

/// Every way `edited` changed `before` outside the `matches` `find` reported: by bytes, and by
/// whole lines.
pub fn minimal_diff(
    before: &Files,
    matches: &BTreeMap<String, Vec<Span>>,
    edited: &Files,
) -> Vec<(String, (Invariant, String))> {
    let mut found = Vec::new();
    for path in differing(before, edited) {
        match (before.get(&path), edited.get(&path)) {
            (Some(_), None) => {
                found.push((path, (Invariant::MinimalDiff, "the edit deleted it".into())));
            }
            (None, _) => found.push((path, (Invariant::MinimalDiff, "the edit created it".into()))),
            (Some(original), Some(after)) => {
                let Some(spans) = matches.get(&path) else {
                    found.push((
                        path,
                        (
                            Invariant::MinimalDiff,
                            "it changed, but find reported no match in it".into(),
                        ),
                    ));
                    continue;
                };
                let Some((encoding, mark)) = encoding_of(&path, original) else {
                    found.push((
                        path,
                        (
                            Invariant::Harness,
                            "find reported matches in it, but its encoding is not one the \
                             harness can decide, so they cannot be placed"
                                .into(),
                        ),
                    ));
                    continue;
                };
                match byte_ranges(original, encoding, mark, spans) {
                    Ok(ranges) => {
                        if let Err(why) = outside_survives(original, &ranges, after) {
                            found.push((path.clone(), (Invariant::MinimalDiff, why)));
                        }
                    }
                    Err(why) => found.push((path.clone(), (Invariant::Harness, why))),
                }
                let lines = line_ranges(original, encoding, mark, spans);
                if let Err(why) = outside_survives(original, &lines, after) {
                    found.push((path, (Invariant::MinimalDiffLines, why)));
                }
            }
        }
    }
    found
}

/// A place in a file as `find` reports it: a line, and a column counting characters, from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// The line, from 1.
    pub line: usize,
    /// The column, from 1.
    pub column: usize,
}

/// A match as `find` reports it: its first and last character, both inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Its first character.
    pub start: Position,
    /// Its last character.
    pub end: Position,
}

/// The matches `find` printed, by file, read from its `path:line:column-line:column: text`
/// lines.  A path is recognised only as one of `paths`, so a path or a text holding `:` cannot
/// be misread.
///
/// # Errors
///
/// If a line is not a match in one of `paths`: the harness would otherwise check fewer matches
/// than were found.
pub fn parse_matches(
    stdout: &str,
    paths: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<Span>>, String> {
    let mut found: BTreeMap<String, Vec<Span>> = BTreeMap::new();
    for line in stdout.lines() {
        let placed = line.match_indices(':').rev().find_map(|(at, _)| {
            let path = &line[..at];
            if !paths.contains(path) {
                return None;
            }
            span(&line[at + 1..]).map(|span| (path, span))
        });
        let (path, span) = placed.ok_or_else(|| {
            format!("find printed {line:?}, which is not `path:line:column-line:column: text` for a file of the project")
        })?;
        found.entry(path.to_owned()).or_default().push(span);
    }
    Ok(found)
}

/// `line:column-line:column: text`, read as a span.
fn span(rest: &str) -> Option<Span> {
    let (place, _) = rest.split_once(": ")?;
    let (start, end) = place.split_once('-')?;
    Some(Span {
        start: position(start)?,
        end: position(end)?,
    })
}

fn position(text: &str) -> Option<Position> {
    let (line, column) = text.split_once(':')?;
    Some(Position {
        line: line.parse().ok()?,
        column: column.parse().ok()?,
    })
}

/// Whether a file is Python to quaff, by its name: the files a coding line applies to.
pub fn is_python(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e == "py" || e == "pyi" || e == "pyw")
}

/// The encoding quaff reads `bytes` in, and how many bytes of byte-order mark come before the
/// text, as far as the file alone decides it: its byte-order mark, then a Python file's coding
/// line, then its bytes being UTF-8.  `None` when none of those decides it, as for quaff with no
/// declaration elsewhere, which [`check`] makes sure the corpus has none of.
pub fn encoding_of(path: &str, bytes: &[u8]) -> Option<(Encoding, usize)> {
    if let Some(found) = byte_order_mark(bytes) {
        return Some(found);
    }
    if is_python(path) {
        match declared_encoding(bytes) {
            Ok(Some((declared, _))) => return Some((declared.into(), 0)),
            Ok(None) => {}
            Err(_) => return None,
        }
    }
    proven_utf8(bytes).ok().map(|()| (Encoding::Utf8, 0))
}

/// Walks the characters of `bytes` after `mark` bytes of byte-order mark, as `find` counts
/// them, calling `visit` with each one's position, the offset of its first byte, and the
/// offset just past its last.  A line ends after LF, after CRLF, or after a CR not followed by
/// LF, and a line ending's characters are columns of the line they end.
fn walk(
    bytes: &[u8],
    encoding: Encoding,
    mark: usize,
    mut visit: impl FnMut(Position, usize, usize),
) {
    let mut place = Position { line: 1, column: 1 };
    let mut previous: Option<(Position, usize)> = None;
    let mut after_cr = false;
    encoding.each_char(&bytes[mark..], |offset, c| {
        let at = mark + offset;
        if let Some((position, start)) = previous.take() {
            visit(position, start, at);
        }
        if after_cr && c != '\n' {
            place = Position {
                line: place.line + 1,
                column: 1,
            };
        }
        after_cr = c == '\r';
        previous = Some((place, at));
        place = if c == '\n' {
            Position {
                line: place.line + 1,
                column: 1,
            }
        } else {
            Position {
                line: place.line,
                column: place.column + 1,
            }
        };
        true
    });
    if let Some((position, start)) = previous {
        visit(position, start, bytes.len());
    }
}

/// The bytes the spans cover in `bytes`, joined where they overlap: a structural match inside
/// another's hole is reported as well as the one round it, and the two are rewritten as one.
///
/// # Errors
///
/// If a span's first or last character is not in the file, or a span starts before the one
/// reported before it: `find` placed them, so either is a fault the harness must report, not
/// guess past.
pub fn byte_ranges(
    bytes: &[u8],
    encoding: Encoding,
    mark: usize,
    spans: &[Span],
) -> Result<Vec<Range<usize>>, String> {
    let wanted: BTreeSet<Position> = spans.iter().flat_map(|s| [s.start, s.end]).collect();
    let mut found: BTreeMap<Position, (usize, usize)> = BTreeMap::new();
    walk(bytes, encoding, mark, |position, start, end| {
        if wanted.contains(&position) {
            found.insert(position, (start, end));
        }
    });
    let mut ranges: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        let place = |position: Position| {
            found.get(&position).copied().ok_or_else(|| {
                format!(
                    "find reported a match at line {}, column {}, which is not in the file",
                    position.line, position.column
                )
            })
        };
        let range = place(span.start)?.0..place(span.end)?.1;
        if range.start >= range.end || ranges.last().is_some_and(|last| last.start > range.start) {
            return Err(format!(
                "find reported matches out of order, at line {}, column {}",
                span.start.line, span.start.column
            ));
        }
        match ranges.last_mut() {
            Some(last) if last.end > range.start => last.end = last.end.max(range.end),
            _ => ranges.push(range),
        }
    }
    Ok(ranges)
}

/// The bytes of the whole lines each span touches in `bytes`, line endings included, joined
/// where they overlap.  Only the spans' line numbers are used, never their columns.
pub fn line_ranges(
    bytes: &[u8],
    encoding: Encoding,
    mark: usize,
    spans: &[Span],
) -> Vec<Range<usize>> {
    // Where each line starts, from line 1; a line past the last starts at the end of the file.
    let mut starts = vec![mark];
    let mut line = 1;
    walk(bytes, encoding, mark, |position, start, _| {
        if position.line > line {
            line = position.line;
            starts.push(start);
        }
    });
    let start_of = |line: usize| starts.get(line - 1).copied().unwrap_or(bytes.len());
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for span in spans {
        let range = start_of(span.start.line)..start_of(span.end.line + 1);
        match ranges.last_mut() {
            Some(last) if last.end >= range.start => last.end = last.end.max(range.end),
            _ => ranges.push(range),
        }
    }
    ranges
}

/// Whether `edited` keeps every byte of `original` outside `inside`, in order: it must start
/// with the text before the first range, end with the text after the last, and hold the text
/// between each pair of ranges in order between them.  What replaced each range is not judged.
///
/// Placing each between-text as early as it can go decides whether any placement exists, since
/// an earlier place leaves more room for the rest.
///
/// # Errors
///
/// Says which text outside the ranges did not survive, and where it was in `original`.
pub fn outside_survives(
    original: &[u8],
    inside: &[Range<usize>],
    edited: &[u8],
) -> Result<(), String> {
    let mut between = Vec::with_capacity(inside.len() + 1);
    let mut from = 0;
    for range in inside {
        between.push(from..range.start);
        from = range.end;
    }
    between.push(from..original.len());
    let line = |offset: usize| memchr::memchr_iter(b'\n', &original[..offset]).count() + 1;
    let first = &original[between[0].clone()];
    let last_range = between.last().expect("at least one").clone();
    let last = &original[last_range.clone()];
    if inside.is_empty() {
        return if original == edited {
            Ok(())
        } else {
            Err("it changed, but nothing in it was matched".into())
        };
    }
    if !edited.starts_with(first) {
        return Err(format!(
            "the text before the first match, up to byte {} (line {}), changed",
            first.len(),
            line(first.len())
        ));
    }
    if edited.len() < first.len() + last.len() || !edited.ends_with(last) {
        return Err(format!(
            "the text after the last match, from byte {} (line {}), changed",
            last_range.start,
            line(last_range.start)
        ));
    }
    let mut at = first.len();
    let limit = edited.len() - last.len();
    for range in &between[1..between.len() - 1] {
        let text = &original[range.clone()];
        match memchr::memmem::find(&edited[at..limit], text) {
            Some(found) => at += found + text.len(),
            None => {
                return Err(format!(
                    "the text between two matches, bytes {}-{} (line {}), is not in the edited \
                     file in order",
                    range.start,
                    range.end,
                    line(range.start)
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
