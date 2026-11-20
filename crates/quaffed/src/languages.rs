// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The languages quaff parses, each by its front end: the one place the binary names an adapter.
//!
//! Everything else reaches a language through [`FrontEnd`], and a run finds each file's among
//! [`LANGUAGES`] by the file's name, so a language is added by adding its front end there and
//! nothing else in the binary changes.  `tests/adapter_containment.rs` fails if any other file
//! of the binary names an adapter crate.  `architecture/syntax.md` lists what a new language
//! implements.

#[cfg(test)]
use std::path::Path;

use quaffed_syntax::FrontEnd;

/// Every language quaff parses, by its front end, in the order the help lists them.
pub static LANGUAGES: &[&dyn FrontEnd] = &[&quaffed_python::Python];

/// The language the file at `path` is in, by its name alone, if quaff parses it: for tests, since a
/// run finds it among the languages it was given.
#[cfg(test)]
pub(crate) fn of(path: &Path) -> Option<&'static dyn FrontEnd> {
    LANGUAGES
        .iter()
        .copied()
        .find(|language| language.claims(path))
}

/// Python's front end, for tests that compile or parse Python itself.
#[cfg(test)]
pub(crate) static PYTHON: &dyn FrontEnd = &quaffed_python::Python;

/// A second language for tests, so that a run over two languages is tested while quaff parses
/// one: Python's front end under another name, claiming `.toy` files, that reads no pattern or
/// rewrite holding `lambda`, so that a pattern or a rewrite one language reads and the other
/// does not can be written.  It answers no question that has a default, so it holds the
/// defaults too.
#[cfg(test)]
pub(crate) static TOY: &dyn FrontEnd = &Toy {
    name: "Toy",
    extensions: &["toy"],
    refuses: "lambda",
};

/// A third language for tests, as [`TOY`] but claiming `.plain` files and reading nothing
/// holding `yield`, so that each of two languages can read what the other cannot.
#[cfg(test)]
pub(crate) static PLAIN: &dyn FrontEnd = &Toy {
    name: "Plain",
    extensions: &["plain"],
    refuses: "yield",
};

/// A test language: Python's front end under the name `name`, claiming files by `extensions`,
/// that reads no pattern or rewrite holding `refuses`.
#[cfg(test)]
struct Toy {
    name: &'static str,
    extensions: &'static [&'static str],
    refuses: &'static str,
}

#[cfg(test)]
impl Toy {
    /// Why it cannot read `text`, if it holds the word it refuses.
    fn refused(&self, text: &str) -> Option<quaffed_syntax::CompileError> {
        text.find(self.refuses).map(|at| {
            let message = format!("`{}` is not {}", self.refuses, self.name);
            quaffed_syntax::CompileError::new(message, Some(at))
        })
    }
}

#[cfg(test)]
impl FrontEnd for Toy {
    fn language(&self) -> quaffed_syntax::Language {
        quaffed_syntax::Language::new(self.name)
    }
    fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }
    fn decode(&self, bytes: &[u8]) -> Result<quaffed_encoding::Source, quaffed_syntax::Refused> {
        PYTHON.decode(bytes)
    }
    fn parse<'a>(
        &self,
        text: &'a str,
    ) -> Result<quaffed_syntax::Arena<'a>, quaffed_syntax::ParseError> {
        PYTHON.parse(text)
    }
    fn compile_pattern(
        &self,
        text: &str,
    ) -> Result<quaffed_syntax::Pattern, quaffed_syntax::CompileError> {
        self.refused(text)
            .map_or_else(|| PYTHON.compile_pattern(text), Err)
    }
    fn compile_rewrite(
        &self,
        text: &str,
        pattern: &quaffed_syntax::Pattern,
    ) -> Result<quaffed_syntax::Rewrite, quaffed_syntax::CompileError> {
        self.refused(text)
            .map_or_else(|| PYTHON.compile_rewrite(text, pattern), Err)
    }
    fn lex(&self, text: &str) -> quaffed_syntax::Lexed {
        PYTHON.lex(text)
    }
    fn code_tokens<'a>(&self, text: &'a str) -> Vec<&'a str> {
        PYTHON.code_tokens(text)
    }
    fn token_kinds(&self, text: &str) -> Vec<String> {
        PYTHON.token_kinds(text)
    }
}

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
    fn a_file_is_claimed_by_its_extension_alone() {
        assert!(TOY.claims(Path::new("src/a.toy")));
        assert!(!TOY.claims(Path::new("a.toy.txt")));
        assert!(!TOY.claims(Path::new("toy")));
        assert!(!TOY.claims(Path::new("a.TOY")));
    }

    #[test]
    fn the_defaults_claim_nothing_the_language_was_not_asked_for() {
        assert_eq!(TOY.declared_encoding(b"# coding: latin-1\nx = 1\n"), None);
        assert_eq!(TOY.misplaced("break\n"), Ok(Vec::new()));
        let part = &TOY.lex("x = 'a'\n").strings[0];
        assert_eq!(TOY.escaped("b", part), None);
        assert_eq!(PYTHON.escaped("b", part), Some("b".to_owned()));
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
