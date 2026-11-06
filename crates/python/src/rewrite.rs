// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A rewrite template in Python - the `with` side of a structural `replace` - checked against
//! the pattern whose captures it writes back.
//!
//! Each `$` before a name is claimed and replaced by a placeholder, as on the match side, and the
//! result is parsed, so that Ruff decides where each one landed.  In code it is a hole: the
//! captured code is written there.  In a string, an f-string's or t-string's literal part, or a
//! comment, it interpolates: the captured source text is written there verbatim.  `${name}` is
//! the same as `$name` anywhere, and separates a name from the text after it, which a name
//! otherwise runs into as an identifier does.  Anything else after a `$` that is a name or a brace
//! is an error rather than literal text, so a typo, a name run into the text after it, or a
//! value filter that does not exist yet stops the run instead of reaching a file.  A `$` before
//! anything else is ordinary text, and a literal `$` before a name is written in Python's own
//! spelling, `\x24` or implicit concatenation.

use std::fmt;
use std::ops::Range;

use quaffed_syntax::{Hole, Pattern};
use ruff_python_ast::token::TokenKind;
use ruff_python_parser::ParseOptions;
use ruff_text_size::Ranged;

use crate::parse::ParseError;
use crate::pattern::{
    Claim, identifier_len, node_spanning, parse_either, placeholder_stem, substitute,
};

/// A compiled rewrite template: the template as written, and where in it each captured name is
/// written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    text: String,
    sites: Vec<Site>,
}

impl Rewrite {
    /// The template as written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where each captured name is written back, in the order written.
    #[must_use]
    pub fn sites(&self) -> &[Site] {
        &self.sites
    }
}

/// One place in a template where a captured name is written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The stretch of the template it replaces: `$name`, `$name...` or `${name}`.
    pub written: Range<usize>,
    /// The name, without `$`.
    pub name: String,
    /// Whether it stands inside a string, an f-string's or t-string's literal part, or a comment,
    /// where it interpolates, rather than in code.
    pub in_literal: bool,
}

/// Why a rewrite template could not be compiled.  Each that has a place carries it as a byte
/// offset into the template as written, which [`RewriteError::offset`] gives; the message leaves
/// it out, for the caller to place in terms its reader knows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RewriteError {
    /// The template, with its holes, is not Python.
    Invalid {
        /// What the parser said, verbatim.
        message: String,
        /// Where in the template the error starts.
        offset: usize,
    },
    /// A `$` before a name the pattern does not bind: a typo, a name run into the text after it
    /// (`$name_id` for `name` then `_id`), or a literal `$` before a name.
    Unbound {
        /// What was written, `$name` or `${name}`.
        spelled: String,
        /// Where.
        offset: usize,
    },
    /// `${` followed by anything but a name and `}`, such as a value filter, `${name|upper}`.
    NotAName {
        /// What was written, from `$` to the closing brace if there is one on the line.
        spelled: String,
        /// Where.
        offset: usize,
    },
    /// `$_`, `$...` or `$_...` in a rewrite, which binds nothing, so there is nothing to write.
    BindsNothing {
        /// What was written.
        spelled: String,
        /// Where.
        offset: usize,
    },
    /// A name the pattern binds to one node written as a sequence, or the other way round.
    MixedHole {
        /// The name.
        name: String,
        /// Where.
        offset: usize,
        /// Whether the rewrite writes it as a sequence, `$name...`.
        sequence: bool,
    },
    /// A hole inside a name Python reads as one, `x$a` or `import a.$m`, so it is no node of its
    /// own.
    InsideAName {
        /// What was written.
        spelled: String,
        /// Where.
        offset: usize,
    },
    /// The template has no code: it is empty, or only whitespace and comments.  Replacing what
    /// matched with nothing, which would delete it, is not built; the caller reports it as a
    /// construct this build does not have, so that it can be given that meaning later.
    Empty,
    /// The template is longer than an arena's offsets reach.
    TooLong(quaffed_syntax::TooLong),
}

impl RewriteError {
    /// Where in the template as written the error is, as a byte offset, if it has a place.
    #[must_use]
    pub fn offset(&self) -> Option<usize> {
        match self {
            RewriteError::Invalid { offset, .. }
            | RewriteError::Unbound { offset, .. }
            | RewriteError::NotAName { offset, .. }
            | RewriteError::BindsNothing { offset, .. }
            | RewriteError::MixedHole { offset, .. }
            | RewriteError::InsideAName { offset, .. } => Some(*offset),
            RewriteError::Empty | RewriteError::TooLong(_) => None,
        }
    }
}

impl fmt::Display for RewriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RewriteError::Invalid { message, .. } => {
                write!(f, "the rewrite is not Python: {message}")
            }
            RewriteError::Unbound { spelled, .. } => write!(
                f,
                "\"{spelled}\" names nothing the pattern binds: check the spelling, put braces \
                 round a name followed by more text, as in \"${{name}}_id\", or write a literal \
                 dollar as Python does, \"\\x24\" or '$' 'text'"
            ),
            RewriteError::NotAName { spelled, .. } => write!(
                f,
                "\"{spelled}\" is not a name the pattern binds: inside braces only a bound name \
                 may stand, as in \"${{name}}\""
            ),
            RewriteError::BindsNothing { spelled, .. } => write!(
                f,
                "\"{spelled}\" binds nothing, so a rewrite has nothing to write for it: name the \
                 hole in the pattern and use that name here"
            ),
            RewriteError::MixedHole { name, sequence, .. } => {
                let (pattern, here) = if *sequence {
                    ("one node", format!("\"${name}...\""))
                } else {
                    ("a sequence", format!("\"${name}\""))
                };
                write!(
                    f,
                    "the pattern binds \"${name}\" to {pattern}, but the rewrite writes {here}: \
                     write it as the pattern does"
                )
            }
            RewriteError::InsideAName { spelled, .. } => write!(
                f,
                "\"{spelled}\" stands inside a name Python reads as one, so it cannot be written \
                 back there: a hole can stand for a whole name, but not for part of one"
            ),
            RewriteError::Empty => f.write_str(
                "a rewrite with no code in it, only whitespace or comments, would delete what \
                 matched, which this build cannot do yet",
            ),
            RewriteError::TooLong(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for RewriteError {}

/// What follows a claimed `$`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `$name` or `$name...`.
    Bare,
    /// `${name}`.
    Braced,
    /// `$...`, which in a literal is ordinary text.
    Dots,
}

/// `text`, a structural `replace`'s rewrite template in Python, compiled against `pattern`, the
/// pattern whose captures it writes back.
///
/// # Errors
///
/// Returns a [`RewriteError`] if the template is not Python, names something the pattern does
/// not bind, writes `${` before anything but a bound name and `}`, uses `$_` or `$...`, writes a
/// name as a sequence that the pattern binds to one node or the other way round,
/// puts a hole inside a name, has no code, or is too long.  Nothing partial
/// is returned.
///
/// # Panics
///
/// If text that parsed into an arena does not parse again for its tokens, which would be a
/// defect in Ruff, not an input.
pub fn compile_rewrite(text: &str, pattern: &Pattern) -> Result<Rewrite, RewriteError> {
    let bound = |name: &str| -> Option<bool> {
        pattern
            .holes()
            .find(|(_, hole)| hole.name.as_deref() == Some(name))
            .map(|(_, hole)| hole.sequence)
    };
    let (claims, spellings) = claim(text)?;
    let stem = placeholder_stem(text);
    let substituted = substitute(text, &claims, |_| true, &stem);
    let (mode, arena) = parse_either(&substituted.text).map_err(|err| match err {
        ParseError::Invalid { message, offset } => RewriteError::Invalid {
            message,
            offset: substituted.written_offset(offset),
        },
        ParseError::TooLong(err) => RewriteError::TooLong(err),
    })?;
    if arena.children(arena.root()).next().is_none() {
        return Err(RewriteError::Empty);
    }
    let parsed = ruff_python_parser::parse(&substituted.text, ParseOptions::from(mode))
        .expect("text the arena was built from parses again");
    let mut sites = Vec::new();
    for ((claim, spelling), placed) in claims.iter().zip(&spellings).zip(&substituted.placeholders)
    {
        let token = parsed
            .tokens()
            .iter()
            .find(|token| {
                let range = token.range();
                usize::from(range.start()) <= placed.start && placed.end <= usize::from(range.end())
            })
            .map(|token| (token.kind(), token.range()));
        let site = match token {
            Some((
                TokenKind::String
                | TokenKind::FStringMiddle
                | TokenKind::TStringMiddle
                | TokenKind::Comment,
                _,
            )) => literal_site(text, claim, *spelling, &bound)?,
            Some((TokenKind::Identifier, range))
                if usize::from(range.start()) == placed.start
                    && usize::from(range.end()) == placed.end
                    && node_spanning(&arena, placed).is_some() =>
            {
                Some(code_site(text, claim, *spelling, &bound)?)
            }
            _ => {
                return Err(RewriteError::InsideAName {
                    spelled: text[claim.written.clone()].to_owned(),
                    offset: claim.written.start,
                });
            }
        };
        sites.extend(site);
    }
    Ok(Rewrite {
        text: text.to_owned(),
        sites,
    })
}

/// The site a claim inside a string, an f-string's or t-string's literal part, or a comment
/// is, if it is one: `$...` there is ordinary text, and the dots after `$name` are text after
/// the name.
fn literal_site(
    text: &str,
    claim: &Claim,
    spelling: Spelling,
    bound: &impl Fn(&str) -> Option<bool>,
) -> Result<Option<Site>, RewriteError> {
    let offset = claim.written.start;
    let (name, written) = match spelling {
        Spelling::Dots => return Ok(None),
        Spelling::Braced => (
            &text[offset + 2..claim.written.end - 1],
            claim.written.clone(),
        ),
        Spelling::Bare => {
            let name = claim.hole.name.as_deref().unwrap_or("_");
            (name, offset..offset + 1 + name.len())
        }
    };
    if bound(name).is_none() {
        return Err(RewriteError::Unbound {
            spelled: text[written].to_owned(),
            offset,
        });
    }
    Ok(Some(Site {
        written,
        name: name.to_owned(),
        in_literal: true,
    }))
}

/// The site a claim in code is: a hole the pattern binds, written as the pattern binds it.
fn code_site(
    text: &str,
    claim: &Claim,
    spelling: Spelling,
    bound: &impl Fn(&str) -> Option<bool>,
) -> Result<Site, RewriteError> {
    let spelled = &text[claim.written.clone()];
    let offset = claim.written.start;
    let Some(name) = claim.hole.name.as_deref() else {
        return Err(RewriteError::BindsNothing {
            spelled: spelled.to_owned(),
            offset,
        });
    };
    match bound(name) {
        None => Err(RewriteError::Unbound {
            spelled: if spelling == Spelling::Braced {
                format!("${{{name}}}")
            } else {
                format!("${name}")
            },
            offset,
        }),
        Some(sequence) if sequence != claim.hole.sequence => Err(RewriteError::MixedHole {
            name: name.to_owned(),
            offset,
            sequence: claim.hole.sequence,
        }),
        Some(_) => Ok(Site {
            written: claim.written.clone(),
            name: name.to_owned(),
            in_literal: false,
        }),
    }
}

/// Every `$` the template spells before a name, a brace or `...`, in order, with how it is
/// spelled; where it landed is learnt only from the parse.  `$_` and `$_...` are refused here,
/// wherever they stand, since `_` is never bound; so is `${` before anything but a name and `}`,
/// which is an error in a literal and not Python in code.
fn claim(text: &str) -> Result<(Vec<Claim>, Vec<Spelling>), RewriteError> {
    let mut claims = Vec::new();
    let mut spellings = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find('$') {
        let start = at + found;
        let after = start + 1;
        if text[after..].starts_with('{') {
            let length = identifier_len(&text[after + 1..]);
            let close = after + 1 + length;
            if length == 0 || !text[close..].starts_with('}') {
                let line_end = text[after..].find('\n').map_or(text.len(), |i| after + i);
                let end = text[after..line_end]
                    .find('}')
                    .map_or(after + 1, |i| after + i + 1);
                return Err(RewriteError::NotAName {
                    spelled: text[start..end].to_owned(),
                    offset: start,
                });
            }
            claims.push(Claim {
                written: start..close + 1,
                hole: Hole {
                    name: Some(text[after + 1..close].to_owned()),
                    sequence: false,
                },
            });
            spellings.push(Spelling::Braced);
            at = close + 1;
            continue;
        }
        if text[after..].starts_with("...") {
            claims.push(Claim {
                written: start..after + 3,
                hole: Hole {
                    name: None,
                    sequence: true,
                },
            });
            spellings.push(Spelling::Dots);
            at = after + 3;
            continue;
        }
        let length = identifier_len(&text[after..]);
        if length == 0 {
            // A bare `$`, or one before something that is neither a name nor a brace: text.
            at = after;
            continue;
        }
        let name = &text[after..after + length];
        let mut end = after + length;
        let sequence = text[end..].starts_with("...");
        if sequence {
            end += 3;
        }
        if name == "_" {
            return Err(RewriteError::BindsNothing {
                spelled: text[start..end].to_owned(),
                offset: start,
            });
        }
        claims.push(Claim {
            written: start..end,
            hole: Hole {
                name: Some(name.to_owned()),
                sequence,
            },
        });
        spellings.push(Spelling::Bare);
        at = end;
    }
    Ok((claims, spellings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_pattern;

    /// A pattern binding `name`, `v`, `a`, `scheme`, `host`, `path`, `mime` and `fn` to one node
    /// each, and `args` to a sequence.
    fn binder() -> Pattern {
        compile_pattern("f($name, $v, $a, $scheme, $host, $path, $mime, $fn, $args...)")
            .expect("the binding pattern compiles")
    }

    /// Each site as its name, `literal` or `code`, and the text it replaces.
    fn sites(template: &str) -> Vec<String> {
        compile_rewrite(template, &binder())
            .unwrap_or_else(|err| panic!("{template}: {err}"))
            .sites()
            .iter()
            .map(|site| {
                format!(
                    "{} {} {}",
                    site.name,
                    if site.in_literal { "literal" } else { "code" },
                    &template[site.written.clone()]
                )
            })
            .collect()
    }

    fn refused(template: &str) -> RewriteError {
        compile_rewrite(template, &binder()).expect_err(template)
    }

    // QU-9's measured cases that are decided when a rewrite is compiled, in the order its table
    // lists them.  Those decided when a value is written - the structure check, a non-ASCII value
    // in bytes, a value spanning lines - belong to the edit.

    #[test]
    fn a_bare_name_in_a_docstring_interpolates() {
        assert_eq!(
            sites(r#""""TODO: document $name.""""#),
            ["name literal $name"]
        );
    }

    #[test]
    fn braced_names_in_a_string_interpolate_beside_code_holes() {
        assert_eq!(
            sites(r#"resource("${scheme}://${host}/v2/${path}", mime_type=$mime)($fn)"#),
            [
                "scheme literal ${scheme}",
                "host literal ${host}",
                "path literal ${path}",
                "mime code $mime",
                "fn code $fn"
            ]
        );
    }

    #[test]
    fn braces_separate_a_name_from_the_text_after_it() {
        assert_eq!(sites(r#""${name}_id""#), ["name literal ${name}"]);
    }

    #[test]
    fn a_name_run_into_the_text_after_it_is_unbound() {
        assert_eq!(
            refused(r#""$name_id""#),
            RewriteError::Unbound {
                spelled: "$name_id".to_owned(),
                offset: 1
            }
        );
    }

    #[test]
    fn a_typo_is_unbound() {
        assert_eq!(
            refused(r#""document $nmae""#),
            RewriteError::Unbound {
                spelled: "$nmae".to_owned(),
                offset: 10
            }
        );
    }

    #[test]
    fn a_dollar_before_anything_but_a_name_or_a_brace_is_text() {
        assert_eq!(sites(r#""costs $5""#), Vec::<String>::new());
    }

    #[test]
    fn a_literal_dollar_before_a_name_nothing_binds_is_unbound() {
        assert!(matches!(
            refused("'echo $HOME'"),
            RewriteError::Unbound { spelled, .. } if spelled == "$HOME"
        ));
        assert!(matches!(
            refused("'echo ${HOME}'"),
            RewriteError::Unbound { spelled, .. } if spelled == "${HOME}"
        ));
    }

    #[test]
    fn a_literal_dollar_before_a_bound_name_interpolates() {
        // The one case the rule leaves silent: renaming the metavariable is the way out.
        assert_eq!(sites("'$name'"), ["name literal $name"]);
    }

    #[test]
    fn raw_and_byte_strings_interpolate() {
        assert_eq!(sites(r#"r"\d+$name""#), ["name literal $name"]);
        assert_eq!(sites(r#"b"key-$name""#), ["name literal $name"]);
    }

    #[test]
    fn an_f_strings_literal_part_interpolates_and_its_field_is_code() {
        assert_eq!(sites(r#"f"{x}: $name""#), ["name literal $name"]);
        assert_eq!(sites(r#"f"{$v!r}""#), ["v code $v"]);
    }

    #[test]
    fn a_comment_and_a_t_strings_literal_part_interpolate() {
        assert_eq!(sites("x  # see $name"), ["name literal $name"]);
        assert_eq!(sites(r#"t"{x}: $name""#), ["name literal $name"]);
    }

    #[test]
    fn a_hole_in_code_is_code() {
        assert_eq!(sites("call($a)"), ["a code $a"]);
    }

    #[test]
    fn a_value_filter_is_refused_today() {
        assert_eq!(
            refused(r#""${name|upper}""#),
            RewriteError::NotAName {
                spelled: "${name|upper}".to_owned(),
                offset: 1
            }
        );
    }

    #[test]
    fn pythons_own_spellings_of_a_literal_dollar_are_untouched() {
        assert_eq!(sites(r"'\x24HOME'"), Vec::<String>::new());
        assert_eq!(sites("'$' 'HOME'"), Vec::<String>::new());
    }

    // The decisions beyond QU-9's table.

    #[test]
    fn a_name_the_pattern_does_not_bind_is_refused_in_code_too() {
        assert!(matches!(
            refused("call($b)"),
            RewriteError::Unbound { spelled, offset: 5 } if spelled == "$b"
        ));
    }

    #[test]
    fn holes_that_bind_nothing_are_refused() {
        for template in ["call($_)", "call($...)", "call($_...)", "'$_'"] {
            assert!(
                matches!(refused(template), RewriteError::BindsNothing { .. }),
                "{template}"
            );
        }
    }

    #[test]
    fn dots_after_a_dollar_or_a_name_in_a_string_are_text() {
        assert_eq!(sites(r#""loading$...""#), Vec::<String>::new());
        assert_eq!(sites(r#""called with $args...""#), ["args literal $args"]);
    }

    #[test]
    fn a_sequence_is_written_as_the_pattern_binds_it() {
        assert_eq!(sites("g($args...)"), ["args code $args..."]);
        assert_eq!(
            refused("g($args)"),
            RewriteError::MixedHole {
                name: "args".to_owned(),
                offset: 2,
                sequence: false
            }
        );
        assert_eq!(
            refused("g($a...)"),
            RewriteError::MixedHole {
                name: "a".to_owned(),
                offset: 2,
                sequence: true
            }
        );
    }

    #[test]
    fn braces_round_a_hole_in_code_are_the_same_hole() {
        // Not needed in code, since a name there cannot run into text, but the same spelling
        // means the same thing everywhere.
        assert_eq!(sites("call(${a})"), ["a code ${a}"]);
        assert!(matches!(
            refused("call(${b})"),
            RewriteError::Unbound { spelled, offset: 5 } if spelled == "${b}"
        ));
        // Braces do not make a hole inside a name possible.
        assert!(matches!(
            refused("call(${a}_b)"),
            RewriteError::InsideAName { .. }
        ));
    }

    #[test]
    fn a_hole_inside_a_name_is_refused() {
        assert!(matches!(
            refused("import a.$a"),
            RewriteError::InsideAName { .. }
        ));
    }

    #[test]
    fn a_template_with_no_code_is_refused() {
        assert_eq!(refused(""), RewriteError::Empty);
        assert_eq!(refused("# only a comment"), RewriteError::Empty);
    }

    #[test]
    fn a_template_that_is_not_python_is_placed_as_written() {
        let err = refused("call($a,, $v)");
        assert!(matches!(err, RewriteError::Invalid { .. }), "{err:?}");
        assert_eq!(err.offset(), Some(8));
    }

    #[test]
    fn every_error_with_a_place_says_where_and_reads_without_it() {
        let err = refused(r#""$nmae""#);
        assert_eq!(err.offset(), Some(1));
        assert!(!err.to_string().contains('1'), "{err}");
    }
}
