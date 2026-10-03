# AGENTS.md

The shared instruction file for this repository.  Every assistant reads this one; `CLAUDE.md`
and `.github/copilot-instructions.md` are pointers to it.

**quaffed**, the QUAntiFied File EDitor, is an editing language for making mechanical code
changes structural rather than textual, with declared cardinality and assertions, so that an
edit either happens exactly as specified or does not happen at all.  The binary is `quaff`.
It is written in Rust and licensed GPL-3.0-or-later.

## Status

**Textual search is built; nothing else is yet.**  `quaff TEXT [PATH]` and `quaff -s TEXT [PATH]`
search a project, and `architecture/search.md` specifies them as built.  The operation
representation that scripts are parsed into is specified in `architecture/representation.md`,
and the script language's parser in `architecture/script.md`, but the command line does not
run scripts yet.  Scripts, structural search and
edits are not built, and the rest of the design is still being settled.

- **Do not implement features from a recollection of the design.**  When a piece of it settles,
  its specification lands in `architecture/` in the same change as the code, and that is what
  to build against.

## This repository will be public

It is private today and will be made public before the first release, so everything
committed now becomes public history.  **Treat it as public already:** no tracker issue key
and no link into the internal wiki or tracker in code, documentation, a commit message or a
pull request description.  Describe the work instead.  The shared hygiene check that CI runs
fails on either.

## Read these before changing the matching area

| Before changing | Read |
|---|---|
| `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, anything under `crates/` structurally, or `.github/workflows/` | [architecture/workspace.md](architecture/workspace.md) |
| adding a workspace member | the checklist in [architecture/workspace.md](architecture/workspace.md) |
| `tools.toml`, `scripts/install-tools` or `.github/actions/install-tools`, or adding a verification tool | [architecture/verification.md](architecture/verification.md) |
| an end-to-end test, or anything that changes what `quaff` prints or how it exits | the snapshot sections of [architecture/verification.md](architecture/verification.md) |
| `crates/quaffed/src/`: the command line, discovery, encodings, matching or output | [architecture/search.md](architecture/search.md) |
| `crates/representation/`: what a program means, the checker, or its lint configuration | [architecture/representation.md](architecture/representation.md) |
| `crates/script/`: the script language's operands, grammar, errors or locations | [architecture/script.md](architecture/script.md) |
| `packaging/`, `scripts/build-deb`, the package or release workflows, or a dependency the binary links | [architecture/release.md](architecture/release.md) |

## Commands

```bash
cargo build                                        # build
cargo run -- <args>                                # run quaff
cargo test --workspace --locked                    # all tests
cargo test --locked --test licence_headers         # one integration test file
cargo clippy --all-targets --locked -- -D warnings # lint, as CI runs it
cargo fmt --all                                    # format
cargo fmt --all -- --check                         # format check, as CI runs it
scripts/install-tools --all                        # every verification tool, at its pinned version
```

CI builds `--locked`, so a dependency change must commit `Cargo.lock` alongside `Cargo.toml`.

## Conventions

CI enforces the licence header on product source, `forbid(unsafe_code)`, `missing_docs` and
clippy pedantic; [architecture/workspace.md](architecture/workspace.md) has the header text
and the reasoning.  These are not enforced, so they are carried here:

- **A pedantic lint that is wrong here is relaxed in `[workspace.lints.clippy]`** with a
  reason, never with a scattered `#[allow]`.
- **One version, in the root `Cargo.toml`.**  Nothing else states it.
- **Every commit is signed off** (`git commit -s`) under the Developer Certificate of Origin; see
  `CONTRIBUTING.md`.
- **Prose is British English in plain ASCII punctuation** - hyphens rather than dashes, straight
  quotes, three full stops rather than an ellipsis character - in documentation, comments,
  commit messages and pull request descriptions.

<!-- BEGIN GENERATED -->
## Read these when they apply

- Read `.agents/policy/review-context.md` always - these apply to every activity.
- Read `.agents/policy/testing.md` when writing or running tests, or adding behaviour that needs them.
- Read `.agents/policy/architecture.md` when changing module structure, public surface, docstrings, generated files, deprecation, or log levels.

<!-- END GENERATED -->
