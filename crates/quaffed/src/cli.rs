// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The command line: what `quaff` was asked to do, read from its arguments.
//!
//! Parsing is hand-written rather than built on an argument library, because the order of
//! sources matters - `-e '...' -s TODO` is the script, then one search - and because a usage
//! error must exit 3, where argument libraries exit 2, which here means an assertion failed.

use std::ffi::OsString;
use std::fmt;

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Print the help and exit 0.
    Help,
    /// Run each source in turn, over the scope if one was given.
    Run {
        /// The sources, in the order they were given.
        sources: Vec<SourceArg>,
        /// The one positional scope argument, as given.
        scope: Option<OsString>,
    },
}

/// One source of statements, as the command line gave it.
#[derive(Debug, PartialEq, Eq)]
pub enum SourceArg {
    /// A textual query: `-s TEXT`, or the bare first positional.
    Query(Query),
    /// An inline script: one `-e`.
    Expression {
        /// Which `-e` it was, counted from 1, so that a diagnostic can name it.
        number: usize,
        /// The script.
        text: String,
    },
    /// A script file: `-f PATH`.
    File(OsString),
    /// A script on standard input: `-f -`.
    Stdin,
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
    /// Nothing to run: only a scope, or nothing after `--`.
    NoQuery,
    /// `-f -` given more than once, when standard input can be read only once.
    StdinTwice,
}

/// Options in the MVP whose stories have not landed, so saying "unknown" would be wrong.
const NOT_BUILT_YET: &[&str] = &[
    "-p",
    "--pattern",
    "-o",
    "--output",
    "--dry-run",
    "--version",
];

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::NoArguments => write!(f, "nothing to do: give a query, or -h for help"),
            UsageError::NotBuiltYet(option) => write!(f, "{option:?} is not built yet"),
            UsageError::UnknownOption(option) => write!(
                f,
                "unknown option {option:?}; to search for text that starts with a dash, put it \
                 after --"
            ),
            UsageError::MissingValue(option) => {
                let wanted = match option.as_str() {
                    "-e" | "--expression" => "the statements to run",
                    "-f" | "--file" => "a script file, or - for standard input",
                    _ => "the text to search for",
                };
                write!(f, "{option:?} needs a value: {wanted}")
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
            UsageError::NoQuery => {
                write!(f, "nothing to run: give a query, or a script with -e or -f")
            }
            UsageError::StdinTwice => {
                write!(
                    f,
                    "-f - is given twice, and standard input can be read only once"
                )
            }
        }
    }
}

/// The options that take a value, by their short and long names.
const STRING: (&str, &str) = ("-s", "--string");
const EXPRESSION: (&str, &str) = ("-e", "--expression");
const FILE: (&str, &str) = ("-f", "--file");

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
    let mut sources = Vec::new();
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
        let (name, inline) = match text.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (text, None),
        };
        let option = [STRING, EXPRESSION, FILE]
            .into_iter()
            .find(|(short, long)| name == *long || (inline.is_none() && name == *short));
        if let Some(option) = option {
            let value = match inline {
                Some(value) => OsString::from(value),
                None => args
                    .next()
                    .ok_or_else(|| UsageError::MissingValue(name.to_owned()))?,
            };
            sources.push(source(option, value, &sources)?);
            continue;
        }
        match name {
            "--" if inline.is_none() => options_ended = true,
            "-h" | "--help" if inline.is_none() => return Ok(Invocation::Help),
            // `--pattern=X` is the not-built option as surely as `--pattern X` is.
            _ if NOT_BUILT_YET.contains(&name) => {
                return Err(UsageError::NotBuiltYet(name.to_owned()));
            }
            // A lone "-" is a positional, as it is to most tools.
            _ if text.starts_with('-') && text != "-" => {
                return Err(UsageError::UnknownOption(text.to_owned()));
            }
            _ => positionals.push(arg),
        }
    }
    let mut positionals = positionals.into_iter();
    // The first positional is the query only when no source was given as an option.
    if sources.is_empty() {
        let first = positionals.next().ok_or(UsageError::NoQuery)?;
        sources.push(SourceArg::Query(query(first, Source::Positional)?));
    }
    let scopes: Vec<OsString> = positionals.collect();
    if scopes.len() > 1 {
        return Err(UsageError::TooManyScopes(scopes));
    }
    Ok(Invocation::Run {
        sources,
        scope: scopes.into_iter().next(),
    })
}

/// The source the option `(short, long)` gives with `value`, after the `sources` before it.
fn source(
    option: (&str, &str),
    value: OsString,
    sources: &[SourceArg],
) -> Result<SourceArg, UsageError> {
    if option == STRING {
        return Ok(SourceArg::Query(query(value, Source::StringOption)?));
    }
    if option == EXPRESSION {
        let number = 1 + sources
            .iter()
            .filter(|s| matches!(s, SourceArg::Expression { .. }))
            .count();
        let text = value
            .into_string()
            .map_err(|argument| UsageError::NotUtf8 {
                what: format!("-e expression {number}"),
                argument,
            })?;
        return Ok(SourceArg::Expression { number, text });
    }
    if value != "-" {
        return Ok(SourceArg::File(value));
    }
    if sources.contains(&SourceArg::Stdin) {
        return Err(UsageError::StdinTwice);
    }
    Ok(SourceArg::Stdin)
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

    fn query_arg(text: &str, source: Source) -> SourceArg {
        SourceArg::Query(Query {
            text: text.to_owned(),
            source,
        })
    }

    fn expression(number: usize, text: &str) -> SourceArg {
        SourceArg::Expression {
            number,
            text: text.to_owned(),
        }
    }

    fn run(sources: Vec<SourceArg>, scope: Option<&str>) -> Invocation {
        Invocation::Run {
            sources,
            scope: scope.map(OsString::from),
        }
    }

    #[test]
    fn a_bare_query_and_a_scope() {
        assert_eq!(
            parse_strs(&["TODO"]),
            Ok(run(vec![query_arg("TODO", Source::Positional)], None))
        );
        assert_eq!(
            parse_strs(&["TODO", "src"]),
            Ok(run(
                vec![query_arg("TODO", Source::Positional)],
                Some("src")
            ))
        );
    }

    #[test]
    fn with_a_string_option_the_first_positional_is_the_scope() {
        assert_eq!(
            parse_strs(&["-s", "TODO", "src"]),
            Ok(run(
                vec![query_arg("TODO", Source::StringOption)],
                Some("src")
            ))
        );
        assert_eq!(
            parse_strs(&["--string=a", "--string", "b"]),
            Ok(run(
                vec![
                    query_arg("a", Source::StringOption),
                    query_arg("b", Source::StringOption)
                ],
                None
            ))
        );
    }

    #[test]
    fn sources_compose_in_the_order_given() {
        assert_eq!(
            parse_strs(&[
                "-e",
                "find \"a\"",
                "-f",
                "x.quaff",
                "-s",
                "TODO",
                "--expression=find \"b=c\"",
                "--file",
                "-",
                "--file=y.quaff",
                "src"
            ]),
            Ok(run(
                vec![
                    expression(1, "find \"a\""),
                    SourceArg::File("x.quaff".into()),
                    query_arg("TODO", Source::StringOption),
                    expression(2, "find \"b=c\""),
                    SourceArg::Stdin,
                    SourceArg::File("y.quaff".into()),
                ],
                Some("src")
            ))
        );
    }

    #[test]
    fn with_a_script_the_first_positional_is_the_scope() {
        assert_eq!(
            parse_strs(&["-e", "find \"a\"", "src"]),
            Ok(run(vec![expression(1, "find \"a\"")], Some("src")))
        );
        assert_eq!(
            parse_strs(&["-f", "x.quaff"]),
            Ok(run(vec![SourceArg::File("x.quaff".into())], None))
        );
    }

    #[test]
    fn an_empty_expression_is_a_source() {
        assert_eq!(
            parse_strs(&["-e", ""]),
            Ok(run(vec![expression(1, "")], None))
        );
    }

    #[test]
    fn an_option_value_may_start_with_a_dash() {
        assert_eq!(
            parse_strs(&["-s", "-x"]),
            Ok(run(vec![query_arg("-x", Source::StringOption)], None))
        );
        assert_eq!(
            parse_strs(&["-f", "-x"]),
            Ok(run(vec![SourceArg::File("-x".into())], None))
        );
    }

    #[test]
    fn only_a_long_option_that_takes_a_value_takes_an_equals_value() {
        for arg in ["-s=x", "-p=x", "--=x", "--help=x", "-h=x"] {
            assert_eq!(
                parse_strs(&[arg]),
                Err(UsageError::UnknownOption(arg.into())),
                "{arg}"
            );
        }
    }

    #[test]
    fn after_a_double_dash_everything_is_positional() {
        assert_eq!(
            parse_strs(&["--", "-h", "--x"]),
            Ok(run(vec![query_arg("-h", Source::Positional)], Some("--x")))
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
            Ok(run(vec![query_arg("-", Source::Positional)], None))
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
            parse_strs(&["--pattern=handle($a)"]),
            Err(UsageError::NotBuiltYet("--pattern".into()))
        );
        assert_eq!(
            parse_strs(&["--explain"]),
            Err(UsageError::UnknownOption("--explain".into()))
        );
        for option in ["-s", "--string", "-e", "--expression", "-f", "--file"] {
            assert_eq!(
                parse_strs(&[option]),
                Err(UsageError::MissingValue(option.into()))
            );
        }
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
        assert_eq!(
            parse_strs(&["-f", "-", "-e", "x", "--file=-"]),
            Err(UsageError::StdinTwice)
        );
    }

    #[test]
    fn text_that_is_not_utf8_is_refused_naming_it() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'a', 0xff]);
        assert_eq!(
            parse([bad.clone()]),
            Err(UsageError::NotUtf8 {
                what: "the query".into(),
                argument: bad.clone()
            })
        );
        assert_eq!(
            parse([
                "-e".into(),
                "x".into(),
                "-s".into(),
                "y".into(),
                "-e".into(),
                bad.clone()
            ]),
            Err(UsageError::NotUtf8 {
                what: "-e expression 2".into(),
                argument: bad.clone()
            })
        );
        // A file name need not be UTF-8: it names a file, and is not read as text.
        assert_eq!(
            parse(["-f".into(), bad.clone()]),
            Ok(run(vec![SourceArg::File(bad)], None))
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
        assert_eq!(
            UsageError::MissingValue("--expression".into()).to_string(),
            "\"--expression\" needs a value: the statements to run"
        );
        assert_eq!(
            UsageError::MissingValue("-f".into()).to_string(),
            "\"-f\" needs a value: a script file, or - for standard input"
        );
        assert_eq!(
            UsageError::MissingValue("--string".into()).to_string(),
            "\"--string\" needs a value: the text to search for"
        );
        assert_eq!(
            UsageError::StdinTwice.to_string(),
            "-f - is given twice, and standard input can be read only once"
        );
        assert_eq!(
            UsageError::NoQuery.to_string(),
            "nothing to run: give a query, or a script with -e or -f"
        );
        assert_eq!(
            UsageError::NotBuiltYet("-p".into()).to_string(),
            "\"-p\" is not built yet"
        );
    }
}
