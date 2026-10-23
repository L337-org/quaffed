//! Every structural match quaff finds across real Python, held to `CPython`'s own count.
//!
//! `scripts/count-structural` counts, with `CPython`'s `ast` and a predicate written for each
//! pattern, the places a set of patterns should match across the real-world corpus - one
//! pattern for each kind of hole - and writes the counts beside the corpus.  This test compiles
//! the same patterns, matches them over the same files, and requires the same count for each.
//! The corpus is fetched, not checked in, so the test is ignored by default and the
//! **Real-world corpus** CI job runs it with `--ignored`, after the script.

mod fetched;

use std::fs;
use std::path::Path;

use fetched::{fetched, python_files, sources};
use quaffed_encoding::python::Source;
use quaffed_python::{compile_pattern, parse_module};
use quaffed_syntax::find;

/// One pattern the script counted: its name, the pattern and `CPython`'s count.
struct Counted {
    name: String,
    pattern: String,
    count: usize,
}

/// Reads `structural.txt` in `dir`, as `scripts/count-structural` writes it: a header counting
/// the files, then per pattern a count, a tab, a name, a tab and the pattern with each line break
/// written as `\n`.
fn counted(dir: &Path) -> Result<(usize, Vec<Counted>), String> {
    let list = dir.join("structural.txt");
    let text = fs::read_to_string(&list).map_err(|err| {
        format!(
            "reading {}: {err}.  Run scripts/count-structural",
            list.display()
        )
    })?;
    let mut lines = text.lines();
    let files = lines
        .next()
        .and_then(|header| header.strip_prefix("# "))
        .and_then(|header| header.split(' ').next())
        .and_then(|count| count.parse().ok())
        .ok_or_else(|| format!("{} does not open with the count of files", list.display()))?;
    let patterns = lines
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let (Some(count), Some(name), Some(pattern)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!(
                    "{}: {line:?} has fewer than three fields",
                    list.display()
                ));
            };
            Ok(Counted {
                name: name.to_owned(),
                pattern: pattern.replace("\\n", "\n"),
                count: count
                    .parse()
                    .map_err(|err| format!("{}: {line:?}: {err}", list.display()))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((files, patterns))
}

#[test]
#[ignore = "needs the real-world corpus from scripts/fetch-corpus and its counts from \
            scripts/count-structural; the Real-world corpus CI job runs it with --ignored"]
fn every_structural_match_across_real_python_is_found_as_cpython_counts_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/corpus");
    let mut failures = Vec::new();
    for dir in sources(&root) {
        let source = fetched(&dir).unwrap_or_else(|err| panic!("{err}"));
        let (files, patterns) = counted(&dir).unwrap_or_else(|err| panic!("{err}"));
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
            files,
            "{}: the counts are of {files} files, but the oracle accepts {}: run \
             scripts/count-structural again",
            dir.display(),
            accepted.len()
        );
        // A pattern list that lost its entries would pass having matched nothing.
        assert!(
            !patterns.is_empty(),
            "{}: no pattern was counted",
            dir.display()
        );
        let compiled: Vec<_> = patterns
            .iter()
            .map(|p| {
                compile_pattern(&p.pattern)
                    .unwrap_or_else(|err| panic!("{} ({:?}): {err}", p.name, p.pattern))
            })
            .collect();
        let mut found = vec![0; patterns.len()];
        for relative in &accepted {
            let path = dir.join(relative);
            let bytes = fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let text = Source::decode(&bytes)
                .unwrap_or_else(|err| panic!("{relative}: decoding: {err}"))
                .text;
            let arena =
                parse_module(&text).unwrap_or_else(|err| panic!("{relative}: parsing: {err}"));
            for (count, pattern) in found.iter_mut().zip(&compiled) {
                *count += find(pattern, &arena).len();
            }
        }
        for (expected, found) in patterns.iter().zip(&found) {
            println!(
                "{}: {}: quaff {found}, CPython {}",
                dir.display(),
                expected.name,
                expected.count
            );
            if *found != expected.count {
                failures.push(format!(
                    "{}: {} ({:?}): quaff finds {found}, CPython counts {}",
                    dir.display(),
                    expected.name,
                    expected.pattern,
                    expected.count
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
