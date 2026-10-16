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
//! only.

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

/// A git repository's working tree.
pub struct Git {
    root: PathBuf,
    /// Each directory's `.gitattributes`, read the first time a file beneath it is asked about.
    files: HashMap<PathBuf, Option<Attributes>>,
    /// `info/attributes`, read with the first question, with the macros.
    info: Option<Attributes>,
    /// Macros from the top-level `.gitattributes` and `info/attributes`: each one's settings of
    /// the encoding attribute, `None` for a macro that does not touch it.
    macros: Option<HashMap<String, Option<State>>>,
    skipped: Vec<String>,
}

/// Finds the nearest directory at or above `start` holding `.git`, if any.
pub(super) fn find(start: &Path) -> Option<Box<dyn Workspace>> {
    start
        .ancestors()
        // symlink_metadata, so a dangling symbolic link named .git still counts as present.
        .find(|dir| dir.join(MARKER).symlink_metadata().is_ok())
        .map(|root| {
            Box::new(Git {
                root: root.to_path_buf(),
                files: HashMap::new(),
                info: None,
                macros: None,
                skipped: Vec::new(),
            }) as Box<dyn Workspace>
        })
}

impl Workspace for Git {
    fn root(&self) -> &Path {
        &self.root
    }

    fn ignore_rules(&self, walk: &mut ignore::WalkBuilder) {
        walk.git_ignore(true).git_exclude(true).git_global(false);
    }

    fn declared_encoding(&mut self, path: &Path) -> Result<Option<Declared>, Error> {
        self.read_top_level()?;
        // info/attributes first, then each .gitattributes from the deepest up: the first with a
        // line that matches decides.
        let dirs: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&self.root))
            .map(Path::to_path_buf)
            .collect();
        if let Some(decided) = self.info.as_ref().and_then(|info| info.decide(path)) {
            return Ok(decided.declared());
        }
        for dir in dirs {
            self.read_dir(&dir)?;
            if let Some(Some(attributes)) = self.files.get(&dir)
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
    /// Reads the files macros come from, once: the top-level `.gitattributes` and
    /// `info/attributes`.
    fn read_top_level(&mut self) -> Result<(), Error> {
        if self.macros.is_some() {
            return Ok(());
        }
        let mut macros = HashMap::new();
        let top = self.root.join(".gitattributes");
        let info = self
            .git_dir()?
            .map(|dir| dir.join("info").join("attributes"));
        for file in [Some(top), info.clone()].into_iter().flatten() {
            if let Some(text) = read(&file)? {
                for raw in text.lines() {
                    if let Some(Parsed::Macro { name, settings }) = parse(raw) {
                        macros.insert(name, encoding_in(&settings, &macros));
                    }
                }
            }
        }
        self.macros = Some(macros);
        self.info = match info {
            Some(file) => self.read_file(&file, &self.root.clone())?,
            None => None,
        };
        Ok(())
    }

    /// Reads `dir`'s `.gitattributes`, once.
    fn read_dir(&mut self, dir: &Path) -> Result<(), Error> {
        if self.files.contains_key(dir) {
            return Ok(());
        }
        let attributes = self.read_file(&dir.join(".gitattributes"), dir)?;
        self.files.insert(dir.to_path_buf(), attributes);
        Ok(())
    }

    /// The rules in `file` that touch the encoding attribute, matched relative to `base`, or
    /// `None` if there is no such file.
    fn read_file(&mut self, file: &Path, base: &Path) -> Result<Option<Attributes>, Error> {
        let Some(text) = read(file)? else {
            return Ok(None);
        };
        let macros = self.macros.clone().unwrap_or_default();
        let shown = super::super::output::relative(file, &self.root);
        let mut rules = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            match parse(raw) {
                None | Some(Parsed::Macro { .. }) => {}
                Some(Parsed::Negative) => self.skipped.push(format!(
                    "{shown:?}: line {line}: a negative pattern, which git ignores in an \
                     attributes file"
                )),
                Some(Parsed::Rule { pattern, settings }) => {
                    let Some(state) = encoding_in(&settings, &macros) else {
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
                        Err(err) => self.skipped.push(format!("{shown:?}: line {line}: {err}")),
                    }
                }
            }
        }
        Ok(Some(Attributes { file: shown, rules }))
    }

    /// The repository's git directory, where `info/attributes` is: `.git` itself, or the one a
    /// `.git` file points to, or the common directory a linked worktree's points to in turn.
    fn git_dir(&self) -> Result<Option<PathBuf>, Error> {
        let marker = self.root.join(MARKER);
        if marker.is_dir() {
            return Ok(Some(marker));
        }
        let Some(text) = read(&marker)? else {
            return Ok(None);
        };
        let Some(pointed) = text.lines().next().and_then(|l| l.strip_prefix("gitdir:")) else {
            return Ok(None);
        };
        let dir = self.root.join(pointed.trim());
        Ok(Some(match read(&dir.join("commondir"))? {
            Some(common) => dir.join(common.trim()),
            None => dir,
        }))
    }
}

/// A file's text, `None` if it does not exist.
fn read(file: &Path) -> Result<Option<String>, Error> {
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
