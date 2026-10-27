# The Linux package and releasing

A release ships a `.deb` for `amd64` and `arm64`, attached to a GitHub release and in no APT
repository.

## What is in the package

`quaffed_<version>_<arch>.deb` installs `/usr/bin/quaff`, a copyright file and a changelog, and
nothing else.  There is no man page.

**The binary is static musl with mimalloc as its global allocator.**  A glibc build does not run on
older distributions such as Debian 11 or Ubuntu 20.04, and a static musl build does.  musl's own
allocator makes parsing markedly slower, so mimalloc replaces it, at the cost of a higher peak
memory.  mimalloc is a dependency only for musl targets; other builds keep the platform allocator.

**The package is built with the `dist` profile** in the root `Cargo.toml`, kept separate so that a
developer's `--release` build stays quick to link.  The `profiling` profile inherits it
(`performance.md`).

**Building it needs a musl C compiler**, `musl-tools`, for mimalloc and for Ruff's parser through
`stacker` and `psm`.  Without it the build fails with *"failed to find tool
"x86_64-linux-musl-gcc""*.

**The copyright file**, `packaging/deb/copyright`, is hand-written in Debian's machine-readable
format and accounts for everything the binary contains.  `Files: *` is quaffed alone.  Each crate,
or group of crates sharing holders and a licence, has its own paragraph as `crates.io/NAME/*`,
named without its version, and the toolchain's parts are `toolchain/NAME/*`.  LLVM's libunwind has
no paragraph, because its licence waives the notice for code embedded in a binary.
`crates/quaffed/tests/packaging.rs` fails naming any linked crate without a paragraph, and takes a
new version of a crate to keep its licence.  The toolchain's parts and the codec tables generated
from CPython (`encoding.md`) are outside the dependency graph it reads, so they are kept by hand:
check musl's paragraph when the toolchain's musl target changes.

When the test names a crate:

1. Read the crate's licence files and its `Cargo.toml` for its holders and its licence.
2. If an existing paragraph has exactly those holders and that licence, add `crates.io/NAME/*` to
   its `Files`; otherwise add a paragraph for it.
3. If its licence is not yet carried, add a `License` paragraph with the text, or the
   `/usr/share/common-licenses` path where Debian ships it.
4. When a crate offers a choice, use one the file already carries, and say what was offered in the
   paragraph's `Comment`.

**The version** is the workspace's (`workspace.md`), read by `scripts/workspace-version`, and the
release's tag must be `v` followed by it.  The changelog has one entry per version, pointing at the
release notes.

## Building it

```bash
sudo apt-get install musl-tools lintian
scripts/build-deb amd64     # on an amd64 Linux machine
scripts/build-deb arm64     # on an arm64 Linux machine
scripts/build-deb --dev amd64              # a development build, for testing
scripts/test-deb target/deb/quaffed_*.deb  # install it on each target distribution and run it
```

**A development build says so in its version**: `--dev` makes it `<version>~dev+g<commit>`, with
`.dirty` appended for uncommitted changes, so it sorts before the release it leads up to and cannot
be mistaken for one that shipped.  CI builds without `--dev`, so every pull request runs exactly the
packaging a release runs.

Each architecture is built on a machine of that architecture; the script refuses otherwise.
Timestamps come from the commit, so a re-run of a release can tell an identical package from a
different one.  **Only Rust is pinned**: the C compiler that builds mimalloc comes from the runner
image, so a re-run after an image update can build different bytes, and the attach step then stops
rather than replace the published package.

**The package is compressed with xz, chosen explicitly**, because `dpkg-deb` may otherwise choose
zstd, which Debian 11's dpkg refuses.

**lintian is a gate**: `scripts/build-deb` accepts exactly the missing man page and one
static-binary finding, and fails on any other.  The static-binary finding is
`shared-library-lacks-prerequisites` on amd64 and `statically-linked-binary` on arm64, because Rust
builds a static PIE for `x86_64` musl, keeping address-space randomisation, and a non-PIE static
executable for `aarch64` musl.  Both are accepted for the MVP, whose package is for trying quaff
out, not held to Debian policy.

## CI

`.github/workflows/package.yaml` builds both packages on every pull request, each on a runner of its
own architecture, then installs and runs each on every target distribution (the package install
test, above), keeping the packages, lintian's output and the install logs as an artefact.
Its jobs are meant to be required status checks, and `release.yaml` calls it, so a package is
installed and run before it can ship.

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

- **A floor**: a release old enough that a glibc build would not run there (above), since
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

## Releasing

`.github/workflows/release.yaml` runs when a release is **published**; pushing a tag ships nothing.
It refuses a release outside `L337-org`, a tag other than `v` plus the workspace version, and a
version below 1.0.0 not marked as a pre-release.  It builds with `package.yaml` at one commit,
attaches the packages only once both architectures built, and never replaces a published package:
one already attached is skipped if identical and stops the run if not.  Then it downloads each back
from the release and checks it is the one built.

### Checklist

1. The workspace version is already the release's, bumped when its work started.  Merge to `main`.
2. Draft a release on GitHub with tag `v<version>` on `main`'s commit, **mark it as a pre-release**,
   and write the notes by hand on top of the generated ones.
3. Publish it, then watch the Release workflow to the end: the first run after a change to it is
   the test of that change.
4. If a job failed, read why and re-run it; the attach step skips what is already there, and
   preflight reads the pre-release flag from the release itself.  If the cause is a defect, fix it
   and release a new version: a published package is never replaced.

A failed release run is posted to Slack (`workspace.md`).
