//! The harness's own checks, on small inputs written for them, and the harness as a whole on a
//! small project with the real `quaff`: passing it, and failing each invariant when a run is
//! broken on purpose in one way.

use super::*;

use quaffed_encoding::{Codec, MultiByteCodec};

fn at(line: usize, column: usize) -> Position {
    Position { line, column }
}

fn span(start: (usize, usize), end: (usize, usize)) -> Span {
    Span {
        start: at(start.0, start.1),
        end: at(end.0, end.1),
    }
}

/// A list of one range: a range alone in `vec![]` reads as a mistake for a list of its numbers.
fn one(range: Range<usize>) -> Vec<Range<usize>> {
    vec![range]
}

fn files(entries: &[(&str, &[u8])]) -> Files {
    entries
        .iter()
        .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
        .collect()
}

#[test]
fn a_span_covers_its_first_to_last_character_across_lf_crlf_and_lone_cr() {
    assert_eq!(
        byte_ranges(b"ab\ncd\n", Encoding::Utf8, 0, &[span((1, 2), (2, 1))]),
        Ok(one(1..4))
    );
    // A CRLF is two columns of the line it ends, so a match may end on its LF.
    assert_eq!(
        byte_ranges(b"one\r\ntwo", Encoding::Utf8, 0, &[span((1, 1), (1, 5))]),
        Ok(one(0..5))
    );
    assert_eq!(
        byte_ranges(b"a\rb", Encoding::Utf8, 0, &[span((2, 1), (2, 1))]),
        Ok(one(2..3))
    );
    assert_eq!(
        byte_ranges(b"a\r\rb", Encoding::Utf8, 0, &[span((3, 1), (3, 1))]),
        Ok(one(3..4))
    );
}

#[test]
fn a_column_counts_characters_not_bytes() {
    // A byte-order mark is not a column.
    assert_eq!(
        byte_ranges(
            b"\xef\xbb\xbfab",
            Encoding::Utf8,
            3,
            &[span((1, 2), (1, 2))]
        ),
        Ok(one(4..5))
    );
    assert_eq!(
        byte_ranges(
            "é日x".as_bytes(),
            Encoding::Utf8,
            0,
            &[span((1, 3), (1, 3))]
        ),
        Ok(one(5..6))
    );
    // A byte that does not decode counts as one character.
    assert_eq!(
        byte_ranges(b"\xffab", Encoding::Utf8, 0, &[span((1, 2), (1, 2))]),
        Ok(one(1..2))
    );
    let sjis =
        Encoding::MultiByte(MultiByteCodec::named("shift_jis").expect("a codec quaff reads"));
    assert_eq!(
        byte_ranges(b"\x83\x41x", sjis, 0, &[span((1, 2), (1, 2))]),
        Ok(one(2..3))
    );
    let latin1 = Encoding::SingleByte(Codec::named("latin_1").expect("a codec quaff reads"));
    assert_eq!(
        byte_ranges(b"\xe9x", latin1, 0, &[span((1, 2), (1, 2))]),
        Ok(one(1..2))
    );
    assert_eq!(
        byte_ranges(
            b"\xff\xfea\x00\n\x00b\x00",
            Encoding::Utf16Le,
            2,
            &[span((2, 1), (2, 1))]
        ),
        Ok(one(6..8))
    );
}

#[test]
fn a_span_outside_the_file_or_out_of_order_is_a_fault_of_find() {
    let outside = byte_ranges(b"ab\n", Encoding::Utf8, 0, &[span((3, 1), (3, 1))]);
    assert_eq!(
        outside,
        Err("find reported a match at line 3, column 1, which is not in the file".into())
    );
    let overlapping = byte_ranges(
        b"abc",
        Encoding::Utf8,
        0,
        &[span((1, 1), (1, 2)), span((1, 2), (1, 3))],
    );
    assert!(overlapping.is_err_and(|why| why.contains("overlapping")));
}

#[test]
fn a_spans_lines_run_from_the_start_of_its_first_to_the_end_of_its_last() {
    let text = b"a\nbXb\r\nc\rd";
    assert_eq!(
        line_ranges(text, Encoding::Utf8, 0, &[span((2, 2), (2, 2))]),
        one(2..7)
    );
    assert_eq!(
        line_ranges(text, Encoding::Utf8, 0, &[span((2, 2), (3, 1))]),
        one(2..9)
    );
    // The last line has no ending, and runs to the end of the file.
    assert_eq!(
        line_ranges(text, Encoding::Utf8, 0, &[span((4, 1), (4, 1))]),
        one(9..10)
    );
    // Two matches on one line make one range.
    assert_eq!(
        line_ranges(
            text,
            Encoding::Utf8,
            0,
            &[span((2, 1), (2, 1)), span((2, 3), (2, 3))]
        ),
        one(2..7)
    );
    // A byte-order mark is before line 1, so it is outside a match on it.
    assert_eq!(
        line_ranges(
            b"\xef\xbb\xbfa\n",
            Encoding::Utf8,
            3,
            &[span((1, 1), (1, 1))]
        ),
        one(3..5)
    );
}

#[test]
fn the_positions_find_reports_are_placed_on_exactly_the_matched_bytes() {
    // Checked against the real quaff: what it reports, placed by the harness, must be the bytes
    // the pattern spells in each encoding, whatever the line endings.
    let todo_in = |encoding: &str| -> Vec<u8> {
        match encoding {
            "utf-16-le" => "TODO".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            _ => b"TODO".to_vec(),
        }
    };
    let project = files(&[
        ("lf.txt", b"x\n\xc3\xa9 TODO y\n"),
        ("crlf.txt", b"x\r\n\xc3\xa9 TODO y\r\n"),
        ("cr.txt", b"x\r\xc3\xa9 TODO y\r"),
        ("bom.txt", b"\xef\xbb\xbfx\n\xc3\xa9 TODO\n"),
        ("u16.txt", b"\xff\xfex\x00\n\x00T\x00O\x00D\x00O\x00"),
        ("sjis.py", b"# coding: shift_jis\nx = '\x83\x41' # TODO\n"),
        ("latin.py", b"# coding: latin-1\nx = '\xe9' # TODO\n"),
    ]);
    let scratch = Scratch::new(&project);
    let run = Binary.run(&scratch.dir, r#"find "TODO" expect 7"#);
    assert_eq!(run.code, Some(0), "{run:?}");
    let paths: BTreeSet<String> = project.keys().cloned().collect();
    let matches = parse_matches(&run.stdout, &paths).expect("find's lines");
    assert_eq!(matches.len(), 7, "{matches:?}");
    for (path, spans) in &matches {
        let bytes = &project[path];
        let (encoding, mark) = encoding_of(path, bytes).expect("an encoding the harness decides");
        let ranges = byte_ranges(bytes, encoding, mark, spans).expect("placed");
        let expected = todo_in(if path == "u16.txt" { "utf-16-le" } else { "" });
        for range in ranges {
            assert_eq!(bytes[range].to_vec(), expected, "{path}");
        }
    }
    // A match across a line ending covers the ending as the file has it.
    let run = Binary.run(&scratch.dir, r#"find "x" $^ "" expect 4"#);
    let matches = parse_matches(&run.stdout, &paths).expect("find's lines");
    for (path, ending) in [
        ("lf.txt", &b"x\n"[..]),
        ("crlf.txt", b"x\r\n"),
        ("cr.txt", b"x\r"),
        ("bom.txt", b"x\n"),
    ] {
        let bytes = &project[path];
        let (encoding, mark) = encoding_of(path, bytes).expect("an encoding");
        let ranges = byte_ranges(bytes, encoding, mark, &matches[path]).expect("placed");
        assert_eq!(
            ranges.iter().map(|r| &bytes[r.clone()]).collect::<Vec<_>>(),
            vec![ending],
            "{path}"
        );
    }
}

#[test]
fn the_encoding_is_the_mark_then_the_coding_line_then_utf8_else_undecided() {
    assert_eq!(
        encoding_of("a.txt", b"\xff\xfea\x00"),
        Some((Encoding::Utf16Le, 2))
    );
    assert_eq!(
        encoding_of("a.txt", "é".as_bytes()),
        Some((Encoding::Utf8, 0))
    );
    assert_eq!(encoding_of("a.txt", b"\xe9"), None);
    let latin1 = Encoding::SingleByte(Codec::named("latin_1").expect("a codec"));
    assert_eq!(
        encoding_of("a.py", b"# coding: latin-1\n\xe9\n"),
        Some((latin1, 0))
    );
    // A coding line means nothing outside Python.
    assert_eq!(encoding_of("a.txt", b"# coding: latin-1\n\xe9\n"), None);
}

#[test]
fn find_is_read_only_as_matches_in_the_projects_files() {
    let paths: BTreeSet<String> = ["a:b.txt".to_owned(), "c.txt".to_owned()].into();
    let read = parse_matches(
        "a:b.txt:1:2-1:4: x:1:1-1:1: y\nc.txt:3:1-4:2: p q\n",
        &paths,
    );
    assert_eq!(
        read,
        Ok([
            ("a:b.txt".to_owned(), vec![span((1, 2), (1, 4))]),
            ("c.txt".to_owned(), vec![span((3, 1), (4, 2))]),
        ]
        .into())
    );
    assert_eq!(parse_matches("", &paths), Ok(BTreeMap::new()));
    assert!(parse_matches("d.txt:1:1-1:1: x\n", &paths).is_err_and(|why| why.contains("d.txt")));
    assert!(parse_matches("c.txt:1:1: x\n", &paths).is_err());
}

#[test]
fn the_text_outside_the_matches_must_survive_in_order() {
    let original = b"aXbXc";
    let inside = [1..2, 3..4];
    assert_eq!(outside_survives(original, &inside, b"aYYbZc"), Ok(()));
    // A replacement may hold the text around it.
    assert_eq!(outside_survives(original, &inside, b"abbbc"), Ok(()));
    let failed =
        |edited: &[u8]| outside_survives(original, &inside, edited).expect_err("a change outside");
    assert!(failed(b"zYbYc").contains("before the first match"));
    assert!(failed(b"aYbYz").contains("after the last match"));
    assert!(failed(b"aYzYc").contains("between two matches"));
    assert!(failed(b"ac").contains("between two matches"));
    // Shorter than the text before and after the matches together.
    assert!(failed(b"a").contains("after the last match"));
}

#[test]
fn matches_at_the_ends_adjacent_or_absent_leave_nothing_to_spare() {
    assert_eq!(outside_survives(b"XabcX", &[0..1, 4..5], b"YYabcZ"), Ok(()));
    assert_eq!(outside_survives(b"aXXc", &[1..2, 2..3], b"aYZc"), Ok(()));
    assert_eq!(outside_survives(b"", &[], b""), Ok(()));
    assert_eq!(outside_survives(b"ab", &[], b"ab"), Ok(()));
    assert!(outside_survives(b"ab", &[], b"aB").is_err());
}

#[test]
fn a_change_within_a_matched_line_is_seen_by_bytes_but_not_by_lines() {
    let before = files(&[("a.txt", b"one the two\nthree\n")]);
    let matches: BTreeMap<String, Vec<Span>> =
        [("a.txt".to_owned(), vec![span((1, 5), (1, 7))])].into();
    let edited = files(&[("a.txt", b"ONE THE two\nthree\n")]);
    let found = minimal_diff(&before, &matches, &edited);
    let invariants: Vec<Invariant> = found.iter().map(|(_, (invariant, _))| *invariant).collect();
    assert_eq!(invariants, vec![Invariant::MinimalDiff], "{found:?}");
    let edited = files(&[("a.txt", b"one THE two\nTHREE\n")]);
    let found = minimal_diff(&before, &matches, &edited);
    let invariants: Vec<Invariant> = found.iter().map(|(_, (invariant, _))| *invariant).collect();
    assert_eq!(
        invariants,
        vec![Invariant::MinimalDiff, Invariant::MinimalDiffLines],
        "{found:?}"
    );
}

#[test]
fn a_file_created_deleted_or_changed_without_a_match_breaks_minimal_diff() {
    let before = files(&[("a.txt", b"a"), ("b.txt", b"b")]);
    let edited = files(&[("a.txt", b"A"), ("c.txt", b"c")]);
    let found = minimal_diff(&before, &BTreeMap::new(), &edited);
    let said: Vec<(&str, &str)> = found
        .iter()
        .map(|(path, (_, what))| (path.as_str(), what.as_str()))
        .collect();
    assert_eq!(
        said,
        vec![
            ("a.txt", "it changed, but find reported no match in it"),
            ("b.txt", "the edit deleted it"),
            ("c.txt", "the edit created it"),
        ]
    );
}

#[test]
fn a_failure_names_the_operation_the_language_the_invariant_and_the_file() {
    let failure = Failure {
        case: "replace string (a common word)".into(),
        language: "Python".into(),
        invariant: Invariant::MinimalDiff,
        reapplying: true,
        file: Some("a.py".into()),
        what: "the text after the last match changed".into(),
    };
    assert_eq!(
        failure.to_string(),
        "replace string (a common word), Python: minimal diff on re-applying the edit to its own \
         output (idempotency), \"a.py\": the text after the last match changed"
    );
}

/// The running `python3` as the oracle: `scripts/oracle-python` with a manifest naming its own
/// version, so the script's version check passes, as `check_synthetic.rs` does.
struct Python {
    dir: PathBuf,
}

impl Python {
    fn new() -> Python {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "quaffed-edit-invariants-oracle-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("creating the manifest's directory");
        let version = Command::new("python3")
            .args([
                "-c",
                "import sys; print('.'.join(map(str, sys.version_info[:3])))",
            ])
            .output()
            .unwrap_or_else(|err| panic!("running python3, which these tests need: {err}"));
        let version = String::from_utf8(version.stdout).expect("a version");
        fs::write(
            dir.join("manifest.toml"),
            format!("[cpython]\noracle = \"{}\"\n", version.trim()),
        )
        .expect("writing the manifest");
        Python { dir }
    }

    fn oracle(&self) -> Oracle {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/oracle-python");
        Oracle {
            program: "python3".into(),
            args: vec![
                script.into(),
                "--manifest".into(),
                self.dir.join("manifest.toml").into(),
            ],
        }
    }
}

impl Drop for Python {
    fn drop(&mut self) {
        // Not checked, as for Scratch.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn languages(python: &Python) -> Vec<Language> {
    vec![
        Language {
            name: "Python",
            claims: is_python,
            judge: Judge::Oracle(python.oracle()),
        },
        Language {
            name: "text",
            claims: |path| !is_python(path),
            judge: Judge::NotApplicable,
        },
    ]
}

/// A project of each language, with matches of `the` in some files and not others, on lines
/// with other text, and lines with no match.
fn project() -> Files {
    files(&[
        (
            "a.py",
            b"def f():\n    return 'the start'\n# the end\nx = 1\n",
        ),
        ("b.txt", b"the start\nnothing\nthe end.\n"),
        ("c.txt", b"untouched\n"),
    ])
}

const WORD: Case = Case {
    operation: "replace string",
    exercises: "a word",
    find: r#"find "the""#,
    edit: r#"replace string "the" with "THE""#,
    languages: &["Python", "text"],
    outcome: Outcome::Edits,
};

/// The real `quaff`, with what it did changed afterwards by `breaks`, given the project, the
/// script and the run.
struct Broken<F: Fn(&Path, &str, &mut Run) + Sync>(F);

impl<F: Fn(&Path, &str, &mut Run) + Sync> Quaff for Broken<F> {
    fn run(&self, project: &Path, script: &str) -> Run {
        let mut run = Binary.run(project, script);
        (self.0)(project, script, &mut run);
        run
    }
}

fn is_edit(script: &str) -> bool {
    script.starts_with("replace")
}

/// Appends `bytes` to the project's file at `path`, if this language's project has it.
fn append(project: &Path, path: &str, bytes: &[u8]) {
    let path = project.join(path);
    if !path.exists() {
        return;
    }
    let mut content = fs::read(&path).expect("reading a project file");
    content.extend_from_slice(bytes);
    fs::write(&path, content).expect("writing a project file");
}

/// The invariants `failures` breaks, and whether on re-applying, in order and without repeats.
fn broken(failures: &[Failure]) -> Vec<(Invariant, bool)> {
    let mut found: Vec<(Invariant, bool)> = Vec::new();
    for failure in failures {
        let key = (failure.invariant, failure.reapplying);
        if !found.contains(&key) {
            found.push(key);
        }
    }
    found
}

#[test]
fn the_real_quaff_keeps_every_invariant_on_a_small_project() {
    let python = Python::new();
    let refused = Case {
        exercises: "an edit breaking files that parsed",
        find: r#"find "def ""#,
        edit: r#"replace string "def " with "def def ""#,
        languages: &["Python"],
        outcome: Outcome::Refused,
        ..WORD
    };
    let own_match = Case {
        exercises: "a replacement holding its own match",
        edit: r#"replace string "the" with "thethe""#,
        ..WORD
    };
    let failures = check(
        &[WORD, own_match, refused],
        &languages(&python),
        &project(),
        &Binary,
    );
    assert_eq!(failures, vec![]);
}

#[test]
fn writing_outside_a_match_breaks_minimal_diff_by_bytes_and_by_lines() {
    let python = Python::new();
    // On "nothing", the unmatched line between two matched ones.  Text added after the last
    // matched line could be part of what replaced it, as far as whole lines can tell, so only
    // the check by bytes would see that.
    let quaff = Broken(|project: &Path, script: &str, run: &mut Run| {
        let path = project.join("b.txt");
        if is_edit(script) && run.code == Some(0) && path.exists() {
            let content = fs::read_to_string(&path).expect("reading b.txt");
            fs::write(&path, content.replace("nothing", "something")).expect("writing b.txt");
        }
    });
    let failures = check(&[WORD], &languages(&python), &project(), &quaff);
    assert!(
        broken(&failures).starts_with(&[
            (Invariant::MinimalDiff, false),
            (Invariant::MinimalDiffLines, false)
        ]),
        "{failures:#?}"
    );
    assert!(
        failures
            .iter()
            .all(|f| f.file.as_deref() == Some("b.txt") && f.language == "text")
    );
}

#[test]
fn writing_within_a_matched_line_breaks_minimal_diff_by_bytes_alone() {
    let python = Python::new();
    let quaff = Broken(|project: &Path, script: &str, run: &mut Run| {
        let path = project.join("b.txt");
        if is_edit(script) && run.code == Some(0) && path.exists() {
            let content = fs::read_to_string(&path).expect("reading b.txt");
            fs::write(&path, content.replacen("start", "START", 1)).expect("writing b.txt");
        }
    });
    let failures = check(&[WORD], &languages(&python), &project(), &quaff);
    assert_eq!(
        broken(&failures)[0],
        (Invariant::MinimalDiff, false),
        "{failures:#?}"
    );
    assert!(
        !failures
            .iter()
            .any(|f| f.invariant == Invariant::MinimalDiffLines),
        "{failures:#?}"
    );
}

#[test]
fn writing_when_the_count_does_not_hold_breaks_atomicity() {
    let python = Python::new();
    let quaff = Broken(|project: &Path, script: &str, run: &mut Run| {
        if is_edit(script) && run.code == Some(2) {
            append(project, "c.txt", b"!");
        }
    });
    let failures = check(&[WORD], &languages(&python), &project(), &quaff);
    // On its own output too, since the re-application's count is checked the same way.
    assert_eq!(
        broken(&failures),
        vec![(Invariant::Atomicity, false), (Invariant::Atomicity, true)],
        "{failures:#?}"
    );
    assert!(
        failures.iter().all(|f| f.file.as_deref() == Some("c.txt")),
        "{failures:#?}"
    );
}

#[test]
fn a_run_differing_from_the_last_breaks_determinism() {
    let python = Python::new();
    // Each run is in a project of its own, so printing where makes every run differ, however
    // the harness orders or overlaps them.
    let quaff = Broken(|project: &Path, script: &str, run: &mut Run| {
        if is_edit(script) && run.code == Some(0) {
            run.stdout += &project.display().to_string();
            run.stdout.push('\n');
        }
    });
    let failures = check(&[WORD], &languages(&python), &project(), &quaff);
    assert!(
        broken(&failures).contains(&(Invariant::Determinism, false)),
        "{failures:#?}"
    );
    assert!(
        failures
            .iter()
            .all(|f| f.invariant == Invariant::Determinism),
        "{failures:#?}"
    );
}

#[test]
fn leaving_a_file_the_oracle_accepted_unparseable_breaks_still_parses() {
    let python = Python::new();
    let quaff = Broken(|project: &Path, script: &str, run: &mut Run| {
        if is_edit(script) && run.code == Some(0) && project.join("a.py").exists() {
            let path = project.join("a.py");
            let content = fs::read_to_string(&path).expect("reading a.py");
            fs::write(&path, content.replace("return", "return return")).expect("writing a.py");
        }
    });
    let failures = check(&[WORD], &languages(&python), &project(), &quaff);
    let parse: Vec<&Failure> = failures
        .iter()
        .filter(|f| f.invariant == Invariant::StillParses)
        .collect();
    assert!(!parse.is_empty(), "{failures:#?}");
    assert!(
        parse
            .iter()
            .all(|f| f.file.as_deref() == Some("a.py") && f.language == "Python")
    );
}

#[test]
fn not_refusing_an_edit_that_breaks_files_breaks_still_parses() {
    let python = Python::new();
    let refused = Case {
        exercises: "an edit breaking files that parsed",
        find: r#"find "def ""#,
        edit: r#"replace string "def " with "def def ""#,
        languages: &["Python"],
        outcome: Outcome::Refused,
        ..WORD
    };
    let quaff = Broken(|project: &Path, script: &str, run: &mut Run| {
        if is_edit(script) && run.code == Some(9) {
            run.code = Some(0);
            append(project, "a.py", b"def def\n");
        }
    });
    let failures = check(&[refused], &languages(&python), &project(), &quaff);
    assert_eq!(
        broken(&failures),
        vec![(Invariant::StillParses, false)],
        "{failures:#?}"
    );
    let said: Vec<Option<&str>> = failures.iter().map(|f| f.file.as_deref()).collect();
    assert_eq!(said, vec![None, Some("a.py")], "{failures:#?}");
}

/// The real `quaff`, but writing outside its matches when the project it edits already holds
/// what an earlier edit wrote: right the first time, wrong on its own output.
struct WrongOnItsOwnOutput;

impl Quaff for WrongOnItsOwnOutput {
    fn run(&self, project: &Path, script: &str) -> Run {
        let again =
            fs::read_to_string(project.join("b.txt")).is_ok_and(|text| text.contains("THE"));
        let run = Binary.run(project, script);
        if is_edit(script) && run.code == Some(0) && again {
            append(project, "c.txt", b"!");
        }
        run
    }
}

#[test]
fn an_edit_wrong_only_on_its_own_output_breaks_idempotency() {
    let python = Python::new();
    let quaff = WrongOnItsOwnOutput;
    let failures = check(&[WORD], &languages(&python), &project(), &quaff);
    // First, so that an empty list cannot pass the check that every failure is on re-applying.
    assert!(
        broken(&failures).contains(&(Invariant::MinimalDiff, true)),
        "{failures:#?}"
    );
    assert!(failures.iter().all(|f| f.reapplying), "{failures:#?}");
}

#[test]
fn a_case_that_finds_or_edits_nothing_is_a_failure_of_the_harness() {
    let python = Python::new();
    let nothing = Case {
        exercises: "a word in no file",
        find: r#"find "absent""#,
        edit: r#"replace string "absent" with "here""#,
        ..WORD
    };
    let unchanged = Case {
        exercises: "a word replaced by itself",
        edit: r#"replace string "the" with "the""#,
        ..WORD
    };
    let failures = check(
        &[nothing, unchanged],
        &languages(&python),
        &project(),
        &Binary,
    );
    let said: Vec<(&str, Invariant, &str)> = failures
        .iter()
        .map(|f| {
            (
                f.case.as_str(),
                f.invariant,
                f.what.split(',').next().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        said,
        vec![
            (
                "replace string (a word in no file)",
                Invariant::Harness,
                "the find matches nothing"
            ),
            (
                "replace string (a word in no file)",
                Invariant::Harness,
                "the find matches nothing"
            ),
            (
                "replace string (a word replaced by itself)",
                Invariant::Harness,
                "the edit changed no file"
            ),
            (
                "replace string (a word replaced by itself)",
                Invariant::Harness,
                "the edit changed no file"
            ),
        ],
        "{failures:#?}"
    );
}

#[test]
fn nothing_to_check_fails_rather_than_passes() {
    let python = Python::new();
    let failures = check(&[], &languages(&python), &project(), &Binary);
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].what.contains("no operation is registered"),
        "{failures:#?}"
    );

    let only_text = files(&[("b.txt", b"the\n")]);
    let failures = check(&[WORD], &languages(&python), &only_text, &Binary);
    assert!(
        failures
            .iter()
            .any(|f| f.language == "Python" && f.what.contains("no file of this language")),
        "{failures:#?}"
    );

    let unknown = Case {
        languages: &["Rust"],
        ..WORD
    };
    let failures = check(&[unknown], &languages(&python), &project(), &Binary);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].what.contains("\"Rust\""), "{failures:#?}");
}

#[test]
fn a_structural_language_without_its_oracle_fails_still_parses() {
    let mut languages = languages(&Python::new());
    languages[0].judge = Judge::Unavailable("not installed".into());
    let failures = check(&[WORD], &languages, &project(), &Binary);
    assert_eq!(failures.len(), 1, "{failures:#?}");
    assert_eq!(
        (failures[0].language.as_str(), failures[0].invariant),
        ("Python", Invariant::StillParses)
    );
    assert!(failures[0].what.ends_with("not installed"));

    languages[0].judge = Judge::Oracle(Oracle {
        program: "/nonexistent/oracle".into(),
        args: Vec::new(),
    });
    let failures = check(&[WORD], &languages, &project(), &Binary);
    assert!(
        failures
            .iter()
            .any(|f| f.invariant == Invariant::Harness && f.what.contains("/nonexistent/oracle")),
        "{failures:#?}"
    );
}

#[test]
fn a_declaration_of_encodings_in_the_corpus_is_reported_not_misread() {
    let python = Python::new();
    let mut corpus = project();
    corpus.insert(
        "sub/.editorconfig".into(),
        b"[*]\ncharset = latin1\n".to_vec(),
    );
    let failures = check(&[WORD], &languages(&python), &corpus, &Binary);
    assert!(
        failures
            .iter()
            .any(|f| f.invariant == Invariant::Harness
                && f.what.starts_with("\"sub/.editorconfig\"")),
        "{failures:#?}"
    );
}

#[test]
fn a_file_no_language_claims_is_reported() {
    let python = Python::new();
    let only_python: Vec<Language> = languages(&python)
        .into_iter()
        .filter(|language| language.name == "Python")
        .collect();
    let failures = check(&[WORD], &only_python, &project(), &Binary);
    assert!(
        failures.iter().any(|f| f.invariant == Invariant::Harness
            && f.what
                .ends_with("is claimed by no language, so no edit of it is checked")),
        "{failures:#?}"
    );
}

#[test]
fn a_file_two_languages_claim_is_reported() {
    let python = Python::new();
    let mut overlapping = languages(&python);
    overlapping[1].claims = |_| true;
    let failures = check(&[WORD], &overlapping, &project(), &Binary);
    assert!(
        failures.iter().any(|f| f.invariant == Invariant::Harness
            && f.what
                .contains("is claimed by more than one language, [\"Python\", \"text\"]")),
        "{failures:#?}"
    );
}
