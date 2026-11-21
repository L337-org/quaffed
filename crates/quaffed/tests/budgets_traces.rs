// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `scripts/budgets` counts the files quaff opens and the bytes it reads from an strace log.  A
//! run on several threads has its calls split across lines, `<unfinished ...>` then `<... resumed>`,
//! with the end of the call padded to line its result up, and a call counted only when it is on
//! one line would leave the counts to how the threads happened to interleave.
//!
//! Each test runs the script's own parser with `python3` over a log written here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

/// What `scripts/budgets` makes of the strace log `log`: each path, with how many times it was
/// opened to read, the bytes read from it and how many times it was opened to write, one line
/// each, sorted.
fn file_access(name: &str, log: &str) -> String {
    let dir = std::env::temp_dir().join(format!("quaffed-budgets-traces-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("creating the directory");
    let path = dir.join(format!("{name}.strace"));
    fs::write(&path, log).expect("writing the log");
    let script = workspace_root().join("scripts/budgets");
    let program = "import importlib.machinery, importlib.util, pathlib, sys\n\
         loader = importlib.machinery.SourceFileLoader('budgets', sys.argv[1])\n\
         module = importlib.util.module_from_spec(importlib.util.spec_from_loader('budgets', loader))\n\
         loader.exec_module(module)\n\
         for path, counts in sorted(module.file_access(pathlib.Path(sys.argv[2])).items()):\n\
         \x20   print(path, *counts)\n";
    let output = Command::new("python3")
        .args(["-I", "-c", program])
        .arg(&script)
        .arg(&path)
        .output()
        .expect("running python3");
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8")
}

#[test]
fn a_call_split_across_lines_by_another_thread_is_counted_once_whole() {
    // As strace -f -y writes two threads opening and reading two files at once, each call split
    // by the other's, and the resumed ends padded.
    let log = "\
4012  open(\"/p/a.py\", O_RDONLY|O_CLOEXEC <unfinished ...>
4013  openat(AT_FDCWD, \"/p/b.py\", O_RDONLY|O_CLOEXEC) = 4</p/b.py>
4012  <... open resumed>)               = 3</p/a.py>
4013  read(4</p/b.py>,  <unfinished ...>
4012  read(3</p/a.py>, \"\"..., 501) = 501
4013  <... read resumed>\"\"..., 102)     = 102
4012  read(3</p/a.py>,  <unfinished ...>
4013  read(4</p/b.py>, \"\", 32)          = 0
4012  <... read resumed>\"\", 32)         = 0
4012  openat(AT_FDCWD, \"/p/c.py\", O_WRONLY|O_CREAT|O_CLOEXEC <unfinished ...>
4013  close(4</p/b.py>)                 = 0
4012  <... openat resumed>)             = 5</p/c.py>
";
    assert_eq!(
        file_access("split", log),
        "/p/a.py 1 501 0\n/p/b.py 1 102 0\n/p/c.py 0 0 1\n"
    );
}
