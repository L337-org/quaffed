# Encodings

What encoding a file is in, how that is decided, and how its bytes are read.  quaff is not
interactive, so it either knows a file's encoding or leaves the file alone and says so: it never
guesses from statistics, and nothing about the machine it runs on - the locale included - changes
an answer.  `crates/encoding`, the `quaffed-encoding` crate, knows what bytes are in a given
encoding and reads the declarations that live in the bytes themselves; `src/resolve.rs` in the
`quaff` binary decides which declaration applies to a file.  Textual search uses both
(`search.md`); so will anything that reads Python source, which takes the text it decodes rather
than decoding for itself.

## How a file's encoding is decided

`SOURCES` in `src/resolve.rs` lists where an encoding can come from, the most explicit and the
closest to the file's bytes first, and **the first that applies decides**: later ones are not
consulted, so declarations never conflict.

1. **A byte-order mark**: UTF-8, UTF-16 or UTF-32.  A Python file's coding line must agree with a
   UTF-8 mark, as CPython requires, or the file is refused.
2. **The file's own language declaration**: a Python file's coding line, below.
3. **The version control system's declaration**, through the workspace: for git, the
   `working-tree-encoding` attribute, below.
4. **EditorConfig's `charset`**, below.
5. **The bytes being proven UTF-8**: valid UTF-8 holding no ISO-2022 switch into a multi-byte set,
   `ESC $`.  ISO-2022-JP is seven-bit, so its bytes are valid UTF-8 and an ASCII query could match
   inside its Japanese text; it announces itself with an escape sequence starting `ESC $`, which
   can come anywhere, so the whole file is checked.  Colour codes start `ESC [` and are not
   mistaken for it.

When none applies, the encoding is **unknown**; so is one declared that quaff does not read, a
name no codec has included.  `search.md` says what a search does with each, and why a file of
unknown encoding is never edited.  git's binary test, a NUL in the first 8000 bytes, applies to
whatever decides, except a byte-order mark or a declaration of UTF-16 or UTF-32, whose text is
full of NULs.

A name declared outside a Python file - `SHIFT-JIS`, `ISO-8859-1`, `UTF-16LE` - is looked up as
CPython looks a codec name up (`named`), so it finds the codec a coding line would.  git's
`-BOM` suffix is dropped, the mark having decided already if it is there; UTF-16 and UTF-32 are
read by name only with their byte order, since plain `UTF-16` needs a mark to say which.

### Checklist: adding a source

1. Write the function in `src/resolve.rs` that decides from it, returning nothing when it does
   not apply.  A version control system's declaration belongs in its workspace implementation,
   not here.
2. Put it in `SOURCES` by how explicit it is and how close to the file's bytes.
3. Test that it decides, that what comes before it wins, and what a failure to read it says.
4. Update this page.

## git's `working-tree-encoding`

Read by `src/workspace/git.rs` from the repository's own files only - every `.gitattributes` from
the root down to the file's directory, and `info/attributes` in the git directory, found through a
`.git` file for a worktree or submodule - and never from git's configuration, so
`core.attributesFile` and the system's attributes file change nothing.  git's rules
(`gitattributes(5)`): within a file a later line overrides an earlier one, a deeper `.gitattributes`
overrides a shallower one, and `info/attributes` overrides them all.  Patterns match as in
`.gitignore`, through the `ignore` crate's matcher, except that a negative pattern is skipped with
a note, as git ignores it, and a pattern naming a directory does not match what is inside it.  A
pattern may be quoted in C style.  `[attr]` macros are read from the top-level `.gitattributes` and
`info/attributes` only.  Only a value declares an encoding; set, unset and unspecified declare none.
A declaration file that cannot be read stops the run, exit 7, rather than reading a file in the
wrong encoding for want of it.

## EditorConfig's `charset`

Read by `src/editorconfig.rs`, outside the workspace because it is the same under every version
control system.  EditorConfig's rules: the `.editorconfig` files from the file's directory up are
read, nearer overriding farther, and within a file a later section overriding an earlier one,
stopping at a file whose preamble says `root = true`.  quaff also stops at the project root, or
where it runs with no project, so a user's own `.editorconfig` above the project changes nothing.
Section globs are EditorConfig's: `*`, `**`, `?`, `[...]` and `[!...]`, `{a,b}`, `{n..m}`, and
escapes; a glob with no `/` matches at any depth.  `unset` declares nothing.  Its five values map
to `latin1`, `utf-8`, `utf-16be` and `utf-16le`, with `utf-8-bom` read as UTF-8.

### Checklist: changing what the declaration readers handle

The readers are held to the tools themselves: `scripts/generate-declaration-cases` builds trees,
asks `git check-attr` and EditorConfig's reference core what applies to each path, and writes
`crates/quaffed/tests/data/declarations.txt`, which a test in `src/resolve.rs` replays.

1. Add a case to `CASES` in the script for the behaviour.
2. Run it as its usage line says, with the reference core's pinned version.
3. Read the diff of the data file: every changed answer is a change in what a tool says.
4. Run `cargo test -p quaffed`, and change the reader until it agrees.

## A Python file

A Python file is UTF-8 unless its first or second line declares otherwise (PEP 263), and a UTF-8
byte-order mark allows no declaration but UTF-8.  quaff follows CPython's tokenizer to the
letter, quirks included, because a file quaff and CPython read differently is a file quaff
misreads.  The sources, at the oracle's release: `Parser/tokenizer/helpers.c` and
`string_tokenizer.c` for finding the declaration, `Objects/unicodeobject.c` and
`Lib/encodings/__init__.py` for resolving its name.

**Finding the declaration** (`src/python.rs`, `declaration` and `coding_spec`):

- Lines end at LF, CRLF or a lone CR, and a last line without an ending counts as a line.
- The first line is read.  The second is read only if the first is blank or nothing but a
  comment.
- A declaration is a comment with only spaces, tabs and form feeds before it, holding `coding:`
  or `coding=` followed, after any spaces or tabs, by a name of ASCII letters, digits, `-`, `_`
  and `.`.  The first such `coding` with a name wins: `# -*- coding: latin-1 -*-` and
  `# vim: set fileencoding=koi8-r :` both declare.
- CPython stops searching a line seven bytes before its end, which changes nothing, since a
  declaration with a name needs at least eight bytes, so quaff does not copy the bound.

**Resolving the name** (`normal_name`, `resolve`):

- First CPython's own shortcut: the name's first 12 bytes, lower-cased with `_` as `-`, that are
  `utf-8` or start `utf-8-` are UTF-8, and the spellings of Latin-1 it lists are `iso-8859-1`.
  Only exactly `utf-8` is allowed after a byte-order mark, so `utf8` with a mark is refused, as
  CPython refuses `tokenizedata/bad_coding2.py`.
- Then CPython's codec lookup: lower-case, each run of anything but letters, digits and `.`
  collapsed to one `_` and dropped at the ends, looked up in its aliases as it is and with `.`
  as `_`, then tried as a module of the `encodings` package.

**Decoding** (`Source::decode`) refuses what CPython refuses, each with its own message, which
the caller prefixes with the file's path:

| the file | is refused because |
|---|---|
| has a NUL byte anywhere | Python source cannot contain one; CPython checks this first |
| declares a name no codec has, such as `uft-8` | Python does not know the encoding |
| declares a codec that is not of text, such as `rot13` | it is not a text encoding |
| declares `undefined` | that codec decodes nothing |
| has a UTF-8 byte-order mark and declares anything but `utf-8` | a mark allows only UTF-8 |
| has a byte its encoding does not decode | the byte is invalid, named with its line; undeclared UTF-8 says to declare or re-encode |
| declares an encoding Python reads and quaff does not yet | quaff's limit, not Python's |

`Source` keeps the text, the encoding and whether there was a mark.  `Source::encode` writes text
back the same way, mark first, so a file's own decoded text encodes to its exact bytes.

**For a reader that tolerates bad bytes**, as a textual search is, `declared_encoding` resolves
the declaration alone, with the line declaring it, and refuses only what no content could make
readable: an unknown name, a codec not of text, `undefined`, or a conflict with a mark.  An
encoding Python reads and quaff does not yet is refused too, and `src/resolve.rs` turns that one
into an unknown encoding naming the declaration, rather than a file not searched at all.  An
undeclared Python file that is not valid UTF-8 is of unknown encoding rather than being read as
UTF-8, so that a non-ASCII query is reported as not searched rather than silently matching
nothing.

## CPython's codecs

`src/codecs.rs` is generated from the oracle by `scripts/generate-codecs`, never edited.  It is
derived from CPython's own codecs, under the PSF License Version 2, which the package's
copyright file attributes and carries.  It lists every codec module of CPython's `encodings`
package with its kind, every alias, and for each single-byte codec what each byte decodes to.  A
codec is single-byte if CPython decodes it from a table, or it is `ascii`, `latin_1` or
`charmap`; the generator checks every such codec byte by byte against CPython's own decoder and
refuses to write one that is not single-byte after all.

**Multi-byte codecs** quaff reads are named in the generator's `MULTI_BYTE`: `shift_jis`, `cp932`
and `euc_jp`.  For each it writes how long a character starting with each byte is, and every run
of bytes that decodes to one character.  They are read by one rule: a byte's length says how long
the character is and the run is looked up; a run not in the table is one undecodable byte, its
first, and reading goes on from the next; a run cut short by the end is one undecodable run of
what is left.  That is where CPython puts its errors, so a column counts as CPython reads the
file.  The generator refuses a codec that does not fit the rule - a byte that both decodes alone
and starts a longer run, or a line ending inside a character - and checks the rule against
CPython's own decoder on byte strings, many broken on purpose, before writing anything.  It
writes those strings with CPython's reading of each to `tests/data/multi_byte.txt`, which
`tests/multi_byte.rs` holds quaff's reader to.  A character a codec spells more than one way, as
`cp932` does some, has no one spelling: a query holding it does not search the file, and it cannot
be written.  `latin-1` is true Latin-1, every byte
its own code point, as CPython has it, and not the web's reading of it as `windows-1252`; that
difference is why quaff does not use the web's encoding tables.

**Accepted limitations:**

- **Other multi-byte, stateful and escape-based encodings are not read**: GBK, Big5, EUC-KR,
  GB18030 and ISO-2022, for instance, or UTF-16 declared by a name without its byte order.
  `src/codecs.rs` lists every such codec as `Unread`.  A file declaring one is of unknown
  encoding, its declaration named.
- **Windows-only codecs** - `mbcs`, `oem` and the `cpNNN` code pages Windows looks up itself -
  are unknown, as they are to CPython anywhere but Windows.
- **A few single-byte codecs spell a character more than one way**: `mac_arabic`, for
  instance, gives the space and ASCII punctuation a second byte; `src/codecs.rs` shows which by
  a character appearing twice in a table.  Such text has no one spelling, so a search for it
  does not search those files and says so, and `Source::encode` refuses to write it.
- **EBCDIC codecs** such as `cp037` decode as CPython does, which means a file declaring one is
  decoded whole in EBCDIC, its ASCII declaration line included.  CPython then fails to parse
  it, and quaff reads it as the same nonsense.

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
4. If the tables now come from another CPython release, name it in the attribution in
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
