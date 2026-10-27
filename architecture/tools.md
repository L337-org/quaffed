# Verification tools

How the verification tools are pinned and installed, locally and in CI.

## The tool manifest

Verification tools installed with `cargo install` are not pinned by `Cargo.lock`, so every one is
pinned in **`tools.toml`**, and **`scripts/install-tools`** is the one way to install one, locally
and in CI.  No workflow names a tool version.

```toml
[cargo-install]
cargo-insta = "X.Y.Z"           # crate = exact version

[toolchain]
nightly-coverage = "nightly-YYYY-MM-DD"

[toolchain-components]
nightly-coverage = "llvm-tools-preview"
```

**The format is a strict subset of TOML**, so a shell script can read it without a parser; a line
outside it, a trailing comment included, is an error, because a half-understood manifest would
install something else.  Entries are keyed by crate, not command (`flamegraph` provides
`cargo flamegraph`).

Not in the manifest: test libraries, which are `=`-pinned dependencies; operating-system packages
such as Valgrind; and a nightly that can be scoped by directory, which that directory's
`rust-toolchain.toml` pins.  `[toolchain]` is for one that cannot, such as coverage's.

## Installing

```bash
scripts/install-tools cargo-insta nightly-coverage   # the named entries
scripts/install-tools --cargo                        # every [cargo-install] entry
scripts/install-tools --all                          # everything, toolchains included
scripts/install-tools --list                         # the manifest as the script reads it
```

It installs `=X.Y.Z`, since a bare version would take a newer release, builds with the pinned
toolchain, and needs rustup for toolchains.  Any failure stops it, naming the tool and version,
because a job without its tool could still pass.

## In CI

A job installs only what it uses, with `./.github/actions/install-tools` (`tools:` names them),
which caches `~/.quaffed-tools` keyed on `tools.toml`.  The **Verification tools install** job
installs every `[cargo-install]` entry and checks each version, so a bad pin fails on the pull
request that introduces it.

**Packages from apt go through `.github/actions/apt-install`**, which caches the downloaded `.deb`
files.  The cache changes how fast a package arrives, never which version, and each restored file
is checked against the SHA-256 the signed index lists before it is installed, since apt checks
only the size of a file already in its archive.

## What the tests enforce

`crates/quaffed/tests/tool_manifest.rs`, in the ordinary run: the manifest is well formed; the
script reads it exactly as the test does; no workflow or local action installs a tool another way,
names a toolchain or a manifest tool's version, or installs from apt except `apt-install`; and
`gungraun-runner` and `gungraun` are pinned to one version, because gungraun refuses a mismatched
runner.  The script is tested under macOS's bash 3.2, the oldest it supports.

**These tests are Unix-only.**  When Windows joins the test matrix, how tools install there needs
deciding, rather than these tests being compiled out, which would pass having checked nothing.

## Checklist: adding or changing a tool

1. Add the crate and its exact version under `[cargo-install]`, or change it there.  Nothing else
   names it.
2. If it has a matching library - `gungraun-runner` with `gungraun` - change both together.
3. If it needs a nightly that cannot be scoped by directory, add `nightly-<purpose>` under
   `[toolchain]` and its components under `[toolchain-components]`.
4. In the job that uses it, install it with `./.github/actions/install-tools`, naming only what
   that job uses.
5. Run `cargo test --locked --test tool_manifest`.
6. A new CI job is a new required status check: see `workspace.md`.
