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
//! - **private by default**: written only into a repository with no remote
//!   (active, or preserved inert from the source in `carry-config`), or
//!   whose every remote URL and push URL, as written and as Git rewrites it
//!   (`insteadOf`, `pushInsteadOf`), is under an operator-given prefix, or a
//!   workspace the operator names; no URL is ever printed;
//! - **removed** by a recorded clear that restores the bytes exactly;
//! - **owned by identity**: a block's BEGIN line names the landing it was
//!   rendered for (`for=`); a block that names another landing was carried
//!   here with the source's bytes and is content, kept byte for byte below
//!   this landing's block (OI-1003-Q185: nothing is dropped from a carry).
//!
//! The source is never read (S2).

pub mod render;

use crate::refuse::RefuseAt as _;
use std::collections::BTreeMap;
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

// ---- records -----------------------------------------------------------------

/// What a target holds of the hand-off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub private_remotes: Vec<String>,
    pub allowed: bool,
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
    /// Targets whose block a person removed by hand, with the workspace
    /// ledger's capture then: not re-added while it stays (design 6.5).
    pub retired: Vec<(Vec<u8>, [u8; 32])>,
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
}

impl landed::Record for HandoffLanded {
    const MAGIC: &'static [u8; 16] = LANDED_MAGIC;
}

impl landed::Record for HandoffIntent {
    const MAGIC: &'static [u8; 16] = INTENT_MAGIC;
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
    if record.is_none() && intent.is_none() {
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
                        .filter(|print| same_fs && print.mtime_ns < racy_from),
                },
                file.base.as_ref(),
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
                },
                None,
            );
        }
    }
    Ok(sides)
}

/// The length of the prefix of `prefixes` that `file` starts with (0 for
/// none), its position left at that length. Reads at most the longest
/// prefix.
pub(super) fn skip_prefix(file: &mut fs::File, prefixes: &[Prefix]) -> Result<u64> {
    const SITE: &str = "git_carry::converge::handoff::skip_prefix";
    let longest = prefixes.iter().map(|prefix| prefix.len).max().unwrap_or(0);
    let mut head = Vec::new();
    std::io::Read::by_ref(file)
        .take(longest)
        .read_to_end(&mut head)
        .refuse_at(SITE)?;
    let found = prefixes
        .iter()
        .find(|prefix| {
            usize::try_from(prefix.len)
                .ok()
                .and_then(|len| head.get(..len))
                .is_some_and(|bytes| digest(bytes) == prefix.digest)
        })
        .map_or(0, |prefix| prefix.len);
    std::io::Seek::seek(file, std::io::SeekFrom::Start(found)).refuse_at(SITE)?;
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
    /// A block at byte 0 rendered for another landing (its `for=` is not
    /// this landing's, or it names none): carried with the source's bytes,
    /// so it is content, handled as `Plain` with the block kept below
    /// (OI-1003-Q185).
    Foreign,
    /// A marker at byte 0 for this landing that is not the record's: an
    /// edit inside the block, a BEGIN with no END, a block with no record.
    Modified,
}

/// What the head of a file holds that no record vouches for: nothing, a
/// block (or a broken one) this landing's identity names, or another's.
fn unrecorded(head: &[u8], ours: &str) -> Held {
    match render::marker(head) {
        render::Marker::None => Held::Plain,
        _ if render::owner_of(head) == Some(ours.as_bytes()) => Held::Modified,
        _ => Held::Foreign,
    }
}

/// Whether `record`'s print of `file` vouches for its bytes now: the same
/// file system, the same print, and stamped before the racy window of the
/// record's `recorded_ns` (as `prove` trusts the workspace ledger's print).
fn trusted(record: &HandoffLanded, file: &HandoffFile, now: Option<&SeatPrint>, fsid: u64) -> bool {
    let racy_from = record
        .recorded_ns
        .saturating_sub(super::super::RACY_GRANULARITY_NS);
    record.fsid == fsid
        && file
            .print
            .as_ref()
            .is_some_and(|print| now == Some(print) && print.mtime_ns < racy_from)
}

/// What `path` (named `name`) holds against `record`: by print when the
/// record's print is unchanged and not racy (no read), else by its head.
/// `ours` is this landing's identity ([`render::owner`]).
fn held(
    path: &Path,
    name: &str,
    meta: &fs::Metadata,
    record: Option<&HandoffLanded>,
    fsid: u64,
    ours: &str,
) -> Result<Held> {
    let entry = record.and_then(|record| {
        record
            .files
            .iter()
            .find(|file| {
                file.path == name.as_bytes() && !matches!(file.kind, FileKind::ViaSymlink(_))
            })
            .map(|file| (record, file))
    });
    let now = SeatPrint::of(name.as_bytes(), meta);
    if let Some((record, file)) = entry {
        if trusted(record, file, now.as_ref(), fsid) {
            return Ok(Held::Intact(file.clone()));
        }
    }
    let head = head_of_file(path)?;
    Ok(match (render::marker(&head), entry) {
        (render::Marker::Block(len), Some((_, file)))
            if file.block_len == len64(head.get(..len).unwrap_or_default())
                && digest(head.get(..len).unwrap_or_default()) == file.block =>
        {
            Held::Intact(file.clone())
        }
        _ => unrecorded(&head, ours),
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

/// What a step expects a target to start with.
#[derive(Debug, Clone, Copy)]
enum Expect<'a> {
    /// The record's block.
    Block(&'a HandoffFile),
    /// No block of this landing's (`ours`): nothing, or a carried one.
    Unowned(&'a str),
}

/// `path` read whole, starting as `expect` says: what the classification
/// found, held still to the read the exchange is checked against. Anything
/// else is a writer between the two reads: `HANDOFF_BLOCK_MODIFIED`.
fn read_as(path: &Path, name: &str, expect: Expect<'_>) -> Result<Whole> {
    let read = read_whole(path, name)?;
    let still = match expect {
        Expect::Block(file) => usize::try_from(file.block_len)
            .ok()
            .and_then(|len| read.bytes.get(..len))
            .is_some_and(|block| digest(block) == file.block),
        Expect::Unowned(ours) => {
            matches!(unrecorded(&read.bytes, ours), Held::Plain | Held::Foreign)
        }
    };
    if still {
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
    let read = match read_as(&worktree.join(name), name, Expect::Block(file)) {
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
        clear(worktree, dir, ours)?;
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
        base.recorded_ns = super::super::pass_start_ns();
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

// ---- privacy -------------------------------------------------------------------

/// Whether `value` starts with `prefix` at a boundary: the prefix ends in
/// `/` or `:`, or the value's next byte is one, or nothing follows.
fn under(value: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && value.strip_prefix(prefix).is_some_and(|rest| {
            prefix.ends_with(['/', ':']) || rest.is_empty() || rest.starts_with(['/', ':'])
        })
}

/// What one configuration source says that the guard reads: remote URL
/// and push URL values, and URL rewrite rules (`(base, prefix, push)`).
#[derive(Debug, Default)]
struct Remotes {
    urls: Vec<String>,
    rules: Vec<(String, String, bool)>,
}

/// Read one configuration source: the repository's own (local and worktree,
/// `file` `None`) or one file by path, with its includes.
fn remotes_in(worktree: &Path, file: Option<&Path>, into: &mut Remotes) -> Result<()> {
    const KEYS: &str = r"^(remote\..*\.(url|pushurl)|url\..*\.(insteadof|pushinsteadof))$";
    let listed = file
        .map_or_else(
            || {
                git(worktree)
                    .args(["config", "--null", "--get-regexp", KEYS])
                    .output()
            },
            |file| {
                git(worktree)
                    .args(["config", "--null", "--includes", "--file"])
                    .arg(file)
                    .args(["--get-regexp", KEYS])
                    .output()
            },
        )
        .refuse_at("git_carry::converge::handoff::remotes_in")?;
    match listed.status.code() {
        Some(1) => return Ok(()),
        Some(0) => {}
        _ => return Err(super::super::estimate::child_failed(&listed.stderr)),
    }
    for entry in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|e| !e.is_empty())
    {
        let (key, value) =
            entry
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or((entry, &b""[..]), |at| {
                    (
                        entry.get(..at).unwrap_or_default(),
                        entry.get(at + 1..).unwrap_or_default(),
                    )
                });
        let key = String::from_utf8_lossy(key);
        let value = String::from_utf8_lossy(value).into_owned();
        if key.starts_with("remote.") {
            into.urls.push(value);
        } else if let Some(base) = key.strip_prefix("url.") {
            if let Some(base) = base.strip_suffix(".pushinsteadof") {
                into.rules.push((base.to_owned(), value, true));
            } else if let Some(base) = base.strip_suffix(".insteadof") {
                into.rules.push((base.to_owned(), value, false));
            }
        }
    }
    Ok(())
}

/// The configuration files a `git push` from the destination reads beyond
/// the repository's own, located as Git locates them: the user's global
/// file(s) and the system file. Bulkload's own Git children never read
/// them (`git_env`), so the guard names them by path.
fn user_config_files() -> Vec<PathBuf> {
    let [global, global_sys, no_global_sys] = super::super::git_env::USER_CONFIG;
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let mut files = Vec::new();
    if let Some(global) = var(global) {
        files.push(PathBuf::from(global));
    } else {
        let home = var("HOME").map(PathBuf::from);
        let xdg = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".config")));
        files.extend(xdg.map(|xdg| xdg.join("git/config")));
        files.extend(home.map(|home| home.join(".gitconfig")));
    }
    let skip = var(no_global_sys).is_some_and(|value| value != "0" && value != "false");
    if !skip {
        files.push(var(global_sys).map_or_else(|| PathBuf::from("/etc/gitconfig"), PathBuf::from));
    }
    files
}

/// `url` as Git rewrites it with the longest matching rule of one kind
/// (`push`: `pushInsteadOf`, else `insteadOf`), or `None` when none
/// matches.
fn rewritten(url: &str, rules: &[(String, String, bool)], push: bool) -> Option<String> {
    rules
        .iter()
        .filter(|(_, prefix, kind)| {
            *kind == push && !prefix.is_empty() && url.starts_with(prefix.as_str())
        })
        .max_by_key(|(_, prefix, _)| prefix.len())
        .map(|(base, prefix, _)| format!("{base}{}", url.get(prefix.len()..).unwrap_or_default()))
}

/// The privacy guard (design 9.1, as amended by the review of 2026-10-16):
/// `allowed` (the operator named the workspace), `no-remote`, or
/// `private-remote`. The repository's remotes are every `remote.*.url` and
/// `remote.*.pushurl` value in its own configuration and in the source's
/// configuration a landing preserved inert (`carry-config/source-config*`
/// in the common directory: an origin kept out of the active configuration
/// is still the repository's remote, and a later `--activate-origin` or a
/// hand-added remote restores it). Each value is checked as written and as
/// Git rewrites it (`url.<base>.insteadOf` and `pushInsteadOf`, from the
/// repository's, the preserved, the user's global and the system
/// configuration): every form must start with a `--private-remote`
/// prefix. No URL is ever returned or printed.
///
/// # Errors
/// `HANDOFF_NOT_PRIVATE`; a Git child failure.
fn privacy(
    worktree: &Path,
    common: &Path,
    prefixes: &[String],
    allowed: bool,
) -> Result<&'static str> {
    if allowed {
        return Ok("allowed");
    }
    let mut found = Remotes::default();
    remotes_in(worktree, None, &mut found)?;
    let preserved = common.join("carry-config");
    for name in ["source-config", "source-config.worktree"] {
        let file = preserved.join(name);
        if lstat(&file)?.is_some_and(|meta| meta.is_file()) {
            remotes_in(worktree, Some(&file), &mut found)?;
        }
    }
    let mut rules = Remotes::default();
    for file in user_config_files() {
        if lstat(&file)?.is_some_and(|meta| meta.is_file()) {
            remotes_in(worktree, Some(&file), &mut rules)?;
        }
    }
    found.rules.extend(rules.rules);
    if found.urls.is_empty() {
        return Ok("no-remote");
    }
    for url in &found.urls {
        let forms = [
            Some(url.clone()),
            rewritten(url, &found.rules, false),
            rewritten(url, &found.rules, true),
        ];
        for form in forms.iter().flatten() {
            if !prefixes.iter().any(|prefix| under(form, prefix)) {
                return Err(BulkloadRefusal::HandoffNotPrivate);
            }
        }
    }
    Ok("private-remote")
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

/// The run's options.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub mode: Mode,
    pub readme: bool,
    pub renew: bool,
    pub private_remotes: &'a [String],
    /// Canonical workspaces the operator vouches for (`--allow-workspace`).
    pub allow: &'a [PathBuf],
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
    pub private: Option<&'static str>,
    pub files: Vec<FileRow>,
    pub inputs: Option<[u8; 32]>,
    pub open: Option<u64>,
    pub block: Option<Vec<u8>>,
    /// `--check`: `HANDOFF_MARKER_COMMITTED` when the token is staged or
    /// committed.
    pub refusal: Option<BulkloadRefusal>,
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
        members.push(Member {
            path,
            main: index == 0,
            item,
            ledger,
            now,
            changed,
        });
    }
    let refs = super::super::output(
        git(common).args(["for-each-ref", "--format=%(objectname) %(refname)"]),
    )?;
    Ok(Census { members, refs })
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
        let consolidated = match &state {
            SiblingState::Head { oid, .. } => {
                held_by_branch(worktree, oid, None)
                    || now_of(worktree)
                        .ok()
                        .flatten()
                        .is_some_and(|(_, here)| in_history(&path, &here, oid))
            }
            _ => false,
        };
        return Ok(Some(Sibling {
            path: path.into_os_string().into_vec(),
            state,
            consolidated,
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
/// prints all keep a render's `--sibling`, `--private-remote`,
/// `--allow-workspace` and `--readme`). To drop one, clear and render
/// again.
#[derive(Debug, Clone)]
struct Effective {
    siblings: Vec<(PathBuf, PathBuf)>,
    private_remotes: Vec<String>,
    allowed: bool,
    readme: bool,
}

fn effective(request: &Request<'_>, record: Option<&HandoffLanded>, this: &Path) -> Effective {
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
        private_remotes: if request.private_remotes.is_empty() {
            kept.map(|kept| kept.private_remotes.clone())
                .unwrap_or_default()
        } else {
            request.private_remotes.to_vec()
        },
        allowed: request.allow.iter().any(|allowed| allowed == this)
            || kept.is_some_and(|kept| kept.allowed),
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
        private_remotes: options.private_remotes.clone(),
        allowed: options.allowed,
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
/// writes over a target.
struct Landed<'a> {
    source: &'a str,
    instance: &'a str,
    sides: Option<BTreeMap<Vec<u8>, Side>>,
}

/// The landed blob of target `name` when `remainder` (the bytes a render
/// keeps under its block) is exactly it, else `None`.
fn landed_base(
    worktree: &Path,
    landed: &mut Landed<'_>,
    name: &str,
    remainder: &[u8],
) -> Result<Option<String>> {
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
    // The landed blob's bytes, as the object store holds them, compared
    // with the remainder (a read through the existing batch reader; no
    // object is written).
    let mut objects = super::super::batch_objects::BatchObjects::new(worktree)?;
    let mut same = SameBytes {
        expected: remainder,
        at: 0,
        equal: true,
    };
    let length = objects.copy_into(&blob, &mut same, None);
    objects.finish()?;
    length?;
    Ok((same.equal && same.at == remainder.len()).then_some(blob))
}

/// A writer that checks it is handed exactly `expected`, in order.
struct SameBytes<'a> {
    expected: &'a [u8],
    at: usize,
    equal: bool,
}

impl std::io::Write for SameBytes<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self.at.saturating_add(bytes.len());
        self.equal &= self.expected.get(self.at..end) == Some(bytes);
        self.at = end;
        Ok(bytes.len())
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
/// `HANDOFF_NOT_LANDED`, `HANDOFF_NOT_PRIVATE`, `HANDOFF_TARGET_UNSUPPORTED`,
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
            if settle(workspace.worktree, &place.dir, estate.source, &ours)?.cleared {
                return Ok(Row {
                    outcome: "cleared",
                    ..Row::default()
                });
            }
            clear(workspace.worktree, &place.dir, &ours)
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
    Replace(Whole, u64),
    Create,
}

/// Whether a target's bytes (under this landing's block, when it has one)
/// start with a block another landing rendered.
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
    let worktree = workspace.worktree;
    let this = canonical(worktree);
    let record_path = place.dir.join(HANDOFF_LANDED);
    let record = landed::read::<HandoffLanded>(&record_path)?;
    let options = effective(request, record.as_ref(), &this);
    let private = privacy(
        worktree,
        &place.common,
        &options.private_remotes,
        options.allowed,
    )?;
    let names = names_for(worktree, options.readme, record.as_ref())?;
    let found = classify(worktree, &names)?;
    let fsid = landed::fsid(worktree)?;
    let stored = stored(estate, &options);
    let census = census_of(session, place, workspace, estate)?.clone();
    let sibling = sibling_of(&this, &options.siblings)?;
    let inputs = inputs_digest(workspace, estate, &stored, &census, sibling.as_ref())?;
    // What each target holds (read only when its print moved).
    let mut held_by: Vec<(&'static str, Option<Held>)> = Vec::new();
    for (name, entry) in &found {
        let state = match entry {
            Found::Regular(meta) => {
                let state = held(
                    &worktree.join(name),
                    name,
                    meta,
                    record.as_ref(),
                    fsid,
                    ours,
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
            record
                .retired
                .iter()
                .any(|(path, capture)| path == name.as_bytes() && *capture == place.ledger.capture)
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
    let mut landed = Landed {
        source: estate.source,
        instance: &place.ledger.instance,
        sides: None,
    };
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
                        .is_none_or(|print| print.mtime_ns >= racy_from)
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
            private: Some(private),
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
                    Plan::Replace(read_as(&path, name, Expect::Block(file))?, file.block_len)
                } else if file.print == SeatPrint::of(name.as_bytes(), meta) {
                    Plan::Keep(Some(file.clone()), "unchanged")
                } else {
                    // The block is kept but the file moved (an edit below
                    // it, a touch): what the record says of the bytes under
                    // the block is checked again before its print is
                    // re-taken (review of 2026-10-16, the BLOCKER).
                    let checked = retaken(worktree, &mut landed, file)?
                        .ok_or(BulkloadRefusal::HandoffBlockModified)?;
                    Plan::Keep(Some(checked), "unchanged")
                }
            }
            (Found::Regular(_), _) => {
                if renewable(name) {
                    Plan::Retire("removed-by-hand")
                } else {
                    Plan::Prepend(read_as(&path, name, Expect::Unowned(ours))?)
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
            Plan::Replace(read, old_len) => {
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
    let mut retired_now: Vec<(Vec<u8>, [u8; 32])> = record
        .iter()
        .flat_map(|record| record.retired.clone())
        .filter(|(_, capture)| *capture == place.ledger.capture && !request.renew)
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
                if !retired_now.iter().any(|(retired, _)| *retired == path) {
                    retired_now.push((path.clone(), place.ledger.capture));
                }
                rows.push(FileRow {
                    path,
                    kind: recorded(name).map_or("prefixed", |file| file.kind.name()),
                    action,
                });
                continue;
            }
            Plan::Prepend(_) | Plan::Replace(..) => (FileKind::Prefixed, "written"),
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
            Plan::Replace(read, old_len) => {
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
            private: Some(private),
            files: rows,
            inputs: Some(inputs),
            open: Some(open),
            block: Some(block),
            ..Row::default()
        });
    }
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
                Plan::Prepend(read) | Plan::Replace(read, _) => {
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
    // bytes were checked at). Each is racy against `recorded_ns` (the
    // pass's start), so converge trusts none until a rerun re-dates it
    // after checking the bytes again.
    for file in &mut next.files {
        if matches!(file.kind, FileKind::ViaSymlink(_)) || file.print.is_some() {
            continue;
        }
        file.print = lstat(&worktree.join(OsStr::from_bytes(&file.path)))?
            .and_then(|meta| SeatPrint::of(&file.path, &meta));
    }
    next.recorded_ns = super::super::pass_start_ns();
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
        private: Some(private),
        files: rows,
        inputs: Some(inputs),
        open: Some(open),
        ..Row::default()
    })
}

/// Clear one workspace (design 7): each intact recorded block stripped (a
/// created file that is the block alone removed), then the record and the
/// intent removed. A marker of this landing's that is not the record's
/// refuses `HANDOFF_BLOCK_MODIFIED` with nothing written; a block carried
/// from another landing is content and stays (OI-1003-Q185). A refusal
/// after the intent keeps it, so the next run of either verb finishes the
/// clear ([`settle`]).
#[allow(clippy::too_many_lines)]
fn clear(worktree: &Path, dir: &Path, ours: &str) -> Result<Row> {
    let record_path = dir.join(HANDOFF_LANDED);
    let intent_path = dir.join(HANDOFF_INTENT);
    let record = landed::read::<HandoffLanded>(&record_path)?;
    let pending = landed::exists(&intent_path)?;
    let fsid = landed::fsid(worktree)?;
    let mut work: Vec<(&'static str, Whole, HandoffFile)> = Vec::new();
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
        match held(&path, name, &meta, record.as_ref(), fsid, ours)? {
            Held::Plain | Held::Foreign => {}
            Held::Modified => return Err(BulkloadRefusal::HandoffBlockModified),
            Held::Intact(file) => {
                let read = read_as(&path, name, Expect::Block(&file))?;
                work.push((name, read, file));
            }
        }
    }
    if work.is_empty() && record.is_none() && !pending {
        return Ok(Row {
            outcome: "unchanged",
            ..Row::default()
        });
    }
    let tag = tag();
    let mut steps = Vec::new();
    for (name, read, file) in &work {
        let cut = usize::try_from(file.block_len).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        let rest = read.bytes.get(cut..).unwrap_or_default();
        let gone = file.kind == FileKind::Created && rest.is_empty();
        steps.push(Step {
            path: name.as_bytes().to_vec(),
            old_block: read.bytes.get(..cut).map(<[u8]>::to_vec),
            new_block: None,
            before: Some(digest(&read.bytes)),
            after: (!gone).then(|| digest(rest)),
        });
    }
    let intent = HandoffIntent {
        op: Op::Clear,
        tag: tag.clone(),
        steps: steps.clone(),
        next: None,
    };
    if !steps.is_empty() {
        landed::write(&intent_path, &intent)?;
        fault_point!(EstateHandoffAfterIntent);
    }
    for (n, ((name, read, file), step)) in work.iter().zip(&steps).enumerate() {
        let temp = temp_name(&tag, n);
        let cut = usize::try_from(file.block_len).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        let outcome = if step.after.is_none() {
            remove(worktree, name, read, &temp)
        } else {
            exchange(
                worktree,
                name,
                read,
                read.bytes.get(cut..).unwrap_or_default(),
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
            path: name.as_bytes().to_vec(),
            kind: file.kind.name(),
            action: "cleared",
        });
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

/// Whether the blob of `name` at `rev` (`:` for the index) holds the token.
fn blob_has_token(worktree: &Path, rev: &str, name: &str) -> Result<bool> {
    let spec = if rev.is_empty() {
        format!(":{name}")
    } else {
        format!("{rev}:{name}")
    };
    let out = git(worktree)
        .args(["cat-file", "blob", &spec])
        .output()
        .refuse_at("git_carry::converge::handoff::blob")?;
    Ok(out.status.success()
        && out
            .stdout
            .windows(TOKEN.len())
            .any(|window| window == TOKEN.as_bytes()))
}

/// Whether a branch's or tag's history (or a detached HEAD's) holds a
/// commit that put the token into one of `names`: a block committed and
/// removed again is still published by a push (review of 2026-10-16). The
/// carried capture refs (`refs/carry/`) are not pushed and are not read.
fn history_has_token(worktree: &Path, names: &[&str]) -> Result<bool> {
    let mut command = git(worktree);
    command.args([
        "log",
        "--branches",
        "--tags",
        "--no-ext-diff",
        "--no-textconv",
        "--format=%H",
        "-1",
        "-S",
        TOKEN,
    ]);
    if now_of(worktree)?.is_some() {
        command.arg("HEAD");
    }
    command.arg("--").args(names);
    let out = command
        .output()
        .refuse_at("git_carry::converge::handoff::history")?;
    Ok(out.status.success() && out.stdout.iter().any(|byte| !byte.is_ascii_whitespace()))
}

/// `--check` (read-only at the destination and in STATE): the hand-off's
/// state, its open rows, and whether its token is staged or committed
/// (the index, HEAD, every listed tip, and the history of every branch and
/// tag).
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
    let options = effective(request, record.as_ref(), &this);
    let names = names_for(worktree, options.readme, record.as_ref())?;
    let found = classify(worktree, &names)?;
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
                    &worktree.join(name),
                    name,
                    meta,
                    record.as_ref(),
                    fsid,
                    ours,
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
            kind: recorded.map_or("absent", |file| file.kind.name()),
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
    // Commit detection: the index blob, the HEAD blob, each destination-
    // ahead, destination-only or diverged tip's blob, and the history of
    // every branch and tag.
    let mut staged = false;
    let mut committed = false;
    for name in &names {
        staged |= blob_has_token(worktree, "", name)?;
        committed |= blob_has_token(worktree, "HEAD", name)?;
        for row in facts
            .refs
            .iter()
            .filter(|row| matches!(row.class.as_str(), "dest-ahead" | "dest-only" | "diverged"))
        {
            committed |= blob_has_token(worktree, &row.name, name)?;
        }
    }
    committed = committed || history_has_token(worktree, &names)?;
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
        ..Row::default()
    })
}

#[cfg(test)]
mod tests;
