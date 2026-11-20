// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The languages quaff parses, each by its front end: the one place the binary names an adapter.
//!
//! Everything else reaches a language through [`FrontEnd`], found here by a file's name, so a
//! language is added by adding its front end to [`LANGUAGES`] and nothing else in the binary
//! changes.  `tests/adapter_containment.rs` fails if any other file of the binary names an
//! adapter crate.  `architecture/syntax.md` lists what a new language implements.

use std::path::Path;

use quaffed_syntax::FrontEnd;

/// Every language quaff parses, by its front end, in the order the help lists them.
pub static LANGUAGES: &[&dyn FrontEnd] = &[&quaffed_python::Python];

/// The language the file at `path` is in, by its name alone, if quaff parses it.
pub fn of(path: &Path) -> Option<&'static dyn FrontEnd> {
    LANGUAGES
        .iter()
        .copied()
        .find(|language| language.claims(path))
}

/// The language a backticked pattern is compiled in.
///
/// The design is that a pattern is compiled once for each language, and each file matched against
/// its own language's: a file whose language cannot read the pattern is not applicable to it.
/// With one language that is this one, and `one_language_until_patterns_are_compiled_for_each`
/// fails when a second is added, at the step `architecture/syntax.md` lists for it.
pub fn of_patterns() -> &'static dyn FrontEnd {
    LANGUAGES[0]
}

/// Python's front end, for tests that compile or parse Python itself.
#[cfg(test)]
pub(crate) static PYTHON: &dyn FrontEnd = &quaffed_python::Python;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_in_the_language_that_claims_it_or_in_none() {
        let python = of(Path::new("src/a.py")).expect("a .py file is Python");
        assert_eq!(python.language(), PYTHON.language());
        assert!(of(Path::new("README.md")).is_none());
        assert!(of(Path::new("Makefile")).is_none());
    }

    #[test]
    fn one_language_until_patterns_are_compiled_for_each() {
        assert_eq!(
            LANGUAGES.len(),
            1,
            "a second language needs each pattern compiled for every language, and each file \
             matched against its own language's, as the step for it in architecture/syntax.md \
             says; then of_patterns and this test go"
        );
    }

    #[test]
    fn no_file_is_claimed_by_two_languages() {
        for language in LANGUAGES {
            for extension in language.extensions() {
                let path = format!("a.{extension}");
                let claiming: Vec<_> = LANGUAGES
                    .iter()
                    .filter(|other| other.claims(Path::new(&path)))
                    .map(|other| other.language().name())
                    .collect();
                assert_eq!(claiming.len(), 1, "{path} is claimed by {claiming:?}");
            }
        }
    }
}
