# Workspace and toolchain

How the Cargo workspace is laid out, what pins what, and the conventions that are enforced
mechanically.  This describes the scaffold only.  The engine's own architecture - the parser,
the operation representation, language modules, the journal - is not written here yet,
because the design it would describe is not settled.

## Layout

```
.
├── Cargo.toml            # the workspace: members, the one version, shared lints
├── Cargo.lock            # committed; every build in CI is --locked
├── rust-toolchain.toml   # the Rust version and components
├── rustfmt.toml          # pins the style edition, nothing else
├── crates/
│   └── quaffed/          # the only member so far; builds the `quaff` binary
│       ├── src/main.rs
│       └── tests/        # integration tests, one file per concern
└── architecture/         # detail notes, each routed from AGENTS.md
```

**One crate, deliberately.**  The package is `quaffed` and the binary is `quaff`, matching
the name decision.  Splitting into several crates - a parser front end over a format-neutral
operation representation, an engine behind it - is an architecture decision for when the
design settles, not something to guess at in a scaffold.  The workspace exists so that split
costs a new directory under `crates/` rather than a restructure: `members = ["crates/*"]`
picks up a new member without an edit, and it inherits the version, edition, licence and lints
from `[workspace.package]` and `[workspace.lints]`.

**`publish = false`** on the member.  Nothing is published until a release decides where and
how, and an accidental `cargo publish` of a placeholder is not undoable.

## The version

Declared once, in `[workspace.package]` in the root `Cargo.toml`.  Members read it with
`version.workspace = true`; code reads it with `env!("CARGO_PKG_VERSION")`.  Nothing else may
state it.  `0.0.0` means nothing has been released.

## The toolchain

`rust-toolchain.toml` names the exact Rust version and the `rustfmt` and `clippy` components.
CI installs from it with `rustup toolchain install` (no arguments), so no workflow names a
version and a local run and CI cannot disagree about what clean means.

**A toolchain file only binds rustup.**  A Rust installed without rustup - Homebrew's, for
instance - ignores it and builds with whatever it is.  That is harmless while the local version
matches the pin and misleading once it does not, so contributors building without rustup
should check `rustc --version` against the file after either one moves.

`rust-version` in `[workspace.package]` is the minimum supported Rust version, which is a
different claim from the pinned toolchain.  It is set to the pinned minor version and nothing
tests an older one yet.

## Lints and formatting

- **`unsafe_code = "forbid"`** across the workspace.  Relaxing it is a decision to record, not
  a local `allow`.
- **`missing_docs = "warn"`**, which CI turns into an error.  Every public item carries a doc
  comment: what it does, for somebody who will never read the body.  Rustdoc conventions apply
  - a summary line, then detail, with `# Errors` and `# Panics` sections where they apply.
- **`clippy::pedantic`** at warn, which CI also turns into an error.  A pedantic lint that is
  wrong for this codebase is relaxed in `[workspace.lints.clippy]` with a comment saying why,
  never with an `#[allow]` scattered through the code.
- **rustfmt's defaults** settle formatting and line length.  `rustfmt.toml` only pins the style
  edition, so that a toolchain bump cannot reformat the tree on its own.

## Licence headers

Every `.rs` file under a member's `src/` opens with exactly:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas
```

The year is the year the project started and is never updated.  Tests, benches and build
scripts carry no header.  Vendored or third-party code keeps its original header unmodified.

Enforced by `crates/quaffed/tests/licence_headers.rs`, which walks every member's `src/` and
fails naming each file that does not open with those lines.  It also fails if it found no
files at all, so a broken walk cannot pass by checking nothing.

## CI

`.github/workflows/premerge.yaml` runs on every pull request and every push to `main`:

| job | checks |
|---|---|
| Repository hygiene | the organisation-wide conventions, by the shared `repo-hygiene` action |
| Action pins are immutable | every `uses:` names a full 40-hex commit SHA, by the shared `action-pins` action |
| rustfmt | `cargo fmt --all -- --check` |
| clippy | `cargo clippy --all-targets --locked -- -D warnings` |
| Test | `cargo test --workspace --locked`, on Linux and macOS |

**Every job here is meant to be a required status check.**  Adding, renaming or removing one
means updating the branch ruleset in the same change, or the branch waits on a check that never
reports, or a new job gates nothing.

### Automated review

`.github/workflows/code-review.yaml` decides when a pull request gets a review from Claude.  The
review itself is the shared `claude-review.yaml` reusable workflow, called by commit SHA.

- **When.**  Once by itself, when a pull request is opened, reopened or marked ready for review.
  A push never starts one.  Otherwise a top-level comment starting `/claude-review` asks for one,
  and `/claude-review full` forces a full review.  Comment commands only work from `main`,
  because GitHub runs comment-triggered workflows from the default branch.
- **Depth.**  The first review covers the whole pull request at high effort with no turn cap.
  Later ones cover the commits since the last reviewed commit at lower effort.  The reviewed
  commit is recorded in a marker at the end of each review comment.  A force-push that drops it
  from the branch falls back to a full review.
- **What Claude can do.**  It reads the checkout and runs `git diff`, `git log` and `git show`,
  and nothing else.  It returns a verdict and a review body, and a later step posts them, so
  Claude never holds a token that can write to GitHub.
- **The verdict is advisory.**  The review is not a required check and does not approve the pull
  request.  This is a deliberate exception to every job here being a required status check.
- **Cost.**  The review authenticates with the `CLAUDE_CODE_OAUTH_TOKEN` secret, a subscription
  token from `claude setup-token`.  Every review draws on the allowance of whoever generated it,
  whoever asked for the review.

### Shared CI

The hygiene check, the action-pins check and the review come from the organisation's public
`github-workflows` repository, each pinned to a full commit SHA, and Dependabot proposes the
bumps.  A change to any of them is made there, and reaches this repository when the pin moves.
To run the hygiene check locally, fetch `actions/repo-hygiene/check-repo-hygiene.py` from that
repository at the pinned commit and run `uv run --script check-repo-hygiene.py .` from the
repository root.

## Checklist: adding a workspace member

1. Create `crates/<name>/Cargo.toml` inheriting `version`, `edition`, `rust-version`,
   `license`, `repository` and `authors` from the workspace, with `publish = false` and
   `[lints] workspace = true`.
2. Give every file under its `src/` the licence header; the header test picks the new member up
   by itself.
3. Add it to the layout above and say what it owns.
4. If it changes which crate a reader should look in before changing something, add the row to
   the table in `AGENTS.md`.
