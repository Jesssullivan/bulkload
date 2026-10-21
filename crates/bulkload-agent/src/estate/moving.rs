//! `estate-move-source` and `estate-move` (M2 item 7).
//!
//! Design `docs/agent-notes/2026-10-16-estate-move-design.md` (PR #251);
//! operator ruling A-20261018-110. The in-place lane's glue (`neo-gen-items*.sh`,
//! `presence.py`, the `items-i0*.tsv` tags, `refs-in.py`, `sting-carry*.sh`)
//! as library steps the two verbs compose. Every write is an existing verb's
//! (`estate-add-batch`, `estate-capture`, `pull`/`copy`, `estate-apply
//! --adopt --beside --refs-in`, `estate-carry-files`, `estate-handoff`,
//! `estate-reseat`); this module only reads, decides and records.
//!
//! - [`inventory`] (source, read-only): every Git repository under the
//!   source roots, grouped as Git groups it (a main and every worktree it
//!   registers, wherever they live; a bare repository and its worktrees), one
//!   plan item per checkout, each path mapped by the longest `--map`
//!   prefix (else kept: same paths, axiom 1). A linked worktree found under
//!   a root brings its whole group, main included, even when the main is
//!   outside every root (nothing excluded, axiom 2). What is not a Git
//!   checkout (a symlink, a plain file, a prunable worktree, an unreadable
//!   repository) is a [`Note`], printed and counted, never silently dropped.
//! - [`presence`] (destination, read-only): what the live path holds now.
//! - [`action`] (pure): one action per item from its capture and presence,
//!   the design's table (section 2.1 step 5). Apply decides adopt-or-beside
//!   itself (its adopt proof), so `adopt` here means "apply proves it".
//! - [`Ledger`] (`MOVE_STATE/move.ledger`): the beside label and the source
//!   label fixed at the first run (a rerun naming others is
//!   `RECEIPT_BINDING_INVALID`), each item's decision, and each phase
//!   reached. A carry decision stays, once a carry was started for it, while
//!   the item stays uncaptured: a rerun never moves an item carried at its
//!   live path beside, or back.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{id, read, write, Item};
use crate::git_carry::converge;
use crate::{BulkloadRefusal, Result};

/// One item [`inventory`] plans, with its shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub item: Item,
    /// `main`, `linked` or `bare`.
    pub kind: &'static str,
}

/// Something under a source root that is not planned, and why: printed and
/// counted by `estate-move-source` (nothing is dropped silently).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub path: PathBuf,
    /// `symlink` (not followed), `not-git` (plain files directly in a
    /// directory the walk descended; `count` of them), `prunable` (a
    /// registered worktree whose directory is gone), `unreadable` (Git
    /// refused to list the repository's worktrees, or a root or entry
    /// could not be read; `code`, typed for a root:
    /// `SOURCE_ROOT_UNREADABLE`), `source-vanished` (a `SOURCE_ROOT` gone
    /// when the pass reached it, `SOURCE_ROOT_MISSING`, #277; or a name
    /// beneath one gone mid-walk, `SOURCE_VANISHED`).
    pub what: &'static str,
    pub count: u64,
    pub code: Option<&'static str>,
}

/// What [`inventory`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub items: Vec<Planned>,
    pub notes: Vec<Note>,
}

/// Map `path` by the longest `--map` prefix, else keep it (axiom 1).
#[must_use]
pub fn live_path(path: &Path, maps: &[(PathBuf, PathBuf)]) -> PathBuf {
    super::retarget::mapped(path, maps).unwrap_or_else(|| path.to_owned())
}

// A directory Git can name a repository by: a `.git` entry, or a bare
// repository's `HEAD` and `objects` (as `estate-diff --scan` reads it).
fn git_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
        && (fs::symlink_metadata(path.join(".git")).is_ok()
            || (path.join("HEAD").is_file() && path.join("objects").is_dir()))
}

/// Every Git repository under `roots` (read-only; see the module docs).
///
/// # Errors
/// `PATH_NOT_ABSOLUTE` for a relative root or map; IO reading a root.
pub fn inventory(roots: &[PathBuf], maps: &[(PathBuf, PathBuf)]) -> Result<Inventory> {
    if roots.iter().any(|root| !root.is_absolute())
        || maps
            .iter()
            .any(|(old, new)| !old.is_absolute() || !new.is_absolute())
    {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    let (found, mut notes) = walk(roots)?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for directory in found {
        group(directory, maps, (&mut seen, &mut items, &mut notes));
    }
    items.sort_by(|a, b| a.item.source.cmp(&b.item.source));
    Ok(Inventory { items, notes })
}

// Every Git directory under `roots` (the walk stops at one and follows no
// symlink), and the notes for what is not one.
fn walk(roots: &[PathBuf]) -> Result<(Vec<PathBuf>, Vec<Note>)> {
    const SITE: &str = "estate::moving::walk";
    let mut found = Vec::new();
    let mut notes = Vec::new();
    let mut usable = 0_usize;
    let mut first_refusal = None;
    for root in roots {
        // #277: the source is live (S5, Q152), so a root listed moments ago
        // may be gone or unreadable by the time the pass reaches it. That is
        // a typed note the pass continues past, never the whole pass's
        // refusal; only a pass with no usable root at all refuses, with
        // the first root's refusal.
        let root = match fs::canonicalize(root) {
            Ok(root) => root,
            Err(error) => {
                let refusal = crate::refuse::source_root(&error, SITE);
                notes.push(typed_note(root.clone(), &refusal));
                first_refusal.get_or_insert(refusal);
                continue;
            }
        };
        let mut stack = vec![root.clone()];
        while let Some(directory) = stack.pop() {
            let is_root = directory == root;
            if git_directory(&directory) {
                usable += usize::from(is_root);
                // B2 part 3: a nested repository or a populated submodule
                // under this one's worktree is its own item, walked as one
                // (the enclosing item excludes its path).
                if let Ok(nests) = crate::git_carry::nested_repositories(&directory) {
                    use std::os::unix::ffi::OsStrExt as _;
                    for nest in nests.iter().rev() {
                        if nest.kind == crate::git_carry::NestedRepositoryKind::Directory {
                            stack.push(directory.join(std::ffi::OsStr::from_bytes(&nest.rel_path)));
                        }
                    }
                }
                found.push(directory);
                continue;
            }
            // A directory or entry the walk cannot read (a protected folder,
            // a name removed under the live source) is a note, never the
            // whole inventory's refusal.
            let listed: std::io::Result<Vec<PathBuf>> =
                fs::read_dir(&directory).and_then(|entries| {
                    entries
                        .map(|entry| entry.map(|entry| entry.path()))
                        .collect()
                });
            // The root itself (mode 000, or gone since it was resolved) is
            // the root's typed refusal and leaves the root unusable; a
            // directory beneath it gone since its listing is
            // `SOURCE_VANISHED` (#283 review 5). Any other failure beneath
            // the root keeps its errno (per-seat typing is gap C5's).
            let mut entries = match listed {
                Ok(entries) => {
                    usable += usize::from(is_root);
                    entries
                }
                Err(error) if is_root => {
                    let refusal = crate::refuse::source_root(&error, SITE);
                    notes.push(typed_note(directory, &refusal));
                    first_refusal.get_or_insert(refusal);
                    continue;
                }
                Err(error) => {
                    notes.push(typed_note(
                        directory,
                        &crate::refuse::source_seat(&error, SITE),
                    ));
                    continue;
                }
            };
            entries.sort();
            let mut plain = 0u64;
            // Pushed in reverse so the walk visits names in order.
            for path in entries.into_iter().rev() {
                // Listed and gone before its stat: `SOURCE_VANISHED`.
                let meta = match fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) => {
                        notes.push(typed_note(path, &crate::refuse::source_seat(&error, SITE)));
                        continue;
                    }
                };
                if meta.is_symlink() {
                    notes.push(Note {
                        path,
                        what: "symlink",
                        count: 1,
                        code: None,
                    });
                } else if meta.is_dir() {
                    stack.push(path);
                } else {
                    plain += 1;
                }
            }
            if plain > 0 {
                notes.push(Note {
                    path: directory,
                    what: "not-git",
                    count: plain,
                    code: None,
                });
            }
        }
    }
    if usable == 0 {
        if let Some(refusal) = first_refusal {
            return Err(refusal);
        }
    }
    Ok((found, notes))
}

const fn unreadable(path: PathBuf, refusal: &BulkloadRefusal) -> Note {
    Note {
        path,
        what: "unreadable",
        count: 1,
        code: Some(refusal.code()),
    }
}

/// A walk refusal as a note: a root or entry that is gone (`ENOENT`) is
/// `source-vanished`, anything else `unreadable`; each keeps its typed code.
const fn typed_note(path: PathBuf, refusal: &BulkloadRefusal) -> Note {
    if matches!(
        refusal,
        BulkloadRefusal::SourceRootMissing | BulkloadRefusal::SourceVanished
    ) {
        Note {
            path,
            what: "source-vanished",
            count: 1,
            code: Some(refusal.code()),
        }
    } else {
        unreadable(path, refusal)
    }
}

// The group of the repository at `directory`, once per repository however
// many of its worktrees the walk met (a linked worktree under a root brings
// its main along).
fn group(
    directory: PathBuf,
    maps: &[(PathBuf, PathBuf)],
    (seen, items, notes): (&mut BTreeSet<PathBuf>, &mut Vec<Planned>, &mut Vec<Note>),
) {
    let listed = match converge::worktree_census(&directory) {
        Ok(listed) => listed,
        Err(refusal) => {
            notes.push(unreadable(directory, &refusal));
            return;
        }
    };
    let Some(main) = listed.first() else {
        notes.push(unreadable(
            directory,
            &BulkloadRefusal::GitInventoryMalformed,
        ));
        return;
    };
    // A directory its repository does not list (a copied worktree whose
    // `.git` names another's administration) is no item of that group.
    let real = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let here = real(&directory);
    if !listed.iter().any(|entry| real(&entry.path) == here) {
        notes.push(Note {
            path: directory,
            what: "not-in-census",
            count: 1,
            code: None,
        });
    }
    let key = fs::canonicalize(&main.path).unwrap_or_else(|_| main.path.clone());
    if !seen.insert(key) {
        return;
    }
    let repository = live_path(&main.path, maps);
    items.push(Planned {
        item: Item {
            source: main.path.clone(),
            repository: repository.clone(),
            workspace: (!main.bare).then(|| repository.clone()),
        },
        kind: if main.bare { "bare" } else { "main" },
    });
    for linked in listed.iter().skip(1) {
        if linked.prunable || !linked.path.is_dir() {
            notes.push(Note {
                path: linked.path.clone(),
                what: "prunable",
                count: 1,
                code: None,
            });
            continue;
        }
        items.push(Planned {
            item: Item {
                source: linked.path.clone(),
                repository: repository.clone(),
                workspace: Some(live_path(&linked.path, maps)),
            },
            kind: "linked",
        });
    }
}

/// What a live path holds now (read-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Presence {
    /// Nothing there.
    Absent,
    /// An empty directory.
    Empty,
    /// A Git checkout or repository.
    Git,
    /// A non-empty directory that holds no repository, or a file.
    NotGit,
    /// A symlink, or a path whose real path is another spelling (a
    /// symlinked ancestor).
    Symlinked,
    /// The path could not be read.
    Unreadable,
}

impl Presence {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Empty => "empty",
            Self::Git => "git",
            Self::NotGit => "not-git",
            Self::Symlinked => "symlinked",
            Self::Unreadable => "unreadable",
        }
    }
}

/// What `path` holds now. Never writes.
#[must_use]
pub fn presence(path: &Path) -> Presence {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Presence::Absent,
        Err(_) => return Presence::Unreadable,
    };
    if meta.is_symlink() {
        return Presence::Symlinked;
    }
    match fs::canonicalize(path) {
        Ok(real) if real == path => {}
        Ok(_) => return Presence::Symlinked,
        Err(_) => return Presence::Unreadable,
    }
    if !meta.is_dir() {
        return Presence::NotGit;
    }
    if git_directory(path) {
        return Presence::Git;
    }
    fs::read_dir(path).map_or(Presence::Unreadable, |mut entries| {
        if entries.next().is_none() {
            Presence::Empty
        } else {
            Presence::NotGit
        }
    })
}

/// One item's action (the design's plan table, section 2.1 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    /// A first landing at the live path (`estate-apply`; parents made).
    Land,
    /// The live checkout exists: `estate-apply --adopt --beside` adopts it
    /// when it proves equal, else lands the capture beside it; refs-in
    /// either way.
    Adopt,
    /// A refs-only (bare) item whose live repository exists: adopted, refs
    /// imported, refs-in.
    RefsOnly,
    /// Capture refused: carried as files at the live path (absent or empty).
    CarryLive,
    /// Capture refused and the live path is occupied: carried as files at
    /// `<path>.<label>`.
    CarryBeside,
    /// No action the table covers: the apply refuses it typed with nothing
    /// written, and closure holds it pending review.
    Refuse,
}

impl Action {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Land => "land",
            Self::Adopt => "adopt",
            Self::RefsOnly => "refs-only",
            Self::CarryLive => "carry-live",
            Self::CarryBeside => "carry-beside",
            Self::Refuse => "refuse",
        }
    }

    /// Carried as files (`estate-carry-files`).
    #[must_use]
    pub const fn carries(self) -> bool {
        matches!(self, Self::CarryLive | Self::CarryBeside)
    }
}

/// What the source half's latest finished pass says of an item's capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capture {
    /// The corpus holds its capture record.
    Held,
    /// The pass refused it with a code Git carry cannot take on a rerun:
    /// carried as files.
    Refused,
    /// The pass refused it with a code a rerun of the source half may clear
    /// ([`TRANSIENT`]): left pending, never carried as files.
    Transient,
    /// No record and no recorded refusal: the pass never reached it.
    Pending,
}

/// Capture refusals a rerun of the source half may clear (drift under the
/// pass, space, budget, a lost worker or a bare IO): the item is left
/// pending, never carried as files.
pub const TRANSIENT: &[&str] = &[
    "CAPTURE_DRIFTED",
    "GIT_AUTHORITY_CHANGED",
    "DESTINATION_SPACE_INSUFFICIENT",
    "SPACE_EXHAUSTED",
    "BUDGET_EXCEEDED",
    "WORKER_LOST",
    "IO",
];

/// What the corpus and the source half's refusals say of `identity`.
#[must_use]
pub fn capture_of(held: bool, refusal: Option<&str>) -> Capture {
    match (held, refusal) {
        (true, _) => Capture::Held,
        (false, Some(code)) if TRANSIENT.contains(&code) || code.starts_with("IO") => {
            Capture::Transient
        }
        (false, Some(_)) => Capture::Refused,
        (false, None) => Capture::Pending,
    }
}

/// The action for an item, and the table row's name as its reason.
///
/// Its inputs: a workspace item or a refs-only one, its [`Capture`], what
/// its live path (the workspace, or a refs-only item's repository) holds,
/// and whether `--side-root` seats symlinked paths. Pure.
#[must_use]
pub const fn action(
    workspace: bool,
    capture: Capture,
    presence: Presence,
    seats: bool,
) -> (Action, &'static str) {
    let held = matches!(capture, Capture::Held);
    match (workspace, capture, presence) {
        (_, Capture::Pending, _) => (Action::Refuse, "capture-pending"),
        (_, Capture::Transient, _) => (Action::Refuse, "capture-transient"),
        (_, _, Presence::Unreadable) => (Action::Refuse, "unreadable"),
        // A live path spelled through a symlink (the path, or an ancestor:
        // map to the real path) is never acted on through it.
        (_, _, Presence::Symlinked) if !(seats && held) => (Action::Refuse, "symlinked-path"),
        (true, Capture::Held, Presence::Absent | Presence::Empty) => (Action::Land, "absent"),
        (true, Capture::Held, Presence::Git | Presence::Symlinked) => {
            (Action::Adopt, "adopt-or-beside")
        }
        (_, Capture::Held, Presence::NotGit) => (Action::Refuse, "not-git"),
        (true, Capture::Refused, Presence::Absent | Presence::Empty) => {
            (Action::CarryLive, "capture-refused")
        }
        (true, Capture::Refused, Presence::Git | Presence::NotGit | Presence::Symlinked) => {
            (Action::CarryBeside, "capture-refused-occupied")
        }
        (false, Capture::Held, Presence::Absent | Presence::Empty) => {
            (Action::Land, "bare-landing")
        }
        (false, Capture::Held, Presence::Git | Presence::Symlinked) => {
            (Action::RefsOnly, "refs-only-adopt")
        }
        (false, Capture::Refused, _) => (Action::Refuse, "capture-refused-refs-only"),
    }
}

/// The plan's preview for a linked worktree item whose live path is a
/// directory with no `.git` at all.
///
/// 2026-10-19, the td01 batch; review of #286: the apply adopts it by
/// proof or lands the capture beside it
/// (`git_carry::converge::check_nonrepo`), so the plan says `adopt`
/// (`nonrepo-adopt-or-beside`), never `refuse` (`not-git`). Every other
/// decision of [`action`] is kept, a standalone item's non-repo directory
/// (refused typed by the apply) and a file included. Reads the path only.
#[must_use]
pub fn nonrepo_preview(
    item: &Item,
    capture: Capture,
    (action, reason): (Action, &'static str),
) -> (Action, &'static str) {
    let linked = item
        .workspace
        .as_ref()
        .filter(|workspace| **workspace != item.repository);
    let nonrepo = linked.is_some_and(|path| {
        fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
            && matches!(
                fs::symlink_metadata(path.join(".git")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
    });
    if action == Action::Refuse && reason == "not-git" && capture == Capture::Held && nonrepo {
        (Action::Adopt, "nonrepo-adopt-or-beside")
    } else {
        (action, reason)
    }
}

/// One item's recorded decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub action: Action,
    pub reason: String,
    pub presence: Presence,
    pub capture: Capture,
    /// When this action was first decided (ns since the epoch).
    pub decided_ns: i128,
    /// A carry was started for it (recorded before the carry runs): from
    /// then on the decision stays while the item is not captured.
    pub attempted: bool,
}

/// One phase a run reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub phase: String,
    /// `ok`, or `refused:<CODE>`.
    pub outcome: String,
    pub ns: i128,
}

/// `MOVE_STATE/move.ledger` (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// The beside label: `<path>.<label>` and `refs/<label>/`.
    pub label: String,
    /// The provenance label every apply, carry and closure is given.
    pub source: String,
    /// The source host (`-` for this host).
    pub host: String,
    /// The source half's `MOVE_STATE`, on the source host.
    pub source_state: PathBuf,
    pub items: BTreeMap<String, Decision>,
    pub phases: Vec<Phase>,
}

/// Where the ledger lives in `state`.
#[must_use]
pub fn ledger_path(state: &Path) -> PathBuf {
    state.join("move.ledger")
}

impl Ledger {
    /// The ledger at `path`, or a new one; bound to the labels, host and
    /// source state.
    ///
    /// # Errors
    /// `RECEIPT_BINDING_INVALID` when the ledger there names another label,
    /// source label, host or source state; a ledger that does not decode.
    pub fn open(
        path: &Path,
        label: &str,
        source: &str,
        host: &str,
        source_state: &Path,
    ) -> Result<Self> {
        if !path
            .try_exists()
            .refuse_at("estate::moving::Ledger::open")?
        {
            return Ok(Self {
                label: label.to_owned(),
                source: source.to_owned(),
                host: host.to_owned(),
                source_state: source_state.to_owned(),
                ..Self::default()
            });
        }
        let ledger: Self = read(path)?;
        if ledger.label != label
            || ledger.source != source
            || ledger.host != host
            || ledger.source_state != source_state
        {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        Ok(ledger)
    }

    /// Write the ledger at `path` (temporary, fsync, rename, directory
    /// fsync).
    ///
    /// # Errors
    /// IO.
    pub fn save(&self, path: &Path) -> Result<()> {
        write(path, self)
    }

    /// Record that `phase` ended with `outcome`, at `ns`.
    pub fn reached(&mut self, phase: &str, outcome: String, ns: i128) {
        self.phases.push(Phase {
            phase: phase.to_owned(),
            outcome,
            ns,
        });
    }
}

/// One item of the destination plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub item: Item,
    pub decision: Decision,
    /// The decision is an earlier run's, kept (a carry stays where it went).
    pub kept: bool,
}

/// What [`decide`] reads besides the plan.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The pulled corpus (capture records).
    pub corpus: &'a Path,
    /// The source half's latest finished pass's refusals, by item.
    pub refusals: &'a BTreeMap<String, String>,
    /// The apply's private state and source label (carry records).
    pub state: &'a Path,
    pub source: &'a str,
    /// `--side-root` was given.
    pub seats: bool,
}

/// Decide every item of `items` (the pulled plan) against the destination.
///
/// The decisions go into `ledger` (see the module docs). A carry decision
/// stays while the item is not captured, once a carry was started for it
/// or a carry record names it; every other decision is taken again. Reads
/// the corpus, the carry records and each live path's presence; writes
/// nothing.
///
/// # Errors
/// An item that does not encode; IO probing the corpus.
pub fn decide(
    items: &[Item],
    inputs: &Inputs<'_>,
    ledger: &mut Ledger,
    now_ns: i128,
) -> Result<Vec<Row>> {
    const SITE: &str = "estate::moving::decide";
    let mut rows = Vec::with_capacity(items.len());
    for item in items {
        let identity = id(item)?;
        // A corpus capture record, or the streamed capture's item snapshot
        // beside the corpus (SPEC rule 2 reset, B1).
        let held = inputs
            .corpus
            .join(format!("{identity}.capture"))
            .try_exists()
            .refuse_at(SITE)?
            || inputs
                .corpus
                .parent()
                .is_some_and(|carry| super::streamed::snapshot_path(carry, &identity).is_file());
        let capture = capture_of(held, inputs.refusals.get(&identity).map(String::as_str));
        let place = item.workspace.as_ref().unwrap_or(&item.repository);
        let now = presence(place);
        let decided = action(item.workspace.is_some(), capture, now, inputs.seats);
        let (action, reason) = nonrepo_preview(item, capture, decided);
        let previous = ledger.items.get(&identity);
        let carried = super::carried::carried_path(inputs.state, &identity, inputs.source)
            .try_exists()
            .refuse_at(SITE)?;
        let keep = capture != Capture::Held
            && previous.is_some_and(|previous| {
                previous.action.carries() && (previous.attempted || carried)
            });
        let decision = if keep {
            previous
                .cloned()
                .ok_or(BulkloadRefusal::GitAuthorityChanged)?
        } else {
            Decision {
                action,
                reason: reason.to_owned(),
                presence: now,
                capture,
                decided_ns: match previous {
                    Some(previous) if previous.action == action => previous.decided_ns,
                    _ => now_ns,
                },
                attempted: false,
            }
        };
        ledger.items.insert(identity.clone(), decision.clone());
        rows.push(Row {
            id: identity,
            item: item.clone(),
            decision,
            kept: keep,
        });
    }
    Ok(rows)
}

/// The count of each action over `rows`, in [`Action`] order.
#[must_use]
pub fn histogram(rows: &[Row]) -> BTreeMap<Action, u64> {
    let mut counts = BTreeMap::new();
    for row in rows {
        *counts.entry(row.decision.action).or_default() += 1;
    }
    counts
}

/// Create `path` (0700) or check the private directory already there.
///
/// # Errors
/// `PATH_ESCAPES_ROOT` for a path that is not this user's private
/// directory; IO.
pub fn private_dir(path: &Path) -> Result<()> {
    super::private_directory(path)
}

/// The source half's refusals file: one `ITEM CODE` line per item its pass
/// refused.
#[must_use]
pub fn render_refusals(refusals: &BTreeMap<String, String>) -> String {
    use std::fmt::Write as _;
    refusals
        .iter()
        .fold(String::new(), |mut text, (item, code)| {
            let _ = writeln!(text, "{item} {code}");
            text
        })
}

/// [`render_refusals`] read back; a malformed line is skipped.
#[must_use]
pub fn parse_refusals(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(item, code)| (item.to_owned(), code.to_owned()))
        .collect()
}

/// The source half's notes file: one `WHAT COUNT CODE PATH` line per note
/// (the path escaped, last).
#[must_use]
pub fn render_notes(notes: &[Note]) -> String {
    use std::fmt::Write as _;
    notes.iter().fold(String::new(), |mut text, note| {
        let _ = writeln!(
            text,
            "{} {} {} {}",
            note.what,
            note.count,
            note.code.unwrap_or("none"),
            note.path.display().to_string().escape_debug()
        );
        text
    })
}

/// The count of each kind of note in a notes file.
#[must_use]
pub fn note_counts(text: &str) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for line in text.lines() {
        let mut words = line.splitn(3, ' ');
        if let (Some(what), Some(count)) = (words.next(), words.next()) {
            *counts.entry(what.to_owned()).or_default() += count.parse::<u64>().unwrap_or(1);
        }
    }
    counts
}

/// Write `bytes` at `path`, replacing it whole (temporary, fsync, rename,
/// directory fsync): the move's closure report.
///
/// # Errors
/// IO.
pub fn write_report(path: &Path, bytes: &[u8]) -> Result<()> {
    super::write_bytes(path, bytes)
}

/// Hold `state`'s move lock for the life of the returned guard: one move
/// per state directory at a time.
///
/// # Errors
/// `MOVE_STATE_BUSY` naming the lock for a lock another process holds
/// (refused, never waited on, #279); IO otherwise.
pub fn lock(state: &Path) -> Result<impl Drop> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = state.join("move.lock");
    super::exclusive(&path).map_err(|refusal| match refusal {
        BulkloadRefusal::Io(Some(libc::EWOULDBLOCK)) => {
            BulkloadRefusal::MoveStateBusy(path.as_os_str().as_bytes().to_vec())
        }
        other => other,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::test_support::prop_config;
    use proptest::prelude::*;
    use std::process::Command;

    // Review of #286: the plan previews a linked item's non-repo directory
    // as the apply treats it (adopt or beside), and nothing else changes.
    #[test]
    fn a_linked_nonrepo_directory_previews_adopt() {
        let base = std::env::temp_dir().join(format!("bulkload-preview-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let wt = base.join("repo.worktrees/wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join("a.txt"), b"a\n").unwrap();
        let linked = Item {
            source: PathBuf::from("/src/wt"),
            repository: base.join("repo"),
            workspace: Some(wt.clone()),
        };
        let refused = (Action::Refuse, "not-git");
        assert_eq!(
            nonrepo_preview(&linked, Capture::Held, refused),
            (Action::Adopt, "nonrepo-adopt-or-beside")
        );
        assert_eq!(nonrepo_preview(&linked, Capture::Pending, refused), refused);
        let standalone = Item {
            repository: wt.clone(),
            ..linked.clone()
        };
        assert_eq!(
            nonrepo_preview(&standalone, Capture::Held, refused),
            refused
        );
        fs::write(wt.join(".git"), b"gitdir: /nowhere\n").unwrap();
        assert_eq!(nonrepo_preview(&linked, Capture::Held, refused), refused);
        fs::remove_dir_all(&base).unwrap();
    }

    const PRESENCES: [Presence; 6] = [
        Presence::Absent,
        Presence::Empty,
        Presence::Git,
        Presence::NotGit,
        Presence::Symlinked,
        Presence::Unreadable,
    ];

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@localhost",
                "-c",
                "commit.gpgsign=false",
                "-C",
            ])
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn base(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("bulkload-moving-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::canonicalize(&base).unwrap()
    }

    const CAPTURES: [Capture; 4] = [
        Capture::Held,
        Capture::Refused,
        Capture::Transient,
        Capture::Pending,
    ];

    #[test]
    fn the_table_never_lands_an_uncaptured_item_or_writes_through_a_link() {
        for workspace in [true, false] {
            for capture in CAPTURES {
                for seen in PRESENCES {
                    for seats in [true, false] {
                        let (action, reason) = action(workspace, capture, seen, seats);
                        assert!(!reason.is_empty());
                        let held = capture == Capture::Held;
                        if !held {
                            assert!(
                                matches!(
                                    action,
                                    Action::CarryLive | Action::CarryBeside | Action::Refuse
                                ),
                                "{workspace} {seen:?} {action:?}"
                            );
                        }
                        // Only a recorded, lasting refusal is carried as files.
                        if action.carries() {
                            assert_eq!(capture, Capture::Refused);
                        }
                        if action == Action::CarryLive {
                            assert!(matches!(seen, Presence::Absent | Presence::Empty));
                        }
                        if !workspace {
                            assert!(!action.carries(), "a refs-only item has no files");
                        }
                        if seen == Presence::Symlinked && !(seats && held) {
                            assert_eq!(action, Action::Refuse);
                        }
                        if seen == Presence::NotGit && held {
                            assert_eq!(action, Action::Refuse, "never adopt a non-Git tree");
                        }
                        if seen == Presence::Unreadable {
                            assert_eq!(action, Action::Refuse);
                        }
                    }
                }
            }
        }
        assert_eq!(capture_of(true, Some("IO")), Capture::Held);
        assert_eq!(
            capture_of(false, Some("CAPTURE_DRIFTED")),
            Capture::Transient
        );
        assert_eq!(
            capture_of(false, Some("GIT_SOURCE_PARTIAL_CLONE")),
            Capture::Refused
        );
        assert_eq!(capture_of(false, None), Capture::Pending);
    }

    #[test]
    fn presence_reads_each_shape() {
        let base = base("presence");
        assert_eq!(presence(&base.join("none")), Presence::Absent);
        fs::create_dir(base.join("empty")).unwrap();
        assert_eq!(presence(&base.join("empty")), Presence::Empty);
        fs::create_dir(base.join("tree")).unwrap();
        fs::write(base.join("tree/f"), b"x").unwrap();
        assert_eq!(presence(&base.join("tree")), Presence::NotGit);
        fs::write(base.join("file"), b"x").unwrap();
        assert_eq!(presence(&base.join("file")), Presence::NotGit);
        fs::create_dir(base.join("repo")).unwrap();
        git(&base.join("repo"), &["init", "--quiet", "--template="]);
        assert_eq!(presence(&base.join("repo")), Presence::Git);
        std::os::unix::fs::symlink(base.join("repo"), base.join("link")).unwrap();
        assert_eq!(presence(&base.join("link")), Presence::Symlinked);
        std::os::unix::fs::symlink(&base, base.join("up")).unwrap();
        assert_eq!(presence(&base.join("up/repo")), Presence::Symlinked);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One fixture, every shape.
    fn the_inventory_groups_every_worktree_and_notes_the_rest() {
        let base = base("inventory");
        let root = base.join("git");
        let elsewhere = base.join("elsewhere");
        fs::create_dir_all(root.join("group")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        // A main with a linked worktree inside the root, one outside it,
        // and one whose directory is gone.
        let main = root.join("group/main");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--quiet", "--template=", "-b", "main"]);
        git(&main, &["commit", "--quiet", "--allow-empty", "-m", "c0"]);
        git(&main, &["worktree", "add", "--quiet", "-b", "a", "../wt-a"]);
        let outside = elsewhere.join("wt-b");
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "b",
                outside.to_str().unwrap(),
            ],
        );
        git(&main, &["worktree", "add", "--quiet", "-b", "c", "../gone"]);
        fs::remove_dir_all(root.join("group/gone")).unwrap();
        // A main outside the root whose linked worktree is under it.
        let far = elsewhere.join("far");
        fs::create_dir(&far).unwrap();
        git(&far, &["init", "--quiet", "--template=", "-b", "main"]);
        git(&far, &["commit", "--quiet", "--allow-empty", "-m", "c0"]);
        git(
            &far,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "x",
                root.join("far-wt").to_str().unwrap(),
            ],
        );
        // A bare repository, a plain file and a symlink.
        git(
            &base,
            &[
                "init",
                "--quiet",
                "--template=",
                "--bare",
                root.join("bare.git").to_str().unwrap(),
            ],
        );
        fs::write(root.join("notes.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        let live = base.join("live");
        let maps = vec![(root.clone(), live.clone())];
        let found = inventory(std::slice::from_ref(&root), &maps).unwrap();
        let mut got: Vec<(PathBuf, PathBuf, Option<PathBuf>, &str)> = found
            .items
            .iter()
            .map(|p| {
                (
                    p.item.source.clone(),
                    p.item.repository.clone(),
                    p.item.workspace.clone(),
                    p.kind,
                )
            })
            .collect();
        got.sort();
        let mut want = vec![
            (
                main,
                live.join("group/main"),
                Some(live.join("group/main")),
                "main",
            ),
            (
                root.join("group/wt-a"),
                live.join("group/main"),
                Some(live.join("group/wt-a")),
                "linked",
            ),
            (
                outside.clone(),
                live.join("group/main"),
                Some(outside),
                "linked",
            ),
            (far.clone(), far.clone(), Some(far.clone()), "main"),
            (
                root.join("far-wt"),
                far,
                Some(live.join("far-wt")),
                "linked",
            ),
            (root.join("bare.git"), live.join("bare.git"), None, "bare"),
        ];
        want.sort();
        assert_eq!(got, want);
        let notes: BTreeSet<(&str, PathBuf)> = found
            .notes
            .iter()
            .map(|note| (note.what, note.path.clone()))
            .collect();
        assert!(
            notes.contains(&("prunable", root.join("group/gone"))),
            "{notes:?}"
        );
        assert!(notes.contains(&("symlink", root.join("link"))), "{notes:?}");
        assert!(notes.contains(&("not-git", root)), "{notes:?}");
        // Relative roots or maps are refused.
        assert_eq!(
            inventory(&[PathBuf::from("git")], &[]),
            Err(BulkloadRefusal::PathNotAbsolute)
        );
        fs::remove_dir_all(&base).unwrap();
    }

    // #277: a root gone by the time the pass reaches it is a typed note and
    // the pass continues with the rest; only no usable root at all refuses.
    #[test]
    fn a_vanished_root_is_a_note_and_the_pass_continues() {
        let base = base("vanished");
        let live = base.join("live");
        fs::create_dir_all(live.join("repo")).unwrap();
        git(&live.join("repo"), &["init", "--quiet", "--template="]);
        let gone = base.join("gone");
        let found = inventory(&[gone.clone(), live.clone()], &[]).unwrap();
        assert_eq!(
            found
                .items
                .iter()
                .map(|planned| planned.item.source.clone())
                .collect::<Vec<_>>(),
            vec![live.join("repo")],
            "{found:?}"
        );
        assert!(
            found.notes.contains(&Note {
                path: gone.clone(),
                what: "source-vanished",
                count: 1,
                code: Some("SOURCE_ROOT_MISSING"),
            }),
            "{:?}",
            found.notes
        );
        assert_eq!(
            inventory(&[gone, base.join("also-gone")], &[]),
            Err(BulkloadRefusal::SourceRootMissing)
        );
        fs::remove_dir_all(&base).unwrap();
    }

    // #283 review 5: a root that cannot be listed (mode 000) is typed and
    // unusable, the pass continues with the rest; a root that holds no
    // repository is usable and yields nothing; every walk refusal keeps its
    // typed code in its note.
    #[test]
    fn unreadable_and_empty_roots_are_typed_notes() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = base("rootkinds");
        let live = base.join("live");
        fs::create_dir_all(live.join("repo")).unwrap();
        git(&live.join("repo"), &["init", "--quiet", "--template="]);
        let empty = base.join("empty");
        fs::create_dir_all(empty.join("plain")).unwrap();
        fs::write(empty.join("plain/f.txt"), b"f\n").unwrap();
        let locked = base.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok();
        let found = inventory(&[locked.clone(), empty.clone(), live], &[]);
        let found_empty = inventory(std::slice::from_ref(&empty), &[]);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        let found = found.unwrap();
        assert_eq!(found.items.len(), 1, "{found:?}");
        if !readable {
            assert!(
                found.notes.contains(&Note {
                    path: locked,
                    what: "unreadable",
                    count: 1,
                    code: Some("SOURCE_ROOT_UNREADABLE"),
                }),
                "{:?}",
                found.notes
            );
        }
        let found_empty = found_empty.unwrap();
        assert!(found_empty.items.is_empty());
        assert!(found_empty.notes.iter().any(|note| note.what == "not-git"));
        for (refusal, what) in [
            (BulkloadRefusal::SourceRootMissing, "source-vanished"),
            (BulkloadRefusal::SourceVanished, "source-vanished"),
            (
                BulkloadRefusal::SourceRootUnreadable(Some(13)),
                "unreadable",
            ),
            (BulkloadRefusal::Io(Some(13)), "unreadable"),
        ] {
            let note = typed_note(PathBuf::from("/x"), &refusal);
            assert_eq!((note.what, note.code), (what, Some(refusal.code())));
        }
        fs::remove_dir_all(&base).unwrap();
    }

    // #279: a held move state is typed and names its lock; once the holder
    // lets go, the next run takes it.
    #[test]
    fn a_held_move_state_is_busy_and_named() {
        let base = base("busy");
        let held = lock(&base).unwrap();
        let lock_path = base.join("move.lock");
        let refused = lock(&base).err().expect("a second lock on a held state");
        assert_eq!(
            refused,
            BulkloadRefusal::MoveStateBusy(lock_path.as_os_str().as_encoded_bytes().to_vec())
        );
        assert_eq!(
            refused.to_string(),
            format!("MOVE_STATE_BUSY path=\"{}\"", lock_path.display())
        );
        drop(held);
        assert!(lock(&base).is_ok());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_ledger_binds_its_labels() {
        let base = base("ledger");
        let path = ledger_path(&base);
        let mut ledger = Ledger::open(&path, "neo-20261018", "neo", "-", &base).unwrap();
        ledger.reached("plan", "ok".to_owned(), 7);
        ledger.save(&path).unwrap();
        let again = Ledger::open(&path, "neo-20261018", "neo", "-", &base).unwrap();
        assert_eq!(again, ledger);
        for (label, source, host) in [
            ("neo-20261019", "neo", "-"),
            ("neo-20261018", "yoga", "-"),
            ("neo-20261018", "neo", "neo"),
        ] {
            assert_eq!(
                Ledger::open(&path, label, source, host, &base),
                Err(BulkloadRefusal::ReceiptBindingInvalid)
            );
        }
        fs::remove_dir_all(&base).unwrap();
    }

    fn item(n: u8, workspace: bool) -> Item {
        let path = PathBuf::from(format!("/nonexistent-bulkload-move/{n}"));
        Item {
            source: path.clone(),
            repository: path.clone(),
            workspace: workspace.then_some(path),
        }
    }

    // P-MOVE (plan): every item gets exactly one recorded decision, the
    // ledger holds one per item, and a carry once started stays while the
    // item is uncaptured, whatever its path holds on a rerun; a carry never
    // started is decided again.
    proptest! {
        #![proptest_config(prop_config(64))]
        #[test]
        fn decisions_are_total_and_started_carries_are_sticky(
            shapes in proptest::collection::vec(any::<bool>(), 1..12),
            stale in proptest::collection::vec(0usize..4, 1..12),
        ) {
            let base = base(&format!("prop-{}", shapes.len()));
            let corpus = base.join("corpus");
            fs::create_dir(&corpus).unwrap();
            let items: Vec<Item> = shapes.iter().enumerate().map(|(n, w)| item(u8::try_from(n).unwrap(), *w)).collect();
            // Every item's capture refused for good in the source pass.
            let refusals: BTreeMap<String, String> = items.iter().map(|item| (id(item).unwrap(), "GIT_INVENTORY_INDEX_ABSENT".to_owned())).collect();
            let mut ledger = Ledger::default();
            // An earlier run decided some workspace items beside; choice 0
            // started their carry, choice 1 did not.
            for (n, choice) in stale.iter().enumerate() {
                if let Some(item) = items.get(n).filter(|item| item.workspace.is_some()) {
                    if *choice < 2 {
                        ledger.items.insert(id(item).unwrap(), Decision {
                            action: Action::CarryBeside,
                            reason: "capture-refused-occupied".to_owned(),
                            presence: Presence::Git,
                            capture: Capture::Refused,
                            decided_ns: 1,
                            attempted: *choice == 0,
                        });
                    }
                }
            }
            let before = ledger.clone();
            let inputs = Inputs { corpus: &corpus, refusals: &refusals, state: &base, source: "neo", seats: false };
            let rows = decide(&items, &inputs, &mut ledger, 9).unwrap();
            prop_assert_eq!(rows.len(), items.len());
            prop_assert_eq!(ledger.items.len(), items.iter().map(|item| id(item).unwrap()).collect::<BTreeSet<_>>().len());
            for row in &rows {
                match before.items.get(&row.id) {
                    Some(was) if was.attempted => {
                        prop_assert!(row.kept);
                        prop_assert_eq!(&row.decision, was);
                    }
                    _ => {
                        // Absent paths: workspace items carry live, refs-only
                        // items are refused.
                        prop_assert!(!row.kept);
                        prop_assert_eq!(row.decision.action, if row.item.workspace.is_some() { Action::CarryLive } else { Action::Refuse });
                    }
                }
            }
            let total: u64 = histogram(&rows).values().sum();
            prop_assert_eq!(total, u64::try_from(items.len()).unwrap());
            fs::remove_dir_all(&base).unwrap();
        }
    }
}
