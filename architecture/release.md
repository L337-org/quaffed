# The Linux package and releasing

How the `.deb` is built, what is in it and why, and how a release is published.  A release ships
a `.deb` for `amd64` and `arm64`, attached to a GitHub release and in no APT repository.

## What is in the package

`quaffed_<version>_<arch>.deb` installs `/usr/bin/quaff`, a copyright file and a changelog, and
nothing else.  There is no man page.

**The binary is static musl with mimalloc as its global allocator.**  A glibc build needs
`GLIBC_2.34`, so it does not run on older distributions such as Debian 11 or Ubuntu 20.04, while a
static musl build runs on all of them.  musl's own allocator makes parsing markedly slower than
glibc's; mimalloc more than recovers that, at the cost of a higher peak memory.  mimalloc is a
dependency only for musl targets, so macOS and glibc builds keep the platform allocator.

**The `dist` profile** - release optimisation, thin LTO, one codegen unit, symbols stripped - is
what the package is built with.  Those are the settings the measurements were taken with; they
are a separate profile so that a developer's `--release` build stays quick to link.

**Building it needs a musl C compiler**, because mimalloc is C.  Without `musl-tools` the build
fails with *"failed to find tool "x86_64-linux-musl-gcc""*.  The spike found Ruff's parser needs
one too, through `stacker` and `psm`'s assembly, so the requirement stays when that arrives.

**The copyright file is hand-written**, in Debian's machine-readable format, and names everything
linked into the binary with its copyright and licence, because the binary distributes it.
`crates/quaffed/tests/packaging.rs` reads the binary's dependency graph for both packaged targets
with `cargo tree` and fails naming any crate the file does not mention, so adding a dependency
means adding it there.  **What comes with the Rust toolchain rather than as a crate - the musl C
library and LLVM's libunwind - is outside that graph, so the test cannot see it**, and it is kept
up to date by hand: check it when the toolchain's musl target changes.

**The version** is the workspace's, read by `scripts/workspace-version`, and the release's tag
must be `v` followed by it.  The changelog has one entry per version pointing at the release notes,
which are the history.

## Building it

```bash
sudo apt-get install musl-tools lintian
scripts/build-deb amd64     # on an amd64 Linux machine
scripts/build-deb arm64     # on an arm64 Linux machine
```

Each architecture is built on a machine of that architecture; the script refuses otherwise.  The
package lands in `target/deb/`, with lintian's full output beside it.  Package timestamps come
from the commit, so two builds of one commit in the same environment give the same bytes - shown
twice for each architecture in one container - and a re-run of a release relies on that to tell
an identical asset from a different one.  **The environment is only partly pinned**: Rust is, by
`rust-toolchain.toml`, but the C compiler that builds mimalloc comes from the runner image's
`musl-tools` and gcc, which GitHub updates.  So a re-run after an image update can rebuild a
package that differs from one already attached with nothing wrong; the attach step stops rather
than replace it, and says so.

**lintian is a gate.**  The script accepts exactly two findings and fails on any other, so a new
one is seen rather than buried:

| | amd64 | arm64 |
|---|---|---|
| no man page | `W: no-manual-page` | `W: no-manual-page` |
| the static binary | `W: shared-library-lacks-prerequisites` | `E: statically-linked-binary` |

The static-binary finding differs because Rust builds a **static PIE** for `x86_64` musl - an ELF of
type `DYN` with no `NEEDED` entries, which keeps address-space randomisation - and a non-PIE static
executable, type `EXEC`, for `aarch64` musl.  lintian reads the first as a shared library with no
prerequisites.  A non-PIE amd64 build draws `statically-linked-binary` like arm64's, but gives up
address-space randomisation to do so.  Both findings, and the missing man page, are accepted for the
MVP: its package is for trying quaff out in a container, not held to Debian policy.

## CI

`.github/workflows/package.yaml` builds both packages on every pull request - `amd64` on
`ubuntu-24.04`, `arm64` natively on `ubuntu-24.04-arm` - and keeps each package and its lintian
output as a workflow artefact, so a change that breaks packaging fails where it is made.  Its jobs,
*Debian package (amd64)* and *Debian package (arm64)*, are meant to be required status checks.

## Releasing

`.github/workflows/release.yaml` runs when a release is **published**; pushing a tag ships
nothing.  It:

1. **Preflight** - fails unless the release belongs to `L337-org`, its tag is `v` plus the
   workspace version, and - for any version below 1.0.0 - it is marked as a pre-release; then
   resolves the commit once for everything after it;
2. **Package** - calls `package.yaml` at that commit;
3. **Attach the packages** - only once both architectures built, uploads each `.deb` to the
   release ID from the event.  A package already attached is compared, not replaced: identical is
   a re-run and is skipped, and different stops the run, because a published asset is never
   replaced;
4. **The release serves both packages** - downloads each asset back from the release and checks it
   is byte-identical to the one built, with the right package name, version and architecture.

### Checklist

1. On the branch for the release's work, the workspace version is already the release's (it is
   bumped when that work starts).  Merge to `main`.
2. Draft a release on GitHub with tag `v<version>` on `main`'s commit, **mark it as a pre-release**,
   and write the notes by hand on top of the generated ones.
3. Publish it, then watch the Release workflow to the end: the first run after a change to it is
   the test of that change.
4. If a job failed, read why and re-run it; the attach step skips what is already there, and
   preflight reads the pre-release flag from the release itself, so ticking it and re-running is
   enough.  If the cause is a defect, fix it and release a new version - a published package is
   never replaced.
5. **At the first release**, change the README's *Installing* section, which says there is no
   release yet, to point at the release's packages.

**Not automated yet:** a failed release run reaches nobody except whoever is watching.  Reporting
unattended failures to the project's chat channel is its own piece of work.
