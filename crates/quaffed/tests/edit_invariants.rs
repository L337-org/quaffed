//! Every editing operation, once registered in [`CASES`], is checked over the corpus for the
//! guarantees an edit exists to give: minimal diff, atomicity, determinism, idempotency and
//! still-parses.
//!
//! [`CASES`] says what to edit and [`languages`] what the corpus holds; `edit_harness/` runs the
//! real `quaff` on fresh copies and judges each run, and nothing in it knows an operation or a
//! language.  For each case, over each language it names:
//!
//! - **Atomicity**: the edit expecting one more match than `find` counts exits 2, every file
//!   byte-identical.
//! - **Minimal diff**: the edit expecting the count changes nothing outside the matches `find`
//!   reports, checked by bytes and again by whole lines.
//! - **Determinism**: two runs give the same exit, output and files.
//! - **Still parses**: every file the language's oracle accepted before, it accepts after; a case
//!   that would break one must be refused with exit 9, nothing written.
//! - **Idempotency**: the edit is applied again to its own output, and judged by the same rules
//!   against a `find` on that output, whether or not it then changes anything.
//!
//! The harness's own checks are tested in the ordinary run, in `edit_harness/tests.rs`.  The corpus
//! run is ignored by default, because it needs the fetched real-world corpus and the oracle's
//! Python; the **Real-world corpus** CI job runs it.  `architecture/corpus.md` describes it, with
//! the checklists for registering an operation and adding a language.

mod edit_harness;

use std::path::{Path, PathBuf};

use edit_harness::{
    Binary, Case, Files, Judge, Language, Oracle, Outcome, check, is_python, read_tree,
};

/// Every registered case.  A case gives a `find` and the edit whose matches it counts, both
/// without `expect`, which the harness adds; see the checklist in `architecture/corpus.md`.
const CASES: &[Case] = &[
    Case {
        operation: "replace string",
        exercises: "a common word",
        find: r#"find "the""#,
        edit: r#"replace string "the" with "THE""#,
        languages: &["Python", "text"],
        outcome: Outcome::Edits,
    },
    Case {
        operation: "replace string",
        exercises: "a replacement holding its own match",
        find: r#"find "the""#,
        edit: r#"replace string "the" with "thethe""#,
        languages: &["Python", "text"],
        outcome: Outcome::Edits,
    },
    Case {
        operation: "replace string",
        exercises: "an anchored match",
        find: r##"find ^"# ""##,
        edit: r##"replace string ^"# " with "#  ""##,
        languages: &["Python", "text"],
        outcome: Outcome::Edits,
    },
    Case {
        operation: "replace string",
        exercises: "a line break written",
        find: r#"find "the ""#,
        edit: r#"replace string "the " with "the" $^ """#,
        languages: &["text"],
        outcome: Outcome::Edits,
    },
    Case {
        operation: "replace string",
        exercises: "a line break matched and written",
        find: r#"find "." $^ """#,
        edit: r#"replace string "." $^ "" with ";" $^ """#,
        languages: &["text"],
        outcome: Outcome::Edits,
    },
    Case {
        operation: "replace string",
        exercises: "an edit breaking files that parsed",
        find: r#"find "def ""#,
        edit: r#"replace string "def " with "def def ""#,
        languages: &["Python"],
        outcome: Outcome::Refused,
    },
];

/// The environment variable naming the oracle's Python: the release `corpus/real-world.toml`
/// names, which `scripts/oracle-python` checks it is.
const PYTHON_ORACLE: &str = "QUAFFED_PYTHON_ORACLE";

/// Every language the corpus holds, with what judges it.  Text is every file no structural
/// language claims, and has no oracle.  See the checklist in `architecture/corpus.md`.
fn languages() -> Vec<Language> {
    let python = match std::env::var_os(PYTHON_ORACLE) {
        Some(program) => Judge::Oracle(Oracle {
            program: PathBuf::from(program),
            args: vec![workspace_root().join("scripts/oracle-python").into()],
        }),
        None => Judge::Unavailable(format!(
            "{PYTHON_ORACLE} is not set; set it to the Python release corpus/real-world.toml \
             names as its oracle"
        )),
    };
    vec![
        Language {
            name: "Python",
            claims: is_python,
            judge: python,
        },
        Language {
            name: "text",
            claims: |path| !is_python(path),
            judge: Judge::NotApplicable,
        },
    ]
}

/// The workspace root.
fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/quaffed; the workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/quaffed has a grandparent")
        .to_path_buf()
}

/// Every file of the corpus: the synthetic corpus, the text corpus, and each fetched source of
/// the real-world corpus, each under its own prefix so that no two collide.
///
/// A fetched source's own files, `refused.txt` and the like, sit at its top level, and the
/// corpus in directories below it, so only those directories are read.
fn corpus() -> Files {
    let root = workspace_root();
    let mut files = Files::new();
    let mut add = |prefix: &str, dir: &Path| {
        let found = read_tree(dir);
        assert!(!found.is_empty(), "found no file under {}", dir.display());
        files.extend(
            found
                .into_iter()
                .map(|(path, bytes)| (format!("{prefix}/{path}"), bytes)),
        );
    };
    add("synthetic", &root.join("corpus/synthetic"));
    add("text", &root.join("corpus/text"));
    let fetched = root.join("target/corpus");
    let sources = std::fs::read_dir(&fetched).unwrap_or_else(|err| {
        panic!(
            "listing {}: {err}.  Run scripts/fetch-corpus first",
            fetched.display()
        )
    });
    let mut read = 0;
    for source in sources {
        let source = source.expect("reading a directory entry").path();
        if !source.join("refused.txt").is_file() {
            continue;
        }
        let name = source
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        for part in std::fs::read_dir(&source).expect("listing a fetched source") {
            let part = part.expect("reading a directory entry").path();
            if part.is_dir() {
                let below = part
                    .file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned();
                add(&format!("{name}/{below}"), &part);
                read += 1;
            }
        }
    }
    assert!(
        read > 0,
        "no fetched source under {}: run scripts/fetch-corpus first",
        fetched.display()
    );
    files
}

#[test]
#[ignore = "needs the fetched real-world corpus and the oracle's Python; the Real-world corpus CI job runs it"]
fn every_registered_operation_keeps_the_invariants_over_the_corpus() {
    let failures = check(CASES, &languages(), &corpus(), &Binary);
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}
