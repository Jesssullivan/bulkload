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
    /// bytes as read: written mapped at the destination, never by the
    /// engine.
    pub host_files: Vec<(Vec<u8>, Vec<u8>)>,
    /// When the pass that read it started (Unix nanoseconds).
    pub started_ns: i128,
}

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
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().to_vec()
}

/// Read `item`'s streamed capture on the source.
///
/// Refused, typed, before anything is read further: a partial clone (`GIT_SOURCE_PARTIAL_CLONE`),
/// a shallow repository (`GIT_HAVES_UNPROVABLE`) or one that borrows
/// objects (`GIT_SOURCE_ALTERNATES`): each is a hold until it has a test.
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
    if common.join("shallow").try_exists().refuse_at(SITE)? {
        return Err(BulkloadRefusal::GitHavesUnprovable);
    }
    let alternates = common.join("objects/info/alternates");
    if alternates.try_exists().refuse_at(SITE)? {
        return Err(BulkloadRefusal::GitSourceAlternates(rel_bytes(&alternates)));
    }
    let snapshot = stream::snapshot(source)?;
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
    match &admin {
        Some(admin) => {
            excluded.push(rel_bytes(&admin.join("objects")));
            let config = admin.join(CONFIG);
            host_files.push((rel_bytes(&config), read_file(&source.join(&config))?));
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
    if !include_rebuildable {
        for omission in crate::git_carry::rebuildable_omissions(source)? {
            excluded.push(omission.rel_path.clone());
        }
    }
    for nest in crate::git_carry::nested_repositories(source)? {
        excluded.push(nest.rel_path.clone());
    }
    for (rel, _) in &host_files {
        excluded.push(rel.clone());
    }
    excluded.sort();
    excluded.dedup();
    Ok(ItemSnapshot {
        item: item.clone(),
        snapshot,
        excluded,
        host_files,
        started_ns,
    })
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).refuse_at(SITE)
}

/// Record `snapshot` at `carry/ITEM.snapshot`, durably.
///
/// # Errors
/// IO; `FRAME_CODEC`.
pub fn record(carry: &Path, snapshot: &ItemSnapshot) -> Result<()> {
    let bytes = postcard::to_allocvec(snapshot).map_err(|_| BulkloadRefusal::FrameCodec)?;
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
    postcard::from_bytes(&bytes)
        .map(Some)
        .map_err(|_| BulkloadRefusal::FrameCodec)
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
}

/// How a landing reaches the source: its repository URL for `git fetch`
/// (the item's source path), the `upload-pack` program `git` runs there,
/// and the seat transfer.
pub struct Reach<'a> {
    pub upload_pack: &'a str,
    /// One recorded seat transfer of `from` (a source path) into `to`.
    pub transfer: &'a dyn Fn(&Path, &Path) -> Result<()>,
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
) -> Result<Landed> {
    let item = &snapshot.item;
    let identity = id(item)?;
    let live = item
        .workspace
        .clone()
        .unwrap_or_else(|| item.repository.clone());
    let source_url = item
        .source
        .to_str()
        .ok_or(BulkloadRefusal::PathNotPortable)?
        .to_owned();
    let mut landed = Landed {
        missing_at_source: snapshot.snapshot.missing,
        ..Landed::default()
    };
    let owns_store = snapshot
        .excluded
        .iter()
        .any(|path| path.ends_with(b"objects"));
    // 1. The objects, into the live store, before any ref names them.
    if owns_store {
        let admin = snapshot
            .excluded
            .iter()
            .find(|path| path.ends_with(b"objects"))
            .map(|path| {
                use std::os::unix::ffi::OsStrExt as _;
                live.join(std::ffi::OsStr::from_bytes(path))
            })
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        for dir in [admin.join("pack"), admin.join("info")] {
            fs::create_dir_all(&dir).refuse_at(SITE)?;
        }
        let fetched = stream::fetch_into(
            &admin,
            &snapshot.snapshot.object_format,
            &source_url,
            reach.upload_pack,
            &snapshot.snapshot,
            &marks_path(state, &identity),
        )?;
        landed.batches = fetched.batches;
        landed.skipped = fetched.skipped;
        landed.object_bytes = fetched.bytes;
    }
    // 2. The seats and the administration's own files.
    let root = {
        fs::create_dir_all(&live).refuse_at(SITE)?;
        fs::canonicalize(&live).refuse_at(SITE)?
    };
    crate::transfer::set_excluded(&root, Some(snapshot.excluded.clone()));
    let moved = (reach.transfer)(&item.source, &live);
    crate::transfer::set_excluded(&root, None);
    moved?;
    // 3. The host-path files, mapped.
    for (rel, bytes) in &snapshot.host_files {
        use std::os::unix::ffi::OsStrExt as _;
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
    // 4. What the landed administration names and the store lacks.
    if owns_store || item.workspace.is_some() {
        let lacking = stream::lacking(&live)?;
        if !lacking.is_empty() {
            let top_up = stream::Snapshot {
                object_format: snapshot.snapshot.object_format.clone(),
                wants: lacking,
                ..stream::Snapshot::default()
            };
            let objects = stream::objects_dir(&live)?;
            let fetched = stream::fetch_into(
                &objects,
                &snapshot.snapshot.object_format,
                &source_url,
                reach.upload_pack,
                &top_up,
                &state.join(format!("{identity}.top-up.objects")),
            )?;
            landed.topped_up = u64::try_from(top_up.wants.len()).unwrap_or(u64::MAX);
            landed.object_bytes = landed.object_bytes.saturating_add(fetched.bytes);
            let _ = fs::remove_file(state.join(format!("{identity}.top-up.objects")));
        }
        // 5. The proof.
        if !stream::lacking(&live)?.is_empty() {
            return Err(BulkloadRefusal::CaptureDrifted);
        }
        let objects = stream::objects_dir(&live)?;
        if let Some(left) = stream::quarantine_left(&objects)?.first() {
            return Err(BulkloadRefusal::StatePayloadLeft(rel_bytes(left)));
        }
    }
    Ok(landed)
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

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
