// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Execution: each file searched for the patterns that look at it, batch by batch, with its
//! edits held, not written.
//!
//! In: the [`Plan`] and the [`Files`].  Out: what was [`Searched`] for, which
//! evaluation and reporting read.

use std::borrow::Cow;
use std::io;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::thread;

use quaffed_encoding::{self as encoding, Encoding, Named};
use quaffed_representation::program::TextPattern;
use quaffed_syntax::{CompileError, FrontEnd, Pattern, Refused, Rewrite};

use super::{
    compile::Compiled,
    failure::Failure,
    parallel,
    plan::{Plan, Replacement, Searching, Wanted},
    report::name_of,
    scoping::{Files, Projects, ToSearch},
};
use crate::changes::{self, Changes};
use crate::filter;
use crate::output;
use crate::resolve::{self, Declarations, Resolution, Unknown};
use crate::rewrite;
use crate::scope;
use crate::search::{self, Match};
use crate::structural;
use crate::workspace::Skipped;

/// Searches each of `files` for the patterns of `plan` that look at it, holding the edits its
/// replaces make, not writing them: up to `limit` files at once, or as many as the machine can
/// search at once with no limit, and what each gave taken in discovery order whatever the number.
///
/// # Errors
///
/// Returns a [`Failure`] for a file that cannot be read, or whose encoding's declarations cannot
/// be - the first in discovery order, as it would be one file at a time - and refuses the run for
/// files a structural search needs that quaff cannot parse: a count that left them out could not
/// be trusted.
pub(super) fn execute(
    plan: &Plan,
    files: &Files,
    limit: Option<NonZeroUsize>,
) -> Result<Searched, Failure> {
    let locale = locale();
    let (jobs, one_at_a_time) = match jobs(limit) {
        Ok(jobs) => (jobs, None),
        Err(err) => (NonZeroUsize::MIN, Some(err.to_string())),
    };
    let mut searched = search_files(
        &files.to_search,
        scope::Roots {
            cwd: &files.cwd,
            root: &files.root,
        },
        &plan.searching,
        &plan.compiled,
        &files.projects,
        locale.as_ref(),
        jobs,
    )?;
    searched.one_at_a_time = one_at_a_time;
    // A structural search over files it cannot parse would give a count that cannot be trusted,
    // whether their language could parse them or not, so the whole run is refused, naming each,
    // before anything is printed.
    if !searched.unparsed.is_empty() {
        return Err(Failure::Unparseable {
            files: searched.unparsed.clone(),
        });
    }
    Ok(searched)
}

/// How many files a run searches at once: `limit` if the user set one, and otherwise as many as
/// the machine can run at once.
///
/// # Errors
///
/// Returns the system's error, verbatim, when no limit was set and how many the machine can run
/// at once cannot be told.
fn jobs(limit: Option<NonZeroUsize>) -> io::Result<NonZeroUsize> {
    limit.map_or_else(thread::available_parallelism, Ok)
}

/// Stretches an edit wrote over, each where it was in the bytes before and where what was written
/// stands in those after, as [`Changes::between`] takes them.
type Over = Vec<(Range<usize>, Range<usize>)>;

/// `over`, stretches a structural rewrite gave in decoded text - from `source`'s text to `edited` -
/// as the same stretches in the file's bytes before and after.  Every encoding quaff decodes a
/// file in spells each character on its own, so a stretch's bytes are its characters' spellings,
/// after the byte-order mark if there is one.  A stretch that does not fall on characters in
/// either text is not one a rewrite gives; then nothing is claimed, and the changes are one
/// stretch from the first byte that differs to the last - exact, but holding about a whole copy of
/// the file for the rest of the run, as a run did before changes were held as stretches.
fn in_bytes(
    source: &encoding::Source,
    edited: &str,
    over: &[(Range<usize>, Range<usize>)],
) -> Over {
    let plain = encoding::Source {
        text: String::new(),
        encoding: source.encoding,
        byte_order_mark: false,
    };
    // Encoding nothing gives the byte-order mark alone, where the file has one.
    let mark = source.encode("").map_or(0, |bytes| bytes.len());
    // Each offset's place in the bytes, found by spelling the text up to it, piece by piece, in
    // order: a sorted list of pairs, looked up by binary search.
    let placed = |text: &str, mut offsets: Vec<usize>| -> Option<Vec<(usize, usize)>> {
        offsets.sort_unstable();
        offsets.dedup();
        let mut placed = Vec::with_capacity(offsets.len());
        let (mut from, mut at) = (0, mark);
        for offset in offsets {
            at += plain.encode(text.get(from..offset)?).ok()?.len();
            placed.push((offset, at));
            from = offset;
        }
        Some(placed)
    };
    let old = placed(
        &source.text,
        over.iter()
            .flat_map(|(was, _)| [was.start, was.end])
            .collect(),
    );
    let new = placed(
        edited,
        over.iter()
            .flat_map(|(_, now)| [now.start, now.end])
            .collect(),
    );
    let (Some(old), Some(new)) = (old, new) else {
        return Vec::new();
    };
    let at = |placed: &[(usize, usize)], offset: usize| {
        let index = placed
            .binary_search_by_key(&offset, |&(offset, _)| offset)
            .expect("every offset was placed");
        placed[index].1
    };
    over.iter()
        .map(|(was, now)| {
            (
                at(&old, was.start)..at(&old, was.end),
                at(&new, now.start)..at(&new, now.end),
            )
        })
        .collect()
}

/// Whether a file's bytes parse in `language`, decoded as the language decodes them; where and why
/// not.
pub(super) fn parses(language: &dyn FrontEnd, bytes: &[u8]) -> Result<(), String> {
    let source = language
        .decode(bytes)
        .map_err(|refusal| refusal.to_string())?;
    structural::search(language, &source.text, &[])
        .map(|_| ())
        .map_err(|unparsed| unparsed_at(&unparsed))
}

/// The encoding the user's locale names, if it names one other than UTF-8 that quaff reads, from
/// `LC_ALL`, `LC_CTYPE` or `LANG`, the first that is set, as `setlocale` reads them.
///
/// Used only for a hint in a message, never to decide anything: a locale describes the user's
/// terminal, not the project, and the same command must give the same answer on every machine.
pub(super) fn locale() -> Option<(String, Encoding)> {
    let value = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))?;
    let codeset = value.split_once('.')?.1;
    let codeset = codeset.split('@').next().unwrap_or(codeset);
    match encoding::named(codeset) {
        Named::Read(Encoding::Utf8) | Named::Unread(_) | Named::Unknown => None,
        Named::Read(encoding) => Some((codeset.to_owned(), encoding)),
    }
}

/// One match a search found, and for a structural one where the code it covers is, which is
/// printed in place of the pattern's text.
///
/// `Copy`, so that it cannot own memory: a run holds every match until it prints, and a copy of
/// each match's code would make that the square of a block's length where the matches overlap.
#[derive(Clone, Copy)]
pub(super) struct Hit {
    pub(super) at: Match,
    pub(super) code: Option<Code>,
}

// A field that owns memory, such as a `String` of the matched code, would not be `Copy`: what a
// match covers is read from the file it was found in when it is printed.
const _: () = {
    const fn holds_no_memory<T: Copy>() {}
    holds_no_memory::<Hit>();
};

/// Where a structural match's code is: in which of the files the run holds, in which version of
/// it - as read, or as a replace left it - and where in that version's decoded text.  Held rather
/// than a copy of the code, since many long matches can overlap, and copies of them all would
/// hold far more than the code itself.
#[derive(Clone, Copy)]
pub(super) struct Code {
    pub(super) held: usize,
    pub(super) version: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// What the run holds of a file until it ends, because a replace changed it or a structural match
/// was found in it: whichever is smaller of the file and the code its matches cover.
pub(super) enum Held {
    /// The whole file: its bytes as read, and what each replace that changed it changed, in
    /// order, from which any version of it is built.  A file a replace changed is always held
    /// so, since it is checked and written from these.
    File {
        original: crate::contents::Bytes,
        changes: Vec<Changes>,
        /// Its language, if quaff parses it, which decodes it for a structural match's code.
        language: Option<&'static dyn FrontEnd>,
    },
    /// The code the structural matches found in a file no replace changed cover, one match's
    /// after another, where that is less than the file: a few short matches in a large file, as
    /// most searches find.  Each match's [`Code`] is where its code stands in this.
    Copied(String),
}

impl Held {
    /// The file's bytes as read and the changes made to it, for a file held whole, as every file
    /// a replace changed is.
    pub(super) fn file(&self) -> (&crate::contents::Bytes, &[Changes]) {
        match self {
            Held::File {
                original, changes, ..
            } => (original, changes),
            Held::Copied(_) => unreachable!("a file a replace changed is held whole"),
        }
    }

    /// The decoded text a match's [`Code`] places it in: the version it was found in, built and
    /// decoded again, for a file held whole, and the copies for one whose matches were copied.
    pub(super) fn text(&self, version: usize) -> Cow<'_, str> {
        match self {
            Held::File {
                original,
                changes,
                language,
            } => {
                let bytes = if version == 0 {
                    Cow::Borrowed(&original[..])
                } else {
                    Cow::Owned(changes::applied(original, &changes[..version]))
                };
                let source = language
                    .expect("a file a structural match was found in has a language")
                    .decode(&bytes)
                    .expect("a version a match was found in decoded then");
                Cow::Owned(source.text)
            }
            Held::Copied(copies) => Cow::Borrowed(copies),
        }
    }

    /// How many bytes it holds.
    #[cfg(test)]
    pub(super) fn size(&self) -> usize {
        match self {
            Held::File {
                original, changes, ..
            } => original.len() + changes.iter().map(Changes::held).sum::<usize>(),
            Held::Copied(copies) => copies.len(),
        }
    }
}

/// What one of the run's patterns found, over the files searched: everything a run knows about it
/// once the search is over, in one place, so that evaluating and reporting it reads one record.
#[derive(Default)]
pub(super) struct Found {
    /// The files with matches, in discovery order.
    pub(super) matches: Vec<(PathBuf, Vec<Hit>)>,
    /// The files in a language quaff parses in its scope, binary ones included: none for a
    /// textual pattern.
    pub(super) supported: usize,
    /// The files in a language quaff parses searched for it.
    pub(super) structural: usize,
    /// The other files in its scope, which it does not apply to.
    pub(super) not_applicable: usize,
    /// The files in its scope in a language quaff parses that cannot read it, binary ones aside.
    pub(super) unread: usize,
    /// The files of unknown encoding holding bytes it would match, by the paths a message shows,
    /// with the lines they are on and why the encoding is unknown.  Never counted, and never
    /// edited.
    pub(super) possible: Vec<(PathBuf, Vec<usize>, Unknown)>,
    /// The files not searched for it because it has no one spelling there, by the paths a message
    /// shows, and why.
    pub(super) unspelled: Vec<(PathBuf, Unspelled)>,
    /// For a replaced pattern, the files whose encoding cannot write its replacement, by the paths
    /// a message shows, with the encoding as `CPython` names its codec, the text it cannot write,
    /// and whether it spells that text more than one way rather than not at all.
    pub(super) unwritable: Vec<(PathBuf, &'static str, String, bool)>,
    /// For a structural replace, the files it cannot be made in as written, by the paths a message
    /// shows, with where and why: refused once its count holds, as an edit its files' encodings
    /// cannot write is, so that a count that does not hold is told first.
    pub(super) edits_refused: Vec<(PathBuf, String)>,
    /// For a structural replace, the places it was made as written but may have changed what the
    /// code means, by the paths a message shows, with where and why: noted only for the
    /// statements the run reached.
    pub(super) questioned: Vec<(PathBuf, String)>,
    /// For a structural pattern, the files an earlier replace left unparseable, though they parse
    /// on disk, with where and why and the replaces that changed them: the pattern cannot be
    /// searched for in them, and the edit is refused.
    pub(super) broken: Vec<(PathBuf, String, Vec<usize>)>,
}

/// What searching found: for one file, as the search of it gives it, or for every file, as those
/// are gathered in discovery order.
pub(super) struct Searched {
    /// What each of the run's patterns found, by its index.
    pub(super) found: Vec<Found>,
    /// Files a structural search needs that quaff cannot parse, by the paths a message shows,
    /// with where and why.
    pub(super) unparsed: Vec<(PathBuf, String)>,
    /// Files in a language quaff parses in some structural pattern's scope, binary ones included.
    pub(super) supported: usize,
    /// Other files in some structural pattern's scope, which have no structure quaff reads.
    pub(super) not_applicable: usize,
    /// Files in a language quaff parses in the scope of some structural pattern the language
    /// cannot read, binary ones aside, so not searched for it.
    pub(super) unread: usize,
    /// Every file a replace changed, held until the run ends.
    pub(super) changed: Vec<Changed>,
    /// Every file the run holds until it ends, which a [`Changed`] or a [`Code`] names by its place
    /// here.
    pub(super) held: Vec<Held>,
    /// The patterns, by their index, that structural matches were found for in the file being
    /// searched, whose last entry in [`Found::matches`] is that file's: it must be held.
    cited: Vec<usize>,
    /// Files whose own declared encoding their language's reader refuses, whatever they hold, so
    /// are searched for nothing, by the paths a message shows, with why.
    pub(super) refused: Vec<(PathBuf, Refused)>,
    /// Files of unknown encoding that read cleanly in the locale's encoding, which a message
    /// mentions as a hint, and that encoding's name as the locale spells it.
    pub(super) hinted: Vec<PathBuf>,
    pub(super) hint: Option<String>,
    /// Where the project's version control system declares encodings, for the hint.
    pub(super) declares_in: Option<&'static str>,
    /// Binary files, counted once whatever the number of patterns.
    pub(super) binary: usize,
    /// The version control system's files consulted for an encoding that have lines the system
    /// could not understand, each with the project it was consulted in: for one file, each it
    /// consulted, in order; for every file, each once, where the first file to consult it did.
    pub(super) skipped: Vec<(usize, Skipped)>,
    /// Why the run searched one file at a time with no limit set: the system's error telling how
    /// many files the machine can search at once.
    pub(super) one_at_a_time: Option<String>,
}

/// A file a replace changed, held in memory until the run is over: by its place among the files
/// held, whose new bytes are built when they are checked and written, one file at a time.
pub(super) struct Changed {
    /// Where it is, to write it.
    pub(super) path: PathBuf,
    /// The path a message shows.
    pub(super) shown: PathBuf,
    /// Its language, if quaff parses it: then it must still parse if it parsed before.
    pub(super) language: Option<&'static dyn FrontEnd>,
    /// Its place in [`Searched::held`].
    pub(super) held: usize,
    /// The replaces that changed it, by their pattern's index.
    pub(super) by: Vec<usize>,
}

/// Why a file was not searched for a pattern.
#[derive(Clone)]
pub(super) enum Unspelled {
    /// Its encoding is known and spells a character of the pattern more than one way.
    Ambiguous(Encoding),
    /// Its encoding is unknown and the pattern is not ASCII.
    Unknown(Unknown),
}

/// The run's patterns in the batches a file is searched in: the patterns between two replaces are
/// searched in one pass over the file as it then is, and each replace changes it for the
/// patterns after.  With no replace, every pattern is in one batch.
enum Batch<'p> {
    /// Search for these, by their index among all the run's patterns: the textual ones, and the
    /// structural ones with their compiled pattern's index.
    Search {
        textual: Vec<(usize, &'p TextPattern)>,
        structural: Vec<(usize, usize)>,
    },
    /// Replace the pattern at this index with this text.
    Replace {
        index: usize,
        pattern: &'p TextPattern,
        with: &'p TextPattern,
    },
    /// Replace the structural pattern at this index, compiled at `compiled`, with the rewrite at
    /// `rewrite`.
    Rewrite {
        index: usize,
        compiled: usize,
        rewrite: usize,
    },
}

/// The run's patterns, `searching`, in batches.
fn batches<'p>(searching: &[Searching<'p>]) -> Vec<Batch<'p>> {
    let mut batches = Vec::new();
    let mut textual = Vec::new();
    let mut structural = Vec::new();
    for (
        index,
        Searching {
            pattern: wanted,
            replacement: with,
            ..
        },
    ) in searching.iter().enumerate()
    {
        if with.is_some() && (!textual.is_empty() || !structural.is_empty()) {
            batches.push(Batch::Search {
                textual: std::mem::take(&mut textual),
                structural: std::mem::take(&mut structural),
            });
        }
        match (wanted, with) {
            (Wanted::Text(pattern), Some(Replacement::Text(with))) => {
                batches.push(Batch::Replace {
                    index,
                    pattern,
                    with,
                });
            }
            (Wanted::Structural { compiled, .. }, Some(Replacement::Structural(rewrite))) => {
                batches.push(Batch::Rewrite {
                    index,
                    compiled: *compiled,
                    rewrite: *rewrite,
                });
            }
            (Wanted::Text(pattern), None) => textual.push((index, *pattern)),
            (Wanted::Structural { compiled, .. }, None) => structural.push((index, *compiled)),
            _ => unreachable!("a replace replaces text with text, and a pattern with a pattern"),
        }
    }
    if !textual.is_empty() || !structural.is_empty() {
        batches.push(Batch::Search {
            textual,
            structural,
        });
    }
    batches
}

/// A file as it was read for the run: where it is, the path a message shows, its bytes on disk,
/// how its encoding was decided, what the run's patterns compile to in its language, and whether
/// it is binary.
struct ReadFile<'a> {
    pub(super) file: &'a Path,
    pub(super) shown: &'a Path,
    pub(super) bytes: &'a [u8],
    resolution: &'a Resolution,
    /// What the run's patterns compile to in its language, if quaff parses it.
    pub(super) compiled: Option<&'a Compiled>,
    pub(super) binary: bool,
}

/// A structural replace as a file is edited by it: its pattern's index among the run's patterns,
/// the compiled pattern, and its rewrite.
struct Rewriting<'a> {
    pub(super) index: usize,
    pub(super) pattern: &'a Pattern,
    /// The rewrite, or why the file's language cannot read it.
    pub(super) rewrite: &'a Result<Rewrite, CompileError>,
}

/// What earlier replaces did to a file: its bytes on disk if they changed it, which did, by
/// their pattern's index, and so which version of it is being searched, for a match to say where
/// it was found.
struct Earlier<'a> {
    on_disk: Option<&'a [u8]>,
    pub(super) by: &'a [usize],
    pub(super) version: Version,
}

/// A version of a file: the place among the files held it is given if it is held, and how many
/// replaces changed it before it.
#[derive(Clone, Copy)]
struct Version {
    pub(super) held: usize,
    pub(super) number: usize,
}

/// What a file is searched for: the batches of patterns, the compiled structural patterns, and
/// whether there is any.
struct Work<'p> {
    batches: Vec<Batch<'p>>,
    /// The structural patterns, rewrites and `where`s, compiled for each language.
    compiled: &'p [Compiled],
    /// The structural patterns, by their index among all the run's patterns, with their place
    /// among the compiled ones.
    structural: Vec<(usize, usize)>,
    /// Where a file's path, as a `FILE` test sees it, is worked out from.
    roots: scope::Roots<'p>,
}

/// Searches each of `files` for each of the run's patterns, `searching`, in order, replacing as
/// each says, reading each file once and deciding its encoding from what declares it.  A pattern
/// after a replace is searched for in the file as the replace left it; the edits are held, not
/// written.  Up to `jobs` files are searched at once.
fn search_files(
    files: &[ToSearch],
    roots: scope::Roots,
    searching: &[Searching],
    compiled: &[Compiled],
    projects: &Projects,
    locale: Option<&(String, Encoding)>,
    jobs: NonZeroUsize,
) -> Result<Searched, Failure> {
    let cwd = roots.cwd;
    let batches = batches(searching);
    let structural = searching
        .iter()
        .enumerate()
        .filter_map(|(i, searching)| match searching.pattern {
            Wanted::Structural { compiled, .. } => Some((i, compiled)),
            Wanted::Text(_) => None,
        })
        .collect();
    let mut searched = Searched::new(searching.len());
    searched.hint = locale.map(|(name, _)| name.clone());
    searched.declares_in = projects.list[0]
        .declarations
        .workspace
        .as_ref()
        .map(|w| w.declares_encodings_in());
    let work = Work {
        batches,
        compiled,
        structural,
        roots,
    };
    // A file is the unit of work: it is taken through every batch, on whichever thread takes
    // it, and what it gave is gathered into the run's in discovery order, so that each pattern's
    // matches stay in that order.
    parallel::in_order(
        files.len(),
        jobs,
        |index| {
            let file = &files[index];
            #[cfg(test)]
            meeting::meet(&file.path);
            let declarations = &projects.list[file.project].declarations;
            let mut in_file = Searched::new(searching.len());
            in_file.file(file, cwd, &work, declarations, locale)?;
            Ok(in_file)
        },
        |in_file| searched.gather(in_file),
    )?;
    Ok(searched)
}

impl Searched {
    /// Nothing found yet, for a run of `patterns` patterns.
    fn new(patterns: usize) -> Self {
        Searched {
            found: (0..patterns).map(|_| Found::default()).collect(),
            unparsed: Vec::new(),
            supported: 0,
            not_applicable: 0,
            unread: 0,
            changed: Vec::new(),
            held: Vec::new(),
            cited: Vec::new(),
            refused: Vec::new(),
            hinted: Vec::new(),
            hint: None,
            declares_in: None,
            binary: 0,
            skipped: Vec::new(),
            one_at_a_time: None,
        }
    }

    /// Adds what searching one more file found, `file`, after everything found so far, so that
    /// gathering the files in discovery order keeps every list in that order.  A [`Code`] or a
    /// [`Changed`] names a file held by its place among the files `file` holds, which becomes its
    /// place after those held so far.
    fn gather(&mut self, file: Searched) {
        let held = self.held.len();
        let Searched {
            found,
            unparsed,
            supported,
            not_applicable,
            unread,
            changed,
            held: holding,
            cited: _,
            refused,
            hinted,
            hint: _,
            declares_in: _,
            binary,
            skipped,
            one_at_a_time: _,
        } = file;
        for (run, in_file) in self.found.iter_mut().zip(found) {
            // Every field named, so that one added to `Found` cannot be left out of gathering.
            let Found {
                mut matches,
                supported,
                structural,
                not_applicable,
                unread,
                possible,
                unspelled,
                unwritable,
                edits_refused,
                questioned,
                broken,
            } = in_file;
            for (_, hits) in &mut matches {
                for code in hits.iter_mut().filter_map(|hit| hit.code.as_mut()) {
                    code.held += held;
                }
            }
            run.matches.append(&mut matches);
            run.supported += supported;
            run.structural += structural;
            run.not_applicable += not_applicable;
            run.unread += unread;
            run.possible.extend(possible);
            run.unspelled.extend(unspelled);
            run.unwritable.extend(unwritable);
            run.edits_refused.extend(edits_refused);
            run.questioned.extend(questioned);
            run.broken.extend(broken);
        }
        self.unparsed.extend(unparsed);
        self.supported += supported;
        self.not_applicable += not_applicable;
        self.unread += unread;
        self.changed.extend(changed.into_iter().map(|mut changed| {
            changed.held += held;
            changed
        }));
        self.held.extend(holding);
        self.refused.extend(refused);
        self.hinted.extend(hinted);
        self.binary += binary;
        // A file's lines are told once, where a search of one file after another first meets
        // them, whichever thread read it.
        for (project, file) in skipped {
            if !self
                .skipped
                .iter()
                .any(|(p, seen)| *p == project && seen.file == file.file)
            {
                self.skipped.push((project, file));
            }
        }
    }

    /// How many bytes what the run holds until it prints comes to: every match, each a fixed
    /// size, and every file held, as read and as each replace changed it.  The code a match covers
    /// is not among them, since it is read from the file when the match is printed.
    #[cfg(test)]
    pub(super) fn held_for_printing(&self) -> usize {
        let hits: usize = self
            .found
            .iter()
            .flat_map(|found| &found.matches)
            .map(|(_, hits)| hits.len())
            .sum();
        let files: usize = self.held.iter().map(Held::size).sum();
        hits * std::mem::size_of::<Hit>() + files
    }

    /// Searches one file for the run's patterns that look at it, as its `applies` says by their
    /// index, batch by batch, holding its edits.  A file no pattern looks at is not read.
    fn file(
        &mut self,
        to_search: &ToSearch,
        cwd: &Path,
        work: &Work,
        declarations: &Declarations,
        locale: Option<&(String, Encoding)>,
    ) -> Result<(), Failure> {
        let ToSearch {
            path: file,
            project,
            applies,
        } = to_search;
        if !applies.contains(&true) {
            return Ok(());
        }
        let shown = output::relative(file, cwd);
        let bytes = declarations
            .contents
            .read(file)
            .map_err(|cause| Failure::Io {
                doing: "reading",
                path: shown.clone(),
                cause,
            })?;
        // The file's language, by its name, and its patterns, rewrites and `where`s compiled for
        // it.
        let compiled = work.compiled.iter().find(|c| c.language.claims(file));
        let language = compiled.map(|c| c.language);
        // Decided once, from the file as it is on disk, whatever the number of patterns.
        let mut skipped = Vec::new();
        let resolution = resolve::resolve(
            &resolve::File {
                path: file,
                bytes: &bytes,
                language,
            },
            declarations,
            &mut skipped,
        )
        .map_err(|err| Failure::Io {
            doing: "reading the encoding declarations in",
            path: output::relative(&err.path, cwd),
            cause: err.cause,
        })?;
        self.skipped
            .extend(skipped.into_iter().map(|skipped| (*project, skipped)));
        let binary = matches!(resolution, Resolution::Binary);
        self.count_for_structure(work, applies, compiled, binary);
        let read = ReadFile {
            file,
            shown: &shown,
            bytes: &bytes,
            resolution: &resolution,
            compiled,
            binary,
        };
        self.cited.clear();
        let (edited, by, changes) = self.through_batches(&read, work, applies);
        self.tally(&shown, resolution, &bytes, locale);
        // Only the changes are kept, not the bytes they made: those are built again when needed.
        let differs = edited.is_some_and(|edited| edited[..] != bytes[..]);
        if differs {
            self.changed.push(Changed {
                path: file.clone(),
                shown,
                language,
                held: self.held.len(),
                by,
            });
        }
        // The place a match found here was given is this one, taken only if something needs it.
        if differs || (!self.cited.is_empty() && !changes.is_empty()) {
            self.held.push(Held::File {
                original: bytes,
                changes,
                language,
            });
        } else if !self.cited.is_empty() {
            let held = self.copied_or_whole(bytes, language);
            self.held.push(held);
        }
        Ok(())
    }

    /// Takes a file through the run's batches in order, as `applies` says which patterns look at
    /// it: each search on the file as the replaces before it left it.  Returns the file's bytes
    /// once a replace has changed them, the replaces that did, by their pattern's index, and what
    /// each changed.
    fn through_batches(
        &mut self,
        read: &ReadFile,
        work: &Work,
        applies: &[bool],
    ) -> (Option<Vec<u8>>, Vec<usize>, Vec<Changes>) {
        // The file as the replaces so far have left it, once one has changed it: whole while the
        // run is on this file, for the statements after each replace to read.
        let mut edited: Option<Vec<u8>> = None;
        // The replaces that changed it so far, by their pattern's index, and what each changed.
        let mut by: Vec<usize> = Vec::new();
        let mut changes: Vec<Changes> = Vec::new();
        let mut unparsed = false;
        for batch in &work.batches {
            let text: &[u8] = edited.as_deref().unwrap_or(read.bytes);
            // On disk, if an edit has changed it, to tell a file that never parsed from one an
            // edit broke.
            let earlier = Earlier {
                on_disk: edited.is_some().then_some(read.bytes),
                by: &by,
                version: Version {
                    held: self.held.len(),
                    number: changes.len(),
                },
            };
            match batch {
                Batch::Search {
                    textual,
                    structural,
                } => {
                    let textual = applying(textual, applies);
                    let structural = reads(read.compiled, applying(structural, applies));
                    if let Some(compiled) = read.compiled
                        && !structural.is_empty()
                        && !read.binary
                    {
                        self.structural_batch(
                            compiled,
                            read.shown,
                            text,
                            &earlier,
                            &structural,
                            work,
                            read.file,
                            &mut unparsed,
                        );
                    }
                    match read.resolution {
                        Resolution::Known { encoding, body, .. } => {
                            self.known(read.shown, &text[*body..], *encoding, &textual);
                        }
                        Resolution::Unknown(reason) => {
                            self.unknown(read.shown, text, reason, &textual);
                        }
                        Resolution::Binary | Resolution::Refused(_) => {}
                    }
                }
                Batch::Replace {
                    index,
                    pattern,
                    with,
                } => {
                    if !applies[*index] {
                        continue;
                    }
                    if let Some((whole, over)) =
                        self.replace_in(read.shown, text, read.resolution, *index, pattern, with)
                    {
                        changes.push(Changes::between(text, &whole, &over));
                        edited = Some(whole);
                        by.push(*index);
                    }
                }
                Batch::Rewrite {
                    index,
                    compiled: at,
                    rewrite,
                } => {
                    let Some(compiled) = read
                        .compiled
                        .filter(|c| applies[*index] && !read.binary && c.patterns[*at].is_some())
                    else {
                        continue;
                    };
                    let edit = Rewriting {
                        index: *index,
                        pattern: compiled.patterns[*at]
                            .as_ref()
                            .expect("rewritten only where the language reads the pattern"),
                        rewrite: compiled.rewrites[*rewrite]
                            .as_ref()
                            .expect("a rewrite is compiled where its pattern is"),
                    };
                    if let Some((whole, over)) = self.rewrite_in(
                        compiled,
                        read.shown,
                        text,
                        &earlier,
                        &edit,
                        work,
                        read.file,
                        &mut unparsed,
                    ) {
                        changes.push(Changes::between(text, &whole, &over));
                        edited = Some(whole);
                        by.push(*index);
                    }
                }
            }
        }
        (edited, by, changes)
    }

    /// Counts a file for each structural pattern that looks at it, as `applies` says: as one with
    /// structural support where its language reads the pattern, a binary one too, since it is in
    /// scope and the run must not say there is none, though it is counted with the binary files
    /// rather than searched; as one whose language cannot read the pattern; or as a text-tier
    /// file, to which no pattern applies.
    fn count_for_structure(
        &mut self,
        work: &Work,
        applies: &[bool],
        compiled: Option<&Compiled>,
        binary: bool,
    ) {
        let (mut supported, mut not_applicable, mut unread) = (false, false, false);
        for &(i, at) in work.structural.iter().filter(|&&(i, _)| applies[i]) {
            match compiled.map(|c| c.patterns[at].is_some()) {
                Some(true) => {
                    self.found[i].supported += 1;
                    supported = true;
                }
                _ if binary => {}
                Some(false) => {
                    self.found[i].unread += 1;
                    unread = true;
                }
                None => {
                    self.found[i].not_applicable += 1;
                    not_applicable = true;
                }
            }
        }
        self.supported += usize::from(supported);
        self.not_applicable += usize::from(not_applicable);
        self.unread += usize::from(unread);
    }

    /// Searches a file in a known encoding, `text` its body after any byte-order mark, for each
    /// textual pattern, by its index among all the run's patterns.
    pub(super) fn known(
        &mut self,
        shown: &Path,
        text: &[u8],
        encoding: Encoding,
        textual: &[(usize, &TextPattern)],
    ) {
        for &(i, pattern) in textual {
            match search::search(text, encoding, pattern) {
                None => self.found[i]
                    .unspelled
                    .push((shown.to_owned(), Unspelled::Ambiguous(encoding))),
                Some(in_file) if in_file.is_empty() => {}
                Some(in_file) => self.found[i].matches.push((
                    shown.to_owned(),
                    in_file
                        .into_iter()
                        .map(|at| Hit { at, code: None })
                        .collect(),
                )),
            }
        }
    }

    /// Searches a file's decoded `text`, in `language`, for each structural pattern, `shapes` in
    /// the order of `structural`, which pairs each pattern's index among all the run's patterns
    /// with its compiled one.  A file that does not parse is searched for nothing, and where and
    /// why is returned.
    fn parsed(
        &mut self,
        language: &dyn FrontEnd,
        shown: &Path,
        text: &str,
        version: Version,
        structural: &[(usize, usize)],
        shapes: &[(&Pattern, Option<&structural::Where>)],
    ) -> Result<(), String> {
        match structural::search(language, text, shapes) {
            Err(unparsed) => Err(unparsed_at(&unparsed)),
            Ok(found) => {
                for (&(i, _), in_file) in structural.iter().zip(found) {
                    self.cite(i, shown, version, in_file);
                }
                Ok(())
            }
        }
    }

    /// What to hold of a file no replace changed, `bytes`, that structural matches were found in:
    /// the code they cover, copied, if that is less than the file, as for a few short matches in a
    /// large file, with each match's [`Code`] moved to where its copy stands; and otherwise the
    /// file, as for matches that overlap, each to the end of a long block.  So a run holds no more
    /// of a file than the smaller of the two.
    fn copied_or_whole(
        &mut self,
        bytes: crate::contents::Bytes,
        language: Option<&'static dyn FrontEnd>,
    ) -> Held {
        let cited = std::mem::take(&mut self.cited);
        let codes = |matches: &[(PathBuf, Vec<Hit>)]| -> Vec<Code> {
            let (_, hits) = matches
                .last()
                .expect("a pattern cited here has this file's matches");
            hits.iter().filter_map(|hit| hit.code).collect()
        };
        let covered: usize = cited
            .iter()
            .flat_map(|&index| codes(&self.found[index].matches))
            .map(|code| code.end - code.start)
            .sum();
        if covered >= bytes.len() {
            return Held::File {
                original: bytes,
                changes: Vec::new(),
                language,
            };
        }
        let text = language
            .expect("a file a structural match was found in has a language")
            .decode(&bytes)
            .expect("a file a structural match was found in decoded then")
            .text;
        let mut copies = String::with_capacity(covered);
        for index in cited {
            let (_, hits) = self.found[index]
                .matches
                .last_mut()
                .expect("a pattern cited here has this file's matches");
            for code in hits.iter_mut().filter_map(|hit| hit.code.as_mut()) {
                let start = copies.len();
                copies.push_str(&text[code.start..code.end]);
                code.start = start;
                code.end = copies.len();
            }
        }
        Held::Copied(copies)
    }

    /// Records the structural matches `found` of the pattern at `index` in a file, as where each
    /// is in the version of it searched, holding the file if there is any.
    fn cite(
        &mut self,
        index: usize,
        shown: &Path,
        version: Version,
        found: Vec<structural::Found>,
    ) {
        if found.is_empty() {
            return;
        }
        self.cited.push(index);
        self.found[index].matches.push((
            shown.to_owned(),
            found
                .into_iter()
                .map(|f| Hit {
                    at: f.at,
                    code: Some(Code {
                        held: version.held,
                        version: version.number,
                        start: f.range.start,
                        end: f.range.end,
                    }),
                })
                .collect(),
        ));
    }

    /// Searches a file's current bytes, `text`, for a batch's structural patterns, as `compiled`
    /// has them for its language.
    /// `unparsed` says whether an earlier batch found the file unparseable, so that it is counted
    /// as such once however many batches search it.  `on_disk` is the file as it is on
    /// disk when an earlier replace has changed it: if that parses and `text` does not, the edit
    /// broke it, which refuses the run as an edit rather than as a file that cannot be parsed.
    #[expect(
        clippy::too_many_arguments,
        reason = "the file, its batch and what earlier batches found, as a search has them"
    )]
    fn structural_batch(
        &mut self,
        compiled: &Compiled,
        shown: &Path,
        text: &[u8],
        earlier: &Earlier,
        structural: &[(usize, usize)],
        work: &Work,
        file: &Path,
        unparsed: &mut bool,
    ) {
        // The file's path as a glob sees it, worked out only if some `where` may read it.
        let language = compiled.language;
        let place = structural
            .iter()
            .any(|&(i, _)| compiled.filters[i].is_some())
            .then(|| scope::Place::of(file, work.roots.cwd, work.roots.root));
        // Each pattern's `where`, decided for this file's path and for what a match binds.
        let filters: Vec<Option<structural::Where>> = structural
            .iter()
            .map(|&(i, _)| {
                compiled.filters[i].as_ref().map(|filter| {
                    deciding(
                        filter,
                        place
                            .as_ref()
                            .expect("worked out for a pattern with a `where`"),
                    )
                })
            })
            .collect();
        let shapes: Vec<(&Pattern, Option<&structural::Where>)> = structural
            .iter()
            .zip(&filters)
            .map(|(&(_, at), filter)| {
                let pattern = compiled.patterns[at]
                    .as_ref()
                    .expect("searched only where the language reads the pattern");
                (pattern, filter.as_ref())
            })
            .collect();
        // The text parsed is the text the language's own reader would read, decoded as it
        // decodes it, whatever else declares the file's encoding for the textual tier.
        let outcome = match language.decode(text) {
            Ok(source) => self.parsed(
                language,
                shown,
                &source.text,
                earlier.version,
                structural,
                &shapes,
            ),
            // A file its language's reader refuses does not parse, whatever it holds, and one in
            // an encoding quaff does not read cannot be parsed by quaff: either way a count
            // without it could not be trusted.
            Err(refusal) => Err(refusal.to_string()),
        };
        if outcome.is_ok() {
            for &(i, _) in structural {
                self.found[i].structural += 1;
            }
        }
        let Err(why) = outcome else {
            return;
        };
        let indices: Vec<usize> = structural.iter().map(|&(i, _)| i).collect();
        self.not_parsed(language, shown, why, earlier, &indices, unparsed);
    }

    /// Records that a file could not be parsed in `language` for the patterns at `indices`, `why`:
    /// as broken by an earlier replace, if it parses on disk and a replace changed it, and
    /// otherwise as a file that cannot be parsed, once however many batches find it so.
    fn not_parsed(
        &mut self,
        language: &dyn FrontEnd,
        shown: &Path,
        why: String,
        earlier: &Earlier,
        indices: &[usize],
        unparsed: &mut bool,
    ) {
        if earlier
            .on_disk
            .is_some_and(|bytes| parses(language, bytes).is_ok())
        {
            for &i in indices {
                self.found[i]
                    .broken
                    .push((shown.to_owned(), why.clone(), earlier.by.to_vec()));
            }
        } else if !*unparsed {
            self.unparsed.push((shown.to_owned(), why));
            *unparsed = true;
        }
    }

    /// Replaces a structural pattern in a file's current bytes, `text`, as `edit` says, and
    /// returns the file's new bytes if that changed it, with each stretch of `text` it wrote over
    /// and where it went in them, as [`Changes::between`] takes.  The text is decoded as the
    /// file's language decodes it and written back in the same encoding, so that every byte
    /// outside the matches is as it was.  A file that does not parse is recorded as a search
    /// records it; an edit that cannot be made as written, or written in the file's encoding, is
    /// recorded for the run to refuse.
    #[expect(
        clippy::too_many_arguments,
        reason = "the file, the edit and what earlier batches found, as a search has them"
    )]
    fn rewrite_in(
        &mut self,
        compiled: &Compiled,
        shown: &Path,
        text: &[u8],
        earlier: &Earlier,
        edit: &Rewriting,
        work: &Work,
        file: &Path,
        unparsed: &mut bool,
    ) -> Option<(Vec<u8>, Over)> {
        let index = edit.index;
        let language = compiled.language;
        // The file's path as a glob sees it, worked out only if a `where` may read it.
        let placed = compiled.filters[index].as_ref().map(|filter| {
            let place = scope::Place::of(file, work.roots.cwd, work.roots.root);
            (filter, place)
        });
        let filter = placed
            .as_ref()
            .map(|(filter, place)| deciding(filter, place));
        let source = match language.decode(text) {
            Ok(source) => source,
            Err(refusal) => {
                self.not_parsed(
                    language,
                    shown,
                    refusal.to_string(),
                    earlier,
                    &[index],
                    unparsed,
                );
                return None;
            }
        };
        let rewrite = match edit.rewrite {
            Ok(rewrite) => rewrite,
            Err(err) => {
                self.unwritten(
                    language,
                    shown,
                    &source.text,
                    earlier,
                    edit,
                    filter.as_ref(),
                    err,
                    unparsed,
                );
                return None;
            }
        };
        let replaced = match rewrite::replace(
            language,
            &source.text,
            edit.pattern,
            filter.as_ref(),
            rewrite,
        ) {
            Ok(replaced) => replaced,
            Err(err) => {
                self.not_parsed(
                    language,
                    shown,
                    unparsed_at(&err),
                    earlier,
                    &[index],
                    unparsed,
                );
                return None;
            }
        };
        self.found[index].structural += 1;
        self.cite(index, shown, earlier.version, replaced.found);
        match replaced.outcome {
            Ok(None) => None,
            Ok(Some(edited)) => match source.encode(&edited.text) {
                Ok(bytes) => {
                    for questioned in edited.questioned {
                        self.found[index].questioned.push((
                            shown.to_owned(),
                            format!(
                                "line {}, column {}: {}",
                                questioned.at.line, questioned.at.column, questioned.why
                            ),
                        ));
                    }
                    let over = in_bytes(&source, &edited.text, &edited.over);
                    Some((bytes, over))
                }
                Err(unencodable) => {
                    self.found[index].unwritable.push((
                        shown.to_owned(),
                        unencodable.encoding,
                        unencodable.character.to_string(),
                        unencodable.ambiguous,
                    ));
                    None
                }
            },
            Err(refused) => {
                self.found[index].edits_refused.push((
                    shown.to_owned(),
                    format!(
                        "line {}, column {}: {}",
                        refused.at.line, refused.at.column, refused.why
                    ),
                ));
                None
            }
        }
    }

    /// Searches a file whose language reads a replace's pattern but not its rewrite: each match
    /// is counted and cited, and the first refuses the edit, since it cannot be written there.
    #[expect(
        clippy::too_many_arguments,
        reason = "the file, the edit and why its rewrite cannot be compiled"
    )]
    fn unwritten(
        &mut self,
        language: &dyn FrontEnd,
        shown: &Path,
        text: &str,
        earlier: &Earlier,
        edit: &Rewriting,
        filter: Option<&structural::Where>,
        err: &CompileError,
        unparsed: &mut bool,
    ) {
        let index = edit.index;
        let found = match structural::search(language, text, &[(edit.pattern, filter)]) {
            Ok(mut found) => found.swap_remove(0),
            Err(why) => {
                self.not_parsed(
                    language,
                    shown,
                    unparsed_at(&why),
                    earlier,
                    &[index],
                    unparsed,
                );
                return;
            }
        };
        self.found[index].structural += 1;
        if let Some(first) = found.first() {
            self.found[index].edits_refused.push((
                shown.to_owned(),
                format!(
                    "line {}, column {}: the rewrite cannot be written in {}: {err}",
                    first.at.start.line,
                    first.at.start.column,
                    language.language()
                ),
            ));
        }
        self.cite(index, shown, earlier.version, found);
    }

    /// Counts a file once by what it is: binary, refused by its language's reader, or of unknown
    /// encoding that reads cleanly in the locale's, for the hint.
    fn tally(
        &mut self,
        shown: &Path,
        resolution: Resolution,
        bytes: &[u8],
        locale: Option<&(String, Encoding)>,
    ) {
        match resolution {
            Resolution::Binary => self.binary += 1,
            Resolution::Refused(refusal) => self.refused.push((shown.to_owned(), refusal)),
            Resolution::Unknown(reason) => {
                if reason == Unknown::NotUtf8
                    && locale.is_some_and(|(_, encoding)| encoding.reads_cleanly(bytes))
                {
                    self.hinted.push(shown.to_owned());
                }
            }
            Resolution::Known { .. } => {}
        }
    }

    /// Replaces the pattern at `index` with `with` in a file's current bytes, `text`, decided as
    /// `resolution` says, and returns its new bytes if that changed it, with where each
    /// replacement went, as [`Changes::between`] takes.  A file of unknown encoding is never
    /// edited: its matches are only possible, and reported as in a search.
    fn replace_in(
        &mut self,
        shown: &Path,
        text: &[u8],
        resolution: &Resolution,
        index: usize,
        pattern: &TextPattern,
        with: &TextPattern,
    ) -> Option<(Vec<u8>, Over)> {
        match resolution {
            Resolution::Known { encoding, body, .. } => {
                let replaced = search::replace(&text[*body..], *encoding, pattern, with);
                let written = self.replaced(shown, index, *encoding, replaced)?;
                let mut whole = text[..*body].to_vec();
                whole.extend_from_slice(&written.bytes);
                // The replace saw the text after any byte-order mark, which is before it in both.
                let over = written
                    .over
                    .into_iter()
                    .map(|(was, now)| {
                        (
                            was.start + body..was.end + body,
                            now.start + body..now.end + body,
                        )
                    })
                    .collect();
                Some((whole, over))
            }
            Resolution::Unknown(reason) => {
                self.unknown(shown, text, reason, &[(index, pattern)]);
                None
            }
            Resolution::Binary | Resolution::Refused(_) => None,
        }
    }

    /// Records what replacing the pattern at `index` in a file in `encoding` found, and returns
    /// the file's new text if the replace changed it.
    pub(super) fn replaced(
        &mut self,
        shown: &Path,
        index: usize,
        encoding: Encoding,
        replaced: search::Replaced,
    ) -> Option<search::Written> {
        let hits = |matches: Vec<Match>| -> Vec<Hit> {
            matches
                .into_iter()
                .map(|at| Hit { at, code: None })
                .collect()
        };
        match replaced {
            search::Replaced::NotSearched => {
                self.found[index]
                    .unspelled
                    .push((shown.to_owned(), Unspelled::Ambiguous(encoding)));
                None
            }
            search::Replaced::Edited { matches, text } => {
                if !matches.is_empty() {
                    self.found[index]
                        .matches
                        .push((shown.to_owned(), hits(matches)));
                }
                text
            }
            search::Replaced::Unwritable {
                matches,
                text,
                ambiguous,
            } => {
                self.found[index]
                    .matches
                    .push((shown.to_owned(), hits(matches)));
                self.found[index].unwritable.push((
                    shown.to_owned(),
                    name_of(encoding),
                    text,
                    ambiguous,
                ));
                None
            }
        }
    }

    /// Searches a file of unknown encoding for each textual pattern.  An ASCII query is the
    /// same bytes in every ASCII-compatible encoding, so where its bytes are absent there is
    /// certainly no match; where present, a match is only possible, since in some encodings those
    /// bytes end a character.  In a declared UTF-16 or UTF-32 it is other bytes, so no query can
    /// be searched.
    pub(super) fn unknown(
        &mut self,
        shown: &Path,
        bytes: &[u8],
        reason: &Unknown,
        textual: &[(usize, &TextPattern)],
    ) {
        for &(i, pattern) in textual {
            let in_file = if reason.ascii_compatible() {
                search::search(bytes, Encoding::Unknown8Bit, pattern)
            } else {
                None
            };
            match in_file {
                None => {
                    self.found[i]
                        .unspelled
                        .push((shown.to_owned(), Unspelled::Unknown(reason.clone())));
                }
                Some(in_file) if in_file.is_empty() => {}
                Some(in_file) => {
                    let mut lines: Vec<usize> = in_file.iter().map(|m| m.start.line).collect();
                    lines.dedup();
                    self.found[i]
                        .possible
                        .push((shown.to_owned(), lines, reason.clone()));
                }
            }
        }
    }
}

/// `filter`, a statement's `where`, decided for a file at `place` and for what a match binds.
pub(super) fn deciding<'f>(
    filter: &'f filter::Filter,
    place: &'f scope::Place,
) -> structural::Where<'f> {
    let found = filter::Found::default();
    structural::Where {
        names: filter.names.iter().map(String::as_str).collect(),
        holds: Box::new(move |arena, nodes| filter.holds(place, arena, nodes, &found)),
    }
}

/// The structural patterns of `batch` that `compiled`'s language reads: the rest do not apply to
/// a file of it.  All of them where it reads them all, as one language reads every pattern.
pub(super) fn reads<'b>(
    compiled: Option<&Compiled>,
    batch: Cow<'b, [(usize, usize)]>,
) -> Cow<'b, [(usize, usize)]> {
    match compiled {
        Some(compiled) if !batch.iter().all(|&(_, at)| compiled.patterns[at].is_some()) => {
            Cow::Owned(
                batch
                    .iter()
                    .copied()
                    .filter(|&(_, at)| compiled.patterns[at].is_some())
                    .collect(),
            )
        }
        _ => batch,
    }
}

/// The patterns in `batch`, each by its index among all the run's patterns, that look at a file,
/// as `applies` says by that index: the batch itself, uncopied, when they all do.
fn applying<'b, T: Copy>(batch: &'b [(usize, T)], applies: &[bool]) -> Cow<'b, [(usize, T)]> {
    if batch.iter().all(|&(i, _)| applies[i]) {
        Cow::Borrowed(batch)
    } else {
        Cow::Owned(batch.iter().copied().filter(|&(i, _)| applies[i]).collect())
    }
}

/// Where and why a file does not parse in its language, as a message gives it.
fn unparsed_at(unparsed: &structural::Unparsed) -> String {
    format!(
        "line {}, column {}: {}",
        unparsed.at.line, unparsed.at.column, unparsed.message
    )
}

/// A way for a test to see how many files a run searches at once, which timing alone cannot
/// show: a file under a directory a test watches waits, as its search starts, until another file
/// there is being searched too, or the test's patience runs out.  One file at a time, no file
/// ever has company.
#[cfg(test)]
pub(super) mod meeting {
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    /// A directory a test watches.
    struct Watched {
        dir: PathBuf,
        patience: Duration,
        /// How many of its files are waiting for company now.
        waiting: usize,
        /// The most that ever were at once.
        most: usize,
        /// Set once one file waited out the patience, so that the rest do not each wait too.
        gave_up: bool,
    }

    /// Every directory being watched, by any test running.
    static WATCHED: Mutex<Vec<Watched>> = Mutex::new(Vec::new());
    /// Told whenever a file starts waiting.
    static ARRIVED: Condvar = Condvar::new();

    fn watched() -> MutexGuard<'static, Vec<Watched>> {
        WATCHED.lock().expect("no test panics holding it")
    }

    /// Called as each file's search starts: a file under a watched directory waits for another
    /// there to start, unless two have already been searched at once.
    pub(in crate::run) fn meet(file: &Path) {
        let mut all = watched();
        let find = |all: &[Watched]| all.iter().position(|w| file.starts_with(&w.dir));
        let Some(at) = find(&all) else {
            return;
        };
        let deadline = Instant::now() + all[at].patience;
        all[at].waiting += 1;
        all[at].most = all[at].most.max(all[at].waiting);
        ARRIVED.notify_all();
        loop {
            // Found again each time, since other tests start and stop watching meanwhile.
            let at = find(&all).expect("watched until the run ends");
            let now = Instant::now();
            if all[at].most >= 2 || all[at].gave_up || now >= deadline {
                all[at].gave_up |= all[at].most < 2;
                all[at].waiting -= 1;
                return;
            }
            all = ARRIVED
                .wait_timeout(all, deadline - now)
                .expect("no test panics holding it")
                .0;
        }
    }

    /// Calls `run` with the files under `dir` watched, each waiting up to `patience` for
    /// company, and returns the most files there were ever searched at once.
    pub(in crate::run) fn most_at_once(
        dir: &Path,
        patience: Duration,
        run: impl FnOnce(),
    ) -> usize {
        watched().push(Watched {
            dir: dir.to_path_buf(),
            patience,
            waiting: 0,
            most: 0,
            gave_up: false,
        });
        run();
        let mut all = watched();
        let at = all
            .iter()
            .position(|w| w.dir == dir)
            .expect("watched until now");
        all.remove(at).most
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use quaffed_representation::program::{Operand, Query};

    use crate::contents::Contents;

    /// A stretch of `text`'s bytes, in `encoding`'s spelling, rewritten: `old` in the file as
    /// decoded becomes `new`, and the claim is checked to be exactly what changed.
    fn claimed_in_bytes(file: &[u8], old: &str, new: &str) {
        let source = crate::languages::PYTHON.decode(file).expect("decodes");
        let at = source.text.find(old).expect("the old text is in the file");
        let edited = source.text.replacen(old, new, 1);
        let over = [(at..at + old.len(), at..at + new.len())];
        let bytes = source.encode(&edited).expect("encodes");
        let claimed = in_bytes(&source, &edited, &over);
        assert_eq!(claimed.len(), 1, "{claimed:?}");
        let (was, now) = &claimed[0];
        let mut rebuilt = file[..was.start].to_vec();
        rebuilt.extend_from_slice(&bytes[now.clone()]);
        rebuilt.extend_from_slice(&file[was.end..]);
        assert_eq!(rebuilt, bytes, "{claimed:?}");
        let changes = Changes::between(file, &bytes, &claimed);
        assert_eq!(changes.held(), now.len());
    }

    #[test]
    fn a_rewrites_stretches_are_found_in_the_files_bytes_whatever_its_encoding() {
        // After a byte-order mark, which comes before every stretch.
        claimed_in_bytes(
            b"\xef\xbb\xbfx = 'caf\xc3\xa9'\ny = 2\n",
            "y = 2",
            "y = (2)",
        );
        // Single-byte: each character one byte, so a decoded offset past "é" is not a byte one.
        claimed_in_bytes(
            b"# -*- coding: latin-1 -*-\nx = 'caf\xe9'\ny = 2\n",
            "y = 2",
            "y = 'na\u{ef}ve'",
        );
        // Multi-byte: "日本" is four bytes in Shift-JIS and six in UTF-8.
        claimed_in_bytes(
            b"# -*- coding: shift_jis -*-\nx = '\x93\xfa\x96\x7b'\ny = 2\n",
            "y = 2",
            "y = '\u{65e5}'",
        );
    }

    #[test]
    fn a_stretch_off_a_character_boundary_claims_nothing() {
        let source = crate::languages::PYTHON
            .decode("x = 'é'\n".as_bytes())
            .unwrap();
        // Byte 6 is inside "é".
        assert_eq!(in_bytes(&source, "x = 'é'\n", &[(6..7, 6..7)]), Vec::new());
    }

    #[test]
    fn a_file_without_matches_is_not_listed_for_a_pattern() {
        let dir = std::env::temp_dir().join(format!("quaffed-run-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creating the directory");
        let with = dir.join("with.txt");
        let without = dir.join("without.txt");
        fs::write(&with, "TODO\n").expect("writing");
        fs::write(&without, "nothing\n").expect("writing");
        let pattern = TextPattern::literal("TODO");
        let span = quaffed_representation::span::Span {
            source: 0,
            start: 0,
            end: 4,
        };
        let query = Query::new(quaffed_representation::span::Spanned::new(
            Operand::Text(pattern.clone()),
            span,
        ));
        let projects = Projects::new(&dir, &Contents::default());
        let file = |path| ToSearch {
            path,
            project: 0,
            applies: vec![true],
        };
        let searched = search_files(
            &[file(with), file(without)],
            scope::Roots {
                cwd: &dir,
                root: &dir,
            },
            &[Searching {
                pattern: Wanted::Text(&pattern),
                query: &query,
                replacement: None,
            }],
            &[],
            &projects,
            None,
            NonZeroUsize::MIN,
        );
        let _ = fs::remove_dir_all(&dir);
        let searched = searched.expect("searching");
        let listed: Vec<&PathBuf> = searched.found[0]
            .matches
            .iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(listed, [&PathBuf::from("with.txt")]);
    }

    /// What searching a directory holding `files` for `script` found, through every phase
    /// before this one.
    fn searched(name: &str, script: &str, files: &[(&str, &str)]) -> Searched {
        use crate::cli::SourceArg;
        use crate::languages;
        use crate::run::{plan, scoping, sources};
        let dir =
            std::env::temp_dir().join(format!("quaffed-execute-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).expect("creating the directory");
        for (path, text) in files {
            fs::write(dir.join(path), text).expect("writing");
        }
        let contents = Contents::default();
        let result = (|| {
            let built = sources::read(
                vec![SourceArg::Expression {
                    number: 1,
                    text: script.into(),
                }],
                None,
                &mut std::io::empty(),
                &contents,
            )?;
            let mut plan = plan::plan(languages::LANGUAGES, &built)?;
            let files = scoping::scope(&mut plan, &built, &dir, &contents)?;
            execute(&plan, &files, None)
        })();
        let _ = fs::remove_dir_all(&dir);
        let Ok(searched) = result else {
            panic!("{:?}", result.err());
        };
        searched
    }

    #[test]
    fn a_run_searches_as_many_files_at_once_as_the_machine_runs_unless_limited() {
        let three = NonZeroUsize::new(3);
        assert_eq!(jobs(three).unwrap(), three.unwrap());
        assert_eq!(
            jobs(None).unwrap(),
            thread::available_parallelism().unwrap()
        );
    }

    #[test]
    fn each_match_gathered_from_its_file_names_the_code_in_that_file() {
        let searched = searched(
            "codes",
            "find `f($a)`\n",
            &[("a.py", "f(1)\n"), ("b.py", "x = 0\nf(2)\n")],
        );
        let codes: Vec<(PathBuf, String)> = searched.found[0]
            .matches
            .iter()
            .flat_map(|(path, hits)| hits.iter().map(move |hit| (path, hit)))
            .map(|(path, hit)| {
                let code = hit.code.expect("a structural match says where its code is");
                let text = searched.held[code.held].text(code.version);
                (path.clone(), text[code.start..code.end].to_owned())
            })
            .collect();
        assert_eq!(
            codes,
            [
                (PathBuf::from("a.py"), "f(1)".to_owned()),
                (PathBuf::from("b.py"), "f(2)".to_owned()),
            ]
        );
    }

    #[test]
    fn each_changed_file_gathered_names_its_own_bytes_as_read() {
        let searched = searched(
            "changed",
            "replace string \"a\" with \"b\" expect 2\n",
            &[("one.txt", "a\n"), ("two.txt", "xa\n")],
        );
        let changed: Vec<(PathBuf, Vec<u8>)> = searched
            .changed
            .iter()
            .map(|file| {
                (
                    file.shown.clone(),
                    searched.held[file.held].file().0.to_vec(),
                )
            })
            .collect();
        assert_eq!(
            changed,
            [
                (PathBuf::from("one.txt"), b"a\n".to_vec()),
                (PathBuf::from("two.txt"), b"xa\n".to_vec()),
            ]
        );
    }
}
