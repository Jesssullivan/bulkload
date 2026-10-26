//! The streamed capture of an estate item (SPEC rule 2, reset 2026-10-20;
//! coordinator decision (a); B1: an absent destination).
//!
//! Nothing is staged on either machine. The source half records one small
//! ledger per item, its [`ItemSnapshot`] (`carry/ITEM.snapshot`): the
//! repository's object ids to carry ([`stream::Snapshot`]), the paths the
//! seat transfer leaves out, and the few administration files whose
//! contents name host paths. The destination half lands the item at its
//! live path ([`land`]):
//!
//! 1. the objects, fetched by id from the LIVE source repository straight
//!    into the live repository's object store ([`stream::fetch`]), batch by
//!    batch, each batch a resume mark;
//! 2. the seats (the worktree's files, tracked, dirty, untracked and
//!    ignored, and the administration's own files: refs, reflogs, HEAD,
//!    the index, hooks, `info/`), through the transfer engine, straight into
//!    the live path, with its racy custody and R25 reuse; the object store,
//!    the rebuildable directories and the nested repositories excluded;
//! 3. the host-path files (the configuration, the worktrees' `gitdir` and
//!    `.git` pointers), written with every planned source path mapped to
//!    its live path;
//! 4. any object the landed administration names that the store still
//!    lacks (the source moved after its snapshot), fetched by id;
//! 5. the proof: no object the landed repository names is missing, and no
//!    quarantine is left in its store.
//!
//! Only the repository's own objects enter its store: no custody refs, no
//! worktree blobs.

use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{id, read, write_bytes, Item};
use crate::git_carry::stream;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "estate::streamed";

/// The administration's configuration file, a host-path file.
const CONFIG: &str = "config";

/// One item's streamed capture: what the source half records in `carry/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemSnapshot {
    /// The planned item.
    pub item: Item,
    /// The repository's objects to carry.
    pub snapshot: stream::Snapshot,
    /// Paths under the item's source path the seat transfer leaves out
    /// (relative): the object store, rebuildable directories, nested
    /// repositories.
    pub excluded: Vec<Vec<u8>>,
    /// Host-path files under the item's source path (relative), with their
    /// bytes as read. The exclude is metadata for proving a hand-off's
    /// own appended policy. Other entries are written with mapped paths.
    pub host_files: Vec<(Vec<u8>, Vec<u8>)>,
    /// When the pass that read it started (Unix nanoseconds).
    pub started_ns: i128,
    /// The stash, reflogs and roots a consolidation unions (B2).
    pub history: stream::History,
    /// The item's index, as read: written at its live index by a landing
    /// and compared by entries on later passes (never by the engine).
    pub index: Option<Vec<u8>>,
    /// The item's checked-out branch (`refs/heads/NAME`; `None` detached)
    /// and HEAD's commit: what a consolidation switches to when clean.
    pub head_branch: Option<String>,
    pub head: String,
    /// A repository whose administration lives outside its worktree and is
    /// no linked worktree's (a submodule's `.git/modules/NAME`, a
    /// `--separate-git-dir`): its path on the source, landed (mapped) as
    /// this item's own, its objects fetched into it.
    pub separate_admin: Option<PathBuf>,
    /// The common repository's explicit shallow boundary, sorted and
    /// validated. Settled separately from the seat transfer, never copied
    /// over a destination's richer history.
    pub shallow: Vec<u8>,
}

// The exact postcard layout before shallow support. Old captures were all
// full repositories, so their frontier is empty. Keep their resume digests.
#[derive(Serialize, Deserialize)]
struct LegacyItemSnapshot {
    item: Item,
    snapshot: stream::Snapshot,
    excluded: Vec<Vec<u8>>,
    host_files: Vec<(Vec<u8>, Vec<u8>)>,
    started_ns: i128,
    history: stream::History,
    index: Option<Vec<u8>>,
    head_branch: Option<String>,
    head: String,
    separate_admin: Option<PathBuf>,
}

impl From<LegacyItemSnapshot> for ItemSnapshot {
    fn from(old: LegacyItemSnapshot) -> Self {
        Self {
            item: old.item,
            snapshot: old.snapshot,
            excluded: old.excluded,
            host_files: old.host_files,
            started_ns: old.started_ns,
            history: old.history,
            index: old.index,
            head_branch: old.head_branch,
            head: old.head,
            separate_admin: old.separate_admin,
            shallow: Vec::new(),
        }
    }
}

const SNAPSHOT_V2: &[u8] = b"bulkload.item.snapshot.v2\0";

impl ItemSnapshot {
    /// The digest a landing's journal is bound to.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key("bulkload 2026-10-20 item snapshot v1");
        hasher.update(&self.snapshot.digest());
        for (path, bytes) in &self.host_files {
            hasher.update(&(path.len() as u64).to_le_bytes());
            hasher.update(path);
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        if !self.shallow.is_empty() {
            hasher.update(b"shallow\0");
            hasher.update(&self.shallow);
        }
        *hasher.finalize().as_bytes()
    }
}

/// `carry/ITEM.snapshot`.
#[must_use]
pub fn snapshot_path(carry: &Path, identity: &str) -> PathBuf {
    carry.join(format!("{identity}.snapshot"))
}

/// The resume marks of an item's object fetch, in the apply's private
/// state.
#[must_use]
pub fn marks_path(state: &Path, identity: &str) -> PathBuf {
    state.join(format!("{identity}.objects"))
}

fn rel_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

// Old full-source snapshots predate the shallow exclusion. Apply the
// exclusion at landing too, without changing their persisted digests.
fn seat_exclusions(snapshot: &ItemSnapshot) -> Vec<Vec<u8>> {
    let mut excluded = snapshot.excluded.clone();
    for path in &snapshot.excluded {
        if let Some(admin) = path.strip_suffix(b"objects") {
            if admin.is_empty() || admin.ends_with(b"/") {
                let mut path = admin.to_vec();
                path.extend_from_slice(b"shallow");
                excluded.push(path);
            }
        }
    }
    excluded.sort();
    excluded.dedup();
    excluded
}

fn is_exclude(path: &[u8]) -> bool {
    path == b"info/exclude" || path.ends_with(b"/info/exclude")
}

/// Read `item`'s streamed capture on the source.
///
/// Partial clones refuse before a lazy fetch can read them. Shallow
/// repositories carry their explicit boundary, never invented parents.
///
/// # Errors
/// As above, and the snapshot's refusals.
pub fn capture_source(
    item: &Item,
    started_ns: i128,
    include_rebuildable: bool,
) -> Result<ItemSnapshot> {
    let source = &item.source;
    crate::git_carry::refuse_partial_clone(source)?;
    let common = crate::git_carry::common_repository(source)?;
    let shallow = stream::shallow_frontier(source)?;
    // A store that borrows objects through `objects/info/alternates` is
    // carried whole: the fetch by id is served from the source's alternates
    // too, so the live store holds every object itself (2026-10-22).
    let mut snapshot = stream::snapshot(source)?;
    // The frontier itself names commits, even when no remaining ref does.
    // Carry those objects before publishing its administration.
    snapshot.wants.extend(
        std::str::from_utf8(&shallow)
            .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
            .lines()
            .map(str::to_owned),
    );
    snapshot.wants.sort();
    snapshot.wants.dedup();
    let mut excluded = Vec::new();
    let mut host_files = Vec::new();
    // The administration directory under this item's path: the main
    // worktree's (or a bare repository's own) common directory, never a
    // linked worktree's, whose administration the main carries.
    let admin = if common == fs::canonicalize(source).refuse_at(SITE)? {
        Some(PathBuf::new())
    } else {
        common
            .strip_prefix(fs::canonicalize(source).refuse_at(SITE)?)
            .ok()
            .map(Path::to_path_buf)
    };
    // A submodule (or `--separate-git-dir`): its own administration, outside
    // its worktree, is its git directory and common directory both.
    let git_dir = fs::canonicalize(crate::git_carry::absolute_git_dir(source)?).refuse_at(SITE)?;
    let separate_admin = (admin.is_none() && git_dir == common).then(|| common.clone());
    match &admin {
        Some(admin) => {
            excluded.push(rel_bytes(&admin.join("objects")));
            // Never copy a frontier that appeared after this snapshot.
            // Full captures need this exclusion too.
            excluded.push(rel_bytes(&admin.join("shallow")));
            // Submodules' administrations are their own items'.
            excluded.push(rel_bytes(&admin.join("modules")));
            let config = admin.join(CONFIG);
            host_files.push((rel_bytes(&config), read_file(&source.join(&config))?));
            capture_exclude(source, admin, &mut host_files)?;
            if let Ok(read) = fs::read_dir(common.join("worktrees")) {
                for entry in read {
                    let entry = entry.refuse_at(SITE)?;
                    let rel = admin
                        .join("worktrees")
                        .join(entry.file_name())
                        .join("gitdir");
                    if let Ok(bytes) = fs::read(source.join(&rel)) {
                        host_files.push((rel_bytes(&rel), bytes));
                    }
                }
            }
        }
        // A linked worktree: its `.git` pointer names the main's
        // administration by host path.
        None => {
            host_files.push((b".git".to_vec(), read_file(&source.join(".git"))?));
        }
    }
    // Nests (each its own item) and rebuildable roots, by one plain walk:
    // nothing about a nest refuses this item (2026-10-22).
    let (nests, rebuildable) = crate::git_carry::streamed_omissions(source, include_rebuildable)?;
    excluded.extend(nests);
    excluded.extend(rebuildable);
    for (rel, _) in &host_files {
        if !is_exclude(rel) {
            excluded.push(rel.clone());
        }
    }
    excluded.sort();
    excluded.dedup();
    // The item's own index (a linked worktree's lives in the main's
    // administration): carried and settled apart from the seats.
    let (head_branch, head) = crate::git_carry::union::head(source)?;
    let index_path = PathBuf::from(crate::git_carry::git_path(source, "index")?);
    let index = match fs::read(&index_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    // Every index under this item's administration is settled by its own
    // item: the main's `index` and each linked worktree's.
    if let Some(admin) = &admin {
        excluded.push(rel_bytes(&admin.join("index")));
        if let Ok(read) = fs::read_dir(common.join("worktrees")) {
            for entry in read {
                let entry = entry.refuse_at(SITE)?;
                excluded.push(rel_bytes(
                    &admin
                        .join("worktrees")
                        .join(entry.file_name())
                        .join("index"),
                ));
            }
        }
        excluded.sort();
        excluded.dedup();
    }
    let history = stream::history(source)?;
    if stream::shallow_frontier(source)? != shallow {
        return Err(BulkloadRefusal::CaptureDrifted);
    }
    Ok(ItemSnapshot {
        item: item.clone(),
        snapshot,
        excluded,
        host_files,
        started_ns,
        history,
        index,
        head_branch,
        head,
        separate_admin,
        shallow,
    })
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).refuse_at(SITE)
}

fn capture_exclude(
    source: &Path,
    admin: &Path,
    host_files: &mut Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<()> {
    let exclude = admin.join("info/exclude");
    let path = source.join(&exclude);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
        if let Some(bytes) = read_regular_exclude(&path)? {
            host_files.push((rel_bytes(&exclude), bytes));
        }
    }
    Ok(())
}

/// Record `snapshot` at `carry/ITEM.snapshot`, durably.
///
/// # Errors
/// IO; `FRAME_CODEC`.
pub fn record(carry: &Path, snapshot: &ItemSnapshot) -> Result<()> {
    let mut bytes = SNAPSHOT_V2.to_vec();
    bytes.extend(postcard::to_allocvec(snapshot).map_err(|_| BulkloadRefusal::FrameCodec)?);
    write_bytes(&snapshot_path(carry, &id(&snapshot.item)?), &bytes)
}

/// The item's recorded streamed capture, if the source half recorded one.
///
/// # Errors
/// An unreadable record.
pub fn recorded(carry: &Path, identity: &str) -> Result<Option<ItemSnapshot>> {
    let path = snapshot_path(carry, identity);
    if !path.try_exists().refuse_at(SITE)? {
        return Ok(None);
    }
    let bytes = fs::read(&path).refuse_at(SITE)?;
    bytes.strip_prefix(SNAPSHOT_V2).map_or_else(
        || {
            postcard::from_bytes::<LegacyItemSnapshot>(&bytes)
                .map(|old| Some(old.into()))
                .map_err(|_| BulkloadRefusal::FrameCodec)
        },
        |current| {
            postcard::from_bytes(current)
                .map(Some)
                .map_err(|_| BulkloadRefusal::FrameCodec)
        },
    )
}

/// Every `(source, live)` path pair of the plan, longest source first: what
/// a host-path file's text is mapped by.
#[must_use]
pub fn path_map(items: &[Item]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = items
        .iter()
        .map(|item| {
            (
                rel_bytes(&item.source),
                rel_bytes(item.workspace.as_ref().unwrap_or(&item.repository)),
            )
        })
        .filter(|(from, to)| from != to)
        .collect();
    pairs.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    pairs.dedup();
    pairs
}

/// `bytes` with every occurrence of a mapped source path (followed by a
/// path end: `/`, a quote, whitespace or the end) replaced by its live
/// path, longest first.
#[must_use]
pub fn mapped(bytes: &[u8], map: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    'scan: while at < bytes.len() {
        for (from, to) in map {
            if bytes.get(at..at + from.len()) == Some(from.as_slice()) {
                let end = bytes.get(at + from.len());
                if end.is_none_or(|byte| matches!(byte, b'/' | b'"' | b'\n' | b' ' | b'\t' | b'\r'))
                {
                    out.extend_from_slice(to);
                    at += from.len();
                    continue 'scan;
                }
            }
        }
        if let Some(byte) = bytes.get(at) {
            out.push(*byte);
        }
        at += 1;
    }
    out
}

/// What a landing did, for its receipt.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Landed {
    /// Object batches fetched, and skipped by a mark.
    pub batches: u64,
    pub skipped: u64,
    /// Bytes the live object store grew by.
    pub object_bytes: u64,
    /// Objects the landed administration named that a top-up fetched.
    pub topped_up: u64,
    /// Ids the snapshot named that the source no longer held.
    pub missing_at_source: u64,
    /// Ids the live administration already named, and its store lacked,
    /// before this landing: the destination's own, never the source's
    /// drift, counted and never refused for ([`own_lacking`]).
    pub dest_lacking_own: u64,
    /// What the index settle did.
    pub index: &'static str,
}

/// How a landing reaches the source: its repository URL for `git fetch`
/// (the item's source path), the `upload-pack` program `git` runs there,
/// and the seat transfer.
pub struct Reach<'a> {
    pub upload_pack: &'a str,
    /// One recorded seat transfer of `from` (a source path) into `to`.
    pub transfer: &'a dyn Fn(&Path, &Path) -> Result<()>,
    /// Transient work files (an index read apart): the move's swept
    /// scratch directory.
    pub scratch: &'a Path,
    /// The consolidation a seat transfer into a live worktree that already
    /// holds its own files runs with (#297's unions).
    pub consolidation: &'a dyn Fn() -> Result<crate::transfer::Consolidation>,
}

/// Land `snapshot`'s item at its live path, absent or a landing of this
/// item's own (see the module docs). `state` is the apply's private state
/// (resume marks); `map` maps host paths.
///
/// # Errors
/// The fetch's, the transfer's and the proof's refusals: `CAPTURE_DRIFTED`
/// when the landed repository still names a missing object (the source
/// moved under the pass; the next pass completes it), `STATE_PAYLOAD_LEFT`
/// naming a quarantine left in the store.
pub fn land(
    snapshot: &ItemSnapshot,
    state: &Path,
    map: &[(Vec<u8>, Vec<u8>)],
    reach: &Reach<'_>,
    fetched: stream::Fetched,
) -> Result<Landed> {
    let provenance = id(&snapshot.item)?;
    land_with_identity(snapshot, state, map, reach, fetched, &provenance)
}

/// Land using the original plan item's identity for recorded hand-off
/// provenance. Home mapping changes paths, never that identity.
///
/// # Errors
/// As [`land`].
pub fn land_with_identity(
    snapshot: &ItemSnapshot,
    state: &Path,
    map: &[(Vec<u8>, Vec<u8>)],
    reach: &Reach<'_>,
    fetched: stream::Fetched,
    provenance: &str,
) -> Result<Landed> {
    let item = &snapshot.item;
    let identity = id(item)?;
    let live = item
        .workspace
        .clone()
        .unwrap_or_else(|| item.repository.clone());
    let mut landed = Landed {
        missing_at_source: snapshot.snapshot.missing,
        ..Landed::default()
    };
    let owns_store = store_of(snapshot, &live, map).is_some();
    verify_frontier(snapshot, map)?;
    landed.batches = fetched.batches;
    landed.skipped = fetched.skipped;
    landed.object_bytes = fetched.bytes;
    let own = own_lacking(&live)?;
    landed.dest_lacking_own = u64::try_from(own.len()).unwrap_or(u64::MAX);
    // 2a. A separate administration (a submodule's): its own files, its
    // objects (fetched) and index (settled) left out.
    if let (Some(from), Some(to)) = (&snapshot.separate_admin, admin_of(snapshot, map)) {
        fs::create_dir_all(&to).refuse_at(SITE)?;
        let root = fs::canonicalize(&to).refuse_at(SITE)?;
        let excluded = vec![b"objects".to_vec(), b"index".to_vec(), b"shallow".to_vec()];
        crate::transfer::set_excluded(&root, Some(excluded));
        let moved = (reach.transfer)(from, &to);
        crate::transfer::set_excluded(&root, None);
        moved?;
    }
    // 2. The seats and the administration's own files.
    let root = {
        fs::create_dir_all(&live).refuse_at(SITE)?;
        fs::canonicalize(&live).refuse_at(SITE)?
    };
    let mut excluded = seat_exclusions(snapshot);
    let preserved_exclude = preserved_handoff_exclude(snapshot, &live, provenance)?;
    if let Some(path) = &preserved_exclude {
        excluded.push(path.clone());
    }
    crate::transfer::set_excluded(&root, Some(excluded));
    let moved = (reach.transfer)(&item.source, &live);
    crate::transfer::set_excluded(&root, None);
    moved?;
    // 3. The host-path files, mapped.
    for (rel, bytes) in &snapshot.host_files {
        // The engine carries excludes normally. Only the proved hand-off
        // extension above keeps a longer live file, with every source byte.
        if is_exclude(rel) {
            continue;
        }
        let path = live.join(std::ffi::OsStr::from_bytes(rel));
        let want = mapped(bytes, map);
        match fs::read(&path) {
            Ok(held) if held == want => {}
            Ok(_) | Err(_) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).refuse_at(SITE)?;
                }
                write_bytes(&path, &want)?;
            }
        }
    }
    // The index, settled by its entries (SPEC rule 8).
    if item.workspace.is_some() {
        landed.index = settle_index(snapshot, &live, state, reach.scratch)?;
    }
    // 4. What the landed administration names and the store lacks.
    if owns_store || item.workspace.is_some() {
        let (ids, bytes) = top_up(
            snapshot,
            &live,
            &own,
            reach.upload_pack,
            &state.join(format!("{identity}.top-up.objects")),
        )?;
        landed.topped_up = ids;
        landed.object_bytes = landed.object_bytes.saturating_add(bytes);
        let objects = stream::objects_dir(&live)?;
        if let Some(left) = stream::own_quarantine_left(&objects)?.first() {
            return Err(BulkloadRefusal::StatePayloadLeft(rel_bytes(left)));
        }
    }
    if let Some(path) = &preserved_exclude {
        verify_preserved_exclude(snapshot, &live, path, provenance)?;
    }
    verify_frontier(snapshot, map)?;
    Ok(landed)
}

fn verify_preserved_exclude(
    snapshot: &ItemSnapshot,
    live: &Path,
    relative: &[u8],
    provenance: &str,
) -> Result<()> {
    if preserved_handoff_exclude(snapshot, live, provenance)?.is_some() {
        return Ok(());
    }
    let source = snapshot
        .host_files
        .iter()
        .find_map(|(path, bytes)| (path == relative).then_some(bytes))
        .ok_or(BulkloadRefusal::GitIgnorePolicyConflict)?;
    let path = live.join(std::ffi::OsStr::from_bytes(relative));
    let parent = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    if !fs::symlink_metadata(parent).is_ok_and(|meta| meta.is_dir())
        || read_regular_exclude(&path)?.as_deref() != Some(source.as_slice())
    {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(())
}

// An unchanged source exclude remains a prefix of the file to which our
// recorded hand-off appended its exact entry. Preserve that file, including
// any destination patterns, only with both the byte proof and provenance.
fn preserved_handoff_exclude(
    snapshot: &ItemSnapshot,
    live: &Path,
    provenance: &str,
) -> Result<Option<Vec<u8>>> {
    let Some((rel, source)) = snapshot
        .host_files
        .iter()
        .find(|(path, _)| is_exclude(path))
    else {
        return Ok(None);
    };
    let path = live.join(std::ffi::OsStr::from_bytes(rel));
    if !fs::symlink_metadata(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .is_ok_and(|meta| meta.is_dir())
    {
        return Ok(None);
    }
    let Some(held) = read_regular_exclude(&path)? else {
        return Ok(None);
    };
    if held == *source {
        return Ok(None);
    }
    if held.len() > 1024 * 1024
        || !held.starts_with(source)
        || !held.ends_with(b"# bulkload hand-off (never tracked)\n/CLAUDE.local.md\n")
        || !recorded_local_block(live, provenance)?
    {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(Some(rel.clone()))
}

fn read_regular_exclude(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(BulkloadRefusal::GitIgnorePolicyConflict);
        }
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let meta = file.metadata().refuse_at(SITE)?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .refuse_at(SITE)?;
    if bytes.len() > 1024 * 1024 {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(Some(bytes))
}

fn recorded_local_block(worktree: &Path, identity: &str) -> Result<bool> {
    use crate::git_carry::{converge::handoff, landed};
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let Some(admin) = landed::worktree_git_dir(worktree)? else {
        return Ok(false);
    };
    let root = admin.join(landed::LEDGER_ROOT);
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        if !entry.file_type().refuse_at(SITE)?.is_dir() {
            continue;
        }
        let Some(record) =
            landed::read::<handoff::HandoffLanded>(&entry.path().join(handoff::HANDOFF_LANDED))?
        else {
            continue;
        };
        if record.item != identity {
            continue;
        }
        for file in record
            .files
            .iter()
            .filter(|file| file.path == handoff::LOCAL.as_bytes())
        {
            if file.block_len > 16 * 1024 * 1024 {
                return Err(BulkloadRefusal::BudgetExceeded);
            }
            let held = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(worktree.join(handoff::LOCAL))
                .refuse_at(SITE)?;
            if !held.metadata().refuse_at(SITE)?.is_file() {
                return Err(BulkloadRefusal::GitIgnorePolicyConflict);
            }
            let mut block = Vec::new();
            held.take(file.block_len)
                .read_to_end(&mut block)
                .refuse_at(SITE)?;
            if u64::try_from(block.len()).ok() == Some(file.block_len)
                && blake3::hash(&block).as_bytes() == &file.block
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// The live object store an item owns (a main or bare repository's), from
// its excluded `objects` path.
fn store_of(snapshot: &ItemSnapshot, live: &Path, map: &[(Vec<u8>, Vec<u8>)]) -> Option<PathBuf> {
    if let Some(admin) = admin_of(snapshot, map) {
        return Some(admin.join("objects"));
    }
    snapshot
        .excluded
        .iter()
        .find(|path| path.ends_with(b"objects"))
        .map(|path| live.join(std::ffi::OsStr::from_bytes(path)))
}

// A separate administration's live path: the source's, mapped.
fn admin_of(snapshot: &ItemSnapshot, map: &[(Vec<u8>, Vec<u8>)]) -> Option<PathBuf> {
    snapshot
        .separate_admin
        .as_ref()
        .map(|admin| PathBuf::from(std::ffi::OsStr::from_bytes(&mapped(&rel_bytes(admin), map))))
}

/// Fetch `snapshot`'s objects into the item's live object store.
///
/// By id, from the live source repository, before any ref names them (step 1 of the
/// module docs): what a pass runs for every item in parallel. A linked
/// worktree owns no store: nothing to fetch.
///
/// # Errors
/// The fetch's refusals.
pub fn fetch_objects(
    snapshot: &ItemSnapshot,
    state: &Path,
    upload_pack: &str,
    map: &[(Vec<u8>, Vec<u8>)],
) -> Result<stream::Fetched> {
    let item = &snapshot.item;
    let live = item
        .workspace
        .clone()
        .unwrap_or_else(|| item.repository.clone());
    let Some(objects) = store_of(snapshot, &live, map) else {
        return Ok(stream::Fetched::default());
    };
    let shallow = shallow_decision(snapshot, map)?;
    for dir in [objects.join("pack"), objects.join("info")] {
        fs::create_dir_all(&dir).refuse_at(SITE)?;
    }
    let fetched = stream::fetch_into_shallow(
        &objects,
        &snapshot.snapshot.object_format,
        item.source
            .to_str()
            .ok_or(BulkloadRefusal::PathNotPortable)?,
        upload_pack,
        &snapshot.snapshot,
        &marks_path(state, &id(item)?),
        &snapshot.shallow,
    )?;
    if !snapshot.shallow.is_empty() {
        let frontier = if shallow == "keep-full" {
            &[][..]
        } else {
            snapshot.shallow.as_slice()
        };
        if !stream::graph_complete(
            &objects,
            &snapshot.snapshot.object_format,
            &snapshot.snapshot.wants,
            frontier,
        )? {
            return Err(BulkloadRefusal::GitHavesUnprovable);
        }
        let path = objects
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?
            .join("shallow");
        if shallow == "create" {
            publish_frontier(&path, &snapshot.shallow, &id(item)?)?;
        }
        // A concurrent Git writer owns any new boundary. Never overwrite
        // it or publish refs against an unproved frontier.
        if stream::read_frontier(&path, &snapshot.snapshot.object_format)? != frontier {
            return Err(BulkloadRefusal::GitHavesUnprovable);
        }
    }
    Ok(fetched)
}

/// The shallow action a plan or execution can prove without changing the
/// destination. Unsupported frontier changes remain typed holds.
///
/// `full`: a full source; `shared`: a linked worktree whose common
/// repository settles the boundary; `create`: an absent administration;
/// `same`: an equal boundary; `keep-full`: the destination already holds
/// all ancestry behind the source's boundary and stays full.
///
/// # Errors
/// `GIT_HAVES_UNPROVABLE` for differing frontiers or unavailable ancestry;
/// IO and Git read refusals retain their types.
pub fn shallow_decision(
    snapshot: &ItemSnapshot,
    map: &[(Vec<u8>, Vec<u8>)],
) -> Result<&'static str> {
    let live = snapshot
        .item
        .workspace
        .as_ref()
        .unwrap_or(&snapshot.item.repository);
    let owns_store = store_of(snapshot, live, map);
    let objects = if let Some(objects) = owns_store.as_ref() {
        objects.clone()
    } else {
        // A linked worktree shares a main's frontier. The main can have
        // been captured at a different moment, so inspect its destination
        // administration before permitting this worktree's seats/index.
        let common = crate::git_carry::common_repository(live)
            .or_else(|_| crate::git_carry::common_repository(&snapshot.item.repository));
        let Ok(common) = common else {
            return Ok("shared");
        };
        common.join("objects")
    };
    let admin = objects.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let held = stream::read_frontier(&admin.join("shallow"), &snapshot.snapshot.object_format)?;
    if snapshot.shallow.is_empty() {
        // A source that became full must not leave the previous landing's
        // boundary hiding history. Boundary removal needs its own proved
        // consolidation; until then this is an explicit hold.
        return if held.is_empty() {
            Ok("full")
        } else {
            Err(BulkloadRefusal::GitHavesUnprovable)
        };
    }
    if held == snapshot.shallow {
        return Ok("same");
    }
    if !held.is_empty() {
        return Err(BulkloadRefusal::GitHavesUnprovable);
    }
    if owns_store.is_some() && crate::git_carry::common_repository(live).is_err() {
        if admin.join("HEAD").try_exists().refuse_at(SITE)?
            || admin.join(CONFIG).try_exists().refuse_at(SITE)?
        {
            return Err(BulkloadRefusal::GitHavesUnprovable);
        }
        return Ok("create");
    }
    let boundary: Vec<String> = std::str::from_utf8(&snapshot.shallow)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .lines()
        .map(str::to_owned)
        .collect();
    if stream::graph_complete(&objects, &snapshot.snapshot.object_format, &boundary, &[])? {
        Ok("keep-full")
    } else {
        Err(BulkloadRefusal::GitHavesUnprovable)
    }
}

// Fetch must already have settled any frontier before administration or
// refs are published. Recheck at both ends of landing/consolidation.
fn verify_frontier(snapshot: &ItemSnapshot, map: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
    if matches!(shallow_decision(snapshot, map)?, "create" | "shared") {
        Err(BulkloadRefusal::GitHavesUnprovable)
    } else {
        Ok(())
    }
}

// Publish a new frontier without replacing any concurrent writer's file.
// The temporary shares the administration's filesystem and holds metadata
// only. A crash leaves a valid frontier before any ref is landed.
fn publish_frontier(path: &Path, bytes: &[u8], identity: &str) -> Result<()> {
    use crate::counters::CountedSync as _;
    let temporary = path.with_file_name(format!("bulkload-shallow-{identity}"));
    write_bytes(&temporary, bytes)?;
    let linked = fs::hard_link(&temporary, path);
    fs::remove_file(&temporary).refuse_at(SITE)?;
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    fs::File::open(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)
        .refuse_at(SITE)?
        .sync_dir_counted()
        .refuse_at(SITE)
}

/// What a consolidation of a related live repository did ([`converge`]).
#[derive(Debug, Default, Clone)]
pub struct Converged {
    /// Object batches fetched, skipped, and bytes the store grew by.
    pub batches: u64,
    pub skipped: u64,
    pub object_bytes: u64,
    /// Each source ref and what the union did to it.
    pub refs: Vec<(String, crate::git_carry::union::RefAction)>,
    /// Stash and reflog entries added.
    pub stash_added: u64,
    pub reflog_added: u64,
    /// The seat transfer held paths it could not union (typed in its
    /// record): the item is not consolidated whole.
    pub seats_held: bool,
    /// Ids the live repository already named, and its store lacked, before
    /// this consolidation ([`own_lacking`]): counted, never refused for.
    pub dest_lacking_own: u64,
    /// The destination's checkout was switched to the source's branch.
    pub switched_to: Option<String>,
    /// The two sides have different branches checked out and the
    /// destination's tree is not clean (or the source's branch is not the
    /// destination's at its tip): held typed, both listed, no seat landed.
    pub branches_held: Option<(String, String)>,
}

/// Consolidate `snapshot`'s item into a related repository at its live path.
///
/// The live path holds a repository of the destination's own (B2): the refs, stash and reflogs
/// unioned ([`crate::git_carry::union`]), then the worktree's seats through
/// the engine with #297's unions (equal adopted, new created, a differing
/// file held typed), the destination's administration and index its own.
/// `source` labels the source's remote-tracking refs.
///
/// # Errors
/// The union's and the transfer's refusals; `CAPTURE_DRIFTED` when the
/// repository still names a missing object; `STATE_PAYLOAD_LEFT` for a
/// quarantine left.
pub fn converge(
    snapshot: &ItemSnapshot,
    reach: &Reach<'_>,
    fetched: stream::Fetched,
    source: &str,
    map: &[(Vec<u8>, Vec<u8>)],
) -> Result<Converged> {
    use crate::git_carry::union;
    let item = &snapshot.item;
    let live = item
        .workspace
        .clone()
        .unwrap_or_else(|| item.repository.clone());
    let mut done = Converged {
        batches: fetched.batches,
        skipped: fetched.skipped,
        object_bytes: fetched.bytes,
        ..Converged::default()
    };
    let owns_store = store_of(snapshot, &live, map).is_some();
    verify_frontier(snapshot, map)?;
    let own = own_lacking(&live)?;
    done.dest_lacking_own = u64::try_from(own.len()).unwrap_or(u64::MAX);
    if owns_store {
        done.refs = union::refs(&live, &snapshot.snapshot.refs, source)?;
        done.stash_added = union::stash(&live, &snapshot.history.stash)?;
        done.reflog_added = union::reflogs(&live, &snapshot.history.reflogs, &done.refs)?;
    }
    if item.workspace.is_some() {
        // A different branch on each side (the GloriousFlywheel case): the
        // destination switches to the source's when its tree is clean and
        // that branch is the source's at its tip; else held, both listed.
        let (dest_branch, _) = union::head(&live)?;
        if let Some(theirs) = &snapshot.head_branch {
            if dest_branch.as_deref() != Some(theirs.as_str()) {
                let tip = union::value(&live, theirs)?;
                if union::is_clean(&live)? && tip.as_deref() == Some(snapshot.head.as_str()) {
                    union::switch(&live, theirs)?;
                    done.switched_to = Some(theirs.clone());
                } else {
                    done.branches_held = Some((
                        dest_branch.unwrap_or_else(|| "HEAD (detached)".to_owned()),
                        theirs.clone(),
                    ));
                    return Ok(done);
                }
            }
        }
        // Each file both sides changed merges three-way against the merge
        // base of the two HEADs.
        let (_, ours) = union::head(&live)?;
        let mut settings = (reach.consolidation)()?;
        if let Some(commit) = union::merge_base(&live, &ours, &snapshot.head)? {
            settings.git = Some(crate::consolidate::merge3::Base {
                repo: live.clone(),
                commit,
                scratch: reach.scratch.to_path_buf(),
            });
        }
        // The destination's administration is its own: nothing of the
        // source's `.git` lands here.
        let mut excluded = snapshot.excluded.clone();
        excluded.push(b".git".to_vec());
        let root = fs::canonicalize(&live).refuse_at(SITE)?;
        crate::transfer::set_excluded(&root, Some(excluded));
        crate::transfer::set_consolidate_for(&root, Some(Some(settings)));
        let moved = (reach.transfer)(&item.source, &live);
        crate::transfer::set_consolidate_for(&root, None);
        crate::transfer::set_excluded(&root, None);
        match moved {
            Ok(()) => {}
            Err(BulkloadRefusal::ItemsRefused) => done.seats_held = true,
            Err(refusal) => return Err(refusal),
        }
    }
    // A source that moved after its snapshot (SPEC rule 6: a live neo
    // worktree lands as it is at pass time): what the live administration
    // now names is fetched by id from the live source.
    fs::create_dir_all(reach.scratch).refuse_at(SITE)?;
    let (_, bytes) = top_up(
        snapshot,
        &live,
        &own,
        reach.upload_pack,
        &reach.scratch.join(format!("{}.top-up.objects", id(item)?)),
    )?;
    done.object_bytes = done.object_bytes.saturating_add(bytes);
    let objects = stream::objects_dir(&live)?;
    if let Some(left) = stream::own_quarantine_left(&objects)?.first() {
        return Err(BulkloadRefusal::StatePayloadLeft(rel_bytes(left)));
    }
    verify_frontier(snapshot, map)?;
    Ok(done)
}

/// The ids `live`'s administration names that its store lacks before a
/// landing writes anything.
///
/// The destination's own state (an `AUTO_MERGE` tree a gc pruned, a reflog
/// entry of its own), never the source's drift: a top-up fetches them when
/// the source holds them and never refuses for them. None when `live`
/// holds no repository Git can read yet (a fresh landing whose fetch made
/// only `.git/objects`): the stricter reading, every lacking id the
/// landing's own.
///
/// # Errors
/// None: an unreadable administration is no baseline.
pub fn own_lacking(live: &Path) -> Result<std::collections::BTreeSet<String>> {
    if fs::symlink_metadata(live.join(".git")).is_err() {
        return Ok(std::collections::BTreeSet::new());
    }
    Ok(stream::lacking(live)
        .map(|ids| ids.into_iter().collect())
        .unwrap_or_default())
}

/// How many rounds [`top_up`] fetches before it holds the item.
pub const TOP_UPS: usize = 3;

/// Fetch, by id from the live source, every object the live repository's
/// administration names and its store lacks. `own` (what it lacked before
/// the landing, [`own_lacking`]) is tried after, best effort, never required.
///
/// At most [`TOP_UPS`] rounds: a source that moved after its snapshot (an
/// agent committing under the pass) lands as it is now. Returns the ids
/// asked and the bytes the store grew by. `marks` is each round's resume
/// file, removed after it.
///
/// # Errors
/// `CAPTURE_DRIFTED` when the store still lacks an object the landing named
/// after the last round (the source does not hold it either); a fetch's
/// own refusal other than `GIT_CHILD_FAILED`; IO.
fn top_up(
    snapshot: &ItemSnapshot,
    live: &Path,
    own: &std::collections::BTreeSet<String>,
    upload_pack: &str,
    marks: &Path,
) -> Result<(u64, u64)> {
    let url = snapshot
        .item
        .source
        .to_str()
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    // What the landing made the live administration name that its store
    // lacks: the destination's own lacking ids are not this item's.
    let wanted = |live: &Path| -> Result<Vec<String>> {
        let mut lacking = stream::lacking(live)?;
        lacking.retain(|id| !own.contains(id));
        Ok(lacking)
    };
    let (mut ids, mut bytes) = (0u64, 0u64);
    for _ in 0..TOP_UPS {
        let lacking = wanted(live)?;
        if lacking.is_empty() {
            break;
        }
        ids = ids.saturating_add(u64::try_from(lacking.len()).unwrap_or(u64::MAX));
        let want = stream::Snapshot {
            object_format: snapshot.snapshot.object_format.clone(),
            wants: lacking,
            ..stream::Snapshot::default()
        };
        let objects = stream::objects_dir(live)?;
        let fetched = stream::fetch_into_shallow(
            &objects,
            &snapshot.snapshot.object_format,
            url,
            upload_pack,
            &want,
            marks,
            &snapshot.shallow,
        );
        let _ = fs::remove_file(marks);
        match fetched {
            Ok(fetched) => bytes = bytes.saturating_add(fetched.bytes),
            // The source no longer holds it (Git's fetch fails): the proof
            // below decides, and drift recaptures.
            Err(BulkloadRefusal::GitChildFailed(_)) => break,
            // Anything else keeps its own code: never drift by another name.
            Err(refusal) => return Err(refusal),
        }
    }
    if !wanted(live)?.is_empty() {
        return Err(BulkloadRefusal::CaptureDrifted);
    }
    // The destination's own lacking ids: fetched when the source holds
    // them, never refused for. One the source lacks fails Git's whole
    // fetch, so a failed batch is tried id by id.
    let still: Vec<String> = stream::lacking(live)?
        .into_iter()
        .filter(|id| own.contains(id))
        .collect();
    if !still.is_empty() {
        let objects = stream::objects_dir(live)?;
        let try_fetch = |wants: Vec<String>| -> Result<u64> {
            let want = stream::Snapshot {
                object_format: snapshot.snapshot.object_format.clone(),
                wants,
                ..stream::Snapshot::default()
            };
            let fetched = stream::fetch_into_shallow(
                &objects,
                &snapshot.snapshot.object_format,
                url,
                upload_pack,
                &want,
                marks,
                &snapshot.shallow,
            );
            let _ = fs::remove_file(marks);
            match fetched {
                Ok(fetched) => Ok(fetched.bytes),
                Err(BulkloadRefusal::GitChildFailed(_)) => Ok(0),
                Err(refusal) => Err(refusal),
            }
        };
        ids = ids.saturating_add(u64::try_from(still.len()).unwrap_or(u64::MAX));
        bytes = bytes.saturating_add(try_fetch(still.clone())?);
        if still.len() > 1 {
            for id in stream::lacking(live)?
                .into_iter()
                .filter(|id| own.contains(id))
                .take(OWN_ONE_BY_ONE)
            {
                bytes = bytes.saturating_add(try_fetch(vec![id])?);
            }
        }
    }
    Ok((ids, bytes))
}

/// At most this many of the destination's own lacking ids are tried one by
/// one after their batch failed ([`top_up`]).
const OWN_ONE_BY_ONE: usize = 64;

/// `private/ITEM.converged`: the item's live path holds the destination's
/// own repository, consolidated (never replaced) on every pass.
#[must_use]
pub fn converged_path(state: &Path, identity: &str) -> PathBuf {
    state.join(format!("{identity}.converged"))
}

/// Record that the item is consolidated into the destination's own
/// repository.
///
/// # Errors
/// IO.
pub fn mark_converged(state: &Path, identity: &str) -> Result<()> {
    let path = converged_path(state, identity);
    if path.is_file() {
        return Ok(());
    }
    write_bytes(&path, b"consolidated\n")
}

/// `private/ITEM.index`: the entries digest of the index a landing last
/// wrote at the item's live index.
fn index_mark(state: &Path, identity: &str) -> PathBuf {
    state.join(format!("{identity}.index"))
}

/// Settle the item's live index from the source's (SPEC rule 8: compared
/// by entries, never by stat data or extensions, so a destination `git
/// status` that refreshed it is no change).
///
/// - No live index: the source's is written.
/// - The live index holds the entries a landing last wrote, or the
///   source's own: the source's is written when its entries differ.
/// - The live index holds entries of the destination's own staging: kept,
///   and the settle says `held`.
///
/// # Errors
/// IO; a Git child's refusal.
pub fn settle_index(
    snapshot: &ItemSnapshot,
    live: &Path,
    state: &Path,
    scratch: &Path,
) -> Result<&'static str> {
    let Some(bytes) = &snapshot.index else {
        return Ok("none");
    };
    let identity = id(&snapshot.item)?;
    let index = PathBuf::from(crate::git_carry::git_path(live, "index")?);
    fs::create_dir_all(scratch).refuse_at(SITE)?;
    let apart = scratch.join(format!("index-{identity}"));
    fs::write(&apart, bytes).refuse_at(SITE)?;
    let source = crate::git_carry::union::index_entries(live, &apart);
    let _ = fs::remove_file(&apart);
    let source = source?;
    let mark = index_mark(state, &identity);
    let write = |what: &'static str| -> Result<&'static str> {
        write_bytes(&index, bytes)?;
        write_bytes(&mark, &source)?;
        Ok(what)
    };
    if !index.try_exists().refuse_at(SITE)? {
        return write("written");
    }
    let held = crate::git_carry::union::index_entries(live, &index)?;
    if held == source {
        write_bytes(&mark, &source)?;
        return Ok("same");
    }
    let last = fs::read(&mark).ok();
    if last.as_deref() == Some(held.as_slice()) {
        return write("updated");
    }
    Ok("held")
}

/// The journal a finished landing writes (the closure ledger's
/// `workspace-restored`), bound to `snapshot`'s digest.
///
/// # Errors
/// IO.
pub fn journal(state: &Path, snapshot: &ItemSnapshot, source: &str) -> Result<()> {
    let identity = id(&snapshot.item)?;
    let path = super::journal_path(state, &identity, source, &snapshot.digest());
    let outcome = if snapshot.item.workspace.is_some() {
        "workspace-restored"
    } else {
        "refs-imported"
    };
    super::write(&path, &outcome.to_owned())
}

/// Whether a journal of this item names a landing of any of its snapshots:
/// the live path then holds this item's own landing.
///
/// # Errors
/// IO.
pub fn landed_before(state: &Path, identity: &str, source: &str) -> Result<bool> {
    let prefix = format!("{identity}-{}-", blake3::hash(source.as_bytes()).to_hex());
    let read = match fs::read_dir(state) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in read {
        let name = entry.refuse_at(SITE)?.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".done") {
            let done: String = read_journal(&state.join(name.as_ref()))?;
            if done == "workspace-restored" || done == "refs-imported" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn read_journal(path: &Path) -> Result<String> {
    read(path)
}

/// Record `item`'s outcome in the apply's private state, as every estate
/// verb does (`ITEM.outcome`).
///
/// # Errors
/// IO; an outcome the record cannot hold.
pub fn record_outcome(
    state: &Path,
    item: &Item,
    outcome: &'static str,
    refusal: Option<&BulkloadRefusal>,
    reason: Option<String>,
) -> Result<()> {
    let record = crate::outcome::OutcomeRecord::of_receipt(
        item.source.clone(),
        outcome,
        refusal,
        reason,
        SITE,
    )?;
    super::write(
        &state.join(format!("{}.outcome", id(item)?)),
        &record.persisted(),
    )
}

/// Write a bare repository's administration at `path` by hand: a HEAD,
/// its configuration (`object_format`) and empty ref and object
/// directories. A streamed fetch runs in it, its objects written into a
/// live store, never here.
///
/// # Errors
/// IO; `GIT_INVENTORY_MALFORMED` for an object format that is not a word.
pub(crate) fn bare_layout(path: &Path, object_format: &str) -> Result<()> {
    for dir in ["refs/heads", "refs/tags", "objects/info", "objects/pack"] {
        fs::create_dir_all(path.join(dir)).refuse_at(SITE)?;
    }
    fs::write(path.join("HEAD"), b"ref: refs/heads/main\n").refuse_at(SITE)?;
    let config = match object_format {
        "sha1" => "[core]\n\trepositoryformatversion = 0\n\tbare = true\n".to_owned(),
        format if format.bytes().all(|byte| byte.is_ascii_alphanumeric()) => format!(
            "[core]\n\trepositoryformatversion = 1\n\tbare = true\n[extensions]\n\tobjectformat = {format}\n"
        ),
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    };
    fs::write(path.join(CONFIG), config).refuse_at(SITE)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn legacy_snapshot() -> LegacyItemSnapshot {
        LegacyItemSnapshot {
            item: Item {
                source: PathBuf::from("/source/repo"),
                repository: PathBuf::from("/destination/repo"),
                workspace: Some(PathBuf::from("/destination/repo")),
            },
            snapshot: stream::Snapshot {
                object_format: "sha1".to_owned(),
                ..stream::Snapshot::default()
            },
            excluded: vec![b".git/objects".to_vec()],
            host_files: vec![(b".git/config".to_vec(), b"[core]\n".to_vec())],
            started_ns: 1,
            history: stream::History::default(),
            index: None,
            head_branch: Some("refs/heads/main".to_owned()),
            head: String::new(),
            separate_admin: None,
        }
    }

    #[test]
    fn legacy_snapshot_records_and_full_source_digests_remain_compatible() {
        let carry = tempfile::tempdir().unwrap();
        let old = legacy_snapshot();
        let identity = id(&old.item).unwrap();
        let mut hash = blake3::Hasher::new_derive_key("bulkload 2026-10-20 item snapshot v1");
        hash.update(&old.snapshot.digest());
        for (path, bytes) in &old.host_files {
            hash.update(&(path.len() as u64).to_le_bytes());
            hash.update(path);
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        let digest = *hash.finalize().as_bytes();
        fs::write(
            snapshot_path(carry.path(), &identity),
            postcard::to_allocvec(&old).unwrap(),
        )
        .unwrap();
        let restored = recorded(carry.path(), &identity).unwrap().unwrap();
        assert!(restored.shallow.is_empty());
        assert!(seat_exclusions(&restored).contains(&b".git/shallow".to_vec()));
        assert_eq!(restored.digest(), digest);
        assert_eq!(
            stream::fetch_digest(&restored.snapshot, &restored.shallow),
            restored.snapshot.digest()
        );
        record(carry.path(), &restored).unwrap();
        assert_eq!(
            recorded(carry.path(), &identity).unwrap().unwrap(),
            restored
        );
    }

    #[test]
    fn shallow_snapshot_records_bind_the_boundary_and_roundtrip() {
        let carry = tempfile::tempdir().unwrap();
        let mut snapshot = ItemSnapshot::from(legacy_snapshot());
        let full_digest = snapshot.digest();
        snapshot.shallow = format!("{}\n", "1".repeat(40)).into_bytes();
        assert_ne!(snapshot.digest(), full_digest);
        let identity = id(&snapshot.item).unwrap();
        record(carry.path(), &snapshot).unwrap();
        assert!(fs::read(snapshot_path(carry.path(), &identity))
            .unwrap()
            .starts_with(SNAPSHOT_V2));
        assert_eq!(
            recorded(carry.path(), &identity).unwrap().unwrap(),
            snapshot
        );
    }

    #[test]
    fn host_paths_map_at_path_ends_only_longest_first() {
        let map = vec![
            (
                b"/Users/me/git/lab-wt".to_vec(),
                b"/home/me/git/lab-wt".to_vec(),
            ),
            (b"/Users/me/git/lab".to_vec(), b"/home/me/git/lab".to_vec()),
        ];
        let config = b"[core]\n\tworktree = /Users/me/git/lab\n[remote \"o\"]\n\turl = /Users/me/git/lab-wt/.git\n\tpushurl = /Users/me/git/labX\n";
        assert_eq!(
            mapped(config, &map),
            b"[core]\n\tworktree = /home/me/git/lab\n[remote \"o\"]\n\turl = /home/me/git/lab-wt/.git\n\tpushurl = /Users/me/git/labX\n".to_vec()
        );
        assert_eq!(
            mapped(b"gitdir: /Users/me/git/lab/.git/worktrees/w\n", &map),
            b"gitdir: /home/me/git/lab/.git/worktrees/w\n".to_vec()
        );
    }

    #[test]
    fn the_path_map_is_the_plans_moved_paths_longest_first() {
        let items = [
            Item {
                source: PathBuf::from("/s/lab"),
                repository: PathBuf::from("/d/lab"),
                workspace: Some(PathBuf::from("/d/lab")),
            },
            Item {
                source: PathBuf::from("/s/lab-wt"),
                repository: PathBuf::from("/d/lab"),
                workspace: Some(PathBuf::from("/d/lab-wt")),
            },
            Item {
                source: PathBuf::from("/same"),
                repository: PathBuf::from("/same"),
                workspace: Some(PathBuf::from("/same")),
            },
        ];
        assert_eq!(
            path_map(&items),
            vec![
                (b"/s/lab-wt".to_vec(), b"/d/lab-wt".to_vec()),
                (b"/s/lab".to_vec(), b"/d/lab".to_vec()),
            ]
        );
    }
}
