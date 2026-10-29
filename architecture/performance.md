# Performance

How quaff's cost is held: budgets on the work the shipped build does, benchmarks on the Python
front end and the matcher, and profiling to find where the time goes.

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
  A scenario that edits gets a fresh copy, at the same path, before each run of quaff.
- **The limits are `budgets.toml`**: `instructions` (Callgrind) and `peak_memory_kib` (GNU time)
  with a margin from `MARGINS`, and `paths_opened`, `bytes_read` and `paths_written` (strace)
  exactly, since a change in any is a change in behaviour.  No path in the project may be opened
  to read twice, or to write more than twice - an edited file is opened once to read, and to write
  once to check it can be and once to write it - and nothing outside it but what `[outside]`
  allows, each with its reason.
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

## Recording to InfluxDB

After each merge to main, the budgets and the benchmarks are posted to InfluxDB, so their trend
can be followed; nothing gates on what is recorded.

- **The measuring jobs write the points**, as InfluxDB line protocol, with
  `scripts/record-measurements budgets` and `benchmarks`, because only they know the runner they
  measured on.  They write them on pull requests too, so a change that breaks them fails before
  it merges.  The benchmarks also run on a merge to main, counting only the merged commit.
- **`.github/workflows/record-measurements.yaml` posts them**, once a merge's measuring workflows
  finish, and it and the daily `catch-up-measurements.yaml` are the only workflows given the
  secrets `QUAFFED_INFLUX_URL`, `QUAFFED_INFLUX_USER` and `QUAFFED_INFLUX_PASSWORD`.  Any run that
  is not a push to main in this repository, a fork's included, skips its job before it starts.
  No merge's run is cancelled by a later one, so every merge is measured.
- **What is recorded**: `quaffed_budget`, one point per scenario, and `quaffed_benchmark`, one per
  benchmark and architecture with Callgrind's counts under Callgrind's own names.  Tags are what a
  dashboard groups by - `scenario`, or `benchmark` and `function`, then `arch`, `os` and
  `runner_image` - and the commit is a field, so the series do not multiply with every commit.
  Each point carries the time it was measured.  A new figure is a new field and a new
  architecture a new tag value, so neither changes what is already there.
- **What could not be posted is kept**, as the run's `unrecorded-measurements` artefact, for the
  `retention-days` the workflow sets; every later run, after posting its own, and the daily
  `catch-up-measurements.yaml` post it and then delete it.  Only an artefact a Record measurements
  run left is posted, since any pull request's run can upload one of that name.  The daily one is
  a workflow of its own, re-enabled by every merge's run, because GitHub disables a scheduled
  workflow after 60 days without activity.  A point posted twice is
  the same point, so posting again is safe.  What InfluxDB refuses as malformed is kept as
  `rejected-measurements` and never posted again.  A failed run is posted to Slack, and its job
  summary says how to post by hand.

### Checklist: recording a new figure

1. Add it to `BUDGET_FIELDS` or `BENCHMARK_FIELDS` in `scripts/record-measurements`, under the
   name a dashboard should see; a new budget figure also needs `scripts/budgets --json` to write
   it.
2. Add it to the expected points in `crates/quaffed/tests/record_measurements.rs`.
