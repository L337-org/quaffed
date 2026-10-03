//! Properties of the script language that hold for any input, checked on generated ones.
//!
//! - **Any content can be written as an operand**, by the shortest fence the rules allow, and
//!   reads back exactly: nothing is escaped, so the only tool is the fence's length.
//! - **Nothing the parser is given makes it panic**: every input is a program or an error.
//! - **Layout never changes meaning**: the same statements with other spacing, blank lines and
//!   comments parse to the same program.

use proptest::prelude::*;
use quaffed_representation::program::Program;
use quaffed_representation::span::Source;
use quaffed_script::lexer::{TokenKind, tokens};
use quaffed_script::parse_source;

/// The lengths of every run of `delimiter` in `content`.
fn runs(content: &str, delimiter: char) -> Vec<usize> {
    let mut found = Vec::new();
    let mut length = 0;
    for c in content.chars() {
        if c == delimiter {
            length += 1;
        } else if length > 0 {
            found.push(length);
            length = 0;
        }
    }
    if length > 0 {
        found.push(length);
    }
    found
}

/// Spells `content` as an operand with `delimiter`, by the shortest fence the rules allow.
///
/// A single delimiter when no lone one occurs and the content neither starts nor ends with
/// one; otherwise the shortest run of three or more that does not occur, padded with a space
/// at each end when the content starts or ends with the delimiter, or already starts and ends
/// with a space, so the fence's trimming gives it back exactly.
fn spell(content: &str, delimiter: char) -> String {
    if content.is_empty() {
        return format!("{delimiter}{delimiter}");
    }
    let present = runs(content, delimiter);
    let edges = content.starts_with(delimiter) || content.ends_with(delimiter);
    if !present.contains(&1) && !edges {
        return format!("{delimiter}{content}{delimiter}");
    }
    // A run longer than the content cannot occur in it, so the search ends there.
    let fence_length = (3..=content.len() + 3)
        .find(|n| !present.contains(n))
        .expect("a length longer than any run is free");
    let fence = delimiter.to_string().repeat(fence_length);
    let spaced = content.len() >= 2 && content.starts_with(' ') && content.ends_with(' ');
    if edges || spaced {
        format!("{fence} {content} {fence}")
    } else {
        format!("{fence}{content}{fence}")
    }
}

/// Parses `text` as one `-e` expression.
fn parse(text: &str) -> Result<Program, quaffed_script::ParseError> {
    let mut program = Program::default();
    parse_source(&mut program, Source::Expression(1), text)?;
    Ok(program)
}

/// Words the parser acts on, so that generated input gets past the first word and reaches the
/// clause, condition and count parsers, which random characters almost never do.
const VOCABULARY: &[&str] = &[
    "find",
    "replace",
    "string",
    "with",
    "delete",
    "expect",
    "in",
    "where",
    "not",
    "and",
    "matches",
    "contains",
    "FILE",
    "at",
    "least",
    "most",
    "or",
    "none",
    "any",
    "as",
    "reject",
    "applicable",
    "no",
    "insert",
    "ENCLOSING",
    "LANGUAGE",
    "\"x\"",
    "\"\"",
    "\"\"\" a \"\"\"",
    "`f($a)`",
    "`g(${a|x})`",
    "``",
    "$x",
    "$_",
    "$x...",
    "$^",
    "^",
    "$",
    "0",
    "1",
    "18446744073709551616",
    "[",
    "]",
    ";",
    "\n",
    "#",
];

/// Statements as token lists, so that they can be laid out in more than one way.
fn statement() -> impl Strategy<Value = Vec<String>> {
    let literal = "[a-z ;#$^]{1,6}";
    let count = prop_oneof![
        (0u8..9).prop_map(|n| vec![n.to_string()]),
        (0u8..9).prop_map(|n| vec!["at".into(), "least".into(), n.to_string()]),
        Just(vec!["none".to_string()]),
        (0u8..9).prop_map(|n| vec![n.to_string(), "or".into(), "none".into()]),
    ];
    prop_oneof![
        (literal, proptest::option::of(count.clone())).prop_map(|(text, count)| {
            let mut statement = vec!["find".to_string(), spell(&text, '"')];
            if let Some(count) = count {
                statement.push("expect".into());
                statement.extend(count);
            }
            statement
        }),
        literal.prop_map(|text| vec![spell(&text, '`')]),
        (literal, literal, count).prop_map(|(from, to, count)| {
            let mut statement = vec![
                "replace".to_string(),
                "string".into(),
                spell(&from, '"'),
                "with".into(),
                spell(&to, '"'),
                "expect".into(),
            ];
            statement.extend(count);
            statement
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn any_content_reads_back_from_its_shortest_spelling(
        content in "[ab\"` \\\\'\n]{0,16}",
        backticked: bool,
    ) {
        let delimiter = if backticked { '`' } else { '"' };
        let spelled = spell(&content, delimiter);
        let found: Vec<TokenKind> = tokens(&spelled)
            .map_err(|e| TestCaseError::fail(format!("{spelled:?}: {}", e.message)))?
            .into_iter()
            .map(|t| t.kind)
            .collect();
        let expected = if backticked {
            TokenKind::Backticked(content.clone())
        } else {
            TokenKind::Quoted(content.clone())
        };
        prop_assert_eq!(found, vec![expected], "spelled as {:?}", spelled);
    }

    #[test]
    fn the_parser_never_panics(text in "[a-z\"`$^;#\\[\\]{}|.0-9 \n]{0,48}") {
        let _ = parse(&text);
    }

    #[test]
    fn the_parser_never_panics_on_sequences_of_its_own_words(
        words in proptest::collection::vec(proptest::sample::select(VOCABULARY), 0..24),
        gaps in proptest::collection::vec(0usize..3, 24),
    ) {
        let mut text = String::new();
        for (word, gap) in words.iter().zip(&gaps) {
            text.push_str(word);
            text.push_str([" ", "", "\n"][*gap]);
        }
        let _ = parse(&text);
    }

    #[test]
    fn layout_and_comments_never_change_the_program(
        statements in proptest::collection::vec(statement(), 1..5),
        gaps in proptest::collection::vec(0usize..4, 64),
    ) {
        let tight: Vec<String> = statements.iter().map(|s| s.join(" ")).collect();
        let tight = tight.join(";");
        let spacing = [" ", "  ", "\t", " \t "];
        let between = ["\n", "\n\n", "\n# a comment\n", " ; "];
        let mut loose = String::from("# leading comment\n");
        let mut gap = gaps.iter().cycle();
        for statement in &statements {
            for token in statement {
                loose.push_str(token);
                loose.push_str(spacing[*gap.next().expect("cycles")]);
            }
            loose.push_str(between[*gap.next().expect("cycles")]);
        }
        let tight_program = parse(&tight)
            .map_err(|e| TestCaseError::fail(format!("{tight:?}: {}", e.message)))?;
        let loose_program = parse(&loose)
            .map_err(|e| TestCaseError::fail(format!("{loose:?}: {}", e.message)))?;
        prop_assert_eq!(tight_program, loose_program, "{:?} against {:?}", tight, loose);
    }
}

#[test]
fn the_spelling_helper_matches_the_syntax_pages_choices() {
    assert_eq!(spell("C:\\temp", '"'), "\"C:\\temp\"");
    assert_eq!(spell("say \"hi\"", '"'), "\"\"\" say \"hi\" \"\"\"");
    assert_eq!(spell("\"hi\"", '"'), "\"\"\" \"hi\" \"\"\"");
    assert_eq!(spell("", '"'), "\"\"");
    assert_eq!(spell("x = \"use `ls`\"", '`'), "```x = \"use `ls`\"```");
}
