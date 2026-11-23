// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `scripts/budgets` counts the files quaff opens, the bytes it reads from them and the threads
//! it starts from an strace log.  A run on several threads has its calls split across lines,
//! `<unfinished ...>` then `<... resumed>`, with the end of the call padded to line its result up,
//! and a call counted only when it is on one line would leave the counts to how the threads
//! happened to interleave.
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
    parsed(
        name,
        log,
        "for path, counts in sorted(module.file_access(log).items()):\n\
         \x20   print(path, *counts)\n",
    )
}

/// How many threads `scripts/budgets` counts the run in the strace log `log` starting.
fn threads_started(name: &str, log: &str) -> String {
    parsed(name, log, "print(module.threads_started(log))\n")
}

/// What `print`, Python run with the script loaded as `module` and the log's path as `log`,
/// prints.
fn parsed(name: &str, log: &str, print: &str) -> String {
    let dir = std::env::temp_dir().join(format!("quaffed-budgets-traces-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("creating the directory");
    let path = dir.join(format!("{name}.strace"));
    fs::write(&path, log).expect("writing the log");
    let script = workspace_root().join("scripts/budgets");
    let program = "import importlib.machinery, importlib.util, pathlib, sys\n\
         loader = importlib.machinery.SourceFileLoader('budgets', sys.argv[1])\n\
         module = importlib.util.module_from_spec(importlib.util.spec_from_loader('budgets', loader))\n\
         loader.exec_module(module)\n\
         log = pathlib.Path(sys.argv[2])\n"
        .to_owned()
        + print;
    let output = Command::new("python3")
        .args(["-I", "-c", &program])
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

#[test]
fn each_thread_started_is_counted_once_and_a_new_process_not_at_all() {
    // As strace -f -y writes musl starting two threads with clone, the second's call split by the
    // first thread's read, and glibc starting one with clone3; then a clone without
    // CLONE_THREAD, which starts a process, and one that failed.
    let log = "\
4012  clone(child_stack=0x7f3a1c1ff000, flags=CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD|CLONE_SYSVSEM|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID|0x400000, parent_tid=[4013], tls=0x7f3a1c1ffb38, child_tidptr=0x7f3a1c1ffb48) = 4013
4012  clone(child_stack=0x7f3a1bffe000, flags=CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD|CLONE_SYSVSEM|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID|0x400000 <unfinished ...>
4013  read(3</p/a.py>, \"\"..., 501) = 501
4012  <... clone resumed>, parent_tid=[4014], tls=0x7f3a1bffeb38, child_tidptr=0x7f3a1bffeb48) = 4014
4012  clone3({flags=CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD|CLONE_SYSVSEM|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID, child_tid=0x7f3a1b7fd910, parent_tid=0x7f3a1b7fd910, exit_signal=0, stack=0x7f3a1affd000, stack_size=0x7fff00, tls=0x7f3a1b7fd640} => {parent_tid=[4015]}, 88) = 4015
4012  clone(child_stack=NULL, flags=CLONE_CHILD_CLEARTID|CLONE_CHILD_SETTID|SIGCHLD, child_tidptr=0x7f3a1d000a10) = 4016
4012  clone(child_stack=0x7f3a1a7fc000, flags=CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD|CLONE_SYSVSEM|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID|0x400000, parent_tid=0x7f3a1a7fcb38, tls=0x7f3a1a7fcb38, child_tidptr=0x7f3a1a7fcb48) = -1 EAGAIN (Resource temporarily unavailable)
";
    assert_eq!(threads_started("threads", log), "3\n");
}
