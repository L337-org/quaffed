// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! The operation representation: what a quaff program means, with no syntax left in it.
//!
//! The DSL parser and the command line both produce a [`Program`](program::Program); the
//! engine consumes nothing else.  The representation depends on no parser - a structural
//! pattern is carried as written and compiled by the engine - and on nothing that iterates in
//! an order that varies between runs, which `clippy.toml` enforces by forbidding hash maps and
//! sets in this crate.  `architecture/representation.md` describes it.

pub mod check;
pub mod program;
pub mod span;
