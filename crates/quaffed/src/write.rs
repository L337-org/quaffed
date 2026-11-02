// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Writing a run's edits: every file in place, or none of them.
//!
//! Every file is written in place, into the file itself, so that whatever a user relies on in it
//! is untouched: its hard links, its owner and group, its permissions.  Writing a new file and
//! renaming it over the old one would be atomic, but it is a different file: a hard link would
//! keep the old contents, and the owner would become whoever ran quaff.
//!
//! Every file is opened for writing, and closed again, before any is written, so a file that
//! cannot be written stops the run with nothing changed.  Each is then opened again only while it
//! is written, so a run that changes thousands of files never holds more than one open, whatever
//! the limit on open files.  If writing then fails - a full disk, say - every file already
//! written, and the one that failed, is written back from its original bytes, which the run still
//! holds.  What this cannot cover is the process being killed, or the machine losing power, while
//! a file is being written: that file can be left half-written.
//!
//! **Nothing is flushed to disk yet, on any system**, so an edit that succeeded can still be lost
//! if the machine loses power soon after.  How to make writes durable is undecided, and flushing
//! each file as it is written makes an edit of many files far slower, most of all on macOS.
//! [`Flush`] is where it goes once decided: one hook after each file is written and one after
//! them all, chosen per system in [`platform`], each a no-op for now.  A file put back after a
//! failure runs the same hooks.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// One file to write: where it is, how a message names it, and its bytes before and after.
pub struct Edit<'a> {
    /// Where it is.
    pub path: &'a Path,
    /// The path a message shows.
    pub shown: &'a Path,
    /// Its bytes as they were read, written back if writing fails.
    pub original: &'a [u8],
    /// Its new bytes.
    pub edited: &'a [u8],
}

/// A flush that failed after files were put back, leaving them as they were but perhaps not on
/// disk.
#[derive(Debug)]
pub enum Unflushed {
    /// One file's flush, after its original bytes were written back.
    File {
        /// The file, as a message shows it.
        path: PathBuf,
        /// The system's error, verbatim.
        cause: io::Error,
    },
    /// The flush of every file put back.
    All {
        /// The system's error, verbatim.
        cause: io::Error,
    },
}

/// Why the edits were not all written.  Every case exits 7, and says what state it left.
#[derive(Debug)]
pub enum Failure {
    /// A file could not be opened for writing, so nothing was written.
    Open {
        /// The file, as a message shows it.
        path: PathBuf,
        /// The system's error, verbatim.
        cause: io::Error,
    },
    /// Writing a file failed, and what was already written was written back.
    Write {
        /// The file, as a message shows it.
        path: PathBuf,
        /// The system's error, verbatim.
        cause: io::Error,
        /// Files that could not be written back, left changed or half-written, with why.
        unrestored: Vec<(PathBuf, io::Error)>,
        /// Flushes of the files written back that failed.
        unflushed: Vec<Unflushed>,
    },
    /// Every file was written, but flushing them all to disk failed, so they were written back.
    Flush {
        /// The system's error, verbatim.
        cause: io::Error,
        /// Files that could not be written back, left changed or half-written, with why.
        unrestored: Vec<(PathBuf, io::Error)>,
        /// Flushes of the files written back that failed.
        unflushed: Vec<Unflushed>,
    },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Open { path, cause } => write!(
                f,
                "opening {path:?} to write the edits: {cause}; nothing was written"
            ),
            Failure::Write {
                path,
                cause,
                unrestored,
                unflushed,
            } => {
                write!(f, "writing the edits to {path:?}: {cause}")?;
                put_back(f, unrestored, unflushed)
            }
            Failure::Flush {
                cause,
                unrestored,
                unflushed,
            } => {
                write!(f, "flushing the edits to disk: {cause}")?;
                put_back(f, unrestored, unflushed)
            }
        }
    }
}

/// What a message says when every file written was put back.
const ALL_PUT_BACK: &str = "; every file written was put back as it was, so nothing is changed";

/// What a message says before listing the flushes of files put back that failed.
const UNFLUSHED: &str =
    "\nThe files put back could not all be flushed to disk, so a power loss could still lose them:";

/// Says what putting the written files back left: every one as it was, or which could not be,
/// and which flushes of them failed.
fn put_back(
    f: &mut fmt::Formatter<'_>,
    unrestored: &[(PathBuf, io::Error)],
    unflushed: &[Unflushed],
) -> fmt::Result {
    if unrestored.is_empty() {
        f.write_str(ALL_PUT_BACK)?;
    } else {
        let (it, was, is) = if unrestored.len() == 1 {
            ("it", "was", "is")
        } else {
            ("they", "were", "are")
        };
        let n = count(unrestored.len());
        write!(
            f,
            "; {n} could not be put back as {it} {was}, and {is} left changed or"
        )?;
        f.write_str(" half-written:")?;
        for (path, why) in unrestored {
            write!(f, "\n  {path:?}: {why}")?;
        }
    }
    if !unflushed.is_empty() {
        f.write_str(UNFLUSHED)?;
    }
    for failed in unflushed {
        match failed {
            Unflushed::File { path, cause } => write!(f, "\n  {path:?}: {cause}")?,
            Unflushed::All { cause } => write!(f, "\n  flushing them all: {cause}")?,
        }
    }
    Ok(())
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Open { cause, .. }
            | Failure::Write { cause, .. }
            | Failure::Flush { cause, .. } => Some(cause),
        }
    }
}

fn count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "file" } else { "files" })
}

/// A file that can be written over with new bytes.
trait Target {
    /// Makes the file's whole contents `bytes`, without flushing them to disk.
    fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()>;
}

impl Target for File {
    fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(0))?;
        self.write_all(bytes)?;
        // After the write, so that a file never holds less than its new bytes while they are
        // being written: shortening first would lose the end of the original if the write failed.
        self.set_len(bytes.len() as u64)
    }
}

/// What makes written files reach the disk: run after each file is written, and once after them
/// all.  An error from either is a failed write, and every file written is put back.
trait Flush<T> {
    /// Run on each file once it is written, still open.
    fn file(&self, target: &mut T) -> io::Result<()>;
    /// Run once every file is written, given them all.
    fn commit(&self, paths: &[&Path]) -> io::Result<()>;
}

/// The hooks for the system quaff was built for, from [`platform`].
struct Platform;

impl Flush<File> for Platform {
    fn file(&self, target: &mut File) -> io::Result<()> {
        platform::file(target)
    }

    fn commit(&self, paths: &[&Path]) -> io::Result<()> {
        platform::commit(paths)
    }
}

/// The flush hooks for each system, every one a no-op until how to make writes durable is
/// decided.  Each system has its own so that one can change without the others: what is cheap
/// and sufficient differs, with `syncfs` on Linux, `F_FULLFSYNC` on macOS and `FlushFileBuffers`
/// on Windows.
#[cfg(target_os = "linux")]
mod platform {
    #![expect(
        clippy::unnecessary_wraps,
        reason = "a no-op until a flush is decided, and a flush can fail"
    )]

    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub fn file(_: &mut File) -> io::Result<()> {
        Ok(())
    }

    pub fn commit(_: &[&Path]) -> io::Result<()> {
        Ok(())
    }
}

/// See the Linux hooks.
#[cfg(target_os = "macos")]
mod platform {
    #![expect(
        clippy::unnecessary_wraps,
        reason = "a no-op until a flush is decided, and a flush can fail"
    )]

    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub fn file(_: &mut File) -> io::Result<()> {
        Ok(())
    }

    pub fn commit(_: &[&Path]) -> io::Result<()> {
        Ok(())
    }
}

/// See the Linux hooks.
#[cfg(target_os = "windows")]
mod platform {
    #![expect(
        clippy::unnecessary_wraps,
        reason = "a no-op until a flush is decided, and a flush can fail"
    )]

    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub fn file(_: &mut File) -> io::Result<()> {
        Ok(())
    }

    pub fn commit(_: &[&Path]) -> io::Result<()> {
        Ok(())
    }
}

/// Every other system.  See the Linux hooks.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    #![expect(
        clippy::unnecessary_wraps,
        reason = "a no-op until a flush is decided, and a flush can fail"
    )]

    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub fn file(_: &mut File) -> io::Result<()> {
        Ok(())
    }

    pub fn commit(_: &[&Path]) -> io::Result<()> {
        Ok(())
    }
}

/// Writes every edit in place, or none of them.
///
/// # Errors
///
/// Returns a [`Failure`] saying what could not be written and what state that left: nothing
/// changed when a file could not be opened, every file put back when a write or a flush failed,
/// or which files could not be put back.
pub fn commit(edits: &[Edit]) -> Result<(), Failure> {
    commit_with(
        edits,
        |path| OpenOptions::new().write(true).open(path),
        &Platform,
    )
}

/// [`commit`], opening each file with `open` and flushing with `flush`.
fn commit_with<T: Target>(
    edits: &[Edit],
    mut open: impl FnMut(&Path) -> io::Result<T>,
    flush: &impl Flush<T>,
) -> Result<(), Failure> {
    // Every file opened, and closed again, before any is written, so one that cannot be written
    // changes nothing.
    for edit in edits {
        drop(open(edit.path).map_err(|cause| Failure::Open {
            path: edit.shown.to_path_buf(),
            cause,
        })?);
    }
    for (written, edit) in edits.iter().enumerate() {
        if let Err((cause, opened)) = write(&mut open, flush, edit.path, edit.edited) {
            // One that failed once opened may be half-written, so it is put back too; one that
            // could not be opened is untouched, and is not reported as possibly changed.
            let failed = if opened { written + 1 } else { written };
            let (unrestored, unflushed) = put_back_all(&edits[..failed], &mut open, flush);
            return Err(Failure::Write {
                path: edit.shown.to_path_buf(),
                cause,
                unrestored,
                unflushed,
            });
        }
    }
    let paths: Vec<&Path> = edits.iter().map(|edit| edit.path).collect();
    if let Err(cause) = flush.commit(&paths) {
        let (unrestored, unflushed) = put_back_all(edits, &mut open, flush);
        return Err(Failure::Flush {
            cause,
            unrestored,
            unflushed,
        });
    }
    Ok(())
}

/// Opens `path`, makes its contents `bytes`, and runs the per-file flush hook on it.  A failure
/// says whether the file had been opened, and so may have been changed.
fn write<T: Target>(
    open: &mut impl FnMut(&Path) -> io::Result<T>,
    flush: &impl Flush<T>,
    path: &Path,
    bytes: &[u8],
) -> Result<(), (io::Error, bool)> {
    let mut target = open(path).map_err(|cause| (cause, false))?;
    target
        .overwrite(bytes)
        .and_then(|()| flush.file(&mut target))
        .map_err(|cause| (cause, true))
}

/// Writes each of `edits` back from its original bytes, flushing each and then all of them, so
/// that recovery is as durable as the edit would have been.  Returns the files that could not be
/// put back, and the flushes that failed.  A file whose bytes were written back but not flushed
/// counts as put back, since it holds its original bytes, and is flushed with the rest.
fn put_back_all<T: Target>(
    edits: &[Edit],
    open: &mut impl FnMut(&Path) -> io::Result<T>,
    flush: &impl Flush<T>,
) -> (Vec<(PathBuf, io::Error)>, Vec<Unflushed>) {
    let mut unrestored = Vec::new();
    let mut unflushed = Vec::new();
    let mut restored = Vec::new();
    for edit in edits {
        let written = open(edit.path).and_then(|mut target| {
            target.overwrite(edit.original)?;
            Ok(target)
        });
        match written {
            Err(why) => unrestored.push((edit.shown.to_path_buf(), why)),
            Ok(mut target) => {
                restored.push(edit.path);
                if let Err(cause) = flush.file(&mut target) {
                    let path = edit.shown.to_path_buf();
                    unflushed.push(Unflushed::File { path, cause });
                }
            }
        }
    }
    if !restored.is_empty()
        && let Err(cause) = flush.commit(&restored)
    {
        unflushed.push(Unflushed::All { cause });
    }
    (unrestored, unflushed)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// Files in memory, failing the writes they are told to, and counting what is open.
    #[derive(Default)]
    struct Disk {
        files: HashMap<PathBuf, Vec<u8>>,
        /// For each file, how many more of its writes fail, each leaving it half-written.
        failures: HashMap<PathBuf, usize>,
        /// How many files are open now, and the most ever open at once.
        open: usize,
        most_open: usize,
        /// For each file, how many more of its per-file flushes fail.
        flush_failures: HashMap<PathBuf, usize>,
        /// How many more end-of-commit flushes fail.
        commit_failures: usize,
        /// Every write and flush, in order.
        log: Vec<String>,
        /// For each file, how many more times it opens before it refuses.
        opens_left: HashMap<PathBuf, usize>,
    }

    struct InMemory {
        path: PathBuf,
        disk: Rc<RefCell<Disk>>,
    }

    impl Target for InMemory {
        fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()> {
            let mut disk = self.disk.borrow_mut();
            if let Some(left) = disk.failures.get_mut(&self.path).filter(|left| **left > 0) {
                *left -= 1;
                // A byte that is neither old nor new: a write that stopped partway.
                disk.files.get_mut(&self.path).unwrap()[0] = b'#';
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            disk.log.push(format!(
                "write {} {:?}",
                self.path.display(),
                String::from_utf8_lossy(bytes)
            ));
            disk.files.insert(self.path.clone(), bytes.to_vec());
            Ok(())
        }
    }

    /// Flush hooks that log each call to the disk, failing those they are told to.
    struct Recorder(Rc<RefCell<Disk>>);

    impl Flush<InMemory> for Recorder {
        fn file(&self, target: &mut InMemory) -> io::Result<()> {
            let mut disk = self.0.borrow_mut();
            disk.log.push(format!("flush {}", target.path.display()));
            match disk.flush_failures.get_mut(&target.path) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    Err(io::Error::other("the file flush failed"))
                }
                _ => Ok(()),
            }
        }

        fn commit(&self, paths: &[&Path]) -> io::Result<()> {
            let mut disk = self.0.borrow_mut();
            let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            disk.log.push(format!("commit {}", names.join(" ")));
            if disk.commit_failures > 0 {
                disk.commit_failures -= 1;
                return Err(io::Error::other("the commit flush failed"));
            }
            Ok(())
        }
    }

    fn log(disk: &Rc<RefCell<Disk>>) -> Vec<String> {
        disk.borrow().log.clone()
    }

    impl Drop for InMemory {
        fn drop(&mut self) {
            self.disk.borrow_mut().open -= 1;
        }
    }

    fn disk(files: &[(&str, &str)]) -> Rc<RefCell<Disk>> {
        let disk = Rc::new(RefCell::new(Disk::default()));
        for (path, text) in files {
            disk.borrow_mut()
                .files
                .insert(PathBuf::from(path), text.as_bytes().to_vec());
        }
        disk
    }

    fn contents(disk: &Rc<RefCell<Disk>>, path: &str) -> String {
        String::from_utf8(disk.borrow().files[Path::new(path)].clone()).unwrap()
    }

    fn edit<'a>(path: &'a str, original: &'a str, edited: &'a str) -> Edit<'a> {
        Edit {
            path: Path::new(path),
            shown: Path::new(path),
            original: original.as_bytes(),
            edited: edited.as_bytes(),
        }
    }

    /// Commits `edits` to `disk`, refusing to open the files named in `unopenable`.
    fn commit_to(
        disk: &Rc<RefCell<Disk>>,
        edits: &[Edit],
        unopenable: &[&str],
    ) -> Result<(), Failure> {
        commit_with(
            edits,
            |path: &Path| {
                if unopenable.iter().any(|p| Path::new(p) == path) {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                let mut state = disk.borrow_mut();
                if let Some(left) = state.opens_left.get_mut(path) {
                    if *left == 0 {
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                    }
                    *left -= 1;
                }
                state.open += 1;
                state.most_open = state.most_open.max(state.open);
                Ok(InMemory {
                    path: path.to_path_buf(),
                    disk: Rc::clone(disk),
                })
            },
            &Recorder(Rc::clone(disk)),
        )
    }

    #[test]
    fn each_file_is_flushed_once_written_and_all_of_them_at_the_end() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        commit_to(&disk, &edits, &[]).unwrap();
        assert_eq!(
            log(&disk),
            [
                "write a \"new a\"",
                "flush a",
                "write b \"new b\"",
                "flush b",
                "commit a b",
            ]
        );
    }

    #[test]
    fn a_file_flush_that_fails_puts_every_file_back_and_flushes_them() {
        let disk = disk(&[("a", "old a"), ("b", "old b"), ("c", "old c")]);
        disk.borrow_mut()
            .flush_failures
            .insert(PathBuf::from("b"), 1);
        let edits = [
            edit("a", "old a", "new a"),
            edit("b", "old b", "new b"),
            edit("c", "old c", "new c"),
        ];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(
            log(&disk),
            [
                "write a \"new a\"",
                "flush a",
                "write b \"new b\"",
                "flush b",
                "write a \"old a\"",
                "flush a",
                "write b \"old b\"",
                "flush b",
                "commit a b",
            ]
        );
        let message = failure.to_string();
        assert!(
            message.starts_with("writing the edits to \"b\": the file flush failed"),
            "{message}"
        );
        assert!(
            message.ends_with("; every file written was put back as it was, so nothing is changed"),
            "{message}"
        );
    }

    #[test]
    fn a_commit_flush_that_fails_puts_every_file_back() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        disk.borrow_mut().commit_failures = 1;
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "old b");
        assert_eq!(log(&disk).last().unwrap(), "commit a b");
        assert_eq!(
            failure.to_string(),
            "flushing the edits to disk: the commit flush failed; every file written was put \
             back as it was, so nothing is changed"
        );
        assert_eq!(
            std::error::Error::source(&failure).map(ToString::to_string),
            Some("the commit flush failed".to_owned())
        );
    }

    #[test]
    fn files_put_back_that_cannot_be_flushed_are_said_to_be_at_risk() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        // b's write fails and so does putting it back, so only a is flushed after the failure,
        // and that flush fails.
        disk.borrow_mut().failures.insert(PathBuf::from("b"), 2);
        disk.borrow_mut().commit_failures = 1;
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(log(&disk).last().unwrap(), "commit a");
        let message = failure.to_string();
        assert!(
            message.starts_with("writing the edits to \"b\": "),
            "{message}"
        );
        assert!(
            message.contains("1 file could not be put back"),
            "{message}"
        );
        assert!(
            message.ends_with(
                "\nThe files put back could not all be flushed to disk, so a power loss could \
                 still lose them:\n  flushing them all: the commit flush failed"
            ),
            "{message}"
        );
    }

    #[test]
    fn a_file_put_back_but_not_flushed_counts_as_put_back_and_is_flushed_with_the_rest() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        // b's flush fails after its new bytes and again after its original ones.
        disk.borrow_mut()
            .flush_failures
            .insert(PathBuf::from("b"), 2);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "old b");
        assert_eq!(log(&disk).last().unwrap(), "commit a b");
        assert_eq!(
            failure.to_string(),
            "writing the edits to \"b\": the file flush failed; every file written was put back \
             as it was, so nothing is changed\nThe files put back could not all be flushed to \
             disk, so a power loss could still lose them:\n  \"b\": the file flush failed"
        );
    }

    #[test]
    fn nothing_is_flushed_after_a_failure_when_nothing_could_be_put_back() {
        let disk = disk(&[("a", "old a")]);
        disk.borrow_mut().failures.insert(PathBuf::from("a"), 2);
        let edits = [edit("a", "old a", "new a")];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(log(&disk), Vec::<String>::new());
        assert!(!failure.to_string().contains("flushed"), "{failure}");
    }

    #[test]
    fn a_file_that_cannot_be_opened_again_to_write_it_puts_back_every_file_written() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        // b opens when every file is checked, then refuses, as if its permissions had changed.
        disk.borrow_mut().opens_left.insert(PathBuf::from("b"), 1);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "old b");
        // b was never written, so only a is put back, and b is not reported as possibly changed.
        assert_eq!(
            log(&disk),
            [
                "write a \"new a\"",
                "flush a",
                "write a \"old a\"",
                "flush a",
                "commit a",
            ]
        );
        let message = failure.to_string();
        assert!(
            message.starts_with("writing the edits to \"b\": "),
            "{message}"
        );
        assert!(
            message.ends_with("; every file written was put back as it was, so nothing is changed"),
            "{message}"
        );
    }

    #[test]
    fn every_edit_is_written_with_one_file_open_at_a_time() {
        let disk = disk(&[("a", "old a"), ("b", "old b"), ("c", "old c")]);
        let edits = [
            edit("a", "old a", "new a"),
            edit("b", "old b", "new b"),
            edit("c", "old c", "new c"),
        ];
        commit_to(&disk, &edits, &[]).unwrap();
        assert_eq!(contents(&disk, "a"), "new a");
        assert_eq!(contents(&disk, "b"), "new b");
        assert_eq!(contents(&disk, "c"), "new c");
        // However many files change, the limit on open files is never reached.
        assert_eq!(disk.borrow().most_open, 1);
        assert_eq!(disk.borrow().open, 0);
    }

    #[test]
    fn the_systems_error_stays_on_the_chain() {
        use std::error::Error as _;
        let open = Failure::Open {
            path: "a".into(),
            cause: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        let write = Failure::Write {
            path: "a".into(),
            cause: io::Error::from(io::ErrorKind::StorageFull),
            unrestored: Vec::new(),
            unflushed: Vec::new(),
        };
        for (failure, kind) in [
            (open, io::ErrorKind::PermissionDenied),
            (write, io::ErrorKind::StorageFull),
        ] {
            assert_eq!(
                failure.source().map(ToString::to_string),
                Some(io::Error::from(kind).to_string())
            );
        }
    }

    #[test]
    fn a_file_that_cannot_be_opened_stops_the_run_before_anything_is_written() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_to(&disk, &edits, &["b"]).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "old b");
        let message = failure.to_string();
        assert!(
            message.starts_with("opening \"b\" to write the edits: "),
            "{message}"
        );
        assert!(message.ends_with("; nothing was written"), "{message}");
    }

    #[test]
    fn a_write_that_fails_puts_every_file_written_back_as_it_was() {
        let disk = disk(&[("a", "old a"), ("b", "old b"), ("c", "old c")]);
        // b's write fails partway; putting it back succeeds.
        disk.borrow_mut().failures.insert(PathBuf::from("b"), 1);
        let edits = [
            edit("a", "old a", "new a"),
            edit("b", "old b", "new b"),
            edit("c", "old c", "new c"),
        ];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "old b");
        assert_eq!(
            contents(&disk, "c"),
            "old c",
            "c was written after b failed"
        );
        let message = failure.to_string();
        assert!(
            message.starts_with("writing the edits to \"b\": "),
            "{message}"
        );
        assert!(
            message.ends_with("; every file written was put back as it was, so nothing is changed"),
            "{message}"
        );
    }

    #[test]
    fn a_file_that_cannot_be_put_back_is_named_as_left_changed() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        // b's write fails, and so does putting it back.
        disk.borrow_mut().failures.insert(PathBuf::from("b"), 2);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_to(&disk, &edits, &[]).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "#ld b", "b is left half-written");
        let message = failure.to_string();
        assert!(
            message.contains("1 file could not be put back as it was"),
            "{message}"
        );
        assert!(message.contains("\n  \"b\": "), "{message}");
        assert!(!message.contains("\"a\": "), "a was put back: {message}");
    }

    #[test]
    fn a_file_is_overwritten_in_place_keeping_its_links_and_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("quaffed-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        let link = dir.join("b.txt");
        std::fs::write(&file, "a longer original\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::hard_link(&file, &link).unwrap();
        let inode = std::fs::metadata(&file).unwrap().ino();
        let edits = [Edit {
            path: &file,
            shown: Path::new("a.txt"),
            original: b"a longer original\n",
            edited: b"short\n",
        }];
        let result = commit(&edits);
        let after = std::fs::metadata(&file).unwrap();
        let linked = std::fs::read(&link).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        result.unwrap();
        assert_eq!(
            after.ino(),
            inode,
            "the file was replaced, not written in place"
        );
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        assert_eq!(
            after.len(),
            6,
            "the file was not shortened to its new bytes"
        );
        assert_eq!(linked, b"short\n", "the hard link does not see the edit");
    }
}
