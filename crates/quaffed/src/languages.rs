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
    editing: None,
};

/// A third language for tests, as [`TOY`] but claiming `.plain` files and reading nothing
/// holding `yield`, so that each of two languages can read what the other cannot.
#[cfg(test)]
pub(crate) static PLAIN: &dyn FrontEnd = &Toy {
    name: "Plain",
    extensions: &["plain"],
    refuses: "yield",
    editing: None,
};

/// A language for tests that parses as Python but edits by other rules: no separator between
/// statements, and no block written on its header's line, as a language whose blocks are not
/// decided by layout, so that what the engine takes from [`FrontEnd::editing`] can be seen.
#[cfg(test)]
pub(crate) static UNLAID: &dyn FrontEnd = &Toy {
    name: "Unlaid",
    extensions: &["unlaid"],
    refuses: "\0",
    editing: Some(&quaffed_syntax::Editing {
        droppable_items: &[],
        lone_item_separated: &[],
        item_separator: ',',
        statement_separator: None,
        line_continuation: Some('\\'),
        header_block_opener: None,
        grouping_bracket: '(',
        misplaceable: &[],
    }),
};

/// A test language: Python's front end under the name `name`, claiming files by `extensions`,
/// that reads no pattern or rewrite holding `refuses`, and edits by `editing`, or Python's
/// rules where it is `None`.
#[cfg(test)]
struct Toy {
    name: &'static str,
    extensions: &'static [&'static str],
    refuses: &'static str,
    editing: Option<&'static quaffed_syntax::Editing>,
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
    fn kinds(&self) -> &'static [quaffed_syntax::Kind] {
        PYTHON.kinds()
    }
    fn editing(&self) -> &'static quaffed_syntax::Editing {
        self.editing.unwrap_or_else(|| PYTHON.editing())
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

    /// Every string literal in `source`, Rust code, with the line it is on, from 1: its text
    /// between the quotes as written, escapes and all.  Comments, character literals and
    /// lifetimes are passed over, so a quote in one starts no string.
    fn string_literals(source: &str) -> Vec<(usize, &str)> {
        let bytes = source.as_bytes();
        let line = |at: usize| source[..at].matches('\n').count() + 1;
        let ident =
            |at: usize| at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        let mut found = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let rest = &source[i..];
            if rest.starts_with("//") {
                i += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                i += rest.find("*/").map_or(rest.len(), |end| end + 2);
            } else if bytes[i] == b'\'' {
                // A character literal, `'a'` or `'\''`, or else a lifetime's quote.
                let mut chars = rest[1..].chars();
                i += match (chars.next(), chars.next()) {
                    (Some('\\'), _) => rest[3..].find('\'').map_or(rest.len(), |end| end + 4),
                    (Some(c), Some('\'')) => 2 + c.len_utf8(),
                    _ => 1,
                };
            } else if bytes[i] == b'r' && !ident(i)
                || bytes[i] == b'r' && i > 0 && bytes[i - 1] == b'b' && !ident(i - 1)
            {
                let hashes = rest[1..].len() - rest[1..].trim_start_matches('#').len();
                if rest[1 + hashes..].starts_with('"') {
                    let open = i + 2 + hashes;
                    let close = format!("\"{}", "#".repeat(hashes));
                    let end = source[open..]
                        .find(&close)
                        .map_or(source.len(), |end| open + end);
                    found.push((line(open), &source[open..end]));
                    i = end + close.len();
                } else {
                    i += 1;
                }
            } else if bytes[i] == b'"' {
                let open = i + 1;
                let mut end = open;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                found.push((line(open), &source[open..end.min(bytes.len())]));
                i = end + 1;
            } else {
                i += 1;
            }
        }
        found
    }

    /// What code would spell to ask a node's kind of one parser: each kind's name, and the
    /// leading word of a name - `Stmt` of `StmtIf` - that two or more kinds share, as a test by
    /// prefix would.
    fn spellings(kinds: &[quaffed_syntax::Kind]) -> Vec<&'static str> {
        let leading = |name: &'static str| {
            let end = name[1..]
                .find(|c: char| c.is_ascii_uppercase())
                .map_or(name.len(), |at| at + 1);
            &name[..end]
        };
        let mut spelled: Vec<&'static str> = kinds.iter().map(|kind| kind.name()).collect();
        for kind in kinds {
            let word = leading(kind.name());
            let sharing = kinds
                .iter()
                .filter(|other| leading(other.name()) == word)
                .count();
            if sharing > 1 {
                spelled.push(word);
            }
        }
        spelled.sort_unstable();
        spelled.dedup();
        spelled
    }

    fn sources(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|err| panic!("listing {dir:?}: {err}")) {
            let path = entry.expect("reading a directory entry").path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }

    /// The engine asks a language what it needs to edit, through its front end, and never spells
    /// one parser's names for its kinds: a name in the binary's source would hold only for that
    /// parser, and a second language's files would be edited by the first's rules.  Every source
    /// file of the binary but this one, which tests the scan, tests included, is read for a
    /// string literal that is a kind's name in any language quaff parses, or the leading word
    /// two or more of a language's kinds share.  `architecture/syntax.md` says what a language
    /// gives instead.
    #[test]
    fn no_source_spells_a_kind_of_any_language() {
        let spelled: Vec<&str> = LANGUAGES
            .iter()
            .flat_map(|language| spellings(language.kinds()))
            .collect();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, &mut files);
        assert!(files.len() > 1, "no sources found under {src:?}");
        let mut found = Vec::new();
        for path in files.iter().filter(|path| !path.ends_with("languages.rs")) {
            let source = std::fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
            for (line, literal) in string_literals(&source) {
                if spelled.contains(&literal) {
                    found.push(format!("{}:{line}: {literal:?}", path.display()));
                }
            }
        }
        assert!(
            found.is_empty(),
            "a kind of one language is spelled where the engine should ask its front end:\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn the_scan_finds_a_kind_spelled_in_code_and_nowhere_else() {
        let source = "let a = 'x'; let b: &'static str = \"one\";\n\
                      // \"two\" in a comment\n\
                      /* \"three\" */ let c = '\"'; let d = '\\''; let e = r#\"fo\"ur\"#;\n\
                      let f = \"fi\\\"ve\"; let g = br\"six\"; let h = for_r(\"seven\");\n";
        let literals: Vec<&str> = string_literals(source)
            .into_iter()
            .map(|(_, literal)| literal)
            .collect();
        assert_eq!(literals, ["one", "fo\"ur", "fi\\\"ve", "six", "seven"]);
        assert_eq!(string_literals(source)[1].0, 3);
        let kinds = [
            quaffed_syntax::Kind::new("StmtIf"),
            quaffed_syntax::Kind::new("StmtFor"),
            quaffed_syntax::Kind::new("ExprCall"),
            quaffed_syntax::Kind::new("Alias"),
        ];
        assert_eq!(
            spellings(&kinds),
            ["Alias", "ExprCall", "Stmt", "StmtFor", "StmtIf"]
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
