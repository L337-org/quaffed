# Workspace and toolchain

How the Cargo workspace is laid out, what pins what, and the conventions that are enforced
mechanically.  What each member does is specified in its own note: textual search in
`search.md`, encodings in `encoding.md`, the operation representation in `representation.md`,
the script language in `script.md`, and the Python adapter in `python.md`.

## Layout

```
.
├── Cargo.toml            # the workspace: members, the one version, shared lints
├── Cargo.lock            # committed; every build in CI is --locked
├── rust-toolchain.toml   # the Rust version and components
├── rustfmt.toml          # pins the style edition, nothing else
├── tools.toml            # the verification tools' versions; see verification.md
├── packaging/deb/        # the Linux package's copyright file; see release.md
├── scripts/
│   ├── install-tools     # installs from tools.toml, locally and in CI; see verification.md
│   ├── build-deb         # builds the Linux package; see release.md
│   ├── fetch-corpus      # fetches the real-world corpus and runs the oracle; see verification.md
│   ├── check-synthetic   # the oracle over the synthetic corpus; see verification.md
│   ├── generate-codecs   # the codec tables, from the oracle's own codecs; see encoding.md
│   └── workspace-version # prints the declared version; see release.md
├── crates/
│   ├── quaffed/          # builds the `quaff` binary
│   │   ├── src/          # main.rs, and one module per concern; see search.md
│   │   └── tests/        # integration tests, one file per concern
│   ├── encoding/         # quaffed-encoding: what a file's bytes are, Python's declared encodings; see encoding.md
│   ├── python/           # quaffed-python: the Python adapter, the only crate naming Ruff; see python.md
│   ├── representation/   # quaffed-representation: what a program means; see representation.md
│   └── script/           # quaffed-script: the script language, text to program; see script.md
├── corpus/
│   ├── synthetic/        # the synthetic Python corpus, checked in; see verification.md
│   └── real-world.toml   # pins the fetched real-world corpus; see verification.md
└── architecture/         # detail notes, each routed from AGENTS.md
```

**The members.**  `quaffed` builds the `quaff` binary, matching the name decision.
`quaffed-encoding` decides what a file's bytes are and reads Python's declared encodings, a
crate of its own so that the binary can read them without linking the Python adapter, and Ruff
with it, and the adapter can take text decoded the same way.  `quaffed-representation` is the
operation representation, a crate of its own so that its
lint configuration - no hash maps or sets - applies to it alone, and so that it can depend on
no parser.  `quaffed-script` parses the script language into it.  `quaffed-python` is the Python
adapter, the one crate allowed to depend on a `ruff_*` crate, so that Ruff's API changes stop
there.  `members = ["crates/*"]` picks up a new member without an edit, and it inherits the
version, edition, licence and lints from `[workspace.package]` and `[workspace.lints]`.

**`publish = false`** on every member.  Nothing is published until a release decides where and
how, and an accidental `cargo publish` of a placeholder is not undoable.

## The version

Declared once, in `[workspace.package]` in the root `Cargo.toml`.  Members read it with
`version.workspace = true`; code reads it with `env!("CARGO_PKG_VERSION")`.  Nothing else may
state it.  It names the release the work in progress is for, not the last one shipped: it is
bumped when work for a new release starts, so everything built along the way already carries
the version it will ship as.

## The toolchain

`rust-toolchain.toml` names the exact Rust version and the `rustfmt` and `clippy` components.
CI installs from it with `rustup toolchain install` (no arguments), so no workflow names a
version and a local run and CI cannot disagree about what clean means.

**A toolchain file only binds rustup.**  A Rust installed without rustup - Homebrew's, for
instance - ignores it and builds with whatever it is.  That is harmless while the local version
matches the pin and misleading once it does not, so contributors building without rustup
should check `rustc --version` against the file after either one moves.

`rust-version` in `[workspace.package]` is the minimum supported Rust version, which is a
different claim from the pinned toolchain.  It is the pinned minor version, because no older one
is tested.

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

`.github/workflows/premerge.yaml` runs on every pull request and every push to `main`; each job's
name and comments say what it checks.

`.github/workflows/package.yaml` also runs on every pull request, building the Linux package for
each architecture; `architecture/release.md` describes it and the release workflow.

`.github/workflows/report-failures.yaml` posts the failure of a run nobody is watching to Slack.
Adding a workflow that runs on a schedule, a push or a release means adding it there: the
checklist is in `architecture/verification.md`, under failed runs nobody is watching.

**Every job here is meant to be a required status check.**  Adding, renaming or removing one
means updating the branch ruleset in the same change, or the branch waits on a check that never
reports, or a new job gates nothing.

### Shared CI

The `Repository hygiene` and `Action pins are immutable` jobs, the Claude review in
`code-review.yaml` and the Slack reporter in `report-failures.yaml` run code from the
organisation's public `github-workflows` repository, pinned to a commit.  What that code does,
and how to run the hygiene check locally, is in that repository's README at the pinned commit;
it is not restated here, because it changes there.  When and for whom this repository asks for
a review is in the header of `code-review.yaml`.

**The review is advisory.**  It is not a required check and does not approve the pull request,
which is a deliberate exception to every job here being a required status check.

## Checklist: adding a workspace member

1. Create `crates/<name>/Cargo.toml` inheriting `version`, `edition`, `rust-version`,
   `license`, `repository` and `authors` from the workspace, with `publish = false` and
   `[lints] workspace = true`.
2. Give every file under its `src/` the licence header; the header test picks the new member up
   by itself.
3. Add it to the layout above and say what it owns.
4. If it changes which crate a reader should look in before changing something, add the row to
   the table in `AGENTS.md`.
