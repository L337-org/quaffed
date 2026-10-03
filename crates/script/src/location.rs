// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Where in a source something is, in the form a diagnostic names it.
//!
//! Sources compose in order, so a location always names its source: `refactor.quaff:12:5` for
//! a script file, `<stdin>:12:5` for standard input, and the expression's number and the
//! offset in it for `-e`.  Lines and columns count from 1, columns in characters.

use quaffed_representation::span::Source;

/// The line and column of byte `offset` in `text`, both from 1, the column in characters.
#[must_use]
pub fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..floor_char_boundary(text, offset)];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[line_start..].chars().count() + 1)
}

/// `offset`, moved back to the start of the character it falls in.
fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut at = offset.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Where byte `offset` of `text` is, as a diagnostic names it.
#[must_use]
pub fn describe(source: &Source, text: &str, offset: usize) -> String {
    let (line, column) = line_and_column(text, offset);
    match source {
        Source::File(path) => format!("{}:{line}:{column}", path.display()),
        Source::Stdin => format!("<stdin>:{line}:{column}"),
        Source::Expression(n) => {
            let at = text[..floor_char_boundary(text, offset)].chars().count() + 1;
            format!("-e expression {n}, at character {at}")
        }
        Source::StringOption => "-s".into(),
        Source::Positional => "the query".into(),
        _ => "an unnamed source".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn lines_and_columns_count_from_one_in_characters() {
        assert_eq!(line_and_column("abc", 0), (1, 1));
        assert_eq!(line_and_column("ab\ncd", 4), (2, 2));
        assert_eq!(line_and_column("é x", 3), (1, 3));
        // An offset inside a character is that character.
        assert_eq!(line_and_column("é", 1), (1, 1));
        assert_eq!(line_and_column("ab", 9), (1, 3));
    }

    #[test]
    fn each_source_is_named_its_own_way() {
        let text = "find \"a\"\nfnd";
        assert_eq!(
            describe(&Source::File(PathBuf::from("refactor.quaff")), text, 9),
            "refactor.quaff:2:1"
        );
        assert_eq!(describe(&Source::Stdin, text, 9), "<stdin>:2:1");
        assert_eq!(
            describe(&Source::Expression(2), "find \"x\" fnd", 9),
            "-e expression 2, at character 10"
        );
        assert_eq!(describe(&Source::StringOption, "x", 0), "-s");
        assert_eq!(describe(&Source::Positional, "x", 0), "the query");
    }
}
