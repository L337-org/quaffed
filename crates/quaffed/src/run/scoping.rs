// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Scoping: which files each pattern looks at, the projects they are in, and discovery from the
//! places those scopes start.
//!
//! The third phase.  In: the [`Plan`], the [`Built`] program and the current directory.  Out:
//! the [`Files`] to search, each with which of the run's patterns look at it, which execution
//! reads.  It compiles each `where` into the plan first, since a `where` reads paths from where
//! the run is, and checks every one before discovery looks at a file.

use std::fs;
use std::path::{Path, PathBuf};

use quaffed_representation::glob::Glob;
use quaffed_representation::program::{Filter, Operand, Query, Subject};
use quaffed_representation::span::{Span, Spanned};

use super::compile::{filters, rewrites_read};
use super::plan::Plan;
use super::{failure::Failure, sources::Built};
use crate::contents::Contents;
use crate::discover;
use crate::editorconfig::EditorConfig;
use crate::output;
use crate::resolve::Declarations;
use crate::scope;
use crate::workspace::{self, Workspace};

/// The files a run looks at, and where it looks from.
pub(super) struct Files {
    /// The current directory, canonical.
    pub(super) cwd: PathBuf,
    /// The root of the current directory's project, which `:/` names.
    pub(super) root: PathBuf,
    /// The projects the files are in, each with what decides its files' encodings.
    pub(super) projects: Projects,
    /// Every file found, in path order, with which of the run's patterns look at it.
    pub(super) to_search: Vec<ToSearch>,
    /// What discovery passed over.
    pub(super) found: discover::Found,
    /// Notes on scopes that name nothing, or that the command line narrowed.
    pub(super) notes: Vec<String>,
}

/// Scopes `plan`'s patterns from `cwd`, `built` being the program it was planned from: compiles
/// each `where` into the plan, works out which files each pattern looks at, and discovers them.
/// `contents` is the run's store of what it has read.
///
/// # Errors
///
/// Returns a [`Failure`] for a current directory or scope that cannot be read, a scope holding a
/// double quote, a `where` or `in` that cannot be compiled, an `in` that names nothing there, or
/// files that cannot all be found.
pub(super) fn scope<'p>(
    plan: &mut Plan<'p>,
    built: &'p Built,
    cwd: &Path,
    contents: &Contents,
) -> Result<Files, Failure> {
    let cwd = fs::canonicalize(cwd).map_err(|cause| Failure::Io {
        doing: "reading the current directory",
        path: cwd.to_path_buf(),
        cause,
    })?;
    let mut projects = Projects::new(&cwd, contents);
    let root = projects.list[0].boundary.clone();
    let roots = scope::Roots {
        cwd: &cwd,
        root: &root,
    };
    let narrow = narrowing(built.program.narrow_to.as_deref(), roots)?;
    // Every `where` is compiled and checked, as every pattern is, before discovery looks at a
    // file, so that a script error is reported as one however large or unreadable the tree.
    filters(&plan.searching, &mut plan.compiled, roots, built)?;
    rewrites_read(&plan.compiled, &plan.rewrites, built)?;
    let mut scoping = Scoping::new(narrow.as_deref(), &cwd);
    let scoped: Vec<Scoped> = plan
        .searching
        .iter()
        .map(|s| scoping.scoped(s.query, roots, built))
        .collect::<Result<_, _>>()?;
    let (files, found) = discover_bases(&scoping.bases, &mut projects, &cwd)?;
    let to_search = to_search(files, &scoped, roots);
    let notes = scoping.notes;
    Ok(Files {
        cwd,
        root,
        projects,
        to_search,
        found,
        notes,
    })
}

/// The command-line scope, if one was given: a plain path relative to the current directory,
/// or from the project root when it begins `:/`, as `in` and `FILE` are.  It may lie outside the
/// project: naming a path is asking for it.
fn narrowing(scope: Option<&Path>, roots: scope::Roots) -> Result<Option<PathBuf>, Failure> {
    let Some(given) = scope else {
        return Ok(None);
    };
    let bytes = given.as_os_str().as_encoded_bytes();
    if bytes.contains(&b'"') {
        return Err(Failure::QuotedScope {
            scope: given.as_os_str().to_owned(),
        });
    }
    // `:` alone is a path's first segment, so the root is only after `:/`.
    let joined = match given.strip_prefix(":") {
        Ok(rest) if bytes.starts_with(b":/") => roots.root.join(rest),
        _ => roots.cwd.join(given),
    };
    fs::canonicalize(joined)
        .map(Some)
        .map_err(|cause| Failure::Io {
            doing: "reading the scope",
            path: given.to_path_buf(),
            cause,
        })
}

/// The projects a run's files are in, each with the declarations that decide its files'
/// encodings.  The first is the current directory's, which `:/` names.
pub(super) struct Projects {
    pub(super) list: Vec<Project>,
    pub(super) contents: Contents,
    /// The project each directory a file was found in belongs to, where finding it meant walking
    /// up from the directory.
    by_dir: std::collections::HashMap<PathBuf, usize>,
}

/// A project, or the directory that stands in for one where there is none.
pub(super) struct Project {
    /// Its root, above which nothing is read.
    boundary: PathBuf,
    pub(super) declarations: Declarations,
}

impl Projects {
    /// The projects of a run from `cwd`: to begin with, the one it is in, or `cwd` standing in.
    pub(super) fn new(cwd: &Path, contents: &Contents) -> Self {
        let mut projects = Projects {
            list: Vec::new(),
            contents: contents.clone(),
            by_dir: std::collections::HashMap::new(),
        };
        projects.add(workspace::find(cwd, contents), cwd, cwd);
        projects
    }

    fn add(&mut self, workspace: Option<Box<dyn Workspace>>, standing_in: &Path, cwd: &Path) {
        let boundary = workspace
            .as_ref()
            .map_or_else(|| standing_in.to_path_buf(), |w| w.root().to_path_buf());
        // EditorConfig is read no higher than the boundary, so that nothing outside a project
        // changes an answer about its files.
        let declarations = Declarations {
            workspace,
            editorconfig: EditorConfig::new(&boundary, &self.contents),
            cwd: cwd.to_path_buf(),
            contents: self.contents.clone(),
        };
        self.list.push(Project {
            boundary,
            declarations,
        });
    }

    /// The project `base`, a path discovery starts from, is in: the current directory's when it
    /// lies under its root; otherwise the one found by walking up from it, or with none the
    /// directory it names, which stands in for one.
    pub(super) fn of(&mut self, base: &Path, cwd: &Path) -> usize {
        if base.starts_with(&self.list[0].boundary) {
            return 0;
        }
        let dir = if base.is_dir() {
            base
        } else {
            base.parent().unwrap_or(base)
        };
        let workspace = workspace::find(dir, &self.contents);
        let boundary = workspace.as_ref().map_or(dir, |w| w.root());
        if let Some(found) = self.list.iter().position(|p| p.boundary == boundary) {
            return found;
        }
        self.add(workspace, dir, cwd);
        self.list.len() - 1
    }
}

impl Projects {
    /// The deepest of the projects so far whose boundary holds `path`.  The current directory's
    /// holds every file a run finds under it, and each base's project holds that base's.
    fn deepest(&self, path: &Path) -> usize {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, project)| path.starts_with(&project.boundary))
            .max_by_key(|(_, project)| project.boundary.components().count())
            .map_or(0, |(index, _)| index)
    }

    /// The project a file found from `base`'s project is in: that one, unless it is no project
    /// and the file is in a repository below it, which then decides the file's encoding as a
    /// nested repository does inside a project.
    fn of_file(&mut self, file: &Path, base: usize, cwd: &Path) -> usize {
        if self.list[base].declarations.workspace.is_some() {
            return base;
        }
        let Some(dir) = file.parent() else {
            return base;
        };
        if let Some(&known) = self.by_dir.get(dir) {
            return known;
        }
        let owner = match workspace::find(dir, &self.contents) {
            Some(found) => {
                let root = found.root().to_path_buf();
                if let Some(known) = self.list.iter().position(|p| p.boundary == root) {
                    known
                } else {
                    self.add(Some(found), &root, cwd);
                    self.list.len() - 1
                }
            }
            None => base,
        };
        self.by_dir.insert(dir.to_path_buf(), owner);
        owner
    }
}

/// Which files each of a run's patterns looks at, worked out from their queries, with the
/// places discovery starts from and what it notes along the way.
struct Scoping<'a> {
    /// The command-line scope, which every pattern's files lie under.
    narrow: Option<&'a Path>,
    pub(super) cwd: &'a Path,
    /// The places discovery starts from: each distinct base a pattern needs.
    bases: Vec<PathBuf>,
    /// Notes on scopes that name nothing, or that the command line narrowed.
    pub(super) notes: Vec<String>,
}

/// Which files one of the run's patterns looks at.
struct Scoped {
    /// The base its files are discovered from, or none when its `in` and the command-line scope
    /// have nothing in common.
    base: Option<usize>,
    /// Its `in`, which its files must match.
    pub(super) within: Option<scope::PathGlob>,
    /// Its `where`, as far as a file's path alone can decide it.
    pub(super) test: Option<scope::FileTest>,
}

impl Scoped {
    /// Whether the pattern looks at the file at `place`, which the bases marked in `found_by`
    /// found.  A file its `where` rules out whatever a match binds is not looked at, so it is
    /// never parsed for this pattern and cannot refuse the run.
    pub(super) fn applies(&self, found_by: &[bool], place: &scope::Place) -> bool {
        self.base.is_some_and(|base| found_by[base])
            && self
                .within
                .as_ref()
                .is_none_or(|within| within.is_match(place))
            && self
                .test
                .as_ref()
                .is_none_or(|test| test.holds(place) != Some(false))
    }
}

impl<'a> Scoping<'a> {
    pub(super) fn new(narrow: Option<&'a Path>, cwd: &'a Path) -> Self {
        Scoping {
            narrow,
            cwd,
            bases: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The scope of `query`'s pattern.
    pub(super) fn scoped(
        &mut self,
        query: &Query,
        roots: scope::Roots,
        built: &Built,
    ) -> Result<Scoped, Failure> {
        let compile_error = |err: scope::Error, span: Span| match err {
            scope::Error::TooLarge(err) => Failure::Script {
                location: built.locate(span, span.start),
                message: err.to_string(),
            },
            scope::Error::Io { path, cause } => Failure::Io {
                doing: "looking for",
                path,
                cause,
            },
        };
        let within = match query.scope.first() {
            Some(glob) => Some((
                scope::PathGlob::new(&glob.node, false, roots)
                    .map_err(|err| compile_error(err, glob.span))?,
                glob,
            )),
            None => None,
        };
        let test = match &query.filter {
            Some(filter) => Some(
                scope::FileTest::new(filter, roots)
                    .map_err(|(err, span)| compile_error(err, span))?,
            ),
            None => None,
        };
        if let Some(filter) = &query.filter {
            self.note_missing_files(filter, roots, built);
        }
        // An `in` names its own base, and fails the run if that is not there, whether or not
        // the command line narrows it.
        let named = match &within {
            Some((compiled, glob)) => {
                let base = match compiled.names() {
                    scope::Names::There(path) => Some(path.to_path_buf()),
                    scope::Names::NotThere => None,
                    scope::Names::Glob => scope::base(&glob.node, roots)
                        .map_err(|err| compile_error(err, glob.span))?,
                };
                Some(base.ok_or_else(|| not_there(glob, built))?)
            }
            None => None,
        };
        let base = match (self.narrow, named) {
            // The deeper of the two is where both look; with neither inside the other, the
            // statement looks at nothing.  So the same `in` finds the same files whether or not
            // a scope that contains it is given, which crosses no boundary the `in` would not.
            (Some(narrow), Some(named)) => {
                let glob = &within.as_ref().expect("an `in` named it").1;
                // Said only where it changes what the statement looks at.
                let said = if named.starts_with(narrow) {
                    None
                } else if narrow.starts_with(&named) {
                    Some("narrows this statement's `in`")
                } else {
                    Some("shares nothing with this statement's `in`, so it looks at nothing")
                };
                if let Some(said) = said {
                    self.notes.push(format!(
                        "{}: the scope {:?} given on the command line {said}",
                        built.locate(glob.span, glob.span.start),
                        output::relative(narrow, self.cwd)
                    ));
                }
                if named.starts_with(narrow) {
                    Some(named)
                } else if narrow.starts_with(&named) {
                    Some(narrow.to_path_buf())
                } else {
                    None
                }
            }
            (Some(narrow), None) => Some(narrow.to_path_buf()),
            (None, Some(named)) => Some(named),
            (None, None) => Some(self.cwd.to_path_buf()),
        };
        let base = base.map(|base| self.base(base));
        Ok(Scoped {
            base,
            within: within.map(|(compiled, _)| compiled),
            test,
        })
    }

    /// The index of `base` among the places discovery starts from.
    fn base(&mut self, base: PathBuf) -> usize {
        if let Some(found) = self.bases.iter().position(|b| *b == base) {
            return found;
        }
        self.bases.push(base);
        self.bases.len() - 1
    }

    /// Notes each `FILE matches` of a path with no wildcard that names nothing that is there,
    /// with how to write it as a glob if that is what it reads as.  A note, not a failure: such a
    /// test may guard against a file that does not exist yet.
    fn note_missing_files(&mut self, filter: &Spanned<Filter>, roots: scope::Roots, built: &Built) {
        match &filter.node {
            Filter::And(parts) => {
                for part in parts {
                    self.note_missing_files(part, roots, built);
                }
            }
            Filter::Not(part) => self.note_missing_files(part, roots, built),
            Filter::Matches(Subject::File, operand) => {
                let Operand::Glob(glob) = &operand.node else {
                    return;
                };
                let Some(path) = glob.as_literal() else {
                    return;
                };
                // A glob that cannot be compiled is refused where its test is built instead.
                let there = scope::PathGlob::new(glob, false, roots)
                    .is_ok_and(|compiled| matches!(compiled.names(), scope::Names::There(_)));
                if !there {
                    let mut text = format!(
                        "{}: FILE matches {path:?}, which names nothing that is there",
                        built.locate(operand.span, operand.span.start),
                    );
                    if let Some(markers) = scope::as_markers(path) {
                        text.push_str("; text in quotes is literal, so to match a pattern write ");
                        text.push_str(&markers);
                    }
                    self.notes.push(text);
                }
            }
            _ => {}
        }
    }
}

/// The failure for an `in` that names nothing there: its path, or the directory its glob's
/// leading text names, with how to write it as a glob if that is what it reads as.
fn not_there(glob: &Spanned<Glob>, built: &Built) -> Failure {
    let mut message = match glob.node.as_literal() {
        Some(path) => format!("`in` names {path:?}, which is not there"),
        None => format!(
            "`in` looks for files under {:?}, which is not there",
            scope::leading_directory(&glob.node)
        ),
    };
    if let Some(markers) = glob.node.as_literal().and_then(scope::as_markers) {
        message.push_str("; text in quotes is literal, so to match a pattern write ");
        message.push_str(&markers);
    }
    Failure::NotThere {
        location: built.locate(glob.span, glob.span.start),
        message,
    }
}

/// Each of `files`, with which of the run's patterns, scoped as `scoped`, look at it.
fn to_search(files: Discovered, scoped: &[Scoped], roots: scope::Roots) -> Vec<ToSearch> {
    // A file's path as a glob sees it is worked out only where some `in` or `where` reads it: a
    // run with neither pays nothing for scoping beyond the base each file is in.
    let reads_paths = scoped
        .iter()
        .any(|scoped| scoped.within.is_some() || scoped.test.is_some());
    files
        .into_iter()
        .map(|(path, project, bases)| {
            let applies = if reads_paths {
                let place = scope::Place::of(&path, roots.cwd, roots.root);
                scoped
                    .iter()
                    .map(|scoped| scoped.applies(&bases, &place))
                    .collect()
            } else {
                scoped
                    .iter()
                    .map(|scoped| scoped.base.is_some_and(|base| bases[base]))
                    .collect()
            };
            ToSearch {
                path,
                project,
                applies,
            }
        })
        .collect()
}

/// A file to search, the project it is in, and which of the run's patterns look at it.
pub(super) struct ToSearch {
    pub(super) path: PathBuf,
    pub(super) project: usize,
    pub(super) applies: Vec<bool>,
}

/// Every file found under a run's bases, in path order, with the project it is in and which
/// bases found it.
type Discovered = Vec<(PathBuf, usize, Vec<bool>)>;

/// Every file under each of `bases`, each discovered by the rules of the project it is in, with
/// the project of each file and which bases found it, and what discovery passed over.
fn discover_bases(
    bases: &[PathBuf],
    projects: &mut Projects,
    cwd: &Path,
) -> Result<(Discovered, discover::Found), Failure> {
    // With one base, discovery's own order is the run's, so nothing need be merged.
    if let [base] = bases {
        let project = projects.of(base, cwd);
        let mut found = discover_base(base, project, projects, cwd)?;
        let files = std::mem::take(&mut found.files)
            .into_iter()
            .map(|path| {
                let owner = projects.of_file(&path, project, cwd);
                (path, owner, vec![true])
            })
            .collect();
        return Ok((files, found));
    }
    let mut files: std::collections::BTreeMap<PathBuf, (usize, Vec<bool>)> =
        std::collections::BTreeMap::new();
    let mut all = discover::Found::default();
    let mut links = std::collections::BTreeSet::new();
    let mut specials = std::collections::BTreeSet::new();
    for (index, base) in bases.iter().enumerate() {
        let project = projects.of(base, cwd);
        let found = discover_base(base, project, projects, cwd)?;
        for path in found.files {
            files
                .entry(path)
                .or_insert_with(|| (project, vec![false; bases.len()]))
                .1[index] = true;
        }
        // Bases overlap when one statement's lies under another's, so each is kept once.
        links.extend(found.links_not_followed);
        specials.extend(found.special_files);
        for rule in found.skipped_rules {
            if !all.skipped_rules.contains(&rule) {
                all.skipped_rules.push(rule);
            }
        }
    }
    all.links_not_followed = links.into_iter().collect();
    all.special_files = specials.into_iter().collect();
    let files = files
        .into_iter()
        .map(|(path, (_, found_by))| {
            // The deepest project holding the file, whichever base found it first, so that its
            // encoding never depends on the order the statements were written in.
            let deepest = projects.deepest(&path);
            let owner = projects.of_file(&path, deepest, cwd);
            (path, owner, found_by)
        })
        .collect();
    Ok((files, all))
}

/// The files under `base`, discovered by the rules of `project`, the one it is in.
fn discover_base(
    base: &Path,
    project: usize,
    projects: &Projects,
    cwd: &Path,
) -> Result<discover::Found, Failure> {
    let project = &projects.list[project];
    // With no project, a repository the walk enters still has its own rules.
    let system = match &project.declarations.workspace {
        Some(workspace) => workspace.ignore_files(),
        None => workspace::ignore_files_outside(&projects.contents),
    };
    discover::files(
        base,
        &project.boundary,
        Some(system),
        cwd,
        &projects.contents,
    )
    .map_err(Failure::Discovery)
}
