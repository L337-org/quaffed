# Verification

How the verification tooling is pinned and installed.  This note grows as the verification
infrastructure lands; for now it covers the tool manifest.

## The tool manifest

Most verification tools are binaries installed with `cargo install`, not crate dependencies,
so `Cargo.lock` does not pin them.  Left alone, CI would name its own versions and a local run
and CI would disagree about what clean means.  So every such tool is pinned in **`tools.toml`**
at the repository root, and **`scripts/install-tools`** is the one way to install one, locally
and in CI alike.  No workflow names a tool version.

```toml
# Crate name = exact version, for `cargo install --locked`.
[cargo-install]
cargo-insta = "1.48.0"

# A dated nightly per purpose, for the one job that needs it.
[toolchain]
nightly-coverage = "nightly-2026-10-01"

# Its components, space-separated.
[toolchain-components]
nightly-coverage = "llvm-tools-preview"
```

**The format is a strict subset of TOML** - section headers, `name = "value"` lines, whole-line
comments and blank lines - so that a shell script can read it without a TOML parser.  A line
outside the subset, a trailing comment included, is an error rather than skipped, because a
half-understood manifest would install something other than what it says.

**Entries are keyed by crate, not by command.**  They usually match; `flamegraph` is the crate
that provides `cargo flamegraph`.

**What is not in the manifest:**

- **Test libraries** (`proptest`, `insta` and the like) are dependencies, pinned with `=` in the
  `Cargo.toml` that uses them, and `Cargo.lock` holds them.
- **Operating-system packages** - Valgrind, the C++ compiler `cargo-fuzz` needs - come from the
  runner's or contributor's package manager.
- **A nightly that can be scoped by directory** is pinned in that directory's
  `rust-toolchain.toml` and installed with `rustup toolchain install` run from there, so it has
  one copy.  The fuzz targets' nightly is to be pinned that way, in `fuzz/rust-toolchain.toml`.
  `[toolchain]` is for a nightly that cannot be scoped, such as coverage's, which instruments
  the whole workspace.

## Installing

```bash
scripts/install-tools cargo-insta nightly-coverage   # the named entries
scripts/install-tools --cargo                        # every [cargo-install] entry
scripts/install-tools --all                          # everything, toolchains included
scripts/install-tools --list                         # the manifest as the script reads it
```

`cargo install` skips a crate already installed at the pinned version, so re-running is cheap,
and it installs into `CARGO_INSTALL_ROOT` when that is set.  The version is passed as `=X.Y.Z`,
because a bare version is a caret requirement to `cargo install` and would take a newer release.
A toolchain needs rustup; a Rust installed another way, such as Homebrew's, cannot install one,
and the script says so rather than falling back to whatever Rust is on PATH.

Any failure stops the script at once, naming the tool and version, with the underlying tool's
own output above it.  A job that carried on without its tool would run something else, or
nothing, and could still pass.

## In CI

A job installs what it uses with the local action, and nothing else:

```yaml
      - name: Install cargo-insta from tools.toml
        uses: ./.github/actions/install-tools
        with:
          tools: cargo-insta
```

The action installs cargo tools into `~/.quaffed-tools`, caches that directory and puts its
`bin` on `PATH`.  The cache key carries a hash of `tools.toml`, so a changed pin misses and builds
- from source, at one to two minutes a tool - while an unchanged tool is restored from an older
entry and skipped.  Toolchains are not cached; rustup installs a nightly in about half a minute.

The **Verification tools install** job in `premerge.yaml` installs every `[cargo-install]` entry
that way on Linux and checks each pinned version against cargo's own record of the install.  It
costs a cache restore until `tools.toml` changes, and then builds what changed, so a bad pin
fails on the pull request that introduces it rather than in the first job to use the tool.

## What the tests enforce

`crates/quaffed/tests/tool_manifest.rs`, in the ordinary test run:

- the manifest is well formed: exact `MAJOR.MINOR.PATCH` versions, nightlies pinned by date,
  each name once, components only for a listed toolchain, and at least one tool;
- the script reads the manifest exactly as the test does, compared through `--list`;
- the script installs the exact version, stops naming the tool and version when cargo or rustup
  fails, and says what is missing when cargo, rustup or a named entry is;
- no workflow or local action calls `cargo install`, `cargo binstall` or an install action,
  names a toolchain or a dated nightly, or names a version of a manifest tool;
- `gungraun-runner` in the manifest and `gungraun` in `Cargo.lock` are either both absent or
  both present at one version, because gungraun refuses to run against a runner of a different
  version.

Each rule has a test showing it fails on the thing it guards against.  The script's tests run
it under the oldest bash it has to support - macOS's 3.2 - with stub `cargo` and `rustup`
commands whose exit status and message were recorded from the real tools.

## Checklist: adding or changing a tool

1. Add the crate and its exact version under `[cargo-install]`, or change the version there.
   Nothing else names it.
2. If it has a matching library - `gungraun-runner` with `gungraun` - add or change both in the
   same change; the pair test fails otherwise.
3. If it needs a nightly that cannot be scoped by directory, add `nightly-<purpose>` under
   `[toolchain]` and its components under `[toolchain-components]`.
4. In the job that uses it, install it with `./.github/actions/install-tools`, naming only what
   that job uses.
5. Run `cargo test --locked --test tool_manifest`.  On the pull request, the **Verification tools
   install** job builds the new pin from source.
6. A new CI job is a new required status check: see `architecture/workspace.md`.
