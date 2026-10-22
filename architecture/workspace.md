# Workspace and toolchain

How the Cargo workspace is laid out, what pins what, and the conventions that are enforced
mechanically.  Each member's design is in its own note: textual search in
`search.md`, encodings in `encoding.md`, the operation representation in `representation.md`,
the script language in `script.md`, the syntax arena in `syntax.md`, and the Python adapter in
`python.md`.

## Layout

```
.
├── Cargo.toml            # the workspace: members, the one version, shared lints
├── Cargo.lock            # committed; every build in CI is --locked
├── rust-toolchain.toml   # the Rust version and components
├── rustfmt.toml          # pins the style edition, nothing else
├── tools.toml            # the verification tools' versions; see verification.md
├── packaging/deb/        # the Linux package's copyright file; see release.md
│   └── install-test/     # the install test's distributions and its in-container check; see verification.md
├── scripts/
│   ├── install-tools     # installs from tools.toml, locally and in CI; see verification.md
│   ├── build-deb         # builds the Linux package; see release.md
│   ├── test-deb          # installs and runs a package on each target distribution; see verification.md
│   ├── break-deb         # breaks a package on purpose, to show test-deb fails; see verification.md
│   ├── fetch-corpus      # fetches the real-world corpus and runs the oracle; see verification.md
│   ├── check-synthetic   # the oracle over the synthetic corpus; see verification.md
│   ├── generate-codecs   # the codec tables, from the oracle's own codecs; see encoding.md
│   ├── generate-declaration-cases # git's and EditorConfig's answers the readers are held to; see encoding.md
│   └── workspace-version # prints the declared version; see release.md
├── crates/
│   ├── quaffed/          # builds the `quaff` binary
│   │   ├── src/          # main.rs, and one module per concern; see search.md
│   │   └── tests/        # integration tests, one file per concern
│   ├── encoding/         # quaffed-encoding: what a file's bytes are, Python's declared encodings; see encoding.md
│   ├── python/           # quaffed-python: the Python adapter, the only crate naming Ruff; see python.md
│   ├── representation/   # quaffed-representation: what a program means; see representation.md
│   ├── script/           # quaffed-script: the script language, text to program; see script.md
│   └── syntax/           # quaffed-syntax: the arena a parsed file becomes, for every language; see syntax.md
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
no parser.  `quaffed-script` parses the script language into it.  `quaffed-syntax` is the arena a parsed
file becomes, the same for every language and depending on no parser, so that the engine reads
one shape whichever parser built it.  `quaffed-python` is the Python adapter, the one crate
allowed to depend on a `ruff_*` crate, so that Ruff's API changes stop there; it builds arenas.  `members = ["crates/*"]` picks up a new member without an edit, and it inherits the
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

### Checklist: upgrading the toolchain

1. Change `channel` in `rust-toolchain.toml` and `rust-version` in the root `Cargo.toml`
   together, then build, test and run clippy.  New lints are fixed or allowed, with a reason, in
   `[workspace.lints]`.
2. **Check whether the new stable accepts condition coverage.**  The release notes say when the
   nightly's `-Z coverage-options` is stabilised and under what name; the stable form is expected
   to be `-C coverage-options`, which an older stable rejects as *unknown codegen option*.  Try it
   on the new toolchain:

   ```bash
   RUSTFLAGS=-Ccoverage-options=condition cargo llvm-cov --workspace --no-report
   ```

   If it builds, condition coverage moves into the gate (`verification.md`, the coverage
   ratchet):
   1. in `scripts/coverage`, build the `stable` run with that flag and report it with
      `--branch`, and remove the `condition` mode;
   2. in `scripts/coverage-ratchet`, read `conditions` from the stable summary and add it to
      `GATED`;
   3. in `.github/workflows/coverage.yaml`, remove `condition` from the matrix and its install
      step;
   4. remove `nightly-coverage` from `tools.toml`;
   5. update the coverage ratchet section of `verification.md`.
3. Update this page and anything else that names the old version.

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

`.github/workflows/report-failures.yaml` posts a failed run to Slack.  Adding a workflow that
runs on a schedule, a push, a release or a pull request means adding it there: the checklist is
in `architecture/verification.md`, under failed runs are posted to Slack.

**Every job here is meant to be a required status check.**  Adding, renaming or removing one
means updating the branch ruleset in the same change, or the branch waits on a check that never
reports, or a new job gates nothing.  The exceptions are the review, below, and `Announce to
Slack`, which runs only when a pull request opens, so never reports on a later push.

### Shared CI

Every `uses:` of `L337-org/github-workflows` runs code from the organisation's public shared
workflows repository, at the major-version tag named in that `uses:`.  What that code does, and
how to run the hygiene check locally, is in that repository's README; it is not restated here,
because it changes there.  When and for whom this repository asks for a review is in the header
of `code-review.yaml`.

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
