// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Compiling: each structural pattern, rewrite and `where` of a plan, for each language, before
//! any file is read.

use quaffed_representation::program::{Filter, Subject};
use quaffed_representation::span::{Span, Spanned};
use quaffed_syntax::{CompileError, FrontEnd, Pattern, Rewrite};

use super::{
    failure::Failure,
    plan::{PlannedRewrite, Searching, Wanted},
    sources::Built,
};
use crate::filter;
use crate::rewrite;
use crate::scope;

/// The run's structural patterns, their rewrites and their `where`s, compiled for one language,
/// each where the language reads it: a file is searched with its own language's.
pub(super) struct Compiled {
    /// The language.
    pub(super) language: &'static dyn FrontEnd,
    /// Each structural pattern, by its place among them, if the language reads it and its
    /// `where`.
    pub(super) patterns: Vec<Option<Pattern>>,
    /// Each rewrite template, by its place among them, where the language reads its pattern:
    /// compiled, or why the language cannot read it, which refuses the edit in a file of the
    /// language where the pattern matches.
    pub(super) rewrites: Vec<Option<Result<Rewrite, CompileError>>>,
    /// Each of the run's patterns' `where`, by the pattern's index among all of them, where it
    /// tests a metavariable and the language reads the pattern.
    pub(super) filters: Vec<Option<filter::Filter>>,
}

/// A script error for text no language could compile, at the place of the first language's
/// error: what it said, or with more than one language, what each said, named.
pub(super) fn unread(
    errors: &[(&dyn FrontEnd, CompileError)],
    span: Span,
    built: &Built,
) -> Failure {
    let (_, first) = &errors[0];
    let message = match errors {
        [(_, only)] => only.to_string(),
        _ => errors
            .iter()
            .map(|(language, err)| format!("as {}: {err}", language.language()))
            .collect::<Vec<_>>()
            .join("; "),
    };
    Failure::Script {
        location: built.locate(
            span,
            first
                .offset()
                .map_or(span.start, |at| built.content_start(span) + at),
        ),
        message,
    }
}

/// Each rewrite, checked against the languages that read its pattern: one that none of them reads
/// stops the run, with what each said.  Checked when the rewrites are compiled, and again once
/// the `where`s have left a pattern unread in the languages that cannot read them.
pub(super) fn rewrites_read(
    compiled: &[Compiled],
    planned_rewrites: &[PlannedRewrite],
    built: &Built,
) -> Result<(), Failure> {
    for (index, (_, span, compiled_at)) in planned_rewrites.iter().enumerate() {
        let reading: Vec<&Compiled> = compiled
            .iter()
            .filter(|c| c.patterns[*compiled_at].is_some())
            .collect();
        let errors: Vec<(&dyn FrontEnd, CompileError)> = reading
            .iter()
            .filter_map(|c| match &c.rewrites[index] {
                Some(Err(err)) => Some((c.language, err.clone())),
                _ => None,
            })
            .collect();
        if !reading.is_empty() && errors.len() == reading.len() {
            return Err(unread(&errors, *span, built));
        }
    }
    Ok(())
}

/// Every structural pattern compiled for each of `languages`, and every rewrite template against
/// the pattern whose captures it writes back, before any file is read.  A pattern no language
/// reads stops the run before it looks at anything, as does a rewrite no language that reads its
/// pattern reads.
pub(super) fn compile(
    languages: &[&'static dyn FrontEnd],
    structural: &[(&str, Span)],
    planned_rewrites: &[PlannedRewrite],
    built: &Built,
) -> Result<Vec<Compiled>, Failure> {
    let mut compiled: Vec<Compiled> = languages
        .iter()
        .map(|&language| Compiled {
            language,
            patterns: Vec::with_capacity(structural.len()),
            rewrites: Vec::with_capacity(planned_rewrites.len()),
            filters: Vec::new(),
        })
        .collect();
    for (written, span) in structural {
        let mut errors = Vec::new();
        for for_language in &mut compiled {
            let language = for_language.language;
            match language.compile_pattern(written) {
                Ok(pattern) => for_language.patterns.push(Some(pattern)),
                Err(err) => {
                    for_language.patterns.push(None);
                    errors.push((language, err));
                }
            }
        }
        if errors.len() == languages.len() {
            return Err(unread(&errors, *span, built));
        }
    }
    for (written, _, compiled_at) in planned_rewrites {
        for for_language in &mut compiled {
            let language = for_language.language;
            let rewrite = for_language.patterns[*compiled_at]
                .as_ref()
                .map(|pattern| language.compile_rewrite(written, pattern));
            for_language.rewrites.push(rewrite);
        }
    }
    rewrites_read(&compiled, planned_rewrites, built)?;
    // A replace's unnamed holes are named once its rewrite is compiled, which reports `$_` in a
    // rewrite as the unnamed hole it is, so that each match says where they matched too.
    for for_language in &mut compiled {
        for (_, _, compiled_at) in planned_rewrites {
            if let Some(pattern) = &mut for_language.patterns[*compiled_at] {
                *pattern = rewrite::name_every_hole(pattern);
            }
        }
    }
    Ok(compiled)
}

/// Each of the run's patterns' `where`, compiled for each language that reads the pattern, where
/// it tests a metavariable: checked first against the pattern, which must be structural, must
/// bind each name it tests to one node, and with the names it tests must not be too costly to
/// search.  A language that cannot read the `where` does not read the pattern either; one that
/// no language reads stops the run, with the first language's error.
pub(super) fn filters(
    searching: &[Searching],
    compiled: &mut [Compiled],
    roots: scope::Roots,
    built: &Built,
) -> Result<(), Failure> {
    for for_language in compiled.iter_mut() {
        for_language.filters = searching.iter().map(|_| None).collect();
    }
    for (index, searching) in searching.iter().enumerate() {
        let Some(written) = &searching.query.filter else {
            continue;
        };
        let mut failures = Vec::new();
        let mut reading = 0;
        for for_language in compiled.iter_mut() {
            let pattern = match searching.pattern {
                Wanted::Structural { compiled: at, .. } => match &for_language.patterns[at] {
                    Some(pattern) => Some(pattern),
                    None => continue,
                },
                Wanted::Text(_) => None,
            };
            reading += 1;
            match filter_for(for_language.language, written, pattern, roots, built) {
                Ok(filter) => for_language.filters[index] = filter,
                Err(failure @ (Failure::Unknown { .. } | Failure::Io { .. })) => {
                    return Err(failure);
                }
                Err(failure) => {
                    if let Wanted::Structural { compiled: at, .. } = searching.pattern {
                        for_language.patterns[at] = None;
                    }
                    failures.push(failure);
                }
            }
        }
        if failures.len() == reading
            && let Some(first) = failures.into_iter().next()
        {
            return Err(first);
        }
    }
    Ok(())
}

/// The `where` written, compiled in `language`, where it tests a metavariable, and checked
/// against `pattern`, the statement's in that language, or `None` for a textual statement.
fn filter_for(
    language: &dyn FrontEnd,
    written: &Spanned<Filter>,
    pattern: Option<&Pattern>,
    roots: scope::Roots,
    built: &Built,
) -> Result<Option<filter::Filter>, Failure> {
    let script = |span: Span, message: String| Failure::Script {
        location: built.locate(span, span.start),
        message,
    };
    let outer: Vec<&str> = pattern.map_or_default(|pattern| {
        pattern
            .holes()
            .filter_map(|(_, hole)| hole.name.as_deref())
            .collect()
    });
    let compiled_filter =
        filter::Filter::new(language, written, roots, &outer).map_err(|err| match err {
            filter::Error::NotBuilt(filter::NotBuilt(span)) => Failure::Unknown {
                location: built.locate(span, span.start),
                message: "this `where` test is not built yet".into(),
            },
            filter::Error::Pattern(err, span) => unread(&[(language, err)], span, built),
            filter::Error::Glob(scope::Error::TooLarge(err), span) => script(span, err.to_string()),
            filter::Error::Glob(scope::Error::Io { path, cause }, _) => Failure::Io {
                doing: "looking for",
                path,
                cause,
            },
        })?;
    let Some(compiled_filter) = compiled_filter else {
        return Ok(None);
    };
    let span = metavariable_test(written).unwrap_or(written.span);
    let Some(pattern) = pattern else {
        return Err(script(
            span,
            "a textual search binds no metavariables, so there is nothing for `where` to test \
             here: test FILE, or search for a backticked pattern"
                .into(),
        ));
    };
    check_filter(&compiled_filter, pattern, span, built)?;
    Ok(Some(compiled_filter))
}

/// Checks a compiled `where` against `pattern`, the statement's: each name it tests must be one
/// the pattern binds to one node, its own patterns must share no name with it, and together
/// they must not be too costly to search.  `span` is where its first test of a metavariable
/// was written.
fn check_filter(
    compiled_filter: &filter::Filter,
    pattern: &Pattern,
    span: Span,
    built: &Built,
) -> Result<(), Failure> {
    let script = |span: Span, message: String| Failure::Script {
        location: built.locate(span, span.start),
        message,
    };
    for name in &compiled_filter.names {
        let holes: Vec<bool> = pattern
            .holes()
            .filter(|(_, hole)| hole.name.as_deref() == Some(name.as_str()))
            .map(|(_, hole)| hole.sequence)
            .collect();
        if name == "_" {
            return Err(script(
                span,
                "`$_` binds nothing, so `where` cannot test it".into(),
            ));
        }
        if holes.is_empty() {
            return Err(script(
                span,
                format!("`${name}` is not bound by this statement's pattern"),
            ));
        }
        if holes.contains(&true) {
            // A name the `where`'s pattern shares is tested because it is shared, so say that.
            let sharing = compiled_filter.shapes.iter().find(|(shape, _)| {
                shape
                    .holes()
                    .any(|(_, hole)| hole.name.as_deref() == Some(name.as_str()))
            });
            return Err(match sharing {
                Some((_, at)) => script(
                    *at,
                    format!(
                        "`${name}...` is a sequence in the statement's pattern and one node in \
                         the `where`'s, and a name shared between them is one or the other: \
                         name it differently in one of them"
                    ),
                ),
                None => script(
                    span,
                    format!("`${name}` binds a sequence, and `where` tests what one node holds"),
                ),
            });
        }
    }
    // A name the `where`'s pattern shares with the statement's is bound there to what the
    // statement's match bound it to, one node, so it must be one node in both.
    for (shape, at) in &compiled_filter.shapes {
        if let Some(shared) = shape
            .holes()
            .filter(|(_, hole)| hole.sequence)
            .filter_map(|(_, hole)| hole.name.as_deref())
            .find(|name| compiled_filter.names.iter().any(|tested| tested == name))
        {
            return Err(script(
                *at,
                format!(
                    "`${shared}...` is a sequence in the `where`'s pattern and one node in the \
                     statement's, and a name shared between them is one or the other: name it \
                     differently in one of them"
                ),
            ));
        }
    }
    let names: Vec<&str> = compiled_filter.names.iter().map(String::as_str).collect();
    if pattern.costly_with(&names).is_some() {
        return Err(script(
            span,
            format!(
                "testing {} in `where` makes a search depend on a third place at once where \
                 it can go more than one way - where a sequence stops, or where a run of \
                 statements starts - and each such place multiplies the work of a search by \
                 the length of the block or list it stands in, so a pattern and its `where` \
                 may depend on two between them: test fewer of its names, or give the \
                 pattern more fixed code",
                names
                    .iter()
                    .map(|name| format!("`${name}`"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        ));
    }
    Ok(())
}

/// Where the first test of a metavariable in `filter` was written, if it has one.
fn metavariable_test(filter: &Spanned<Filter>) -> Option<Span> {
    match &filter.node {
        Filter::And(parts) => parts.iter().find_map(metavariable_test),
        Filter::Not(part) => metavariable_test(part),
        Filter::Matches(Subject::Metavariable(_), _)
        | Filter::Contains(Subject::Metavariable(_), _) => Some(filter.span),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::SourceArg;
    use crate::contents::Contents;
    use crate::languages;
    use crate::run::sources;

    /// The program `text` makes as one `-e`, and the span of its first backticked operand.
    fn built(text: &str) -> (Built, Span) {
        let built = sources::read(
            vec![SourceArg::Expression {
                number: 1,
                text: text.into(),
            }],
            None,
            &mut std::io::empty(),
            &Contents::default(),
        );
        let Ok(built) = built else {
            panic!("{:?}", built.err());
        };
        let start = text.find('`').expect("a backticked operand");
        let end = text.rfind('`').expect("a backticked operand") + 1;
        (
            built,
            Span {
                source: 0,
                start,
                end,
            },
        )
    }

    #[test]
    fn a_pattern_is_compiled_for_each_language_and_held_where_one_reads_it() {
        let (built, span) = built("find `f($a)`\n");
        let compiled = compile(languages::LANGUAGES, &[("f($a)", span)], &[], &built);
        let Ok(compiled) = compiled else {
            panic!("{:?}", compiled.err());
        };
        assert_eq!(compiled.len(), languages::LANGUAGES.len());
        assert!(compiled.iter().any(|c| c.patterns[0].is_some()));
    }

    #[test]
    fn a_pattern_no_language_reads_is_a_script_error_before_any_file_is_read() {
        let (built, span) = built("find `f(`\n");
        match compile(languages::LANGUAGES, &[("f(", span)], &[], &built) {
            Err(failure @ Failure::Script { .. }) => assert_eq!(failure.exit_code(), 3),
            other => panic!("{:?}", other.err()),
        }
    }
}
