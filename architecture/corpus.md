# The corpora

The Python the front end is checked against - a synthetic corpus written for it, and CPython's
own tests, fetched - and the checks run over them.

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
