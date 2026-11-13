//! Every example in the language guide, `docs/language.md`, run against the built `quaff`.
//!
//! The guide teaches by example: a command, then what `quaff` prints for it, standard output and
//! standard error together, and the exit status.  This test builds the guide's example project
//! from the files the guide shows, runs each example's command in a fresh copy of it, and fails,
//! naming the example by its line in the guide, wherever what `quaff` prints, how it exits, or
//! what it leaves in a file the guide shows it leaving is not what the guide shows.  An example
//! it cannot run fails too, rather than being passed over.  `architecture/testing.md` says how
//! to write an example so that it is checked.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The note every structural search over the whole project prints for `docs/notes.md`, which
/// the guide leaves out of every example after the first.
const TEXT_TIER_NOTE: &str = "quaff: 1 text-tier file not searched for structural patterns, \
    which apply to Python files only";

/// What opens a directive, an HTML comment that says how to run the example after it.
const DIRECTIVE_OPEN: &str = "<!-- check:";

/// A file the guide shows, with the line its block opens on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    path: String,
    content: String,
    line: usize,
}

/// One command in an example block, and what the guide shows it printing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Example {
    /// The guide's line the command starts on, counting from 1.
    line: usize,
    /// The command as the guide writes it, after the `$ `, its lines joined by newlines.
    command: String,
    /// The arguments after `quaff`, as a shell would pass them.
    args: Vec<String>,
    /// The directory the command runs from, relative to the project root.
    cwd: String,
    /// The files added to the example project for this command, each as last shown before it.
    with: Vec<Shown>,
    /// The output lines the guide shows, standard output and standard error together.
    output: Vec<String>,
    /// The exit status the guide shows, or 0 where it shows none.
    exit: i32,
    /// The files the guide shows as the command leaves them, each in the first block showing its
    /// path after the example.
    then: Vec<Shown>,
}

/// The guide, read: the example project, the examples, and what could not be read.
#[derive(Debug, Default)]
struct Guide {
    /// The files every example runs against: those shown before the first `##` heading.
    project: Vec<Shown>,
    examples: Vec<Example>,
    /// Each thing in the guide that cannot be checked as written, with its line.
    unreadable: Vec<String>,
}

/// A fenced block, its content with the fence's indentation taken off each line.
struct Block {
    /// The line of the opening fence.
    line: usize,
    info: String,
    lines: Vec<String>,
}

/// How the example after a directive is to be run.
#[derive(Default)]
struct Directive {
    line: usize,
    cwd: Option<String>,
    with: Vec<String>,
    then: Vec<String>,
}

/// Reads the guide's text into its project, its examples and what cannot be checked.
///
/// Never fails: anything it cannot read is recorded in `unreadable`, naming its line, so that
/// the caller reports it with the examples that fail.
fn read_guide(text: &str) -> Guide {
    let lines: Vec<&str> = text.lines().collect();
    let mut reader = Reader::default();
    let mut in_intro = true;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if line.starts_with("## ") {
            in_intro = false;
        }
        if let Some(rest) = trimmed.strip_prefix(DIRECTIVE_OPEN) {
            reader.no_example_after_directive();
            match read_directive(i + 1, rest) {
                Ok(read) => reader.directive = Some(read),
                Err(err) => reader.guide.unreadable.push(err),
            }
            i += 1;
        } else if trimmed.starts_with("```") {
            match read_block(&lines, i) {
                Ok((block, next)) => {
                    reader.block(&block, in_intro);
                    i = next;
                }
                Err(err) => {
                    reader.guide.unreadable.push(err);
                    break;
                }
            }
        } else {
            if !trimmed.is_empty() {
                reader.no_example_after_directive();
            }
            i += 1;
        }
    }
    reader.finish()
}

/// What has been read of the guide so far.
#[derive(Default)]
struct Reader {
    guide: Guide,
    /// Each file shown after the first `##` heading, as most recently shown, by its path.
    shown: BTreeMap<String, Shown>,
    /// Each file shown after the first `##` heading, by its path and line, and whether a
    /// directive has added it.
    used: BTreeMap<(String, usize), bool>,
    /// The directive waiting for the example block after it.
    directive: Option<Directive>,
    /// Each file a directive's `then` expects shown after its example: the example's index in
    /// `guide.examples`, the path, and the directive's line.
    then: Vec<(usize, String, usize)>,
}

impl Reader {
    /// Records the waiting directive, if there is one, as having no example after it.
    fn no_example_after_directive(&mut self) {
        if let Some(old) = self.directive.take() {
            self.guide.unreadable.push(format!(
                "docs/language.md:{}: a check directive with no example block straight after it",
                old.line
            ));
        }
    }

    /// Reads one fenced block: a file the guide shows, an example, or neither.
    fn block(&mut self, block: &Block, in_intro: bool) {
        let directive = self.directive.take();
        if let Some(file) = shown_file(block) {
            if let Some(at) = self.then.iter().position(|(_, path, _)| *path == file.path) {
                // A file shown as an example left it is a result, not an input, so it is not
                // what a later `with` adds.
                let (example, _, _) = self.then.remove(at);
                self.guide.examples[example].then.push(file);
            } else if in_intro {
                self.guide.project.push(file);
            } else {
                self.used.insert((file.path.clone(), file.line), false);
                self.shown.insert(file.path.clone(), file);
            }
            if let Some(old) = directive {
                self.guide.unreadable.push(format!(
                    "docs/language.md:{}: a check directive before a file rather than an example",
                    old.line
                ));
            }
            return;
        }
        if !block.lines.iter().any(|l| l.starts_with("$ ")) {
            if let Some(old) = directive {
                self.guide.unreadable.push(format!(
                    "docs/language.md:{}: a check directive before a block with no `$ ` command",
                    old.line
                ));
            }
            if block.lines.iter().any(|l| l.starts_with("[exit ")) {
                self.guide.unreadable.push(format!(
                    "docs/language.md:{}: a block that shows an exit status but no `$ ` command, \
                     so it cannot be run",
                    block.line
                ));
            }
            return;
        }
        let directive = directive.unwrap_or_default();
        let mut with = Vec::new();
        for path in &directive.with {
            match self.shown.get(path) {
                Some(file) => {
                    self.used.insert((file.path.clone(), file.line), true);
                    with.push(file.clone());
                }
                None => self.guide.unreadable.push(format!(
                    "docs/language.md:{}: the check directive adds {path:?}, which no block \
                     before it shows",
                    directive.line
                )),
            }
        }
        let cwd = directive.cwd.unwrap_or_default();
        let examples = match read_examples(block, &cwd, &with) {
            Ok(examples) => examples,
            Err(err) => {
                self.guide.unreadable.push(err);
                return;
            }
        };
        if !directive.then.is_empty() {
            if examples.len() == 1 {
                let example = self.guide.examples.len();
                for path in directive.then {
                    self.then.push((example, path, directive.line));
                }
            } else {
                self.guide.unreadable.push(format!(
                    "docs/language.md:{}: the check directive's `then` is before a block of {} \
                     commands, so which one leaves the file is not known",
                    directive.line,
                    examples.len()
                ));
            }
        }
        self.guide.examples.extend(examples);
    }

    /// Ends the reading, recording a directive left waiting and every file no directive added.
    fn finish(mut self) -> Guide {
        self.no_example_after_directive();
        for (_, path, line) in &self.then {
            self.guide.unreadable.push(format!(
                "docs/language.md:{line}: the check directive says what the example leaves in \
                 {path:?}, but no block after the example shows it"
            ));
        }
        for ((path, line), was_used) in self.used {
            if !was_used {
                self.guide.unreadable.push(format!(
                    "docs/language.md:{line}: {path:?} is shown, but no example's check \
                     directive adds it, so what the guide says of it is not checked"
                ));
            }
        }
        self.guide
    }
}

/// Reads a directive's settings, `cwd DIR`, `with PATH, ...` and `then PATH, ...`, separated by
/// `;`, from the text after `<!-- check:`.
fn read_directive(line: usize, rest: &str) -> Result<Directive, String> {
    let Some(body) = rest.trim_end().strip_suffix("-->") else {
        return Err(format!(
            "docs/language.md:{line}: a check directive must close with `-->` on its own line"
        ));
    };
    let mut directive = Directive {
        line,
        ..Directive::default()
    };
    for setting in body.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (key, value) = setting.split_once(' ').unwrap_or((setting, ""));
        let value = value.trim();
        match key {
            "cwd" if !value.is_empty() && directive.cwd.is_none() => {
                directive.cwd = Some(value.to_string());
            }
            "with" if !value.is_empty() => {
                directive
                    .with
                    .extend(value.split(',').map(|p| p.trim().to_string()));
            }
            "then" if !value.is_empty() => {
                directive
                    .then
                    .extend(value.split(',').map(|p| p.trim().to_string()));
            }
            _ => {
                return Err(format!(
                    "docs/language.md:{line}: {setting:?} is not a check setting; the settings are \
                     `cwd DIR`, once, `with PATH, ...` and `then PATH, ...`"
                ));
            }
        }
    }
    Ok(directive)
}

/// Reads the fenced block opening at `lines[start]`, returning it and the index after it.
fn read_block(lines: &[&str], start: usize) -> Result<(Block, usize), String> {
    let open = lines[start];
    let indent = open.len() - open.trim_start().len();
    let info = open.trim_start().trim_start_matches('`').trim().to_string();
    let mut content = Vec::new();
    for (offset, line) in lines[start + 1..].iter().enumerate() {
        if line.trim() == "```" {
            let block = Block {
                line: start + 1,
                info,
                lines: content,
            };
            return Ok((block, start + offset + 2));
        }
        let leading = line.len() - line.trim_start().len();
        content.push(line[leading.min(indent)..].to_string());
    }
    Err(format!(
        "docs/language.md:{}: a fenced block that is never closed",
        start + 1
    ))
}

/// The file a block shows, if it is one: a block with a language whose first line is `# PATH`,
/// a label that is not part of the file.
///
/// The path has no space and holds a `.`, so that a block of code that opens with an ordinary
/// comment, such as `# a note`, is not taken for a file.
fn shown_file(block: &Block) -> Option<Shown> {
    if block.info.is_empty() {
        return None;
    }
    let path = block.lines.first()?.strip_prefix("# ")?;
    if path.is_empty() || path.contains(' ') || !path.contains('.') {
        return None;
    }
    let mut content = block.lines[1..].join("\n");
    content.push('\n');
    Some(Shown {
        path: path.to_string(),
        content,
        line: block.line,
    })
}

/// Reads the commands in an example block, each with the output after it.
fn read_examples(block: &Block, cwd: &str, with: &[Shown]) -> Result<Vec<Example>, String> {
    let mut examples = Vec::new();
    let lines = &block.lines;
    let mut i = 0;
    if !lines.first().is_some_and(|l| l.starts_with("$ ")) {
        return Err(format!(
            "docs/language.md:{}: an example block must start with its `$ ` command",
            block.line
        ));
    }
    while i < lines.len() {
        let start = block.line + 1 + i;
        let mut command = lines[i]["$ ".len()..].to_string();
        let args = loop {
            match split_command(&command) {
                Ok(Some(args)) => break args,
                Ok(None) if i + 1 < lines.len() => {
                    i += 1;
                    command.push('\n');
                    command.push_str(&lines[i]);
                }
                Ok(None) => {
                    return Err(format!(
                        "docs/language.md:{start}: the command's quotes are never closed"
                    ));
                }
                Err(err) => return Err(format!("docs/language.md:{start}: {err}")),
            }
        };
        if args.first().map(String::as_str) != Some("quaff") {
            return Err(format!(
                "docs/language.md:{start}: the command is not quaff, so it cannot be checked"
            ));
        }
        i += 1;
        let mut output = Vec::new();
        while i < lines.len() && !lines[i].starts_with("$ ") {
            output.push(lines[i].clone());
            i += 1;
        }
        while output.last().is_some_and(String::is_empty) {
            output.pop();
        }
        let mut exit = 0;
        if let Some(last) = output.last()
            && let Some(code) = last
                .strip_prefix("[exit ")
                .and_then(|rest| rest.strip_suffix(']'))
        {
            exit = code
                .parse()
                .map_err(|_| format!("docs/language.md:{start}: {last:?} is not an exit status"))?;
            output.pop();
        }
        if output.iter().any(|l| l.starts_with("[exit ")) {
            return Err(format!(
                "docs/language.md:{start}: an exit status must be the last line of an example's \
                output"
            ));
        }
        examples.push(Example {
            line: start,
            command,
            args: args[1..].to_vec(),
            cwd: cwd.to_string(),
            with: with.to_vec(),
            output,
            exit,
            then: Vec::new(),
        });
    }
    Ok(examples)
}

/// Splits a command into words as a POSIX shell would, for the quoting the guide uses.
///
/// Returns `Ok(None)` when a quote is still open at the end, so the command continues on the
/// next line, and an error for anything a shell would expand or treat specially, since the
/// command is never given to a shell and so could not be checked as written.
fn split_command(command: &str) -> Result<Option<Vec<String>>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if let Some(done) = word.take() {
                    words.push(done);
                }
            }
            '\'' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return Ok(None),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('$' | '`' | '"' | '\\')) => current.push(escaped),
                            Some('\n') => {}
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Ok(None),
                        },
                        Some(special @ ('$' | '`')) => {
                            return Err(format!(
                                "the command has an unescaped {special:?} in double quotes, \
                                 which a shell would expand"
                            ));
                        }
                        Some(inner) => current.push(inner),
                        None => return Ok(None),
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || "-_./:=,@%+".contains(c) => {
                word.get_or_insert_with(String::new).push(c);
            }
            other => {
                return Err(format!(
                    "the command has {other:?} outside quotes, which a shell would treat \
                     specially, so it cannot be checked without one"
                ));
            }
        }
    }
    if let Some(done) = word {
        words.push(done);
    }
    Ok(Some(words))
}

/// What one run of `quaff` printed, both streams as one, and how it exited.
struct Ran {
    output: String,
    exit: Option<i32>,
}

/// Runs `quaff` with `args` from `cwd`, standard output and standard error into one pipe, so
/// that what is read back is in the order `quaff` wrote it.
fn run_quaff(cwd: &Path, args: &[String]) -> Ran {
    let (mut reader, writer) = std::io::pipe().expect("creating a pipe for quaff's output");
    let mut command = Command::new(env!("CARGO_BIN_EXE_quaff"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(writer.try_clone().expect("cloning the output pipe"))
        .stderr(writer);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("starting quaff from {}: {err}", cwd.display()));
    // The command holds the pipe's write ends; until it is dropped the read below never sees
    // the end of the output.
    drop(command);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .expect("reading quaff's output");
    let status = child.wait().expect("waiting for quaff");
    Ran {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        exit: status.code(),
    }
}

/// A fresh copy of the example project, removed when dropped.
struct ProjectDir(PathBuf);

impl Drop for ProjectDir {
    fn drop(&mut self) {
        // Not checked: a panic here, during the unwinding of a failed test, would abort the
        // whole run and hide that failure.  A directory left behind is cleared by the next
        // build_project that reuses its name.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Creates a fresh directory holding the project and `with`, and an empty `.git`, so that it is
/// a project as the guide's is.
fn build_project(project: &[Shown], with: &[Shown]) -> ProjectDir {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "quaffed-language-guide-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    // A directory left by an earlier run whose process ID this one reuses would hold stale
    // files, so anything but "not there" stops the test.
    match fs::remove_dir_all(&dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            panic!("clearing {} before the test: {err}", dir.display())
        }
        _ => {}
    }
    fs::create_dir_all(dir.join(".git")).expect("creating the project");
    for file in project.iter().chain(with) {
        let path = dir.join(&file.path);
        fs::create_dir_all(path.parent().expect("a shown file has a parent"))
            .expect("creating a project directory");
        fs::write(&path, &file.content).expect("writing a project file");
    }
    ProjectDir(dir)
}

/// Checks one example, returning what is wrong with it, or `None` if it holds.
///
/// The guide's conventions are applied as it states them: the text-tier note is dropped from
/// what `quaff` printed where the example leaves it out, and no exit status shown means 0.
fn check(example: &Example, ran: &Ran) -> Option<String> {
    let mut printed: Vec<&str> = ran.output.lines().collect();
    if !example.output.iter().any(|l| l == TEXT_TIER_NOTE) {
        printed.retain(|l| *l != TEXT_TIER_NOTE);
    }
    let shown: Vec<&str> = example.output.iter().map(String::as_str).collect();
    if printed == shown && ran.exit == Some(example.exit) {
        return None;
    }
    let mut report = format!(
        "docs/language.md:{}: $ {}\n",
        example.line,
        example.command.replace('\n', "\n  ")
    );
    if printed != shown {
        report.push_str("  the guide shows (-), quaff printed (+):\n");
        for line in diff(&shown, &printed) {
            let _ = writeln!(report, "    {line}");
        }
    }
    if ran.exit != Some(example.exit) {
        let exit = ran
            .exit
            .map_or_else(|| "a signal".to_string(), |c| c.to_string());
        let _ = writeln!(
            report,
            "  the guide shows exit {}, quaff exited with {exit}",
            example.exit
        );
    }
    Some(report)
}

/// Checks each file the guide shows an example leaving, returning what is wrong with each.
fn check_left(example: &Example, dir: &Path) -> Vec<String> {
    let mut wrong = Vec::new();
    for file in &example.then {
        let path = dir.join(&file.path);
        let held = match fs::read_to_string(&path) {
            Ok(held) => held,
            Err(err) => {
                wrong.push(format!(
                    "docs/language.md:{}: the guide shows what {:?} holds after the example at \
                     line {}, but reading it failed: {err}",
                    file.line, file.path, example.line
                ));
                continue;
            }
        };
        if held != file.content {
            let shown: Vec<&str> = file.content.lines().collect();
            let held_lines: Vec<&str> = held.lines().collect();
            let mut report = format!(
                "docs/language.md:{}: {:?} after the example at line {}: the guide shows (-), \
                 the file holds (+):\n",
                file.line, file.path, example.line
            );
            for line in diff(&shown, &held_lines) {
                let _ = writeln!(report, "    {line}");
            }
            if shown == held_lines {
                report.push_str(
                    "    (the lines agree; the line endings or the last newline differ)\n",
                );
            }
            wrong.push(report);
        }
    }
    wrong
}

/// A line diff of `old` against `new`, each line prefixed `-`, `+` or a space, by their longest
/// common subsequence.
fn diff(old: &[&str], new: &[&str]) -> Vec<String> {
    let mut common = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i][j] = if old[i] == new[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut lines = Vec::new();
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            lines.push(format!("  {}", old[i]));
            i += 1;
            j += 1;
        } else if j < new.len() && (i == old.len() || common[i][j + 1] >= common[i + 1][j]) {
            lines.push(format!("+ {}", new[j]));
            j += 1;
        } else {
            lines.push(format!("- {}", old[i]));
            i += 1;
        }
    }
    lines
}

/// Runs every example in `text` and returns a report of each that fails or cannot be run.
fn failures(text: &str) -> Vec<String> {
    let guide = read_guide(text);
    let mut failures = guide.unreadable.clone();
    for example in &guide.examples {
        let dir = build_project(&guide.project, &example.with);
        let cwd = dir.0.join(&example.cwd);
        if !cwd.is_dir() {
            failures.push(format!(
                "docs/language.md:{}: the check directive runs it from {:?}, which is not a \
                 directory in the example project",
                example.line, example.cwd
            ));
            continue;
        }
        let ran = run_quaff(&cwd, &example.args);
        failures.extend(check(example, &ran));
        failures.extend(check_left(example, &dir.0));
    }
    failures
}

/// The guide, as it is in the repository.
fn the_guide() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/language.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading the language guide at {}: {err}", path.display()))
}

#[test]
fn every_example_in_the_language_guide_is_what_quaff_does() {
    let guide = the_guide();
    let read = read_guide(&guide);
    assert!(
        !read.project.is_empty() && !read.examples.is_empty(),
        "no example project or no examples were read from docs/language.md, so nothing was \
         checked; the format this test reads is in architecture/testing.md"
    );
    let failures = failures(&guide);
    assert!(
        failures.is_empty(),
        "{} of the language guide's examples are not what quaff does, or cannot be checked; \
         architecture/testing.md says how an example is read:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A small guide in the language guide's format, with one example whose output is `output`.
fn tiny_guide(output: &str) -> String {
    format!(
        "# Guide\n\n```python\n# src/a.py\nx = 1  # TODO\n```\n\n## Section\n\n```\n$ quaff TODO\n\
         {output}\n```\n"
    )
}

#[test]
fn an_example_that_matches_passes() {
    assert_eq!(
        failures(&tiny_guide("src/a.py:1:10-1:13: TODO")),
        Vec::<String>::new()
    );
}

#[test]
fn an_example_whose_output_differs_fails_naming_its_line_and_the_difference() {
    let failures = failures(&tiny_guide("src/a.py:1:9-1:12: TODO"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with("docs/language.md:11: $ quaff TODO\n"),
        "{}",
        failures[0]
    );
    assert!(
        failures[0].contains("- src/a.py:1:9-1:12: TODO"),
        "{}",
        failures[0]
    );
    assert!(
        failures[0].contains("+ src/a.py:1:10-1:13: TODO"),
        "{}",
        failures[0]
    );
}

#[test]
fn an_example_whose_exit_status_differs_fails() {
    let failures = failures(&tiny_guide("src/a.py:1:10-1:13: TODO\n[exit 1]"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("the guide shows exit 1, quaff exited with 0"),
        "{}",
        failures[0]
    );
}

#[test]
fn an_example_that_is_not_a_quaff_command_fails_as_uncheckable() {
    let guide = "# Guide\n\n## Section\n\n```\n$ cat checks.quaff\nfind \"x\"\n```\n";
    assert_eq!(
        failures(guide),
        vec!["docs/language.md:6: the command is not quaff, so it cannot be checked".to_string()]
    );
}

#[test]
fn a_command_a_shell_would_expand_fails_as_uncheckable() {
    let guide = "# Guide\n\n## Section\n\n```\n$ quaff -p \"f($x)\"\n```\n";
    let failures = failures(guide);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with("docs/language.md:6: "),
        "{}",
        failures[0]
    );
    assert!(
        failures[0].contains("which a shell would expand"),
        "{}",
        failures[0]
    );
}

#[test]
fn an_exit_status_with_no_command_fails_as_uncheckable() {
    let guide = "# Guide\n\n## Section\n\n```\nsrc/a.py:1:1-1:1: x\n[exit 0]\n```\n";
    let failures = failures(guide);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("no `$ ` command"), "{}", failures[0]);
}

#[test]
fn a_file_no_example_adds_fails_as_unchecked() {
    let guide = "# Guide\n\n## Section\n\n```python\n# src/b.py\nb = 2\n```\n";
    let failures = failures(guide);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("\"src/b.py\" is shown, but no example"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_directive_adds_the_file_last_shown_and_sets_where_it_runs() {
    let guide = "# Guide\n\n```python\n# src/a.py\na = 1\n```\n\n## Section\n\n\
                 ```python\n# src/b.py\nTODO = 1\n```\n\n\
                 <!-- check: with src/b.py -->\n```\n$ quaff TODO\nsrc/b.py:1:1-1:4: TODO\n```\n\n\
                 ```python\n# src/b.py\nb = 2  # TODO\n```\n\n\
                 <!-- check: cwd src; with src/b.py -->\n```\n$ quaff TODO\n\
                     b.py:1:10-1:13: TODO\n```\n";
    assert_eq!(failures(guide), Vec::<String>::new());
}

#[test]
fn a_file_shown_again_before_any_example_adds_it_fails_as_unchecked() {
    let guide = "# Guide\n\n## Section\n\n\
                 ```python\n# src/b.py\nTODO = 1\n```\n\n\
                 ```python\n# src/b.py\nb = 2  # TODO\n```\n\n\
                 <!-- check: with src/b.py -->\n```\n$ quaff TODO\nsrc/b.py:1:10-1:13: TODO\n```\n";
    assert_eq!(
        failures(guide),
        vec![
            "docs/language.md:5: \"src/b.py\" is shown, but no example's check directive adds \
             it, so what the guide says of it is not checked"
                .to_string()
        ]
    );
}

#[test]
fn a_directive_naming_a_file_not_shown_fails() {
    let guide = "# Guide\n\n## Section\n\n<!-- check: with src/c.py -->\n```\n$ quaff x\n\
        quaff: no matches for \"x\"\n[exit 1]\n```\n";
    let failures = failures(guide);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("adds \"src/c.py\", which no block before it shows")),
        "{failures:?}"
    );
}

#[test]
fn a_command_continues_while_its_quotes_are_open() {
    assert_eq!(split_command("quaff -p 'a"), Ok(None));
    assert_eq!(
        split_command("quaff -p 'a\nb' play"),
        Ok(Some(vec![
            "quaff".to_string(),
            "-p".to_string(),
            "a\nb".to_string(),
            "play".to_string()
        ]))
    );
    assert_eq!(
        split_command(r#"quaff -p "log('x', \$v)""#),
        Ok(Some(vec![
            "quaff".to_string(),
            "-p".to_string(),
            "log('x', $v)".to_string()
        ]))
    );
}

#[test]
fn the_text_tier_note_is_dropped_only_where_the_example_leaves_it_out() {
    let example = Example {
        line: 1,
        command: "quaff -p x".to_string(),
        args: Vec::new(),
        cwd: String::new(),
        with: Vec::new(),
        output: vec!["a".to_string()],
        exit: 0,
        then: Vec::new(),
    };
    let ran = Ran {
        output: format!("a\n{TEXT_TIER_NOTE}\n"),
        exit: Some(0),
    };
    assert_eq!(check(&example, &ran), None);
    let shown_twice = Example {
        output: vec!["a".to_string(), TEXT_TIER_NOTE.to_string()],
        ..example
    };
    let ran_without = Ran {
        output: "a\n".to_string(),
        exit: Some(0),
    };
    assert!(check(&shown_twice, &ran_without).is_some());
}

/// Fails unless reading `guide` reports exactly one problem, and that problem contains `needle`.
fn assert_one_failure(guide: &str, needle: &str) {
    let failures = failures(guide);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains(needle), "{}", failures[0]);
}

#[test]
fn a_fence_never_closed_fails() {
    assert_one_failure(
        "# Guide\n\n## Section\n\n```\n$ quaff TODO\n",
        "docs/language.md:5: a fenced block that is never closed",
    );
}

#[test]
fn a_directive_not_closed_on_its_line_fails() {
    assert_one_failure(
        "# Guide\n\n## Section\n\n<!-- check: cwd src\n-->\n```\n$ quaff x\n\
        quaff: no matches for \"x\"\n[exit 1]\n```\n",
        "docs/language.md:5: a check directive must close with `-->`",
    );
}

#[test]
fn an_unknown_or_repeated_setting_fails() {
    for setting in ["dir src", "cwd src; cwd docs", "with", "cwd"] {
        let guide = format!(
            "# Guide\n\n## Section\n\n<!-- check: {setting} -->\n```\n$ quaff x\n\
            quaff: no matches for \"x\"\n[exit 1]\n```\n"
        );
        assert_one_failure(&guide, "is not a check setting");
    }
}

#[test]
fn a_directive_with_no_example_straight_after_it_fails() {
    for after in [
        "",
        "Some prose.\n\n```\n$ quaff x\nquaff: no matches for \"x\"\n[exit 1]\n```\n",
    ] {
        let guide = format!("# Guide\n\n## Section\n\n<!-- check: cwd src -->\n{after}");
        assert_one_failure(
            &guide,
            "docs/language.md:5: a check directive with no example block straight after it",
        );
    }
}

#[test]
fn a_directive_before_a_file_fails() {
    let guide = "# Guide\n\n## Section\n\n<!-- check: cwd src -->\n\
                 ```python\n# src/b.py\nb = 1\n```\n\n\
                 <!-- check: with src/b.py -->\n```\n$ quaff zzz\nquaff: no matches for \"zzz\"\n\
                     [exit 1]\n```\n";
    assert_one_failure(
        guide,
        "docs/language.md:5: a check directive before a file rather than an example",
    );
}

#[test]
fn a_directive_before_a_block_with_no_command_fails() {
    assert_one_failure(
        "# Guide\n\n## Section\n\n<!-- check: cwd src -->\n```\nsome text\n```\n",
        "docs/language.md:5: a check directive before a block with no `$ ` command",
    );
}

#[test]
fn a_cwd_that_is_not_a_directory_fails() {
    assert_one_failure(
        "# Guide\n\n## Section\n\n<!-- check: cwd nowhere -->\n```\n$ quaff x\n\
        quaff: no matches for \"x\"\n[exit 1]\n```\n",
        "runs it from \"nowhere\", which is not a directory in the example project",
    );
}

#[test]
fn quotes_never_closed_fail() {
    assert_one_failure(
        "# Guide\n\n## Section\n\n```\n$ quaff -e 'find \"x\"\n[exit 1]\n```\n",
        "docs/language.md:6: the command's quotes are never closed",
    );
}

#[test]
fn an_exit_status_that_is_not_last_or_not_a_number_fails() {
    assert_one_failure(
        "# Guide\n\n## Section\n\n```\n$ quaff x\nquaff: no matches for \"x\"\n[exit 1]\nmore\n\
        ```\n",
        "docs/language.md:6: an exit status must be the last line of an example's output",
    );
    assert_one_failure(
        "# Guide\n\n## Section\n\n```\n$ quaff x\n[exit one]\n```\n",
        "docs/language.md:6: \"[exit one]\" is not an exit status",
    );
}

#[test]
fn each_example_s_project_is_removed_after_it_runs() {
    let dir = build_project(
        &[Shown {
            path: "src/a.py".to_string(),
            content: "a = 1\n".to_string(),
            line: 1,
        }],
        &[],
    );
    let path = dir.0.clone();
    assert!(path.join("src/a.py").is_file());
    drop(dir);
    assert!(!path.exists(), "{} was left behind", path.display());
}

/// A guide with one edit example that leaves `src/a.py` holding `left`.
fn edit_guide(left: &str) -> String {
    format!(
        "# Guide\n\n## Section\n\n```python\n# src/a.py\nx = old\n```\n\n\
         <!-- check: with src/a.py; then src/a.py -->\n```\n\
         $ quaff -e 'replace string \"old\" with \"new\" expect 1'\n\
         src/a.py:1:5-1:7: old\nquaff: replaced 1 match in 1 file\n```\n\n\
         leaves:\n\n```python\n# src/a.py\n{left}\n```\n"
    )
}

#[test]
fn a_file_an_example_leaves_as_shown_passes() {
    assert_eq!(failures(&edit_guide("x = new")), Vec::<String>::new());
}

#[test]
fn a_file_an_example_leaves_otherwise_fails_with_a_diff() {
    let failures = failures(&edit_guide("x = newer"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with(
            "docs/language.md:19: \"src/a.py\" after the example at line 12: the guide shows (-), \
             the file holds (+):"
        ),
        "{}",
        failures[0]
    );
    assert!(failures[0].contains("- x = newer"), "{}", failures[0]);
    assert!(failures[0].contains("+ x = new"), "{}", failures[0]);
}

#[test]
fn a_then_with_no_block_after_it_fails() {
    let guide = edit_guide("x = new");
    let cut = &guide[..guide.find("leaves:").expect("the guide has a result")];
    assert_one_failure(
        cut,
        "docs/language.md:10: the check directive says what the example leaves in \"src/a.py\", \
         but no block after the example shows it",
    );
}

#[test]
fn a_then_before_several_commands_fails() {
    let guide = "# Guide\n\n## Section\n\n<!-- check: then src/a.py -->\n```\n$ quaff zzz\n\
                 quaff: no matches for \"zzz\"\n[exit 1]\n\n$ quaff zzz\n\
                 quaff: no matches for \"zzz\"\n[exit 1]\n```\n";
    assert_one_failure(
        guide,
        "the check directive's `then` is before a block of 2 commands",
    );
}

#[test]
fn a_result_block_is_not_what_a_later_with_adds() {
    let guide = format!(
        "{}\n<!-- check: with src/a.py -->\n```\n$ quaff old\nsrc/a.py:1:5-1:7: old\n```\n",
        edit_guide("x = new")
    );
    assert_eq!(failures(&guide), Vec::<String>::new());
}
