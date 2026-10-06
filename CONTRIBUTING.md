# Contributing to quaffed

Contributions are welcome.  By participating, you are expected to uphold the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

**The design is not settled, and only textual search and textual scripts are implemented.**
Until it settles, the most useful contribution is discussion rather than code: open an issue
before starting anything beyond a small fix, so the approach can be agreed before you invest
time in it.

`AGENTS.md` in the repository root is the always-loaded brief for every assistant, and the map
of everything else: it carries the conventions and a table pointing at the deeper notes.  Those
notes live in `architecture/`, one file per area, and they are the canonical detail for people
and assistants alike.  This file covers the practical day-to-day: setup, testing, and
submitting a change.

## Local development

You need a Rust toolchain.  [rustup](https://rustup.rs) is recommended, because it reads
`rust-toolchain.toml` and installs exactly the version CI uses.  A Rust installed another way
ignores that file, so check `rustc --version` against it; see
[architecture/workspace.md](architecture/workspace.md).

The tests also need `python3` on your `PATH`, at 3.12 or later: some of them run
`scripts/fetch-corpus`, which is Python.  Fetching the real-world corpus itself needs the exact
CPython release named in `corpus/real-world.toml`; see
[architecture/verification.md](architecture/verification.md).

```bash
cargo build                                        # build
cargo run -- <args>                                # run quaff
cargo test --workspace --locked                    # all tests
cargo clippy --all-targets --locked -- -D warnings # lint, as CI runs it
cargo fmt --all                                    # format
```

The verification tools - coverage, mutation testing, snapshot review and the rest - are not
crate dependencies.  Install the ones you need at the versions CI uses with
`scripts/install-tools` (`--all` for every one); [architecture/verification.md](architecture/verification.md)
has the details.  A tool at another version can disagree with CI about what clean means.

Add a dependency with `cargo add`, and commit the updated `Cargo.lock` alongside `Cargo.toml`:
CI builds `--locked` and fails if the two disagree rather than silently re-locking.  A new
dependency's licence must be compatible with GPL-3.0-or-later, and a dependency the binary links
must be named in the Linux package's copyright file - a test says so if it is not; see
[architecture/release.md](architecture/release.md).

## Conventions

- **Formatting is rustfmt's**, with its default line length.  Do not argue style; run
  `cargo fmt`.
- **clippy pedantic is on, and CI treats every warning as an error.**  A pedantic lint that is
  wrong for this codebase is relaxed once in the root `Cargo.toml` with a comment saying why,
  not with `#[allow]` scattered through the code.
- **Every public item has a doc comment** saying what it does, for somebody who will never read
  its body, with `# Errors` and `# Panics` sections where they apply.  A code comment says why
  the code is the way it is; it does not restate the line below it.
- **Every product source file opens with the licence header** - see
  [architecture/workspace.md](architecture/workspace.md).  A test fails if one is missing.
- **Prose is British English in plain ASCII punctuation** everywhere it ships: documentation,
  comments, commit messages and pull request descriptions.  Hyphens rather than dashes, straight
  quotes, and three full stops rather than an ellipsis character.
- **A commit message says why**, not only what.  The diff already says what.

## Testing

- Unit tests sit beside the code in a `#[cfg(test)]` module; integration tests go under the
  crate's `tests/`, one file per concern.
- Test the failure paths as well as the happy path - malformed input, a missing file, permission
  denied - and assert what a failure *says*, not only that it happened.
- What `quaff` prints and how it exits is recorded in reviewed snapshots.  A change to either
  fails until you review the snapshot with `cargo insta review` (`cargo-insta` comes from
  `scripts/install-tools`); [architecture/verification.md](architecture/verification.md) says how
  to add an end-to-end test.
- A green suite shows the code behaves as the tests assert.  Before calling a feature working,
  run the real binary against a real file.

## Developer Certificate of Origin

Contributions are accepted under the [Developer Certificate of Origin](https://developercertificate.org/)
rather than a contributor licence agreement.  It costs nothing: add a sign-off line to every
commit, certifying that you wrote the change or otherwise have the right to submit it under the
project's licence.

```bash
git commit -s
```

That appends `Signed-off-by: Your Name <you@example.com>`, using your configured name and email.
CI checks every commit on a pull request for a sign-off carrying the same email address the
commit is authored under, so a sign-off typed by hand, or added under another identity, fails.

If you forgot, sign off every commit on your branch at once and force-push:

```bash
git rebase --signoff main
git push --force-with-lease
```

## Submitting your change

CI (`.github/workflows/premerge.yaml`) runs on every pull request and every push to `main`, and
every job must pass before merging: the repository hygiene check, the action-pin check, the
sign-off check (pull requests only), rustfmt, clippy, the verification tools install, the
tests on Linux and macOS, and the real-world corpus fetch.  `.github/workflows/package.yaml` also runs on every pull request and
must pass: it builds the Linux package for amd64 and arm64 and checks it with lintian - see
[architecture/release.md](architecture/release.md).

Keep pull requests focused: one logical change per pull request is easier to review than a
bundle of unrelated fixes.  A change that alters behaviour carries its tests and its
documentation in the same pull request.

## Reporting issues

Bug reports and feature requests have templates you can choose when you
[create an issue](https://github.com/L337-org/quaffed/issues/new/choose).  Security issues are
not reported as issues; see [SECURITY.md](SECURITY.md).
