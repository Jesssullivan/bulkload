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
//!   reached. A carry decision is sticky while the item stays uncaptured: a
//!   rerun never moves an item carried at its live path beside, or back.

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
    /// refused to list the repository's worktrees; `code`).
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
    for root in roots {
        let root = fs::canonicalize(root).refuse_at(SITE)?;
        let mut stack = vec![root];
        while let Some(directory) = stack.pop() {
            if git_directory(&directory) {
                found.push(directory);
                continue;
            }
            let mut entries: Vec<PathBuf> = fs::read_dir(&directory)
                .refuse_at(SITE)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<_>>()
                .refuse_at(SITE)?;
            entries.sort();
            let mut plain = 0u64;
            // Pushed in reverse so the walk visits names in order.
            for path in entries.into_iter().rev() {
                let meta = fs::symlink_metadata(&path).refuse_at(SITE)?;
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
    Ok((found, notes))
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
            notes.push(Note {
                path: directory,
                what: "unreadable",
                count: 1,
                code: Some(refusal.code()),
            });
            return;
        }
    };
    let Some(main) = listed.first() else {
        notes.push(Note {
            path: directory,
            what: "unreadable",
            count: 1,
            code: Some(BulkloadRefusal::GitInventoryMalformed.code()),
        });
        return;
    };
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

/// The action for an item, and the table row's name as its reason.
///
/// Its inputs: a workspace item or a refs-only one, whether the corpus
/// holds its capture, what its live path (the workspace, or a refs-only
/// item's repository) holds, and whether `--side-root` seats symlinked
/// paths. Pure.
#[must_use]
pub const fn action(
    workspace: bool,
    captured: bool,
    presence: Presence,
    seats: bool,
) -> (Action, &'static str) {
    match (workspace, captured, presence) {
        (_, _, Presence::Unreadable) => (Action::Refuse, "unreadable"),
        (_, _, Presence::Symlinked) if !(seats && captured) => (Action::Refuse, "symlinked-path"),
        (true, true, Presence::Absent | Presence::Empty) => (Action::Land, "absent"),
        (true, true, Presence::Git | Presence::Symlinked) => (Action::Adopt, "adopt-or-beside"),
        (_, true, Presence::NotGit) => (Action::Refuse, "not-git"),
        (true, false, Presence::Absent | Presence::Empty) => (Action::CarryLive, "capture-refused"),
        (true, false, Presence::Git | Presence::NotGit) => {
            (Action::CarryBeside, "capture-refused-occupied")
        }
        (false, true, Presence::Absent | Presence::Empty) => (Action::Land, "bare-landing"),
        (false, true, Presence::Git | Presence::Symlinked) => (Action::RefsOnly, "refs-only-adopt"),
        (false, false, _) => (Action::Refuse, "capture-refused-refs-only"),
        // Unreachable: a symlinked uncaptured path is refused above.
        (true, false, Presence::Symlinked) => (Action::Refuse, "symlinked-path"),
    }
}

/// One item's recorded decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub action: Action,
    pub reason: String,
    pub presence: Presence,
    pub captured: bool,
    /// When this action was first decided (ns since the epoch).
    pub decided_ns: i128,
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

/// Decide every item of `items` (the pulled plan) against the destination.
///
/// The decisions are recorded in `ledger` (see the module docs). Reads the
/// corpus for capture records and each live path's presence; writes
/// nothing.
///
/// # Errors
/// An item that does not encode; IO probing the corpus.
pub fn decide(
    items: &[Item],
    corpus: &Path,
    ledger: &mut Ledger,
    seats: bool,
    now_ns: i128,
) -> Result<Vec<Row>> {
    let mut rows = Vec::with_capacity(items.len());
    for item in items {
        let identity = id(item)?;
        let captured = corpus
            .join(format!("{identity}.capture"))
            .try_exists()
            .refuse_at("estate::moving::decide")?;
        let place = item.workspace.as_ref().unwrap_or(&item.repository);
        let now = presence(place);
        let (action, reason) = action(item.workspace.is_some(), captured, now, seats);
        let previous = ledger.items.get(&identity);
        // A carry stays where it went while the item is still uncaptured.
        let keep = previous.is_some_and(|previous| previous.action.carries() && !captured);
        let decision = if keep {
            previous
                .cloned()
                .ok_or(BulkloadRefusal::GitAuthorityChanged)?
        } else {
            Decision {
                action,
                reason: reason.to_owned(),
                presence: now,
                captured,
                decided_ns: match previous {
                    Some(previous) if previous.action == action => previous.decided_ns,
                    _ => now_ns,
                },
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
/// IO, including a lock another process holds (refused, never waited on).
pub fn lock(state: &Path) -> Result<impl Drop> {
    super::exclusive(&state.join("move.lock"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::test_support::prop_config;
    use proptest::prelude::*;
    use std::process::Command;

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

    #[test]
    fn the_table_never_lands_an_uncaptured_item_or_writes_through_a_link() {
        for workspace in [true, false] {
            for captured in [true, false] {
                for seen in PRESENCES {
                    for seats in [true, false] {
                        let (action, reason) = action(workspace, captured, seen, seats);
                        assert!(!reason.is_empty());
                        if !captured {
                            assert!(
                                matches!(
                                    action,
                                    Action::CarryLive | Action::CarryBeside | Action::Refuse
                                ),
                                "{workspace} {seen:?} {action:?}"
                            );
                        }
                        if action == Action::CarryLive {
                            assert!(matches!(seen, Presence::Absent | Presence::Empty));
                        }
                        if !workspace {
                            assert!(!action.carries(), "a refs-only item has no files");
                        }
                        if seen == Presence::Symlinked && !(seats && captured) {
                            assert_eq!(action, Action::Refuse);
                        }
                        if seen == Presence::NotGit && captured {
                            assert_eq!(action, Action::Refuse, "never adopt a non-Git tree");
                        }
                        if seen == Presence::Unreadable {
                            assert_eq!(action, Action::Refuse);
                        }
                    }
                }
            }
        }
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
    // ledger holds one per item, and a carry decided once stays while the
    // item is uncaptured, whatever its path holds on a rerun.
    proptest! {
        #![proptest_config(prop_config(64))]
        #[test]
        fn decisions_are_total_and_carries_are_sticky(
            shapes in proptest::collection::vec(any::<bool>(), 1..12),
            stale in proptest::collection::vec(0usize..4, 1..12),
        ) {
            let base = base(&format!("prop-{}", shapes.len()));
            let corpus = base.join("corpus");
            fs::create_dir(&corpus).unwrap();
            let items: Vec<Item> = shapes.iter().enumerate().map(|(n, w)| item(u8::try_from(n).unwrap(), *w)).collect();
            let mut ledger = Ledger::default();
            // A first run carried some uncaptured workspace items beside.
            for (n, choice) in stale.iter().enumerate() {
                if let Some(item) = items.get(n).filter(|item| item.workspace.is_some()) {
                    if *choice == 0 {
                        ledger.items.insert(id(item).unwrap(), Decision {
                            action: Action::CarryBeside,
                            reason: "capture-refused-occupied".to_owned(),
                            presence: Presence::Git,
                            captured: false,
                            decided_ns: 1,
                        });
                    }
                }
            }
            let before = ledger.clone();
            let rows = decide(&items, &corpus, &mut ledger, false, 9).unwrap();
            prop_assert_eq!(rows.len(), items.len());
            prop_assert_eq!(ledger.items.len(), items.iter().map(|item| id(item).unwrap()).collect::<BTreeSet<_>>().len());
            for row in &rows {
                if let Some(was) = before.items.get(&row.id) {
                    prop_assert!(row.kept);
                    prop_assert_eq!(&row.decision, was);
                } else {
                    // Absent paths, nothing captured: workspace items carry
                    // live, refs-only items are refused.
                    prop_assert_eq!(row.decision.action, if row.item.workspace.is_some() { Action::CarryLive } else { Action::Refuse });
                    prop_assert_eq!(row.decision.decided_ns, 9);
                }
            }
            let total: u64 = histogram(&rows).values().sum();
            prop_assert_eq!(total, u64::try_from(items.len()).unwrap());
            fs::remove_dir_all(&base).unwrap();
        }
    }
}
