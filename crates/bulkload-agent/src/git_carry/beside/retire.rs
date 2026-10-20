//! Beside retirement (operator ruling 2026-10-19, "Yes, advance those":
//! "the beside copy is retired only after bulkload proves the live tree
//! equals it").
//!
//! After `--advance-ancestor` advanced an item's live checkout, the beside
//! landing an earlier pass made for it (`<live>SUFFIX`: a linked worktree
//! of the live repository bulkload landed, or a standalone checkout #282
//! adopted by proof) holds what the live tree now holds. It is removed only
//! when it is this item's (its workspace ledger names the item and the
//! current capture, which the caller establishes) and [`retire`] proves,
//! read-only, that the live tree equals it:
//!
//! - the same HEAD commit, and the same index entries;
//! - every entry under the beside checkout (its `.git` aside), tracked,
//!   untracked or ignored, held by the live tree at the same path with the
//!   same kind, the same bytes (or link target) and the same executable
//!   bit, or, for a captured file the live checkout keeps as its own
//!   ignored file, at the file-level beside `<path>SUFFIX` the advance
//!   landed; a bulkload hand-off block at byte 0 of `AGENTS.md` or
//!   `CLAUDE.md`, sealed as estate-handoff renders it, is the one
//!   difference allowed (either side);
//! - every tracked or untracked file of the live tree that is not ignored
//!   held by the beside checkout, but the untracked files the advance
//!   found there and left as they were, apart from every captured path
//!   (the live tree's own, as its ignored files are);
//! - nothing the beside checkout's Git holds lost with it: no nested
//!   repository under it, no worktree of its own, no lock and no
//!   operation in progress (merge, rebase, cherry-pick, revert, bisect);
//!   every commit its refs, its stash and its reflogs name reachable from
//!   the live repository's refs (a linked landing's: its per-worktree
//!   reflogs, and no per-worktree ref; its administration directory under
//!   the live repository's `worktrees/`, its `gitdir` naming the beside
//!   checkout); a standalone one's every ref held by the live repository
//!   by name and value (its own, under the beside namespace, or in the
//!   capture's provenance; a provenance `refs/carry*` ref held by name and
//!   value or a reachable commit), no hook of its own, no remote URL the
//!   live repository lacks, no `modules` directory (review of #289,
//!   finding 5). Its configuration entries (the per-machine `core.*` keys
//!   `git init` writes aside) whose key the live configuration lacks, and
//!   its `info/exclude` patterns the live exclude lacks, go into the live
//!   repository first, create-only (the coordinator's decision of
//!   2026-10-20: `git config --add` only for a key the live repository's
//!   effective configuration lacks, every value of it or none, planned in
//!   the STATE record first; values `safe_configuration_value` allows;
//!   patterns by the exclude union's rule, recorded in STATE), counted in
//!   the receipt; a key the live configuration holds with another value,
//!   one not safe to carry, or an identity or signing key it does not hold
//!   (never carried; the verification of #289, r7) keeps the beside
//!   (`configuration:<key>`), as does a pattern no pure union carries or
//!   another converge proves (`exclude`).
//!
//! Any difference keeps the beside checkout and returns why
//! ([`Retired::Kept`]: `differs:head`, `differs:index`,
//! `differs:seat=<path>`, `missing:<path>`, `extra:<path>`,
//! `unreachable`, `refs:<name>`, `hooks`, `remotes`,
//! `configuration:<key>`, `exclude`, `modules`, `in-progress`,
//! `nested:<path>`, `locked`,
//! `worktrees`, `worktree-refs`, `admin-foreign`, `in-use`, `raced`).
//! The prints of the tree and of a linked landing's administration
//! directory (its HEAD, index, refs and reflogs) are taken before the
//! proof and again after it: a write while the proof read it (a session
//! working there) keeps it. A process whose working directory or an open
//! file is inside it (`/proc`, Linux; elsewhere unprovable) keeps it
//! (`in-use`). Nothing is written before the proof.
//!
//! The removal is crash-safe and never loses either tree: the retirement
//! is recorded in STATE ([`RetireIntent`]: the path, a private name beside
//! it, the directory's identity, a digest of every entry's print and when
//! the proof began), the checkout is renamed to that private name
//! (`estate.advance.retire_after_rename`; no process inside it first), and
//! before its deletion re-proved (review of #289, finding 2): its prints
//! unchanged and no process inside, else it goes back to its path
//! (`raced`, `in-use`); a deletion a crash or a failure cut short
//! resumes only while no file under it changed since the proof began
//! (its ctime), else what is left goes back (`written-since`). Then it is
//! marked deleting (`estate.advance.retire_mid`), the renamed tree deleted
//! (its identity checked first), a linked landing's administration
//! directory removed (under `worktrees/`, its `gitdir` as proved), and the
//! record removed. A rerun finishes from the record ([`settle`], before
//! the item's workspace step): nothing moved, the record goes and the
//! caller proves again; renamed, re-proved and finished, or put back.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::converge::advance::Left;
use super::super::converge::{handoff::render, head_of, index_digest};
use super::super::{git, landed, output};
use crate::{BulkloadRefusal, Result};

/// A retirement under way, in STATE (`{item}-{blake3(SOURCE)}.retire`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetireIntent {
    /// `estate::id` of the item.
    pub item: String,
    /// The capture both trees held when proved equal.
    pub capture: [u8; 32],
    /// The beside checkout's path.
    pub path: Vec<u8>,
    /// Its private retirement name, in the same directory.
    pub temp: Vec<u8>,
    /// A linked landing's administration directory; `None` for a
    /// standalone checkout.
    pub admin: Option<Vec<u8>>,
    /// What its `gitdir` file held when proved (its removal is bound to
    /// it, never to a spelling of the path).
    pub admin_gitdir: Vec<u8>,
    /// The beside directory's identity when proved.
    pub dev: u64,
    pub ino: u64,
    /// The digest of the print of every entry under it when proved.
    pub prints: [u8; 32],
    /// Renamed, re-proved and under deletion.
    pub deleting: bool,
    /// When the proof began, with a margin: a file under the tree whose
    /// ctime is later was written since, and a deletion cut short never
    /// resumes over it.
    pub recorded_ns: i128,
    /// The live checkout's path.
    pub live: Vec<u8>,
    /// The configuration a standalone beside carries into the live
    /// repository before it goes (each key with all its values), added
    /// after this record is written: a crash between two values is
    /// finished from here ([`settle`]), never left with some of a key.
    pub config: Vec<(String, Vec<String>)>,
}

impl landed::Record for RetireIntent {
    const MAGIC: &'static [u8; 16] = b"bulkload-retire1";
}

/// What became of a beside landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retired {
    /// Proved equal to the live tree and removed.
    Retired,
    /// Kept, and why.
    Kept(String),
}

impl Retired {
    /// The word STATE and the receipt record: `retired` or `kept`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Retired => "retired",
            Self::Kept(_) => "kept",
        }
    }

    /// Why it was kept (empty when retired).
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Retired => "",
            Self::Kept(reason) => reason,
        }
    }
}

const SITE: &str = "git_carry::beside::retire";

fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

pub(super) fn lstat(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(crate::refuse::io(
            &error,
            "git_carry::beside::retire::lstat",
        )),
    }
}

/// One entry under a tree, as a walk found it.
pub(super) struct Entry {
    pub(super) relative: Vec<u8>,
    pub(super) meta: fs::Metadata,
}

// Every entry under `root` (never following a link), sorted by path; with
// `with_git`, the top-level `.git` and what is under it too.
pub(super) fn walk(root: &Path, with_git: bool) -> Result<Vec<Entry>> {
    const SITE: &str = "git_carry::beside::retire::walk";
    let mut found = Vec::new();
    let mut pending = vec![Vec::<u8>::new()];
    while let Some(relative) = pending.pop() {
        let at = if relative.is_empty() {
            root.to_path_buf()
        } else {
            root.join(OsStr::from_bytes(&relative))
        };
        for entry in fs::read_dir(&at).refuse_at(SITE)? {
            let entry = entry.refuse_at(SITE)?;
            let name = entry.file_name();
            if !with_git && relative.is_empty() && name.as_bytes() == b".git" {
                continue;
            }
            let mut path = relative.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name.as_bytes());
            let meta = fs::symlink_metadata(entry.path()).refuse_at(SITE)?;
            if meta.is_dir() {
                pending.push(path.clone());
            }
            found.push(Entry {
                relative: path,
                meta,
            });
        }
    }
    found.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(found)
}

// The digest of every entry's print under `root`, its `.git` included,
// and under a linked landing's administration directory `admin` (its
// HEAD, index, refs and reflogs; review of #289, finding 4): a write
// anywhere under either since changes it.
pub(super) fn prints(root: &Path, admin: Option<&Path>) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-retire-prints-v2\0");
    let admin_entries = match admin {
        Some(admin) if lstat(admin)?.is_some_and(|meta| meta.is_dir()) => walk(admin, true)?,
        _ => Vec::new(),
    };
    for (part, entries) in [
        (b"tree".as_slice(), walk(root, true)?),
        (b"admin", admin_entries),
    ] {
        hasher.update(part);
        for entry in entries {
            let identity = crate::freshness::StatIdentity::from_metadata(&entry.meta);
            hasher.update(&(entry.relative.len() as u64).to_le_bytes());
            hasher.update(&entry.relative);
            hasher.update(&entry.meta.mode().to_le_bytes());
            hasher.update(&identity.ino.to_le_bytes());
            hasher.update(&identity.size.to_le_bytes());
            hasher.update(&identity.mtime_ns.to_le_bytes());
            hasher.update(&identity.ctime_ns.to_le_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

// Whether a file (or link) under `root` changed since `since_ns` (its
// ctime is later): a deletion cut short never resumes over a write made
// since its proof. Directories are not judged: the deletion itself moves
// their times.
fn written_since(root: &Path, since_ns: i128) -> Result<bool> {
    Ok(walk(root, true)?.iter().any(|entry| {
        !entry.meta.is_dir()
            && crate::freshness::StatIdentity::from_metadata(&entry.meta).ctime_ns > since_ns
    }))
}

// Whether a process has its working directory or an open file under
// `root` (review of #289, finding 2); `true` where it cannot be proved.
fn in_use(root: &Path) -> Result<bool> {
    Ok(crate::procs::inside(&[root])?.is_none_or(|pids| !pids.is_empty()))
}

// Now, in nanoseconds since the epoch.
fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i128::try_from(since.as_nanos()).unwrap_or(i128::MAX)
        })
}

// A file's bytes, counted as destination verification reads.
fn read_counted(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).refuse_at("git_carry::beside::retire::read")?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, bytes.len());
    Ok(bytes)
}

// `bytes` without a sealed bulkload hand-off block at byte 0.
pub(super) fn unblocked(bytes: &[u8]) -> &[u8] {
    match render::marker(bytes) {
        render::Marker::Block(len) => match bytes.get(..len) {
            Some(block) if render::sealed(block) => bytes.get(len..).unwrap_or_default(),
            _ => bytes,
        },
        render::Marker::None | render::Marker::Malformed => bytes,
    }
}

// Whether two regular files hold the same bytes (read in step, counted);
// `targets` allows a sealed hand-off block at byte 0 of either.
fn same_bytes(a: &Path, b: &Path, targets: bool) -> Result<bool> {
    const SITE: &str = "git_carry::beside::retire::same_bytes";
    if targets {
        let (left, right) = (read_counted(a)?, read_counted(b)?);
        return Ok(unblocked(&left) == unblocked(&right));
    }
    let (Ok(left), Ok(right)) = (fs::File::open(a), fs::File::open(b)) else {
        return Ok(false);
    };
    if left.metadata().refuse_at(SITE)?.len() != right.metadata().refuse_at(SITE)?.len() {
        return Ok(false);
    }
    let mut left = std::io::BufReader::with_capacity(1 << 16, left);
    let mut right = std::io::BufReader::with_capacity(1 << 16, right);
    let mut one = vec![0u8; 1 << 16];
    let mut two = vec![0u8; 1 << 16];
    loop {
        let read = left.read(&mut one).refuse_at(SITE)?;
        let mut filled = 0;
        while filled < read {
            let more = right
                .read(two.get_mut(filled..read).unwrap_or_default())
                .refuse_at(SITE)?;
            if more == 0 {
                return Ok(false);
            }
            filled += more;
        }
        crate::counters::add_len(crate::counters::Counter::DestVerifyRead, read * 2);
        if read == 0 {
            return Ok(right.read(&mut two).refuse_at(SITE)? == 0);
        }
        if one.get(..read) != two.get(..read) {
            return Ok(false);
        }
    }
}

// Why the entry `entry` of the beside checkout is not held by the live
// tree at `live`; `None` when it is.
// A kept captured file the advance landed as a file-level beside
// (`<path>SUFFIX`, `left`) is held there.
fn entry_differs(beside: &Path, live: &Path, entry: &Entry, left: &Left) -> Result<Option<String>> {
    const SITE: &str = "git_carry::beside::retire::entry";
    let shown = || entry.relative.escape_ascii().to_string();
    let name = entry
        .relative
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or_default();
    if name == b".git" {
        return Ok(Some(format!("nested:{}", shown())));
    }
    let relative = OsStr::from_bytes(&entry.relative);
    let target = !entry.relative.contains(&b'/')
        && super::super::converge::handoff::TARGETS
            .iter()
            .any(|target| target.as_bytes() == entry.relative.as_slice());
    let Some(there) = lstat(&live.join(relative))? else {
        // A target the hand-off created at the beside checkout (a sealed
        // block and nothing else, or the link to the other target) is
        // bulkload's own, not the source's.
        let created = target
            && if entry.meta.file_type().is_symlink() {
                let to = fs::read_link(beside.join(relative)).refuse_at(SITE)?;
                super::super::converge::handoff::TARGETS
                    .iter()
                    .any(|other| Path::new(other) == to && other.as_bytes() != entry.relative)
            } else {
                entry.meta.is_file() && unblocked(&read_counted(&beside.join(relative))?).is_empty()
            };
        return Ok((!created).then(|| format!("missing:{}", shown())));
    };
    let held = |at: &Path, there: &fs::Metadata| -> Result<bool> {
        let meta = &entry.meta;
        Ok(if meta.is_dir() {
            there.is_dir()
        } else if meta.file_type().is_symlink() {
            there.file_type().is_symlink()
                && fs::read_link(beside.join(relative)).refuse_at(SITE)?
                    == fs::read_link(at).refuse_at(SITE)?
        } else if meta.is_file() {
            there.is_file()
                && (meta.mode() & 0o111 == 0) == (there.mode() & 0o111 == 0)
                && same_bytes(&beside.join(relative), at, target)?
        } else {
            false
        })
    };
    if held(&live.join(relative), &there)? {
        return Ok(None);
    }
    if let Some(file_beside) = left.beside_of(&entry.relative) {
        let at = live.join(OsStr::from_bytes(&file_beside));
        if let Some(there) = lstat(&at)? {
            if held(&at, &there)? {
                return Ok(None);
            }
        }
    }
    Ok(Some(format!("differs:seat={}", shown())))
}

// The commits a reflog file names (each line's new value), when it exists.
fn reflog_commits(path: &Path) -> Result<Vec<String>> {
    let Some(meta) = lstat(path)? else {
        return Ok(Vec::new());
    };
    if !meta.is_file() {
        return Ok(Vec::new());
    }
    let text = read_counted(path)?;
    Ok(text
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let mut fields = line.split(|byte| *byte == b' ');
            fields.next()?;
            let new = std::str::from_utf8(fields.next()?).ok()?;
            (super::super::oid(new) && new.bytes().any(|byte| byte != b'0')).then(|| new.to_owned())
        })
        .collect())
}

// Whether every one of `commits` is in `live`'s object store and reachable
// from one of its refs (a Git child that fails says no).
pub(super) fn reachable(live: &Path, commits: &[String]) -> bool {
    commits.chunks(1024).all(|chunk| {
        let mut command = git(live);
        command.args(["rev-list", "--max-count=1"]);
        command.args(chunk);
        command.args(["--not", "--all"]);
        output(&mut command).is_ok_and(|listed| listed.iter().all(u8::is_ascii_whitespace))
    })
}

// The commits every reflog file under `logs` names.
pub(super) fn reflogs(logs: &Path) -> Result<Vec<String>> {
    let mut commits = Vec::new();
    if lstat(logs)?.is_some_and(|meta| meta.is_dir()) {
        for entry in walk(logs, true)? {
            if entry.meta.is_file() {
                commits.extend(reflog_commits(
                    &logs.join(OsStr::from_bytes(&entry.relative)),
                )?);
            }
        }
    }
    Ok(commits)
}

// The remote URLs `repo`'s own configuration names.
fn remote_urls(repo: &Path) -> Result<BTreeSet<Vec<u8>>> {
    let out = git(repo)
        .args([
            "config",
            "--local",
            "-z",
            "--get-regexp",
            r"^remote\..*\.url$",
        ])
        .output()
        .refuse_at(SITE)?;
    match out.status.code() {
        Some(0) => {}
        Some(1) => return Ok(BTreeSet::new()),
        _ => return Err(super::super::estimate::child_failed(&out.stderr)),
    }
    Ok(out
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let at = entry.iter().position(|byte| *byte == b'\n')?;
            entry.get(at + 1..).map(<[u8]>::to_vec)
        })
        .collect())
}

// The per-machine keys `git init` writes, never a difference between two
// copies of one repository.
const MACHINE_KEYS: &[&[u8]] = &[
    b"core.repositoryformatversion",
    b"core.filemode",
    b"core.bare",
    b"core.logallrefupdates",
    b"core.ignorecase",
    b"core.precomposeunicode",
    b"core.symlinks",
];

// The entries (`key\nvalue`, or `key` alone) a `git config ... --list`
// child prints, in file order.
fn listed_entries(out: &std::process::Output) -> Result<Vec<Vec<u8>>> {
    match out.status.code() {
        Some(0) => {}
        Some(1) => return Ok(Vec::new()),
        _ => return Err(super::super::estimate::child_failed(&out.stderr)),
    }
    Ok(out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

// `repo`'s own configuration entries, in file order: what goes with it.
fn config_entries(repo: &Path) -> Result<Vec<Vec<u8>>> {
    let out = git(repo)
        .args(["config", "--local", "-z", "--list"])
        .output()
        .refuse_at(SITE)?;
    listed_entries(&out)
}

// The configuration the live repository at `live` is in effect run with
// (the verification of #289, r7): its own with its includes, and the
// global and system files the operator's Git reads (`$XDG_CONFIG_HOME` or
// `~/.config` `git/config`, `~/.gitconfig`, `/etc/gitconfig`), each with
// its includes. Bulkload's own Git children run without the global and
// system files, so they are read by name here.
fn effective_entries(live: &Path) -> Result<BTreeSet<Vec<u8>>> {
    let out = git(live)
        .args(["config", "--local", "--includes", "-z", "--list"])
        .output()
        .refuse_at(SITE)?;
    let mut entries: BTreeSet<Vec<u8>> = listed_entries(&out)?.into_iter().collect();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let files = [
        xdg.map(|xdg| xdg.join("git/config")),
        home.map(|home| home.join(".gitconfig")),
        Some(PathBuf::from("/etc/gitconfig")),
    ];
    for file in files.into_iter().flatten() {
        if fs::metadata(&file).is_ok_and(|meta| meta.is_file()) {
            let out = git(live)
                .args(["config", "--includes", "-z", "--list", "--file"])
                .arg(&file)
                .output()
                .refuse_at(SITE)?;
            entries.extend(listed_entries(&out)?);
        }
    }
    Ok(entries)
}

// Identity and signing keys (the verification of #289, r7) are never
// carried: a local identity the live configuration does not hold (sting's
// global one, with its signing key) keeps the beside.
fn identity_key(key: &[u8]) -> bool {
    key.starts_with(b"user.")
        || key.starts_with(b"gpg.")
        || matches!(
            key,
            b"commit.gpgsign" | b"tag.gpgsign" | b"push.gpgsign" | b"tag.forcesignannotated"
        )
}

// Whether a Git operation is in progress, or a Git lock held, in the git
// directory `dir`.
fn operation_in(dir: &Path) -> Result<bool> {
    use super::super::converge::advance::{IN_PROGRESS, LOCKS};
    for name in IN_PROGRESS.iter().chain(LOCKS) {
        if lstat(&dir.join(name))?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What the live repository takes from a standalone beside checkout
/// before the beside is retirable (the coordinator's decision on #289):
/// configuration entries whose key the live configuration lacks, added
/// create-only, and the `info/exclude` patterns it lacks, appended
/// create-only by the exclude union's rule.
#[derive(Debug, Default)]
struct Carry {
    /// Each key the live configuration lacks, with every value the beside
    /// holds for it, in order: all go in, or none.
    config: Vec<(String, Vec<String>)>,
    exclude: Vec<Vec<u8>>,
}

/// What a retirement carried into the live repository: configuration
/// entries added and exclude lines appended, for the receipt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Carried {
    pub config: u64,
    pub exclude: u64,
}

// A configuration entry's key (`key\nvalue`, or `key` alone).
fn key_of(entry: &[u8]) -> &[u8] {
    entry
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default()
}

// Whether a bulkload ledger at `common` other than an advance's of
// `source` records its exclude (another label's workspace ledger, a
// landing's, or a ref ledger's exclude row): that converge owns it, and an
// append would fail its proof. An advanced checkout's exclude is never
// proved.
fn exclude_proved(common: &Path, source: &str) -> Result<bool> {
    let root = common.join(landed::LEDGER_ROOT);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        if !entry.file_type().refuse_at(SITE)?.is_dir() {
            continue;
        }
        let label = entry.path();
        let advanced = entry.file_name().as_bytes() == source.as_bytes()
            && landed::exists(&label.join(super::super::converge::advance::ADVANCED))?;
        if !advanced && landed::exists(&label.join(landed::WORKSPACE_LANDED))? {
            return Ok(true);
        }
        if landed::read::<landed::RefLedger>(&label.join(landed::REFS_LANDED))?
            .is_some_and(|ledger| ledger.exclude.is_some())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

// Why a standalone beside checkout's own Git state would be lost with it
// (review of #289, finding 5), or what the live repository must take from
// it first (the coordinator's decision on #289). Kept, typed: an operation
// in progress (`in-progress`), a `modules` directory (`modules`), a
// configuration entry whose key the live configuration holds with
// another value, or one not safe to carry (`configuration:<key>`, the
// value never named), exclude patterns no pure union carries or that
// another converge proves (`exclude`). Otherwise the entries whose key the
// live configuration lacks, and the patterns its exclude lacks.
fn state_differs(
    beside: &Path,
    live: &Path,
    source: &str,
) -> Result<std::result::Result<Carry, String>> {
    let dir = beside.join(".git");
    if operation_in(&dir)? {
        return Ok(Err("in-progress".to_owned()));
    }
    let modules = dir.join("modules");
    if let Some(meta) = lstat(&modules)? {
        if !meta.is_dir() || fs::read_dir(&modules).refuse_at(SITE)?.next().is_some() {
            return Ok(Err("modules".to_owned()));
        }
    }
    // Decided per key against the live repository's effective
    // configuration (the verification of #289, r6 and r7).
    let theirs = effective_entries(live)?;
    let keys: BTreeSet<&[u8]> = theirs.iter().map(|entry| key_of(entry)).collect();
    let mut by_key = BTreeMap::<Vec<u8>, Vec<Vec<u8>>>::new();
    for entry in config_entries(beside)? {
        let key = key_of(&entry).to_vec();
        if !MACHINE_KEYS.contains(&key.as_slice()) {
            by_key.entry(key).or_default().push(entry);
        }
    }
    let mut carry = Carry::default();
    for (key, entries) in &by_key {
        if entries.iter().all(|entry| theirs.contains(entry)) {
            continue;
        }
        let kept = || Ok(Err(format!("configuration:{}", key.escape_ascii())));
        if identity_key(key) || keys.contains(key.as_slice()) {
            return kept();
        }
        let Ok(name) = std::str::from_utf8(key) else {
            return kept();
        };
        let mut values = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(Ok(value)) = entry.get(key.len() + 1..).map(std::str::from_utf8) else {
                return kept();
            };
            if !super::super::safe_configuration_value(live, name, value) {
                return kept();
            }
            values.push(value.to_owned());
        }
        carry.config.push((name.to_owned(), values));
    }
    let common = landed::worktree_git_dir(live)?
        .map(|git_dir| landed::common_dir(&git_dir))
        .transpose()?
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let read = |path: &Path| -> Result<Vec<u8>> {
        match fs::read(path) {
            Ok(bytes) => Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(crate::refuse::io(&error, SITE)),
        }
    };
    let existing = read(&common.join("info/exclude"))?;
    match super::super::exclude_union::missing(&existing, &read(&dir.join("info/exclude"))?) {
        Err(_) => return Ok(Err("exclude".to_owned())),
        Ok(lines) if lines.is_empty() => {}
        Ok(_) if exclude_proved(&common, source)? => return Ok(Err("exclude".to_owned())),
        Ok(lines) => carry.exclude = lines,
    }
    Ok(Ok(carry))
}

// Add the configuration a retirement planned (`config`, recorded in its
// STATE record first) to the live repository at `live`, create-only and
// all of a key's values or none (the verification of #289, r6): a key
// whose effective values are some of the planned ones (this retirement's,
// a crash cut short) gets the rest; one holding any other value keeps the
// beside, nothing of that key written. Returns how many values went in.
fn carry_config(
    live: &Path,
    config: &[(String, Vec<String>)],
) -> Result<std::result::Result<u64, String>> {
    let mut added = 0;
    for (key, values) in config {
        let theirs = effective_entries(live)?;
        let held: Vec<&[u8]> = theirs
            .iter()
            .filter(|entry| key_of(entry) == key.as_bytes())
            .map(|entry| entry.get(key.len() + 1..).unwrap_or_default())
            .collect();
        if held
            .iter()
            .any(|value| !values.iter().any(|planned| planned.as_bytes() == *value))
        {
            return Ok(Err(format!("configuration:{}", key.escape_default())));
        }
        for value in values {
            if held.contains(&value.as_bytes()) {
                continue;
            }
            output(git(live).args(["config", "--local", "--add", key, value]))?;
            added += 1;
            fault_point!(EstateAdvanceRetireCarried);
        }
    }
    Ok(Ok(added))
}

// Carry the exclude patterns a standalone beside holds and the live
// exclude lacks through the exclude union (its lock, its STATE record
// `exclude_record` for `item`, its crash settle). `Err` keeps the beside,
// with why. Returns how many lines went in.
fn carry_exclude(
    live: &Path,
    carry: &Carry,
    (exclude_record, item, beside): (&Path, &str, &Path),
) -> Result<std::result::Result<u64, String>> {
    let mut appended = 0;
    if !carry.exclude.is_empty() {
        let common = landed::worktree_git_dir(live)?
            .map(|git_dir| landed::common_dir(&git_dir))
            .transpose()?
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let path = common.join("info/exclude");
        let _held = super::super::exclude_union::lock(&common)?;
        super::super::exclude_union::settle(exclude_record, &path)?;
        let existing = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        };
        let captured = match fs::read(beside.join(".git/info/exclude")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        };
        let Ok(lines) = super::super::exclude_union::missing(&existing, &captured) else {
            return Ok(Err("exclude".to_owned()));
        };
        if !lines.is_empty() {
            match super::super::exclude_union::apply(
                (exclude_record, item),
                &path,
                &existing,
                &lines,
            ) {
                Ok(_) => {}
                Err(BulkloadRefusal::GitIgnorePolicyConflict) => {
                    return Ok(Err("exclude".to_owned()))
                }
                Err(other) => return Err(other),
            }
            appended = u64::try_from(lines.len()).unwrap_or(u64::MAX);
        }
    }
    Ok(Ok(appended))
}

// The refs (name to value) the live repository holds that a standalone
// beside checkout's refs may stand for: its own, those under the beside
// namespace, and the current capture's carried refs (its provenance, named
// by the live checkout's workspace ledger).
fn live_names(live: &Path, source: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let mut names = BTreeMap::<String, Vec<String>>::new();
    let (native, _) = super::super::native_refs::native_refs(live)?;
    for (name, value) in native {
        names.entry(name).or_default().push(value);
    }
    let ledger = landed::worktree_git_dir(live)?
        .map(|git_dir| {
            landed::read::<landed::LandedWorkspace>(
                &landed::ledger_dir(&git_dir, source).join(landed::WORKSPACE_LANDED),
            )
        })
        .transpose()?
        .flatten();
    if let Some(ledger) = ledger {
        for (name, value) in super::super::native_refs::carried(live, source, &ledger.instance)? {
            names.entry(name).or_default().push(value);
        }
    }
    Ok(names)
}

// Why removing the beside checkout's Git would lose something; `None`
// when it would not. `admin` is a linked landing's administration
// directory (its refs and reflogs are the live repository's but its own
// per-worktree ones); `None` for a standalone checkout, whose every ref
// must be one the live repository holds by name (its own, under the
// namespace `ns`, or the capture's provenance), whose every commit (refs,
// stash, every reflog) must be reachable there, with no hook of its own
// and no remote the live repository lacks.
// Whether a linked checkout's administration directory `admin` holds a
// per-worktree ref of its own (`refs/bisect`, `refs/worktree`, ...).
pub(super) fn worktree_refs(admin: &Path) -> Result<bool> {
    let own = admin.join("refs");
    Ok(lstat(&own)?.is_some_and(|meta| meta.is_dir()) && !walk(&own, true)?.is_empty())
}

fn git_differs(
    beside: &Path,
    live: &Path,
    admin: Option<&Path>,
    (ns, source): (&str, &str),
) -> Result<std::result::Result<Carry, String>> {
    let mut commits = Vec::new();
    let mut carry = Carry::default();
    if let Some(admin) = admin {
        if lstat(&admin.join("locked"))?.is_some() {
            return Ok(Err("locked".to_owned()));
        }
        if operation_in(admin)? {
            return Ok(Err("in-progress".to_owned()));
        }
        if worktree_refs(admin)? {
            return Ok(Err("worktree-refs".to_owned()));
        }
        commits.extend(reflogs(&admin.join("logs"))?);
    } else {
        if super::super::converge::worktree_census(beside)?.len() > 1 {
            return Ok(Err("worktrees".to_owned()));
        }
        let hooks = beside.join(".git/hooks");
        if lstat(&hooks)?.is_some_and(|meta| meta.is_dir())
            && walk(&hooks, true)?
                .iter()
                .any(|entry| !entry.meta.is_dir() && !entry.relative.ends_with(b".sample"))
        {
            return Ok(Err("hooks".to_owned()));
        }
        let urls = remote_urls(live)?;
        if !remote_urls(beside)?.is_subset(&urls) {
            return Ok(Err("remotes".to_owned()));
        }
        carry = match state_differs(beside, live, source)? {
            Ok(carry) => carry,
            Err(why) => return Ok(Err(why)),
        };
        let names = live_names(live, source)?;
        let all = |repo: &Path| -> Result<BTreeMap<String, String>> {
            let listed =
                output(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
            Ok(String::from_utf8_lossy(&listed)
                .lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(value, name)| (name.to_owned(), value.to_owned()))
                .collect())
        };
        let live_refs = all(live)?;
        let listed =
            output(git(beside).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
        for line in String::from_utf8_lossy(&listed).lines() {
            let Some((value, name)) = line.split_once(' ') else {
                return Ok(Err("unreachable".to_owned()));
            };
            // Provenance: held by the live repository by name and value
            // (the same capture's), else its commit must be reachable
            // there (review of #289, finding 5).
            if name.starts_with("refs/carry") {
                if live_refs.get(name).map(String::as_str) != Some(value) {
                    commits.push(value.to_owned());
                }
                continue;
            }
            // The stash's entries are decided by commit below.
            if name != "refs/stash" {
                let spaced = name
                    .strip_prefix("refs/")
                    .map(|rest| format!("refs/{ns}/{rest}"));
                let held = |at: &str| {
                    names
                        .get(at)
                        .is_some_and(|values| values.iter().any(|held| held == value))
                };
                if !held(name) && !spaced.as_deref().is_some_and(held) {
                    return Ok(Err(format!("refs:{name}")));
                }
            }
            // A tag is peeled by the reachability walk; a ref to a tree or
            // a blob fails it, and the checkout is kept.
            commits.push(value.to_owned());
        }
        for entry in super::super::native_refs::stash_reflog(beside)? {
            commits.push(entry.oid);
        }
        commits.extend(reflogs(&beside.join(".git/logs"))?);
    }
    commits.sort();
    commits.dedup();
    if !reachable(live, &commits) {
        return Ok(Err("unreachable".to_owned()));
    }
    Ok(Ok(carry))
}

// Why the live tree at `live` does not equal the beside checkout at
// `beside` (module docs), or, when it does, what the live repository must
// take from a standalone one first ([`Carry`]). `left` is what the advance
// left to the live tree (its own untracked files), `ns` the beside
// namespace and `source` the SOURCE label. Read-only.
fn differs(
    beside: &Path,
    live: &Path,
    (left, ns, source): (&Left, &str, &str),
) -> Result<std::result::Result<Carry, String>> {
    let admin = linked_admin(beside)?;
    if let Some(admin) = &admin {
        if !admin_owned(beside, live, admin)? {
            return Ok(Err("admin-foreign".to_owned()));
        }
    }
    if head_of(beside)?.oid != head_of(live)?.oid {
        return Ok(Err("differs:head".to_owned()));
    }
    if index_digest(beside, None)? != index_digest(live, None)? {
        return Ok(Err("differs:index".to_owned()));
    }
    let carry = match git_differs(beside, live, admin.as_deref(), (ns, source))? {
        Ok(carry) => carry,
        Err(why) => return Ok(Err(why)),
    };
    let entries = walk(beside, false)?;
    for entry in &entries {
        if let Some(why) = entry_differs(beside, live, entry, left)? {
            return Ok(Err(why));
        }
    }
    let held: std::collections::BTreeSet<&[u8]> = entries
        .iter()
        .map(|entry| entry.relative.as_slice())
        .collect();
    let listed = output(git(live).args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]))?;
    for path in listed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        // An index entry whose file the worktree lacks (a deletion not
        // staged) is the index's, compared above.
        if !held.contains(path)
            && !left.own_untracked(path)
            && !left.is_file_beside(path)
            && lstat(&live.join(OsStr::from_bytes(path)))?.is_some()
        {
            return Ok(Err(format!("extra:{}", path.escape_ascii())));
        }
    }
    Ok(Ok(carry))
}

// Whether `admin`, the administration directory a linked beside
// checkout's `.git` names, is the live repository's own registration of
// that checkout (review of #289, finding 4): directly under its common
// directory's `worktrees/`, its `gitdir` naming `<beside>/.git`.
fn admin_owned(beside: &Path, live: &Path, admin: &Path) -> Result<bool> {
    let Some(common) = landed::worktree_git_dir(live)?
        .map(|git_dir| landed::common_dir(&git_dir))
        .transpose()?
    else {
        return Ok(false);
    };
    let worktrees = fs::canonicalize(common.join("worktrees")).refuse_at(SITE)?;
    if admin.parent() != Some(worktrees.as_path()) {
        return Ok(false);
    }
    let named = match fs::read(admin.join("gitdir")) {
        Ok(named) => named,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    let dotgit = fs::canonicalize(beside.join(".git")).refuse_at(SITE)?;
    Ok(fs::canonicalize(path_of(named.trim_ascii_end())).is_ok_and(|named| named == dotgit))
}

// A linked checkout's administration directory (what its `.git` file
// names); `None` for a standalone checkout.
pub(super) fn linked_admin(beside: &Path) -> Result<Option<PathBuf>> {
    if lstat(&beside.join(".git"))?.is_some_and(|meta| meta.is_dir()) {
        return Ok(None);
    }
    landed::worktree_git_dir(beside)?
        .map(|dir| fs::canonicalize(dir).refuse_at("git_carry::beside::retire::admin"))
        .transpose()
}

// The private name a retirement renames `beside` to, in its directory.
fn temp_of(beside: &Path, capture: &[u8; 32]) -> Result<PathBuf> {
    let parent = beside.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(beside.as_os_str().as_bytes());
    hasher.update(capture);
    let tag = hasher.finalize().to_hex();
    Ok(parent.join(format!(
        ".bulkload-retire-{}",
        tag.get(..16).unwrap_or_default()
    )))
}

fn cname(path: &Path) -> Result<CString> {
    CString::new(
        path.file_name()
            .ok_or(BulkloadRefusal::PathEscapesRoot)?
            .as_bytes(),
    )
    .map_err(|_| BulkloadRefusal::PathNotPortable)
}

// Rename `from` to `to` (one directory) never over anything, then sync it.
fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    const SITE: &str = "git_carry::beside::retire::rename";
    let parent = from.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
    let directory = fs::File::open(parent).refuse_at(SITE)?;
    crate::io::sys::rename_exclusive_at(&directory, &cname(from)?, &directory, &cname(to)?)
        .refuse_at(SITE)?;
    landed::sync_dir(parent)
}

/// Prove the beside checkout at `beside` equal to the live tree at `live`
/// and retire it (module docs).
///
/// The retirement is recorded at `record` (STATE); `exclude_record` is the
/// item's exclude-union record (STATE), for patterns a standalone beside
/// carries into the live exclude. The caller has established the checkout
/// is this item's landing of `capture`; `left` is what the advance left to
/// the live tree. Returns what became of it and what the live repository
/// took from it first.
///
/// # Errors
/// Whatever reading either tree, the carry, the rename, the removal or a
/// record write refuses.
pub fn retire(
    (record, exclude_record): (&Path, &Path),
    beside: &Path,
    live: &Path,
    (item, capture): (&str, [u8; 32]),
    (left, ns, source): (&Left, &str, &str),
) -> Result<(Retired, Carried)> {
    let mut carried = Carried::default();
    let retired = retire_carrying(
        (record, exclude_record),
        (beside, live),
        (item, capture),
        (left, ns, source),
        &mut carried,
    )?;
    Ok((retired, carried))
}

fn retire_carrying(
    (record, exclude_record): (&Path, &Path),
    (beside, live): (&Path, &Path),
    (item, capture): (&str, [u8; 32]),
    (left, ns, source): (&Left, &str, &str),
    carried: &mut Carried,
) -> Result<Retired> {
    // When the proof began, with a margin for a filesystem's coarse
    // timestamps: a file written after it has a later ctime.
    let started = now_ns() - 2_000_000_000;
    let admin = linked_admin(beside)?;
    // The prints before the proof and after it: a write while the proof
    // read the tree (a session working there) keeps it.
    let before = prints(beside, admin.as_deref())?;
    if in_use(beside)? {
        return Ok(Retired::Kept("in-use".to_owned()));
    }
    let carry = match differs(beside, live, (left, ns, source))? {
        Ok(carry) => carry,
        Err(why) => return Ok(Retired::Kept(why)),
    };
    if prints(beside, admin.as_deref())? != before {
        return Ok(Retired::Kept("raced".to_owned()));
    }

    let meta = fs::symlink_metadata(beside).refuse_at("git_carry::beside::retire")?;
    let admin_gitdir = match &admin {
        Some(admin) => fs::read(admin.join("gitdir"))
            .refuse_at("git_carry::beside::retire")?
            .trim_ascii_end()
            .to_vec(),
        None => Vec::new(),
    };
    let temp = temp_of(beside, &capture)?;
    if lstat(&temp)?.is_some() {
        return Ok(Retired::Kept(format!(
            "occupied:{}",
            temp.as_os_str().as_bytes().escape_ascii()
        )));
    }
    let intent = RetireIntent {
        item: item.to_owned(),
        capture,
        path: beside.as_os_str().as_bytes().to_vec(),
        temp: temp.as_os_str().as_bytes().to_vec(),
        admin: admin
            .as_ref()
            .map(|dir| dir.as_os_str().as_bytes().to_vec()),
        admin_gitdir,
        dev: meta.dev(),
        ino: meta.ino(),
        prints: before,
        deleting: false,
        recorded_ns: started,
        live: live.as_os_str().as_bytes().to_vec(),
        config: carry.config.clone(),
    };
    landed::write(record, &intent)?;
    fault_point!(EstateAdvanceRetireAfterIntent);
    // What the live repository lacks of a standalone beside's state goes
    // in first, create-only (the coordinator's decision on #289): the
    // configuration as the record plans it, then the exclude patterns.
    let done = match carry_config(live, &carry.config)? {
        Ok(added) => carry_exclude(live, &carry, (exclude_record, item, beside))?
            .map(|appended| (added, appended)),
        Err(why) => Err(why),
    };
    match done {
        Ok((config, exclude)) => *carried = Carried { config, exclude },
        Err(why) => {
            landed::remove(record)?;
            return Ok(Retired::Kept(why));
        }
    }
    // No process inside it, again, right before it is renamed.
    if in_use(beside)? {
        landed::remove(record)?;
        return Ok(Retired::Kept("in-use".to_owned()));
    }
    rename_exclusive(beside, &temp)?;
    fault_point!(EstateAdvanceRetireAfterRename);
    finish(record, intent)
}

/// Remove the beside tree at `beside` once its caller proved it holds
/// nothing the live checkout at `live` lacks (`estate-beside-retire`,
/// [`super::subset`]), its prints `before` (`prints(beside, admin)`) taken
/// before that proof began at `started_ns`: the crash-safe removal of the
/// module docs (the record at `record`, no process inside, the rename to a
/// private name, the re-proof, the tree, a linked landing's
/// administration, the record). `capture` names the private name; `item`
/// is recorded. Nothing is carried: the caller carried it first.
///
/// # Errors
/// Whatever the rename, the removal or a record write refuses.
pub(super) fn remove_proved(
    record: &Path,
    (beside, live): (&Path, &Path),
    (item, capture): (&str, [u8; 32]),
    (before, started_ns): ([u8; 32], i128),
) -> Result<Retired> {
    let admin = linked_admin(beside)?;
    if prints(beside, admin.as_deref())? != before {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    if in_use(beside)? {
        return Ok(Retired::Kept("in-use".to_owned()));
    }
    let meta = fs::symlink_metadata(beside).refuse_at(SITE)?;
    let admin_gitdir = match &admin {
        Some(admin) => fs::read(admin.join("gitdir"))
            .refuse_at(SITE)?
            .trim_ascii_end()
            .to_vec(),
        None => Vec::new(),
    };
    let temp = temp_of(beside, &capture)?;
    if lstat(&temp)?.is_some() {
        return Ok(Retired::Kept(format!(
            "occupied:{}",
            temp.as_os_str().as_bytes().escape_ascii()
        )));
    }
    let intent = RetireIntent {
        item: item.to_owned(),
        capture,
        path: beside.as_os_str().as_bytes().to_vec(),
        temp: temp.as_os_str().as_bytes().to_vec(),
        admin: admin
            .as_ref()
            .map(|dir| dir.as_os_str().as_bytes().to_vec()),
        admin_gitdir,
        dev: meta.dev(),
        ino: meta.ino(),
        prints: before,
        deleting: false,
        recorded_ns: started_ns,
        live: live.as_os_str().as_bytes().to_vec(),
        ..RetireIntent::default()
    };
    landed::write(record, &intent)?;
    rename_exclusive(beside, &temp)?;
    finish(record, intent)
}

/// Now, in nanoseconds since the epoch, less the margin a proof's start
/// takes for a filesystem's coarse timestamps ([`RetireIntent::recorded_ns`]).
pub(super) fn proof_start_ns() -> i128 {
    now_ns() - 2_000_000_000
}

// The renamed tree goes back to the beside path (when nothing is there
// since), the record with it; `why` says what kept it.
fn put_back(record: &Path, intent: &RetireIntent, why: &str) -> Result<Retired> {
    let (path, temp) = (path_of(&intent.path), path_of(&intent.temp));
    if lstat(&path)?.is_some() {
        // Something new at the beside path since: both kept, the record
        // names the renamed tree.
        return Ok(Retired::Kept(format!(
            "raced:{}",
            intent.temp.escape_ascii()
        )));
    }
    rename_exclusive(&temp, &path)?;
    landed::remove(record)?;
    Ok(Retired::Kept(why.to_owned()))
}

// From the rename on: re-proved before its deletion (review of #289,
// finding 2: its prints, or for a deletion cut short no file written since
// the proof; no process inside), marked deleting, the tree deleted, a
// linked landing's administration removed, the record removed.
fn finish(record: &Path, mut intent: RetireIntent) -> Result<Retired> {
    let temp = path_of(&intent.temp);
    let admin = intent.admin.as_deref().map(path_of);
    if lstat(&temp)?.is_some() {
        let whole = prints(&temp, admin.as_deref())? == intent.prints;
        if !whole && (!intent.deleting || written_since(&temp, intent.recorded_ns)?) {
            return put_back(
                record,
                &intent,
                if intent.deleting {
                    "written-since"
                } else {
                    "raced"
                },
            );
        }
        if in_use(&temp)? {
            // A tree a deletion cut short stays under its private name,
            // the record pending (closure lists it); a whole one goes back.
            if whole {
                return put_back(record, &intent, "in-use");
            }
            return Ok(Retired::Kept(format!(
                "in-use:{}",
                intent.temp.escape_ascii()
            )));
        }
        if !intent.deleting {
            intent.deleting = true;
            landed::write(record, &intent)?;
        }
        fault_point!(EstateAdvanceRetireMid);
        // The renamed tree is the one proved (its directory's identity), or
        // left as it is.
        let meta = fs::symlink_metadata(&temp).refuse_at(SITE)?;
        if meta.dev() != intent.dev || meta.ino() != intent.ino || !meta.is_dir() {
            return Ok(Retired::Kept(format!(
                "replaced:{}",
                intent.temp.escape_ascii()
            )));
        }
        // Captured directory modes may forbid removing what they hold (a
        // read-only module cache): the renamed tree is this retirement's
        // own, so its directories are made writable first. A deletion that
        // still fails keeps the record (deleting) for the next pass.
        if let Err(refusal) =
            writable(&temp).and_then(|()| fs::remove_dir_all(&temp).refuse_at(SITE))
        {
            return Ok(Retired::Kept(format!("delete-failed:{refusal}")));
        }
        if let Some(parent) = temp.parent() {
            landed::sync_dir(parent)?;
        }
    }
    if let Some(admin) = &admin {
        if !remove_admin(admin, &intent.admin_gitdir)? {
            return Ok(Retired::Kept(format!(
                "admin-unremoved:{}",
                admin.as_os_str().as_bytes().escape_ascii()
            )));
        }
    }
    landed::remove(record)?;
    Ok(Retired::Retired)
}

// Give every directory under `root` (and `root`) its owner's write and
// search bits, never following a link.
fn writable(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut pending = vec![root.to_path_buf()];
    while let Some(at) = pending.pop() {
        let meta = fs::symlink_metadata(&at).refuse_at(SITE)?;
        if !meta.is_dir() {
            continue;
        }
        if meta.mode() & 0o700 != 0o700 {
            fs::set_permissions(
                &at,
                fs::Permissions::from_mode((meta.mode() & 0o7777) | 0o700),
            )
            .refuse_at(SITE)?;
        }
        for entry in fs::read_dir(&at).refuse_at(SITE)? {
            let entry = entry.refuse_at(SITE)?;
            if entry.file_type().refuse_at(SITE)?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

// Remove a retired linked landing's administration directory when it is
// one under a `worktrees/` directory whose `gitdir` still holds what it
// held when proved (`proved`: the proof checked it names the beside
// checkout, [`admin_owned`]), or is gone (a removal a crash cut short: the
// record names the directory); `false` when it is anything else now, left
// as it is (review of #289, finding 4).
fn remove_admin(admin: &Path, proved: &[u8]) -> Result<bool> {
    let Some(meta) = lstat(admin)? else {
        return Ok(true);
    };
    let registered = admin
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name.as_bytes() == b"worktrees");
    if !meta.is_dir() || !registered {
        return Ok(false);
    }
    match fs::read(admin.join("gitdir")) {
        Ok(named) if named.trim_ascii_end() != proved => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    }
    writable(admin)?;
    fs::remove_dir_all(admin).refuse_at(SITE)?;
    if let Some(parent) = admin.parent() {
        landed::sync_dir(parent)?;
    }
    Ok(true)
}

/// Finish the retirement STATE records at `record`, a crash cut short.
///
/// `None` when there is none, or when nothing had moved yet (the record is
/// removed; the caller proves again); else what became of the beside
/// landing.
///
/// # Errors
/// Whatever reading the record or the trees, the rename or the removal
/// refuses.
pub fn settle(record: &Path) -> Result<Option<Retired>> {
    let Some(intent) = landed::read::<RetireIntent>(record)? else {
        return Ok(None);
    };
    let (path, temp) = (path_of(&intent.path), path_of(&intent.temp));
    if !intent.deleting {
        match (lstat(&path)?.is_some(), lstat(&temp)?.is_some()) {
            (true, false) => {
                // Nothing moved; a configuration carry a crash cut short
                // is finished (a key holding anything else since is left
                // for the proof to keep the beside), then the caller
                // proves again.
                if !intent.config.is_empty() {
                    let _ = carry_config(&path_of(&intent.live), &intent.config)?;
                }
                landed::remove(record)?;
                return Ok(None);
            }
            (true, true) => {
                return Ok(Some(Retired::Kept(format!(
                    "raced:{}",
                    intent.temp.escape_ascii()
                ))));
            }
            (false, false) => {
                landed::remove(record)?;
                return Ok(Some(Retired::Kept("gone".to_owned())));
            }
            (false, true) => {}
        }
    }
    finish(record, intent).map(Some)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_sealed_block_is_the_one_difference_a_target_allows() {
        let body = "<!-- bulkload-handoff:v1 BEGIN id=0000000000000000 for=0123456789abcdef BULKLOAD-HANDOFF-DO-NOT-COMMIT -->\nhello\n<!-- bulkload-handoff:v1 END id=0000000000000000 -->\n\n";
        let id = blake3::hash(body.as_bytes()).to_hex();
        let block = body.replace("id=0000000000000000", &format!("id={}", &id.as_str()[..16]));
        let file = format!("{block}# captured\n");
        assert_eq!(unblocked(file.as_bytes()), b"# captured\n");
        // An edit inside the block: not sealed, not stripped.
        let edited = file.replace("hello", "hullo");
        assert_eq!(unblocked(edited.as_bytes()), edited.as_bytes());
        assert_eq!(unblocked(b"# captured\n"), b"# captured\n");
    }
}
