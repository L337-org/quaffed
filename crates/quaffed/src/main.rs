// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! `quaff`, the command-line entry point.
//!
//! A placeholder until the design settles: it says that nothing is implemented and exits
//! non-zero, so that nothing can mistake it for a working build.

use std::process::ExitCode;

// The Linux package is a static musl binary, and musl's own allocator parsed far slower than
// mimalloc when measured; see architecture/release.md.  Other builds use the platform's.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    eprintln!(
        "quaff {}: nothing is implemented yet",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::FAILURE
}
