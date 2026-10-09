# Encodings

`crates/encoding`, the `quaffed-encoding` crate: what a file's bytes are, as far as quaff can tell
without guessing.  It decides what any file is from its byte-order mark and its bytes, and reads
a Python file the way CPython does, by the encoding the file declares.  Textual search uses it
(`search.md`); so will anything that reads Python source, which takes the text it decodes rather
than decoding for itself.

## Any file

`classify` decides from a byte-order mark - UTF-8, UTF-16 or UTF-32 - then git's binary test,
then whether the bytes are valid UTF-8, and otherwise calls them an unknown 8-bit encoding.
`search.md` gives the table and what a search does with each.

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
| declares an encoding Python reads and quaff does not | quaff reads UTF-8 and single-byte encodings only, which is quaff's limit, not Python's |

`Source` keeps the text, the encoding and whether there was a mark.  `Source::encode` writes text
back the same way, mark first, so a file's own decoded text encodes to its exact bytes.

**For a reader that tolerates bad bytes**, `declared_encoding` resolves the declaration alone and
refuses only what no content could make readable: an unknown name, a codec not of text,
`undefined`, an encoding quaff does not read, or a conflict with a mark.  `classify_python` uses
it: a UTF-16 or UTF-32 mark, or a NUL in the first 8000 bytes, decides as for any file; otherwise a declaration decides,
trusted as a byte-order mark is, so bytes it does not decode count as one character each; with
none, the file is classified as any other.  An undeclared Python file that is not valid UTF-8
stays an unknown 8-bit encoding there rather than being read as UTF-8, so that a non-ASCII query
is reported as not searched rather than silently matching nothing.

## CPython's codecs

`src/codecs.rs` is generated from the oracle by `scripts/generate-codecs`, never edited.  It
lists every codec module of CPython's `encodings` package with its kind, every alias, and for
each single-byte codec what each byte decodes to.  A codec is single-byte if CPython decodes it
from a table, or it is `ascii`, `latin_1` or `charmap`; the generator checks every such codec
byte by byte against CPython's own decoder and refuses to write one that is not single-byte
after all.  `latin-1` is true Latin-1, every byte its own code point, as CPython has it, and not
the web's reading of it as `windows-1252`; that difference is why quaff does not use the web's
encoding tables.

**Accepted limitations:**

- **Multi-byte, stateful and escape-based encodings are not read**: Shift-JIS, for instance,
  or UTF-16 declared by name.  `src/codecs.rs` lists every such codec as `Unread`.  Such a file
  is refused by name with the reason.  Reading one means giving its kind a decoder, not
  changing how names resolve.  Searching one honestly
  needs more than spelling the query in its bytes: in Shift-JIS and Big5 the second byte of a
  character can be an ASCII byte, so a byte search would find matches inside characters.
- **Windows-only codecs** - `mbcs`, `oem` and the `cpNNN` code pages Windows looks up itself -
  are unknown, as they are to CPython anywhere but Windows.
- **A few single-byte codecs spell a character more than one way**: `mac_arabic`, for
  instance, gives the space and ASCII punctuation a second byte; `src/codecs.rs` shows which by
  a character appearing twice in a table.  Such text has no one spelling, so a search for it does not search those files
  and says so, and `Source::encode` refuses to write it.
- **EBCDIC codecs** such as `cp037` decode as CPython does, which means a file declaring one is
  decoded whole in EBCDIC, its ASCII declaration line included.  CPython then fails to parse
  it, and quaff reads it as the same nonsense.

### Checklist: regenerating the codec tables

To read a new single-byte codec, or to follow a CPython release that changed one:

1. Run `scripts/generate-codecs` with the oracle's Python, the CPython `corpus/real-world.toml`
   names.  It stops naming the version it needs if it is run with another.
2. Read the diff of `src/codecs.rs`: every changed table or kind is a change to how quaff reads
   files.
3. Run `cargo test -p quaffed-encoding`.  The tables must stay sorted, and every single-byte
   codec must spell CR and LF one way each, which a textual search relies on to find line
   endings.
4. Update this page in the same change.

### Checklist: reading a multi-byte encoding

1. Give `Module` a kind for it in `src/python.rs`, and make the generator emit that kind for its
   codecs instead of `Unread`.
2. Give `SourceEncoding` and `Encoding` a variant for it, with decoding, `encode` and `each_char`.
3. Decide how a textual search finds a query in it honestly before searching it (`search.md`):
   a byte search is only sound where no character's bytes can contain another's.
4. Update the refusal message for `Unread`, this page and `search.md` in the same change.
