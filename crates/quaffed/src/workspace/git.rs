// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! git, as a [`Workspace`]: the root is the nearest directory holding `.git`, the ignore files
//! are git's, and a file's encoding may be declared by the `working-tree-encoding` attribute.
//!
//! Attributes are read from the repository's own files only - every `.gitattributes` from the
//! root down to the file's directory, and `info/attributes` in the git directory - and never
//! from git's configuration, so `core.attributesFile` and the system's attributes file do not
//! change an answer.  git's rules for them, from `gitattributes(5)`: within a file a later line
//! overrides an earlier one, a deeper `.gitattributes` overrides a shallower one, and
//! `info/attributes` overrides them all.  Patterns are matched as in `.gitignore`, except that a
//! negative pattern is ignored, and a pattern naming a directory does not match what is inside
//! it.  `[attr]` lines define macros, in the top-level `.gitattributes` and `info/attributes`
//! only.  A file in a repository nested in the project, a submodule among them, takes its
//! attributes from that repository's files alone.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{Declared, Error, Origin, Workspace};

/// The name whose presence marks a project root.
///
/// Tested for existence, not type: a `.git` file - a submodule's or a linked worktree's - marks
/// a root as surely as a `.git` directory.  `.gitignore` is not a marker.
const MARKER: &str = ".git";

/// The attribute that declares a file's encoding in the working tree.
const ENCODING: &str = "working-tree-encoding";

/// A git repository's working tree, and any repositories nested in it.
pub struct Git {
    root: PathBuf,
    /// Each repository asked about, by its root: the outer one, and any nested in it.
    repositories: HashMap<PathBuf, Repository>,
    /// The repository each directory asked about is in, by the nearest `.git` at or above it.
    owners: HashMap<PathBuf, PathBuf>,
    skipped: Vec<String>,
}

/// One repository's attributes files, read as they are needed.
struct Repository {
    /// Each directory's `.gitattributes`, read the first time a file beneath it is asked about.
    files: HashMap<PathBuf, Option<Attributes>>,
    /// `info/attributes`.
    info: Option<Attributes>,
    /// Macros from the top-level `.gitattributes` and `info/attributes`: each one's settings of
    /// the encoding attribute, `None` for a macro that does not touch it.
    macros: HashMap<String, Option<State>>,
}

/// Finds the nearest directory at or above `start` holding `.git`, if any.
pub(super) fn find(start: &Path) -> Option<Box<dyn Workspace>> {
    start
        .ancestors()
        .find(|dir| is_root(dir))
        .map(|root| Box::new(Git::new(root)) as Box<dyn Workspace>)
}

/// Whether `dir` holds `.git`.
fn is_root(dir: &Path) -> bool {
    // symlink_metadata, so a dangling symbolic link named .git still counts as present.
    dir.join(MARKER).symlink_metadata().is_ok()
}

impl Workspace for Git {
    fn root(&self) -> &Path {
        &self.root
    }

    fn ignore_rules(&self, walk: &mut ignore::WalkBuilder) {
        walk.git_ignore(true).git_exclude(true).git_global(false);
    }

    fn declared_encoding(
        &mut self,
        path: &Path,
        shown_from: &Path,
    ) -> Result<Option<Declared>, Error> {
        let Some(dir) = path.parent() else {
            return Ok(None);
        };
        // A file in a nested repository, a submodule among them, takes its attributes from that
        // repository alone, as git does when it works there.
        let root = self.owner(dir);
        if !self.repositories.contains_key(&root) {
            let repository = Repository::read(&root, shown_from, &mut self.skipped)?;
            self.repositories.insert(root.clone(), repository);
        }
        let repository = self
            .repositories
            .get_mut(&root)
            .expect("read above if it was not there");
        // info/attributes first, then each .gitattributes from the deepest up: the first with a
        // line that matches decides.
        if let Some(decided) = repository.info.as_ref().and_then(|info| info.decide(path)) {
            return Ok(decided.declared());
        }
        for dir in path
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&root))
        {
            if !repository.files.contains_key(dir) {
                let attributes = read_file(
                    &dir.join(".gitattributes"),
                    dir,
                    &repository.macros,
                    shown_from,
                    &mut self.skipped,
                )?;
                repository.files.insert(dir.to_path_buf(), attributes);
            }
            if let Some(Some(attributes)) = repository.files.get(dir)
                && let Some(decided) = attributes.decide(path)
            {
                return Ok(decided.declared());
            }
        }
        Ok(None)
    }

    fn skipped(&self) -> &[String] {
        &self.skipped
    }

    fn declares_encodings_in(&self) -> &'static str {
        ".gitattributes"
    }
}

impl Git {
    fn new(root: &Path) -> Git {
        Git {
            root: root.to_path_buf(),
            repositories: HashMap::new(),
            owners: HashMap::new(),
            skipped: Vec::new(),
        }
    }

    /// The root of the repository `dir` is in: the nearest directory at or above it holding
    /// `.git`, and never above the workspace's root.
    fn owner(&mut self, dir: &Path) -> PathBuf {
        if let Some(root) = self.owners.get(dir) {
            return root.clone();
        }
        let root = dir
            .ancestors()
            .take_while(|d| d.starts_with(&self.root))
            .find(|d| is_root(d))
            .unwrap_or(&self.root)
            .to_path_buf();
        self.owners.insert(dir.to_path_buf(), root.clone());
        root
    }
}

impl Repository {
    /// Reads the files macros come from, the top-level `.gitattributes` and `info/attributes`,
    /// and the rules in each.  Each is read once: its macros and its rules come from the same
    /// text, and the top-level file's rules are kept with the root's, so the walk up from a file
    /// never reads it again.
    fn read(
        root: &Path,
        shown_from: &Path,
        skipped: &mut Vec<String>,
    ) -> Result<Repository, Error> {
        let top_file = root.join(".gitattributes");
        let info_file = git_dir(root)?.map(|dir| dir.join("info").join("attributes"));
        let top = read(&top_file)?;
        let info = match &info_file {
            Some(file) => read(file)?,
            None => None,
        };
        let mut macros = HashMap::new();
        for text in [&top, &info].into_iter().flatten() {
            for raw in text.lines() {
                if let Some(Parsed::Macro { name, settings }) = parse(raw) {
                    let state = encoding_in(&settings, &macros);
                    macros.insert(name, state);
                }
            }
        }
        let mut files = HashMap::new();
        let top = top.map(|text| rules_in(&text, &top_file, root, &macros, shown_from, skipped));
        files.insert(root.to_path_buf(), top);
        let info = match (info, &info_file) {
            (Some(text), Some(file)) => {
                Some(rules_in(&text, file, root, &macros, shown_from, skipped))
            }
            _ => None,
        };
        Ok(Repository {
            files,
            info,
            macros,
        })
    }
}

/// The rules in `file` that touch the encoding attribute, matched relative to `base`, or `None`
/// if there is no such file; a line that cannot be used is described in `skipped`, its file
/// shown relative to `shown_from`.
fn read_file(
    file: &Path,
    base: &Path,
    macros: &HashMap<String, Option<State>>,
    shown_from: &Path,
    skipped: &mut Vec<String>,
) -> Result<Option<Attributes>, Error> {
    Ok(read(file)?.map(|text| rules_in(&text, file, base, macros, shown_from, skipped)))
}

/// The rules in `text`, the contents of `file`, that touch the encoding attribute, matched
/// relative to `base`; a line that cannot be used is described in `skipped`, its file shown
/// relative to `shown_from`.
fn rules_in(
    text: &str,
    file: &Path,
    base: &Path,
    macros: &HashMap<String, Option<State>>,
    shown_from: &Path,
    skipped: &mut Vec<String>,
) -> Attributes {
    let shown = crate::output::relative(file, shown_from);
    let mut rules = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        match parse(raw) {
            None | Some(Parsed::Macro { .. }) => {}
            Some(Parsed::Negative) => skipped.push(format!(
                "{shown:?}: line {line}: a negative pattern, which git ignores in an attributes \
                 file"
            )),
            Some(Parsed::Rule { pattern, settings }) => {
                let Some(state) = encoding_in(&settings, macros) else {
                    continue;
                };
                let mut builder = GitignoreBuilder::new(base);
                match builder
                    .add_line(None, &pattern)
                    .and_then(|builder| builder.build())
                {
                    Ok(matcher) => rules.push(Rule {
                        matcher,
                        state,
                        line,
                    }),
                    Err(err) => skipped.push(format!("{shown:?}: line {line}: {err}")),
                }
            }
        }
    }
    Attributes { file: shown, rules }
}

/// The git directory of the repository at `root`, where `info/attributes` is: `.git` itself, or
/// the one a `.git` file points to, or the common directory a linked worktree's points to in
/// turn.
fn git_dir(root: &Path) -> Result<Option<PathBuf>, Error> {
    let marker = root.join(MARKER);
    if marker.is_dir() {
        return Ok(Some(marker));
    }
    let Some(text) = read(&marker)? else {
        return Ok(None);
    };
    let Some(pointed) = text.lines().next().and_then(|l| l.strip_prefix("gitdir:")) else {
        return Ok(None);
    };
    let dir = root.join(pointed.trim());
    Ok(Some(match read(&dir.join("commondir"))? {
        Some(common) => dir.join(common.trim()),
        None => dir,
    }))
}

#[cfg(test)]
thread_local! {
    /// Every file `read` was asked for on this thread, so a test can count them.
    static READS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A file's text, `None` if it does not exist.
fn read(file: &Path) -> Result<Option<String>, Error> {
    #[cfg(test)]
    READS.with(|reads| reads.borrow_mut().push(file.to_path_buf()));
    match fs::read(file) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        // A `.git` file's directory, or a path under a file, is not there to read.
        Err(err) if err.kind() == io::ErrorKind::NotADirectory => Ok(None),
        Err(cause) => Err(Error {
            path: file.to_path_buf(),
            cause,
        }),
    }
}

/// One attributes file's rules that touch the encoding attribute, in order.
struct Attributes {
    /// The file, as a message names it.
    file: PathBuf,
    rules: Vec<Rule>,
}

impl Attributes {
    /// What the last matching rule says of the encoding for `path`, or `None` if no rule
    /// matches.
    fn decide(&self, path: &Path) -> Option<Decision> {
        let rule = self
            .rules
            .iter()
            .rev()
            .find(|rule| rule.matcher.matched(path, false).is_ignore())?;
        Some(match &rule.state {
            State::Value(name) => Decision::Declared(Declared {
                name: name.clone(),
                origin: Origin {
                    kind: ENCODING,
                    file: self.file.clone(),
                    line: rule.line,
                },
            }),
            State::Other => Decision::Nothing,
        })
    }
}

/// What a matching rule decides: an encoding, or - set, unset or unspecified - none.
enum Decision {
    Declared(Declared),
    Nothing,
}

impl Decision {
    fn declared(self) -> Option<Declared> {
        match self {
            Decision::Declared(declared) => Some(declared),
            Decision::Nothing => None,
        }
    }
}

/// One line's pattern and what it sets the encoding attribute to.
struct Rule {
    matcher: Gitignore,
    state: State,
    line: usize,
}

/// What a line sets the encoding attribute to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// A value, the encoding's name.
    Value(String),
    /// Set, unset or unspecified: no encoding.
    Other,
}

/// A line of an attributes file.
#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    /// A pattern and its settings, in order.
    Rule {
        pattern: String,
        settings: Vec<Setting>,
    },
    /// `[attr]name` and its settings.
    Macro {
        name: String,
        settings: Vec<Setting>,
    },
    /// A pattern starting `!`, which git ignores.
    Negative,
}

/// One attribute setting on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Setting {
    /// `name`: set, or a macro used.
    Set(String),
    /// `-name` or `!name`: unset or unspecified.
    Cleared(String),
    /// `name=value`.
    Value(String, String),
}

/// A line parsed as git's `parse_attr_line` does, or `None` for a blank line or a comment.
///
/// Leading whitespace is skipped; a pattern starting `"` is unquoted in git's C style; the rest
/// is settings separated by whitespace.
fn parse(raw: &str) -> Option<Parsed> {
    let line = raw.trim_start_matches([' ', '\t', '\r', '\n']);
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (pattern, rest) = if let Some(quoted) = line.strip_prefix('"') {
        unquote(quoted)?
    } else {
        let end = line.find([' ', '\t', '\r', '\n']).unwrap_or(line.len());
        (line[..end].to_owned(), &line[end..])
    };
    let settings = rest
        .split([' ', '\t', '\r', '\n'])
        .filter(|token| !token.is_empty())
        .map(|token| {
            if let Some(name) = token.strip_prefix(['-', '!']) {
                Setting::Cleared(name.to_owned())
            } else if let Some((name, value)) = token.split_once('=') {
                Setting::Value(name.to_owned(), value.to_owned())
            } else {
                Setting::Set(token.to_owned())
            }
        })
        .collect();
    if let Some(name) = pattern.strip_prefix("[attr]") {
        return Some(Parsed::Macro {
            name: name.to_owned(),
            settings,
        });
    }
    if pattern.starts_with('!') {
        return Some(Parsed::Negative);
    }
    Some(Parsed::Rule { pattern, settings })
}

/// What `settings` set the encoding attribute to, with macros expanded, or `None` if they do not
/// touch it.  A later setting overrides an earlier one, as in git.
fn encoding_in(settings: &[Setting], macros: &HashMap<String, Option<State>>) -> Option<State> {
    let mut state = None;
    for setting in settings {
        match setting {
            Setting::Value(name, value) if name == ENCODING => {
                state = Some(State::Value(value.clone()));
            }
            Setting::Set(name) | Setting::Cleared(name) if name == ENCODING => {
                state = Some(State::Other);
            }
            Setting::Set(name) => {
                if let Some(Some(from_macro)) = macros.get(name) {
                    state = Some(from_macro.clone());
                }
            }
            _ => {}
        }
    }
    state
}

/// A pattern quoted in git's C style, after its opening `"`: the pattern and what follows the
/// closing quote, or `None` if it is never closed, which git treats as no pattern.
fn unquote(quoted: &str) -> Option<(String, &str)> {
    let mut pattern = Vec::new();
    let bytes = quoted.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                return Some((
                    String::from_utf8_lossy(&pattern).into_owned(),
                    &quoted[i + 1..],
                ));
            }
            b'\\' => {
                let next = *bytes.get(i + 1)?;
                let escaped = match next {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0C,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 0x0B,
                    b'\\' | b'"' => next,
                    b'0'..=b'3' => {
                        let digits = bytes.get(i + 1..i + 4)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        i += 2;
                        digits.iter().fold(0, |n, d| n * 8 + (d - b'0'))
                    }
                    _ => return None,
                };
                pattern.push(escaped);
                i += 2;
            }
            other => {
                pattern.push(other);
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(line: &str) -> Vec<Setting> {
        match parse(line) {
            Some(Parsed::Rule { settings, .. } | Parsed::Macro { settings, .. }) => settings,
            other => panic!("{line:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn a_line_is_a_pattern_then_settings() {
        assert_eq!(
            parse("  *.c\tworking-tree-encoding=SHIFT-JIS -text !diff eol=lf"),
            Some(Parsed::Rule {
                pattern: "*.c".into(),
                settings: vec![
                    Setting::Value(ENCODING.into(), "SHIFT-JIS".into()),
                    Setting::Cleared("text".into()),
                    Setting::Cleared("diff".into()),
                    Setting::Value("eol".into(), "lf".into()),
                ],
            })
        );
        assert_eq!(parse("# comment"), None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse("!*.c text"), Some(Parsed::Negative));
    }

    #[test]
    fn a_quoted_pattern_is_unquoted_in_c_style() {
        assert_eq!(
            parse(r#""a b\t\"c\101" working-tree-encoding=X"#),
            Some(Parsed::Rule {
                pattern: "a b\t\"cA".into(),
                settings: vec![Setting::Value(ENCODING.into(), "X".into())],
            })
        );
        assert_eq!(parse(r#""never closed x"#), None);
        assert_eq!(
            unquote(r#"\a\b\f\n\r\t\v\\" rest"#),
            Some(("\u{7}\u{8}\u{c}\n\r\t\u{b}\\".to_owned(), " rest"))
        );
        // An escape git does not know, and an octal one too short or not octal, are no pattern.
        assert_eq!(unquote(r#"\q""#), None);
        assert_eq!(unquote(r#"\08""#), None);
        assert_eq!(unquote(r"\1"), None);
    }

    #[test]
    fn a_pattern_that_cannot_be_matched_is_skipped_with_a_note() {
        let dir = std::env::temp_dir().join(format!("quaffed-git-skip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".gitattributes"), "a{b working-tree-encoding=X\n").unwrap();
        let mut workspace = find(&dir).unwrap();
        let declared = workspace.declared_encoding(&dir.join("x.c"), &dir).unwrap();
        let skipped = workspace.skipped().to_vec();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(declared, None);
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(
            skipped[0].starts_with("\".gitattributes\": line 1: "),
            "{skipped:?}"
        );
    }

    #[test]
    fn the_top_level_attributes_and_info_attributes_are_each_read_once() {
        // Their macros and their rules come from one read, and later questions find them
        // cached, however many files are asked about.
        let dir = std::env::temp_dir().join(format!("quaffed-git-once-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git/info")).unwrap();
        fs::write(
            dir.join(".gitattributes"),
            "[attr]sjis working-tree-encoding=SHIFT-JIS\n*.c sjis\n",
        )
        .unwrap();
        fs::write(
            dir.join(".git/info/attributes"),
            "*.h working-tree-encoding=EUC-JP\n",
        )
        .unwrap();
        READS.with(|reads| reads.borrow_mut().clear());
        let mut workspace = find(&dir).unwrap();
        let names: Vec<_> = ["a.c", "b.c", "a.h", "sub/c.c"]
            .iter()
            .map(|path| {
                workspace
                    .declared_encoding(&dir.join(path), &dir)
                    .unwrap()
                    .map(|d| d.name)
            })
            .collect();
        let reads = READS.with(|reads| reads.borrow().clone());
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            [
                Some("SHIFT-JIS".to_owned()),
                Some("SHIFT-JIS".to_owned()),
                Some("EUC-JP".to_owned()),
                Some("SHIFT-JIS".to_owned()),
            ]
        );
        for file in [dir.join(".gitattributes"), dir.join(".git/info/attributes")] {
            let count = reads.iter().filter(|read| **read == file).count();
            assert_eq!(count, 1, "{} read {count} times: {reads:?}", file.display());
        }
    }

    #[test]
    fn a_git_file_with_no_gitdir_line_has_no_info_attributes() {
        let dir = std::env::temp_dir().join(format!("quaffed-git-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".git"), "not a pointer\n").unwrap();
        let declared = find(&dir)
            .unwrap()
            .declared_encoding(&dir.join("x.c"), &dir)
            .unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(declared, None);
    }

    #[test]
    fn the_workspace_names_where_it_declares_encodings_and_its_errors() {
        let dir = std::env::temp_dir();
        assert_eq!(Git::new(&dir).declares_encodings_in(), ".gitattributes");
        let err = Error {
            path: "a/.gitattributes".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert!(
            err.to_string()
                .starts_with("reading \"a/.gitattributes\": "),
            "{err}"
        );
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn the_last_setting_on_a_line_wins_and_macros_expand() {
        let mut macros = HashMap::new();
        macros.insert(
            "sjis".into(),
            encoding_in(
                &settings("[attr]sjis working-tree-encoding=SHIFT-JIS"),
                &macros,
            ),
        );
        macros.insert(
            "other".into(),
            encoding_in(&settings("[attr]other -text"), &macros),
        );
        assert_eq!(
            encoding_in(&settings("*.c sjis"), &macros),
            Some(State::Value("SHIFT-JIS".into()))
        );
        assert_eq!(encoding_in(&settings("*.c other"), &macros), None);
        assert_eq!(
            encoding_in(&settings("*.c sjis -working-tree-encoding"), &macros),
            Some(State::Other)
        );
        assert_eq!(
            encoding_in(&settings("*.c -working-tree-encoding sjis"), &macros),
            Some(State::Value("SHIFT-JIS".into()))
        );
    }
}
