// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Deciding what encoding a file is in, from what declares it.
//!
//! [`SOURCES`] lists where an encoding can come from, the most explicit and closest to the
//! file's bytes first, and the first that applies decides: later ones are not consulted, so
//! declarations never conflict.  A source is added or moved by changing that list alone.  When
//! none applies the encoding is unknown, and quaff never guesses: a search reports possible
//! matches there instead of hits, and nothing is ever edited.

use std::fmt;
use std::path::{Path, PathBuf};

use quaffed_encoding::{self as encoding, Encoding, Named, Refusal, Unproven};

use crate::contents::Contents;
use crate::editorconfig::EditorConfig;
use crate::workspace::{self, Origin, Workspace};

/// The declarations kept outside a file that its encoding may come from.
pub struct Declarations {
    /// The project's version control system, if there is a project.
    pub workspace: Option<Box<dyn Workspace>>,
    /// `EditorConfig`, up to the project root or where quaff runs.
    pub editorconfig: EditorConfig,
    /// The directory paths in messages are relative to.
    pub cwd: PathBuf,
    /// Where every file of the run is read through, the declaring files and the searched ones
    /// alike, so that a declaring file also searched is read once.
    pub contents: Contents,
}

/// A file to decide the encoding of.
pub struct File<'a> {
    /// Its absolute path.
    pub path: &'a Path,
    /// Its bytes.
    pub bytes: &'a [u8],
}

impl File<'_> {
    /// Whether it is Python source, by its name, as `architecture/search.md` lists them.
    fn is_python(&self) -> bool {
        self.path
            .extension()
            .is_some_and(|e| e == "py" || e == "pyi" || e == "pyw")
    }
}

/// What decides a file's encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Its byte-order mark.
    ByteOrderMark,
    /// A Python file's coding line, on this line.
    CodingLine(usize),
    /// A version control system's declaration, or `EditorConfig`'s.
    Declared(Origin),
    /// Its bytes are proven UTF-8.
    ProvenUtf8,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::ByteOrderMark => write!(f, "its byte-order mark"),
            Source::CodingLine(line) => write!(f, "the coding line on line {line}"),
            Source::Declared(origin) => write!(
                f,
                "{} on line {} of {:?}",
                origin.kind, origin.line, origin.file
            ),
            Source::ProvenUtf8 => write!(f, "its bytes being valid UTF-8"),
        }
    }
}

/// Why a file's encoding is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unknown {
    /// It is not valid UTF-8 and nothing declares its encoding.
    NotUtf8,
    /// It is valid UTF-8 but switches into a multi-byte set the ISO-2022 way, `ESC $`.
    Iso2022Switch,
    /// Something declares an encoding quaff does not read.
    Unreadable {
        /// The name, as the declaration spells it.
        name: String,
        /// The `CPython` codec it names, if it names one quaff does not read; `None` if it names
        /// no encoding quaff knows.
        codec: Option<&'static str>,
        /// What declares it.
        source: Source,
    },
}

impl Unknown {
    /// Whether ASCII text would be the same bytes in the file's encoding, as it is in every
    /// encoding but UTF-16 and UTF-32: `false` only for a declaration of one of those by a name
    /// quaff does not read, such as plain `UTF-16`, which leaves the byte order to a mark.
    #[must_use]
    pub fn ascii_compatible(&self) -> bool {
        !matches!(
            self,
            Unknown::Unreadable { codec: Some(codec), .. }
                if codec.starts_with("utf_16") || codec.starts_with("utf_32")
        )
    }
}

impl fmt::Display for Unknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unknown::NotUtf8 => write!(f, "it is not UTF-8, and nothing declares its encoding"),
            Unknown::Iso2022Switch => write!(
                f,
                "it switches into a multi-byte character set the ISO-2022 way, with ESC $, so \
                 may not be the UTF-8 it looks like, and nothing declares its encoding"
            ),
            Unknown::Unreadable {
                name,
                codec: Some(codec),
                source,
            } => write!(
                f,
                "{source} declares {name:?}, Python's {codec:?}, which quaff does not read"
            ),
            Unknown::Unreadable {
                name,
                codec: None,
                source,
            } => write!(
                f,
                "{source} declares {name:?}, which is not an encoding quaff knows"
            ),
        }
    }
}

/// What a file is, once its encoding is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Binary, by git's test: a NUL in its first 8000 bytes.
    Binary,
    /// Text in a known encoding, starting `body` bytes in, after any byte-order mark.
    Known {
        /// The encoding.
        encoding: Encoding,
        /// How many bytes of byte-order mark come before the text.
        body: usize,
        /// What decided it.
        source: Source,
    },
    /// Text in an encoding quaff does not know.
    Unknown(Unknown),
    /// A Python file `CPython` refuses to read whatever it holds, so quaff does not search it.
    Refused(Refusal),
}

/// One place an encoding can come from: the file's decision if this source makes one.
type Decide = fn(&File, &mut Declarations) -> Result<Option<Resolution>, workspace::Error>;

/// Where a file's encoding comes from, most explicit and closest to its bytes first.  The first
/// that decides wins.  The last always decides, so every file gets an answer.
const SOURCES: [Decide; 5] = [
    |file, _| Ok(byte_order_mark(file)),
    |file, _| Ok(coding_line(file)),
    version_control,
    editorconfig,
    |file, _| Ok(Some(proven_utf8(file))),
];

/// Decides what `file` is.
///
/// git's binary test - a NUL in the first 8000 bytes - applies to whatever decides, except a
/// byte-order mark or a declaration of UTF-16 or UTF-32, whose text is full of NULs.
///
/// # Errors
///
/// Returns a [`workspace::Error`] when a file that could declare the encoding cannot be read.
pub fn resolve(
    file: &File,
    declarations: &mut Declarations,
) -> Result<Resolution, workspace::Error> {
    for source in SOURCES {
        let Some(decided) = source(file, declarations)? else {
            continue;
        };
        // A wide encoding's text is full of NULs, so a mark or a declaration of one - even
        // UTF-16 or UTF-32 by a name quaff does not read - exempts a file from the binary test.
        let wide = match &decided {
            Resolution::Known {
                encoding:
                    Encoding::Utf16Le | Encoding::Utf16Be | Encoding::Utf32Le | Encoding::Utf32Be,
                ..
            }
            | Resolution::Known {
                source: Source::ByteOrderMark,
                ..
            } => true,
            Resolution::Unknown(unknown) => !unknown.ascii_compatible(),
            _ => false,
        };
        if !wide && encoding::looks_binary(file.bytes) {
            return Ok(Resolution::Binary);
        }
        return Ok(decided);
    }
    unreachable!("the last source always decides")
}

/// A byte-order mark.  A Python file's coding line must agree with a UTF-8 one, as `CPython`
/// requires.
fn byte_order_mark(file: &File) -> Option<Resolution> {
    let (encoding, body) = encoding::byte_order_mark(file.bytes)?;
    if encoding == Encoding::Utf8
        && file.is_python()
        && let Err(refusal) = encoding::python::declared_encoding(file.bytes)
    {
        return Some(Resolution::Refused(refusal));
    }
    Some(Resolution::Known {
        encoding,
        body,
        source: Source::ByteOrderMark,
    })
}

/// A Python file's coding line, read as `CPython` reads it.
fn coding_line(file: &File) -> Option<Resolution> {
    if !file.is_python() {
        return None;
    }
    match encoding::python::declared_encoding(file.bytes) {
        Ok(None) => None,
        Ok(Some((declared, line))) => Some(Resolution::Known {
            encoding: declared.into(),
            body: 0,
            source: Source::CodingLine(line),
        }),
        Err(Refusal::Unread { name, codec, line }) => {
            Some(Resolution::Unknown(Unknown::Unreadable {
                name,
                codec: Some(codec),
                source: Source::CodingLine(line),
            }))
        }
        Err(refusal) => Some(Resolution::Refused(refusal)),
    }
}

/// The project's version control system, such as git's `working-tree-encoding` attribute.
fn version_control(
    file: &File,
    declarations: &mut Declarations,
) -> Result<Option<Resolution>, workspace::Error> {
    let Some(workspace) = declarations.workspace.as_mut() else {
        return Ok(None);
    };
    let Some(declared) = workspace.declared_encoding(file.path, &declarations.cwd)? else {
        return Ok(None);
    };
    // git writes a byte-order mark for these, which has decided before a name is read; the
    // name without the suffix is the encoding.
    let name = declared
        .name
        .strip_suffix("-BOM")
        .or_else(|| declared.name.strip_suffix("-bom"))
        .unwrap_or(&declared.name);
    Ok(Some(by_name(
        name,
        &declared.name,
        Source::Declared(declared.origin),
    )))
}

/// `EditorConfig`'s `charset`.
fn editorconfig(
    file: &File,
    declarations: &mut Declarations,
) -> Result<Option<Resolution>, workspace::Error> {
    let Some(declared) = declarations
        .editorconfig
        .charset(file.path, &declarations.cwd)?
    else {
        return Ok(None);
    };
    // EditorConfig's own names for its five values; `utf-8-bom` is UTF-8 whose mark, if there,
    // has already decided.
    let name = match declared.name.as_str() {
        "utf-8-bom" => "utf-8",
        other => other,
    };
    Ok(Some(by_name(
        name,
        &declared.name,
        Source::Declared(declared.origin),
    )))
}

/// The bytes being proven UTF-8, and otherwise an unknown encoding.
fn proven_utf8(file: &File) -> Resolution {
    match encoding::proven_utf8(file.bytes) {
        Ok(()) => Resolution::Known {
            encoding: Encoding::Utf8,
            body: 0,
            source: Source::ProvenUtf8,
        },
        Err(Unproven::Invalid) => Resolution::Unknown(Unknown::NotUtf8),
        Err(Unproven::Iso2022Switch) => Resolution::Unknown(Unknown::Iso2022Switch),
    }
}

/// What a declaration of the encoding `name` makes of a file, `spelled` being how the
/// declaration wrote it.
fn by_name(name: &str, spelled: &str, source: Source) -> Resolution {
    match encoding::named(name) {
        Named::Read(encoding) => Resolution::Known {
            encoding,
            body: 0,
            source,
        },
        Named::Unread(codec) => Resolution::Unknown(Unknown::Unreadable {
            name: spelled.to_owned(),
            codec: Some(codec),
            source,
        }),
        Named::Unknown => Resolution::Unknown(Unknown::Unreadable {
            name: spelled.to_owned(),
            codec: None,
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use quaffed_encoding::{Codec, MultiByteCodec};

    /// A temporary project with `files`, and a `.git` directory when `git` is set.
    struct Project(PathBuf);

    impl Project {
        fn new(name: &str, git: bool, files: &[(&str, &[u8])]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("quaffed-resolve-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let dir = fs::canonicalize(dir).unwrap();
            if git {
                fs::create_dir_all(dir.join(".git/info")).unwrap();
            }
            for (path, bytes) in files {
                let path = dir.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, bytes).unwrap();
            }
            Project(dir)
        }

        fn resolve(&self, path: &str) -> Resolution {
            let contents = Contents::default();
            let mut declarations = Declarations {
                workspace: workspace::find(&self.0, &contents),
                editorconfig: EditorConfig::new(&self.0, &contents),
                cwd: self.0.clone(),
                contents,
            };
            let path = self.0.join(path);
            let bytes = fs::read(&path).unwrap();
            resolve(
                &File {
                    path: &path,
                    bytes: &bytes,
                },
                &mut declarations,
            )
            .unwrap()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn encoding_of(resolution: Resolution) -> Encoding {
        match resolution {
            Resolution::Known { encoding, .. } => encoding,
            other => panic!("expected a known encoding, got {other:?}"),
        }
    }

    fn sjis() -> Encoding {
        Encoding::MultiByte(MultiByteCodec::named("shift_jis").unwrap())
    }

    /// Every answer `scripts/generate-declaration-cases` recorded from git and from
    /// `EditorConfig`'s reference core, for the same trees rebuilt here: quaff's readers must
    /// agree with the tools themselves.
    #[test]
    fn the_declaration_readers_agree_with_git_and_editorconfig() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/declarations.txt");
        let data = fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {path}: {err}"));
        let mut project: Option<Project> = None;
        let mut checked = 0;
        for (number, line) in data.lines().enumerate() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let at = format!("{path}:{}", number + 1);
            match fields.as_slice() {
                ["case", name] => project = Some(Project::new(name, true, &[])),
                ["repository", dir] => {
                    let root = &project.as_ref().expect("a case first").0;
                    fs::create_dir_all(root.join(dir).join(".git/info")).unwrap();
                }
                ["file", file, hex] => {
                    let bytes: Vec<u8> = (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
                        .collect();
                    let root = &project.as_ref().expect("a case first").0;
                    let target = root.join(file);
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    fs::write(target, bytes).unwrap();
                }
                [kind, file, expected] => {
                    let root = &project.as_ref().expect("a case first").0;
                    let target = root.join(file);
                    let declared = if *kind == "attribute" {
                        workspace::find(root, &Contents::default())
                            .expect("a git workspace")
                            .declared_encoding(&target, root)
                            .unwrap()
                    } else {
                        EditorConfig::new(root, &Contents::default())
                            .charset(&target, root)
                            .unwrap()
                    };
                    let found = declared.map_or("-".to_owned(), |d| d.name);
                    assert_eq!(&found, expected, "{at}: {kind} of {file}");
                    checked += 1;
                }
                _ => panic!("{at}: cannot read {line:?}"),
            }
        }
        // A file that held no answers would pass having checked nothing.
        assert!(checked >= 80, "only {checked} answers in {path}");
    }

    #[test]
    fn each_source_names_itself() {
        assert_eq!(Source::ByteOrderMark.to_string(), "its byte-order mark");
        assert_eq!(
            Source::ProvenUtf8.to_string(),
            "its bytes being valid UTF-8"
        );
        assert_eq!(
            Source::CodingLine(2).to_string(),
            "the coding line on line 2"
        );
    }

    #[test]
    fn info_attributes_is_found_through_a_git_file() {
        // A linked worktree, as `git worktree add` lays one out: its .git file points to the
        // worktree's git directory, whose commondir points back to the main one, which holds
        // info/attributes.  A submodule's .git file points to its directory directly.
        let project = Project::new(
            "pointers",
            false,
            &[
                (
                    "main/.git/info/attributes",
                    b"*.c working-tree-encoding=EUC-JP\n",
                ),
                ("main/.git/worktrees/linked/commondir", b"../..\n"),
                (
                    "main/.git/modules/sub/info/attributes",
                    b"*.c working-tree-encoding=CP932\n",
                ),
                ("linked/a.c", b"x\n"),
                ("sub/a.c", b"x\n"),
            ],
        );
        let pointer = format!(
            "gitdir: {}\n",
            project.0.join("main/.git/worktrees/linked").display()
        );
        fs::write(project.0.join("linked/.git"), pointer).unwrap();
        fs::write(
            project.0.join("sub/.git"),
            "gitdir: ../main/.git/modules/sub\n",
        )
        .unwrap();
        let declared = |path: &str| {
            let target = project.0.join(path);
            workspace::find(target.parent().unwrap(), &Contents::default())
                .expect("a git workspace")
                .declared_encoding(&target, &project.0)
                .unwrap()
                .map(|d| d.name)
        };
        assert_eq!(declared("linked/a.c"), Some("EUC-JP".to_owned()));
        assert_eq!(declared("sub/a.c"), Some("CP932".to_owned()));
    }

    #[test]
    fn the_first_source_that_applies_decides() {
        let project = Project::new(
            "order",
            true,
            &[
                (
                    ".gitattributes",
                    b"*.py working-tree-encoding=EUC-JP\n*.c working-tree-encoding=SHIFT-JIS\n",
                ),
                (".editorconfig", b"[*]\ncharset = latin1\n"),
                // The coding line comes before the project's declaration.
                ("a.py", b"# coding: shift_jis\nx = 1\n"),
                // The project's declaration comes before EditorConfig's.
                ("b.c", b"int x;\n"),
                // EditorConfig's comes before the bytes being valid UTF-8.
                ("c.txt", b"plain\n"),
                // A byte-order mark comes before everything.
                ("d.c", b"\xEF\xBB\xBFint x;\n"),
            ],
        );
        assert_eq!(encoding_of(project.resolve("a.py")), sjis());
        assert_eq!(encoding_of(project.resolve("b.c")), sjis());
        assert_eq!(
            encoding_of(project.resolve("c.txt")),
            Encoding::SingleByte(Codec::named("latin_1").unwrap())
        );
        assert_eq!(
            project.resolve("d.c"),
            Resolution::Known {
                encoding: Encoding::Utf8,
                body: 3,
                source: Source::ByteOrderMark
            }
        );
    }

    #[test]
    fn with_nothing_declared_valid_utf8_is_proven_and_anything_else_unknown() {
        let project = Project::new(
            "undeclared",
            false,
            &[
                ("plain.txt", "café\n".as_bytes()),
                ("latin.txt", b"caf\xE9\n"),
                ("iso2022.txt", b"x \x1b$B%3\x1b(B\n"),
                ("binary.txt", b"a\0b"),
            ],
        );
        assert_eq!(
            project.resolve("plain.txt"),
            Resolution::Known {
                encoding: Encoding::Utf8,
                body: 0,
                source: Source::ProvenUtf8
            }
        );
        assert_eq!(
            project.resolve("latin.txt"),
            Resolution::Unknown(Unknown::NotUtf8)
        );
        assert_eq!(
            project.resolve("iso2022.txt"),
            Resolution::Unknown(Unknown::Iso2022Switch)
        );
        assert_eq!(project.resolve("binary.txt"), Resolution::Binary);
    }

    #[test]
    fn a_declaration_quaff_cannot_read_leaves_the_encoding_unknown_naming_it() {
        let project = Project::new(
            "unread",
            true,
            &[
                (
                    ".gitattributes",
                    b"*.c working-tree-encoding=GBK\n*.h working-tree-encoding=NOPE\n",
                ),
                ("a.c", b"x\n"),
                ("a.h", b"x\n"),
                ("a.py", b"# coding: gbk\n"),
            ],
        );
        let gitattributes = |line| {
            Source::Declared(Origin {
                kind: "working-tree-encoding",
                file: ".gitattributes".into(),
                line,
            })
        };
        assert_eq!(
            project.resolve("a.c"),
            Resolution::Unknown(Unknown::Unreadable {
                name: "GBK".into(),
                codec: Some("gbk"),
                source: gitattributes(1),
            })
        );
        assert_eq!(
            project.resolve("a.h"),
            Resolution::Unknown(Unknown::Unreadable {
                name: "NOPE".into(),
                codec: None,
                source: gitattributes(2),
            })
        );
        assert_eq!(
            project.resolve("a.py"),
            Resolution::Unknown(Unknown::Unreadable {
                name: "gbk".into(),
                codec: Some("gbk"),
                source: Source::CodingLine(1),
            })
        );
        assert_eq!(
            Unknown::Unreadable {
                name: "GBK".into(),
                codec: Some("gbk"),
                source: gitattributes(1),
            }
            .to_string(),
            "working-tree-encoding on line 1 of \".gitattributes\" declares \"GBK\", Python's \
             \"gbk\", which quaff does not read"
        );
    }

    #[test]
    fn a_declared_utf16_file_is_not_binary_and_a_declared_8_bit_one_is() {
        let utf16: Vec<u8> = "hi\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let project = Project::new(
            "wide",
            true,
            &[
                (".gitattributes", b"*.ps1 working-tree-encoding=UTF-16LE-BOM\n*.dat working-tree-encoding=latin1\n"),
                ("a.ps1", &utf16),
                ("a.dat", b"a\0b"),
            ],
        );
        assert_eq!(encoding_of(project.resolve("a.ps1")), Encoding::Utf16Le);
        assert_eq!(project.resolve("a.dat"), Resolution::Binary);
    }

    #[test]
    fn python_refusals_stay_refusals_and_a_mark_must_agree_with_the_coding_line() {
        let project = Project::new(
            "refused",
            false,
            &[
                ("typo.py", b"# coding: uft-8\n"),
                ("marked.py", b"\xEF\xBB\xBF# coding: latin-1\n"),
            ],
        );
        assert!(matches!(
            project.resolve("typo.py"),
            Resolution::Refused(Refusal::Unknown { .. })
        ));
        assert!(matches!(
            project.resolve("marked.py"),
            Resolution::Refused(Refusal::MarkConflict { .. })
        ));
    }

    #[test]
    fn editorconfig_above_the_boundary_is_not_read() {
        let project = Project::new(
            "boundary",
            true,
            &[
                (".editorconfig", b"root = true\n[*]\ncharset = latin1\n"),
                ("sub/a.txt", b"x\n"),
            ],
        );
        // Asked with the subdirectory as the boundary, the project's file is above it.
        let mut declarations = Declarations {
            workspace: None,
            editorconfig: EditorConfig::new(&project.0.join("sub"), &Contents::default()),
            cwd: project.0.clone(),
            contents: Contents::default(),
        };
        let path = project.0.join("sub/a.txt");
        assert_eq!(
            resolve(
                &File {
                    path: &path,
                    bytes: b"x\n"
                },
                &mut declarations
            )
            .unwrap(),
            Resolution::Known {
                encoding: Encoding::Utf8,
                body: 0,
                source: Source::ProvenUtf8
            }
        );
    }
}
