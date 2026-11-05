# Tests

How quaffed's behaviour is held by its tests: end-to-end snapshots of what `quaff` prints, and
the coverage ratchet that keeps the tests from covering less.

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

## The cost check's corpus

`the_cost_check_gives_the_corpus_its_recorded_verdicts` (`crates/syntax/src/matching.rs`) holds
the cost check (`Pattern::costly`) to a fixed corpus of generated patterns.  Its snapshot has one
line per pattern: its verdict alone and with each name a `where` could test, and the matcher's
work over the pattern filled in at two lengths, and spoiled so that it fails at its end.  The test
fails where an accepted pattern's work grows faster than the square of the code, and its snapshot
names every pattern whose verdict, reported hole or work moved.  The property
`no_accepted_pattern_costs_more_than_the_square` measures the same way over patterns generated
afresh on every run, deeper than the corpus's, so that a shape the corpus never holds is still
met in time.  Proptest saves each failure it finds in `crates/syntax/proptest-regressions/` and
replays it first on every later run; commit that file with the fix, so the failure stays a test.
The targets are not worst cases, so passing is evidence, not proof, that what is accepted stays
quadratic.

When changing the cost check or how the matcher searches a list:

1. Run `cargo insta test -p quaffed-syntax`.
2. If the corpus fails with "accepted, but its work grows faster than the square", or the growth
   property fails, the change accepts a pattern the matcher cannot search within the bound.  Find
   out whether the check miscounts or the matcher does more than the check assumes: measure the
   pattern's work at growing sizes, and again with comparing code left uncounted.  Fix whichever
   is at fault at its cause; never raise the bound in the test to pass, and never single out the
   failing pattern's shape.
3. Read the snapshot's diff in `cargo insta review`.  Each changed line should be explained by the
   change: a newly accepted pattern by the rule that now accepts it, a newly refused one by the
   rule that refuses it, and a change in work by the matcher change.
4. For a newly accepted shape whose cost the corpus's targets cannot show, add a named test with a
   target built to make every way fail, as `a_sequence_after_another_is_moved_by_both_their_stops`
   does, and check it fails without the change.
5. Accept the snapshot only once every changed line is explained.

## The coverage ratchet

The **Coverage ratchet** (`.github/workflows/coverage.yaml`) holds every pull request to the same
fixed bar, whatever state main is in, so that what a pull request must cover does not depend on
when it lands.  It keeps the name it had when it compared against main.

- **An overall floor per metric**: a pull request whose line or region coverage is below its floor
  in `FLOORS` (`scripts/coverage-ratchet`) fails.  The floors sit a little below where coverage
  stands, so one pull request's unreachable error branches do not fail it, but a slide does.
- **A floor for new code**: the coverable lines a pull request adds, from `git diff` of main against
  the merge being measured and the pull request's line coverage in lcov, fail when their share
  covered is below `NEW_CODE_FLOOR` *and* more than `NEW_CODE_ALLOWANCE` of them are uncovered.
  The allowance is what lets a change of a line or two through, which a percentage alone would
  hold to 0% or 100%; the overall floor catches what allowances add up to.  It counts lines, not
  regions, because every `?` is a region of its own, and an error branch no test can reach would
  otherwise fail a small change.  A pull request adding no coverable line passes, and says so.
- **Main is measured and reported, never gated**: the change in each metric, and the files with
  more uncovered than on main, appear in the job summary.  Both sides are measured in the same
  run, the pull request merged into main and main as that merge's first parent, from a second
  worktree with the pull request's own `scripts/coverage`.
- **Condition coverage is reported only**, because it needs an unstable flag and so runs on the
  pinned nightly.  It joins `FLOORS` when the flag is stable, which the toolchain-upgrade
  checklist in `workspace.md` checks.
- **The measurement is deterministic**: `scripts/coverage` fixes proptest's seed, and the pull
  request is measured twice on separate runners, failing if the two differ.  A difference means
  a non-deterministic test, in the file named.
- **A gate that cannot judge fails rather than passes**: a missing summary, lcov or diff, a metric
  counting nothing, or a diff naming a path the script cannot read fails.

Raising a floor is a change of its own: set the figure in `FLOORS` or `NEW_CODE_FLOOR`, below
where coverage stands on main, and update the tests in `crates/quaffed/tests/coverage_ratchet.rs`
whose figures sit either side of it.

```bash
scripts/install-tools cargo-llvm-cov nightly-coverage   # once
LCOV=target/coverage.lcov scripts/coverage stable target/coverage-stable.json
scripts/coverage condition target/coverage-condition.json
git diff --unified=0 --find-renames main > target/changes.diff
scripts/coverage-ratchet --head-stable target/coverage-stable.json \
  --head-lcov target/coverage.lcov --diff target/changes.diff
```

To compare with another checkout, measure it the same way, giving its path as the third argument,
and pass it as `--base-stable`.
