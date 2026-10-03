// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The command line: what `quaff` was asked to do, read from its arguments.
//!
//! Parsing is hand-written rather than built on an argument library, because the order of
//! sources matters - `-s a -s b` is two searches, in that order - and because a usage error must
//! exit 3, where argument libraries exit 2, which here means an assertion failed.

use std::ffi::OsString;
use std::fmt;

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Print the help and exit 0.
    Help,
    /// Search for each query in turn, over the scope if one was given.
    Search {
        /// The queries, in the order they were given.
        queries: Vec<Query>,
        /// The one positional scope argument, as given.
        scope: Option<OsString>,
    },
}

/// One textual query and where on the command line it came from.
#[derive(Debug, PartialEq, Eq)]
pub struct Query {
    /// The text searched for, exactly as given: nothing in it is special.
    pub text: String,
    /// Where it came from, so that a diagnostic can name it.
    pub source: Source,
}

/// Where a query came from on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The bare first positional argument.
    Positional,
    /// The argument of a `-s` or `--string` option.
    StringOption,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Positional => "the query",
            Source::StringOption => "-s",
        })
    }
}

/// A command line `quaff` cannot act on.  Every one exits 3.
#[derive(Debug, PartialEq, Eq)]
pub enum UsageError {
    /// No arguments at all.
    NoArguments,
    /// An option that is in the MVP but not built yet.
    NotBuiltYet(String),
    /// An option `quaff` does not have.
    UnknownOption(String),
    /// An option that takes a value was last on the command line.
    MissingValue(String),
    /// A query that is empty, and so would match nothing anyone means.
    EmptyQuery(Source),
    /// An argument that is not valid UTF-8 where text is needed.
    NotUtf8 { what: String, argument: OsString },
    /// More than one positional scope argument.
    TooManyScopes(Vec<OsString>),
    /// Nothing to search for: only a scope, or nothing after `--`.
    NoQuery,
}

/// Options in the MVP whose stories have not landed, so saying "unknown" would be wrong.
const NOT_BUILT_YET: &[&str] = &[
    "-p",
    "--pattern",
    "-e",
    "--expression",
    "-f",
    "--file",
    "-o",
    "--output",
    "--dry-run",
    "--version",
];

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::NoArguments => write!(f, "nothing to do: give a query, or -h for help"),
            UsageError::NotBuiltYet(option) => write!(
                f,
                "{option:?} is not built yet; only a textual search - a bare query or -s - is"
            ),
            UsageError::UnknownOption(option) => write!(
                f,
                "unknown option {option:?}; to search for text that starts with a dash, put it \
                 after --"
            ),
            UsageError::MissingValue(option) => {
                write!(f, "{option:?} needs a value: the text to search for")
            }
            UsageError::EmptyQuery(source) => {
                write!(f, "{source} is empty: give the text to search for")
            }
            UsageError::NotUtf8 { what, argument } => {
                write!(f, "{what} {argument:?} is not valid UTF-8")
            }
            UsageError::TooManyScopes(scopes) => {
                let listed: Vec<String> = scopes.iter().map(|s| format!("{s:?}")).collect();
                write!(
                    f,
                    "{} scope arguments, {}; quaff takes one.  The shell has probably expanded \
                     a glob: name one file or directory instead",
                    scopes.len(),
                    listed.join(", ")
                )
            }
            UsageError::NoQuery => write!(f, "nothing to search for: give a query"),
        }
    }
}

/// Reads the command line, excluding the program name.
///
/// # Errors
///
/// Returns a [`UsageError`] for any command line `quaff` cannot act on; nothing is partly
/// accepted.
pub fn parse<I>(args: I) -> Result<Invocation, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();
    if args.peek().is_none() {
        return Err(UsageError::NoArguments);
    }
    let mut queries = Vec::new();
    let mut positionals = Vec::new();
    let mut options_ended = false;
    while let Some(arg) = args.next() {
        if options_ended {
            positionals.push(arg);
            continue;
        }
        let Some(text) = arg.to_str() else {
            // An option is always ASCII, so this is a positional argument.
            positionals.push(arg);
            continue;
        };
        match text {
            "--" => options_ended = true,
            "-h" | "--help" => return Ok(Invocation::Help),
            "-s" | "--string" => {
                let value = args
                    .next()
                    .ok_or_else(|| UsageError::MissingValue(text.to_owned()))?;
                queries.push(query(value, Source::StringOption)?);
            }
            _ if text.starts_with("--string=") => {
                let value = OsString::from(&text["--string=".len()..]);
                queries.push(query(value, Source::StringOption)?);
            }
            _ if NOT_BUILT_YET.contains(&text) => {
                return Err(UsageError::NotBuiltYet(text.to_owned()));
            }
            // A lone "-" is a positional, as it is to most tools.
            _ if text.starts_with('-') && text != "-" => {
                return Err(UsageError::UnknownOption(text.to_owned()));
            }
            _ => positionals.push(arg),
        }
    }
    let mut positionals = positionals.into_iter();
    // The first positional is the query only when no query was given as an option.
    if queries.is_empty() {
        let first = positionals.next().ok_or(UsageError::NoQuery)?;
        queries.push(query(first, Source::Positional)?);
    }
    let scopes: Vec<OsString> = positionals.collect();
    if scopes.len() > 1 {
        return Err(UsageError::TooManyScopes(scopes));
    }
    Ok(Invocation::Search {
        queries,
        scope: scopes.into_iter().next(),
    })
}

fn query(value: OsString, source: Source) -> Result<Query, UsageError> {
    let text = value
        .into_string()
        .map_err(|argument| UsageError::NotUtf8 {
            what: source.to_string(),
            argument,
        })?;
    if text.is_empty() {
        return Err(UsageError::EmptyQuery(source));
    }
    Ok(Query { text, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Invocation, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    fn search(queries: &[(&str, Source)], scope: Option<&str>) -> Invocation {
        Invocation::Search {
            queries: queries
                .iter()
                .map(|(text, source)| Query {
                    text: (*text).to_owned(),
                    source: *source,
                })
                .collect(),
            scope: scope.map(OsString::from),
        }
    }

    #[test]
    fn a_bare_query_and_a_scope() {
        assert_eq!(
            parse_strs(&["TODO"]),
            Ok(search(&[("TODO", Source::Positional)], None))
        );
        assert_eq!(
            parse_strs(&["TODO", "src"]),
            Ok(search(&[("TODO", Source::Positional)], Some("src")))
        );
    }

    #[test]
    fn with_a_string_option_the_first_positional_is_the_scope() {
        assert_eq!(
            parse_strs(&["-s", "TODO", "src"]),
            Ok(search(&[("TODO", Source::StringOption)], Some("src")))
        );
        assert_eq!(
            parse_strs(&["--string=a", "--string", "b"]),
            Ok(search(
                &[("a", Source::StringOption), ("b", Source::StringOption)],
                None
            ))
        );
    }

    #[test]
    fn an_option_value_may_start_with_a_dash() {
        assert_eq!(
            parse_strs(&["-s", "-x"]),
            Ok(search(&[("-x", Source::StringOption)], None))
        );
    }

    #[test]
    fn after_a_double_dash_everything_is_positional() {
        assert_eq!(
            parse_strs(&["--", "-h", "--x"]),
            Ok(search(&[("-h", Source::Positional)], Some("--x")))
        );
        assert_eq!(parse_strs(&["--"]), Err(UsageError::NoQuery));
    }

    #[test]
    fn help_wins_wherever_it_appears() {
        assert_eq!(parse_strs(&["-h"]), Ok(Invocation::Help));
        assert_eq!(parse_strs(&["TODO", "--help"]), Ok(Invocation::Help));
    }

    #[test]
    fn a_lone_dash_is_positional() {
        assert_eq!(
            parse_strs(&["-"]),
            Ok(search(&[("-", Source::Positional)], None))
        );
    }

    #[test]
    fn each_usage_error() {
        assert_eq!(parse_strs(&[]), Err(UsageError::NoArguments));
        assert_eq!(
            parse_strs(&["-p", "x"]),
            Err(UsageError::NotBuiltYet("-p".into()))
        );
        assert_eq!(
            parse_strs(&["--version"]),
            Err(UsageError::NotBuiltYet("--version".into()))
        );
        assert_eq!(
            parse_strs(&["--explain"]),
            Err(UsageError::UnknownOption("--explain".into()))
        );
        assert_eq!(
            parse_strs(&["-s"]),
            Err(UsageError::MissingValue("-s".into()))
        );
        assert_eq!(
            parse_strs(&[""]),
            Err(UsageError::EmptyQuery(Source::Positional))
        );
        assert_eq!(
            parse_strs(&["--string="]),
            Err(UsageError::EmptyQuery(Source::StringOption))
        );
        assert_eq!(
            parse_strs(&["TODO", "a.py", "b.py"]),
            Err(UsageError::TooManyScopes(vec![
                "a.py".into(),
                "b.py".into()
            ]))
        );
    }

    #[test]
    fn a_query_that_is_not_utf8_is_refused_naming_it() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'a', 0xff]);
        assert_eq!(
            parse([bad.clone()]),
            Err(UsageError::NotUtf8 {
                what: "the query".into(),
                argument: bad
            })
        );
    }

    #[test]
    fn the_messages_say_what_to_do() {
        assert_eq!(
            UsageError::TooManyScopes(vec!["a.py".into(), "b.py".into()]).to_string(),
            "2 scope arguments, \"a.py\", \"b.py\"; quaff takes one.  The shell has probably \
             expanded a glob: name one file or directory instead"
        );
        assert_eq!(
            UsageError::UnknownOption("-x".into()).to_string(),
            "unknown option \"-x\"; to search for text that starts with a dash, put it after --"
        );
    }
}
