// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The Python adapter: Python source through Ruff's parser, in quaff's own terms.
//!
//! This is the only crate that names a `ruff_*` type.  Everything it offers is quaff's own,
//! so a Ruff upgrade that changes its API is absorbed here and reaches nothing else.  So far it
//! offers what the test corpus's coverage check needs: every kind of node in the grammar, and
//! the kinds a parsed piece of source contains.  `architecture/python.md` specifies it.

mod kinds;

pub use kinds::{
    Kind, ParseError, all_kinds, kinds_in_expression, kinds_in_ipython, kinds_in_module,
};
