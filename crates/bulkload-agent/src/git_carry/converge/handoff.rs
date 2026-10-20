//! The hand-off block (Linear TIN-6569, operator ruling OI-1003-Q183,
//! 2026-10-16; design `docs/agent-notes/2026-10-16-estate-handoff-design.md`).
//!
//! `estate-handoff` writes a dated, delimited block at byte 0 of
//! `AGENTS.md` and `CLAUDE.md` (and, opted in, an existing `README.md`) in
//! each landed workspace: absolute paths, the landing's known details and
//! differences, and the instruction to converge and consolidate before any
//! other work. The block is a bulkload-owned overlay on a landed seat:
//!
//! - **recorded** in its own ledger, `handoff.landed` beside
//!   `workspace.landed` (`$GIT_DIR/bulkload/v1/<SOURCE>/`), by length and
//!   blake3, with `handoff.intent` durable before any target is written;
//! - **kept by converge**: a seat whose bytes are a recorded block followed
//!   by the landed blob holds landed content ([`landed_sides`], one
//!   predicate in `holds`); any other content is still
//!   `GIT_DESTINATION_MODIFIED`, so an edit inside the block or below it is
//!   the user's (S5, `NoLocalWorkLost`);
//! - **written crash-safely**: a temporary beside the target, fsynced,
//!   `RENAME_EXCHANGE`d with it and the displaced file checked against the
//!   print the plan read (a no-replace rename for a created file), each
//!   step settled by the next run ([`settle`]);
//! - **always written** into each landed workspace, whatever its remotes
//!   (OI-1003-Q188 removed the privacy guard); no remote URL is ever read
//!   for it, rendered or printed, and commit detection reports a block
//!   committed at the destination (`--check`);
//! - **removed** by a recorded clear that restores the bytes exactly;
//! - **owned by identity**: a block's BEGIN line names the landing it was
//!   rendered for (`for=`); a block that names another landing was carried
//!   here with the source's bytes and is content, kept byte for byte below
//!   this landing's block (OI-1003-Q185: nothing is dropped from a carry).
//!
//! The source is never read (S2).

pub mod render;

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::git;
use super::super::landed::{self, Kind, LandedWorkspace, SeatPrint};
use super::{head_of, lstat, sides, Meta, Side};
use crate::{BulkloadRefusal, Result};

pub use render::{
    rfc3339, Build, CaptureFacts, DestOnly, FamilyRow, Inputs, Landing, Omitted, RefDiff, Sibling,
    SiblingState, MAX_BLOCK, TOKEN,
};

/// The schema of every `estate-handoff` line.
pub const SCHEMA: &str = "bulkload.estate-handoff.v1";
/// The hand-off record's file name, beside `workspace.landed`.
pub const HANDOFF_LANDED: &str = "handoff.landed";
/// The hand-off intent's file name.
pub const HANDOFF_INTENT: &str = "handoff.intent";
/// The record of carried blocks removed from landed content.
///
/// `--clear --clear-carried` writes it (OI-1003-Q190), beside
/// `workspace.landed`. It outlives `handoff.landed`: converge needs it
/// while the landed blob stands.
pub const HANDOFF_STRIPPED: &str = "handoff.stripped";
/// The targets, at the worktree root: Codex reads `AGENTS.md`, Claude
/// `CLAUDE.md`.
pub const TARGETS: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
/// The opt-in target (`--readme`), never created.
pub const README: &str = "README.md";
/// Codex reads `AGENTS.override.md` instead of `AGENTS.md` in a directory
/// that holds one: an existing override carries the block too; it is never
/// created.
pub const OVERRIDE: &str = "AGENTS.override.md";
/// The name every hand-off temporary starts with, beside its target.
const TEMP_PREFIX: &str = ".bulkload-handoff-";
const ALL_NAMES: [&str; 4] = ["AGENTS.md", "CLAUDE.md", OVERRIDE, README];
const LANDED_MAGIC: &[u8; 16] = b"bulkload-hoff-v1";
const INTENT_MAGIC: &[u8; 16] = b"bulkload-hoffi-1";
const STRIPPED_MAGIC: &[u8; 16] = b"bulkload-hofs-v1";

// ---- records -----------------------------------------------------------------

/// What a target holds of the hand-off.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FileKind {
    /// The block on top of the file's own bytes.
    Prefixed,
    /// A file the hand-off created (untracked), the block alone.
    Created,
    /// A symbolic link to the other target in the same worktree: covered by
    /// that target's block, never written.
    ViaSymlink(Vec<u8>),
}

impl FileKind {
    const fn name(&self) -> &'static str {
        match self {
            Self::Prefixed => "prefixed",
            Self::Created => "created",
            Self::ViaSymlink(_) => "via-symlink",
        }
    }
}

/// One target as the record names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffFile {
    /// The target's name at the worktree root.
    pub path: Vec<u8>,
    pub kind: FileKind,
    /// The block's length and blake3 (for `ViaSymlink`, the target's).
    pub block_len: u64,
    pub block: [u8; 32],
    /// The file's print when the hand-off last wrote or proved it.
    pub print: Option<SeatPrint>,
    /// The landed blob the block was written over, when the bytes under it
    /// were exactly that landed seat's at `print`: only then may converge
    /// trust the record's print of the file without reading it (a block
    /// re-rendered over a user's edit below it never vouches for that
    /// edit). Every write of a new `print` re-checks it against the bytes
    /// (review of 2026-10-16: a kept block's re-taken print, a settled
    /// crash's and a re-dated racy one's).
    pub base: Option<String>,
    /// The bytes under the block start with another landing's block (one
    /// carried with the source's bytes).
    pub carried: bool,
}

/// The options a record was rendered under: a re-render needs no flag.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub plan: Vec<u8>,
    pub corpus: Vec<u8>,
    pub state: Vec<u8>,
    pub siblings: Vec<(Vec<u8>, Vec<u8>)>,
    pub readme: bool,
}

/// `handoff.landed`: the blocks bulkload owns in one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffLanded {
    /// `estate::id` of the item it was rendered for.
    pub item: String,
    /// The workspace ledger's capture it was rendered against.
    pub capture: [u8; 32],
    /// The metadata digest of everything the block says (design 3.2).
    pub inputs: [u8; 32],
    /// The block's open rows (design 4.4), a function of `inputs`: an
    /// unchanged rerun reports it without reading anything again.
    pub open: u64,
    pub as_of_ns: i128,
    /// When the prints below were taken (the racy window).
    pub recorded_ns: i128,
    pub fsid: u64,
    pub options: Stored,
    /// Sorted by path.
    pub files: Vec<HandoffFile>,
    /// Targets whose block a person removed by hand: not re-added while
    /// the workspace ledger's capture stays (design 6.5).
    pub retired: Vec<Retired>,
}

/// A target whose block a person removed by hand.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Retired {
    pub path: Vec<u8>,
    /// The workspace ledger's capture when the block was removed.
    pub capture: [u8; 32],
    /// The block it held then: put back byte for byte (an editor's undo, a
    /// `git stash pop`), it is this landing's block again, never carried
    /// content (re-review 2 of 2026-10-16, finding 3).
    pub kind: FileKind,
    pub block_len: u64,
    pub block: [u8; 32],
}

/// What an intent does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Render,
    Clear,
}

/// One target an intent changes: its old and new block (`None`: none), and
/// the whole file's digest before and after (`None`: absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub path: Vec<u8>,
    pub old_block: Option<Vec<u8>>,
    pub new_block: Option<Vec<u8>>,
    pub before: Option<[u8; 32]>,
    pub after: Option<[u8; 32]>,
}

/// `handoff.intent`: durable before the first target is written, removed
/// after the record it produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffIntent {
    pub op: Op,
    /// The temporaries' name tag.
    pub tag: String,
    pub steps: Vec<Step>,
    /// The record a render writes when every step is done.
    pub next: Option<HandoffLanded>,
    /// A clear with `--clear-carried` (OI-1003-Q190): the carried blocks it
    /// removes from landed content, recorded in `handoff.stripped` when
    /// its steps are done (converge accepts them while the intent is
    /// pending), and whether a settle finishing it removes carried blocks.
    pub stripped: Vec<Stripped>,
    pub carried: bool,
}

/// Carried blocks bulkload removed from the head of a target's landed blob.
///
/// `--clear --clear-carried` (OI-1003-Q190): the file holds the landed
/// blob `base` without its first `len` bytes (whose blake3 is `digest`),
/// under this landing's block when one is rendered. Converge accepts that
/// as the landed content while `base` is the seat's landed blob; a
/// converge that lands a new blob makes the entry stale.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stripped {
    pub path: Vec<u8>,
    pub base: String,
    pub len: u64,
    pub digest: [u8; 32],
}

/// `handoff.stripped`: every recorded removal of carried blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffStripped {
    pub files: Vec<Stripped>,
}

impl landed::Record for HandoffLanded {
    const MAGIC: &'static [u8; 16] = LANDED_MAGIC;
}

impl landed::Record for HandoffIntent {
    const MAGIC: &'static [u8; 16] = INTENT_MAGIC;
}

impl landed::Record for HandoffStripped {
    const MAGIC: &'static [u8; 16] = STRIPPED_MAGIC;
}

/// The recorded removals of carried blocks in the ledger directory `dir`.
///
/// # Errors
/// An unreadable `handoff.stripped`.
fn stripped_of(dir: &Path) -> Result<Vec<Stripped>> {
    Ok(
        landed::read::<HandoffStripped>(&dir.join(HANDOFF_STRIPPED))?
            .map(|record| record.files)
            .unwrap_or_default(),
    )
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn len64(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

// ---- the converge overlay ------------------------------------------------------

/// A block a seat may carry above its landed bytes: a recorded block or a
/// pending intent's. `trusted` is the record's print of the file, when it
/// is outside the racy window: `prove` trusts an unchanged print of it as
/// it trusts the ledger's (design 6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Prefix {
    pub(super) len: u64,
    pub(super) digest: [u8; 32],
    pub(super) trusted: Option<SeatPrint>,
    /// The bytes of the landed blob's head the file lacks under the block:
    /// carried blocks `--clear --clear-carried` removed (OI-1003-Q190); 0
    /// for none. A prefix of length 0 with a strip is the removal alone.
    pub(super) strip: u64,
}

/// The old side of every landed seat, as [`sides`] reads it, with each
/// block bulkload owns at the seat's path as an accepted prefix: the
/// record's, and a pending intent's old and new blocks. Only the old side
/// of a converge carries a prefix.
///
/// # Errors
/// Whatever [`sides`] refuses; an unreadable hand-off record or intent.
pub(super) fn landed_sides(
    worktree: &Path,
    git_dir: &Path,
    source: &str,
    meta: &Meta,
) -> Result<BTreeMap<Vec<u8>, Side>> {
    let mut sides = sides(worktree, meta)?;
    let dir = landed::ledger_dir(git_dir, source);
    let record = landed::read::<HandoffLanded>(&dir.join(HANDOFF_LANDED))?;
    let intent = landed::read::<HandoffIntent>(&dir.join(HANDOFF_INTENT))?;
    let mut stripped = stripped_of(&dir)?;
    if record.is_none() && intent.is_none() && stripped.is_empty() {
        return Ok(sides);
    }
    let mut attach = |path: &[u8], mut prefix: Prefix, base: Option<&String>| {
        if let Some(side) = sides
            .get_mut(path)
            .filter(|side| side.kind == Kind::Regular)
        {
            if base.is_none() || base != side.blob.as_ref() {
                prefix.trusted = None;
            }
            if !side.prefix.contains(&prefix) {
                side.prefix.push(prefix);
            }
        }
    };
    if let Some(record) = record {
        let same_fs = landed::fsid(worktree)? == record.fsid;
        let racy_from = record
            .recorded_ns
            .saturating_sub(super::super::RACY_GRANULARITY_NS);
        for file in &record.files {
            if matches!(file.kind, FileKind::ViaSymlink(_)) {
                continue;
            }
            attach(
                &file.path,
                Prefix {
                    len: file.block_len,
                    digest: file.block,
                    trusted: file
                        .print
                        .clone()
                        .filter(|print| {
                            same_fs && print.mtime_ns < racy_from && print.ctime_ns < racy_from
                        }),
                    strip: 0,
                },
                file.base.as_ref(),
            );
        }
        // A retired block put back byte for byte is bulkload's own bytes
        // again (re-review 2 of 2026-10-16, finding 3): accepted, never
        // trusted by print.
        for retired in &record.retired {
            attach(
                &retired.path,
                Prefix {
                    len: retired.block_len,
                    digest: retired.block,
                    trusted: None,
                    strip: 0,
                },
                None,
            );
        }
    }
    for step in intent.iter().flat_map(|intent| &intent.steps) {
        for block in [&step.old_block, &step.new_block].into_iter().flatten() {
            attach(
                &step.path,
                Prefix {
                    len: len64(block),
                    digest: digest(block),
                    trusted: None,
                    strip: 0,
                },
                None,
            );
        }
    }
    // Carried blocks removed from a landed blob (OI-1003-Q190), recorded
    // or a pending clear's: the seat holds that blob without them, alone
    // or under any block accepted above. Only over the blob they were
    // removed from.
    stripped.extend(intent.iter().flat_map(|intent| intent.stripped.clone()));
    for strip in &stripped {
        if let Some(side) = sides.get_mut(&strip.path).filter(|side| {
            side.kind == Kind::Regular && side.blob.as_deref() == Some(strip.base.as_str())
        }) {
            let mut variants: Vec<Prefix> = side
                .prefix
                .iter()
                .filter(|prefix| prefix.strip == 0)
                .map(|prefix| Prefix {
                    strip: strip.len,
                    ..prefix.clone()
                })
                .collect();
            variants.push(Prefix {
                len: 0,
                digest: digest(b""),
                trusted: None,
                strip: strip.len,
            });
            for variant in variants {
                if !side.prefix.contains(&variant) {
                    side.prefix.push(variant);
                }
            }
        }
    }
    Ok(sides)
}

/// The hand-off block at byte 0 of target `name` in `worktree`, if it is
/// a regular file whose head is a well-formed block ([`render::marker`])
/// sealed as estate-handoff rendered it ([`render::sealed`]: an edit inside
/// it, or a block a person wrote, is not one).
fn marked(worktree: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let path = worktree.join(name);
    if !lstat(&path)?.is_some_and(|meta| meta.is_file()) {
        return Ok(None);
    }
    let head = head_of_file(&path)?;
    Ok(match render::marker(&head) {
        render::Marker::Block(len) => head
            .get(..len)
            .filter(|block| render::sealed(block))
            .map(<[u8]>::to_vec),
        render::Marker::None | render::Marker::Malformed => None,
    })
}

/// The adopt proof's one declared difference for a beside checkout
/// bulkload did not land (gap C12, A-115).
///
/// `estate-apply --adopt --beside` allows a bulkload hand-off block at byte
/// 0 of a target (`AGENTS.md`, `CLAUDE.md`), well formed and sealed as
/// estate-handoff rendered it, so its own write never blocks the adoption
/// while an edit inside the block still does. Each regular target seat of
/// `sides` whose file starts with such a block accepts it as a prefix above
/// the captured bytes (never trusted by print: the bytes under it are
/// read). A block that is not sealed, or bytes below it that are not the
/// captured blob, still differ. Read-only.
///
/// # Errors
/// Whatever reading a target refuses.
pub(super) fn declared_blocks(worktree: &Path, sides: &mut BTreeMap<Vec<u8>, Side>) -> Result<()> {
    for name in TARGETS {
        let Some(side) = sides
            .get_mut(name.as_bytes())
            .filter(|side| side.kind == Kind::Regular)
        else {
            continue;
        };
        if let Some(block) = marked(worktree, name)? {
            let prefix = Prefix {
                len: len64(&block),
                digest: digest(&block),
                trusted: None,
                strip: 0,
            };
            if !side.prefix.contains(&prefix) {
                side.prefix.push(prefix);
            }
        }
    }
    Ok(())
}

/// How many targets (`AGENTS.md`, `CLAUDE.md`) of `worktree` start with a
/// sealed hand-off block: the receipt's `beside-handoff-block` after
/// a beside adoption [`declared_blocks`] proved. Read-only.
///
/// # Errors
/// Whatever reading a target refuses.
pub(super) fn marked_targets(worktree: &Path) -> Result<u64> {
    let mut found = 0;
    for name in TARGETS {
        if marked(worktree, name)?.is_some() {
            found += 1;
        }
    }
    Ok(found)
}

/// The prefixes of `prefixes` that `file` starts with (by length and
/// digest), longest first: a prefix of length 0 always matches. Reads at
/// most the longest prefix.
pub(super) fn matching(file: &mut fs::File, prefixes: &[Prefix]) -> Result<Vec<Prefix>> {
    const SITE: &str = "git_carry::converge::handoff::matching";
    let longest = prefixes.iter().map(|prefix| prefix.len).max().unwrap_or(0);
    let mut head = Vec::new();
    std::io::Read::by_ref(file)
        .take(longest)
        .read_to_end(&mut head)
        .refuse_at(SITE)?;
    let mut found: Vec<Prefix> = prefixes
        .iter()
        .filter(|prefix| {
            usize::try_from(prefix.len)
                .ok()
                .and_then(|len| head.get(..len))
                .is_some_and(|bytes| digest(bytes) == prefix.digest)
        })
        .cloned()
        .collect();
    found.sort_by(|a, b| b.len.cmp(&a.len).then(a.strip.cmp(&b.strip)));
    Ok(found)
}

// ---- reading targets -----------------------------------------------------------

/// A target as the plan classifies it, before anything is read.
#[derive(Debug, Clone)]
enum Found {
    Missing,
    Regular(fs::Metadata),
    /// A link to the other target in the same worktree.
    Via(Vec<u8>),
}

/// Whether a symbolic link named `name` to `to` (in the same worktree) is
/// covered by the block of the target it names.
fn covered_link(name: &str, to: &[u8]) -> bool {
    match name {
        "AGENTS.md" => to == b"CLAUDE.md",
        "CLAUDE.md" => to == b"AGENTS.md",
        OVERRIDE => to == b"AGENTS.md" || to == b"CLAUDE.md",
        _ => false,
    }
}

/// The targets of a run: `AGENTS.md` and `CLAUDE.md`; `AGENTS.override.md`
/// when something is at that name or the record names it; `README.md`
/// with `readme`.
fn names_for(
    worktree: &Path,
    readme: bool,
    record: Option<&HandoffLanded>,
) -> Result<Vec<&'static str>> {
    let mut names: Vec<&'static str> = TARGETS.to_vec();
    let recorded = |name: &str| {
        record.is_some_and(|record| record.files.iter().any(|file| file.path == name.as_bytes()))
    };
    if lstat(&worktree.join(OVERRIDE))?.is_some() || recorded(OVERRIDE) {
        names.push(OVERRIDE);
    }
    if readme || recorded(README) {
        names.push(README);
    }
    Ok(names)
}

/// A target bulkload never creates.
fn never_created(name: &str) -> bool {
    name == README || name == OVERRIDE
}

/// Classify every target (design 10): a missing file, a regular one, or a
/// link to the other target; anything else refuses
/// `HANDOFF_TARGET_UNSUPPORTED` for the whole workspace.
fn classify(worktree: &Path, names: &[&'static str]) -> Result<Vec<(&'static str, Found)>> {
    let mut found = Vec::new();
    for name in names {
        let path = worktree.join(name);
        let entry = match lstat(&path)? {
            None => Found::Missing,
            Some(meta) if meta.is_file() => Found::Regular(meta),
            Some(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&path)
                    .refuse_at("git_carry::converge::handoff::classify")?
                    .into_os_string()
                    .into_vec();
                if covered_link(name, &link) {
                    Found::Via(link)
                } else {
                    return Err(BulkloadRefusal::HandoffTargetUnsupported);
                }
            }
            Some(_) => return Err(BulkloadRefusal::HandoffTargetUnsupported),
        };
        found.push((*name, entry));
    }
    // A link's target must be a target that is a file or missing (no chain,
    // no cycle).
    for (_, entry) in &found {
        if let Found::Via(to) = entry {
            let target = found
                .iter()
                .find(|(name, _)| name.as_bytes() == to.as_slice());
            if !matches!(target, Some((_, Found::Regular(_) | Found::Missing))) {
                return Err(BulkloadRefusal::HandoffTargetUnsupported);
            }
        }
    }
    Ok(found)
}

fn open_target(path: &Path) -> Result<fs::File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .refuse_at("git_carry::converge::handoff::open")
}

/// At most [`MAX_BLOCK`] bytes of `path`'s head (counted as a destination
/// read).
fn head_of_file(path: &Path) -> Result<Vec<u8>> {
    let file = open_target(path)?;
    let mut head = Vec::new();
    file.take(u64::try_from(MAX_BLOCK).unwrap_or(u64::MAX))
        .read_to_end(&mut head)
        .refuse_at("git_carry::converge::handoff::head")?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, head.len());
    Ok(head)
}

/// What a regular target holds of the hand-off. A run holds one per
/// target (at most four), so the record entry is kept inline.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
enum Held {
    /// The recorded block, intact (by print, or read).
    Intact(HandoffFile),
    /// No marker at byte 0.
    Plain,
    /// A block at byte 0 that is the file's content, not bulkload's:
    /// rendered for another landing (its `for=` is not this landing's, or
    /// it names none), or one whose whole file is the target's landed blob
    /// (a round trip: the block carried back to the source and landed
    /// again with the source's bytes, while the record stands or after it
    /// is gone). Carried with the source's bytes, so it is content,
    /// handled as `Plain` with the block kept below (OI-1003-Q185;
    /// re-review 2 of 2026-10-16, finding 2).
    Foreign,
    /// A marker at byte 0 for this landing that is not the record's block
    /// and not landed content: an edit inside the block, a BEGIN with no
    /// END, or a block whose record is lost (never presented as carried).
    Modified,
}

/// The record's entry for target `name` (never a link's), or, for a target
/// whose block a person removed by hand, that block as an entry with no
/// print: put back byte for byte, it is this landing's block again
/// (re-review 2 of 2026-10-16, finding 3).
fn entry_of(record: Option<&HandoffLanded>, name: &str) -> Option<HandoffFile> {
    let record = record?;
    record
        .files
        .iter()
        .find(|file| file.path == name.as_bytes() && !matches!(file.kind, FileKind::ViaSymlink(_)))
        .cloned()
        .or_else(|| {
            record
                .retired
                .iter()
                .find(|retired| retired.path == name.as_bytes())
                .map(|retired| HandoffFile {
                    path: retired.path.clone(),
                    kind: retired.kind.clone(),
                    block_len: retired.block_len,
                    block: retired.block,
                    print: None,
                    base: None,
                    carried: false,
                })
        })
}

/// What `bytes` (a target's head, or all of it) hold against its record
/// entry `entry` (see [`entry_of`]). `landed` says whether the target's
/// landed blob itself starts with the block at byte 0 ([`lead_is_landed`]);
/// it is asked only when a block that could be this landing's stands
/// there, and a yes makes that block the file's content (`Foreign`):
/// bulkload's block is never part of what the source landed, so a block
/// the landed blob starts with came back with the source's bytes, whatever
/// a user has written below it since (re-review 2 of 2026-10-16, finding
/// 2; re-review 3, findings 1 and 2; OI-1003-Q185).
fn classify_head(
    bytes: &[u8],
    entry: Option<&HandoffFile>,
    ours: &str,
    landed: &mut dyn FnMut() -> Result<bool>,
) -> Result<Held> {
    let marker = render::marker(bytes);
    if let (render::Marker::Block(len), Some(file)) = (marker, entry) {
        let block = bytes.get(..len).unwrap_or_default();
        if file.block_len == len64(block) && digest(block) == file.block {
            return Ok(if landed()? {
                Held::Foreign
            } else {
                Held::Intact(file.clone())
            });
        }
    }
    unrecorded(bytes, ours, landed)
}

/// What the head of a file holds that its record entry does not vouch
/// for: nothing; a block (or a broken one) of this landing's identity
/// (`ours`), which is this landing's own edited or stale block unless the
/// landed blob itself starts with it (then it came back with the source's
/// bytes: a round trip, with or without a record, and edits below it are
/// the user's; re-reviews of 2026-10-16, finding 4, finding 2 and
/// re-review 3, finding 2); or another landing's block, carried content.
fn unrecorded(head: &[u8], ours: &str, landed: &mut dyn FnMut() -> Result<bool>) -> Result<Held> {
    Ok(match render::marker(head) {
        render::Marker::None => Held::Plain,
        _ if render::owner_of(head) == Some(ours.as_bytes()) => {
            if landed()? {
                Held::Foreign
            } else {
                Held::Modified
            }
        }
        _ => Held::Foreign,
    })
}

/// Whether `record`'s print of `file` vouches for its bytes now: the same
/// file system, the same print, and both its stamps (mtime and ctime; #292
/// review F1) before the racy window of the record's `recorded_ns` (as
/// `prove` trusts the workspace ledger's print).
fn trusted(record: &HandoffLanded, file: &HandoffFile, now: Option<&SeatPrint>, fsid: u64) -> bool {
    let racy_from = record
        .recorded_ns
        .saturating_sub(super::super::RACY_GRANULARITY_NS);
    record.fsid == fsid
        && file
            .print
            .as_ref()
            .is_some_and(|print| {
                now == Some(print) && print.mtime_ns < racy_from && print.ctime_ns < racy_from
            })
}

/// The bytes of the block at byte 0 of `bytes`: a well-formed block's, or,
/// for a broken one (a BEGIN line with no END line), as far as
/// [`render::marker`] looks; none without a marker.
fn lead(bytes: &[u8]) -> &[u8] {
    let len = match render::marker(bytes) {
        render::Marker::None => 0,
        render::Marker::Block(len) => len,
        render::Marker::Malformed => MAX_BLOCK,
    };
    bytes.get(..len.min(bytes.len())).unwrap_or_default()
}

/// Whether the block at byte 0 of `bytes` (target `name` read whole) is
/// landed content: the target's landed blob itself starts with it (or
/// does once carried blocks a recorded `--clear-carried` removed are
/// skipped). Bulkload's own block is never part of what the source
/// landed, so such a block came back with the source's bytes (a round
/// trip), and an edit below it never makes it bulkload's (re-review 3 of
/// 2026-10-16, findings 1 and 2; OI-1003-Q185). A block of this landing's
/// that the blob does not start with (a lost record, H49) is not.
fn lead_is_landed(
    worktree: &Path,
    landed: &mut Landed<'_>,
    name: &str,
    bytes: &[u8],
) -> Result<bool> {
    let head = lead(bytes);
    if head.is_empty() {
        return Ok(false);
    }
    Ok(landed_compare(worktree, landed, name, head, true, false)?.is_some())
}

/// [`lead_is_landed`] of target `name` (read whole, counted).
fn lead_is_landed_at(worktree: &Path, landed: &mut Landed<'_>, name: &str) -> Result<bool> {
    let read = read_whole(&worktree.join(name), name)?;
    lead_is_landed(worktree, landed, name, &read.bytes)
}

/// What target `name` of `worktree` holds against `record`: by print when
/// the record's print is unchanged and not racy (no read), else by its
/// head (and, for a block that could be this landing's, by whether the
/// landed blob starts with it). `ours` is this landing's identity
/// ([`render::owner`]).
fn held(
    worktree: &Path,
    name: &str,
    meta: &fs::Metadata,
    record: Option<&HandoffLanded>,
    fsid: u64,
    ours: &str,
    landed: &mut Landed<'_>,
) -> Result<Held> {
    let entry = entry_of(record, name);
    let now = SeatPrint::of(name.as_bytes(), meta);
    if let (Some(record), Some(file)) = (record, &entry) {
        if trusted(record, file, now.as_ref(), fsid) {
            return Ok(Held::Intact(file.clone()));
        }
    }
    let head = head_of_file(&worktree.join(name))?;
    classify_head(&head, entry.as_ref(), ours, &mut || {
        lead_is_landed_at(worktree, landed, name)
    })
}

// ---- writing targets -----------------------------------------------------------

fn cname(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| BulkloadRefusal::PathNotPortable)
}

/// The tag of one run's temporaries.
fn tag() -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-handoff-tag\0");
    hasher.update(&super::super::pass_start_ns().to_le_bytes());
    hasher.update(&std::process::id().to_le_bytes());
    hasher
        .finalize()
        .to_hex()
        .get(..16)
        .unwrap_or_default()
        .to_owned()
}

fn temp_name(tag: &str, n: usize) -> String {
    format!("{TEMP_PREFIX}{tag}-{n}")
}

/// A target read whole for a step, through the descriptor it is checked by.
struct Whole {
    held: fs::File,
    bytes: Vec<u8>,
    print: SeatPrint,
}

fn read_whole(path: &Path, name: &str) -> Result<Whole> {
    const SITE: &str = "git_carry::converge::handoff::read_whole";
    let mut held = open_target(path)?;
    let meta = held.metadata().refuse_at(SITE)?;
    let print =
        SeatPrint::of(name.as_bytes(), &meta).ok_or(BulkloadRefusal::HandoffTargetUnsupported)?;
    if print.kind != Kind::Regular {
        return Err(BulkloadRefusal::HandoffTargetUnsupported);
    }
    let mut bytes = Vec::new();
    held.read_to_end(&mut bytes).refuse_at(SITE)?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, bytes.len());
    Ok(Whole { held, bytes, print })
}

/// `path` read whole, still starting with `file`'s block (what the
/// classification found), held still to the read the exchange is checked
/// against. Anything else is a writer between the two reads:
/// `HANDOFF_BLOCK_MODIFIED`.
fn read_block(path: &Path, name: &str, file: &HandoffFile) -> Result<Whole> {
    let read = read_whole(path, name)?;
    let still = usize::try_from(file.block_len)
        .ok()
        .and_then(|len| read.bytes.get(..len))
        .is_some_and(|block| digest(block) == file.block);
    if still {
        Ok(read)
    } else {
        Err(BulkloadRefusal::HandoffBlockModified)
    }
}

/// Target `name` read whole, still holding no block of this landing's
/// (nothing at byte 0, or a carried block: [`classify_head`] over the
/// whole file, against `entry`). Anything else is a writer between the
/// classification and this read: `HANDOFF_BLOCK_MODIFIED`.
fn read_unowned(
    worktree: &Path,
    name: &str,
    entry: Option<&HandoffFile>,
    ours: &str,
    landed: &mut Landed<'_>,
) -> Result<Whole> {
    let read = read_whole(&worktree.join(name), name)?;
    let bytes = &read.bytes;
    let state = classify_head(bytes, entry, ours, &mut || {
        lead_is_landed(worktree, landed, name, bytes)
    })?;
    if matches!(state, Held::Plain | Held::Foreign) {
        Ok(read)
    } else {
        Err(BulkloadRefusal::HandoffBlockModified)
    }
}

fn same_file(a: &SeatPrint, meta: &fs::Metadata) -> bool {
    let b = crate::freshness::StatIdentity::from_metadata(meta);
    a.ino == b.ino && a.size == b.size && a.mtime_ns == b.mtime_ns
}

/// Write `bytes` at a temporary beside the target, fsynced at `mode`.
fn write_temp(temp: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    const SITE: &str = "git_carry::converge::handoff::write_temp";
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(temp)
        .refuse_at(SITE)?;
    file.write_all(bytes).refuse_at(SITE)?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .refuse_at(SITE)?;
    crate::counters::CountedSync::sync_file_counted(&file).refuse_at(SITE)?;
    Ok(())
}

/// Replace the target `name` (read as `read`) by `bytes`: temporary,
/// fsync, `RENAME_EXCHANGE`, the displaced file checked against the print
/// the plan read and unlinked; any other displaced file goes back and the
/// step refuses `HANDOFF_BLOCK_MODIFIED` (design 8).
fn exchange(
    worktree: &Path,
    name: &str,
    read: &Whole,
    bytes: &[u8],
    temp: &str,
    op: Op,
) -> Result<()> {
    const SITE: &str = "git_carry::converge::handoff::exchange";
    let temp_path = worktree.join(temp);
    write_temp(&temp_path, bytes, read.print.mode & 0o7777)?;
    fault_point!(EstateHandoffAfterTemp);
    let directory = fs::File::open(worktree).refuse_at(SITE)?;
    let (temp_c, leaf) = (cname(OsStr::new(temp))?, cname(OsStr::new(name))?);
    if let Err(error) = crate::io::exchange(&directory, &temp_c, &leaf) {
        let _ = fs::remove_file(&temp_path);
        return Err(if crate::io::rename_unsupported(&error) {
            BulkloadRefusal::DestinationExchangeUnsupported
        } else {
            crate::refuse::io(&error, SITE)
        });
    }
    #[allow(clippy::match_same_arms)]
    match op {
        Op::Render => fault_point!(EstateHandoffAfterExchange),
        Op::Clear => fault_point!(EstateHandoffClearAfterExchange),
    }
    let unchanged = read
        .held
        .metadata()
        .is_ok_and(|meta| same_file(&read.print, &meta));
    let displaced = lstat(&temp_path)?.is_some_and(|meta| same_file(&read.print, &meta));
    if unchanged && displaced {
        fs::remove_file(&temp_path).refuse_at(SITE)?;
        return landed::sync_dir(worktree);
    }
    crate::io::exchange(&directory, &temp_c, &leaf).refuse_at(SITE)?;
    landed::sync_dir(worktree)?;
    let _ = fs::remove_file(&temp_path);
    Err(BulkloadRefusal::HandoffBlockModified)
}

/// Create the target `name` holding `bytes` (mode 0644), never replacing
/// a file someone created since the plan.
fn create(worktree: &Path, name: &str, bytes: &[u8], temp: &str) -> Result<()> {
    const SITE: &str = "git_carry::converge::handoff::create";
    let temp_path = worktree.join(temp);
    write_temp(&temp_path, bytes, 0o644)?;
    fault_point!(EstateHandoffAfterTemp);
    let directory = fs::File::open(worktree).refuse_at(SITE)?;
    let (temp_c, leaf) = (cname(OsStr::new(temp))?, cname(OsStr::new(name))?);
    if let Err(error) = crate::io::sys::rename_exclusive_at(&directory, &temp_c, &directory, &leaf)
    {
        let _ = fs::remove_file(&temp_path);
        return Err(if error.raw_os_error() == Some(libc::EEXIST) {
            BulkloadRefusal::HandoffBlockModified
        } else {
            crate::refuse::io(&error, SITE)
        });
    }
    fault_point!(EstateHandoffAfterExchange);
    landed::sync_dir(worktree)
}

/// Remove the created target `name` (read as `read`, exactly a block):
/// renamed to a temporary, checked, unlinked; anything else goes back.
fn remove(worktree: &Path, name: &str, read: &Whole, temp: &str) -> Result<()> {
    const SITE: &str = "git_carry::converge::handoff::remove";
    let temp_path = worktree.join(temp);
    let directory = fs::File::open(worktree).refuse_at(SITE)?;
    let (temp_c, leaf) = (cname(OsStr::new(temp))?, cname(OsStr::new(name))?);
    crate::io::sys::rename_exclusive_at(&directory, &leaf, &directory, &temp_c).refuse_at(SITE)?;
    fault_point!(EstateHandoffClearAfterExchange);
    let unchanged = read
        .held
        .metadata()
        .is_ok_and(|meta| same_file(&read.print, &meta));
    let moved = lstat(&temp_path)?.is_some_and(|meta| same_file(&read.print, &meta));
    if unchanged && moved {
        fs::remove_file(&temp_path).refuse_at(SITE)?;
        return landed::sync_dir(worktree);
    }
    let _ = crate::io::sys::rename_exclusive_at(&directory, &temp_c, &directory, &leaf);
    landed::sync_dir(worktree)?;
    Err(BulkloadRefusal::HandoffBlockModified)
}

// ---- settling a crashed run ----------------------------------------------------

/// Whether the file at `path` starts with `block` (len and digest).
fn starts_with(path: &Path, len: u64, block: &[u8; 32]) -> Result<bool> {
    let Some(meta) = lstat(path)? else {
        return Ok(false);
    };
    if !meta.is_file() || meta.len() < len {
        return Ok(false);
    }
    let file = open_target(path)?;
    let mut head = Vec::new();
    file.take(len)
        .read_to_end(&mut head)
        .refuse_at("git_carry::converge::handoff::starts_with")?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, head.len());
    Ok(len64(&head) == len && digest(&head) == *block)
}

/// Whether the file at `path` still starts with the carried blocks `strip`
/// names, under `old` (the block of this landing's the same clear step
/// removes with them) or alone: the step that removes them has not run, or
/// was undone. A missing or non-regular file holds none.
fn still_carries(path: &Path, old: Option<&[u8]>, strip: &Stripped) -> Result<bool> {
    let Some(meta) = lstat(path)? else {
        return Ok(false);
    };
    if !meta.is_file() {
        return Ok(false);
    }
    let old = old.unwrap_or_default();
    let len = usize::try_from(strip.len).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    let mut head = Vec::new();
    open_target(path)?
        .take(len64(old).saturating_add(strip.len))
        .read_to_end(&mut head)
        .refuse_at("git_carry::converge::handoff::still_carries")?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, head.len());
    let carries = |at: usize| {
        at.checked_add(len)
            .and_then(|end| head.get(at..end))
            .is_some_and(|bytes| digest(bytes) == strip.digest)
    };
    Ok(carries(0) || (!old.is_empty() && head.starts_with(old) && carries(old.len())))
}

fn whole_digest(path: &Path) -> Result<Option<[u8; 32]>> {
    let Some(meta) = lstat(path)? else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    open_target(path)?
        .read_to_end(&mut bytes)
        .refuse_at("git_carry::converge::handoff::whole_digest")?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, bytes.len());
    Ok(Some(digest(&bytes)))
}

/// What a settle did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Settled {
    /// Temporaries removed or put back, and intents settled.
    swept: u64,
    /// A pending clear was finished (its intent rolled forward).
    cleared: bool,
}

/// Remove what a crashed hand-off left at its temporaries' names in
/// `worktree`: ours removed; a third party's file a crash displaced put
/// back when the target holds this run's file. Returns how many.
fn sweep(
    worktree: &Path,
    intent: Option<&HandoffIntent>,
    record: Option<&HandoffLanded>,
) -> Result<u64> {
    const SITE: &str = "git_carry::converge::handoff::sweep";
    let mut swept = 0;
    let mut temps: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(worktree).refuse_at(SITE)? {
        let entry = entry.refuse_at(SITE)?;
        if entry
            .file_name()
            .as_bytes()
            .starts_with(TEMP_PREFIX.as_bytes())
            && entry.file_type().refuse_at(SITE)?.is_file()
        {
            temps.push(entry.path());
        }
    }
    temps.sort();
    for temp in temps {
        let Some(found) = whole_digest(&temp)? else {
            continue;
        };
        let name = temp
            .file_name()
            .map(OsStr::as_bytes)
            .unwrap_or_default()
            .to_vec();
        let step = intent.and_then(|intent| {
            let index = std::str::from_utf8(&name)
                .ok()?
                .strip_prefix(&format!("{TEMP_PREFIX}{}-", intent.tag))?
                .parse::<usize>()
                .ok()?;
            intent.steps.get(index)
        });
        let blocks = record
            .iter()
            .flat_map(|record| &record.files)
            .map(|file| file.block)
            .chain(intent.iter().flat_map(|intent| {
                intent
                    .steps
                    .iter()
                    .flat_map(|step| [&step.old_block, &step.new_block])
                    .flatten()
                    .map(|block| digest(block))
            }));
        let ours = step.is_some_and(|step| [step.before, step.after].contains(&Some(found)))
            || intent
                .iter()
                .flat_map(|intent| &intent.steps)
                .any(|step| [step.before, step.after].contains(&Some(found)))
            || blocks.into_iter().any(|block| block == found);
        if ours {
            fs::remove_file(&temp).refuse_at(SITE)?;
            landed::sync_dir(worktree)?;
            swept += 1;
            continue;
        }
        // A third party's file a crash displaced: back to its target when
        // the target holds this run's file.
        if let Some(step) = step {
            let target = worktree.join(OsStr::from_bytes(&step.path));
            if whole_digest(&target)? == step.after {
                let directory = fs::File::open(worktree).refuse_at(SITE)?;
                let (temp_c, leaf) = (
                    cname(temp.file_name().unwrap_or_default())?,
                    cname(OsStr::from_bytes(&step.path))?,
                );
                if step.after.is_some() {
                    crate::io::exchange(&directory, &temp_c, &leaf).refuse_at(SITE)?;
                    fs::remove_file(&temp).refuse_at(SITE)?;
                } else {
                    crate::io::sys::rename_exclusive_at(&directory, &temp_c, &directory, &leaf)
                        .refuse_at(SITE)?;
                }
                landed::sync_dir(worktree)?;
                swept += 1;
            }
        }
    }
    Ok(swept)
}

/// The landed sides of the workspace whose ledger directory is `dir`, for
/// [`landed_base`]: `None` without a workspace ledger.
fn landed_of(dir: &Path) -> Result<Option<String>> {
    Ok(
        landed::read::<LandedWorkspace>(&dir.join(landed::WORKSPACE_LANDED))?
            .map(|ledger| ledger.instance),
    )
}

/// `file`, a recorded entry whose block stands at its target's byte 0, with
/// its print re-taken now and everything the record says of the bytes under
/// the block re-checked at that print: `base` (the landed blob, when the
/// bytes under the block are exactly it) and `carried`. A record's print
/// is never re-taken without this (review of 2026-10-16: a re-taken print
/// with an old `base` vouched for a user's edit below the block, and
/// converge landed over it). `None`: the target no longer starts with the
/// block, or moved while it was read.
fn retaken(
    worktree: &Path,
    landed: &mut Landed<'_>,
    file: &HandoffFile,
) -> Result<Option<HandoffFile>> {
    let name = std::str::from_utf8(&file.path).map_err(|_| BulkloadRefusal::PathNotPortable)?;
    let read = match read_block(&worktree.join(name), name, file) {
        Ok(read) => read,
        Err(BulkloadRefusal::HandoffBlockModified) => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    if !read
        .held
        .metadata()
        .is_ok_and(|meta| same_file(&read.print, &meta))
    {
        return Ok(None);
    }
    let cut = usize::try_from(file.block_len).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    let rest = read.bytes.get(cut..).unwrap_or_default();
    let mut file = file.clone();
    file.base = landed_base(worktree, landed, name, rest)?;
    file.carried = render::marker(rest) != render::Marker::None;
    file.print = Some(read.print);
    Ok(Some(file))
}

/// Settle what a crashed hand-off left in `worktree` (design 8), under the
/// destination lock: its temporaries ([`sweep`]); then a pending clear is
/// finished (rolled forward: every recorded block still intact stripped,
/// the record and the intent removed), and a pending render leaves the
/// record naming exactly the blocks the targets hold, each re-checked at
/// its print ([`retaken`]), and the intent removed. `ours` is this
/// landing's identity.
///
/// # Errors
/// An unreadable record or intent; a file-system error; a clear the
/// settle finishes refuses as [`clear`] does (its intent kept).
fn settle(worktree: &Path, dir: &Path, source: &str, ours: &str) -> Result<Settled> {
    // The settled record's racy instant, before this settle's first read
    // or write (C03): each print it records is re-taken below, after it.
    let started_ns = super::super::pass_start_ns();
    let intent_path = dir.join(HANDOFF_INTENT);
    let record_path = dir.join(HANDOFF_LANDED);
    let intent = landed::read::<HandoffIntent>(&intent_path)?;
    let record = landed::read::<HandoffLanded>(&record_path)?;
    let swept = sweep(worktree, intent.as_ref(), record.as_ref())?;
    let Some(intent) = intent else {
        return Ok(Settled {
            swept,
            cleared: false,
        });
    };
    if intent.op == Op::Clear {
        // The operator asked for the block to go: finish that, never put
        // it back (review of 2026-10-16, a crashed clear re-rendered by
        // the next apply's H step).
        clear(worktree, dir, source, ours, intent.carried)?;
        return Ok(Settled {
            swept: swept + 1,
            cleared: true,
        });
    }
    let instance = landed_of(dir)?;
    let mut landed = Landed {
        source,
        instance: instance.as_deref().unwrap_or_default(),
        sides: None,
        stripped: stripped_of(dir)?,
    };
    // The record names exactly the blocks the targets hold: the intent's
    // new one where it is in place, else the record's own.
    let base = intent.next.clone().or_else(|| record.clone());
    if let Some(mut base) = base {
        let mut files = Vec::new();
        let mut names: Vec<Vec<u8>> = intent
            .next
            .iter()
            .chain(record.iter())
            .flat_map(|record| record.files.iter().map(|file| file.path.clone()))
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            let candidates = intent
                .next
                .iter()
                .chain(record.iter())
                .filter_map(|record| record.files.iter().find(|file| file.path == name));
            for file in candidates {
                let target = worktree.join(OsStr::from_bytes(&file.path));
                let present = match &file.kind {
                    FileKind::ViaSymlink(to) => fs::read_link(&target)
                        .is_ok_and(|link| link.as_os_str().as_bytes() == to.as_slice()),
                    _ => starts_with(&target, file.block_len, &file.block)?,
                };
                if !present {
                    continue;
                }
                let file = match &file.kind {
                    FileKind::ViaSymlink(_) => Some(file.clone()),
                    _ if instance.is_some() => retaken(worktree, &mut landed, file)?,
                    _ => Some(HandoffFile {
                        base: None,
                        print: None,
                        ..file.clone()
                    }),
                };
                if let Some(file) = file {
                    files.push(file);
                    break;
                }
            }
        }
        let complete = intent.next.as_ref().is_some_and(|next| {
            next.files.len() == files.len()
                && next
                    .files
                    .iter()
                    .zip(&files)
                    .all(|(a, b)| a.path == b.path && a.block == b.block)
        });
        if !complete {
            // Not the record the run meant: its next run renders again.
            base.inputs = [0; 32];
        }
        base.files = files;
        base.recorded_ns = started_ns;
        if base.files.is_empty() && base.retired.is_empty() {
            landed::remove(&record_path)?;
        } else {
            landed::write(&record_path, &base)?;
        }
    }
    landed::remove(&intent_path)?;
    Ok(Settled {
        swept: swept + 1,
        cleared: false,
    })
}

// ---- lifting a created file before converge adds the path -----------------------

/// Before converge adds a path where a hand-off created file stands (design
/// 6.3): a file that is exactly its recorded block is bulkload's, renamed
/// to a temporary, checked and unlinked, and dropped from the record; any
/// other file is left to converge's clash check (`GIT_DESTINATION_MODIFIED`:
/// an untracked file the user changed is the user's). Under the
/// destination lock. Returns how many were lifted.
///
/// # Errors
/// An unreadable record; a file-system error.
pub(super) fn lift(worktree: &Path, git_dir: &Path, source: &str, adds: &[&[u8]]) -> Result<u64> {
    const SITE: &str = "git_carry::converge::handoff::lift";
    let path = landed::ledger_dir(git_dir, source).join(HANDOFF_LANDED);
    let Some(mut record) = landed::read::<HandoffLanded>(&path)? else {
        return Ok(0);
    };
    let tag = tag();
    let mut lifted = 0;
    for (n, add) in adds.iter().enumerate() {
        let Some(at) = record
            .files
            .iter()
            .position(|file| file.path == *add && file.kind == FileKind::Created)
        else {
            continue;
        };
        let file = record
            .files
            .get(at)
            .cloned()
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let name = std::str::from_utf8(add).map_err(|_| BulkloadRefusal::PathNotPortable)?;
        if !TARGETS.contains(&name) {
            continue;
        }
        let target = worktree.join(name);
        let Some(meta) = lstat(&target)? else {
            continue;
        };
        if !meta.is_file() || meta.len() != file.block_len {
            continue;
        }
        let read = read_whole(&target, name)?;
        if digest(&read.bytes) != file.block {
            continue;
        }
        let temp = format!("{TEMP_PREFIX}{tag}-lift-{n}");
        let temp_path = worktree.join(&temp);
        let directory = fs::File::open(worktree).refuse_at(SITE)?;
        let (temp_c, leaf) = (cname(OsStr::new(&temp))?, cname(OsStr::new(name))?);
        crate::io::sys::rename_exclusive_at(&directory, &leaf, &directory, &temp_c)
            .refuse_at(SITE)?;
        let unchanged = read
            .held
            .metadata()
            .is_ok_and(|meta| same_file(&read.print, &meta));
        if !(unchanged && lstat(&temp_path)?.is_some_and(|meta| same_file(&read.print, &meta))) {
            let _ = crate::io::sys::rename_exclusive_at(&directory, &temp_c, &directory, &leaf);
            landed::sync_dir(worktree)?;
            continue;
        }
        fs::remove_file(&temp_path).refuse_at(SITE)?;
        landed::sync_dir(worktree)?;
        fault_point!(EstateHandoffLiftAfterUnlink);
        record.files.remove(at);
        lifted += 1;
    }
    if lifted > 0 {
        if record.files.is_empty() && record.retired.is_empty() {
            landed::remove(&path)?;
        } else {
            landed::write(&path, &record)?;
        }
    }
    Ok(lifted)
}

// ---- the census: what a block says, read as metadata -----------------------------

/// One planned item, as the estate side knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub id: String,
    /// The item's workspace, canonical when it exists.
    pub workspace: Option<PathBuf>,
    /// The corpus's current capture of the item.
    pub current: Option<[u8; 32]>,
    pub omitted: Option<Vec<Omitted>>,
    /// The code of the item's last outcome, when it is a refusal.
    pub refused: Option<String>,
    /// The digest of the item's capture and outcome records' bytes.
    pub records: [u8; 32],
}

/// What the estate side knows of the run.
#[derive(Debug, Clone)]
pub struct Estate<'a> {
    pub plan: &'a Path,
    pub plan_digest: [u8; 32],
    pub corpus: &'a Path,
    pub state: &'a Path,
    pub source: &'a str,
    pub items: &'a [Planned],
}

/// One workspace to hand off.
#[derive(Debug, Clone, Copy)]
pub struct Workspace<'a> {
    pub item: &'a str,
    pub worktree: &'a Path,
    /// The item's source path on the origin host (from the plan).
    pub source_path: &'a Path,
}

/// What one run does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Render,
    DryRun,
    Clear,
    Check,
}

/// The run's options. Each flag is an independent switch of the verb.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct Request<'a> {
    pub mode: Mode,
    pub readme: bool,
    pub renew: bool,
    /// `--clear --clear-carried`: also remove carried blocks, as a recorded
    /// edit (OI-1003-Q190).
    pub clear_carried: bool,
    /// `(landed prefix, live prefix)` pairs (`--sibling`).
    pub siblings: &'a [(PathBuf, PathBuf)],
    pub as_of: Option<i128>,
    /// `estate-apply`'s H step: re-render an existing record only (never
    /// start one), after settling what a crash left.
    pub h_step: bool,
}

/// One target in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: Vec<u8>,
    pub kind: &'static str,
    pub action: &'static str,
}

/// What one workspace's run did, for its line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub outcome: &'static str,
    pub state: Option<&'static str>,
    pub files: Vec<FileRow>,
    pub inputs: Option<[u8; 32]>,
    pub open: Option<u64>,
    pub block: Option<Vec<u8>>,
    /// `--check`: `HANDOFF_MARKER_COMMITTED` when the token is staged or
    /// committed.
    pub refusal: Option<BulkloadRefusal>,
    /// `--check`: whether the token is in the source's history (a commit
    /// the carried capture reaches): information, never a refusal
    /// (OI-1003-Q191).
    pub source_history: Option<bool>,
}

/// One worktree of the family, read as metadata.
#[derive(Debug, Clone)]
struct Member {
    path: PathBuf,
    main: bool,
    item: Option<String>,
    ledger: Option<(LandedWorkspace, [u8; 32])>,
    now: Option<(Option<String>, String)>,
    changed: u64,
    /// A destination-only worktree (no plan item names it): whether its
    /// working tree has uncommitted changes (OI-1003-Q189).
    dirty: bool,
}

/// The family of the repository a workspace belongs to.
#[derive(Debug, Clone)]
struct Census {
    members: Vec<Member>,
    refs: Vec<u8>,
}

/// Facts a run reads once per repository family, shared by its workspaces.
#[derive(Debug, Default)]
pub struct Session {
    census: BTreeMap<PathBuf, Census>,
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_target(path: &[u8]) -> bool {
    ALL_NAMES.iter().any(|name| name.as_bytes() == path)
}

fn now_of(worktree: &Path) -> Result<Option<(Option<String>, String)>> {
    let head = head_of(worktree)?;
    Ok((!head.oid.is_empty()).then_some((head.symbolic, head.oid)))
}

fn census(common: &Path, worktree: &Path, source: &str, items: &[Planned]) -> Result<Census> {
    let mut members = Vec::new();
    for (index, (path, _)) in super::worktrees(worktree)?.into_iter().enumerate() {
        let path = canonical(&path);
        let item = items
            .iter()
            .find(|item| item.workspace.as_deref() == Some(path.as_path()))
            .map(|item| item.id.clone());
        let git_dir = landed::worktree_git_dir(&path)?.filter(|dir| dir.is_dir());
        let ledger = match &git_dir {
            Some(git_dir) => landed::read::<LandedWorkspace>(
                &landed::ledger_dir(git_dir, source).join(landed::WORKSPACE_LANDED),
            )?,
            None => None,
        };
        let mut changed = 0;
        if let Some(ledger) = &ledger {
            for seat in ledger.seats.iter().filter(|seat| !is_target(&seat.path)) {
                let now =
                    super::super::safe_destination(&path, Path::new(OsStr::from_bytes(&seat.path)))
                        .ok()
                        .and_then(|at| fs::symlink_metadata(at).ok())
                        .and_then(|meta| SeatPrint::of(&seat.path, &meta));
                if !now.is_some_and(|now| now.same(seat)) {
                    changed += 1;
                }
            }
        }
        let ledger = match ledger {
            Some(ledger) => {
                let bytes = landed::encode(&ledger)?;
                Some((ledger, digest(&bytes)))
            }
            None => None,
        };
        let now = if git_dir.is_some() {
            now_of(&path)?
        } else {
            None
        };
        let dirty = item.is_none() && git_dir.is_some() && dirty(&path);
        members.push(Member {
            path,
            main: index == 0,
            item,
            ledger,
            now,
            changed,
            dirty,
        });
    }
    let refs = super::super::output(
        git(common).args(["for-each-ref", "--format=%(objectname) %(refname)"]),
    )?;
    Ok(Census { members, refs })
}

/// Whether the checkout at `path` has uncommitted changes, as a read-only
/// `git status` lists them (untracked files included, ignored ones not;
/// `--no-optional-locks`, so the index is never refreshed on disk), with
/// every filter driver the checkout configures switched off: the read runs
/// no program of the repository's and a required driver cannot fail it
/// (re-review 3 of 2026-10-16, finding 5). A status that cannot be read
/// counts as dirty: `ready` never vouches for a tree it could not read
/// (OI-1003-Q189).
fn dirty(path: &Path) -> bool {
    let Ok(mut status) = super::super::unfiltered_status(path) else {
        return true;
    };
    !status
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ])
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout.is_empty())
}

/// Whether `oid` is a commit some branch of `repo` holds (`except` left
/// out). A commit `repo` does not have is held by none. Read-only.
fn held_by_branch(repo: &Path, oid: &str, except: Option<&str>) -> bool {
    let Ok(listed) = super::super::output(git(repo).args([
        "for-each-ref",
        "--format=%(refname)",
        "--contains",
        oid,
        "refs/heads/",
    ])) else {
        return false;
    };
    listed
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .any(|line| except.is_none_or(|except| line != except.as_bytes()))
}

/// Whether `ancestor` is in `tip`'s history in `repo` (read-only; a commit
/// `repo` does not have is in no history).
fn in_history(repo: &Path, ancestor: &str, tip: &str) -> bool {
    git(repo)
        .args(["merge-base", "--is-ancestor", ancestor, tip])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The live sibling of `worktree` (the landing), if `siblings` names one:
/// what is at its path and, for a checkout, whether its HEAD and the
/// landing's HEAD are reconciled. Every read of the sibling is read-only
/// (`--no-optional-locks`, no hooks).
fn sibling_of(worktree: &Path, siblings: &[(PathBuf, PathBuf)]) -> Result<Option<Sibling>> {
    for (landed, live) in siblings {
        let Ok(rest) = worktree.strip_prefix(landed) else {
            continue;
        };
        let path = live.join(rest);
        let state = match lstat(&path)? {
            None => SiblingState::Absent,
            Some(_) => {
                if landed::worktree_git_dir(&path)?.is_some_and(|dir| dir.is_dir()) {
                    match now_of(&path) {
                        Ok(Some((branch, oid))) => SiblingState::Head { branch, oid },
                        _ => SiblingState::NotACheckout,
                    }
                } else {
                    SiblingState::NotACheckout
                }
            }
        };
        // Reconciled when the landing holds the sibling's HEAD: on a branch,
        // or in its own HEAD's history (a detached landing). A sibling
        // strictly ahead holds a commit the landing lacks: open (re-review
        // of 2026-10-16, finding 2).
        let consolidated = match &state {
            SiblingState::Head { oid, .. } => {
                held_by_branch(worktree, oid, None)
                    || now_of(worktree)
                        .ok()
                        .flatten()
                        .is_some_and(|(_, here)| in_history(worktree, oid, &here))
            }
            _ => false,
        };
        let dirty = matches!(state, SiblingState::Head { .. }) && dirty(&path);
        return Ok(Some(Sibling {
            path: path.into_os_string().into_vec(),
            state,
            consolidated,
            dirty,
        }));
    }
    Ok(None)
}

/// The build that renders, as the block names it.
fn build() -> Build {
    Build {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        rev: option_env!("BULKLOAD_BUILD_REV")
            .unwrap_or("unknown")
            .to_owned(),
        exe: std::env::current_exe().map_or_else(
            |_| b"bulkload-agent".to_vec(),
            |exe| exe.into_os_string().into_vec(),
        ),
    }
}

/// This host's name (`uname`'s node name).
fn host() -> String {
    let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: `name` is a writable utsname that uname fills on success.
    if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
        return "unknown".to_owned();
    }
    // SAFETY: uname returned 0, so it initialised `name`.
    let name = unsafe { name.assume_init() };
    // SAFETY: `nodename` is NUL-terminated within its array on success.
    unsafe { std::ffi::CStr::from_ptr(name.nodename.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// The options a run renders or checks under: what the request gives,
/// and for what it leaves out, what the record was rendered with (review
/// of 2026-10-16: a plain rerun, the H step and the `--check` a block
/// prints all keep a render's `--sibling` and `--readme`). To drop one,
/// clear and render again.
#[derive(Debug, Clone)]
struct Effective {
    siblings: Vec<(PathBuf, PathBuf)>,
    readme: bool,
}

fn effective(request: &Request<'_>, record: Option<&HandoffLanded>) -> Effective {
    let kept = record.map(|record| &record.options);
    let path = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));
    Effective {
        siblings: if request.siblings.is_empty() {
            kept.map(|kept| {
                kept.siblings
                    .iter()
                    .map(|(landed, live)| (path(landed), path(live)))
                    .collect()
            })
            .unwrap_or_default()
        } else {
            request.siblings.to_vec()
        },
        readme: request.readme
            || record.is_some_and(|record| {
                record
                    .files
                    .iter()
                    .any(|file| file.path == README.as_bytes())
            }),
    }
}

fn stored(estate: &Estate<'_>, options: &Effective) -> Stored {
    Stored {
        plan: estate.plan.as_os_str().as_bytes().to_vec(),
        corpus: estate.corpus.as_os_str().as_bytes().to_vec(),
        state: estate.state.as_os_str().as_bytes().to_vec(),
        siblings: options
            .siblings
            .iter()
            .map(|(a, b)| {
                (
                    a.as_os_str().as_bytes().to_vec(),
                    b.as_os_str().as_bytes().to_vec(),
                )
            })
            .collect(),
        readme: options.readme,
    }
}

/// This landing's hand-off identity (the `for=` its blocks carry).
fn ours(workspace: &Workspace<'_>, estate: &Estate<'_>) -> String {
    render::owner(
        &host(),
        workspace.worktree.as_os_str().as_bytes(),
        estate.source,
    )
}

/// The metadata digest of everything a block says (design 3.2), its
/// targets left out (their prints are checked against the record
/// separately, so a run's own writes never move it).
fn inputs_digest(
    workspace: &Workspace<'_>,
    estate: &Estate<'_>,
    stored: &Stored,
    census: &Census,
    sibling: Option<&Sibling>,
) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-handoff-inputs-v1\0");
    let mut put = |bytes: &[u8]| {
        hasher.update(&len64(bytes).to_le_bytes());
        hasher.update(bytes);
    };
    put(&encode(stored)?);
    put(workspace.item.as_bytes());
    put(workspace.worktree.as_os_str().as_bytes());
    put(workspace.source_path.as_os_str().as_bytes());
    put(estate.source.as_bytes());
    put(&estate.plan_digest);
    put(&encode(&build())?);
    put(host().as_bytes());
    put(&census.refs);
    for member in &census.members {
        put(member.path.as_os_str().as_bytes());
        put(&[u8::from(member.main)]);
        put(member.item.as_deref().unwrap_or_default().as_bytes());
        put(&member
            .ledger
            .as_ref()
            .map_or([0; 32], |(_, digest)| *digest));
        put(&encode(&member.now)?);
        put(&member.changed.to_le_bytes());
        put(&[u8::from(member.dirty)]);
        let planned = member
            .item
            .as_ref()
            .and_then(|id| estate.items.iter().find(|item| &item.id == id));
        put(&planned.map_or([0; 32], |item| item.records));
    }
    put(&encode(&sibling)?);
    Ok(*hasher.finalize().as_bytes())
}

/// postcard bytes of a value, for the digest.
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(value).map_err(|_| BulkloadRefusal::FrameCodec)
}

fn diff_count(repo: &Path, a: &str, b: &str) -> Option<(u64, u64)> {
    let listed = super::super::output(git(repo).args([
        "diff-tree",
        "-r",
        "-z",
        "--no-renames",
        "--name-status",
        a,
        b,
    ]))
    .ok()?;
    let fields: Vec<&[u8]> = listed.split(|byte| *byte == 0).collect();
    let (mut added, mut other) = (0u64, 0u64);
    for pair in fields.chunks(2) {
        match pair.first() {
            Some(status) if status.starts_with(b"A") => added += 1,
            Some(status) if !status.is_empty() => other += 1,
            _ => {}
        }
    }
    Some((added, other))
}

/// What the capture of `instance` held for its checkout.
fn capture_facts(repo: &Path, source: &str, instance: &str) -> Option<CaptureFacts> {
    let meta = Meta::read(repo, source, instance).ok()?;
    let head = meta.get("head").ok()?.to_owned();
    let branch = meta.symbolic(repo).ok()?;
    let staged = meta.get("staged").ok()?.to_owned();
    let worktree = meta.get(super::super::WORKTREE_REVISION).ok()?.to_owned();
    let (added, changed) = diff_count(repo, &head, &staged)?;
    let (untracked, modified) = diff_count(repo, &staged, &worktree)?;
    let stashes = super::super::output(git(repo).args([
        "for-each-ref",
        "--format=%(objectname)",
        &format!("refs/carry/v1/{source}/{instance}/stashes/"),
    ]))
    .ok()?;
    let stash = stashes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count();
    Some(CaptureFacts {
        branch,
        head,
        staged: added + changed,
        modified,
        untracked,
        stash: u64::try_from(stash).unwrap_or(u64::MAX),
    })
}

/// Every ref whose destination value is not the landed capture's, in
/// `estate-diff`'s classes (`converge::diff::ref_class`).
fn ref_diffs(repo: &Path, source: &str, instance: &str) -> Result<Vec<RefDiff>> {
    let (native, symbolic) = super::super::native_refs::native_refs(repo)?;
    let carried = super::super::native_refs::carried(repo, source, instance)?;
    let mut names: Vec<&String> = native.keys().chain(carried.keys()).collect();
    names.sort();
    names.dedup();
    let mut rows = Vec::new();
    for name in names {
        if !super::super::native_refs::restorable(name) || symbolic.contains(name) {
            continue;
        }
        if let Some(class) = super::diff::ref_class(repo, native.get(name), carried.get(name))? {
            if class != "same" {
                rows.push(RefDiff {
                    class: class.to_owned(),
                    name: name.clone(),
                });
            }
        }
    }
    Ok(rows)
}

/// Everything the block says, read: the census, the capture facts, the
/// ref classes.
#[allow(clippy::too_many_arguments)]
fn gather(
    workspace: &Workspace<'_>,
    estate: &Estate<'_>,
    census: &Census,
    ledger: &LandedWorkspace,
    sibling: Option<Sibling>,
    carried: Vec<Vec<u8>>,
    as_of_ns: i128,
) -> Result<Inputs> {
    let this = canonical(workspace.worktree);
    let mut family = Vec::new();
    let mut dest_only = Vec::new();
    let mut refused = Vec::new();
    for member in &census.members {
        let planned = member
            .item
            .as_ref()
            .and_then(|id| estate.items.iter().find(|item| &item.id == id));
        if member.item.is_none() {
            // A destination-only worktree is reconciled once another branch
            // holds its HEAD commit (merged or picked into the copy that
            // carries the work forward).
            let consolidated = member.now.as_ref().is_some_and(|(branch, oid)| {
                held_by_branch(workspace.worktree, oid, branch.as_deref())
            });
            dest_only.push(DestOnly {
                path: member.path.as_os_str().as_bytes().to_vec(),
                consolidated,
                dirty: member.dirty,
            });
        }
        if let Some(code) = planned.and_then(|item| item.refused.clone()) {
            refused.push((
                planned.map(|item| item.id.clone()).unwrap_or_default(),
                code,
            ));
        }
        family.push(FamilyRow {
            path: member.path.as_os_str().as_bytes().to_vec(),
            main: member.main,
            this: member.path == this,
            capture: member.ledger.as_ref().and_then(|(ledger, _)| {
                capture_facts(&member.path, estate.source, &ledger.instance)
            }),
            now: member.now.clone(),
            landing: match (&member.ledger, &member.item) {
                (Some(_), _) => Landing::Applied {
                    changed: member.changed,
                },
                (None, Some(_)) => Landing::NotLanded,
                (None, None) => Landing::NotPlanned,
            },
        });
    }
    let planned = estate.items.iter().find(|item| item.id == workspace.item);
    Ok(Inputs {
        as_of_ns,
        source: estate.source.to_owned(),
        dest_host: host(),
        plan: estate.plan.as_os_str().as_bytes().to_vec(),
        plan_digest: estate.plan_digest,
        corpus: estate.corpus.as_os_str().as_bytes().to_vec(),
        state: estate.state.as_os_str().as_bytes().to_vec(),
        build: build(),
        item: workspace.item.to_owned(),
        capture: ledger.capture,
        current: planned.and_then(|item| item.current),
        captured_ns: ledger.observed_ns,
        landed_ns: ledger.recorded_ns,
        workspace: workspace.worktree.as_os_str().as_bytes().to_vec(),
        source_path: workspace.source_path.as_os_str().as_bytes().to_vec(),
        sibling,
        family,
        refs: ref_diffs(workspace.worktree, estate.source, &ledger.instance)?,
        dest_only,
        carried,
        refused,
        omitted: planned.and_then(|item| item.omitted.clone()),
    })
}

fn block_for(inputs: &Inputs) -> Result<Vec<u8>> {
    let block = render::render(inputs, false);
    if block.len() <= MAX_BLOCK {
        return Ok(block);
    }
    let compact = render::render(inputs, true);
    if compact.len() <= MAX_BLOCK {
        return Ok(compact);
    }
    Err(BulkloadRefusal::BudgetExceeded)
}

/// The landed sides of a workspace, read once and only when a render
/// writes over a target, and the carried blocks recorded as removed from
/// them (`handoff.stripped`, OI-1003-Q190).
struct Landed<'a> {
    source: &'a str,
    instance: &'a str,
    sides: Option<BTreeMap<Vec<u8>, Side>>,
    stripped: Vec<Stripped>,
}

/// The landed blob of target `name` when `remainder` (the bytes a render
/// keeps under its block) is exactly it, or it without carried blocks a
/// recorded `--clear --clear-carried` removed from its head (OI-1003-Q190),
/// else `None`.
fn landed_base(
    worktree: &Path,
    landed: &mut Landed<'_>,
    name: &str,
    remainder: &[u8],
) -> Result<Option<String>> {
    landed_match(worktree, landed, name, remainder, true)
}

/// [`landed_base`], counting the recorded removals only with `stripped`.
fn landed_match(
    worktree: &Path,
    landed: &mut Landed<'_>,
    name: &str,
    remainder: &[u8],
    stripped: bool,
) -> Result<Option<String>> {
    landed_compare(worktree, landed, name, remainder, stripped, true)
}

/// The landed blob of target `name` when it is `bytes` (`whole`), or
/// starts with them (not `whole`), from its start or after the carried
/// blocks a recorded removal took from its head (only with `stripped`).
fn landed_compare(
    worktree: &Path,
    landed: &mut Landed<'_>,
    name: &str,
    bytes: &[u8],
    stripped: bool,
    whole: bool,
) -> Result<Option<String>> {
    let remainder = bytes;
    if landed.instance.is_empty() {
        return Ok(None);
    }
    if landed.sides.is_none() {
        let meta = Meta::read(worktree, landed.source, landed.instance)?;
        landed.sides = Some(sides(worktree, &meta)?);
    }
    let Some(blob) = landed
        .sides
        .as_ref()
        .and_then(|sides| sides.get(name.as_bytes()))
        .filter(|side| side.kind == Kind::Regular)
        .and_then(|side| side.blob.clone())
    else {
        return Ok(None);
    };
    let mut skips = vec![0];
    if stripped {
        skips.extend(
            landed
                .stripped
                .iter()
                .filter(|strip| strip.path == name.as_bytes() && strip.base == blob)
                .map(|strip| strip.len),
        );
    }
    // The landed blob's bytes, as the object store holds them, compared
    // with the remainder (a read through the existing batch reader; no
    // object is written).
    let mut objects = super::super::batch_objects::BatchObjects::new(worktree)?;
    let mut found = false;
    for skip in skips {
        let mut same = SameBytes {
            expected: remainder,
            at: 0,
            equal: true,
            skip,
            whole,
        };
        objects.copy_into(&blob, &mut same, None)?;
        let long_enough = if whole {
            same.at == remainder.len()
        } else {
            same.at >= remainder.len()
        };
        if same.equal && same.skip == 0 && long_enough {
            found = true;
            break;
        }
    }
    objects.finish()?;
    Ok(found.then_some(blob))
}

/// A writer that checks it is handed exactly `expected` (`whole`), or
/// bytes that start with it (not `whole`), in order, after its first `skip`
/// bytes.
struct SameBytes<'a> {
    expected: &'a [u8],
    at: usize,
    equal: bool,
    skip: u64,
    whole: bool,
}

impl std::io::Write for SameBytes<'_> {
    fn write(&mut self, given: &[u8]) -> std::io::Result<usize> {
        let skipped = usize::try_from(self.skip)
            .unwrap_or(usize::MAX)
            .min(given.len());
        self.skip = self
            .skip
            .saturating_sub(u64::try_from(skipped).unwrap_or(u64::MAX));
        let bytes = given.get(skipped..).unwrap_or_default();
        let end = self.at.saturating_add(bytes.len());
        if self.whole {
            self.equal &= self.expected.get(self.at..end) == Some(bytes);
        } else {
            // Only the bytes `expected` covers are compared.
            let (from, to) = (
                self.at.min(self.expected.len()),
                end.min(self.expected.len()),
            );
            self.equal &= self.expected.get(from..to) == bytes.get(..to - from);
        }
        self.at = end;
        Ok(given.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---- one workspace ------------------------------------------------------------------

/// Where one workspace's records are.
struct Place {
    common: PathBuf,
    dir: PathBuf,
    ledger: LandedWorkspace,
}

fn place(worktree: &Path, source: &str) -> Result<Place> {
    let git_dir = landed::worktree_git_dir(worktree)?
        .filter(|dir| dir.is_dir())
        .ok_or(BulkloadRefusal::HandoffNotLanded)?;
    let dir = landed::ledger_dir(&git_dir, source);
    let ledger = landed::read::<LandedWorkspace>(&dir.join(landed::WORKSPACE_LANDED))?
        .ok_or(BulkloadRefusal::HandoffNotLanded)?;
    let common = landed::common_dir(&git_dir)?;
    Ok(Place {
        common,
        dir,
        ledger,
    })
}

/// The options `worktree`'s hand-off record was rendered under: the H
/// step's opt-in (design 6.4).
///
/// With no record yet, a pending intent's: a render's next record, or the
/// defaults for a clear, which the H step only finishes. `None` when the
/// worktree holds neither for `source`.
///
/// # Errors
/// An unreadable record or intent.
pub fn recorded_options(worktree: &Path, source: &str) -> Result<Option<Stored>> {
    let Some(git_dir) = landed::worktree_git_dir(worktree)?.filter(|dir| dir.is_dir()) else {
        return Ok(None);
    };
    let dir = landed::ledger_dir(&git_dir, source);
    if let Some(record) = landed::read::<HandoffLanded>(&dir.join(HANDOFF_LANDED))? {
        return Ok(Some(record.options));
    }
    Ok(landed::read::<HandoffIntent>(&dir.join(HANDOFF_INTENT))?
        .map(|intent| intent.next.map(|next| next.options).unwrap_or_default()))
}

/// Whether `worktree` holds a pending hand-off intent for `source` (one
/// `lstat`): a crashed hand-off, or a crashed H step an apply's journal
/// no-op would never reach again, which the next apply settles.
#[must_use]
pub fn intent_pending(worktree: &Path, source: &str) -> bool {
    landed::worktree_git_dir(worktree)
        .ok()
        .flatten()
        .filter(|dir| dir.is_dir())
        .is_some_and(|git_dir| {
            landed::exists(&landed::ledger_dir(&git_dir, source).join(HANDOFF_INTENT))
                .unwrap_or(false)
        })
}

/// Hand off one workspace (render, dry run, clear or check).
///
/// # Errors
/// `HANDOFF_NOT_LANDED`, `HANDOFF_TARGET_UNSUPPORTED`,
/// `HANDOFF_BLOCK_MODIFIED`, `GIT_CONVERGE_INTERRUPTED` (a converge of the
/// workspace is pending), `DESTINATION_EXCHANGE_UNSUPPORTED`,
/// `BUDGET_EXCEEDED`, or a file-system or Git failure.
pub fn run(
    workspace: &Workspace<'_>,
    request: &Request<'_>,
    estate: &Estate<'_>,
    session: &mut Session,
) -> Result<Row> {
    let place = place(workspace.worktree, estate.source)?;
    if landed::exists(&place.dir.join(landed::WORKSPACE_INTENT))? {
        return Err(BulkloadRefusal::GitConvergeInterrupted);
    }
    let ours = ours(workspace, estate);
    match request.mode {
        Mode::Check => check(workspace, request, estate, session, &place, &ours),
        Mode::Clear => {
            let _lock = landed::lock(&place.common, estate.source)?;
            let settled = settle(workspace.worktree, &place.dir, estate.source, &ours)?;
            if settled.cleared && !request.clear_carried {
                return Ok(Row {
                    outcome: "cleared",
                    ..Row::default()
                });
            }
            // A pending clear finished, then `--clear-carried` asked for
            // more: a clear of what is left.
            let mut row = clear(
                workspace.worktree,
                &place.dir,
                estate.source,
                &ours,
                request.clear_carried,
            )?;
            if settled.cleared {
                row.outcome = "cleared";
            }
            Ok(row)
        }
        Mode::Render | Mode::DryRun => {
            let _lock = if request.mode == Mode::Render {
                let lock = landed::lock(&place.common, estate.source)?;
                let settled = settle(workspace.worktree, &place.dir, estate.source, &ours)?;
                // The H step re-renders a record; it never starts one, and
                // never undoes a clear it just finished.
                if request.h_step
                    && (settled.cleared || !landed::exists(&place.dir.join(HANDOFF_LANDED))?)
                {
                    return Ok(Row {
                        outcome: if settled.cleared {
                            "cleared"
                        } else {
                            "unchanged"
                        },
                        ..Row::default()
                    });
                }
                Some(lock)
            } else {
                None
            };
            render_workspace(workspace, request, estate, session, &place, &ours)
        }
    }
}

fn census_of<'s>(
    session: &'s mut Session,
    place: &Place,
    workspace: &Workspace<'_>,
    estate: &Estate<'_>,
) -> Result<&'s Census> {
    let key = canonical(&place.common);
    if !session.census.contains_key(&key) {
        let found = census(
            &place.common,
            workspace.worktree,
            estate.source,
            estate.items,
        )?;
        session.census.insert(key.clone(), found);
    }
    session
        .census
        .get(&key)
        .ok_or(BulkloadRefusal::GitInventoryMalformed)
}

/// What a render plans for one target.
enum Plan {
    /// Nothing to write; the entry the record keeps (if any, its print and
    /// base re-checked when the print moved) and the action.
    Keep(Option<HandoffFile>, &'static str),
    /// Retired: removed by hand while the capture stayed.
    Retire(&'static str),
    Prepend(Whole),
    /// The bytes read, the old block's length, and the target's kind.
    Replace(Whole, u64, FileKind),
    Create,
}

/// Whether a target's bytes (under this landing's block, when it has one)
/// start with a carried block: another landing's, or one of this landing's
/// identity that the target's landed blob itself starts with (a round
/// trip). A block of this landing's that no record names and that the
/// landed blob does not start with is never carried (`Modified`, H49).
const fn carried_of(state: Option<&Held>) -> bool {
    match state {
        Some(Held::Foreign) => true,
        Some(Held::Intact(file)) => file.carried,
        _ => false,
    }
}

#[allow(clippy::too_many_lines)]
fn render_workspace(
    workspace: &Workspace<'_>,
    request: &Request<'_>,
    estate: &Estate<'_>,
    session: &mut Session,
    place: &Place,
    ours: &str,
) -> Result<Row> {
    // The record's racy instant, before this render's first read of a
    // target and its first write (C03): every print it records is taken
    // after it, so a target the render wrote, or one a writer changed
    // between that write and its print, stays racy until a rerun has read
    // it again, however long the render takes.
    let started_ns = super::super::pass_start_ns();
    let worktree = workspace.worktree;
    let this = canonical(worktree);
    let record_path = place.dir.join(HANDOFF_LANDED);
    let record = landed::read::<HandoffLanded>(&record_path)?;
    let options = effective(request, record.as_ref());
    let names = names_for(worktree, options.readme, record.as_ref())?;
    let found = classify(worktree, &names)?;
    let fsid = landed::fsid(worktree)?;
    let stored = stored(estate, &options);
    let census = census_of(session, place, workspace, estate)?.clone();
    let sibling = sibling_of(&this, &options.siblings)?;
    let inputs = inputs_digest(workspace, estate, &stored, &census, sibling.as_ref())?;
    let mut landed = Landed {
        source: estate.source,
        instance: &place.ledger.instance,
        sides: None,
        stripped: stripped_of(&place.dir)?,
    };
    // What each target holds (read only when its print moved).
    let mut held_by: Vec<(&'static str, Option<Held>)> = Vec::new();
    for (name, entry) in &found {
        let state = match entry {
            Found::Regular(meta) => {
                let state = held(
                    worktree,
                    name,
                    meta,
                    record.as_ref(),
                    fsid,
                    ours,
                    &mut landed,
                )?;
                if state == Held::Modified {
                    return Err(BulkloadRefusal::HandoffBlockModified);
                }
                Some(state)
            }
            _ => None,
        };
        held_by.push((name, state));
    }
    let retired = |name: &str| {
        record.as_ref().is_some_and(|record| {
            record.retired.iter().any(|retired| {
                retired.path == name.as_bytes() && retired.capture == place.ledger.capture
            })
        })
    };
    let recorded = |name: &str| {
        record.as_ref().and_then(|record| {
            record
                .files
                .iter()
                .find(|file| file.path == name.as_bytes())
        })
    };
    let current = record.as_ref().is_some_and(|record| {
        record.inputs == inputs && record.item == workspace.item && record.options == stored
    });
    let unchanged = current
        && !request.renew
        && found
            .iter()
            .zip(&held_by)
            .all(|((name, entry), (_, state))| match (entry, state) {
                (Found::Regular(meta), Some(Held::Intact(file))) => {
                    file.print == SeatPrint::of(name.as_bytes(), meta)
                }
                (Found::Via(to), _) => {
                    recorded(name).is_some_and(|file| file.kind == FileKind::ViaSymlink(to.clone()))
                }
                (Found::Missing, _) if never_created(name) => true,
                (Found::Missing | Found::Regular(_), _) => retired(name),
            });
    if unchanged && request.mode == Mode::Render {
        // A print the record could not vouch for (racy against its
        // `recorded_ns`) was proved by reading the block; once the clock is
        // past its stamp, the record is re-dated so the next rerun trusts
        // it and reads nothing (as Git re-dates a racily clean index). The
        // bytes under each such block are checked again first (`retaken`),
        // so a re-dated print never vouches for an edit below the block
        // made inside the racy window (review of 2026-10-16).
        if let Some(record) = &record {
            let now_ns = super::super::pass_start_ns();
            let racy_from = now_ns.saturating_sub(super::super::RACY_GRANULARITY_NS);
            let mut redated = record.clone();
            let mut due = false;
            let mut held_still = true;
            for file in &mut redated.files {
                let Some(Found::Regular(meta)) = found
                    .iter()
                    .find(|(name, _)| name.as_bytes() == file.path.as_slice())
                    .map(|(_, entry)| entry)
                else {
                    continue;
                };
                let now = SeatPrint::of(&file.path, meta);
                if trusted(record, file, now.as_ref(), fsid)
                    || file
                        .print
                        .as_ref()
                        .is_none_or(|print| {
                            print.mtime_ns >= racy_from || print.ctime_ns >= racy_from
                        })
                {
                    continue;
                }
                due = true;
                match retaken(worktree, &mut landed, file)? {
                    Some(checked) if checked.print == file.print => *file = checked,
                    _ => held_still = false,
                }
            }
            if due && held_still {
                redated.recorded_ns = now_ns;
                landed::write(&record_path, &redated)?;
            }
        }
        return Ok(Row {
            outcome: "unchanged",
            open: record.as_ref().map(|record| record.open),
            files: record
                .iter()
                .flat_map(|record| &record.files)
                .map(|file| FileRow {
                    path: file.path.clone(),
                    kind: file.kind.name(),
                    action: "unchanged",
                })
                .collect(),
            inputs: Some(inputs),
            ..Row::default()
        });
    }
    let as_of_ns = request.as_of.unwrap_or_else(|| match &record {
        Some(record) if current => record.as_of_ns,
        _ => super::super::pass_start_ns(),
    });
    let carried: Vec<Vec<u8>> = held_by
        .iter()
        .filter(|(_, state)| carried_of(state.as_ref()))
        .map(|(name, _)| name.as_bytes().to_vec())
        .collect();
    let facts = gather(
        workspace,
        estate,
        &census,
        &place.ledger,
        sibling,
        carried,
        as_of_ns,
    )?;
    let open = facts.open();
    let block = block_for(&facts)?;
    let block_digest = digest(&block);
    let renewable = |name: &str| {
        // A block a person removed by hand stays removed while the
        // workspace ledger's capture is the record's (design 6.5).
        !request.renew
            && (retired(name)
                || (recorded(name).is_some()
                    && record
                        .as_ref()
                        .is_some_and(|record| record.capture == place.ledger.capture)))
    };
    let mut plans: Vec<(&'static str, Plan)> = Vec::new();
    for ((name, entry), (_, state)) in found.iter().zip(&held_by) {
        let path = worktree.join(name);
        let plan = match (entry, state) {
            (Found::Via(_), _) => Plan::Keep(None, "none"),
            (Found::Regular(meta), Some(Held::Intact(file))) => {
                if file.block != block_digest {
                    // The kind stays: a created file re-rendered is still
                    // the hand-off's to remove and to lift (H43).
                    Plan::Replace(
                        read_block(&path, name, file)?,
                        file.block_len,
                        file.kind.clone(),
                    )
                } else if record.as_ref().is_some_and(|record| {
                    trusted(
                        record,
                        file,
                        SeatPrint::of(name.as_bytes(), meta).as_ref(),
                        fsid,
                    )
                }) {
                    Plan::Keep(Some(file.clone()), "unchanged")
                } else {
                    // The block is kept but the record's print cannot vouch
                    // for the bytes: the file moved (an edit below it, a
                    // touch), or the print is racy (it may have caught an
                    // edit made in the tick of the render's exchange, and
                    // this run re-dates the record). What the record says
                    // of the bytes under the block is checked again before
                    // its print is kept or re-taken (review of 2026-10-16,
                    // the BLOCKER; re-review, finding 1).
                    let checked = retaken(worktree, &mut landed, file)?
                        .ok_or(BulkloadRefusal::HandoffBlockModified)?;
                    // A retired block put back is recorded again.
                    let action = if recorded(name).is_some() {
                        "unchanged"
                    } else {
                        "restored"
                    };
                    Plan::Keep(Some(checked), action)
                }
            }
            (Found::Regular(_), _) => {
                if renewable(name) {
                    Plan::Retire("removed-by-hand")
                } else {
                    // Read whole and classified again as `held` did: no
                    // block of this landing's at byte 0 (a carried one is
                    // content, kept below the new block).
                    Plan::Prepend(read_unowned(
                        worktree,
                        name,
                        entry_of(record.as_ref(), name).as_ref(),
                        ours,
                        &mut landed,
                    )?)
                }
            }
            (Found::Missing, _) if never_created(name) => Plan::Keep(None, "none"),
            (Found::Missing, _) => {
                if renewable(name) {
                    Plan::Retire("removed-by-hand")
                } else {
                    Plan::Create
                }
            }
        };
        plans.push((name, plan));
    }
    // A link is covered when its target carries (or keeps) the block.
    let covered = |to: &[u8], plans: &[(&'static str, Plan)]| {
        plans.iter().any(|(name, plan)| {
            name.as_bytes() == to
                && matches!(
                    plan,
                    Plan::Prepend(_) | Plan::Replace(..) | Plan::Create | Plan::Keep(Some(_), _)
                )
        })
    };
    let tag = tag();
    let mut steps = Vec::new();
    let mut contents: Vec<Option<Vec<u8>>> = Vec::new();
    for (name, plan) in &plans {
        let (old_block, before, new) = match plan {
            Plan::Prepend(read) => {
                let mut new = block.clone();
                new.extend_from_slice(&read.bytes);
                (None, Some(digest(&read.bytes)), new)
            }
            Plan::Replace(read, old_len, _) => {
                let cut = usize::try_from(*old_len).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
                let mut new = block.clone();
                new.extend_from_slice(read.bytes.get(cut..).unwrap_or_default());
                (
                    read.bytes.get(..cut).map(<[u8]>::to_vec),
                    Some(digest(&read.bytes)),
                    new,
                )
            }
            Plan::Create => (None, None, block.clone()),
            Plan::Keep(..) | Plan::Retire(_) => {
                contents.push(None);
                continue;
            }
        };
        steps.push(Step {
            path: name.as_bytes().to_vec(),
            old_block,
            new_block: Some(block.clone()),
            before,
            after: Some(digest(&new)),
        });
        contents.push(Some(new));
    }
    let mut files = Vec::new();
    let mut rows = Vec::new();
    let mut retired_now: Vec<Retired> = record
        .iter()
        .flat_map(|record| record.retired.clone())
        .filter(|retired| retired.capture == place.ledger.capture && !request.renew)
        .collect();
    for ((name, plan), (_, entry)) in plans.iter().zip(&found) {
        let (kind, action) = match plan {
            Plan::Keep(Some(file), action) => (file.kind.clone(), *action),
            Plan::Keep(None, action) => match entry {
                Found::Via(to) if covered(to, &plans) => {
                    (FileKind::ViaSymlink(to.clone()), "unchanged")
                }
                _ => {
                    rows.push(FileRow {
                        path: name.as_bytes().to_vec(),
                        kind: "absent",
                        action,
                    });
                    continue;
                }
            },
            Plan::Retire(action) => {
                let path = name.as_bytes().to_vec();
                if !retired_now.iter().any(|retired| retired.path == path) {
                    if let Some(file) = recorded(name) {
                        retired_now.push(Retired {
                            path: path.clone(),
                            capture: place.ledger.capture,
                            kind: file.kind.clone(),
                            block_len: file.block_len,
                            block: file.block,
                        });
                    }
                }
                rows.push(FileRow {
                    path,
                    kind: recorded(name).map_or("prefixed", |file| file.kind.name()),
                    action,
                });
                continue;
            }
            Plan::Prepend(_) => (FileKind::Prefixed, "written"),
            Plan::Replace(.., kind) => (kind.clone(), "written"),
            Plan::Create => (FileKind::Created, "created"),
        };
        rows.push(FileRow {
            path: name.as_bytes().to_vec(),
            kind: kind.name(),
            action,
        });
        let under = |read: &'_ Whole, cut: usize| -> Vec<u8> {
            read.bytes.get(cut..).unwrap_or_default().to_vec()
        };
        let (base, carried, print) = match plan {
            Plan::Keep(Some(file), _) => (file.base.clone(), file.carried, file.print.clone()),
            Plan::Prepend(read) => (
                landed_base(worktree, &mut landed, name, &read.bytes)?,
                render::marker(&read.bytes) != render::Marker::None,
                None,
            ),
            Plan::Replace(read, old_len, _) => {
                let rest = under(
                    read,
                    usize::try_from(*old_len).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
                );
                (
                    landed_base(worktree, &mut landed, name, &rest)?,
                    render::marker(&rest) != render::Marker::None,
                    None,
                )
            }
            _ => (None, false, None),
        };
        files.push(HandoffFile {
            path: name.as_bytes().to_vec(),
            kind,
            block_len: len64(&block),
            block: block_digest,
            print,
            base,
            carried,
        });
    }
    if request.mode == Mode::DryRun {
        return Ok(Row {
            outcome: "planned",
            files: rows,
            inputs: Some(inputs),
            open: Some(open),
            block: Some(block),
            ..Row::default()
        });
    }
    // A retired target whose block is back (put back by hand, or renewed)
    // is a recorded entry again.
    retired_now.retain(|retired| {
        !files
            .iter()
            .any(|file: &HandoffFile| file.path == retired.path)
    });
    retired_now.sort();
    retired_now.dedup();
    let mut next = HandoffLanded {
        item: workspace.item.to_owned(),
        capture: place.ledger.capture,
        inputs,
        open,
        as_of_ns,
        recorded_ns: 0,
        fsid,
        options: stored,
        files,
        retired: retired_now,
    };
    next.files.sort_by(|a, b| a.path.cmp(&b.path));
    let wrote = !steps.is_empty();
    if wrote {
        let intent = HandoffIntent {
            op: Op::Render,
            tag: tag.clone(),
            steps: steps.clone(),
            next: Some(next.clone()),
            stripped: Vec::new(),
            carried: false,
        };
        landed::write(&place.dir.join(HANDOFF_INTENT), &intent)?;
        fault_point!(EstateHandoffAfterIntent);
        let mut n = 0;
        let mut outcome = Ok(());
        for ((name, plan), content) in plans.iter().zip(&contents) {
            let Some(content) = content else {
                continue;
            };
            let temp = temp_name(&tag, n);
            n += 1;
            outcome = match plan {
                Plan::Prepend(read) | Plan::Replace(read, ..) => {
                    exchange(worktree, name, read, content, &temp, Op::Render)
                }
                Plan::Create => create(worktree, name, content, &temp),
                Plan::Keep(..) | Plan::Retire(_) => Ok(()),
            };
            if outcome.is_err() {
                break;
            }
        }
        if let Err(refusal) = outcome {
            // What was written stays recorded; the next run renders again.
            settle(worktree, &place.dir, estate.source, ours)?;
            return Err(refusal);
        }
    }
    #[cfg(test)]
    super::super::mid_pass::fire(&this, super::super::mid_pass::Stage::HandoffWritten);
    // The prints of what this run wrote (a kept target's is the one its
    // bytes were checked at). Each is racy against `recorded_ns`, this
    // render's start, taken before its first write (C03), so converge
    // trusts none until a rerun re-dates it after checking the bytes again.
    for file in &mut next.files {
        if matches!(file.kind, FileKind::ViaSymlink(_)) || file.print.is_some() {
            continue;
        }
        file.print = lstat(&worktree.join(OsStr::from_bytes(&file.path)))?
            .and_then(|meta| SeatPrint::of(&file.path, &meta));
    }
    next.recorded_ns = started_ns;
    let rewrite = wrote || record.as_ref() != Some(&next);
    if rewrite {
        if next.files.is_empty() && next.retired.is_empty() {
            landed::remove(&record_path)?;
        } else {
            landed::write(&record_path, &next)?;
        }
    }
    if wrote {
        fault_point!(EstateHandoffAfterRecord);
        landed::remove(&place.dir.join(HANDOFF_INTENT))?;
    }
    Ok(Row {
        outcome: if wrote { "written" } else { "unchanged" },
        files: rows,
        inputs: Some(inputs),
        open: Some(open),
        ..Row::default()
    })
}

/// The length of the carried blocks at the head of `bytes`: every
/// well-formed block there, one after another (a broken one ends them).
fn carried_len(bytes: &[u8]) -> usize {
    let mut at = 0;
    while let render::Marker::Block(len) = render::marker(bytes.get(at..).unwrap_or_default()) {
        at += len;
    }
    at
}

/// One target a clear rewrites: the bytes read, this landing's block's
/// length (0: none), the carried blocks' length removed under it (0: none,
/// OI-1003-Q190) and the record's entry for the block.
struct Clearing {
    name: &'static str,
    read: Whole,
    cut: usize,
    drop: usize,
    file: Option<HandoffFile>,
}

/// Clear one workspace (design 7): each intact recorded block stripped (a
/// created file that is the block alone removed), then the record and the
/// intent removed. A marker of this landing's that is not the record's
/// refuses `HANDOFF_BLOCK_MODIFIED` with nothing written; a carried block is
/// content and stays (OI-1003-Q185) unless `carried` (`--clear-carried`,
/// OI-1003-Q190) asks: then the carried blocks at the head of a target's
/// landed content (under this landing's block, or alone) are removed by
/// the same intent and exchange, and recorded in `handoff.stripped`, which
/// converge reads (the landing's objects still hold the bytes). A carried
/// block above content that is not the landed blob (a user's edit) is
/// kept, row `carried-kept`. A refusal after the intent keeps it, so the
/// next run of either verb finishes the clear ([`settle`]).
#[allow(clippy::too_many_lines)]
fn clear(worktree: &Path, dir: &Path, source: &str, ours: &str, carried: bool) -> Result<Row> {
    let record_path = dir.join(HANDOFF_LANDED);
    let intent_path = dir.join(HANDOFF_INTENT);
    let stripped_path = dir.join(HANDOFF_STRIPPED);
    let record = landed::read::<HandoffLanded>(&record_path)?;
    // A pending clear this call finishes (from `settle`): its choice and
    // the removals of the steps it completed are kept.
    let prior = landed::read::<HandoffIntent>(&intent_path)?;
    let carried = carried || prior.as_ref().is_some_and(|prior| prior.carried);
    let fsid = landed::fsid(worktree)?;
    let instance = landed_of(dir)?;
    let mut landed = Landed {
        source,
        instance: instance.as_deref().unwrap_or_default(),
        sides: None,
        stripped: stripped_of(dir)?,
    };
    let mut work: Vec<Clearing> = Vec::new();
    let mut strips: Vec<Stripped> = Vec::new();
    let mut rows = Vec::new();
    for name in ALL_NAMES {
        let path = worktree.join(name);
        let Some(meta) = lstat(&path)? else {
            continue;
        };
        if !meta.is_file() {
            if let Some(file) = record.as_ref().and_then(|record| {
                record
                    .files
                    .iter()
                    .find(|file| file.path == name.as_bytes())
            }) {
                rows.push(FileRow {
                    path: file.path.clone(),
                    kind: file.kind.name(),
                    action: "cleared",
                });
            }
            continue;
        }
        let file = match held(
            worktree,
            name,
            &meta,
            record.as_ref(),
            fsid,
            ours,
            &mut landed,
        )? {
            Held::Plain | Held::Foreign => None,
            Held::Modified => return Err(BulkloadRefusal::HandoffBlockModified),
            Held::Intact(file) => Some(file),
        };
        if file.is_none() && !carried {
            continue;
        }
        let read = match &file {
            Some(file) => read_block(&path, name, file)?,
            None => read_whole(&path, name)?,
        };
        let cut = match &file {
            Some(file) => {
                usize::try_from(file.block_len).map_err(|_| BulkloadRefusal::BudgetExceeded)?
            }
            None => 0,
        };
        let rest = read.bytes.get(cut..).unwrap_or_default();
        let mut drop = 0;
        if carried {
            let blocks = carried_len(rest);
            if blocks > 0 {
                // Only from the landed content itself: its bytes stay in the
                // landing's objects, and converge can account for them.
                match landed_match(worktree, &mut landed, name, rest, false)? {
                    Some(base) => {
                        drop = blocks;
                        let removed = rest.get(..blocks).unwrap_or_default();
                        strips.push(Stripped {
                            path: name.as_bytes().to_vec(),
                            base,
                            len: len64(removed),
                            digest: digest(removed),
                        });
                    }
                    None => rows.push(FileRow {
                        path: name.as_bytes().to_vec(),
                        kind: file.as_ref().map_or("absent", |file| file.kind.name()),
                        action: "carried-kept",
                    }),
                }
            }
        }
        if cut == 0 && drop == 0 {
            continue;
        }
        work.push(Clearing {
            name,
            read,
            cut,
            drop,
            file,
        });
    }
    if work.is_empty() && record.is_none() && prior.is_none() {
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        return Ok(Row {
            outcome: "unchanged",
            files: rows,
            ..Row::default()
        });
    }
    let tag = tag();
    let mut steps = Vec::new();
    for clearing in &work {
        let rest = clearing.read.bytes.get(clearing.cut..).unwrap_or_default();
        let gone = clearing
            .file
            .as_ref()
            .is_some_and(|file| file.kind == FileKind::Created)
            && rest.is_empty();
        let after = clearing
            .read
            .bytes
            .get(clearing.cut + clearing.drop..)
            .unwrap_or_default();
        steps.push(Step {
            path: clearing.name.as_bytes().to_vec(),
            old_block: (clearing.cut > 0)
                .then(|| clearing.read.bytes.get(..clearing.cut).map(<[u8]>::to_vec))
                .flatten(),
            new_block: None,
            before: Some(digest(&clearing.read.bytes)),
            after: (!gone).then(|| digest(after)),
        });
    }
    let intent = HandoffIntent {
        op: Op::Clear,
        tag: tag.clone(),
        steps: steps.clone(),
        next: None,
        stripped: strips.clone(),
        carried,
    };
    if !steps.is_empty() {
        landed::write(&intent_path, &intent)?;
        fault_point!(EstateHandoffAfterIntent);
    }
    for (n, (clearing, step)) in work.iter().zip(&steps).enumerate() {
        let temp = temp_name(&tag, n);
        let outcome = if step.after.is_none() {
            remove(worktree, clearing.name, &clearing.read, &temp)
        } else {
            exchange(
                worktree,
                clearing.name,
                &clearing.read,
                clearing
                    .read
                    .bytes
                    .get(clearing.cut + clearing.drop..)
                    .unwrap_or_default(),
                &temp,
                Op::Clear,
            )
        };
        if let Err(refusal) = outcome {
            // The intent stays: the next run finishes the clear.
            sweep(worktree, Some(&intent), record.as_ref())?;
            return Err(refusal);
        }
        rows.push(FileRow {
            path: clearing.name.as_bytes().to_vec(),
            kind: clearing
                .file
                .as_ref()
                .map_or("absent", |file| file.kind.name()),
            action: if clearing.drop > 0 {
                "carried-cleared"
            } else {
                "cleared"
            },
        });
    }
    // The removals recorded: the ones already recorded, a finished prior
    // clear's (its step done: the target no longer starts with the carried
    // blocks it removed, whatever a user has written since; re-review 3 of
    // 2026-10-16, finding 3), and this run's (OI-1003-Q190).
    let mut recorded = landed.stripped.clone();
    if let Some(prior) = &prior {
        for strip in &prior.stripped {
            let done = prior
                .steps
                .iter()
                .find(|step| step.path == strip.path)
                .is_some_and(|step| {
                    still_carries(
                        &worktree.join(OsStr::from_bytes(&strip.path)),
                        step.old_block.as_deref(),
                        strip,
                    )
                    .is_ok_and(|still| !still)
                });
            if done {
                recorded.push(strip.clone());
            }
        }
    }
    recorded.extend(strips);
    recorded.sort();
    recorded.dedup();
    if recorded != landed.stripped {
        landed::write(&stripped_path, &HandoffStripped { files: recorded })?;
    }
    landed::remove(&record_path)?;
    landed::remove(&intent_path)?;
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Row {
        outcome: "cleared",
        files: rows,
        ..Row::default()
    })
}

/// The lines of the blob of `name` at `rev` (`""`: the index) that hold
/// the token (none when there is no such blob).
fn token_lines(worktree: &Path, rev: &str, name: &str) -> Result<BTreeSet<Vec<u8>>> {
    let spec = if rev.is_empty() {
        format!(":{name}")
    } else {
        format!("{rev}:{name}")
    };
    let out = git(worktree)
        .args(["cat-file", "blob", &spec])
        .output()
        .refuse_at("git_carry::converge::handoff::blob")?;
    if !out.status.success() {
        return Ok(BTreeSet::new());
    }
    Ok(out
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| holds_token(line))
        .map(<[u8]>::to_vec)
        .collect())
}

fn holds_token(bytes: &[u8]) -> bool {
    bytes
        .windows(TOKEN.len())
        .any(|window| window == TOKEN.as_bytes())
}

/// The carried capture: every ref under `refs/carry/` (the source's
/// history as captured, never pushed). What it reaches is the source's
/// history, not a commit made here (OI-1003-Q191).
const CARRIED: &str = "--glob=refs/carry/";

/// The token lines a patch (`git log -p` output, plain or combined diffs)
/// adds: hunk lines marked `+` against every parent (a line removed, or one
/// a merge took unchanged from a parent, adds nothing), without the marks.
fn added_token_lines(patch: &[u8]) -> BTreeSet<Vec<u8>> {
    let mut columns = 0;
    let mut added = BTreeSet::new();
    for line in patch.split(|byte| *byte == b'\n') {
        if line.starts_with(b"diff ") {
            columns = 0;
        } else if line.starts_with(b"@@") {
            columns = line
                .iter()
                .take_while(|byte| **byte == b'@')
                .count()
                .saturating_sub(1)
                .max(1);
        } else if columns > 0
            && line
                .get(..columns)
                .is_some_and(|marks| marks.iter().all(|mark| *mark == b'+'))
        {
            let text = line.get(columns..).unwrap_or_default();
            if holds_token(text) {
                added.insert(text.to_vec());
            }
        }
    }
    added
}

/// `git log` of the token lines added to a name a hand-off may write
/// ([`ALL_NAMES`]) by the commits `revs` select, as patches.
fn token_history(worktree: &Path, revs: &[&OsStr]) -> Result<BTreeSet<Vec<u8>>> {
    let out = git(worktree)
        .args([
            "log",
            // A destination config's log.showSignature would run its
            // gpg.program on every signed commit read (H57).
            "--no-show-signature",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--format=",
            "--patch",
            "--cc",
            "--unified=0",
            "-G",
            TOKEN,
        ])
        .args(revs)
        .arg("--")
        .args(ALL_NAMES)
        .output()
        .refuse_at("git_carry::converge::handoff::history")?;
    if !out.status.success() {
        return Err(super::super::estimate::child_failed(&out.stderr));
    }
    Ok(added_token_lines(&out.stdout))
}

/// The token lines the carried capture's history (the source's) added to a
/// name a hand-off may write: blocks the source committed. Non-empty is
/// `"source_history":true`, information only (OI-1003-Q191).
fn carried_token_lines(worktree: &Path) -> Result<BTreeSet<Vec<u8>>> {
    token_history(worktree, &[OsStr::new(CARRIED)])
}

/// Every stash entry (`refs/stash` and its reflog), or none.
fn stashes(worktree: &Path) -> Result<Vec<std::ffi::OsString>> {
    let listed = super::super::output(git(worktree).args([
        "for-each-ref",
        "--format=%(refname)",
        "refs/stash",
    ]))?;
    if listed.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let entries = super::super::output(git(worktree).args([
        "reflog",
        "show",
        "--format=%H",
        "refs/stash",
        "--",
    ]))?;
    let mut oids: Vec<std::ffi::OsString> = entries
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| OsStr::from_bytes(line).to_owned())
        .collect();
    oids.push("refs/stash".into());
    Ok(oids)
}

/// Whether a commit the carried capture does not reach, in the history of
/// a branch, a tag, a remote-tracking ref, a stash entry or a detached
/// HEAD, adds a token line to a name a hand-off may write ([`ALL_NAMES`],
/// whatever this run's targets are: re-review of 2026-10-16, finding 5)
/// that the source's history (`carried`, [`carried_token_lines`]) never
/// added. A block committed here and removed again is still published by
/// a push (review of 2026-10-16); a block the source committed, also
/// cherry-picked or rebased here, is the source's history, never flagged
/// (OI-1003-Q191; re-review 3 of 2026-10-16, finding 4): every BEGIN line
/// names its block's id and landing, so a block of this landing's is never
/// one of the source's lines. A pushed branch deleted here is still in its
/// remote-tracking ref, and the block says to stash nothing that holds it
/// (open item 19.10.4).
fn history_adds_token(worktree: &Path, carried: &BTreeSet<Vec<u8>>) -> Result<bool> {
    let stashes = stashes(worktree)?;
    let mut revs: Vec<&OsStr> = ["--branches", "--tags", "--remotes"]
        .into_iter()
        .map(OsStr::new)
        .collect();
    revs.extend(stashes.iter().map(std::ffi::OsString::as_os_str));
    if now_of(worktree)?.is_some() {
        revs.push(OsStr::new("HEAD"));
    }
    revs.extend([OsStr::new("--not"), OsStr::new(CARRIED)]);
    Ok(!token_history(worktree, &revs)?.is_subset(carried))
}

/// `--check` (read-only at the destination and in STATE): the hand-off's
/// state, its open rows, and whether a token line the source's history
/// never added is staged or committed (the index against HEAD, and the
/// history of every branch, tag, remote-tracking ref, stash entry and
/// HEAD).
#[allow(clippy::too_many_lines)]
fn check(
    workspace: &Workspace<'_>,
    request: &Request<'_>,
    estate: &Estate<'_>,
    session: &mut Session,
    place: &Place,
    ours: &str,
) -> Result<Row> {
    let worktree = workspace.worktree;
    let this = canonical(worktree);
    let record = landed::read::<HandoffLanded>(&place.dir.join(HANDOFF_LANDED))?;
    let fsid = landed::fsid(worktree)?;
    let options = effective(request, record.as_ref());
    let names = names_for(worktree, options.readme, record.as_ref())?;
    let found = classify(worktree, &names)?;
    let mut landed = Landed {
        source: estate.source,
        instance: &place.ledger.instance,
        sides: None,
        stripped: stripped_of(&place.dir)?,
    };
    let mut modified = false;
    let mut missing = false;
    let mut present = false;
    let mut rows = Vec::new();
    let mut carried = Vec::new();
    for (name, entry) in &found {
        let recorded = record.as_ref().and_then(|record| {
            record
                .files
                .iter()
                .find(|file| file.path == name.as_bytes())
        });
        let action = match entry {
            Found::Regular(meta) => {
                let state = held(
                    worktree,
                    name,
                    meta,
                    record.as_ref(),
                    fsid,
                    ours,
                    &mut landed,
                )?;
                if carried_of(Some(&state)) {
                    carried.push(name.as_bytes().to_vec());
                }
                match state {
                    Held::Intact(_) => {
                        present = true;
                        "present"
                    }
                    Held::Modified => {
                        modified = true;
                        "modified"
                    }
                    Held::Foreign => {
                        missing |= recorded.is_some();
                        "carried"
                    }
                    Held::Plain => {
                        missing |= recorded.is_some();
                        "none"
                    }
                }
            }
            Found::Via(_) => "none",
            Found::Missing => {
                missing |= recorded.is_some() && !never_created(name);
                "none"
            }
        };
        rows.push(FileRow {
            path: name.as_bytes().to_vec(),
            kind: recorded
                .map(|file| file.kind.name())
                .or_else(|| entry_of(record.as_ref(), name).map(|file| file.kind.name()))
                .unwrap_or("absent"),
            action,
        });
    }
    let mut temps = landed::exists(&place.dir.join(HANDOFF_INTENT))?;
    for entry in fs::read_dir(worktree).refuse_at("git_carry::converge::handoff::check")? {
        let entry = entry.refuse_at("git_carry::converge::handoff::check")?;
        temps |= entry
            .file_name()
            .as_bytes()
            .starts_with(TEMP_PREFIX.as_bytes());
    }
    let stored = stored(estate, &options);
    let census = census_of(session, place, workspace, estate)?.clone();
    let sibling = sibling_of(&this, &options.siblings)?;
    let inputs = inputs_digest(workspace, estate, &stored, &census, sibling.as_ref())?;
    let as_of = request.as_of.unwrap_or_else(super::super::pass_start_ns);
    let any_carried = !carried.is_empty();
    let facts = gather(
        workspace,
        estate,
        &census,
        &place.ledger,
        sibling,
        carried,
        as_of,
    )?;
    let open = facts.open();
    // Commit detection (OI-1003-Q191): staged, a line holding the token in
    // a target's index blob that its HEAD blob lacks and the source's
    // history never added (a block the source committed is in HEAD
    // already, or comes back by a cherry-pick); committed, a commit the
    // carried capture does not reach that adds such a line, in the history
    // of any branch, tag, remote-tracking ref, stash entry or HEAD, for
    // every name a hand-off may write. A token the source's history holds
    // is information only (re-review 3 of 2026-10-16, finding 4).
    let carried_lines = carried_token_lines(worktree)?;
    let mut staged = false;
    for name in ALL_NAMES {
        let head = token_lines(worktree, "HEAD", name)?;
        staged |= token_lines(worktree, "", name)?
            .iter()
            .any(|line| !head.contains(line) && !carried_lines.contains(line));
    }
    let committed = history_adds_token(worktree, &carried_lines)?;
    let source_history = !carried_lines.is_empty();
    let state = if committed {
        "committed"
    } else if staged {
        "staged"
    } else if modified {
        "modified"
    } else if temps {
        "temp-left"
    } else if missing {
        "removed-by-hand"
    } else if !present {
        if any_carried {
            "carried"
        } else {
            "absent"
        }
    } else if record
        .as_ref()
        .is_some_and(|record| record.inputs != inputs)
    {
        "stale"
    } else {
        "present"
    };
    Ok(Row {
        outcome: "checked",
        state: Some(state),
        files: rows,
        inputs: Some(inputs),
        open: Some(open),
        refusal: (staged || committed).then_some(BulkloadRefusal::HandoffMarkerCommitted),
        source_history: Some(source_history),
        ..Row::default()
    })
}

#[cfg(test)]
mod tests;
