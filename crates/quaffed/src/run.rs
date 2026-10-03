// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Carrying out a search: the scope, the files in it, the matches, and what was not looked at.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cli::{Query, UsageError};
use crate::discover;
use crate::encoding::{self, Content};
use crate::output;
use crate::project;
use crate::search::{self, Match};

/// Why a run could not give an answer.  Each kind maps to one exit code, because each asks the
/// caller for a different action.
#[derive(Debug)]
pub enum Failure {
    /// The command line cannot be acted on: fix it.  Exit 3.
    Usage(UsageError),
    /// The scope lies outside the project, and a command-line scope narrows, never widens.
    /// Exit 3.
    OutsideProject {
        /// The scope as given.
        scope: OsString,
        /// The project root it lies outside.
        root: PathBuf,
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
}

impl Failure {
    /// The exit code this failure ends the run with.
    pub fn exit_code(&self) -> u8 {
        match self {
            Failure::Usage(_) | Failure::OutsideProject { .. } => 3,
            Failure::Io { .. } | Failure::Discovery(_) => 7,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Usage(err) => err.fmt(f),
            Failure::OutsideProject { scope, root } => write!(
                f,
                "the scope {scope:?} is outside the project at {root:?}, and a scope can only \
                 narrow a search; run quaff from inside the other project instead"
            ),
            Failure::Io { doing, path, cause } => {
                write!(f, "{doing} {path:?}: {cause}")?;
                let looks_like_a_glob = path.to_string_lossy().contains(['*', '?', '[']);
                if cause.kind() == io::ErrorKind::NotFound && looks_like_a_glob {
                    write!(
                        f,
                        ".  quaff does not expand a glob given as the scope; name one file or \
                         directory"
                    )?;
                }
                Ok(())
            }
            Failure::Discovery(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Io { cause, .. } => Some(cause),
            Failure::Discovery(err) => Some(err),
            Failure::Usage(_) | Failure::OutsideProject { .. } => None,
        }
    }
}

/// Searches for each query in turn, printing matches to `out` and notes to `notes`.
///
/// The search covers the project - found by walking up from `cwd` - or `cwd` when there is no
/// project, narrowed to `scope` when one is given.  Every file is read once, whatever the
/// number of queries; the matches print query by query, in the order the queries were given.
///
/// Returns whether anything was found, which decides between exit 0 and exit 1.
///
/// # Errors
///
/// Returns a [`Failure`] when the search cannot be answered in full: a scope outside the
/// project, a scope or file that cannot be read, or a directory that cannot be listed.  Nothing
/// is printed for a run that fails, so a partial answer never passes for a whole one.
pub fn search(
    queries: &[Query],
    scope: Option<&OsString>,
    cwd: &Path,
    out: &mut impl Write,
    notes: &mut impl Write,
) -> Result<bool, Failure> {
    let cwd = fs::canonicalize(cwd).map_err(|cause| Failure::Io {
        doing: "reading the current directory",
        path: cwd.to_path_buf(),
        cause,
    })?;
    let root = project::find_root(&cwd);
    let scope = match scope {
        None => root.clone().unwrap_or_else(|| cwd.clone()),
        Some(given) => {
            let scope = fs::canonicalize(cwd.join(given)).map_err(|cause| Failure::Io {
                doing: "reading the scope",
                path: PathBuf::from(given),
                cause,
            })?;
            if let Some(root) = &root
                && !scope.starts_with(root)
            {
                return Err(Failure::OutsideProject {
                    scope: given.clone(),
                    root: output::relative(root, &cwd),
                });
            }
            scope
        }
    };
    let found = discover::files(&scope, &cwd).map_err(Failure::Discovery)?;

    let mut matches: Vec<Vec<(PathBuf, Vec<Match>)>> = vec![Vec::new(); queries.len()];
    let mut binary = 0;
    let mut unknown_encoding = vec![0; queries.len()];
    for file in &found.files {
        let shown = output::relative(file, &cwd);
        let bytes = fs::read(file).map_err(|cause| Failure::Io {
            doing: "reading",
            path: shown.clone(),
            cause,
        })?;
        // Classified once, whatever the number of queries.
        let (encoding, body) = match encoding::classify(&bytes) {
            Content::Binary => {
                binary += 1;
                continue;
            }
            Content::Text { encoding, body } => (encoding, body),
        };
        for (i, query) in queries.iter().enumerate() {
            match search::search(&bytes[body..], encoding, &query.text) {
                None => unknown_encoding[i] += 1,
                Some(in_file) if in_file.is_empty() => {}
                Some(in_file) => matches[i].push((shown.clone(), in_file)),
            }
        }
    }

    let mut any = false;
    for (query, per_file) in queries.iter().zip(&matches) {
        for (path, in_file) in per_file {
            for found in in_file {
                any = true;
                if !print(out, &output::line(path, found, &query.text))? {
                    return Ok(any);
                }
            }
        }
    }
    // Notes are best effort: a run that cannot write to standard error has nowhere to say so.
    let mut note = |text: String| {
        let _ = writeln!(notes, "quaff: {text}");
    };
    for ((query, per_file), &unknown) in queries.iter().zip(&matches).zip(&unknown_encoding) {
        if unknown > 0 {
            note(format!(
                "{} in an unknown 8-bit encoding not searched for {:?}, which is not ASCII and so \
                 has no one spelling there",
                count(unknown, "file", "files"),
                query.text
            ));
        }
        if per_file.is_empty() {
            note(format!("no matches for {:?}", query.text));
        }
    }
    if binary > 0 {
        note(format!(
            "{} not searched",
            count(binary, "binary file", "binary files")
        ));
    }
    if found.special_files > 0 {
        note(format!(
            "{} - FIFOs, sockets or devices - not searched",
            count(found.special_files, "special file", "special files")
        ));
    }
    if found.links_not_followed > 0 {
        note(format!(
            "{} not followed",
            count(found.links_not_followed, "symbolic link", "symbolic links")
        ));
    }
    Ok(any)
}

/// Writes one line of results.  Returns `false` if the reader has gone, which ends the run
/// quietly, as it does for any tool whose output is piped into `head`.
fn print(out: &mut impl Write, line: &str) -> Result<bool, Failure> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(cause) => Err(Failure::Io {
            doing: "writing the results to",
            path: PathBuf::from("standard output"),
            cause,
        }),
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failure_has_the_exit_code_for_its_remedy() {
        assert_eq!(Failure::Usage(UsageError::NoQuery).exit_code(), 3);
        let outside = Failure::OutsideProject {
            scope: "/x".into(),
            root: ".".into(),
        };
        assert_eq!(outside.exit_code(), 3);
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
                "quaff does not expand a glob given as the scope; name one file or directory"
            ),
            "{message}"
        );
    }

    #[test]
    fn a_closed_pipe_ends_the_run_quietly() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(!print(&mut Closed, "x").unwrap());
    }

    #[test]
    fn counts_are_worded_for_one_and_for_many() {
        assert_eq!(count(1, "binary file", "binary files"), "1 binary file");
        assert_eq!(count(3, "binary file", "binary files"), "3 binary files");
    }
}
