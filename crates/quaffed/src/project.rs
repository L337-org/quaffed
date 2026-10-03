// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The project root: the directory a search covers when no scope narrows it.

use std::path::{Path, PathBuf};

/// The name whose presence marks a project root.
///
/// Tested for existence, not type: a `.git` file - a submodule's or a linked worktree's - marks
/// a root as surely as a `.git` directory.  `.gitignore` is not a marker.
const MARKER: &str = ".git";

/// Returns the nearest directory at or above `start` that contains the marker, if any.
///
/// There may be no project root at all, and that is not an error: the caller searches from
/// where it is instead.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        // symlink_metadata, so a dangling symbolic link named .git still counts as present.
        .find(|dir| dir.join(MARKER).symlink_metadata().is_ok())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("quaffed-project-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creating a temporary directory");
        dir
    }

    #[test]
    fn the_root_is_found_from_a_subdirectory() {
        let dir = temp_dir("subdir");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("a/b")).unwrap();
        assert_eq!(find_root(&dir.join("a/b")), Some(dir.clone()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_git_file_marks_a_root_too() {
        let dir = temp_dir("gitfile");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
        assert_eq!(find_root(&dir.join("sub")), Some(dir.join("sub")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gitignore_is_not_a_marker() {
        let dir = temp_dir("gitignore");
        fs::write(dir.join(".gitignore"), "x\n").unwrap();
        // The temporary directory's own ancestors carry no marker on any machine this runs on;
        // if one ever did, this would name it rather than pass for the wrong reason.
        assert_eq!(find_root(&dir), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
