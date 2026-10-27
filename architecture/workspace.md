# Workspace and toolchain

How the Cargo workspace is laid out, what pins what, and the conventions enforced mechanically.
Each member's design is in its own note: textual search in `search.md`, encodings in
`encoding.md`, the operation representation in `representation.md`, the script language in
`script.md`, the syntax arena in `syntax.md`, and the Python adapter in `python.md`.

## Layout

```
.
├── Cargo.toml            # the workspace: members, the one version, shared lints
├── Cargo.lock            # committed; every build in CI is --locked
├── rust-toolchain.toml   # the Rust version and components
├── rustfmt.toml          # pins the style edition, nothing else
├── tools.toml            # the verification tools' versions; see tools.md
├── packaging/deb/        # the Linux package's copyright file; see release.md
│   └── install-test/     # the install test's distributions and its in-container check; see release.md
├── scripts/
│   ├── install-tools     # installs from tools.toml, locally and in CI; see tools.md
│   ├── build-deb         # builds the Linux package; see release.md
│   ├── test-deb          # installs and runs a package on each target distribution; see release.md
│   ├── break-deb         # breaks a package on purpose, to show test-deb fails; see release.md
│   ├── fetch-corpus      # fetches the real-world corpus and runs the oracle; see corpus.md
│   ├── check-synthetic   # the oracle over the synthetic corpus; see corpus.md
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
│   ├── synthetic/        # the synthetic Python corpus, checked in; see corpus.md
│   └── real-world.toml   # pins the fetched real-world corpus; see corpus.md
└── architecture/         # detail notes, each routed from AGENTS.md
```

**Why the members are split:**

- `quaffed-encoding` is its own crate so that the binary can read declared encodings without
  linking the Python adapter, and Ruff with it.
- `quaffed-representation` is its own crate so that its lint configuration - no hash maps or sets -
  applies to it alone, and so that it depends on no parser.
- `quaffed-syntax` depends on nothing, so that the engine reads one shape whichever parser built it.
- `quaffed-python` is the one crate allowed to depend on a `ruff_*` crate, so that Ruff's API
  changes stop there.

`members = ["crates/*"]` picks up a new member without an edit, and it inherits the version,
edition, licence and lints from `[workspace.package]` and `[workspace.lints]`.

**`publish = false`** on every member, because an accidental `cargo publish` cannot be undone.

## The version

Declared once, in `[workspace.package]` in the root `Cargo.toml`.  Members read it with
`version.workspace = true` and code with `env!("CARGO_PKG_VERSION")`; nothing else may state it.
It names the release the work in progress is for, and is bumped when work for a new release starts.

## The toolchain

`rust-toolchain.toml` names the exact Rust version and the `rustfmt` and `clippy` components.  CI
installs from it with `rustup toolchain install`, with no arguments, so no workflow names a
version.  **A toolchain file only binds rustup**: a Rust installed another way ignores it, so check
`rustc --version` against the file.

`rust-version` in `[workspace.package]` is the minimum supported Rust version, and is the pinned
minor version, because no older one is tested.

### Checklist: upgrading the toolchain

1. Change `channel` in `rust-toolchain.toml` and `rust-version` in the root `Cargo.toml`
   together, then build, test and run clippy.  Fix new lints, or allow them with a reason in
   `[workspace.lints]`.
2. **Check whether the new stable accepts condition coverage**, which an older stable rejects as
   *unknown codegen option*:

   ```bash
   RUSTFLAGS=-Ccoverage-options=condition cargo llvm-cov --workspace --no-report
   ```

   If it builds, move condition coverage into the gate (`testing.md`, the coverage ratchet):
   1. in `scripts/coverage`, build the `stable` run with that flag and report it with `--branch`,
      and remove the `condition` mode;
   2. in `scripts/coverage-ratchet`, read `conditions` from the stable summary and add it to
      `GATED`;
   3. in `.github/workflows/coverage.yaml`, remove `condition` from the matrix and its install
      step;
   4. remove `nightly-coverage` from `tools.toml`;
   5. update the coverage ratchet section of `testing.md`.
3. Update anything else that names the old version.

## Lints and formatting

- **`unsafe_code = "forbid"`** across the workspace.  Relaxing it is a decision to record, not a
  local `allow`.
- **`missing_docs = "warn"`**, which CI turns into an error.  Every public item carries a doc
  comment, following rustdoc's conventions, with `# Errors` and `# Panics` sections where they
  apply.
- **`clippy::pedantic`** at warn, which CI also turns into an error.  A pedantic lint that is wrong
  for this codebase is relaxed in `[workspace.lints.clippy]` with a comment saying why, never with
  an `#[allow]` in the code.
- **rustfmt's defaults** settle formatting.  `rustfmt.toml` only pins the style edition, so that a
  toolchain bump cannot reformat the tree on its own.

## Licence headers

Every `.rs` file under a member's `src/` opens with exactly:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas
```

The year is the year the project started and is never updated.  Tests, benches and build scripts
carry no header, and vendored code keeps its own.  `crates/quaffed/tests/licence_headers.rs`
enforces it.

## CI

- `.github/workflows/premerge.yaml` runs on every pull request and every push to `main`; each job's
  name and comments say what it checks.
- `.github/workflows/package.yaml` builds the Linux package on every pull request (`release.md`).
- `.github/workflows/benchmarks.yaml` counts the front end's and the matcher's instructions against
  main's on every pull request, and the merged commit's after a merge (`performance.md`).
- `.github/workflows/record-measurements.yaml` posts each merge's budgets and benchmark counts to
  InfluxDB, and `catch-up-measurements.yaml` posts daily what it could not (`performance.md`).
- `.github/workflows/report-failures.yaml` posts a failed run to Slack.  A new workflow that runs on
  a schedule, a push, a release or a pull request is added there, as below.

**Every job is meant to be a required status check.**  Adding, renaming or removing one means
updating the branch ruleset in the same change, or the branch waits on a check that never reports,
or a new job gates nothing.  The exceptions:

- the review, which is advisory and does not approve the pull request;
- `Announce to Slack`, which runs only when a pull request opens, so never reports on a later push;
- the two `Benchmarks` jobs, whose alerts and refusals are for the reviewer to weigh
  (`performance.md`, under the benchmarks).

### Failed runs are posted to Slack

`.github/workflows/report-failures.yaml` posts a failed run of the workflows it names to the
project's Slack channel, through the shared reporter from `github-workflows`, whose README says
which runs it posts.  A run started by hand is left out, because whoever started it is watching.
To add a workflow that runs on a schedule, a push, a release, a pull request or another
workflow's run:

1. Give it a `name:`, and every job a `timeout-minutes:` (a job calling a reusable workflow
   cannot take one).
2. Add its `name:` to the `workflows:` list in `report-failures.yaml`, exactly as written; the
   repository hygiene check enforces the list.
3. If its trigger is not yet in that file's `events`, add it there, or its failures are not
   posted.

It posts through the `SLACK_BOT_TOKEN` secret.  **Accepted limitation:** a failed post is only a
failed *Report failures* run, so a dead token shows as an absence of posts.

`announce-pull-requests.yaml` announces pull requests through the shared announcer, whose README
says which.  `SLACK_BOT_TOKEN` must also be a Dependabot secret, or Dependabot's are not announced.

### Shared CI

Every `uses:` of `L337-org/github-workflows` runs the organisation's public shared workflows, at the
major-version tag it names.  What they do, and how to run the hygiene check locally, is in that
repository's README.  When and for whom this repository asks for a review is in the header of
`code-review.yaml`.

## Checklist: adding a workspace member

1. Create `crates/<name>/Cargo.toml` inheriting `version`, `edition`, `rust-version`, `license`,
   `repository` and `authors` from the workspace, with `publish = false` and
   `[lints] workspace = true`.
2. Give every file under its `src/` the licence header; the header test picks the new member up by
   itself.
3. Add it to the layout above and say what it owns.
4. If it changes which crate a reader should look in before changing something, add the row to the
   table in `AGENTS.md`.
