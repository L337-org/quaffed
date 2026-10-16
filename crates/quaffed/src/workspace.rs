// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The version control system a project is kept in, as far as quaff needs one.
//!
//! Everything quaff takes from a version control system goes through [`Workspace`]: where the
//! project root is, which files its ignore rules leave out, and what it declares about a path.
//! Nothing else names a system, so supporting another means adding an implementation and
//! listing it in [`SYSTEMS`].  Only a system's own files in the project are read, never a user's
//! or the machine's configuration, so the same project gives the same answer anywhere.

mod git;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// What a version control system tells quaff about a project.
pub trait Workspace {
    /// The project root, which a search covers when no scope narrows it.
    fn root(&self) -> &Path;

    /// Sets `walk` to leave out what this system's ignore files leave out.
    fn ignore_rules(&self, walk: &mut ignore::WalkBuilder);

    /// The encoding this system's own files declare for the file at `path`, under the root, if
    /// they declare one, with the declaring file shown relative to `shown_from`.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when a file that could declare one cannot be read, so that a file is
    /// never read in the wrong encoding for want of its declaration.
    fn declared_encoding(
        &mut self,
        path: &Path,
        shown_from: &Path,
    ) -> Result<Option<Declared>, Error>;

    /// Each line of this system's files that could not be understood and was passed over,
    /// described for the user, in the order met, its file shown as `declared_encoding` was asked
    /// to show it.
    fn skipped(&self) -> &[String];

    /// Where this system lets a project declare a file's encoding, as a message tells the user,
    /// such as `.gitattributes`.
    fn declares_encodings_in(&self) -> &'static str;
}

/// An encoding a project declares for a file, by name, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// The name, as the declaring file spells it, such as `SHIFT-JIS`.
    pub name: String,
    /// The file and line that declare it, as a message names them.
    pub origin: Origin,
}

/// Where a declaration was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// What kind of declaration it is, such as `working-tree-encoding`.
    pub kind: &'static str,
    /// The file it is in.
    pub file: PathBuf,
    /// Its line, from 1.
    pub line: usize,
}

/// A file a system keeps its declarations in could not be read.
#[derive(Debug)]
pub struct Error {
    /// The file.
    pub path: PathBuf,
    /// The system's error, verbatim.
    pub cause: io::Error,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reading {:?}: {}", self.path, self.cause)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// How a version control system finds its project root at or above a directory, if there is one.
type Find = fn(&Path) -> Option<Box<dyn Workspace>>;

/// Each version control system quaff knows.
const SYSTEMS: &[Find] = &[git::find];

/// The workspace `start` is in: the nearest project root any known system finds at or above
/// it, or none.
///
/// With no project, there is no root and no declarations; the caller searches from where it is
/// instead, and that is not an error.
#[must_use]
pub fn find(start: &Path) -> Option<Box<dyn Workspace>> {
    // The nearest root wins, whichever system finds it, so a project nested in another is its
    // own project.
    SYSTEMS
        .iter()
        .filter_map(|system| system(start))
        .max_by_key(|workspace| workspace.root().components().count())
}

/// Ignore rules with no version control system: only the `.ignore` files any project may have,
/// which are not a system's.
pub fn no_system_ignore_rules(walk: &mut ignore::WalkBuilder) {
    walk.git_ignore(false).git_exclude(false).git_global(false);
}
