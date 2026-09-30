# quaffed

**quaffed**, the QUAntiFied File EDitor, is an editing language for making mechanical changes to
code quick, accurate and reliable.  Edits are structural rather than textual, and they are
*quantified*: every edit declares how many matches it expects and what must be true before and
after it, and if anything differs the edit does not happen at all.  You find out at the end.

The binary is `quaff`, so you quaff a file, or quaff every file in a repository.

## Status

**Pre-release, and nothing is implemented yet.**  The design is still being settled.  This
repository currently holds a scaffold - a Cargo workspace, CI and the contributor
documentation - and a placeholder `quaff` that says it is not implemented and exits non-zero.

## Installing

There is no release yet.  Homebrew and APT packages for macOS and Linux are planned.

## Building from source

With a Rust toolchain installed (the version is pinned in `rust-toolchain.toml`, which rustup
picks up automatically):

```bash
git clone https://github.com/L337-org/quaffed.git
cd quaffed
cargo build --release
./target/release/quaff
```

## Running

Not yet: see Status.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, conventions and how to submit a change, and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for the standards expected of everyone taking part.
Security issues are reported privately; see [SECURITY.md](SECURITY.md).

## Licence

GPL-3.0-or-later; see [LICENSE](LICENSE).  Running `quaff` over a project's files does not make
that project a derivative work, in the same way that compiling with a GPL compiler does not, and
scripts you write for it carry whatever licence your own project chooses.
