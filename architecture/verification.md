# Verification

How quaffed's own behaviour is verified, and how the verification tooling is pinned and
installed.

## End-to-end tests and snapshots

`crates/quaffed/tests/end_to_end.rs` runs the built `quaff` binary as a user runs it, in a
throwaway project directory, and snapshots its exit status, standard output and standard error
with `insta` and `insta-cmd`.  A changed message or exit code fails until somebody reviews it, and
shows on the pull request as a diff of a `.snap` file.  Every diagnostic `quaff` can print, and
`--help`, has one.

- **The version is filtered out**, as `[VERSION]`, so a version bump re-accepts nothing;
  `the_version_it_reports_is_the_workspace_version` checks the real value instead.
- **The command's environment is cleared** and it runs from the project's root, so nothing from
  the machine reaches the output; pass relative paths, since the snapshot records them.
  `LLVM_PROFILE_FILE` alone is passed through, for coverage.

### Adding an end-to-end test

1. Build the project the scenario needs with `Project::new(&[(path, content), ...])`.
2. Run `assert_quaff_snapshot!(project.quaff(&[args...]))`.
3. Check the files left behind: `project.assert_unchanged()` for anything that must not edit
   (it compares content, permission bits and link targets, so it is Unix-only), and assertions
   on the edited content for anything that must.
4. Run `cargo insta test`, read the new snapshot in `cargo insta review`, and accept it only if it
   says what `quaff` should say.  A snapshot accepted unread asserts whatever the code printed.
5. Commit the `.snap` file with the test.

### Reviewing a changed snapshot

`cargo insta test` writes a changed result as `.snap.new`; `cargo insta review` shows each as a
diff to accept or reject, and `cargo insta reject` drops them all.  In CI a changed or new
snapshot fails the run.  CI also runs `cargo insta test --workspace --check
--unreferenced=reject`, so a snapshot no test produces fails; `cargo test --locked` runs
separately because `cargo insta test` cannot take `--locked`.

## The synthetic corpus

`corpus/synthetic/` is Python written to contain every construct quaff's Python front end must
handle.  It is the only input to the budgets and benchmarks, and it is checked in.

- **`modules/*.py`** are parsed as modules, one theme per file, so that a gap points at a file.
- **`expressions/*.pyexpr`** are parsed as a single expression, the only way to produce Ruff's
  `ModExpression`.
- **`ipython/*.ipy`** are `IPython` source with escape commands (`%timeit`, `!ls`).  They are not
  Python, so nothing judges them by CPython, but they cover Ruff's escape-command kinds.

**Its bytes are its point**, so `.gitattributes` marks it `-text`, keeping CRLF, lone-CR line
endings and the byte-order mark exactly.

`crates/python/tests/corpus.rs` requires every node kind in `all_kinds()` (`python.md`) to appear,
with no exceptions, every layout form to be present byte for byte, every file to parse, and at
least the floor of files.  `scripts/check-synthetic`, in the **Real-world corpus** CI job, requires
every module and expression file to be valid Python to the oracle.

### Adding to the synthetic corpus

1. Add the construct to the file whose theme it fits, or a new themed file under `modules/`,
   `expressions/` or `ipython/`.
2. Keep it valid Python: run `scripts/check-synthetic` with the oracle's Python, the release
   `corpus/real-world.toml` names.
3. Run `cargo test -p quaffed-python`, which runs the coverage check and the docker-mcp exclusion
   below.
4. A file kept for its bytes must be written with those bytes, not through an editor that
   normalises them; check with `od -c`.

## The real-world corpus

**CPython's own `Lib/test` at the release tag matching the oracle**: real Python, as a check on
what the synthetic corpus did not think to write.  It is used for the round trip and the
structural counts, and for profiling by hand, **never by a budget or a benchmark**, so it cannot
move a measured number.

It is **fetched, not checked in**: it is under the PSF licence and large.  `corpus/real-world.toml`
pins it and is the one place that names it: python.org's tarball by URL and SHA-256 (not GitHub's
generated archive, whose bytes can change), the path in it, the oracle by exact version (a
release, never `main`), and a floor of `.py` files below which the fetch counts as broken.

### Fetching it

```bash
python3.X scripts/fetch-corpus      # 3.X: the oracle in corpus/real-world.toml
```

Run it with the oracle's Python; any other stops it.  It verifies the hash, extracts only the
named path safely, runs the oracle over every file and writes `refused.txt`, one line per refused
file with the reason.  **Nothing is filtered out**: a consumer must expect exactly those files to
be refused rather than skip them.  A fetch is replaced only once complete, and reused while the
manifest entry and the script are unchanged.  `refused.txt` is generated, never committed.

### In CI

The **Real-world corpus** job in `premerge.yaml` runs `scripts/check-synthetic` and
`scripts/fetch-corpus`, then the round trip, `scripts/count-structural` and the structural counts.
`crates/quaffed/tests/fetch_corpus.rs` and `check_synthetic.rs` test the scripts themselves in the
ordinary run.

### Checklist: moving the pin to a new CPython release

1. Change `tag`, `archive`, `root` and `oracle` in `corpus/real-world.toml` together.
2. Download the new tarball, and set `sha256` from it.
3. Run `scripts/fetch-corpus` with the new Python, and read the refused files it prints: each
   should be a deliberate fixture.  Run `scripts/check-synthetic` with it too.
4. Raise `floor` if the release has many more files, keeping it below the count printed.
5. Run the round trip, `cargo test -p quaffed-python --test round_trip -- --ignored`.  Syntax the
   new oracle accepts and the pinned Ruff refuses means upgrading Ruff first, by the checklist in
   `python.md`.

## The round trip

`crates/python/tests/round_trip.rs` holds the Python front end to two promises: **it loses no
byte, and it agrees with CPython on what parses.**  It decodes each file as quaff does
(`encoding.md`), parses it into an arena (`syntax.md`), prints the arena back and encodes it, and
requires the original bytes.

- **The synthetic corpus** runs in the ordinary test run; every file must round-trip.
- **The real-world corpus** is `#[ignore]`d, and the **Real-world corpus** job runs it with
  `--ignored`.  Every file the oracle accepts must round-trip, and **every file in `refused.txt`
  must be refused**: the oracle decides, never our parser.  The job's step fails unless exactly
  one test ran, so adding a second ignored test to the file means changing that count.
- **It cannot pass having read nothing**: a group below its floor, a count differing from the
  oracle's, or a missing fetch fails.

It does not prove that every construct is a node, since text an adapter leaves out still prints
back; the synthetic corpus's coverage check requires every kind.

```bash
python3.X scripts/fetch-corpus        # 3.X: the oracle in corpus/real-world.toml
cargo test -p quaffed-python --test round_trip -- --ignored
```

## The structural counts

`crates/python/tests/real_world_matches.rs` holds structural matching to **an independent count
made by CPython's own parser**.  `scripts/count-structural`, run with the oracle's Python, counts
each pattern in its `PATTERNS` by a predicate over `ast`'s tree written for that pattern alone,
and writes `structural.txt`.  The test matches the same patterns over the same files and requires
the same count per pattern per file.  It is ignored in the ordinary run and run like the round
trip, with the same exactly-one-test check.

**A disagreement is a defect in quaff or in a predicate**: list both sides' matches by file and
line, `quaff -p` over the tree with the refused files in a `.ignore` against the predicate's walk.

### Checklist: adding a pattern to the structural counts

1. Add the pattern to `PATTERNS` in `scripts/count-structural`, with a predicate written from the
   language reference rather than from quaff's matcher.  Compare trees field by field, not
   through `ast.dump`, whose text differs between releases.
2. Run the script with the oracle's Python, then `cargo test -p quaffed-python --test
   real_world_matches -- --ignored`.
3. If the counts differ, list both sides' matches and decide which is wrong.  A predicate changed
   to agree with quaff checks nothing.

## docker-mcp is never in either corpus

docker-mcp is the repository quaff's first version is to be trialled on, so code from it in either
corpus would tune quaff against what it is then judged by.
`crates/python/tests/corpus_exclusion.rs` fails if a non-comment line of `corpus/real-world.toml`,
or any synthetic file's path or content, names `docker-mcp` or `docker_mcp`.  It cannot catch a
renamed mirror or rewritten code; the corpus is written, not copied, and a new source in the
manifest is reviewed.

## The coverage ratchet

A pull request that lowers line or region coverage against main, with more of it uncovered than
on main, fails the **Coverage ratchet** (`.github/workflows/coverage.yaml`).  It is a floor, not a
target.

- **Both sides are measured in the same run**: the pull request merged into main, and main as that
  merge's first parent, from a second worktree with the pull request's own `scripts/coverage`.  No
  figure is stored, so none can drift.
- **`scripts/coverage-ratchet` compares exact fractions**, for the metrics in its `GATED` list.  A
  share that falls with no more uncovered, as when well-tested code is deleted, passes.
- **Condition coverage is reported only**, because it needs an unstable flag and so runs on the
  pinned nightly.  It joins `GATED` when the flag is stable, which the toolchain-upgrade checklist
  in `workspace.md` checks.
- **The measurement is deterministic**: `scripts/coverage` fixes proptest's seed, and the pull
  request is measured twice on separate runners, failing if the two differ.  A difference means
  a non-deterministic test, in the file named.
- **No figure fails rather than passes**: a missing summary or a metric counting nothing fails.

```bash
scripts/install-tools cargo-llvm-cov nightly-coverage   # once
scripts/coverage stable target/coverage-stable.json
scripts/coverage condition target/coverage-condition.json
```

To compare with another checkout, measure it the same way, giving its path as the third argument,
and pass both to `scripts/coverage-ratchet`.

## Profiling

When a gate flags a regression, a flamegraph shows where the time goes.  The `profiling` Cargo
profile is the shipped `dist` build with full debug information, so every frame is named.

```bash
scripts/install-tools flamegraph    # once
cargo flamegraph --profile profiling --bin quaff -o target/flamegraph.svg \
    -- 'def ' target/corpus/cpython/Lib/test > target/flamegraph.out
```

- **On Linux** it needs `perf` (`linux-perf`), and `--root` where `perf_event_paranoid` refuses an
  unprivileged user; in a container, `--privileged`.  **On macOS** it needs Xcode; if `xctrace`
  fails with *"Failed stoping ktrace session"*, repeat the run.
- **To profile the package's own build** (musl, mimalloc), add `--target` with the package's
  triple and leave out `--bin quaff`, which `cargo flamegraph` refuses alongside `--target`.
- **Send standard output to a file**, as a user's pipe would; the terminal's speed is not quaff's.
- **An `[unknown]` frame inside quaff's code** means debug information is missing: check the
  profile.
- **Clean up** `perf.data` and `perf.data.old` (root-owned under `--root`), or
  `cargo-flamegraph.trace` on macOS after a failed run; `.gitignore` covers all three.

## The budgets

The work the shipped build does over the synthetic corpus is held to budgets on every pull
request, by the **Budgets** job in `premerge.yaml`.  **Nothing is timed**: hosted runners vary in
speed, so a time budget fails on noise; each budget counts something the runner's speed cannot
move.

- **What is measured** is the amd64 musl binary built with the `profiling` profile, on the
  runner image `runs-on:` names, not `ubuntu-latest`, because Valgrind comes from the image and
  its version moves the counts.
- **The scenarios are `SCENARIOS` in `scripts/budgets`**, each in a scratch copy of the synthetic
  corpus, always under `/tmp` at the same depth, because a deeper path executes more instructions.
- **The limits are `budgets.toml`**: `instructions` (Callgrind) and `peak_memory_kib` (GNU time)
  with a margin from `MARGINS`, and `paths_opened` and `bytes_read` (strace) exactly, since a
  change in either is a change in behaviour.  No path in the project may be opened twice, and
  nothing outside it but what `[outside]` allows, each with its reason.
- **Each scenario runs `--repeat` times** and is judged on its worst run; the file access must be
  identical every time.  Anything that could not be measured fails, never passes.
- **Every run's traces are kept** in the job's `budgets` artefact: what a reviewer reads to judge
  whether a moved budget is still correct behaviour.

**The guard has a negative test on every run**: the job fails unless `scripts/budgets` fails on
each broken copy `scripts/break-budgets` builds, naming the budget that guards it.

Locally it needs amd64 Linux with Valgrind, strace, GNU time and the musl target; elsewhere, a
container of the job's image:

```bash
docker run --rm -it --ulimit nofile=1024:1024 \
  -v "$PWD":/src -w /src ubuntu:<the runs-on release> bash
# inside: apt-get update && apt-get install -y valgrind strace time musl-tools curl build-essential
#         python3 git, then rustup with the musl target, then:
cargo build --locked --profile profiling --target x86_64-unknown-linux-musl --bin quaff
scripts/budgets target/x86_64-unknown-linux-musl/profiling/quaff --repeat 3
```

### Checklist: changing a limit

A limit changes only in the pull request that moves it, and only once the new behaviour is
agreed to be correct.

1. Read the failing job's summary, and download the `budgets` artefact.
2. Compare the trace with main's latest artefact: `callgrind_annotate` the judged runs' profiles
   for instructions, or diff the `.files.txt` lists for files.
3. Decide whether that is what the change should do.  If not, fix the code, not the limit.
4. If it is, set the limit in `budgets.toml` to the summary's *Limit to set*.
5. Say in the pull request description which budget moved, by how much, and why.

## The benchmarks

The benchmarks hold the Python front end and the matcher, the parts most likely to grow, closer
than the budgets.  `.github/workflows/benchmarks.yaml` counts instructions with gungraun under
Callgrind on every pull request, on amd64 and arm64.

- **What is counted is `crates/python/benches/front_end.rs`**, over the synthetic corpus only:
  Ruff's parse alone, the parse with the arena, compiling a pattern, and matching.  Everything
  else is setup, which gungraun does not count, and each benchmark hands its input back so that
  freeing it is not counted.
- **They build with the `benchmarks` profile**: the shipped optimisation with symbols but **no
  debug information**, because with any, line tables included, some counts on arm64 collapse to a
  fraction of the work.  `LEAST_COUNT` in `scripts/benchmark-gate` refuses such a count.
- **Main is counted in the same job**, at the merge's first parent, because a count moves with the
  runner image and Valgrind; each architecture is compared with itself, and each tree uses the
  runner version its own `tools.toml` pins.
- **A rise is an alert, not a gate**: a benchmark more than `RULE_PERCENT` above main is flagged in
  the summary and as a warning, and the job passes; the reviewer judges it.
- **The arena's budget**: building the arena may cost at most `ARENA_BUDGET_PERCENT` of Ruff's
  parse, or the job fails, so that a slower arena cannot hide inside a faster parse.
- **A gate that could not compare has not passed**: the refusals are in `scripts/benchmark-gate`,
  and fail the job as not evaluated.
- **Whether the jobs are required checks** is in `workspace.md`.

**The gate has a negative test on every run**, on amd64: the job fails unless the matcher slowed
by `scripts/break-benchmarks` raises an alert and passes, and the arena slowed fails the budget.

Locally they need Linux with Valgrind and gungraun's runner at the pinned version; elsewhere:

```bash
docker run --rm -it --security-opt seccomp=unconfined --ulimit nofile=1024:1024 \
  -v "$PWD":/src -w /src rust:<rust-toolchain.toml's channel> bash
# inside: apt-get update && apt-get install -y valgrind git, then:
scripts/install-tools gungraun-runner
git worktree add --detach /tmp/main origin/main
scripts/benchmarks /tmp/main "$(mktemp -d)/main"        # note the two directories printed
scripts/benchmarks /src "$(mktemp -d)/pull-request"
scripts/benchmark-gate --main <main's directory> --pull-request <the pull request's directory>
```

`seccomp=unconfined` lets gungraun turn off address-space randomisation.  A local count compares
only with another made in the same container.

### Checklist: adding a benchmark

1. Add a `#[bench::<id>(...)]` case or a new `#[library_benchmark]` function in
   `crates/python/benches/front_end.rs`, with everything not being counted in its setup call.  A
   comment above a benchmark is `//`: gungraun refuses a doc comment.
2. Use only the synthetic corpus, through the tests' `common` module.
3. Run it locally, and check with `callgrind_annotate` that its count is the work it names.
4. A new or renamed benchmark is compared with nothing on its own pull request; say so in the
   description when renaming.

### Checklist: an alert or a blown budget

1. Read the job summary, and download the `benchmarks-<arch>` artefact.
2. `callgrind_annotate` the benchmark's profile on both sides and compare.
3. Decide whether that is what the change should cost.  If not, fix the code.
4. If it is, say in the pull request description which benchmark rose, by how much, and why.
5. For the arena, once the owner agrees the new cost, change `ARENA_BUDGET_PERCENT` in the same
   pull request, with the reason in the commit, and with it the figure and fixtures in
   `crates/quaffed/tests/benchmark_gate.rs`, and the slowed adapter in
   `scripts/break-benchmarks`, which must still exceed the new budget.
6. Changing `RULE_PERCENT`, or making a rise fail the job, is the owner's decision.

## The package install test

`scripts/test-deb PACKAGE` checks that a built package installs and runs, on every pull request
and before every release, in the *Debian package* job.  For each target distribution it starts a
clean container with no network and runs `packaging/deb/install-test/check`, which installs the
package with `dpkg -i`, checks its version and path, runs `quaff --help`, and runs searches whose
results are known, so a binary that runs but answers wrongly fails.  A failure names the step;
each distribution's whole log is kept in the job's artefact.  Only a registry 5xx while building
an image is retried.

**The target distributions are the Dockerfiles in `packaging/deb/install-test/`**, each a single
`FROM` pinned by digest, which Dependabot refreshes without ever proposing another release.  For
each of Debian and Ubuntu there are two:

- **A floor**: a release old enough that a glibc build would not run there (`release.md`), since
  the static musl binary exists to run on one.  It is a chosen release, and may be past its own
  support, since the test needs no package archive.
- **A current release**: what a new user is most likely to have, re-chosen by hand, below.

**The guard has a negative test on every run**: `scripts/break-deb` builds two broken copies, and
the job fails unless `test-deb` fails each at the expected step.

Locally, `test-deb` needs docker on a host of the package's architecture;
`scripts/test-deb PACKAGE debian-13` tests one distribution.  `break-deb` needs `dpkg-deb`.

### Checklist: changing the target distributions

1. Add, remove or rename a Dockerfile in `packaging/deb/install-test/`, named for the
   distribution and release, with a single `FROM <image>:<tag>@sha256:<digest>`.  The digest is
   the multi-architecture index's, from `docker buildx imagetools inspect`, which must list both
   `linux/amd64` and `linux/arm64`.
2. Run `scripts/test-deb` against it locally; CI runs every Dockerfile on both architectures.
3. When Debian publishes a new stable release or Ubuntu a new long-term release, re-choose the
   current release this way.

## Failed runs are posted to Slack

`.github/workflows/report-failures.yaml` posts a failed run of the workflows it names to the
project's Slack channel, through the shared reporter from `github-workflows`, whose README says
which runs it posts.  A run started by hand is left out, because whoever started it is watching.
To add a workflow that runs on a schedule, a push, a release or a pull request:

1. Give it a `name:`, and every job a `timeout-minutes:` (a job calling a reusable workflow
   cannot take one).
2. Add its `name:` to the `workflows:` list in `report-failures.yaml`, exactly as written; the
   repository hygiene check enforces the list.

It posts through the `SLACK_BOT_TOKEN` secret.  **Accepted limitation:** a failed post is only a
failed *Report failures* run, so a dead token shows as an absence of posts.

`announce-pull-requests.yaml` announces pull requests through the shared announcer, whose README
says which.  `SLACK_BOT_TOKEN` must also be a Dependabot secret, or Dependabot's are not announced.

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
