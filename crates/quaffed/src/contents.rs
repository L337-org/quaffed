// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The bytes of the files quaff reads for their rules, kept so that no file is read twice.
//!
//! An ignore file or a declaration file - a `.gitignore`, an `.editorconfig` - may be in the
//! search's scope as well as read for its rules.  Each reader names the files it reads, by file
//! name, and every read of a file with such a name goes through here: the first reads it from
//! disk and keeps it, and every later one, the search's or a reader's, has the kept bytes.  The
//! order does not matter, so a file searched before it is read for its rules is not read again.
//! Nothing else is kept, so the search holds no file it has finished with.
//!
//! Every read records the file's [`Identity`] as well as its bytes, so that a file can be checked
//! with [`unchanged`] before it is written, which is the one read that bypasses the store.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::identity::Identity;

/// The files read for their rules, shared by every reader of a run.
#[derive(Clone, Default)]
pub struct Contents {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// File names a reader reads its rules from, such as `.gitignore`.
    names: HashSet<OsString>,
    /// Particular files read before the search and possibly searched too, such as a script.
    paths: HashSet<PathBuf>,
    kept: HashMap<PathBuf, (Arc<[u8]>, Identity)>,
}

/// A file's bytes, as [`Contents::read`] gives them: kept and shared, or read for this caller
/// alone; and the file's identity when they were read.
#[derive(Debug)]
pub enum Bytes {
    /// Kept by the store, for whichever reader comes next.
    Kept(Arc<[u8]>, Identity),
    /// Read for this caller alone, so never copied.
    Read(Vec<u8>, Identity),
}

impl Bytes {
    /// The file's identity, taken as these bytes were read.
    pub fn identity(&self) -> &Identity {
        match self {
            Bytes::Kept(_, identity) | Bytes::Read(_, identity) => identity,
        }
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Kept(bytes, _) => bytes,
            Bytes::Read(bytes, _) => bytes,
        }
    }
}

/// Whether a file is as it was read, as [`unchanged`] finds it.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    /// It holds the bytes it was read with.
    Unchanged,
    /// It holds other bytes, or is no longer a file.
    Changed,
    /// It is no longer there.
    Gone,
}

#[cfg(test)]
thread_local! {
    /// Every path read from disk on this thread, so a test can count them.
    pub static DISK_READS: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl Contents {
    /// Names `name` as a file name a reader reads its rules from, so that every file with that
    /// name is kept once read.
    pub fn keep_named(&self, name: &str) {
        self.lock().names.insert(OsString::from(name));
    }

    /// Names `path` as a file to keep once read, whatever its name: one read before the search
    /// that the search may read again, such as a script given with `-f` that is in the scope.
    pub fn keep_path(&self, path: &Path) {
        self.lock().paths.insert(path.to_path_buf());
    }

    /// `path`'s bytes: the kept ones if it has been read before, otherwise read from disk, and
    /// kept if a reader reads files of its name or it was named with `keep_path`.
    ///
    /// # Errors
    ///
    /// Returns the system's error, verbatim, when the file cannot be read.
    pub fn read(&self, path: &Path) -> io::Result<Bytes> {
        if let Some((bytes, identity)) = self.lock().kept.get(path) {
            return Ok(Bytes::Kept(Arc::clone(bytes), *identity));
        }
        let (bytes, identity) = read_from_disk(path)?;
        let mut inner = self.lock();
        if inner.paths.contains(path)
            || path
                .file_name()
                .is_some_and(|name| inner.names.contains(name))
        {
            // Shared, so a later reader has them; copying into it is paid only for the files
            // kept, a handful of small ones, never for every file searched.
            let kept: Arc<[u8]> = Arc::from(bytes);
            inner
                .kept
                .insert(path.to_path_buf(), (Arc::clone(&kept), identity));
            return Ok(Bytes::Kept(kept, identity));
        }
        Ok(Bytes::Read(bytes, identity))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("no thread panics holding it")
    }
}

/// `path`'s bytes from disk, and its identity taken from the open file before they were read, so
/// that a write landing while they are read is seen as a change when the file is checked.
fn read_from_disk(path: &Path) -> io::Result<(Vec<u8>, Identity)> {
    #[cfg(test)]
    DISK_READS.with(|reads| reads.borrow_mut().push(path.to_path_buf()));
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let identity = Identity::of(&metadata, SystemTime::now());
    // The size is only a hint: the file may grow or shrink while it is read.
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok((bytes, identity))
}

/// Whether the file at `path` is still as it was read with `identity` and `original` bytes, for
/// checking just before it is written.  Its metadata is read, and its bytes are read again and
/// compared only when `identity` cannot vouch for that metadata, which for a file nobody touched
/// since a moment before the run is never.  Read straight from disk, bypassing the store, which
/// holds what the file was.
///
/// # Errors
///
/// Returns the system's error, verbatim, when the file's metadata or bytes cannot be read for any
/// reason but its no longer being there, which is [`Check::Gone`].
pub fn unchanged(path: &Path, identity: &Identity, original: &[u8]) -> io::Result<Check> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Check::Gone),
        Err(err) => return Err(err),
    };
    if !metadata.is_file() {
        return Ok(Check::Changed);
    }
    if identity.vouches_for(&Identity::of(&metadata, SystemTime::now())) {
        return Ok(Check::Unchanged);
    }
    let (now, _) = match read_from_disk(path) {
        Ok(read) => read,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Check::Gone),
        Err(err) => return Err(err),
    };
    Ok(if now == original {
        Check::Unchanged
    } else {
        Check::Changed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk_reads_of(path: &Path) -> usize {
        DISK_READS.with(|reads| reads.borrow().iter().filter(|read| *read == path).count())
    }

    #[test]
    fn a_named_file_is_read_from_disk_once_and_any_other_every_time() {
        let dir = std::env::temp_dir().join(format!("quaffed-contents-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rules = dir.join(".gitignore");
        let other = dir.join("a.py");
        fs::write(&rules, "*.log\n").unwrap();
        fs::write(&other, "x\n").unwrap();
        let contents = Contents::default();
        contents.keep_named(".gitignore");
        for _ in 0..3 {
            assert_eq!(&*contents.read(&rules).unwrap(), b"*.log\n");
            assert_eq!(&*contents.read(&other).unwrap(), b"x\n");
        }
        let missing = contents.read(&dir.join("missing")).unwrap_err().kind();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(disk_reads_of(&rules), 1);
        assert_eq!(disk_reads_of(&other), 3);
        assert_eq!(missing, io::ErrorKind::NotFound);
    }

    /// A fresh directory for one test, holding `a.py` with `bytes`.
    fn one_file(test: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("quaffed-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.py");
        fs::write(&file, bytes).unwrap();
        (dir, file)
    }

    /// The identity of `file` as if it were read long after its last change, so that its
    /// timestamps are trusted.
    fn settled(file: &Path) -> Identity {
        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        Identity::of(&fs::metadata(file).unwrap(), later)
    }

    #[test]
    fn an_unchanged_file_with_settled_timestamps_is_not_read_again() {
        let (dir, file) = one_file("unchanged", b"x = 1\n");
        let identity = settled(&file);
        DISK_READS.with(|reads| reads.borrow_mut().clear());
        let check = unchanged(&file, &identity, b"x = 1\n").unwrap();
        let reads = disk_reads_of(&file);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(check, Check::Unchanged);
        assert_eq!(reads, 0, "an unchanged file's bytes were read again");
    }

    #[test]
    fn a_file_read_as_it_was_changed_has_its_bytes_compared() {
        // Just written, so a second write in the same clock tick could leave the same times.
        let (dir, file) = one_file("racy", b"x = 1\n");
        let bytes = Contents::default().read(&file).unwrap();
        DISK_READS.with(|reads| reads.borrow_mut().clear());
        let same = unchanged(&file, bytes.identity(), b"x = 1\n").unwrap();
        let reads = disk_reads_of(&file);
        let changed = unchanged(&file, bytes.identity(), b"x = 2\n").unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            (same, reads, changed),
            (Check::Unchanged, 1, Check::Changed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_read_long_after_its_last_change_is_not_read_again_when_unchanged() {
        let (dir, file) = one_file("settled-read", b"x = 1\n");
        // Past the racy window, so the identity the read records is trusted.
        std::thread::sleep(std::time::Duration::from_millis(2100));
        let bytes = Contents::default().read(&file).unwrap();
        DISK_READS.with(|reads| reads.borrow_mut().clear());
        let check = unchanged(&file, bytes.identity(), &bytes).unwrap();
        let reads = disk_reads_of(&file);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!((check, reads), (Check::Unchanged, 0));
    }

    #[cfg(unix)]
    #[test]
    fn a_same_size_edit_with_its_modification_time_restored_is_seen() {
        let (dir, file) = one_file("restored", b"x = 1\n");
        let identity = settled(&file);
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        // Past the coarsest tick Linux stamps files with, so the change time must move.
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&file, b"x = 2\n").unwrap();
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let now = fs::metadata(&file).unwrap();
        assert_eq!(
            now.modified().unwrap(),
            modified,
            "the time was not restored"
        );
        let vouched = identity.vouches_for(&Identity::of(&now, SystemTime::now()));
        let check = unchanged(&file, &identity, b"x = 1\n").unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(!vouched, "the change time did not show the edit");
        assert_eq!(check, Check::Changed);
    }

    #[test]
    fn a_file_replaced_or_removed_is_changed_or_gone() {
        let (dir, file) = one_file("replaced", b"x = 1\n");
        let identity = settled(&file);
        // Renamed over it with the same bytes, as an editor saves: compared, and the same.
        let other = dir.join("b.py");
        fs::write(&other, b"x = 1\n").unwrap();
        fs::rename(&other, &file).unwrap();
        let renamed = unchanged(&file, &identity, b"x = 1\n").unwrap();
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        let directory = unchanged(&file, &identity, b"x = 1\n").unwrap();
        fs::remove_dir(&file).unwrap();
        let gone = unchanged(&file, &identity, b"x = 1\n").unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            (renamed, directory, gone),
            (Check::Unchanged, Check::Changed, Check::Gone)
        );
    }
}
