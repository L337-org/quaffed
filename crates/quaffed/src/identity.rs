// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! What a file was when quaff read it, so that before writing it quaff can tell, from its
//! metadata alone, whether anything may have changed it since.
//!
//! Each system has its own [`platform`] module, so that one can change without the others.  An
//! identity is only ever compared with another of the same file, taken later in the same run.
//! [`Identity::vouches_for`] saying no means only that the file may have changed: the caller then
//! compares the file's bytes, so a difference here costs a read but never refuses a write by
//! itself.  Saying yes is trusted, so the file is not read again.

use std::fs::Metadata;
use std::time::SystemTime;

/// A file's identity, from its metadata, compared rather than inspected.
#[derive(Clone, Copy, Debug)]
pub struct Identity {
    stamp: platform::Stamp,
    /// Whether the file's timestamps were old enough, when it was read, that a later change
    /// must move them.
    settled: bool,
}

impl Identity {
    /// The identity of the file `metadata` describes, read at `now`.
    pub fn of(metadata: &Metadata, now: SystemTime) -> Identity {
        Identity {
            stamp: platform::stamp(metadata),
            settled: platform::settled(metadata, now),
        }
    }

    /// Whether `later`, the same file's identity taken since, shows it unchanged, so that its
    /// bytes need not be compared.
    pub fn vouches_for(&self, later: &Identity) -> bool {
        self.settled && self.stamp == later.stamp
    }
}

/// Linux, macOS and every other Unix: the device and inode, the size, and the modification and
/// change times to the nanosecond.
///
/// The inode catches a file replaced by renaming another over it, which is how most editors
/// save; the size and modification time catch most writes in place.  The change time, `ctime`,
/// catches the rest: every write and every change of the modification time sets it to the
/// current time, and nothing short of changing the system clock sets it back.  So an edit of the
/// same length whose modification time is then restored, as `touch -r`, `cp -p` or a restore
/// from backup does, is still seen.  A change of owner, permissions or link count sets it too,
/// which costs a comparison of the bytes and nothing more.
///
/// **A timestamp is only as fine as the clock that set it**: Linux stamps files from a clock that
/// ticks every few milliseconds, and FAT keeps two seconds.  Two writes in one tick leave the
/// same times, so a file changed in the tick quaff read it in could change again unseen.  As git
/// does for its "racily clean" files, a file whose newest timestamp is within [`RACY`] of the
/// read is not vouched for at all, and has its bytes compared before it is written.
#[cfg(unix)]
mod platform {
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, SystemTime};

    /// How recent a timestamp must be for a change in the same tick to be possible: the
    /// coarsest a filesystem quaff may write keeps, FAT's two seconds.
    const RACY: Duration = Duration::from_secs(2);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Stamp {
        device: u64,
        inode: u64,
        size: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }

    pub fn stamp(metadata: &Metadata) -> Stamp {
        Stamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    pub fn settled(metadata: &Metadata, now: SystemTime) -> bool {
        let newest = (metadata.mtime(), metadata.mtime_nsec())
            .max((metadata.ctime(), metadata.ctime_nsec()));
        // A time before 1970, on either side, vouches for nothing.
        let Ok(now) = now.duration_since(SystemTime::UNIX_EPOCH) else {
            return false;
        };
        let (Ok(seconds), Ok(nanoseconds)) = (u64::try_from(newest.0), u32::try_from(newest.1))
        else {
            return false;
        };
        Duration::new(seconds, nanoseconds)
            .checked_add(RACY)
            .is_some_and(|safe| safe < now)
    }
}

/// Every system that is not Unix, Windows among them: no stamp yet, so every file is vouched for
/// and written without its bytes being compared.  A file that is no longer there, or no longer a
/// file, is still refused, since that is seen from whether its metadata can be read.
#[cfg(not(unix))]
mod platform {
    use std::fs::Metadata;
    use std::time::SystemTime;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Stamp;

    pub fn stamp(_: &Metadata) -> Stamp {
        Stamp
    }

    pub fn settled(_: &Metadata, _: SystemTime) -> bool {
        true
    }
}
