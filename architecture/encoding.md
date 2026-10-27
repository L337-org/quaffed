# Encodings

What encoding a file is in, how that is decided, and how its bytes are read.  quaff is not
interactive, so it either knows a file's encoding or leaves the file alone and says so: it never
guesses from statistics, and nothing about the machine it runs on - the locale included - changes
an answer.  `crates/encoding`, the `quaffed-encoding` crate, knows what bytes are in a given
encoding and reads the declarations that live in the bytes themselves; `src/resolve.rs` in the
`quaff` binary decides which declaration applies to a file.  Anything that reads Python source
takes the text this decodes rather than decoding for itself.

## How a file's encoding is decided

`SOURCES` in `src/resolve.rs` lists where an encoding can come from, the most explicit and the
closest to the file's bytes first, and **the first that applies decides**, so declarations never
conflict:

1. **A byte-order mark**.  A Python file's coding line must agree with a UTF-8 mark, as CPython
   requires, or the file is refused.
2. **The file's own language declaration**: a Python file's coding line, below.
3. **The version control system's declaration**, through the workspace: for git, the
   `working-tree-encoding` attribute, below.
4. **EditorConfig's `charset`**, below.
5. **The bytes being proven UTF-8**: valid UTF-8 holding no ISO-2022 switch into a multi-byte
   set, `ESC $`, anywhere in the file, since ISO-2022-JP's seven-bit bytes are valid UTF-8 too.

When none applies, or one declares a name no codec has included, the encoding is **unknown**;
`search.md` says what a search does then, and why such a file is never edited.  git's binary
test, a NUL in the first 8000 bytes, applies to whatever decides, except a byte-order mark or a
declaration of UTF-16 or UTF-32, whose text is full of NULs.

A name declared outside a Python file is looked up as CPython looks a codec name up (`named`), so
it finds the codec a coding line would.  git's `-BOM` suffix is dropped, the mark having decided
already if it is there; UTF-16 and UTF-32 are read by name only with their byte order.

### Checklist: adding a source

1. Write the function in `src/resolve.rs` that decides from it, returning nothing when it does
   not apply.  A version control system's declaration belongs in its workspace implementation,
   not here.
2. Put it in `SOURCES` by how explicit it is and how close to the file's bytes.
3. Test that it decides, that what comes before it wins, and what a failure to read it says.
4. Update this page.

## git's `working-tree-encoding`

Read by `src/workspace/git.rs` with git's own rules (`gitattributes(5)`), from the repository's
own files only - every `.gitattributes` from the root down, and `info/attributes` - and **never
from git's configuration**, so `core.attributesFile` and the system's attributes file change
nothing.  A negative pattern is skipped with a note, as git ignores it.  A file in a nested
repository, a submodule among them, takes its attributes from that repository's files alone.
**A declaration file that cannot be read stops the run**, exit 7, rather than reading a file in
the wrong encoding for want of it.

## EditorConfig's `charset`

Read by `src/editorconfig.rs`, outside the workspace because it is the same under every version
control system, with EditorConfig's own rules.  **It stops at the project root**, or where it
runs with no project, as well as at `root = true`, so a user's own `.editorconfig` above the
project changes nothing.  `utf-8-bom` is read as UTF-8, and `unset` declares nothing.

### Checklist: changing what the declaration readers handle

The readers are held to the tools themselves: `scripts/generate-declaration-cases` builds trees,
asks `git check-attr` and EditorConfig's reference core what applies to each path, and writes
`crates/quaffed/tests/data/declarations.txt`, which a test in `src/resolve.rs` replays.

1. Add a case to `CASES` in the script for the behaviour.
2. Run it as its usage line says, with the reference core's pinned version.
3. Read the diff of the data file: every changed answer is a change in what a tool says.
4. Run `cargo test -p quaffed`, and change the reader until it agrees.

## A Python file

A Python file is UTF-8 unless its first or second line declares otherwise (PEP 263).  **quaff
follows CPython's tokenizer to the letter, quirks included**, because a file quaff and CPython
read differently is a file quaff misreads.  The sources, at the oracle's release:
`Parser/tokenizer/helpers.c` and `string_tokenizer.c` for finding the declaration
(`src/python.rs`, `declaration` and `coding_spec`), `Objects/unicodeobject.c` and
`Lib/encodings/__init__.py` for resolving its name (`normal_name`, `resolve`).

**Decoding** (`Source::decode`) refuses what CPython refuses, each with its own message, and
also an encoding Python reads and quaff does not.  `Source` keeps the text, the encoding and
whether there was a mark, and `Source::encode` writes text back the same way, so a file's own
decoded text encodes to its exact bytes.

**For a reader that tolerates bad bytes**, as a textual search is, `declared_encoding` resolves
the declaration alone, refusing only what no content could make readable.  `src/resolve.rs` turns
an encoding Python reads and quaff does not into an unknown encoding naming the declaration.  An
undeclared Python file that is not valid UTF-8 is of unknown encoding rather than read as UTF-8,
so a non-ASCII query is reported as not searched rather than silently matching nothing.

## CPython's codecs

`src/codecs.rs` is generated from the oracle by `scripts/generate-codecs`, never edited.  It is
derived from CPython's own codecs, under the PSF License Version 2, which the package's
copyright file attributes and carries.  It lists every codec module of CPython's `encodings`
package with its kind, every alias, and for each single-byte codec what each byte decodes to,
checked byte by byte against CPython's own decoder.

**Multi-byte codecs** quaff reads are those named in the generator's `MULTI_BYTE`, all read by one
rule: a byte's length says how long the character is and the run is looked up; a run not in the
table is one undecodable byte, its first, and reading goes on from the next; a run cut short by
the end is one undecodable run.  That is where CPython puts its errors, so a column counts as
CPython reads the file.  The generator refuses a codec that does not fit the rule, and writes
the byte strings it checked against CPython to `tests/data/multi_byte.txt`, which
`tests/multi_byte.rs` holds quaff's reader to.

**A character an encoding spells more than one way**, as `cp932` and a few single-byte codecs
do, has no one spelling: a query holding it does not search the file, and it cannot be written.
`latin-1` is true Latin-1, every byte its own code point, as CPython has it, not the web's
`windows-1252`; that is why quaff does not use the web's encoding tables.

**Accepted limitations:** other multi-byte, stateful and escape-based encodings - GBK, Big5,
EUC-KR, GB18030, ISO-2022 - are not read, and `src/codecs.rs` lists them as `Unread`; Windows-only
codecs are unknown, as they are to CPython anywhere but Windows; and EBCDIC codecs decode a file
whole, its declaration line included, as CPython does.

### Checklist: regenerating the codec tables

To read a new codec, or to follow a CPython release that changed one:

1. Run `scripts/generate-codecs` with the oracle's Python, the CPython `corpus/real-world.toml`
   names.  It stops naming the version it needs if it is run with another, and stops naming the
   codec if a multi-byte one does not read by the rule above.
2. Read the diff of `src/codecs.rs` and `tests/data/multi_byte.txt`: every changed table, kind or
   reading is a change to how quaff reads files.
3. Run `cargo test -p quaffed-encoding`.  The tables must stay sorted, every codec must spell CR
   and LF one way each, which a textual search relies on to find line endings, and every
   multi-byte reading must match CPython's.
4. If the tables now come from another CPython release, name it in the tables' paragraph in
   `packaging/deb/copyright`; `crates/quaffed/tests/packaging.rs` fails until it does.
5. Update this page in the same change.

### Checklist: reading a multi-byte encoding

1. Name it in `MULTI_BYTE` in `scripts/generate-codecs`, and run the generator as the
   regenerating checklist says.  It refuses a codec that does not fit the reading rule, naming
   why; one that does not fit needs a reader of its own kind, and a stateful one, such as
   ISO-2022, always does.
2. Run `cargo test -p quaffed-encoding`, which holds the reader to the strings the generator
   checked against CPython.
3. Add an end-to-end test that finds text in a file declaring it, at the right line and column,
   and that bytes matching inside its characters are not matched.
4. Update this page in the same change.
