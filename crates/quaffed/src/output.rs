// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! How a match is printed: one line per match, `path:line:column-endline:endcolumn: text`.
//!
//! The text rendering is deliberately lossy.  A match that spans lines is folded onto one, so
//! the output stays one match per line for a pipeline, and the end position shows that it was.

use std::path::{Component, Path, PathBuf};

use crate::search::Match;

/// One output line for `found` in the file at `path`, whose matched text is `text`.
pub fn line(path: &Path, found: &Match, text: &str) -> String {
    format!(
        "{}:{}:{}-{}:{}: {}",
        path.display(),
        found.start.line,
        found.start.column,
        found.end.line,
        found.end.column,
        fold(text)
    )
}

/// `text` on one line: each line ending, with the indentation after it, becomes one space.
///
/// A line ending is never simply deleted, because `return` and `value` on two lines would read
/// as `returnvalue` - a different answer that looks right.
pub fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            while chars
                .peek()
                .is_some_and(|&next| next == ' ' || next == '\t')
            {
                chars.next();
            }
            folded.push(' ');
        } else {
            folded.push(c);
        }
    }
    folded
}

/// `path` relative to `base`, both absolute: `../` where it lies outside, `.` where they are one.
///
/// Worked out from the components, without touching the file system, so both must already be
/// in the same form - both canonical, as the caller makes them.
pub fn relative(path: &Path, base: &Path) -> PathBuf {
    let path: Vec<Component> = path.components().collect();
    let base: Vec<Component> = base.components().collect();
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    for component in &path[common..] {
        relative.push(component);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    relative
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Position;

    #[test]
    fn a_line_names_both_ends_of_the_match() {
        let found = Match {
            start: Position {
                line: 12,
                column: 5,
            },
            end: Position {
                line: 12,
                column: 22,
            },
        };
        assert_eq!(
            line(Path::new("src/app.py"), &found, "def connect(self):"),
            "src/app.py:12:5-12:22: def connect(self):"
        );
    }

    #[test]
    fn line_endings_and_the_indentation_after_them_fold_to_one_space() {
        assert_eq!(fold("one\n    two"), "one two");
        assert_eq!(fold("one\r\n\ttwo\rthree"), "one two three");
        assert_eq!(fold("return\nvalue"), "return value");
        assert_eq!(fold("trailing\n"), "trailing ");
        assert_eq!(fold("a  b"), "a  b");
    }

    #[test]
    fn paths_are_made_relative_in_either_direction() {
        assert_eq!(
            relative(Path::new("/p/src/a.py"), Path::new("/p")),
            Path::new("src/a.py")
        );
        assert_eq!(
            relative(Path::new("/p/a.py"), Path::new("/p/src")),
            Path::new("../a.py")
        );
        assert_eq!(
            relative(Path::new("/p/x/a.py"), Path::new("/p/y/z")),
            Path::new("../../x/a.py")
        );
        assert_eq!(relative(Path::new("/p"), Path::new("/p")), Path::new("."));
    }
}
