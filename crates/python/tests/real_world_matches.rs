//! Every structural match quaff finds across real Python, held to `CPython`'s own count.
//!
//! `scripts/count-structural` counts, with `CPython`'s `ast` and a predicate written for each
//! pattern, the places a set of patterns should match across the real-world corpus, and writes
//! the counts beside the corpus, file by file.  This test compiles the same patterns, matches
//! them over the same files, and requires the same count for each pattern in each file, so a
//! false match in one file cannot hide behind a missed one in another.  The corpus is fetched,
//! not checked in, so that test is ignored by default and the **Real-world corpus** CI job runs
//! it with `--ignored`, after the script; the reading of the counts is tested in every run.

mod fetched;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use fetched::{fetched, python_files, sources};
use quaffed_encoding::python::Source;
use quaffed_python::{compile_pattern, parse_module};
use quaffed_syntax::find;

/// One pattern the script counted: its name, the pattern and `CPython`'s total.
#[derive(Debug, PartialEq, Eq)]
struct Pattern {
    name: String,
    text: String,
    total: usize,
}

/// What the script counted for one source: how many files, each pattern, and each file's count
/// of each pattern in order, for the files with any match.
#[derive(Debug, PartialEq, Eq)]
struct Counted {
    files: usize,
    patterns: Vec<Pattern>,
    per_file: BTreeMap<String, Vec<usize>>,
}

/// `field` with `\\`, `\t` and `\n` read back as a backslash, a tab and a line break.
fn unescaped(field: &str) -> Result<String, String> {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            other => return Err(format!("{field:?} has a backslash before {other:?}")),
        }
    }
    Ok(out)
}

/// Reads `structural.txt`'s text as `scripts/count-structural` writes it: a header counting the
/// files, a `pattern` line per pattern with its total, name and pattern, and a `file` line per
/// file with any match, with its path and a count per pattern, fields separated by tabs.
fn read_counted(text: &str) -> Result<Counted, String> {
    let mut lines = text.lines();
    let files = lines
        .next()
        .and_then(|header| header.strip_prefix("# "))
        .and_then(|header| header.split(' ').next())
        .and_then(|count| count.parse().ok())
        .ok_or("it does not open with the count of files")?;
    let number = |field: &str, line: &str| -> Result<usize, String> {
        field
            .parse()
            .map_err(|err| format!("{line:?}: {field:?}: {err}"))
    };
    let mut patterns = Vec::new();
    let mut per_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for line in lines.filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["pattern", total, name, pattern] => patterns.push(Pattern {
                name: unescaped(name)?,
                text: unescaped(pattern)?,
                total: number(total, line)?,
            }),
            ["file", path, counts @ ..] if counts.len() == patterns.len() => {
                let counts = counts
                    .iter()
                    .map(|count| number(count, line))
                    .collect::<Result<_, _>>()?;
                per_file.insert(unescaped(path)?, counts);
            }
            ["file", ..] => {
                return Err(format!("{line:?} has not one count per pattern"));
            }
            _ => return Err(format!("{line:?} is neither a pattern nor a file line")),
        }
    }
    if patterns.is_empty() {
        return Err("it counts no pattern".to_owned());
    }
    for (i, pattern) in patterns.iter().enumerate() {
        let sum: usize = per_file.values().map(|counts| counts[i]).sum();
        if sum != pattern.total {
            return Err(format!(
                "{}: its files' counts add up to {sum}, not its total {}",
                pattern.name, pattern.total
            ));
        }
    }
    Ok(Counted {
        files,
        patterns,
        per_file,
    })
}

fn counted(dir: &Path) -> Counted {
    let list = dir.join("structural.txt");
    let text = fs::read_to_string(&list).unwrap_or_else(|err| {
        panic!(
            "reading {}: {err}.  Run scripts/count-structural",
            list.display()
        )
    });
    read_counted(&text).unwrap_or_else(|err| panic!("{}: {err}", list.display()))
}

#[test]
#[ignore = "needs the real-world corpus from scripts/fetch-corpus and its counts from \
            scripts/count-structural; the Real-world corpus CI job runs it with --ignored"]
fn every_structural_match_across_real_python_is_found_as_cpython_counts_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/corpus");
    let mut failures = Vec::new();
    for dir in sources(&root) {
        let source = fetched(&dir).unwrap_or_else(|err| panic!("{err}"));
        let counted = counted(&dir);
        let all = python_files(&source.dir);
        assert_eq!(
            all.len(),
            source.read,
            "{}: found {} .py files, but the oracle read {}",
            source.dir.display(),
            all.len(),
            source.read
        );
        let accepted: Vec<String> = all
            .into_iter()
            .filter(|relative| !source.refused.contains_key(relative))
            .collect();
        assert_eq!(
            accepted.len(),
            counted.files,
            "{}: the counts are of {} files, but the oracle accepts {}: run \
             scripts/count-structural again",
            dir.display(),
            counted.files,
            accepted.len()
        );
        let compiled: Vec<_> = counted
            .patterns
            .iter()
            .map(|p| {
                compile_pattern(&p.text)
                    .unwrap_or_else(|err| panic!("{} ({:?}): {err}", p.name, p.text))
            })
            .collect();
        let none = vec![0; compiled.len()];
        let mut totals = vec![0; compiled.len()];
        for relative in &accepted {
            let path = dir.join(relative);
            let bytes = fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let text = Source::decode(&bytes)
                .unwrap_or_else(|err| panic!("{relative}: decoding: {err}"))
                .text;
            let arena =
                parse_module(&text).unwrap_or_else(|err| panic!("{relative}: parsing: {err}"));
            let found: Vec<usize> = compiled.iter().map(|p| find(p, &arena).len()).collect();
            let expected = counted.per_file.get(relative).unwrap_or(&none);
            for (i, (found, expected)) in found.iter().zip(expected).enumerate() {
                totals[i] += found;
                if found != expected {
                    failures.push(format!(
                        "{relative}: {} ({:?}): quaff finds {found}, CPython counts {expected}",
                        counted.patterns[i].name, counted.patterns[i].text
                    ));
                }
            }
        }
        for (pattern, total) in counted.patterns.iter().zip(&totals) {
            println!(
                "{}: {}: quaff {total}, CPython {}",
                dir.display(),
                pattern.name,
                pattern.total
            );
        }
    }
    assert!(
        failures.is_empty(),
        "{} files differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

const COUNTED: &str = "\
# 2 files; counted by CPython 3.14.8's ast
# Generated by scripts/count-structural: pattern lines, then file lines.
pattern\t3\tone\tf($a)
pattern\t1\ttwo, over lines\tx = 1\\n$...\\nreturn x\\t\\\\
file\tLib/a.py\t2\t1
file\tLib/b.py\t1\t0
";

#[test]
fn the_counts_are_read_as_the_script_writes_them() {
    let counted = read_counted(COUNTED).unwrap();
    assert_eq!(counted.files, 2);
    assert_eq!(
        counted.patterns[1],
        Pattern {
            name: "two, over lines".into(),
            text: "x = 1\n$...\nreturn x\t\\".into(),
            total: 1,
        }
    );
    assert_eq!(counted.per_file["Lib/a.py"], [2, 1]);
}

#[test]
fn counts_that_cannot_be_read_or_do_not_add_up_are_refused_saying_why() {
    for (text, why) in [
        ("", "count of files"),
        ("pattern\t1\tone\tf($a)\n", "count of files"),
        ("# 1 files\n", "counts no pattern"),
        ("# 1 files\npattern\tmany\tone\tf($a)\n", "\"many\""),
        (
            "# 1 files\npattern\t1\tone\n",
            "neither a pattern nor a file",
        ),
        (
            "# 1 files\npattern\t1\tone\tf($a)\nfile\ta.py\t1\t1\n",
            "one count per pattern",
        ),
        (
            "# 1 files\npattern\t2\tone\tf($a)\nfile\ta.py\t1\n",
            "add up to 1, not its total 2",
        ),
        ("# 1 files\npattern\t0\tone\tf(\\q)\n", "backslash before"),
    ] {
        let err = read_counted(text).unwrap_err();
        assert!(err.contains(why), "{text:?}: {err}");
    }
}
