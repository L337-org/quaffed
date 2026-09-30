// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `quaff`, the command-line entry point.
//!
//! A placeholder until the design settles: it says that nothing is implemented and exits
//! non-zero, so that nothing can mistake it for a working build.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "quaff {}: nothing is implemented yet",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::FAILURE
}
