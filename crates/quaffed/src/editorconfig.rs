// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The `charset` `EditorConfig` declares for a file.
//!
//! `EditorConfig` is the same under every version control system, so it is read here rather than
//! by a [`crate::workspace::Workspace`].  Its rules, from the `EditorConfig` specification: the
//! `.editorconfig` files from the file's directory up are read, stopping at one whose preamble
//! says `root = true`; a nearer file overrides a farther one, and within a file a later section
//! overrides an earlier one.  quaff also stops at the project root, so that a user's own
//! `.editorconfig` above the project cannot change an answer.  Only `charset` is read.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::contents::Contents;
use crate::workspace::{Declared, Error, Origin};

/// The file `EditorConfig` keeps its settings in.
const FILE: &str = ".editorconfig";

/// The property that declares a file's encoding.
const CHARSET: &str = "charset";

/// The `.editorconfig` files under a boundary, each read once.
pub struct EditorConfig {
    /// The directory the search stops at: the project root, or where quaff runs.
    boundary: PathBuf,
    files: HashMap<PathBuf, Option<File>>,
    /// Where files are read through, so an `.editorconfig` also searched is read once.
    contents: Contents,
}

/// One `.editorconfig` file's `root` setting and its sections that set `charset`.
struct File {
    /// The file, as a message names it.
    shown: PathBuf,
    root: bool,
    /// Each section's glob, the `charset` it sets, and the line setting it, in order.
    sections: Vec<(Vec<Node>, String, usize)>,
}

impl EditorConfig {
    /// Reads `.editorconfig` files at or below `boundary` as they are asked for, through
    /// `contents`.
    pub fn new(boundary: &Path, contents: &Contents) -> Self {
        contents.keep_named(FILE);
        EditorConfig {
            boundary: boundary.to_path_buf(),
            files: HashMap::new(),
            contents: contents.clone(),
        }
    }

    /// The `charset` declared for the file at `path`, under the boundary, if any.
    ///
    /// `unset` declares nothing.  Any other value is returned as it is spelled, lower-cased as
    /// `EditorConfig` compares values.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when a `.editorconfig` that exists cannot be read.
    pub fn charset(&mut self, path: &Path, shown_from: &Path) -> Result<Option<Declared>, Error> {
        let dirs: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&self.boundary))
            .map(Path::to_path_buf)
            .collect();
        // Nearest first, so the first file with a matching section decides; a root file ends
        // the search.
        for dir in dirs {
            if !self.files.contains_key(&dir) {
                let file = read(&self.contents, &dir, shown_from)?;
                self.files.insert(dir.clone(), file);
            }
            let Some(Some(file)) = self.files.get(&dir) else {
                continue;
            };
            let relative = path
                .strip_prefix(&dir)
                .expect("a file is under each directory above it");
            let relative: Vec<char> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
                .chars()
                .collect();
            if let Some((_, value, line)) = file
                .sections
                .iter()
                .rev()
                .find(|(glob, _, _)| matches(glob, &relative))
            {
                return Ok((value != "unset").then(|| Declared {
                    name: value.clone(),
                    origin: Origin {
                        kind: CHARSET,
                        file: file.shown.clone(),
                        line: *line,
                    },
                }));
            }
            if file.root {
                break;
            }
        }
        Ok(None)
    }
}

/// Reads `dir`'s `.editorconfig`, `None` if it has none.
fn read(contents: &Contents, dir: &Path, shown_from: &Path) -> Result<Option<File>, Error> {
    let path = dir.join(FILE);
    let text = match contents.read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => return Err(Error { path, cause }),
    };
    let mut file = File {
        shown: crate::output::relative(&path, shown_from),
        root: false,
        sections: Vec::new(),
    };
    let mut section: Option<Vec<Node>> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(compile(header));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        match &section {
            None if key == "root" => file.root = value == "true",
            Some(glob) if key == CHARSET => file.sections.push((glob.clone(), value, index + 1)),
            _ => {}
        }
    }
    Ok(Some(file))
}

/// One piece of a section's glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    /// This character.
    Char(char),
    /// `?`: any one character but `/`.
    One,
    /// `*`: any characters but `/`.
    Star,
    /// `**`: any characters at all.
    Stars,
    /// `[...]`: one character in the set, or with `!` first, not in it.
    Class {
        negated: bool,
        items: Vec<(char, char)>,
    },
    /// `{a,b}`: any one of the alternatives.
    Either(Vec<Vec<Node>>),
    /// `{n..m}`: a whole number from `n` to `m`.
    Numbers(i64, i64),
}

/// A section header's glob, compiled.  A glob with no `/` outside brackets matches a file at
/// any depth below the `.editorconfig`; one with a `/` matches from its directory, a leading `/`
/// only anchoring it there.
fn compile(header: &str) -> Vec<Node> {
    let chars: Vec<char> = header.chars().collect();
    let has_slash = slash_outside_brackets(&chars);
    let body: &[char] = match chars.first() {
        Some('/') => &chars[1..],
        _ => &chars,
    };
    let mut nodes = Vec::new();
    if !has_slash {
        nodes.extend([Node::Stars, Node::Char('/')]);
        // `**/x` must also match `x` at the top: an empty prefix.
        let mut top = Vec::new();
        parse(body, &mut 0, &mut top, false);
        let mut deeper = nodes;
        deeper.extend(top.clone());
        return vec![Node::Either(vec![top, deeper])];
    }
    parse(body, &mut 0, &mut nodes, false);
    nodes
}

/// Whether `chars` hold a `/` that is not inside brackets.
fn slash_outside_brackets(chars: &[char]) -> bool {
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            '/' if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Parses glob characters from `*at` into `out`, until the end or, inside braces, an unescaped
/// `,` or `}`.
fn parse(chars: &[char], at: &mut usize, out: &mut Vec<Node>, in_braces: bool) {
    while *at < chars.len() {
        let c = chars[*at];
        match c {
            ',' | '}' if in_braces => return,
            '\\' if *at + 1 < chars.len() => {
                out.push(Node::Char(chars[*at + 1]));
                *at += 2;
            }
            // `/**/` is zero or more directories, so `a/**/b` matches `a/b` as well as `a/x/b`.
            '/' if chars.get(*at + 1..*at + 4) == Some(&['*', '*', '/']) => {
                out.push(Node::Either(vec![
                    vec![Node::Char('/')],
                    vec![Node::Char('/'), Node::Stars, Node::Char('/')],
                ]));
                *at += 4;
            }
            '*' if chars.get(*at + 1) == Some(&'*') => {
                out.push(Node::Stars);
                *at += 2;
            }
            '*' => {
                out.push(Node::Star);
                *at += 1;
            }
            '?' => {
                out.push(Node::One);
                *at += 1;
            }
            '[' => {
                if let Some((node, end)) = class(chars, *at) {
                    out.push(node);
                    *at = end;
                } else {
                    out.push(Node::Char('['));
                    *at += 1;
                }
            }
            '{' => {
                if let Some((node, end)) = braces(chars, *at) {
                    out.push(node);
                    *at = end;
                } else {
                    out.push(Node::Char('{'));
                    *at += 1;
                }
            }
            _ => {
                out.push(Node::Char(c));
                *at += 1;
            }
        }
    }
}

/// A `[...]` class starting at `start`, and the index after it, or `None` if it is never closed
/// or holds a `/`, when the `[` is literal.
fn class(chars: &[char], start: usize) -> Option<(Node, usize)> {
    let mut i = start + 1;
    let negated = chars.get(i) == Some(&'!');
    if negated {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    while i < chars.len() {
        let mut c = chars[i];
        if c == ']' && !first {
            return Some((Node::Class { negated, items }, i + 1));
        }
        if c == '/' {
            return None;
        }
        if c == '\\' {
            i += 1;
            c = *chars.get(i)?;
        }
        first = false;
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            let end = chars[i + 2];
            items.push((c, end));
            i += 3;
        } else {
            items.push((c, c));
            i += 1;
        }
    }
    None
}

/// A `{...}` group starting at `start`, and the index after it: `{n..m}` numbers, or `{a,b}`
/// alternatives.  `None` if it is never closed, or has no `,` and is not numbers, when the `{`
/// is literal as `EditorConfig` has it.
fn braces(chars: &[char], start: usize) -> Option<(Node, usize)> {
    let close = matching_brace(chars, start)?;
    let inside: String = chars[start + 1..close].iter().collect();
    if let Some((low, high)) = inside.split_once("..")
        && let (Ok(low), Ok(high)) = (low.parse::<i64>(), high.parse::<i64>())
    {
        return Some((Node::Numbers(low.min(high), low.max(high)), close + 1));
    }
    let mut alternatives = Vec::new();
    let mut at = start + 1;
    loop {
        let mut alternative = Vec::new();
        parse(&chars[..close], &mut at, &mut alternative, true);
        alternatives.push(alternative);
        if at >= close {
            break;
        }
        at += 1;
    }
    if alternatives.len() < 2 {
        return None;
    }
    Some((Node::Either(alternatives), close + 1))
}

/// The index of the `}` closing the `{` at `start`, minding nesting and escapes.
fn matching_brace(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `glob` matches the whole of `text`.
fn matches(glob: &[Node], text: &[char]) -> bool {
    match_from(glob, text, 0)
}

fn match_from(glob: &[Node], text: &[char], at: usize) -> bool {
    let Some((node, rest)) = glob.split_first() else {
        return at == text.len();
    };
    match node {
        Node::Char(c) => text.get(at) == Some(c) && match_from(rest, text, at + 1),
        Node::One => text.get(at).is_some_and(|&c| c != '/') && match_from(rest, text, at + 1),
        Node::Star => {
            let mut end = at;
            loop {
                if match_from(rest, text, end) {
                    return true;
                }
                if end >= text.len() || text[end] == '/' {
                    return false;
                }
                end += 1;
            }
        }
        Node::Stars => (at..=text.len()).any(|end| match_from(rest, text, end)),
        Node::Class { negated, items } => {
            text.get(at).is_some_and(|&c| {
                c != '/' && items.iter().any(|&(low, high)| low <= c && c <= high) != *negated
            }) && match_from(rest, text, at + 1)
        }
        Node::Either(alternatives) => alternatives.iter().any(|alternative| {
            let joined: Vec<Node> = alternative.iter().chain(rest).cloned().collect();
            match_from(&joined, text, at)
        }),
        Node::Numbers(low, high) => {
            let sign = usize::from(text.get(at) == Some(&'-'));
            let digits = text[(at + sign).min(text.len())..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            (1..=digits).any(|width| {
                let end = at + sign + width;
                let number: String = text[at..end].iter().collect();
                number.parse::<i64>().is_ok_and(|n| *low <= n && n <= *high)
                    && match_from(rest, text, end)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn glob_matches(glob: &str, path: &str) -> bool {
        matches(&compile(glob), &path.chars().collect::<Vec<_>>())
    }

    #[test]
    fn a_glob_without_a_slash_matches_at_any_depth() {
        assert!(glob_matches("*.py", "a.py"));
        assert!(glob_matches("*.py", "src/deep/a.py"));
        assert!(!glob_matches("*.py", "a.pyc"));
        assert!(glob_matches("Makefile", "sub/Makefile"));
    }

    #[test]
    fn a_glob_with_a_slash_matches_from_its_directory() {
        assert!(glob_matches("src/*.py", "src/a.py"));
        assert!(!glob_matches("src/*.py", "x/src/a.py"));
        assert!(!glob_matches("src/*.py", "src/deep/a.py"));
        assert!(glob_matches("/src/**.py", "src/deep/a.py"));
        assert!(glob_matches("src/**/a.py", "src/x/y/a.py"));
    }

    #[test]
    fn classes_braces_and_numbers() {
        assert!(glob_matches("[ab].c", "a.c"));
        assert!(!glob_matches("[!ab].c", "a.c"));
        assert!(glob_matches("[a-c]x", "bx"));
        assert!(glob_matches("*.{py,pyi}", "m.pyi"));
        assert!(!glob_matches("*.{py,pyi}", "m.pyc"));
        assert!(glob_matches("{a,{b,c}}.x", "c.x"));
        assert!(glob_matches("file{1..12}.txt", "file9.txt"));
        assert!(!glob_matches("file{1..12}.txt", "file13.txt"));
        assert!(glob_matches("n{-3..3}", "n-2"));
        // A brace with no comma, and one never closed, are literal.
        assert!(glob_matches("{single}.c", "{single}.c"));
        assert!(glob_matches("{open.c", "{open.c"));
        assert!(glob_matches(r"\*.c", "*.c"));
        assert!(!glob_matches(r"\*.c", "a.c"));
    }

    #[test]
    fn a_question_mark_is_one_character_but_a_slash() {
        assert!(glob_matches("a?.c", "ab.c"));
        assert!(!glob_matches("a?.c", "a.c"));
        assert!(!glob_matches("src/a?b", "src/a/b"));
    }

    #[test]
    fn a_class_never_closed_or_holding_a_slash_is_literal() {
        assert!(glob_matches("[ab.c", "[ab.c"));
        assert!(glob_matches("x/[a/b]", "x/[a/b]"));
        // An escaped bracket inside a class is a member of it.
        assert!(glob_matches(r"[\]a]x", "]x"));
        assert!(glob_matches(r"[a\]]x", "ax"));
        // A class whose escape runs off the end is literal too.
        assert!(glob_matches(r"[a\", r"[a\"));
    }

    #[test]
    fn an_escaped_brace_does_not_open_or_close_a_group() {
        assert!(glob_matches(r"{a,\}}.x", "}.x"));
        assert!(glob_matches(r"{a,\{}.x", "{.x"));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_error_naming_it() {
        let dir = std::env::temp_dir().join(format!("quaffed-editorconfig-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // A directory where the file should be cannot be read as one.
        fs::create_dir_all(dir.join(FILE)).unwrap();
        let err = EditorConfig::new(&dir, &Contents::default())
            .charset(&dir.join("a.txt"), &dir)
            .unwrap_err();
        let _ = fs::remove_dir_all(&dir);
        assert!(err.path.ends_with(FILE), "{err}");
    }

    #[test]
    fn lines_outside_a_section_and_keys_it_does_not_read_are_passed_over() {
        let dir =
            std::env::temp_dir().join(format!("quaffed-editorconfig-keys-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(FILE),
            "no equals sign here\nindent_style = tab\n[*]\nindent_size = 4\ncharset = latin1\n",
        )
        .unwrap();
        let declared = EditorConfig::new(&dir, &Contents::default())
            .charset(&dir.join("a.txt"), &dir)
            .unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            declared.map(|d| (d.name, d.origin.line)),
            Some(("latin1".into(), 5))
        );
    }
}
