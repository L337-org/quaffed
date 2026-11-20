// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The Python adapter: Python source through Ruff's parser, in quaff's own terms.
//!
//! This is the only crate that names a `ruff_*` type.  It parses source into a syntax arena
//! (`quaffed-syntax`), the shape the engine reads for every language, and [`Python`] is the
//! front end the engine reaches it through, so a Ruff upgrade that changes its API is absorbed
//! here and reaches nothing else.  It also offers every kind of node in the grammar, and the
//! kinds a parsed piece of source contains, which is what the test corpus's coverage check
//! needs.  `architecture/python.md` describes it.

mod front;
mod kinds;
mod lex;
mod lists;
mod parse;
mod pattern;
mod rewrite;
mod scope;

pub use front::Python;
pub use kinds::all_kinds;
pub use lex::{code_tokens, escaped, lex, token_kinds};
pub use parse::{
    PYTHON, kinds_in_expression, kinds_in_ipython, kinds_in_module, parse_expression,
    parse_ipython, parse_module,
};
pub use pattern::{PatternError, compile_pattern};
pub use quaffed_syntax::Kind;
pub use rewrite::{RewriteError, compile_rewrite};
pub use scope::misplaced;
