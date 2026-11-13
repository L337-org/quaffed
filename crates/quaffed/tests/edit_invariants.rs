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
//!   reports, checked by bytes and again by whole lines; for a deletion, by bytes outside what it
//!   may take next to each match alone.
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
    Binary, Case, Files, Judge, Language, Oracle, Outcome, Quaff, Run, Scratch, check, differing,
    is_python, read_tree,
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
        parsed_only: false,
        deletes: false,
    },
    Case {
        operation: "replace string",
        exercises: "a replacement holding its own match",
        find: r#"find "the""#,
        edit: r#"replace string "the" with "thethe""#,
        languages: &["Python", "text"],
        outcome: Outcome::Edits,
        parsed_only: false,
        deletes: false,
    },
    Case {
        operation: "replace string",
        exercises: "an anchored match",
        find: r##"find ^"# ""##,
        edit: r##"replace string ^"# " with "#  ""##,
        languages: &["Python", "text"],
        outcome: Outcome::Edits,
        parsed_only: false,
        deletes: false,
    },
    Case {
        operation: "replace string",
        exercises: "a line break written",
        find: r#"find "the ""#,
        edit: r#"replace string "the " with "the" $^ """#,
        languages: &["text"],
        outcome: Outcome::Edits,
        parsed_only: false,
        deletes: false,
    },
    Case {
        operation: "replace string",
        exercises: "a line break matched and written",
        find: r#"find "." $^ """#,
        edit: r#"replace string "." $^ "" with ";" $^ """#,
        languages: &["text"],
        outcome: Outcome::Edits,
        parsed_only: false,
        deletes: false,
    },
    Case {
        operation: "replace string",
        exercises: "an edit breaking files that parsed",
        find: r#"find "def ""#,
        edit: r#"replace string "def " with "def def ""#,
        languages: &["Python"],
        outcome: Outcome::Refused("unparseable"),
        parsed_only: false,
        deletes: false,
    },
    // A structural replace runs only over files that parse: it refuses a run with any it cannot,
    // by design, and the corpus holds some on purpose.
    Case {
        operation: "replace",
        exercises: "captures written back",
        find: "find `print($a)`",
        edit: "replace `print($a)` with `log($a)`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: false,
    },
    Case {
        operation: "replace",
        exercises: "holes reordered, trailing commas carried",
        find: "find `isinstance($x, $t)`",
        edit: "replace `isinstance($x, $t)` with `instance_of($t, $x)`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: false,
    },
    Case {
        operation: "replace",
        exercises: "a replacement holding its own match",
        find: "find `isinstance($x, $t)`",
        edit: "replace `isinstance($x, $t)` with `isinstance($x, ($t))`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: false,
    },
    Case {
        operation: "replace",
        exercises: "a template over several lines, indented where each match stands",
        find: "find `print($a)`",
        edit: "replace `print($a)` with `log(\n    $a\n)`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: false,
    },
    Case {
        operation: "replace",
        exercises: "a match inside another's hole, rewritten first",
        find: "find `len($a)`",
        edit: "replace `len($a)` with `size($a)`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: false,
    },
    Case {
        operation: "replace",
        exercises: "matches that overlap outside each other's holes",
        find: "find `$x = $y\n$z = $w`",
        edit: "replace `$x = $y\n$z = $w` with `$x = $y\n$z = $w`",
        languages: &["Python"],
        outcome: Outcome::Refused("this match overlaps"),
        parsed_only: true,
        deletes: false,
    },
    Case {
        operation: "replace",
        exercises: "a comment no capture holds",
        find: "find `if __name__ == \"__main__\":\n    $body...`",
        edit: "replace `if __name__ == \"__main__\":\n    $body...` with `$body...`",
        languages: &["Python"],
        outcome: Outcome::Refused("in no capture the rewrite writes back"),
        parsed_only: true,
        deletes: false,
    },
    // A rewrite with no code takes the separator or the line a match leaves behind, so it may
    // change what lies next to each match, and nothing else.
    Case {
        operation: "delete",
        exercises: "statements alone on their lines, the comments inside them included",
        find: "find `__all__ = $x`",
        edit: "delete `__all__ = $x`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: true,
    },
    Case {
        operation: "delete",
        exercises: "list items and arguments, each taking one comma",
        find: "find `\"__all__\"`",
        edit: "delete `\"__all__\"`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: true,
    },
    Case {
        operation: "delete",
        exercises: "a block left empty",
        find: "find `pass`",
        edit: "delete `pass`",
        languages: &["Python"],
        outcome: Outcome::Refused("unparseable"),
        parsed_only: true,
        deletes: true,
    },
    Case {
        operation: "delete",
        exercises: "a call used as a value, which would leave what holds it without it",
        find: "find `print($a)`",
        edit: "delete `print($a)`",
        languages: &["Python"],
        outcome: Outcome::Refused("would mean something else"),
        parsed_only: true,
        deletes: true,
    },
    Case {
        operation: "replace",
        exercises: "a rewrite with no code, as delete, leaving what holds a match without it",
        find: "find `isinstance($x, $t)`",
        edit: "replace `isinstance($x, $t)` with ``",
        languages: &["Python"],
        outcome: Outcome::Refused("would mean something else"),
        parsed_only: true,
        deletes: true,
    },
    Case {
        operation: "replace",
        exercises: "a rewrite that is only a comment, in place of a statement",
        find: "find `from __future__ import annotations`",
        edit: "replace `from __future__ import annotations` with `# from __future__ import annotations`",
        languages: &["Python"],
        outcome: Outcome::Edits,
        parsed_only: true,
        deletes: true,
    },
    Case {
        operation: "replace",
        exercises: "a rewrite that is only a comment, with code after it on its line",
        find: "find `pass`",
        edit: "replace `pass` with `# pass`",
        languages: &["Python"],
        outcome: Outcome::Refused("would swallow that code"),
        parsed_only: true,
        deletes: true,
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

/// Structural rewrites that must change nothing but spacing the template spells: each pattern
/// written back as itself.  Every token comes back, comments included, and inside brackets every
/// line break and the indentation after it, so any other change, or any refusal, is a fault in
/// what the rewrite derives.  Spacing within a line round the tokens a template writes is the
/// template's, so `print (x)` may come back `print(x)`, `p=x` as `p = x`, and `f(a,b)` as
/// `f(a, b)`.
const IDENTITIES: &[&str] = &[
    "print($a)",
    "isinstance($x, $t)",
    "self.assertEqual($args...)",
    "{$xs...}",
    "[$xs...]",
    "$x = $y",
    "return $x",
    "$f($args...)",
];

/// Renames there and back: a pattern, the name in it renamed, the new name, and the pattern
/// renamed.  The way there must keep every file's meaning, the tree `CPython` reads differing only
/// by the name, and the way back must restore every file but for spacing the template spells.
const ROUND_TRIPS: &[(&str, &str, &str, &str)] = &[
    (
        "print($a)",
        "print",
        "quaff_round_trip",
        "quaff_round_trip($a)",
    ),
    (
        "self.assertEqual($args...)",
        "assertEqual",
        "quaff_round_trip",
        "self.quaff_round_trip($args...)",
    ),
];

/// Reads, from standard input, lines of a file's path before an edit and after it, a tab
/// between, and prints each pair whose trees differ once `argv[2]` is read as `argv[1]`, a tab,
/// and how.  The tree is `CPython`'s own, so this judges meaning without quaff's parser.
const SAME_TREE: &str = r#"
import ast, sys
was, now = sys.argv[1], sys.argv[2]
class Back(ast.NodeTransformer):
    def visit_Name(self, node):
        if node.id == now:
            node.id = was
        return node
    def visit_Attribute(self, node):
        self.generic_visit(node)
        if node.attr == now:
            node.attr = was
        return node
for line in sys.stdin.read().splitlines():
    before, after = line.split("\t")
    with open(before, "rb") as f:
        old = ast.dump(ast.parse(f.read()))
    with open(after, "rb") as f:
        new = ast.dump(Back().visit(ast.parse(f.read())))
    if old != new:
        print(f"{after}\tits tree differs from the file's before the edit")
"#;

/// Reads pairs of files as [`SAME_TREE`] does, and prints each pair that differs by more than
/// spacing a template spells, a tab, and where.  The two must hold the same tokens, as `CPython`'s
/// tokenizer reads them, comments and the line breaks inside brackets included; and inside
/// brackets, where layout is the file's, the same line breaks and the same indentation after
/// each.  Spacing within a line is the template's round the tokens it writes, even inside a
/// bracket, as in `f (x)` written in an argument list.
/// A file with a lone carriage return is compared by its tokens alone, since where its lines
/// start is not worth deciding here.
const SAME_LAYOUT: &str = r#"
import io, re, sys, tokenize
def read(path):
    with open(path, "rb") as f:
        data = f.read()
    encoding, _ = tokenize.detect_encoding(io.BytesIO(data).readline)
    text = data.decode(encoding)
    tokens = list(tokenize.generate_tokens(io.StringIO(text).readline))
    starts, at = [], 0
    for line in re.findall(r"[^\r\n]*(?:\r\n|\r|\n)|[^\r\n]+$", text):
        starts.append(at)
        at += len(line)
    def offset(position):
        row, column = position
        return starts[row - 1] + column if row <= len(starts) else len(text)
    gaps, depth, last, after_break = [], 0, 0, False
    for token in tokens:
        gap = text[last:offset(token.start)]
        # A line break inside brackets is a token of its own, so its layout is the indentation
        # of the line after it: spacing within a line is the template's.
        gaps.append(gap if depth > 0 and after_break else None)
        after_break = token.type == tokenize.NL
        last = offset(token.end)
        if token.type == tokenize.OP and token.string in "([{":
            depth += 1
        elif token.type == tokenize.OP and token.string in ")]}":
            depth -= 1
    lone = re.search(r"\r(?!\n)", text) is not None
    return [(t.type, t.string) for t in tokens], gaps, lone, tokens
for line in sys.stdin.read().splitlines():
    before, after = line.split("\t")
    try:
        old, old_gaps, old_lone, _ = read(before)
        new, new_gaps, new_lone, tokens = read(after)
    except (tokenize.TokenError, SyntaxError) as err:
        print(f"{after}\tit could not be tokenized: {err}")
        continue
    if old != new:
        at = next((i for i, (a, b) in enumerate(zip(old, new)) if a != b), min(len(old), len(new)))
        row = tokens[min(at, len(tokens) - 1)].start[0]
        print(f"{after}\tits tokens differ from the file's before the edit, at line {row}")
    elif not (old_lone or new_lone) and old_gaps != new_gaps:
        at = next(i for i, (a, b) in enumerate(zip(old_gaps, new_gaps)) if a != b)
        row = tokens[at].start[0]
        print(f"{after}\tits layout inside brackets differs from the file's before the edit, at line {row}")
"#;

/// The pairs of files, `(before, after)`, whose trees differ with `now` read as `was`, by
/// `python`, with why.
fn different_trees(
    python: &Path,
    was: &str,
    now: &str,
    pairs: &[(PathBuf, PathBuf)],
) -> Result<Vec<String>, String> {
    compared(python, SAME_TREE, &[was, now], pairs)
}

/// The pairs of files, `(before, after)`, that differ by more than spacing a template spells,
/// by `python`, with why.
fn different_layout(python: &Path, pairs: &[(PathBuf, PathBuf)]) -> Result<Vec<String>, String> {
    compared(python, SAME_LAYOUT, &[], pairs)
}

/// What `script`, run by `python` with `args`, prints of `pairs`, given one to a line on its
/// standard input: a line for each pair it faults.
fn compared(
    python: &Path,
    script: &str,
    args: &[&str],
    pairs: &[(PathBuf, PathBuf)],
) -> Result<Vec<String>, String> {
    use std::fmt::Write as _;
    use std::io::Write as _;
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = std::process::Command::new(python)
        .args(["-c", script])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("running {}: {err}", python.display()))?;
    let mut input = String::new();
    for (before, after) in pairs {
        let _ = writeln!(input, "{}\t{}", before.display(), after.display());
    }
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(input.as_bytes())
        .map_err(|err| format!("writing to {}: {err}", python.display()))?;
    let output = child
        .wait_with_output()
        .map_err(|err| format!("waiting for {}: {err}", python.display()))?;
    if !output.status.success() {
        return Err(format!(
            "comparing files with {} exited with {}: {}",
            python.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// What `script` did to a fresh copy of `files`: the run, the files after it, and the copy.
fn applied(files: &Files, script: &str) -> (Run, Files, Scratch) {
    let project = Scratch::new(files);
    let run = Binary.run(&project.dir, script);
    let after = project.files();
    (run, after, project)
}

/// The identities, renames there and back, and tree comparisons over `files`, each a Python
/// file `python` parses, with every failure said.
fn fidelity(files: &Files, python: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    let originals = Scratch::new(files);
    let pairs = |changed: &[String], project: &Scratch| -> Vec<(PathBuf, PathBuf)> {
        changed
            .iter()
            .map(|path| (originals.dir.join(path), project.dir.join(path)))
            .collect()
    };
    for pattern in IDENTITIES {
        let (found, _, _) = applied(files, &format!("find `{pattern}`"));
        if found.code != Some(0) {
            failures.push(format!(
                "identity `{pattern}`: the find matched nothing or failed, so nothing was checked: \
                 {}",
                found.stderr.trim()
            ));
            continue;
        }
        let (run, after, project) = applied(
            files,
            &format!("replace `{pattern}` with `{pattern}` expect any"),
        );
        if run.code != Some(0) {
            failures.push(format!(
                "identity `{pattern}` exited {:?}: {}",
                run.code,
                run.stderr.trim()
            ));
        }
        match different_layout(python, &pairs(&differing(files, &after), &project)) {
            Ok(different) => failures.extend(
                different
                    .into_iter()
                    .map(|why| format!("identity `{pattern}`: {why}")),
            ),
            Err(why) => failures.push(why),
        }
    }
    for (pattern, was, name, renamed) in ROUND_TRIPS {
        let (there, after, project) = applied(
            files,
            &format!("replace `{pattern}` with `{renamed}` expect any"),
        );
        let changed = differing(files, &after);
        if there.code != Some(0) || changed.is_empty() {
            failures.push(format!(
                "renaming `{pattern}` to `{renamed}` exited {:?} changing {} files: {}",
                there.code,
                changed.len(),
                there.stderr.trim()
            ));
            continue;
        }
        match different_trees(python, was, name, &pairs(&changed, &project)) {
            Ok(different) => failures.extend(
                different
                    .into_iter()
                    .map(|why| format!("renaming `{pattern}` to `{renamed}`: {why}")),
            ),
            Err(why) => failures.push(why),
        }
        let back = Binary.run(
            &project.dir,
            &format!("replace `{renamed}` with `{pattern}` expect any"),
        );
        let restored = project.files();
        if back.code != Some(0) {
            failures.push(format!(
                "renaming `{renamed}` back to `{pattern}` exited {:?}: {}",
                back.code,
                back.stderr.trim()
            ));
        }
        match different_layout(python, &pairs(&differing(files, &restored), &project)) {
            Ok(different) => failures.extend(
                different
                    .into_iter()
                    .map(|why| format!("renaming `{pattern}` to `{renamed}` and back: {why}")),
            ),
            Err(why) => failures.push(why),
        }
    }
    failures
}

#[test]
#[ignore = "needs the fetched real-world corpus and the oracle's Python; the Real-world corpus CI job runs it"]
fn structural_rewrites_keep_what_they_should_over_the_corpus() {
    let python =
        PathBuf::from(std::env::var_os(PYTHON_ORACLE).unwrap_or_else(|| {
            panic!("{PYTHON_ORACLE} is not set; set it to the oracle's Python")
        }));
    let oracle = Oracle {
        program: python.clone(),
        args: vec![workspace_root().join("scripts/oracle-python").into()],
    };
    let python_files: Files = corpus()
        .into_iter()
        .filter(|(path, _)| is_python(path))
        .collect();
    let judged = Scratch::new(&python_files);
    let refused = oracle
        .refused(&judged.dir, &python_files.keys().collect::<Vec<_>>())
        .unwrap_or_else(|why| panic!("{why}"));
    let parsed: Files = python_files
        .into_iter()
        .filter(|(path, _)| !refused.contains_key(path))
        .collect();
    let failures = fidelity(&parsed, &python);
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_tree_comparison_tells_a_rename_from_a_change_of_meaning() {
    let before = Scratch::new(&Files::from([(
        "a.py".to_owned(),
        b"print(x + 1)\nself.assertEqual(a, b)\n".to_vec(),
    )]));
    let renamed = Scratch::new(&Files::from([(
        "a.py".to_owned(),
        b"quaff_round_trip(x + 1)\nself.assertEqual(a, b)\n".to_vec(),
    )]));
    let changed = Scratch::new(&Files::from([(
        "a.py".to_owned(),
        b"quaff_round_trip(x) + 1\nself.assertEqual(a, b)\n".to_vec(),
    )]));
    let python = Path::new("python3");
    let pair = |after: &Scratch| vec![(before.dir.join("a.py"), after.dir.join("a.py"))];
    assert_eq!(
        different_trees(python, "print", "quaff_round_trip", &pair(&renamed)),
        Ok(vec![])
    );
    let different =
        different_trees(python, "print", "quaff_round_trip", &pair(&changed)).expect("compared");
    assert_eq!(different.len(), 1, "{different:?}");
}

#[test]
fn the_layout_comparison_allows_only_spacing_within_a_line() {
    let file = |text: &str| {
        Scratch::new(&Files::from([(
            "a.py".to_owned(),
            text.as_bytes().to_vec(),
        )]))
    };
    let before = file("print (x,  # why\n      y)\np=1\nq = \\\n    2\n");
    let spaced = file("print(x,  # why\n      y)\np = 1\nq = 2\n");
    let within = file("print (x , # why\n      y )\np=1\nq = \\\n    2\n");
    let joined = file("print (x,  # why\n  y)\np=1\nq = \\\n    2\n");
    let dropped = file("print (x,\n      y)\np=1\nq = \\\n    2\n");
    let python = Path::new("python3");
    let pair = |after: &Scratch| vec![(before.dir.join("a.py"), after.dir.join("a.py"))];
    assert_eq!(different_layout(python, &pair(&spaced)), Ok(vec![]));
    // Inside a bracket too, spacing within a line is the template's.
    assert_eq!(different_layout(python, &pair(&within)), Ok(vec![]));
    let inside = different_layout(python, &pair(&joined)).expect("compared");
    assert!(
        matches!(inside.as_slice(), [why] if why.contains("layout inside brackets differs")),
        "{inside:?}"
    );
    let comment = different_layout(python, &pair(&dropped)).expect("compared");
    assert!(
        matches!(comment.as_slice(), [why] if why.contains("tokens differ")),
        "{comment:?}"
    );
}
