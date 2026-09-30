## Summary

<!-- What does this change do, and why? -->

## Test plan

<!-- How did you verify this? e.g. `cargo test --locked`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --all -- --check`, running `quaff` against a real file, manual steps. -->

## Checklist

- [ ] `cargo test --locked`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo fmt --all -- --check` all pass locally
- [ ] Every commit carries a `Signed-off-by:` line (`git commit -s`), per the Developer Certificate of Origin in `CONTRIBUTING.md`
- [ ] If this changes structure, conventions, the command line, the language or exit codes: the rule reaches whichever layer carries it - `AGENTS.md`, or the detail in `architecture/` or `CONTRIBUTING.md`
- [ ] If this changes a dependency: `Cargo.lock` is updated and committed alongside `Cargo.toml`, and the new dependency's licence is compatible with GPL-3.0-or-later
