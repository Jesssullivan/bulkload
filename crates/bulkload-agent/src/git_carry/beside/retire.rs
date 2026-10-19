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
//!   bit; a bulkload hand-off block at byte 0 of `AGENTS.md` or
//!   `CLAUDE.md`, sealed as estate-handoff renders it, is the one
//!   difference allowed (either side);
//! - every tracked or untracked file of the live tree that is not ignored
//!   held by the beside checkout, but the untracked files the advance
//!   found there and left as they were, apart from every captured path
//!   (the live tree's own, as its ignored files are);
//! - nothing the beside checkout's Git holds lost with it: no nested
//!   repository under it, no worktree of its own, no lock; every commit
//!   its refs (provenance `refs/carry*` aside), its stash and its reflogs
//!   name reachable from the live repository's refs (a linked landing's:
//!   its per-worktree reflogs, and no per-worktree ref); a standalone
//!   one's every ref held by the live repository by name and value (its
//!   own, under the beside namespace, or in the capture's provenance), no
//!   hook of its own and no remote URL the live repository lacks.
//!
//! Any difference keeps the beside checkout and returns why
//! ([`Retired::Kept`]: `differs:head`, `differs:index`,
//! `differs:seat=<path>`, `missing:<path>`, `extra:<path>`,
//! `unreachable`, `refs:<name>`, `hooks`, `remotes`, `nested:<path>`,
//! `locked`, `worktrees`, `worktree-refs`, `raced`). The tree's prints
//! are taken before the proof and again after it: a write while the proof
//! read it (a session working there) keeps it. Nothing is written before
//! the proof.
//!
//! The removal is crash-safe and never loses either tree: the retirement
//! is recorded in STATE ([`RetireIntent`]: the path, a private name beside
//! it, the directory's identity and a digest of every entry's print), the
//! checkout is renamed to that private name (`estate.advance.
//! retire_after_rename`), re-proved by its prints (a write since the proof
//! puts it back: `raced`), marked deleting, a linked landing's
//! administration directory removed (its `gitdir` must name the beside
//! checkout; `estate.advance.retire_mid`), the renamed tree deleted (its
//! identity checked first), and the record removed. A rerun finishes from
//! the record ([`settle`]): nothing moved, the record goes and the caller
//! proves again; renamed, re-proved and finished, or put back; deleting,
//! finished.

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

fn lstat(path: &Path) -> Result<Option<fs::Metadata>> {
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
struct Entry {
    relative: Vec<u8>,
    meta: fs::Metadata,
}

// Every entry under `root` (never following a link), sorted by path; with
// `with_git`, the top-level `.git` and what is under it too.
fn walk(root: &Path, with_git: bool) -> Result<Vec<Entry>> {
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

// The digest of every entry's print under `root`, its `.git` included: a
// write anywhere under it since changes it.
fn prints(root: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload-retire-prints-v1\0");
    for entry in walk(root, true)? {
        let identity = crate::freshness::StatIdentity::from_metadata(&entry.meta);
        hasher.update(&(entry.relative.len() as u64).to_le_bytes());
        hasher.update(&entry.relative);
        hasher.update(&entry.meta.mode().to_le_bytes());
        hasher.update(&identity.ino.to_le_bytes());
        hasher.update(&identity.size.to_le_bytes());
        hasher.update(&identity.mtime_ns.to_le_bytes());
        hasher.update(&identity.ctime_ns.to_le_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

// A file's bytes, counted as destination verification reads.
fn read_counted(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).refuse_at("git_carry::beside::retire::read")?;
    crate::counters::add_len(crate::counters::Counter::DestVerifyRead, bytes.len());
    Ok(bytes)
}

// `bytes` without a sealed bulkload hand-off block at byte 0.
fn unblocked(bytes: &[u8]) -> &[u8] {
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
fn entry_differs(beside: &Path, live: &Path, entry: &Entry) -> Result<Option<String>> {
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
    let meta = &entry.meta;
    let same = if meta.is_dir() {
        there.is_dir()
    } else if meta.file_type().is_symlink() {
        there.file_type().is_symlink()
            && fs::read_link(beside.join(relative)).refuse_at(SITE)?
                == fs::read_link(live.join(relative)).refuse_at(SITE)?
    } else if meta.is_file() {
        there.is_file()
            && (meta.mode() & 0o111 == 0) == (there.mode() & 0o111 == 0)
            && same_bytes(&beside.join(relative), &live.join(relative), target)?
    } else {
        false
    };
    Ok((!same).then(|| format!("differs:seat={}", shown())))
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
fn reachable(live: &Path, commits: &[String]) -> bool {
    commits.chunks(1024).all(|chunk| {
        let mut command = git(live);
        command.args(["rev-list", "--max-count=1"]);
        command.args(chunk);
        command.args(["--not", "--all"]);
        output(&mut command).is_ok_and(|listed| listed.iter().all(u8::is_ascii_whitespace))
    })
}

// The commits every reflog file under `logs` names.
fn reflogs(logs: &Path) -> Result<Vec<String>> {
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
fn git_differs(
    beside: &Path,
    live: &Path,
    admin: Option<&Path>,
    (ns, source): (&str, &str),
) -> Result<Option<String>> {
    let mut commits = Vec::new();
    if let Some(admin) = admin {
        if lstat(&admin.join("locked"))?.is_some() {
            return Ok(Some("locked".to_owned()));
        }
        if lstat(&admin.join("refs"))?.is_some_and(|meta| meta.is_dir())
            && !walk(&admin.join("refs"), true)?.is_empty()
        {
            return Ok(Some("worktree-refs".to_owned()));
        }
        commits.extend(reflogs(&admin.join("logs"))?);
    } else {
        if super::super::converge::worktree_census(beside)?.len() > 1 {
            return Ok(Some("worktrees".to_owned()));
        }
        let hooks = beside.join(".git/hooks");
        if lstat(&hooks)?.is_some_and(|meta| meta.is_dir())
            && walk(&hooks, true)?
                .iter()
                .any(|entry| !entry.meta.is_dir() && !entry.relative.ends_with(b".sample"))
        {
            return Ok(Some("hooks".to_owned()));
        }
        let urls = remote_urls(live)?;
        if !remote_urls(beside)?.is_subset(&urls) {
            return Ok(Some("remotes".to_owned()));
        }
        let names = live_names(live, source)?;
        let listed = output(git(beside).args([
            "for-each-ref",
            "--format=%(objectname) %(objecttype) %(*objectname) %(*objecttype) %(refname)",
        ]))?;
        for line in String::from_utf8_lossy(&listed).lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            let (Some(value), Some(kind), Some(peeled), Some(peeled_kind), Some(name)) = (
                fields.first(),
                fields.get(1),
                fields.get(2),
                fields.get(3),
                fields.get(4),
            ) else {
                return Ok(Some("unreachable".to_owned()));
            };
            if name.starts_with("refs/carry") {
                continue;
            }
            // The stash's entries are decided by commit below.
            if *name != "refs/stash" {
                let spaced = name
                    .strip_prefix("refs/")
                    .map(|rest| format!("refs/{ns}/{rest}"));
                let held = |at: &str| {
                    names
                        .get(at)
                        .is_some_and(|values| values.iter().any(|held| held == value))
                };
                if !held(name) && !spaced.as_deref().is_some_and(held) {
                    return Ok(Some(format!("refs:{name}")));
                }
            }
            match (*kind, *peeled_kind) {
                ("commit", _) => commits.push((*value).to_owned()),
                ("tag", "commit") => commits.push((*peeled).to_owned()),
                _ => {
                    // A ref to a tree or a blob: present in the live
                    // object store, or lost with the checkout.
                    let present = git(live)
                        .args(["cat-file", "-e", value])
                        .output()
                        .refuse_at(SITE)?;
                    if !present.status.success() {
                        return Ok(Some("unreachable".to_owned()));
                    }
                }
            }
        }
        for entry in super::super::native_refs::stash_reflog(beside)? {
            commits.push(entry.oid);
        }
        commits.extend(reflogs(&beside.join(".git/logs"))?);
    }
    commits.sort();
    commits.dedup();
    Ok((!reachable(live, &commits)).then(|| "unreachable".to_owned()))
}

/// Why the live tree at `live` does not equal the beside checkout at
/// `beside` (module docs); `None` when it does.
///
/// `left` is what the advance left to the live tree (its own untracked
/// files), `ns` the beside namespace and `source` the SOURCE label.
/// Read-only.
///
/// # Errors
/// Whatever reading either tree refuses.
pub fn differs(
    beside: &Path,
    live: &Path,
    (left, ns, source): (&Left, &str, &str),
) -> Result<Option<String>> {
    let admin = linked_admin(beside)?;
    if head_of(beside)?.oid != head_of(live)?.oid {
        return Ok(Some("differs:head".to_owned()));
    }
    if index_digest(beside, None)? != index_digest(live, None)? {
        return Ok(Some("differs:index".to_owned()));
    }
    if let Some(why) = git_differs(beside, live, admin.as_deref(), (ns, source))? {
        return Ok(Some(why));
    }
    let entries = walk(beside, false)?;
    for entry in &entries {
        if let Some(why) = entry_differs(beside, live, entry)? {
            return Ok(Some(why));
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
            && lstat(&live.join(OsStr::from_bytes(path)))?.is_some()
        {
            return Ok(Some(format!("extra:{}", path.escape_ascii())));
        }
    }
    Ok(None)
}

// A linked checkout's administration directory (what its `.git` file
// names); `None` for a standalone checkout.
fn linked_admin(beside: &Path) -> Result<Option<PathBuf>> {
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
/// The retirement is recorded at `record` (STATE). The caller has
/// established the checkout is this item's landing of `capture`; `left` is
/// what the advance left to the live tree.
///
/// # Errors
/// Whatever reading either tree, the rename, the removal or a record
/// write refuses.
pub fn retire(
    record: &Path,
    beside: &Path,
    live: &Path,
    (item, capture): (&str, [u8; 32]),
    (left, ns, source): (&Left, &str, &str),
) -> Result<Retired> {
    // The prints before the proof and after it: a write while the proof
    // read the tree (a session working there) keeps it.
    let before = prints(beside)?;
    if let Some(why) = differs(beside, live, (left, ns, source))? {
        return Ok(Retired::Kept(why));
    }
    if prints(beside)? != before {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    let meta = fs::symlink_metadata(beside).refuse_at("git_carry::beside::retire")?;
    let admin = linked_admin(beside)?;
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
    };
    landed::write(record, &intent)?;
    fault_point!(EstateAdvanceRetireAfterIntent);
    rename_exclusive(beside, &temp)?;
    fault_point!(EstateAdvanceRetireAfterRename);
    finish(record, intent)
}

// From the rename on: re-prove the renamed tree by its prints (put back
// when a write since the proof changed it), mark it deleting, remove a
// linked landing's administration, delete the tree, remove the record.
fn finish(record: &Path, mut intent: RetireIntent) -> Result<Retired> {
    let (path, temp) = (path_of(&intent.path), path_of(&intent.temp));
    if !intent.deleting {
        if prints(&temp)? != intent.prints {
            if lstat(&path)?.is_some() {
                // Something new at the beside path since: both kept, the
                // record names the renamed tree.
                return Ok(Retired::Kept(format!(
                    "raced:{}",
                    intent.temp.escape_ascii()
                )));
            }
            rename_exclusive(&temp, &path)?;
            landed::remove(record)?;
            return Ok(Retired::Kept("raced".to_owned()));
        }
        intent.deleting = true;
        landed::write(record, &intent)?;
    }
    if let Some(admin) = intent.admin.as_deref().map(path_of) {
        if !remove_admin(&admin, &intent.admin_gitdir)? {
            return Ok(Retired::Kept(format!(
                "admin-unremoved:{}",
                admin.as_os_str().as_bytes().escape_ascii()
            )));
        }
    }
    fault_point!(EstateAdvanceRetireMid);
    if let Some(meta) = lstat(&temp)? {
        // The renamed tree is the one proved (its directory's identity), or
        // left as it is.
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

// Remove a retired linked landing's administration directory when its
// `gitdir` still holds what it held when proved (`proved`), or is gone (a
// removal a crash cut short: the record names the directory); `false`
// when it names something else now, left as it is.
fn remove_admin(admin: &Path, proved: &[u8]) -> Result<bool> {
    let Some(meta) = lstat(admin)? else {
        return Ok(true);
    };
    if !meta.is_dir() {
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
