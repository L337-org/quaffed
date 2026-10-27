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
