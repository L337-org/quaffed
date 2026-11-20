// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Reading a run's sources - scripts, expressions and queries - into one program, with the
//! text of each, which locations are worked out from.
//!
//! In: the sources as the command line gives them, and its scope.  Out: a
//! [`Built`] program, checked against the rules every program keeps, which planning reads.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use quaffed_representation::check;
use quaffed_representation::program::{Operand, Program, Query, Statement, TextPattern};
use quaffed_representation::span::{Source, Span, Spanned};
use quaffed_script::{ErrorKind, lexer, location, parse_source};

use super::failure::Failure;
use crate::cli::{self, SourceArg};
use crate::contents::Contents;

/// A program and the text of each of its sources, which locations are worked out from.
pub(super) struct Built {
    pub(super) program: Program,
    texts: Vec<String>,
}

/// Reads `sources` in the order given into one program, narrowed to `scope` if one was given,
/// and checks it, so that a script that cannot run in full does not run at all.  `stdin` is read
/// for `-f -`, and a script file is kept in `contents`, so that it is read once if it is searched
/// too.
///
/// # Errors
///
/// Returns a [`Failure`] for a source that cannot be read, a script that is malformed or uses
/// what this build does not have, or sources that hold no statements.
pub(super) fn read(
    sources: Vec<SourceArg>,
    scope: Option<&OsString>,
    stdin: &mut impl Read,
    contents: &Contents,
) -> Result<Built, Failure> {
    let mut built = build(sources, stdin, contents)?;
    built.program.narrow_to = scope.map(PathBuf::from);
    if built.program.body().is_empty() {
        return Err(Failure::Empty {
            sources: built.program.sources.iter().map(name).collect(),
        });
    }
    check::check(&built.program).map_err(|violation| {
        let location = built.locate(violation.span, violation.span.start);
        let message = violation.to_string();
        if violation.is_unknown_statement() {
            Failure::Unknown { location, message }
        } else {
            Failure::Script { location, message }
        }
    })?;
    Ok(built)
}

impl Built {
    /// Where in its source the pattern operand at `span` starts its content: past the backtick
    /// fence and any padding space in a script, as the lexer cuts it, and at its start for `-p`,
    /// whose source is the pattern alone.
    pub(super) fn content_start(&self, span: Span) -> usize {
        if matches!(self.program.sources[span.source], Source::PatternOption) {
            return span.start;
        }
        span.start + lexer::content_start(&self.texts[span.source][span.start..span.end])
    }

    /// Where byte `offset` of the source `span` is in, as a diagnostic names it.
    pub(super) fn locate(&self, span: Span, offset: usize) -> String {
        location::describe(
            &self.program.sources[span.source],
            &self.texts[span.source],
            offset,
        )
    }
}

/// A script source as a diagnostic names it, with no position.  A query always holds a
/// statement, so only a script can be empty.
pub(super) fn name(source: &Source) -> String {
    match source {
        Source::File(path) => format!("{path:?}"),
        Source::Stdin => "<stdin>".into(),
        Source::Expression(n) => format!("-e expression {n}"),
        // Unreachable while only scripts can be empty; `Source` is non-exhaustive, so a source
        // added later still gets a name rather than a compile error here.
        _ => "a source".into(),
    }
}

/// Reads and parses each source in turn into one program.
pub(super) fn build(
    sources: Vec<SourceArg>,
    stdin: &mut impl Read,
    contents: &Contents,
) -> Result<Built, Failure> {
    let mut built = Built {
        program: Program::default(),
        texts: Vec::new(),
    };
    for source in sources {
        let (source, text) = match source {
            SourceArg::Query(query) => {
                built.push_query(query);
                continue;
            }
            SourceArg::Expression { number, text } => (Source::Expression(number), text),
            SourceArg::File(path) => {
                let path = PathBuf::from(path);
                let source = Source::File(path.clone());
                // Kept under its canonical path, as the walker names files, so that a script in
                // the scope is not read again when it is searched.  A path that cannot be
                // canonicalised is read as given, and fails with the system's own error.
                let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                contents.keep_path(&key);
                let read = contents.read(&key).map(|bytes| bytes.to_vec());
                let text = script_text(read, &source)?;
                (source, text)
            }
            SourceArg::Stdin => {
                let mut bytes = Vec::new();
                let read = stdin.read_to_end(&mut bytes).map(|_| bytes);
                (Source::Stdin, script_text(read, &Source::Stdin)?)
            }
        };
        let parsed = parse_source(&mut built.program, source, &text);
        built.texts.push(text);
        if let Err(err) = parsed {
            let span = Span {
                source: built.texts.len() - 1,
                start: err.start,
                end: err.end,
            };
            let location = built.locate(span, err.start);
            let message = err.message;
            return Err(match err.kind {
                ErrorKind::Unknown => Failure::Unknown { location, message },
                ErrorKind::Malformed => Failure::Script { location, message },
            });
        }
    }
    Ok(built)
}

impl Built {
    /// Adds a query from the command line as the `find` it means: `-p` a structural one, and
    /// `-s` or the bare query a textual one.
    fn push_query(&mut self, query: cli::Query) {
        let source = self.program.sources.len();
        self.program.sources.push(match query.source {
            cli::Source::Positional => Source::Positional,
            cli::Source::StringOption => Source::StringOption,
            cli::Source::PatternOption => Source::PatternOption,
        });
        let span = Span {
            source,
            start: 0,
            end: query.text.len(),
        };
        let operand = match query.source {
            cli::Source::PatternOption => Operand::Pattern(query.text.clone()),
            cli::Source::Positional | cli::Source::StringOption => {
                Operand::Text(TextPattern::literal(query.text.clone()))
            }
        };
        let find = Statement::Find(Query::new(Spanned::new(operand, span)));
        self.program.push(Spanned::new(find, span));
        self.texts.push(query.text);
    }
}

/// The UTF-8 byte-order mark.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// The text of a script that `read` read from `source`, which must be UTF-8, without a leading
/// byte-order mark.
fn script_text(read: io::Result<Vec<u8>>, source: &Source) -> Result<String, Failure> {
    let bytes = read.map_err(|cause| Failure::Io {
        doing: "reading the script",
        path: match source {
            Source::File(path) => path.clone(),
            _ => PathBuf::from("<stdin>"),
        },
        cause,
    })?;
    // A leading byte-order mark is skipped, as it is in a searched file: some editors write
    // one, and nothing else can mean anything there.  It goes before decoding, so a location
    // counts from the first visible character.
    let bytes = match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) => rest.to_vec(),
        None => bytes,
    };
    String::from_utf8(bytes).map_err(|err| {
        let valid_up_to = err.utf8_error().valid_up_to();
        let text = String::from_utf8_lossy(&err.into_bytes()[..valid_up_to]).into_owned();
        Failure::Script {
            location: location::describe(source, &text, valid_up_to),
            message: "the script is not valid UTF-8 here; a script is read as UTF-8".into(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expression(text: &str) -> Vec<SourceArg> {
        vec![SourceArg::Expression {
            number: 1,
            text: text.into(),
        }]
    }

    fn read_expression(text: &str, scope: Option<&OsString>) -> Result<Built, Failure> {
        read(
            expression(text),
            scope,
            &mut io::empty(),
            &Contents::default(),
        )
    }

    #[test]
    fn the_program_read_is_narrowed_to_the_scope_given() {
        let built = read_expression("find \"x\"\n", Some(&OsString::from("src")));
        let Ok(built) = built else {
            panic!("{:?}", built.err());
        };
        assert_eq!(built.program.body().len(), 1);
        assert_eq!(built.program.narrow_to, Some(PathBuf::from("src")));
    }

    #[test]
    fn sources_with_no_statements_are_refused_naming_them() {
        match read_expression("# nothing but a comment\n", None) {
            Err(failure @ Failure::Empty { .. }) => {
                assert_eq!(failure.exit_code(), 3);
                assert!(failure.to_string().contains("-e expression 1"), "{failure}");
            }
            other => panic!("{:?}", other.err()),
        }
    }

    #[test]
    fn the_whole_program_is_checked_before_anything_plans_it() {
        // An edit with no `expect` parses, and the checker refuses it.
        match read_expression("replace string \"a\" with \"b\"\n", None) {
            Err(failure @ Failure::Script { .. }) => assert_eq!(failure.exit_code(), 3),
            other => panic!("{:?}", other.err()),
        }
    }
}
