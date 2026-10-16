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

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    kept: HashMap<PathBuf, Arc<[u8]>>,
}

/// A file's bytes, as [`Contents::read`] gives them: kept and shared, or read for this caller
/// alone.
#[derive(Debug)]
pub enum Bytes {
    /// Kept by the store, for whichever reader comes next.
    Kept(Arc<[u8]>),
    /// Read for this caller alone, so never copied.
    Read(Vec<u8>),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Kept(bytes) => bytes,
            Bytes::Read(bytes) => bytes,
        }
    }
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
        if let Some(bytes) = self.lock().kept.get(path) {
            return Ok(Bytes::Kept(Arc::clone(bytes)));
        }
        #[cfg(test)]
        DISK_READS.with(|reads| reads.borrow_mut().push(path.to_path_buf()));
        let bytes = fs::read(path)?;
        let mut inner = self.lock();
        if inner.paths.contains(path)
            || path
                .file_name()
                .is_some_and(|name| inner.names.contains(name))
        {
            // Shared, so a later reader has them; copying into it is paid only for the files
            // kept, a handful of small ones, never for every file searched.
            let kept: Arc<[u8]> = Arc::from(bytes);
            inner.kept.insert(path.to_path_buf(), Arc::clone(&kept));
            return Ok(Bytes::Kept(kept));
        }
        Ok(Bytes::Read(bytes))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("no thread panics holding it")
    }
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
}
