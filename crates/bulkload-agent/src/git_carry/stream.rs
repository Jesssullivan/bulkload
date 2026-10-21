//! The streamed capture's objects (SPEC rule 2, reset 2026-10-20; B1).
//!
//! No bundle, repository copy or corpus: a repository's objects go from the
//! LIVE source repository straight into the LIVE destination repository's
//! object store, over Git's own pack protocol inside bulkload:
//!
//! - [`snapshot`] reads, on the source, every object id the move must carry
//!   (the wants): every ref, every worktree's HEAD and per-worktree refs,
//!   every reflog entry's old and new ids, the stash, notes and replace
//!   refs, the in-progress operation files (`ORIG_HEAD`, a merge, a rebase,
//!   a sequencer), and each worktree's index blobs. Objects the source no
//!   longer holds are counted, never wanted. Nothing is written.
//! - [`serve_upload_pack`] is the source end: `git upload-pack` through the
//!   sanctioned builder (a read), with `uploadpack.allowAnySHA1InWant` on
//!   its command line only, so pinned ids resolve however the refs moved
//!   since; the source's configuration is never written.
//! - [`fetch`] is the destination end: `git fetch` by id into the live
//!   repository, in batches. Git negotiates what the repository holds
//!   (haves), indexes each pack in its quarantine and checks connectivity
//!   before the objects join the store; each finished batch is a resume
//!   mark, so a crash re-fetches only unfinished batches, and a rerun
//!   fetches nothing.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use super::{durable, git, input, oid, output, text};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "git_carry::stream";

/// The configuration the source end runs `upload-pack` with, on its command
/// line only.
///
/// Any object id the snapshot pinned may be wanted, reachable
/// from a ref now or not (a reflog entry, a moved ref's old value, an
/// index blob).
pub const UPLOAD_PACK_CONFIG: &str = "uploadpack.allowAnySHA1InWant=true";

/// The most ids one fetch wants: one resume mark's worth.
pub const BATCH: usize = 4096;

/// What a repository holds to be carried, read on the source.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `sha1` or `sha256`.
    pub object_format: String,
    /// Every ref of the common repository, `(name, value)`, sorted.
    pub refs: Vec<(String, String)>,
    /// Each worktree (the main first): its path, its HEAD's value (empty
    /// for an unborn branch) and its per-worktree refs.
    pub worktrees: Vec<Worktree>,
    /// Every object id to carry, sorted, each one held by the source.
    pub wants: Vec<String>,
    /// Ids named somewhere (a reflog entry, an operation file) that the
    /// source no longer holds: counted, never wanted.
    pub missing: u64,
    /// Objects in the source's store that nothing names (no ref, reflog,
    /// index or operation file): they have no name to carry and `gc` would
    /// prune them. Counted, never carried; closure shows the number.
    pub unreachable: u64,
}

/// One worktree of a snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// Its path on the source.
    pub path: PathBuf,
    /// HEAD's commit (empty for an unborn branch or a bare repository).
    pub head: String,
    /// Per-worktree refs (`refs/bisect/`, `refs/worktree/`,
    /// `refs/rewritten/`), `(name, value)`.
    pub refs: Vec<(String, String)>,
}

impl Snapshot {
    /// A digest of everything the snapshot pins: the batches' resume marks
    /// are bound to it.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key("bulkload 2026-10-20 stream snapshot v1");
        hasher.update(self.object_format.as_bytes());
        hasher.update(b"\0");
        for want in &self.wants {
            hasher.update(want.as_bytes());
            hasher.update(b"\n");
        }
        *hasher.finalize().as_bytes()
    }
}

// `%(objectname) %(refname)` lines.
fn ref_lines(listed: &str) -> Result<Vec<(String, String)>> {
    let mut refs = Vec::new();
    for line in listed.lines().filter(|line| !line.is_empty()) {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        refs.push((name.to_owned(), value.to_owned()));
    }
    refs.sort();
    Ok(refs)
}

// `worktree list --porcelain -z`: each record's path and whether it is
// bare or prunable (its path gone).
fn worktree_paths(repo: &Path) -> Result<Vec<(PathBuf, bool)>> {
    use std::os::unix::ffi::OsStrExt as _;
    let listed = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut found = Vec::new();
    let mut current: Option<(PathBuf, bool)> = None;
    for field in listed.split(|byte| *byte == 0) {
        if field.is_empty() {
            found.extend(current.take());
            continue;
        }
        if let Some(path) = field.strip_prefix(b"worktree ") {
            found.extend(current.take());
            current = Some((PathBuf::from(std::ffi::OsStr::from_bytes(path)), false));
        } else if field == b"bare" || field.starts_with(b"prunable") {
            if let Some((_, skip)) = current.as_mut() {
                *skip = true;
            }
        }
    }
    found.extend(current);
    Ok(found)
}

// Every full object id token in `bytes` (an operation file or a reflog).
fn ids_in(bytes: &[u8], ids: &mut BTreeSet<String>) {
    for token in bytes.split(|byte| !byte.is_ascii_hexdigit()) {
        if let Ok(token) = std::str::from_utf8(token) {
            if oid(token) && token.bytes().any(|byte| byte != b'0') {
                ids.insert(token.to_owned());
            }
        }
    }
}

// Every file under `dir`, read whole (a missing `dir` holds none).
fn read_tree_files(dir: &Path, ids: &mut BTreeSet<String>) -> Result<()> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in read {
        let path = entry.refuse_at(SITE)?.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        };
        if meta.is_dir() {
            read_tree_files(&path, ids)?;
        } else if meta.is_file() {
            match fs::read(&path) {
                Ok(bytes) => ids_in(&bytes, ids),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(crate::refuse::io(&error, SITE)),
            }
        }
    }
    Ok(())
}

/// The operation files of one administration directory whose ids are
/// wanted: what an in-progress merge, cherry-pick, revert, bisect or
/// rebase names, and `ORIG_HEAD`.
const OPERATION_FILES: &[&str] = &[
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_HEAD",
    "AUTO_MERGE",
    "REBASE_HEAD",
];

/// The operation directories of one administration directory, read whole.
const OPERATION_DIRS: &[&str] = &["rebase-merge", "rebase-apply", "sequencer", "logs"];

fn operation_ids(admin: &Path, ids: &mut BTreeSet<String>) -> Result<()> {
    for name in OPERATION_FILES {
        match fs::read(admin.join(name)) {
            Ok(bytes) => ids_in(&bytes, ids),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        }
    }
    for name in OPERATION_DIRS {
        read_tree_files(&admin.join(name), ids)?;
    }
    Ok(())
}

/// Read what `repo` (any worktree of it, or a bare repository) holds to be
/// carried. Reads only: Git children through the sanctioned builder, and
/// the administration's reflogs and operation files.
///
/// # Errors
/// Git children's refusals; `GIT_INVENTORY_MALFORMED` for output that does
/// not parse; IO.
pub fn snapshot(repo: &Path) -> Result<Snapshot> {
    let (object_format, refs, worktrees, named) = named(repo)?;
    let (wants, missing) = partition(repo, &named)?;
    let unreachable = unreachable(repo)?;
    Ok(Snapshot {
        object_format,
        refs,
        worktrees,
        wants,
        missing,
        unreachable,
    })
}

// How many objects `repo`'s store holds that no ref, reflog, index or
// operation file reaches (`fsck --unreachable`, connectivity only: commits
// and trees are read, blobs are not).
fn unreachable(repo: &Path) -> Result<u64> {
    let listed = output(git(repo).args([
        "fsck",
        "--unreachable",
        "--connectivity-only",
        "--no-progress",
        "--no-dangling",
    ]))?;
    u64::try_from(
        listed
            .split(|byte| *byte == b'\n')
            .filter(|line| line.starts_with(b"unreachable "))
            .count(),
    )
    .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
}

/// The ids `repo`'s administration names that its object store lacks: what
/// a landing must still fetch, and what its proof requires to be none.
///
/// # Errors
/// As [`snapshot`].
pub fn lacking(repo: &Path) -> Result<Vec<String>> {
    let (_, _, _, named) = named(repo)?;
    let (_, missing) = partition_ids(repo, &named)?;
    Ok(missing)
}

/// `repo`'s object store directory, absolute.
///
/// # Errors
/// Git's refusal.
pub fn objects_dir(repo: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ]))?))
}

// What every name in `named` is in `repo`'s store: held, or missing.
fn partition_ids(repo: &Path, named: &BTreeSet<String>) -> Result<(Vec<String>, Vec<String>)> {
    let request: String = named.iter().fold(String::new(), |mut all, id| {
        all.push_str(id);
        all.push('\n');
        all
    });
    let answered = input(
        git(repo).args(["cat-file", "--batch-check=%(objectname)"]),
        request.as_bytes(),
    )?;
    let answered =
        String::from_utf8(answered).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let (mut held, mut missing) = (Vec::new(), Vec::new());
    for line in answered.lines() {
        if let Some(id) = line.strip_suffix(" missing") {
            if !oid(id) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            missing.push(id.to_owned());
        } else if oid(line) {
            held.push(line.to_owned());
        } else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    held.sort();
    held.dedup();
    Ok((held, missing))
}

fn partition(repo: &Path, named: &BTreeSet<String>) -> Result<(Vec<String>, u64)> {
    let (held, missing) = partition_ids(repo, named)?;
    Ok((held, u64::try_from(missing.len()).unwrap_or(u64::MAX)))
}

type Named = (
    String,
    Vec<(String, String)>,
    Vec<Worktree>,
    BTreeSet<String>,
);

// The object format, the refs, the worktrees and every id `repo`'s
// administration names.
fn named(repo: &Path) -> Result<Named> {
    let common = super::common_repository(repo)?;
    let object_format = text(git(repo).args(["rev-parse", "--show-object-format"]))?;
    let refs = ref_lines(&text(
        git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]),
    )?)?;
    let mut named: BTreeSet<String> = refs.iter().map(|(_, value)| value.clone()).collect();
    let mut worktrees = Vec::new();
    for (path, skip) in worktree_paths(repo)? {
        if skip {
            continue;
        }
        let head = git(&path)
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .output()
            .refuse_at(SITE)?;
        let head = if head.status.success() {
            String::from_utf8(head.stdout)
                .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
                .trim_end()
                .to_owned()
        } else {
            String::new()
        };
        let own = ref_lines(&text(git(&path).args([
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/bisect/",
            "refs/worktree/",
            "refs/rewritten/",
        ]))?)?;
        named.extend(own.iter().map(|(_, value)| value.clone()));
        if !head.is_empty() {
            named.insert(head.clone());
        }
        // The index's blobs (gitlinks name another repository's commits).
        let staged = output(git(&path).args(["ls-files", "-s", "-z"]))?;
        for entry in staged
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let mut words = entry.split(|byte| *byte == b' ');
            if let (Some(mode), Some(id)) = (words.next(), words.next()) {
                if mode != b"160000" {
                    ids_in(id, &mut named);
                }
            }
        }
        worktrees.push(Worktree {
            path,
            head,
            refs: own,
        });
    }
    // Reflogs and operation files: the common directory's, and each linked
    // worktree's administration.
    operation_ids(&common, &mut named)?;
    if let Ok(read) = fs::read_dir(common.join("worktrees")) {
        for entry in read {
            let admin = entry.refuse_at(SITE)?.path();
            if fs::symlink_metadata(&admin).is_ok_and(|meta| meta.is_dir()) {
                operation_ids(&admin, &mut named)?;
            }
        }
    }
    Ok((object_format, refs, worktrees, named))
}

/// The source end of a fetch: `git upload-pack` of `repo`, its stdin and
/// stdout this process's, until the destination is done. Read-only on the
/// source: no ref, lock, reflog or configuration write.
///
/// # Errors
/// `GIT_CHILD_FAILED` when `upload-pack` fails; IO.
pub fn serve_upload_pack(repo: &Path) -> Result<()> {
    let status = git(repo)
        .args(["-c", UPLOAD_PACK_CONFIG, "upload-pack"])
        .arg(repo)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .refuse_at(SITE)?;
    if status.success() {
        Ok(())
    } else {
        Err(BulkloadRefusal::GitChildFailed(
            super::estimate::StderrClass::Other,
        ))
    }
}

/// The resume marks of one repository's fetch: the snapshot digest the
/// batches are bound to, and the batches finished.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marks {
    /// [`Snapshot::digest`] of the snapshot these marks are for.
    pub snapshot: [u8; 32],
    /// Indices of the finished batches.
    pub done: BTreeSet<u64>,
}

/// What one [`fetch`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Fetched {
    /// Batches fetched by this call.
    pub batches: u64,
    /// Batches a mark said were already done (skipped).
    pub skipped: u64,
    /// Bytes the live object store grew by (pack and loose files).
    pub bytes: u64,
}

fn read_marks(path: &Path, digest: [u8; 32]) -> Result<Marks> {
    match fs::read(path) {
        Ok(bytes) => {
            let marks: Marks =
                postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)?;
            // Marks for another snapshot say nothing about this one.
            Ok(if marks.snapshot == digest {
                marks
            } else {
                Marks {
                    snapshot: digest,
                    done: BTreeSet::new(),
                }
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Marks {
            snapshot: digest,
            done: BTreeSet::new(),
        }),
        Err(error) => Err(crate::refuse::io(&error, SITE)),
    }
}

// Bytes of every file under `objects`.
fn store_bytes(objects: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![objects.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if let Ok(meta) = fs::symlink_metadata(entry.path()) {
                if meta.is_dir() {
                    pending.push(entry.path());
                } else {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

/// A bare repository of its own that a fetch runs in, its objects written
/// into another store; removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Where a fetch's scratch repositories are made ([`set_scratch_root`]).
static SCRATCH_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Make every fetch's scratch repository under `root` (a move's own
/// `MOVE_STATE/scratch`, swept at its startup and counted by its state
/// guard, so one a crash left shows as payload); `None`: the temporary
/// directory.
pub fn set_scratch_root(root: Option<PathBuf>) {
    *SCRATCH_ROOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = root;
}

fn scratch(object_format: &str) -> Result<Scratch> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = SCRATCH_ROOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&root).refuse_at(SITE)?;
    let path = root.join(format!(
        "bulkload-fetch-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let made = Scratch(path.clone());
    crate::estate::streamed::bare_layout(&path, object_format)?;
    Ok(made)
}

/// Fetch `snapshot`'s wants from `url` into the object store `objects`.
///
/// In batches of [`BATCH`], running `upload_pack` as the source end (`git`
/// runs it, with the path `url` names, through its shell). The fetch runs in
/// a scratch bare repository whose objects are written into `objects` (a
/// live repository's store, whose refs may not exist yet): no ref, no
/// `FETCH_HEAD`, and Git's quarantine and connectivity check in that store.
/// Each finished batch is recorded in `marks` (written durably) before the
/// next starts; a batch a mark names is skipped. A rerun over a store that
/// holds every want fetches nothing: Git sends what is missing only.
///
/// # Errors
/// `GIT_CHILD_FAILED` when a fetch fails (its batch stays unmarked); IO.
pub fn fetch_into(
    objects: &Path,
    object_format: &str,
    url: &str,
    upload_pack: &str,
    snapshot: &Snapshot,
    marks: &Path,
) -> Result<Fetched> {
    let digest = snapshot.digest();
    let mut recorded = read_marks(marks, digest)?;
    let before = store_bytes(objects);
    let mut fetched = Fetched::default();
    let mut work: Option<Scratch> = None;
    for (index, batch) in snapshot.wants.chunks(BATCH).enumerate() {
        let index = u64::try_from(index).unwrap_or(u64::MAX);
        if recorded.done.contains(&index) {
            fetched.skipped += 1;
            continue;
        }
        if work.is_none() {
            work = Some(scratch(object_format)?);
        }
        let repo = &work.as_ref().ok_or(BulkloadRefusal::WorkerLost)?.0;
        let request = batch.iter().fold(String::new(), |mut all, id| {
            all.push_str(id);
            all.push('\n');
            all
        });
        let mut command = git(repo);
        durable(&mut command);
        super::into_store(&mut command, objects);
        input(
            command
                .args([
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--no-recurse-submodules",
                    "--no-auto-gc",
                    "--no-auto-maintenance",
                    "--stdin",
                    "--upload-pack",
                    upload_pack,
                ])
                .arg(url),
            request.as_bytes(),
        )?;
        recorded.done.insert(index);
        crate::estate::write_bytes(
            marks,
            &postcard::to_allocvec(&recorded).map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
        fetched.batches += 1;
    }
    fetched.bytes = store_bytes(objects).saturating_sub(before);
    Ok(fetched)
}

/// Index-pack's and the object writer's temporaries a pass must never
/// leave in a live object store: Git's quarantine (`incoming-*`,
/// `tmp_objdir-*`), and temporary packs and objects. Named by path.
///
/// # Errors
/// IO for an unreadable store directory.
pub fn quarantine_left(objects: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for (dir, prefixes) in [
        (objects.to_path_buf(), &["incoming-", "tmp_objdir-"][..]),
        (
            objects.join("pack"),
            &["tmp_pack_", "tmp_idx_", "tmp_rev_"][..],
        ),
    ] {
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        };
        for entry in read {
            let entry = entry.refuse_at(SITE)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if prefixes.iter().any(|prefix| name.starts_with(prefix)) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Write `snapshot` to `out` as one postcard record: what the source half
/// hands the destination over the wire.
///
/// # Errors
/// IO; `FRAME_CODEC`.
pub fn write_snapshot(out: &mut impl std::io::Write, snapshot: &Snapshot) -> Result<()> {
    let bytes = postcard::to_allocvec(snapshot).map_err(|_| BulkloadRefusal::FrameCodec)?;
    out.write_all(&bytes).refuse_at(SITE)?;
    out.flush().refuse_at(SITE)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("bulkload-stream-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Scratch(fs::canonicalize(&path).unwrap())
    }

    fn g(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=t",
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
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    // A source with history only a reflog names, a stash, a note, a staged
    // blob nothing else names, and a linked worktree with its own staged
    // blob and HEAD.
    fn source(root: &Path) -> (PathBuf, Vec<String>) {
        let repo = root.join("src");
        fs::create_dir_all(&repo).unwrap();
        g(&repo, &["init", "-q", "--template=", "-b", "main"]);
        fs::write(repo.join("a"), "a\n").unwrap();
        g(&repo, &["add", "a"]);
        g(&repo, &["commit", "-q", "-m", "c1"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "c2"]);
        let reflog_only = g(&repo, &["rev-parse", "HEAD"]);
        g(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
        fs::write(repo.join("a"), "edited\n").unwrap();
        g(&repo, &["stash", "-q"]);
        let stash = g(&repo, &["rev-parse", "refs/stash"]);
        g(&repo, &["notes", "add", "-m", "a note", "HEAD"]);
        fs::write(repo.join("b"), "staged only\n").unwrap();
        g(&repo, &["add", "b"]);
        let staged = g(&repo, &["rev-parse", ":b"]);
        let wt = root.join("wt");
        g(
            &repo,
            &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
        );
        g(&wt, &["commit", "-q", "--allow-empty", "-m", "side work"]);
        let side = g(&wt, &["rev-parse", "HEAD"]);
        fs::write(wt.join("c"), "wt staged\n").unwrap();
        g(&wt, &["add", "c"]);
        let wt_staged = g(&wt, &["rev-parse", ":c"]);
        (repo, vec![reflog_only, stash, staged, side, wt_staged])
    }

    #[test]
    fn the_snapshot_wants_every_named_object_and_counts_the_missing() {
        let root = scratch("snapshot");
        let (repo, named) = source(&root.0);
        let snapshot = snapshot(&repo).unwrap();
        for id in &named {
            assert!(snapshot.wants.contains(id), "{id} not wanted: {snapshot:?}");
        }
        assert!(snapshot
            .refs
            .iter()
            .any(|(name, _)| name == "refs/notes/commits"));
        assert!(snapshot.refs.iter().any(|(name, _)| name == "refs/stash"));
        assert_eq!(snapshot.worktrees.len(), 2);
        assert_eq!(snapshot.missing, 0);
        // An operation file naming an object the source lacks: counted.
        let gone = "1".repeat(snapshot.wants[0].len());
        fs::write(repo.join(".git/ORIG_HEAD"), format!("{gone}\n")).unwrap();
        let again = super::snapshot(&repo).unwrap();
        assert_eq!(again.missing, 1);
        assert!(!again.wants.contains(&gone));
        // The snapshot read nothing it could write: the source's refs,
        // reflog and config are as they were.
        assert_eq!(again.refs, snapshot.refs);
    }

    #[test]
    fn a_fetch_carries_every_want_and_a_rerun_carries_nothing() {
        let root = scratch("fetch");
        let (repo, named) = source(&root.0);
        let snapshot = snapshot(&repo).unwrap();
        let live = root.0.join("dst");
        fs::create_dir_all(&live).unwrap();
        g(&live, &["init", "-q", "--template=", "-b", "main"]);
        let marks = root.0.join("marks");
        let upload = format!("git -c {UPLOAD_PACK_CONFIG} upload-pack");
        let url = repo.to_str().unwrap();
        let objects = live.join(".git/objects");
        let first = fetch_into(&objects, "sha1", url, &upload, &snapshot, &marks).unwrap();
        assert_eq!(first.batches, 1);
        assert!(first.bytes > 0);
        for id in snapshot.wants.iter().chain(&named) {
            g(&live, &["cat-file", "-e", id]);
        }
        // No ref and no FETCH_HEAD written, no quarantine left.
        assert_eq!(g(&live, &["for-each-ref"]), "");
        assert!(!live.join(".git/FETCH_HEAD").exists());
        assert!(quarantine_left(&live.join(".git/objects"))
            .unwrap()
            .is_empty());
        // The mark skips the batch; with the mark gone, the store's haves
        // make the fetch carry nothing.
        let marked = fetch_into(&objects, "sha1", url, &upload, &snapshot, &marks).unwrap();
        assert_eq!((marked.batches, marked.skipped, marked.bytes), (0, 1, 0));
        fs::remove_file(&marks).unwrap();
        let negotiated = fetch_into(&objects, "sha1", url, &upload, &snapshot, &marks).unwrap();
        assert_eq!((negotiated.batches, negotiated.bytes), (1, 0));
    }

    #[test]
    fn marks_for_another_snapshot_are_not_reused() {
        let root = scratch("marks");
        let path = root.0.join("marks");
        let old = Marks {
            snapshot: [1; 32],
            done: BTreeSet::from([0, 1]),
        };
        fs::write(&path, postcard::to_allocvec(&old).unwrap()).unwrap();
        assert!(read_marks(&path, [2; 32]).unwrap().done.is_empty());
        assert_eq!(read_marks(&path, [1; 32]).unwrap(), old);
    }

    #[test]
    fn a_quarantine_left_in_a_store_is_named() {
        let root = scratch("quarantine");
        let objects = root.0.join("objects");
        fs::create_dir_all(objects.join("incoming-abc")).unwrap();
        fs::create_dir_all(objects.join("pack")).unwrap();
        fs::write(objects.join("pack/tmp_pack_x"), b"").unwrap();
        fs::write(objects.join("pack/pack-1.pack"), b"").unwrap();
        assert_eq!(
            quarantine_left(&objects).unwrap(),
            vec![
                objects.join("incoming-abc"),
                objects.join("pack/tmp_pack_x")
            ]
        );
    }
}
