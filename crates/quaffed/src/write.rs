// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Writing a run's edits: every file in place, or none of them.
//!
//! Every file is written in place, into the file itself, so that whatever a user relies on in it
//! is untouched: its hard links, its owner and group, its permissions.  Writing a new file and
//! renaming it over the old one would be atomic, but it is a different file: a hard link would
//! keep the old contents, and the owner would become whoever ran quaff.
//!
//! Every file is opened for writing before any is written, so a file that cannot be written stops
//! the run with nothing changed.  If writing then fails - a full disk, say - every file already
//! written, and the one that failed, is written back from its original bytes, which the run still
//! holds.  What this cannot cover is the process being killed, or the machine losing power, while
//! a file is being written: that file can be left half-written.

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
            } => {
                write!(f, "writing the edits to {path:?}: {cause}")?;
                if unrestored.is_empty() {
                    return write!(
                        f,
                        "; every file written was put back as it was, so nothing is changed"
                    );
                }
                write!(
                    f,
                    "; {} could not be put back as {} {}, and {} left changed or half-written:",
                    count(unrestored.len()),
                    if unrestored.len() == 1 { "it" } else { "they" },
                    if unrestored.len() == 1 { "was" } else { "were" },
                    if unrestored.len() == 1 { "is" } else { "are" },
                )?;
                for (path, why) in unrestored {
                    write!(f, "\n  {path:?}: {why}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Open { cause, .. } | Failure::Write { cause, .. } => Some(cause),
        }
    }
}

fn count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "file" } else { "files" })
}

/// A file that can be written over with new bytes.
trait Target {
    /// Makes the file's whole contents `bytes`, and flushes them to disk.
    fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()>;
}

impl Target for File {
    fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(0))?;
        self.write_all(bytes)?;
        // After the write, so that a file never holds less than its new bytes while they are
        // being written: shortening first would lose the end of the original if the write failed.
        self.set_len(bytes.len() as u64)?;
        self.sync_all()
    }
}

/// Writes every edit in place, or none of them.
///
/// # Errors
///
/// Returns a [`Failure`] saying what could not be written and what state that left: nothing
/// changed when a file could not be opened, every file put back when a write failed, or which
/// files could not be put back.
pub fn commit(edits: &[Edit]) -> Result<(), Failure> {
    commit_with(edits, |path| OpenOptions::new().write(true).open(path))
}

/// [`commit`], opening each file with `open`.
fn commit_with<T: Target>(
    edits: &[Edit],
    mut open: impl FnMut(&Path) -> io::Result<T>,
) -> Result<(), Failure> {
    // Every file opened before any is written, so one that cannot be written changes nothing.
    let mut targets = Vec::with_capacity(edits.len());
    for edit in edits {
        let target = open(edit.path).map_err(|cause| Failure::Open {
            path: edit.shown.to_path_buf(),
            cause,
        })?;
        targets.push(target);
    }
    for (written, edit) in edits.iter().enumerate() {
        if let Err(cause) = targets[written].overwrite(edit.edited) {
            // The one that failed may be half-written, so it is put back too.
            let unrestored = edits[..=written]
                .iter()
                .zip(&mut targets)
                .filter_map(|(edit, target)| {
                    target
                        .overwrite(edit.original)
                        .err()
                        .map(|why| (edit.shown.to_path_buf(), why))
                })
                .collect();
            return Err(Failure::Write {
                path: edit.shown.to_path_buf(),
                cause,
                unrestored,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// Files in memory.
    #[derive(Default)]
    struct Disk {
        files: HashMap<PathBuf, Vec<u8>>,
    }

    struct InMemory {
        path: PathBuf,
        disk: Rc<RefCell<Disk>>,
    }

    impl Target for InMemory {
        fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.disk
                .borrow_mut()
                .files
                .insert(self.path.clone(), bytes.to_vec());
            Ok(())
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

    fn opener(
        disk: &Rc<RefCell<Disk>>,
        unopenable: &'static [&'static str],
    ) -> impl FnMut(&Path) -> io::Result<InMemory> {
        let disk = Rc::clone(disk);
        move |path: &Path| {
            if unopenable.iter().any(|p| Path::new(p) == path) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            Ok(InMemory {
                path: path.to_path_buf(),
                disk: Rc::clone(&disk),
            })
        }
    }

    #[test]
    fn every_edit_is_written() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        commit_with(&edits, opener(&disk, &[])).unwrap();
        assert_eq!(contents(&disk, "a"), "new a");
        assert_eq!(contents(&disk, "b"), "new b");
    }

    #[test]
    fn a_file_that_cannot_be_opened_stops_the_run_before_anything_is_written() {
        let disk = disk(&[("a", "old a"), ("b", "old b")]);
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        let failure = commit_with(&edits, opener(&disk, &["b"])).unwrap_err();
        assert_eq!(contents(&disk, "a"), "old a");
        assert_eq!(contents(&disk, "b"), "old b");
        let message = failure.to_string();
        assert!(
            message.starts_with("opening \"b\" to write the edits: "),
            "{message}"
        );
        assert!(message.ends_with("; nothing was written"), "{message}");
    }

    /// An in-memory file whose writes fail `failures` times, each leaving it half-written, then
    /// succeed.
    struct Flaky {
        inner: InMemory,
        failures: usize,
    }

    impl Target for Flaky {
        fn overwrite(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.failures > 0 {
                self.failures -= 1;
                // A write that failed partway, leaving a byte that is neither old nor new.
                let mut disk = self.inner.disk.borrow_mut();
                let file = disk.files.get_mut(&self.inner.path).unwrap();
                file[0] = b'#';
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            self.inner.overwrite(bytes)
        }
    }

    /// Commits `edits` to `disk`, the writes to `failing` failing `failures` times each.
    fn commit_flaky(
        disk: &Rc<RefCell<Disk>>,
        edits: &[Edit],
        failing: &str,
        failures: usize,
    ) -> Result<(), Failure> {
        let mut open = opener(disk, &[]);
        commit_with(edits, |path: &Path| {
            open(path).map(|inner| Flaky {
                failures: if path == Path::new(failing) {
                    failures
                } else {
                    0
                },
                inner,
            })
        })
    }

    #[test]
    fn a_write_that_fails_puts_every_file_written_back_as_it_was() {
        let disk = disk(&[("a", "old a"), ("b", "old b"), ("c", "old c")]);
        let edits = [
            edit("a", "old a", "new a"),
            edit("b", "old b", "new b"),
            edit("c", "old c", "new c"),
        ];
        // b's write fails partway; putting it back succeeds.
        let failure = commit_flaky(&disk, &edits, "b", 1).unwrap_err();
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
        let edits = [edit("a", "old a", "new a"), edit("b", "old b", "new b")];
        // b's write fails, and so does putting it back.
        let failure = commit_flaky(&disk, &edits, "b", 2).unwrap_err();
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
