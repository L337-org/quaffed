// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! How this crate lays out its source files, for the tests that read them: which files are test
//! modules, which the containment checks leave out.

use std::fs;
use std::path::Path;

/// Whether `path` is a test module in a file of its own, as its parent module declares it:
/// `#[cfg(test)]` and then `mod <name>;`, in the file named for the directory it is in, as this
/// crate lays out a module's children.  A test may read freely, as the test module inside a file
/// may, and is left out whole.  Known by its declaration rather than its name, so that no product
/// file is left out for being named like one; a test module laid out any other way is read as
/// product code, which fails loudly rather than skipping anything.
pub fn is_test_module(path: &Path) -> bool {
    let (Some(name), Some(dir)) = (path.file_stem(), path.parent()) else {
        return false;
    };
    let declared = format!("#[cfg(test)]\nmod {};", name.to_string_lossy());
    fs::read_to_string(dir.with_extension("rs")).is_ok_and(|text| text.contains(&declared))
}

#[test]
fn a_test_module_in_a_file_of_its_own_is_known_by_its_parents_declaration() {
    let dir = std::env::temp_dir().join(format!("quaffed-test-module-{}", std::process::id()));
    fs::create_dir_all(dir.join("parent")).expect("creating the directory");
    fs::write(
        dir.join("parent.rs"),
        "mod tested;\n#[cfg(test)]\nmod tests;\n",
    )
    .expect("writing");
    let tests = is_test_module(&dir.join("parent/tests.rs"));
    let tested = is_test_module(&dir.join("parent/tested.rs"));
    let elsewhere = is_test_module(&dir.join("tests.rs"));
    let _ = fs::remove_dir_all(&dir);
    assert!(tests, "a file its parent declares a test module is one");
    assert!(
        !tested,
        "a file its parent declares a plain module is product code"
    );
    assert!(
        !elsewhere,
        "a file named tests.rs that nothing declares is product code"
    );
}
