// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Why a run gave no answer: the failures, each with its exit code and its wording.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use super::report::count;
use crate::cli::UsageError;
use crate::discover;
use crate::write;

/// Why a run could not give an answer.  Each kind maps to one exit code, because each asks the
/// caller for a different action.
#[derive(Debug)]
pub enum Failure {
    /// The command line cannot be acted on: fix it.  Exit as [`UsageError::exit_code`] says.
    Usage(UsageError),
    /// A script that must be fixed: malformed, or breaking a rule every program keeps.  Exit 3.
    Script {
        /// Where, as `location::describe` names it.
        location: String,
        /// What is wrong.
        message: String,
    },
    /// Sources that hold no statements at all - only blank lines and comments - so there is
    /// nothing to run.  Usually a script that was not what was meant, such as `-e "$CHECKS"`
    /// with the variable unset.  Exit 3.
    Empty {
        /// Each source, as a diagnostic names it.
        sources: Vec<String>,
    },
    /// A statement or construct this build does not have: outside the MVP, or in it and not
    /// built yet.  Exit 5.
    Unknown {
        /// Where, as `location::describe` names it.
        location: String,
        /// What it is.
        message: String,
    },
    /// The command-line scope holds a double quote: it is a plain path, and a pattern is
    /// written with `in`.  Refused rather than read as a path, so that an expression there later
    /// only adds to what a scope may be.  Exit 3.
    QuotedScope {
        /// The scope as given.
        scope: OsString,
    },
    /// An `in` names a path that is not there: asking for what is not there is a mistake, and
    /// whatever ran instead would not be what was meant.  Exit 7, as for a missing scope.
    NotThere {
        /// Where the `in` was written, as `location::describe` names it.
        location: String,
        /// What it names, and how to write it if it reads as a glob.
        message: String,
    },
    /// Something could not be read.  Exit 7.
    Io {
        /// What was being done, in the user's terms.
        doing: &'static str,
        /// The path, as the user would recognise it.
        path: PathBuf,
        /// The system's error, verbatim.
        cause: io::Error,
    },
    /// The files to search could not all be found.  Exit 7.
    Discovery(discover::Error),
    /// A structural search needs files quaff cannot parse - ones that do not parse in their
    /// language, and ones in an encoding their language reads and quaff does not - so the run is
    /// refused: a count that left them out could not be trusted.  Exit 9.
    Unparseable {
        /// Each such file, as the user would recognise it, and why it cannot be parsed.
        files: Vec<(PathBuf, String)>,
    },
    /// A replace matched in files whose encoding cannot write its replacement, so the run is
    /// refused and nothing written.  Exit 9: the script or the file must change first.
    Unwritable {
        /// Each such file, and what it cannot write.
        files: Vec<Unwritable>,
    },
    /// The edits would leave files that parsed in their language unparseable, so the run is
    /// refused and nothing written.  Exit 9.
    BrokenByEdit {
        /// Each such file.
        files: Vec<Broken>,
    },
    /// A structural replace cannot be made as written in some files - its matches overlap, it
    /// would drop a comment, or it would change a literal's structure - so the run is refused
    /// and nothing written.  Exit 9.
    EditRefused {
        /// The statement, as a message names it: `replace` or `delete`.
        verb: &'static str,
        /// Each such file, as the user would recognise it, with where and why.
        files: Vec<(PathBuf, String)>,
    },
    /// The edits could not all be written.  Exit 7.
    Write(write::Failure),
}

/// A file an edit would leave unparseable in its language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    /// The file, as the user would recognise it.
    pub(super) path: PathBuf,
    /// Where and why it would not parse.
    pub(super) why: String,
    /// Each statement that changed it, and where it was written: "the replace at" a place.
    pub(super) by: Vec<String>,
}

/// A file whose encoding cannot write a replacement.
#[derive(Debug)]
pub struct Unwritable {
    /// The file, as the user would recognise it.
    pub(super) path: PathBuf,
    /// Its encoding, as `CPython` names the codec.
    pub(super) encoding: &'static str,
    /// The text of the replacement it cannot write.
    pub(super) text: String,
    /// Whether the encoding spells the text more than one way, rather than not at all.
    pub(super) ambiguous: bool,
}

impl Failure {
    /// The exit code this failure ends the run with.
    pub fn exit_code(&self) -> u8 {
        match self {
            Failure::Usage(err) => err.exit_code(),
            Failure::Script { .. } | Failure::Empty { .. } | Failure::QuotedScope { .. } => 3,
            Failure::Unknown { .. } => 5,
            Failure::NotThere { .. } | Failure::Io { .. } | Failure::Discovery(_) => 7,
            Failure::Write(err) => err.exit_code(),
            Failure::Unparseable { .. }
            | Failure::Unwritable { .. }
            | Failure::BrokenByEdit { .. }
            | Failure::EditRefused { .. } => 9,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Usage(err) => err.fmt(f),
            Failure::Script { location, message }
            | Failure::Unknown { location, message }
            | Failure::NotThere { location, message } => write!(f, "{location}: {message}"),
            Failure::Empty { sources } => write!(
                f,
                "nothing to run: {} {} no statements, only blank lines and comments",
                sources.join(", "),
                if sources.len() == 1 { "has" } else { "have" }
            ),
            Failure::QuotedScope { scope } => write!(
                f,
                "the scope {scope:?} holds a double quote: the scope on the command line is a \
                 plain path.  To scope by a pattern, write it with `in` in the script, as -e \
                 'find \"x\" in \"src/\"**'"
            ),
            Failure::Io { doing, path, cause } => {
                write!(f, "{doing} {path:?}: {cause}")?;
                let looks_like_a_glob = path.to_string_lossy().contains(['*', '?', '[']);
                if cause.kind() == io::ErrorKind::NotFound && looks_like_a_glob {
                    write!(
                        f,
                        ".  quaff does not expand a glob given as the scope; name one file or \
                         directory, or write the pattern with `in` in the script"
                    )?;
                }
                Ok(())
            }
            Failure::Discovery(err) => err.fmt(f),
            Failure::Unparseable { files } => {
                write!(
                    f,
                    "refused: {} in scope cannot be parsed, so a structural search cannot count \
                     matches in {}:",
                    count(files.len(), "file", "files"),
                    if files.len() == 1 { "it" } else { "them" }
                )?;
                for (path, why) in files {
                    write!(f, "\n  {path:?}: {why}")?;
                }
                let them = if files.len() == 1 { "it" } else { "them" };
                write!(
                    f,
                    "\nTo search without {them}, leave {them} out of the statement: {}, or \
                     project-wide by listing {them} in .ignore",
                    leave_out(files.first().map(|(path, _)| path.as_path()))
                )
            }
            Failure::Unwritable { files } => unwritable(f, files),
            Failure::BrokenByEdit { files } => broken(f, files),
            Failure::EditRefused { verb, files } => {
                write!(
                    f,
                    "refused: the {verb} cannot be made as written in {}, so nothing was written:",
                    count(files.len(), "file", "files")
                )?;
                for (path, why) in files {
                    write!(f, "\n  {path:?}: {why}")?;
                }
                Ok(())
            }
            Failure::Write(err) => err.fmt(f),
        }
    }
}

/// The refusal of a replacement files' encodings cannot write.
pub(super) fn unwritable(f: &mut fmt::Formatter<'_>, files: &[Unwritable]) -> fmt::Result {
    write!(
        f,
        "refused: the replacement cannot be written in the encoding of {}, so nothing \
             was written:",
        count(files.len(), "file", "files")
    )?;
    for file in files {
        if file.ambiguous {
            write!(
                f,
                "\n  {:?}: {} spells {:?} more than one way",
                file.path, file.encoding, file.text
            )?;
        } else {
            write!(
                f,
                "\n  {:?}: {} cannot hold {:?}",
                file.path, file.encoding, file.text
            )?;
        }
    }
    write!(
        f,
        "\nUse a replacement {} can hold, or leave {} out of the statement: {}, or with a \
         narrower scope on the command line",
        if files.len() == 1 {
            "its encoding"
        } else {
            "their encodings"
        },
        if files.len() == 1 { "it" } else { "them" },
        leave_out(files.first().map(|file| file.path.as_path()))
    )
}

/// The ways a statement can leave out files it refuses, as a refusal offers them: `in`, and
/// `where not FILE matches` with the first of them, `first`, as the example.
fn leave_out(first: Option<&Path>) -> String {
    // A path as printed is one `FILE` names, so the clause can be copied as it is.
    let example = first.map_or_else(String::new, |path| {
        format!(
            ", such as where not FILE matches {}",
            crate::scope::quote(&path.to_string_lossy())
        )
    });
    format!("with `in`, with `where not FILE matches`{example}")
}

/// The refusal of edits that would leave files unparseable in their language.
pub(super) fn broken(f: &mut fmt::Formatter<'_>, files: &[Broken]) -> fmt::Result {
    write!(
        f,
        "refused: the edit would leave {} that {} unparseable, so nothing was written:",
        count(files.len(), "file", "files"),
        if files.len() == 1 { "parses" } else { "parse" }
    )?;
    for file in files {
        write!(f, "\n  {:?}: {}", file.path, file.why)?;
        if !file.by.is_empty() {
            write!(f, "; changed by {}", file.by.join(" and by "))?;
        }
    }
    // Each `by` names its statement, "the delete at" or "the replace at".
    let deleting = files
        .iter()
        .flat_map(|file| &file.by)
        .all(|by| by.starts_with("the delete "));
    write!(
        f,
        "\nChange the {} so that {} still {}, or leave {} out of the statement: {}, \
         or with a narrower scope on the command line",
        if deleting { "deletion" } else { "replacement" },
        if files.len() == 1 {
            "the file"
        } else {
            "the files"
        },
        if files.len() == 1 { "parses" } else { "parse" },
        if files.len() == 1 { "it" } else { "them" },
        leave_out(files.first().map(|file| file.path.as_path()))
    )
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Io { cause, .. } => Some(cause),
            Failure::Discovery(err) => Some(err),
            Failure::Write(err) => Some(err),
            Failure::Usage(_)
            | Failure::Script { .. }
            | Failure::Empty { .. }
            | Failure::Unknown { .. }
            | Failure::QuotedScope { .. }
            | Failure::NotThere { .. }
            | Failure::Unparseable { .. }
            | Failure::Unwritable { .. }
            | Failure::BrokenByEdit { .. }
            | Failure::EditRefused { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failure_has_the_exit_code_for_its_remedy() {
        assert_eq!(Failure::Usage(UsageError::NoQuery).exit_code(), 3);
        let quoted = Failure::QuotedScope {
            scope: "\"x\"".into(),
        };
        assert_eq!(quoted.exit_code(), 3);
        let io = Failure::Io {
            doing: "reading",
            path: "a".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(io.exit_code(), 7);
    }

    #[test]
    fn a_missing_scope_that_looks_like_a_glob_says_globs_are_not_expanded() {
        let failure = Failure::Io {
            doing: "reading the scope",
            path: "src/**/*.py".into(),
            cause: io::Error::from(io::ErrorKind::NotFound),
        };
        let message = failure.to_string();
        assert!(
            message.starts_with("reading the scope \"src/**/*.py\": "),
            "{message}"
        );
        assert!(
            message.ends_with(
                "quaff does not expand a glob given as the scope; name one file or directory, or \
                 write the pattern with `in` in the script"
            ),
            "{message}"
        );
    }

    #[test]
    fn the_underlying_error_stays_on_the_chain() {
        use std::error::Error as _;
        let io = Failure::Io {
            doing: "reading",
            path: "a".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            io.source().map(ToString::to_string),
            Some(io::Error::from(io::ErrorKind::PermissionDenied).to_string())
        );
        assert!(Failure::Usage(UsageError::NoQuery).source().is_none());
    }

    #[test]
    fn the_refusals_of_an_edit_are_worded_for_one_file_and_for_many() {
        let unwritable = |path: &str, ambiguous| Unwritable {
            path: path.into(),
            encoding: "mac_arabic",
            text: "a b".into(),
            ambiguous,
        };
        let one = Failure::Unwritable {
            files: vec![unwritable("a.txt", true)],
        }
        .to_string();
        assert!(
            one.contains("\"a.txt\": mac_arabic spells \"a b\" more than one way"),
            "{one}"
        );
        assert!(
            one.ends_with(
                "Use a replacement its encoding can hold, or leave it out of the statement: with \
                 `in`, with `where not FILE matches`, such as where not FILE matches \"a.txt\", \
                 or with a narrower scope on the command line"
            ),
            "{one}"
        );
        let many = Failure::Unwritable {
            files: vec![unwritable("a.txt", false), unwritable("b.txt", false)],
        }
        .to_string();
        assert!(many.contains("the encoding of 2 files"), "{many}");
        assert!(
            many.contains("a replacement their encodings can hold"),
            "{many}"
        );
        assert!(
            many.contains("or leave them out of the statement"),
            "{many}"
        );
        let broken = Failure::BrokenByEdit {
            files: vec![
                Broken {
                    path: "a.py".into(),
                    why: "line 1, column 1: x".into(),
                    by: vec![
                        "the replace at -e expression 1, at character 1".into(),
                        "the delete at -e expression 2, at character 1".into(),
                    ],
                },
                Broken {
                    path: "b.py".into(),
                    why: "line 2, column 1: y".into(),
                    by: Vec::new(),
                },
            ],
        };
        let message = broken.to_string();
        assert_eq!(broken.exit_code(), 9);
        assert!(
            message.contains("2 files that parse unparseable"),
            "{message}"
        );
        assert!(
            message.contains(
                "x; changed by the replace at -e expression 1, at character 1 and by the delete \
                 at -e expression 2, at character 1"
            ),
            "{message}"
        );
        assert!(
            message.contains("\"b.py\": line 2, column 1: y\n"),
            "{message}"
        );
        // One of the statements replaced, so it is the replacement that must change.
        assert!(
            message.contains("\nChange the replacement so that"),
            "{message}"
        );
        assert!(
            message.ends_with(
                "so that the files still parse, or leave them out of the statement: with `in`, \
                 with `where not FILE matches`, such as where not FILE matches \"a.py\", or \
                 with a narrower scope on the command line"
            ),
            "{message}"
        );
        for failure in [
            &broken,
            &Failure::Unwritable {
                files: vec![unwritable("a.txt", true), unwritable("b.txt", false)],
            },
        ] {
            refuses_every_write(failure);
        }
    }

    /// A destination refusing one write, the `refused`th from 0, and accepting every other, so
    /// that a message dropping the refusal goes on to succeed.
    struct Refusing {
        writes: usize,
        refused: Option<usize>,
    }

    impl fmt::Write for Refusing {
        fn write_str(&mut self, _: &str) -> fmt::Result {
            self.writes += 1;
            if Some(self.writes - 1) == self.refused {
                return Err(fmt::Error);
            }
            Ok(())
        }
    }

    /// Displays `failure` into destinations that refuse its first write, then its second, and so
    /// on to its last, and requires each refusal to end the message as an error, not be dropped.
    fn refuses_every_write(failure: &dyn fmt::Display) {
        let display = |refused| {
            let mut destination = Refusing { writes: 0, refused };
            fmt::Write::write_fmt(&mut destination, format_args!("{failure}"))
                .map(|()| destination.writes)
        };
        let writes = display(None).unwrap();
        assert!(writes > 1, "{failure} was written in {writes} writes");
        for refused in 0..writes {
            assert!(
                display(Some(refused)).is_err(),
                "a refusal of write {} of {writes} was dropped: {failure}",
                refused + 1
            );
        }
    }

    #[test]
    fn a_write_failure_exits_7_with_the_systems_error_on_the_chain() {
        use std::error::Error as _;
        let failure = Failure::Write(write::Failure::Open {
            path: "a.txt".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        });
        assert_eq!(failure.exit_code(), 7);
        let cause = failure
            .source()
            .and_then(|err| err.source())
            .map(ToString::to_string);
        assert_eq!(
            cause,
            Some(io::Error::from(io::ErrorKind::PermissionDenied).to_string())
        );
        refuses_every_write(&Failure::Write(write::Failure::Write {
            path: "a.txt".into(),
            cause: io::Error::from(io::ErrorKind::StorageFull),
            unrestored: vec![
                (
                    "a.txt".into(),
                    io::Error::from(io::ErrorKind::PermissionDenied),
                ),
                (
                    "b.txt".into(),
                    io::Error::from(io::ErrorKind::PermissionDenied),
                ),
            ],
            unflushed: Vec::new(),
        }));
    }
}
