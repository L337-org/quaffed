// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Which files a statement looks at: the scope its `in` and the command line name, and the
//! `FILE` conditions of its `where`, which are known before a file is parsed.
//!
//! The command-line scope, `in` and `FILE` follow one set of path rules
//! (`docs/language.md`, _Paths and globs_):
//!
//! - **A path is relative to the current directory**, as the output prints it, so `..` is an
//!   ordinary segment to a glob.  A path that begins with `:/` starts at the project root
//!   instead, or the current directory with no project, and an absolute path is where it says.
//! - **A path with no wildcard names a file or a directory**, and a directory everything under
//!   it, by where it is on disk, so `in "src"` is the same as the scope `src` on the command line.
//! - **A glob** is matched against the whole path, `*`, `?` and a class within one directory
//!   name and `**` across them.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use quaffed_representation::glob::{Glob, GlobPart};
use quaffed_representation::program::{Filter, Operand, Subject};
use quaffed_representation::span::{Span, Spanned};

use crate::glob::{Kind, Matcher, TooLarge};

/// A file's path, as the globs that test it see it.
pub struct Place<'a> {
    /// Where it is.
    pub path: &'a Path,
    /// Relative to the current directory, with `/` between its segments.
    pub from_cwd: String,
    /// Relative to the project root, if it lies under it, with `/` between its segments.
    pub from_root: Option<String>,
}

impl<'a> Place<'a> {
    /// The place of `path`, absolute and canonical, as are `cwd` and `root`.
    pub fn of(path: &'a Path, cwd: &Path, root: &Path) -> Self {
        Place {
            path,
            from_cwd: slashed(&crate::output::relative(path, cwd)),
            from_root: path.strip_prefix(root).ok().map(slashed),
        }
    }
}

/// A relative path with `/` between its segments, whatever the system writes, so that a glob
/// means the same everywhere.  A segment that is not Unicode is read with replacement
/// characters, which only a wildcard can match, as no script can spell it.
fn slashed(path: &Path) -> String {
    let segments: Vec<String> = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    segments.join("/")
}

/// Where a path glob starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// The current directory.
    Cwd,
    /// The project root, written `:/`.
    Root,
    /// Nowhere but where it says: an absolute path.
    Absolute,
}

/// A path glob, compiled: an `in`, or a `FILE` test.
pub struct PathGlob {
    from: From,
    test: Test,
}

enum Test {
    /// A path with no wildcard: the file or directory it names and everything under it, by where
    /// it is, or nothing when there is none.
    Named(Option<PathBuf>),
    /// A glob, matched against the path as written from where it starts.
    Pattern(Matcher),
}

/// What compiling a path glob could not do.
#[derive(Debug)]
pub enum Error {
    /// The glob is too large to match.
    TooLarge(TooLarge),
    /// The path it names could not be looked up, other than for not being there.
    Io {
        /// The path, as written.
        path: PathBuf,
        /// The system's error, verbatim.
        cause: io::Error,
    },
}

/// The roots a path glob is resolved against.
#[derive(Debug, Clone, Copy)]
pub struct Roots<'a> {
    /// The current directory.
    pub cwd: &'a Path,
    /// The project root, or the current directory with no project.
    pub root: &'a Path,
}

impl PathGlob {
    /// Compiles `glob` as a path.  `anywhere` is `contains`: the text anywhere in the path, so a
    /// path with no wildcard is not then a file or directory it names.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] for a glob too large to match, or a named path that could not be
    /// looked up for a reason other than not being there.
    pub fn new(glob: &Glob, anywhere: bool, roots: Roots) -> Result<PathGlob, Error> {
        // `contains` is text anywhere in the path as printed, so nothing in it says where the
        // path starts: `FILE contains "./x"` is those three characters, not `x`.
        let (from, glob) = if anywhere {
            (From::Cwd, glob.clone())
        } else {
            from(glob)
        };
        let test = match glob.as_literal() {
            Some(path) if !anywhere => Test::Named(named(path, from, roots)?),
            _ => Test::Pattern(Matcher::new(&glob, Kind::Path, anywhere).map_err(Error::TooLarge)?),
        };
        Ok(PathGlob { from, test })
    }

    /// Whether `place` matches.
    #[must_use]
    pub fn is_match(&self, place: &Place) -> bool {
        match &self.test {
            Test::Named(None) => false,
            Test::Named(Some(named)) => place.path.starts_with(named),
            Test::Pattern(matcher) => match self.from {
                From::Cwd => matcher.is_match(&place.from_cwd),
                From::Root => place
                    .from_root
                    .as_deref()
                    .is_some_and(|path| matcher.is_match(path)),
                From::Absolute => matcher.is_match(&absolute(place.path)),
            },
        }
    }

    /// What this names on disk, which tells discovery where to look.
    #[must_use]
    pub fn names(&self) -> Names<'_> {
        match &self.test {
            Test::Named(Some(path)) => Names::There(path),
            Test::Named(None) => Names::NotThere,
            Test::Pattern(_) => Names::Glob,
        }
    }
}

/// What a path glob names on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Names<'a> {
    /// A path with no wildcard, and what is there.
    There(&'a Path),
    /// A path with no wildcard, and nothing there.
    NotThere,
    /// A glob, which names files only as they are found.
    Glob,
}

/// An absolute path as a glob sees it, with `/` between its segments.
fn absolute(path: &Path) -> String {
    let written = path.to_string_lossy();
    if cfg!(windows) {
        written.replace('\\', "/")
    } else {
        written.into_owned()
    }
}

/// Where `glob` starts, and the glob with what said so taken off: a leading `:/` for the
/// project root, and any leading `./`, which a printed path never has.  An absolute path is
/// left as it is, to be matched against where a file is.
fn from(glob: &Glob) -> (From, Glob) {
    let mut rest = Glob::default();
    let mut from = From::Cwd;
    for (i, part) in glob.parts().iter().enumerate() {
        match part {
            GlobPart::Literal(text) if i == 0 && Path::new(text).is_absolute() => {
                from = From::Absolute;
                rest.push_literal(text.clone());
            }
            GlobPart::Literal(text) if i == 0 => {
                let mut text = text.as_str();
                if let Some(after) = text.strip_prefix(":/") {
                    from = From::Root;
                    text = after;
                }
                // `./:/x` is a directory named `:`, so `./` comes off after `:/` is looked for.
                while let Some(after) = text.strip_prefix("./") {
                    text = after.trim_start_matches('/');
                }
                if text == "." {
                    text = "";
                }
                rest.push_literal(text.to_owned());
            }
            other => rest.push(other.clone()),
        }
    }
    (from, rest)
}

/// The file or directory `path`, with no wildcard, names from where it starts: canonical, or
/// `None` when there is nothing there.
fn named(path: &str, from: From, roots: Roots) -> Result<Option<PathBuf>, Error> {
    // An absolute path joined to anything is itself.
    let start = match from {
        From::Cwd | From::Absolute => roots.cwd,
        From::Root => roots.root,
    };
    match fs::canonicalize(start.join(path)) {
        Ok(found) => Ok(Some(found)),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(cause) => Err(Error::Io {
            path: PathBuf::from(path),
            cause,
        }),
    }
}

/// What a `where` says about a file before the file is parsed: its `FILE` tests, with every
/// test of a metavariable not known until a match binds it.
pub enum FileTest {
    /// Every part holds.
    And(Vec<FileTest>),
    /// The part does not hold.
    Not(Box<FileTest>),
    /// The file's path matches.
    File(PathGlob),
    /// A test of a metavariable, which only a match can answer.
    Later,
}

impl FileTest {
    /// The test `filter` makes of a file.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`], with where its glob was written, for a `FILE` glob that cannot be
    /// compiled.
    pub fn new(filter: &Spanned<Filter>, roots: Roots) -> Result<FileTest, (Error, Span)> {
        Ok(match &filter.node {
            Filter::And(parts) => FileTest::And(
                parts
                    .iter()
                    .map(|part| FileTest::new(part, roots))
                    .collect::<Result<_, _>>()?,
            ),
            Filter::Not(part) => FileTest::Not(Box::new(FileTest::new(part, roots)?)),
            Filter::Matches(Subject::File, operand) | Filter::Contains(Subject::File, operand) => {
                let Operand::Glob(glob) = &operand.node else {
                    // The parser compares `FILE` with globs alone.
                    return Ok(FileTest::Later);
                };
                let anywhere = matches!(filter.node, Filter::Contains(..));
                FileTest::File(PathGlob::new(glob, anywhere, roots).map_err(|e| (e, operand.span))?)
            }
            _ => FileTest::Later,
        })
    }

    /// Whether the test holds of `place`: `Some(false)` when it cannot, whatever a match binds,
    /// so the file is not needed; `None` when that depends on the match.
    #[must_use]
    pub fn holds(&self, place: &Place) -> Option<bool> {
        match self {
            FileTest::And(parts) => {
                let mut known = Some(true);
                for part in parts {
                    match part.holds(place) {
                        Some(false) => return Some(false),
                        None => known = None,
                        Some(true) => {}
                    }
                }
                known
            }
            FileTest::Not(part) => part.holds(place).map(|holds| !holds),
            FileTest::File(glob) => Some(glob.is_match(place)),
            FileTest::Later => None,
        }
    }
}

/// The directory a glob's files must lie under, from its leading literal text up to its last
/// `/`: discovery starts there rather than at the root.  `None` when there is no such directory.
///
/// # Errors
///
/// Returns the system's error when the directory could not be looked up for a reason other than
/// not being there.
pub fn base(glob: &Glob, roots: Roots) -> Result<Option<PathBuf>, Error> {
    let (from, glob) = from(glob);
    let prefix = match glob.parts().first() {
        Some(GlobPart::Literal(text)) if glob.as_literal().is_none() => {
            text.rfind('/').map_or("", |slash| &text[..=slash])
        }
        Some(GlobPart::Literal(text)) => text.as_str(),
        _ => "",
    };
    named(prefix, from, roots)
}

/// The directory a glob's files must lie under, as written: its leading literal text up to its
/// last `/`.
#[must_use]
pub fn leading_directory(glob: &Glob) -> String {
    match glob.parts().first() {
        Some(GlobPart::Literal(text)) => text.rfind('/').map_or("", |slash| &text[..=slash]),
        _ => "",
    }
    .to_owned()
}

/// A literal path that reads as a glob - holding `*`, `?`, `[` or `{` - spelled with markers,
/// as a note suggests it: `"**/test_*.py"` becomes `**"/test_"*".py"`.  `None` when it holds
/// none of them.
#[must_use]
pub fn as_markers(literal: &str) -> Option<String> {
    if !literal.contains(['*', '?', '[', '{']) {
        return None;
    }
    let mut spelled = String::new();
    let mut text = String::new();
    let flush = |text: &mut String, spelled: &mut String| {
        if !text.is_empty() {
            spelled.push_str(&quote(text));
            text.clear();
        }
    };
    let chars: Vec<char> = literal.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let marker = match c {
            '*' if chars.get(i + 1) == Some(&'*') => Some(("**".to_owned(), 2)),
            '*' => Some(("*".to_owned(), 1)),
            '?' => Some(("?".to_owned(), 1)),
            '[' => chars[i + 1..]
                .iter()
                .skip(1)
                .position(|&c| c == ']')
                .map(|at| {
                    let end = i + 2 + at;
                    (chars[i..=end].iter().collect::<String>(), end + 1 - i)
                }),
            '{' => chars[i..].iter().position(|&c| c == '}').map(|at| {
                let inner: String = chars[i + 1..i + at].iter().collect();
                let alternatives: Vec<String> = inner.split(',').map(quote).collect();
                (format!("{{{}}}", alternatives.join(",")), at + 1)
            }),
            _ => None,
        };
        if let Some((marker, length)) = marker {
            flush(&mut text, &mut spelled);
            spelled.push_str(&marker);
            i += length;
        } else {
            text.push(c);
            i += 1;
        }
    }
    flush(&mut text, &mut spelled);
    Some(spelled)
}

/// `text` as a quoted operand: in double quotes, or, where it holds one, in a fence of three or
/// more that it does not hold, as the lexer reads it back.
#[must_use]
pub fn quote(text: &str) -> String {
    if !text.contains('"') {
        return format!("\"{text}\"");
    }
    let mut fence = "\"\"\"".to_owned();
    while text.contains(&fence) {
        fence.push('"');
    }
    format!("{fence} {text} {fence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal_then(text: &str, part: GlobPart) -> Glob {
        let mut glob = Glob::literal(text);
        glob.push(part);
        glob
    }

    #[test]
    fn a_glob_starts_at_the_root_only_after_a_leading_colon_slash() {
        let (start, rest) = from(&literal_then(":/tests/", GlobPart::Star));
        assert_eq!(start, From::Root);
        assert_eq!(rest, literal_then("tests/", GlobPart::Star));
        let (start, rest) = from(&literal_then("./src/", GlobPart::Star));
        assert_eq!(start, From::Cwd);
        assert_eq!(rest, literal_then("src/", GlobPart::Star));
        // A directory really named `:` is written after `./`.
        let (start, rest) = from(&Glob::literal("./:/x"));
        assert_eq!((start, rest), (From::Cwd, Glob::literal(":/x")));
        assert_eq!(from(&Glob::literal(".")).1, Glob::default());
        // Only the first piece is looked at.
        let mut later = Glob::default();
        later.push(GlobPart::Star);
        later.push_literal(":/x".into());
        assert_eq!(from(&later), (From::Cwd, later.clone()));
    }

    #[test]
    fn a_path_is_matched_from_where_it_starts() {
        let place = Place {
            path: Path::new("/p/tests/test_a.py"),
            from_cwd: "../tests/test_a.py".into(),
            from_root: Some("tests/test_a.py".into()),
        };
        let roots = Roots {
            cwd: Path::new("/p/src"),
            root: Path::new("/p"),
        };
        let mut from_root = Glob::literal(":/tests/");
        from_root.push(GlobPart::DoubleStar);
        assert!(
            PathGlob::new(&from_root, false, roots)
                .unwrap()
                .is_match(&place)
        );
        let mut from_cwd = Glob::default();
        from_cwd.push(GlobPart::DoubleStar);
        from_cwd.push_literal("/test_".into());
        from_cwd.push(GlobPart::Star);
        assert!(
            PathGlob::new(&from_cwd, false, roots)
                .unwrap()
                .is_match(&place)
        );
        let tests = Glob::literal("tests");
        assert!(PathGlob::new(&tests, true, roots).unwrap().is_match(&place));
        let outside = Place {
            from_root: None,
            ..place
        };
        assert!(
            !PathGlob::new(&from_root, false, roots)
                .unwrap()
                .is_match(&outside)
        );
    }

    #[test]
    fn a_file_is_needed_unless_its_path_alone_rules_it_out() {
        let place = Place {
            path: Path::new("/p/a.py"),
            from_cwd: "a.py".into(),
            from_root: Some("a.py".into()),
        };
        let yes = || {
            let mut star = Glob::default();
            star.push(GlobPart::Star);
            FileTest::File(
                PathGlob::new(
                    &star,
                    false,
                    Roots {
                        cwd: Path::new("/p"),
                        root: Path::new("/p"),
                    },
                )
                .unwrap(),
            )
        };
        let no = || FileTest::Not(Box::new(yes()));
        assert_eq!(yes().holds(&place), Some(true));
        assert_eq!(no().holds(&place), Some(false));
        assert_eq!(FileTest::Later.holds(&place), None);
        assert_eq!(FileTest::Not(Box::new(FileTest::Later)).holds(&place), None);
        // A part that cannot hold rules the file out whatever a match would bind.
        assert_eq!(
            FileTest::And(vec![FileTest::Later, no()]).holds(&place),
            Some(false)
        );
        assert_eq!(
            FileTest::And(vec![FileTest::Later, yes()]).holds(&place),
            None
        );
        assert_eq!(FileTest::And(vec![yes(), yes()]).holds(&place), Some(true));
        // `not (FILE ... and $x ...)` can still hold, so the file is needed.
        let both = FileTest::And(vec![yes(), FileTest::Later]);
        assert_eq!(FileTest::Not(Box::new(both)).holds(&place), None);
    }

    #[test]
    fn a_literal_that_reads_as_a_glob_is_spelled_with_markers() {
        assert_eq!(
            as_markers("**/test_*.py").as_deref(),
            Some(r#"**"/test_"*".py""#)
        );
        assert_eq!(
            as_markers("src/{a,b}/[!x]?").as_deref(),
            Some(r#""src/"{"a","b"}"/"[!x]?"#)
        );
        assert_eq!(as_markers("[]x]").as_deref(), Some("[]x]"));
        // An unclosed bracket or brace is text.
        assert_eq!(as_markers("a[b").as_deref(), Some(r#""a[b""#));
        assert_eq!(as_markers("src/a.py"), None);
        assert_eq!(quote(r#"say "hi""#), r#"""" say "hi" """"#);
    }
}
