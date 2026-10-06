# Verification

How quaffed's own behaviour is verified, and how the verification tooling is pinned and
installed.  This note grows as the verification infrastructure lands; for now it covers the
end-to-end tests, the two Python corpora and the tool manifest.

## End-to-end tests and snapshots

`crates/quaffed/tests/end_to_end.rs` runs the built `quaff` binary as a user runs it, in a
throwaway project directory, and records what it printed and how it exited as a snapshot with
`insta` and `insta-cmd`.  The snapshot holds the exit status, standard output and standard
error, so a changed message or exit code fails until somebody reviews it, and the reviewed
change shows on the pull request as a diff of a `.snap` file under `tests/snapshots/`.  Every
diagnostic `quaff` can print, and `--help`, has one.

**The version is filtered out of snapshots**, shown as `[VERSION]`, because it is declared in one
place and a snapshot recording it would be a second copy every version bump had to re-accept.
`the_version_it_reports_is_the_workspace_version` checks the real value against
`CARGO_PKG_VERSION` instead.

**The binary is `CARGO_BIN_EXE_quaff`**, which cargo sets only when the crate has that binary, so
a missing binary fails to compile rather than skipping; if the file is gone at run time the
harness fails naming the path.  **The command's environment is cleared** and it runs from the
project's root, so nothing from the machine running the tests reaches the binary, and arguments
should be relative paths: the snapshot records them.

### Adding an end-to-end test

1. Build the project the scenario needs with `Project::new(&[(path, content), ...])`.
2. Run `assert_quaff_snapshot!(project.quaff(&[args...]))`, which snapshots the result with the
   version filtered out.
3. Check the files left behind: `project.assert_unchanged()` for anything that must not edit,
   and assertions on the edited content for anything that must.  `assert_unchanged` compares each
   file's content and permission bits, each directory, and each symbolic link's target, without
   following links, so a rename that drops the executable bit or replaces a link with a file is
   caught.  It uses Unix permissions, so like the tool-manifest tests it is Unix-only.
4. Run `cargo insta test`, read the new snapshot in `cargo insta review`, and accept it only if it
   says what `quaff` should say.  A snapshot accepted unread asserts whatever the code happened to
   print.
5. Commit the `.snap` file with the test.

### Reviewing a changed snapshot

`cargo insta test` stores a changed result beside the old one as `.snap.new`, and
`cargo insta review` shows each as a diff to accept or reject; `cargo insta reject` drops them
all.  In CI, where `CI` is set, a changed or new snapshot fails the run and nothing is written.

CI runs `cargo insta test --workspace --check --unreferenced=reject` after
`cargo test --workspace --locked`, so a snapshot file that no test produces - a test renamed or
deleted without its snapshot - also fails the run.  The `cargo test` step is the one that enforces
`Cargo.lock`: `cargo insta test` passes arguments after `--` to the test binaries rather than to
cargo, whatever its help says, so it cannot take `--locked`.

## The synthetic corpus

`corpus/synthetic/` is Python written to contain every construct quaff's Python front end must
handle.  It is the primary input to the invariant harness, the budgets and the benchmarks as
they land, and it is checked in, so it needs no fetching and carries no third-party licence.

- **`modules/*.py`** are parsed as modules: a file's worth of statements.  Each file has a
  theme - statements, expressions, strings, match patterns, type parameters, non-ASCII text,
  pragmas, indentation, line endings, a byte-order mark - so that a gap points at a file.
- **`expressions/*.pyexpr`** are parsed as a single expression, the only way to produce Ruff's
  `ModExpression`, and the shape many structural patterns take.
- **`ipython/*.ipy`** are parsed as `IPython` source: statements that may include its escape
  commands - `%timeit`, `!ls`, `?len` - which IPython and Jupyter notebooks run before handing
  the rest to Python.  They are not Python, so CPython refuses these files, and anything that
  judges the corpus by CPython takes this directory as its own, without the oracle.  quaff will
  need to edit notebooks; until it does, these files exist so that Ruff's two escape-command
  kinds are covered like every other.

**Its bytes are its point**, so `.gitattributes` marks it `-text`: git stores and checks out
every file exactly, CRLF and lone-CR line endings and the byte-order mark included.

`crates/python/tests/corpus.rs` checks it in the ordinary test run:

- **Every kind of node in the grammar appears**, taken from the adapter's `all_kinds()` and so
  held to Ruff's own enum (`python.md`).  It fails naming each kind no file produces; removing
  `patterns.py` was shown to fail it with the twelve match-statement kinds.  There are no
  exceptions: every kind Ruff lists comes from some file.
- **Every layout form is there byte for byte**: a UTF-8 byte-order mark, LF, CRLF and a lone CR
  in one file, tab and space indentation, non-ASCII text and a `# noqa` pragma.
- **Every file parses**, and a file that does not fails the run naming it and Ruff's message.
- **Fewer files than the floor fails**, saying how many it found, so an emptied or missing
  directory cannot pass by checking nothing.

Every module and expression file is also accepted by CPython 3.14's `ast.parse`, in the
matching mode; the `IPython` files are not Python, so CPython refuses them.

### Adding to the synthetic corpus

1. Add the construct to the file whose theme it fits, or a new themed file under `modules/`,
   `expressions/` for a single expression, or `ipython/` for an `IPython` escape command.
2. Keep it valid Python: check it with `python3.14 -c "import ast,sys; ast.parse(open(sys.argv[1],'rb').read(), sys.argv[1])" FILE`,
   adding `mode='eval'` for an expression.
3. Run `cargo test -p quaffed-python --test corpus`.
4. A file kept for its bytes - line endings, a byte-order mark, tabs - must be written with
   those bytes, not through an editor that normalises them; check with `od -c`.

## The real-world corpus

Real Python, as a check on what the synthetic corpus did not think to write: **CPython's own
`Lib/test` at the release tag matching the oracle**, `v3.14.8`, about 1150 files written to
exercise the language.  It is **used for round-tripping only**, never by a gate, a budget or a
benchmark, so its contents cannot move one.  Nothing that gates reads `target/corpus/`.

It is **fetched, not checked in**: it is CPython's code, under the PSF licence, and far larger
than the rest of the repository.  `corpus/real-world.toml` pins it, and is the one place that
names it:

- **the archive**, python.org's source tarball for the release, by URL and SHA-256.  Not
  GitHub's archive of the tag: GitHub generates those on request, and their bytes have changed
  before.  The two hold identical `Lib/test` trees, checked when it was pinned;
- **the path** extracted from it, `Lib/test`, under the archive's top directory;
- **the oracle**, by exact version: the CPython whose `ast.parse` says which files are valid
  Python.  Pinned to the release, never `main`, which carries syntax the release refuses;
- **the floor**: fewer `.py` files than this means the fetch or the walk is broken.

### Fetching it

```bash
python3.14 scripts/fetch-corpus     # into target/corpus/cpython/
```

**One command, run with the oracle's Python**; any other version, or a Python that is not
CPython, stops it, naming the one it needs.  It downloads the archive and refuses it unless the SHA-256 matches, extracts only the
named path with Python's `data` filter - which refuses any member that would land outside the
directory, by an absolute path, `..` or a link - and runs the oracle over every `.py` file.  It
writes `refused.txt` beside the files, one line per refused file with the oracle's reason, and
prints the counts and the refused files.  At `v3.14.8` that is 1150 files, 1146 accepted, and
four refused, all encoding or tokenizer failures in `Lib/test/tokenizedata/`.  **Nothing is
filtered out**: every file stays in the tree, and a refused one is listed, so a consumer must
expect the oracle's refusal of exactly those files rather than skip them.

The tree replaces an earlier one only once it is complete, by renames alone, so an interrupted
run never leaves a partly replaced tree that looks finished; a failed refetch keeps the
previous tree.  A fetch is reused only while everything that shaped it is unchanged - the
source's whole entry in the manifest, and the script - which is also what CI's cache key covers.

**The refused list is not committed.**  It can only change when the pin does, and it comes from
running the oracle, so it is generated with the files rather than kept in step by hand.

### In CI

The **Real-world corpus** job reads the oracle's version from the manifest, installs that
Python with `actions/setup-python`, restores `target/corpus` from a cache keyed on the manifest
and the script, and runs `scripts/fetch-corpus`.  `crates/quaffed/tests/fetch_corpus.rs` tests the
script itself in the ordinary run, against small archives it builds: only the path is
extracted, a changed hash is refused, a member or a link escaping the directory is refused, the
floor fails saying how many it read, another Python is refused, a completed fetch is reused, a
changed pin is fetched again, a failed download leaves nothing behind, and a failed refetch
keeps the previous tree.

### Checklist: moving the pin to a new CPython release

1. Change `tag`, `archive`, `root` and `oracle` in `corpus/real-world.toml` together.
2. Download the new tarball, and set `sha256` from it.
3. Run `scripts/fetch-corpus` with the new Python, and read the refused files it prints: each
   should be a deliberate fixture, not a file the parser should handle.
4. Raise `floor` if the release has many more files, keeping it below the count, and update the
   count in the comment beside it.
5. Update the release everywhere else it is named, in the same change: the counts, the tag and
   the `python3.X` command in this section, and the fetch command in `AGENTS.md`.  The script
   names no release; its refusal says which Python it needs.

## The tool manifest

Most verification tools are binaries installed with `cargo install`, not crate dependencies,
so `Cargo.lock` does not pin them.  Left alone, CI would name its own versions and a local run
and CI would disagree about what clean means.  So every such tool is pinned in **`tools.toml`**
at the repository root, and **`scripts/install-tools`** is the one way to install one, locally
and in CI alike.  No workflow names a tool version.

```toml
# Crate name = exact version, for `cargo install --locked`.
[cargo-install]
cargo-insta = "X.Y.Z"

# A dated nightly per purpose, for the one job that needs it.
[toolchain]
nightly-coverage = "nightly-YYYY-MM-DD"

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
and it installs into `CARGO_INSTALL_ROOT` when that is set.  Run from the repository, it builds
each tool with the toolchain `rust-toolchain.toml` pins rather than rustup's default, so a tool
builds the same everywhere; cargo warns that the default toolchain was overridden, and that is
expected.  The version is passed as `=X.Y.Z`,
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

Each rule has a test showing it fails on the thing it guards against, and every malformed
manifest the test's parser refuses is shown to be refused by the script at the same line.  The
script's tests run it under the oldest bash it has to support - macOS's 3.2 - with stub `cargo`
and `rustup` commands whose exit status and message were recorded from the real tools.

**These tests are Unix-only**: they run a bash script and set Unix file permissions.  When
Windows joins the test matrix, how the tools install there needs deciding, rather than these
tests being compiled out, which would report a pass having checked nothing.

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
