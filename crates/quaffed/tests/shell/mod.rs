// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Running a command written as it would be typed: split into words as a shell would, and run
//! with the built `quaff`, for the tests that check the examples in the language guide and in
//! the help.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

/// Splits a command into words as a POSIX shell would, for the quoting the guide and the help use.
///
/// Returns `Ok(None)` when a quote is still open at the end, so the command continues on the
/// next line, and an error for anything a shell would expand or treat specially, since the
/// command is never given to a shell and so could not be checked as written.
pub fn split_command(command: &str) -> Result<Option<Vec<String>>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if let Some(done) = word.take() {
                    words.push(done);
                }
            }
            '\'' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return Ok(None),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('$' | '`' | '"' | '\\')) => current.push(escaped),
                            Some('\n') => {}
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Ok(None),
                        },
                        Some(special @ ('$' | '`')) => {
                            return Err(format!(
                                "the command has an unescaped {special:?} in double quotes, \
                                 which a shell would expand"
                            ));
                        }
                        Some(inner) => current.push(inner),
                        None => return Ok(None),
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || "-_./:=,@%+".contains(c) => {
                word.get_or_insert_with(String::new).push(c);
            }
            other => {
                return Err(format!(
                    "the command has {other:?} outside quotes, which a shell would treat \
                     specially, so it cannot be checked without one"
                ));
            }
        }
    }
    if let Some(done) = word {
        words.push(done);
    }
    Ok(Some(words))
}

/// What one run of `quaff` printed, both streams as one, and how it exited.
pub struct Ran {
    /// Standard output and standard error, in the order `quaff` wrote them.
    pub output: String,
    /// The exit status, or `None` if a signal ended it.
    pub exit: Option<i32>,
}

/// Runs `quaff` with `args` from `cwd`, with `stdin` on its standard input or none, and standard
/// output and standard error into one pipe, so that what is read back is in the order `quaff`
/// wrote it.
pub fn run_quaff(cwd: &Path, args: &[String], stdin: Option<&str>) -> Ran {
    let (mut reader, writer) = std::io::pipe().expect("creating a pipe for quaff's output");
    let mut command = Command::new(env!("CARGO_BIN_EXE_quaff"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(writer.try_clone().expect("cloning the output pipe"))
        .stderr(writer);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command
        .spawn()
        .unwrap_or_else(|err| panic!("starting quaff from {}: {err}", cwd.display()));
    if let Some(text) = stdin {
        // Written whole and closed before any output is read: a script is small enough for the
        // pipe's buffer, and quaff reads it all before it prints anything.
        let mut input = child.stdin.take().expect("quaff's standard input");
        input
            .write_all(text.as_bytes())
            .expect("writing quaff's standard input");
    }
    // The command holds the pipe's write ends; until it is dropped the read below never sees
    // the end of the output.
    drop(command);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .expect("reading quaff's output");
    let status = child.wait().expect("waiting for quaff");
    Ran {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        exit: status.code(),
    }
}
