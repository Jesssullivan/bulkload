//! `estate-beside-retire` (operator ruling 2026-10-16 21:24Z: "refs in
//! always; a tree beside ONLY where neo has uncommitted work"; lane note
//! `docs/agent-notes/2026-10-19-clean-source-no-beside.md`).
//!
//! The beside trees an earlier pass landed (`<live>SUFFIX`) whatever the
//! source held, before the clean-source rule ([`super::super::converge::clean`]),
//! are retired here when their content is a subset of the live repository
//! by that same rule. [`prove`] reads one, read-only:
//!
//! 1. its shape: an empty directory (a refused carry's leftover) is
//!    retirable as it is; otherwise a checkout, a linked worktree of the
//!    live repository or a standalone repository, whose live path holds a
//!    checkout;
//! 2. **tracked-clean**: no staged or unstaged change, no operation in
//!    progress, no hidden index flag, no index lock;
//! 3. **its HEAD reachable in the live repository** from a ref (provenance
//!    `refs/carry*` aside, and never through its own worktree HEAD);
//! 4. **nothing its Git holds lost with it**: a linked tree has no lock and
//!    no per-worktree ref, and every commit its reflogs name is reachable
//!    in the live repository; a standalone one has no worktree and no hook
//!    of its own, and every commit its refs, stash and reflogs name is in
//!    the live repository and reachable there. Its branches, tags and other
//!    refs (remote-tracking ones aside, which a fetch rebuilds) are held
//!    by the live repository by name and value, natively or under
//!    `refs/<ns>/`, or are created there under `refs/<ns>/` (create-only;
//!    one held there at another value refuses `refs:<name>`), and each
//!    stash commit no `refs/<ns>-stash/` ref holds goes in at a free index.
//!    Its configuration and `info/exclude` bytes are kept in the
//!    retirement's ledger record;
//! 5. **every untracked and ignored entry present and equal in the live
//!    checkout, or landed there as a file-level beside**: the file-level
//!    reconcile of [`super::super::converge::files`] plans each one (an
//!    entry absent at the live checkout is placed, one that differs lands
//!    at `<path>SUFFIX`); an entry whose beside path holds something else
//!    refuses `occupied:<path>`. Entries under a rebuildable root (the
//!    capture's own [`super::super::REBUILDABLE_DIRECTORIES`], a real
//!    directory nothing tracked is under) are counted, as a capture omits
//!    them, never reconciled. A nested repository refuses `nested:<path>`.
//!
//! The tree's prints are taken before the proof and after it (a session
//! writing there refuses `raced`). Anything not provable is a typed
//! refusal naming why, and nothing is written.
//!
//! Report only by default. With `--delete` ([`retire`]), under the live
//! repository's destination lock: the file-level reconcile runs
//! (recorded write-ahead), the refs go in, the tree is proved again (every
//! entry now present: placed or beside), the ledger record is written
//! (`beside-retire-<tag>.retired` in STATE: path, live path, shape, HEAD,
//! counts, refs, bytes, configuration), and the tree is removed by the
//! beside retirement's crash-safe removal ([`super::retire`]: a STATE
//! record, a rename to a private name, a re-proof by prints, a linked
//! tree's administration, the tree). A rerun finishes a removal a crash cut
//! short from its record.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::converge::files::{self, Tally, Want};
use super::super::converge::handoff::TARGETS;
use super::super::converge::{advance, clean, head_of, same_dir};
use super::super::{git, landed, native_refs, output, REBUILDABLE_DIRECTORIES};
use super::retire::{self, Retired};
use crate::{BulkloadRefusal, Result};

const SITE: &str = "git_carry::beside::subset";

/// What a beside tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape {
    /// An empty directory.
    Empty,
    /// A linked worktree of the live repository.
    Linked,
    /// A repository of its own.
    Standalone,
}

impl Shape {
    /// The receipt word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Linked => "linked",
            Self::Standalone => "standalone",
        }
    }
}

/// What [`prove`] found of a retirable beside tree.
#[derive(Debug, Clone)]
pub struct Proof {
    pub shape: Shape,
    /// Its HEAD commit (empty for an empty directory).
    pub head: String,
    /// The files and links its index tracks (the HEAD's: it is clean).
    pub tracked: u64,
    /// What the file-level reconcile of its untracked and ignored entries
    /// does (planned; `placed` and `beside` are still to write).
    pub tally: Tally,
    /// Entries under a rebuildable root, not reconciled.
    pub rebuildable: u64,
    /// Hand-off targets bulkload wrote there (a sealed block above a
    /// tracked file's bytes, or a target it created): bulkload's own.
    pub handoff: u64,
    /// Refs the live repository gets, create-only, as `(name, value)`.
    pub refs: Vec<(String, String)>,
    /// The bytes it holds on disk (allocated), its administration
    /// included.
    pub bytes: u64,
    prints: [u8; 32],
    wants: Vec<Want>,
}

/// [`prove`]'s answer.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Its content is a subset of the live repository's.
    Retirable(Box<Proof>),
    /// Not provable, and why (a typed reason word, then its detail).
    Refused(String),
}

/// `(name, value)` pairs: refs, or commits under the name they are
/// reported by.
type Named = Vec<(String, String)>;

fn refused(why: impl Into<String>) -> Verdict {
    Verdict::Refused(why.into())
}

fn shown(path: &[u8]) -> String {
    path.escape_ascii().to_string()
}

// The bytes allocated under `root` (never following a link).
fn allocated(root: &Path) -> Result<u64> {
    let mut total = fs::symlink_metadata(root).refuse_at(SITE)?.blocks() * 512;
    for entry in retire::walk(root, true)? {
        total += entry.meta.blocks() * 512;
    }
    Ok(total)
}

// The index entries' paths of the checkout at `worktree`.
fn index_paths(worktree: &Path) -> Result<BTreeSet<Vec<u8>>> {
    let listed = output(git(worktree).args(["ls-files", "-z"]))?;
    Ok(listed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

// Whether `path` or a path under it is in `tracked`.
fn tracks(tracked: &BTreeSet<Vec<u8>>, path: &[u8]) -> bool {
    let mut under = path.to_vec();
    under.push(b'/');
    let mut past = path.to_vec();
    past.push(b'/' + 1);
    tracked.contains(path) || tracked.range(under..past).next().is_some()
}

/// The commits a standalone tree's refs and stash name (each with the
/// name it is reported under), and the refs to create in the live
/// repository; `Err` with the refusal word.
fn standalone_refs(
    beside: &Path,
    live: &Path,
    ns: &str,
) -> Result<std::result::Result<(Named, Named), String>> {
    let (held, _) = native_refs::native_refs(live)?;
    let (own, _) = native_refs::native_refs(beside)?;
    let mut commits = Vec::new();
    let mut wanted = Vec::new();
    for (name, value) in &own {
        commits.push((name.clone(), value.clone()));
        if name == "refs/stash" || name.starts_with("refs/remotes/") {
            continue;
        }
        let Some(rest) = name.strip_prefix("refs/") else {
            return Ok(Err(format!("refs:{name}")));
        };
        let spaced = format!("refs/{ns}/{rest}");
        if held.get(name) == Some(value) || held.get(&spaced) == Some(value) {
            continue;
        }
        if held.contains_key(&spaced) {
            return Ok(Err(format!("refs:{name}")));
        }
        wanted.push((spaced, value.clone()));
    }
    let prefix = format!("refs/{ns}-stash/");
    let stashed: BTreeSet<&String> = held
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(_, value)| value)
        .collect();
    let mut free = 0_usize;
    for (n, entry) in native_refs::stash_reflog(beside)?.into_iter().enumerate() {
        commits.push((format!("stash@{{{n}}}"), entry.oid.clone()));
        if stashed.contains(&entry.oid) || wanted.iter().any(|(_, value)| *value == entry.oid) {
            continue;
        }
        while held.contains_key(&format!("{prefix}{free}")) {
            free += 1;
        }
        wanted.push((format!("{prefix}{free}"), entry.oid));
        free += 1;
    }
    Ok(Ok((commits, wanted)))
}

fn labelled(label: &str, commits: Vec<String>) -> impl Iterator<Item = (String, String)> + '_ {
    commits
        .into_iter()
        .map(move |commit| (label.to_owned(), commit))
}

// Why some of `commits` (each with the name it is reported under) is not
// in the live repository at `live` or not reachable there, naming the
// first such (a remote-tracking ref's apart: a fetch may bring it back);
// `None` when every one is.
fn unreachable(live: &Path, mut commits: Vec<(String, String)>) -> Option<String> {
    commits.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    commits.dedup_by(|a, b| a.1 == b.1);
    let all: Vec<String> = commits.iter().map(|(_, commit)| commit.clone()).collect();
    if retire::reachable(live, &all) {
        return None;
    }
    commits.sort_by_key(|(name, _)| (name.starts_with("refs/remotes/"), name.clone()));
    // At most 256 single reads to name it; past them, unnamed.
    for (name, commit) in commits.iter().take(256) {
        if !retire::reachable(live, std::slice::from_ref(commit)) {
            let kind = if name.starts_with("refs/remotes/") {
                "unreachable-remote"
            } else {
                "unreachable"
            };
            return Some(format!("{kind}:{name}={commit}"));
        }
    }
    Some("unreachable".to_owned())
}

// Whether the checkout at `beside` (git directory `git_dir`) is
// tracked-clean, a sealed bulkload hand-off block at byte 0 of a top-level
// `AGENTS.md` or `CLAUDE.md` above its HEAD's bytes the one difference
// allowed (estate-handoff writes them into beside landings): `Some(n)` with
// the targets that carry one, `None` when it is not clean.
fn clean_but_blocks(beside: &Path, git_dir: &Path) -> Result<Option<u64>> {
    if advance::status(beside, git_dir)?.is_some() {
        return Ok(Some(0));
    }
    for name in advance::IN_PROGRESS {
        if retire::lstat(&git_dir.join(name))?.is_some() {
            return Ok(None);
        }
    }
    let tagged = output(git(beside).args(["ls-files", "-v", "-z"]))?;
    if tagged
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry.first() != Some(&b'H'))
    {
        return Ok(None);
    }
    let listed = output(git(beside).args([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=no",
        "--ignore-submodules=none",
        "--no-renames",
    ]))?;
    let mut blocks = 0;
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(path) = entry.strip_prefix(b" M ") else {
            return Ok(None);
        };
        if !TARGETS.iter().any(|target| target.as_bytes() == path) {
            return Ok(None);
        }
        let at = beside.join(OsStr::from_bytes(path));
        if !retire::lstat(&at)?.is_some_and(|meta| meta.is_file()) {
            return Ok(None);
        }
        let bytes = fs::read(&at).refuse_at(SITE)?;
        let unblocked = retire::unblocked(&bytes);
        if unblocked.len() == bytes.len() {
            return Ok(None);
        }
        let mut spec = b"HEAD:".to_vec();
        spec.extend_from_slice(path);
        let committed = output(
            git(beside)
                .args(["cat-file", "blob"])
                .arg(OsStr::from_bytes(&spec)),
        )?;
        if unblocked != committed.as_slice() {
            return Ok(None);
        }
        blocks += 1;
    }
    Ok(Some(blocks))
}

// Whether the untracked top-level `relative` of `beside` is a hand-off
// target bulkload created there: a sealed block and nothing else, or a link
// to the other target.
fn handoff_created(beside: &Path, relative: &[u8]) -> Result<bool> {
    if !TARGETS.iter().any(|target| target.as_bytes() == relative) {
        return Ok(false);
    }
    let at = beside.join(OsStr::from_bytes(relative));
    let Some(meta) = retire::lstat(&at)? else {
        return Ok(false);
    };
    if meta.file_type().is_symlink() {
        let to = fs::read_link(&at).refuse_at(SITE)?;
        return Ok(TARGETS
            .iter()
            .any(|other| Path::new(other) == to && other.as_bytes() != relative));
    }
    Ok(meta.is_file() && retire::unblocked(&fs::read(&at).refuse_at(SITE)?).is_empty())
}

/// Prove the beside tree at `beside` a subset of the live checkout at
/// `live` (module docs), with `suffix` the file-level beside suffix and
/// `ns` the ref namespace. Read-only.
///
/// # Errors
/// Whatever reading either tree refuses (a reason the tree is not provable
/// is a [`Verdict::Refused`], never an error).
#[allow(clippy::too_many_lines)]
pub fn prove(beside: &Path, live: &Path, (suffix, ns): (&str, &str)) -> Result<Verdict> {
    let Some(meta) = retire::lstat(beside)? else {
        return Ok(refused("absent"));
    };
    if !meta.is_dir() {
        return Ok(refused("not-a-directory"));
    }
    let before = retire::prints(beside)?;
    if fs::read_dir(beside).refuse_at(SITE)?.next().is_none() {
        return Ok(Verdict::Retirable(Box::new(Proof {
            shape: Shape::Empty,
            head: String::new(),
            tracked: 0,
            tally: Tally::default(),
            rebuildable: 0,
            handoff: 0,
            refs: Vec::new(),
            bytes: meta.blocks() * 512,
            prints: before,
            wants: Vec::new(),
        })));
    }
    if !retire::lstat(live)?.is_some_and(|meta| meta.is_dir()) {
        return Ok(refused("live-absent"));
    }
    let Ok(Some(live_git)) = landed::worktree_git_dir(live) else {
        return Ok(refused("live-not-a-checkout"));
    };
    let Ok(Some(git_dir)) = landed::worktree_git_dir(beside) else {
        return Ok(refused("not-a-checkout"));
    };
    if !git_dir.is_dir() {
        return Ok(refused("not-a-checkout"));
    }
    let (Ok(live_common), Ok(common)) =
        (landed::common_dir(&live_git), landed::common_dir(&git_dir))
    else {
        return Ok(refused("not-a-checkout"));
    };
    let shape = if same_dir(&git_dir, &common) {
        Shape::Standalone
    } else if same_dir(&common, &live_common) {
        Shape::Linked
    } else {
        return Ok(refused("other-repository"));
    };
    if retire::lstat(&git_dir.join("index.lock"))?.is_some() {
        return Ok(refused("index-locked"));
    }
    // 2. Tracked-clean (and every in-progress operation and hidden flag),
    // a sealed hand-off block the one difference allowed.
    let Some(blocks) = clean_but_blocks(beside, &git_dir)? else {
        return Ok(refused("tracked-changes"));
    };
    // 3. Its HEAD, reachable from a ref of the live repository.
    let head = head_of(beside)?.oid;
    if head.is_empty() {
        return Ok(refused("unborn"));
    }
    if !clean::reachable_in(live, &head)? {
        return Ok(refused(format!("unreachable:head={head}")));
    }
    // 4. Its Git: every commit it names, with the name it is reported
    // under.
    let mut commits: Vec<(String, String)> = Vec::new();
    let mut refs = Vec::new();
    let mut bytes = allocated(beside)?;
    match shape {
        Shape::Linked => {
            if retire::lstat(&git_dir.join("locked"))?.is_some() {
                return Ok(refused("locked"));
            }
            if retire::worktree_refs(&git_dir)? {
                return Ok(refused("worktree-refs"));
            }
            commits.extend(labelled("logged", retire::reflogs(&git_dir.join("logs"))?));
            bytes += allocated(&git_dir)?;
        }
        Shape::Standalone => {
            if super::super::converge::worktree_census(beside)?.len() > 1 {
                return Ok(refused("worktrees"));
            }
            let hooks = git_dir.join("hooks");
            if retire::lstat(&hooks)?.is_some_and(|meta| meta.is_dir())
                && retire::walk(&hooks, true)?
                    .iter()
                    .any(|entry| !entry.meta.is_dir() && !entry.relative.ends_with(b".sample"))
            {
                return Ok(refused("hooks"));
            }
            match standalone_refs(beside, live, ns)? {
                Ok((named, wanted)) => {
                    commits.extend(named);
                    refs = wanted;
                }
                Err(why) => return Ok(refused(why)),
            }
            commits.extend(labelled("logged", retire::reflogs(&git_dir.join("logs"))?));
        }
        Shape::Empty => {}
    }
    if let Some(why) = unreachable(live, commits) {
        return Ok(refused(why));
    }
    // 5. Its untracked and ignored entries.
    let tracked = index_paths(beside)?;
    let mut dirs = BTreeSet::new();
    for path in &tracked {
        for (at, byte) in path.iter().enumerate() {
            if *byte == b'/' {
                dirs.insert(path.get(..at).unwrap_or_default().to_vec());
            }
        }
    }
    let mut rebuildable = 0_u64;
    let mut handoff = blocks;
    let mut roots: Vec<Vec<u8>> = Vec::new();
    let mut wants = Vec::new();
    for entry in retire::walk(beside, false)? {
        let path = entry.relative;
        let name = path.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
        if name.eq_ignore_ascii_case(b".git") {
            return Ok(refused(format!("nested:{}", shown(&path))));
        }
        if tracked.contains(&path) || dirs.contains(&path) {
            continue;
        }
        // A hand-off target estate-handoff created in the beside tree is
        // bulkload's own, never the source's.
        if !path.contains(&b'/') && handoff_created(beside, &path)? {
            handoff += 1;
            continue;
        }
        if roots.iter().any(|root| {
            path.strip_prefix(root.as_slice())
                .is_some_and(|rest| rest.first() == Some(&b'/'))
        }) {
            rebuildable += 1;
            continue;
        }
        if entry.meta.is_dir()
            && REBUILDABLE_DIRECTORIES
                .iter()
                .any(|candidate| candidate.as_bytes() == name)
            && !tracks(&tracked, &path)
        {
            rebuildable += 1;
            roots.push(path);
            continue;
        }
        match Want::on_disk(beside, path.clone())? {
            Some(want) => wants.push(want),
            None => return Ok(refused(format!("special:{}", shown(&path)))),
        }
    }
    wants.sort_by(|a, b| a.path.cmp(&b.path));
    let planned = files::plan(live, suffix, &wants, None)?;
    if let Some(first) = planned.occupied().first() {
        return Ok(refused(format!("occupied:{}", shown(first))));
    }
    if retire::prints(beside)? != before {
        return Ok(refused("raced"));
    }
    let files = u64::try_from(tracked.iter().filter(|path| !dirs.contains(*path)).count())
        .unwrap_or(u64::MAX);
    Ok(Verdict::Retirable(Box::new(Proof {
        shape,
        head,
        tracked: files,
        tally: planned.tally(),
        rebuildable,
        handoff,
        refs,
        bytes,
        prints: before,
        wants,
    })))
}

/// What the ledger keeps of one retired beside tree
/// (`beside-retire-<tag>.retired` in STATE).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredRecord {
    pub path: Vec<u8>,
    pub live: Vec<u8>,
    pub shape: String,
    pub head: String,
    pub tracked: u64,
    pub tally: Tally,
    pub rebuildable: u64,
    pub refs: Vec<(String, String)>,
    pub bytes: u64,
    /// A standalone tree's own configuration (`config --local --list -z`)
    /// and its `info/exclude` bytes.
    pub config: Vec<u8>,
    pub exclude: Vec<u8>,
    /// Unix nanoseconds at the proof.
    pub at_ns: i128,
    /// The tree is gone.
    pub retired: bool,
}

impl landed::Record for RetiredRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-subset1";
}

/// The tag a beside tree's records are named by in STATE.
#[must_use]
pub fn tag(beside: &Path) -> String {
    blake3::hash(beside.as_os_str().as_bytes())
        .to_hex()
        .to_string()
}

/// Where a beside tree's retirement records are in `state`: the ledger
/// record, the file reconcile and the removal intent.
#[must_use]
pub fn records(state: &Path, beside: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let tag = tag(beside);
    (
        state.join(format!("beside-retire-{tag}.retired")),
        state.join(format!("beside-retire-{tag}.files")),
        state.join(format!("beside-retire-{tag}.retire")),
    )
}

/// Finish a removal a crash cut short for the beside tree at `beside`
/// (its intent in `state`): `None` when there was none.
///
/// # Errors
/// Whatever finishing the removal refuses.
pub fn settle(state: &Path, beside: &Path) -> Result<Option<Retired>> {
    let (ledger, _, intent) = records(state, beside);
    let done = retire::settle(&intent)?;
    if matches!(done, Some(Retired::Retired)) {
        if let Some(mut record) = landed::read::<RetiredRecord>(&ledger)? {
            record.retired = true;
            landed::write(&ledger, &record)?;
        }
    }
    Ok(done)
}

/// Finish every removal a crash cut short whose intent is in `state`,
/// recording each finished one in its ledger record: the beside paths and
/// what became of them.
///
/// # Errors
/// Whatever reading `state` or finishing a removal refuses.
pub fn settle_all(state: &Path) -> Result<Vec<(PathBuf, Retired)>> {
    let mut settled = Vec::new();
    let Ok(entries) = fs::read_dir(state) else {
        return Ok(settled);
    };
    let mut intents = Vec::new();
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        let name = entry.file_name();
        if name.as_bytes().starts_with(b"beside-retire-") && name.as_bytes().ends_with(b".retire") {
            intents.push(entry.path());
        }
    }
    intents.sort();
    for intent in intents {
        let Some(read) = landed::read::<retire::RetireIntent>(&intent)? else {
            continue;
        };
        let beside = PathBuf::from(OsStr::from_bytes(&read.path));
        if let Some(done) = settle(state, &beside)? {
            settled.push((beside, done));
        }
    }
    Ok(settled)
}

/// Whether `state` holds a removal a crash cut short (read-only).
///
/// # Errors
/// Whatever reading `state` refuses.
pub fn pending(state: &Path) -> Result<u64> {
    let Ok(entries) = fs::read_dir(state) else {
        return Ok(0);
    };
    let mut count = 0;
    for entry in entries {
        let name = entry.refuse_at(SITE)?.file_name();
        if name.as_bytes().starts_with(b"beside-retire-") && name.as_bytes().ends_with(b".retire") {
            count += 1;
        }
    }
    Ok(count)
}

fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i128::try_from(since.as_nanos()).unwrap_or(i128::MAX)
        })
}

/// Retire the beside tree `beside` [`prove`] found retirable (`proof`)
/// into the live checkout `live` (module docs), recorded in `state`,
/// under the live repository's destination lock for `source`.
///
/// # Errors
/// `GIT_DESTINATION_MODIFIED` when the live tree moved under the
/// reconcile; whatever a write, the ref creation or the removal refuses.
pub fn retire(
    state: &Path,
    source: &str,
    (beside, live): (&Path, &Path),
    (suffix, ns): (&str, &str),
    proof: &Proof,
) -> Result<Retired> {
    let (ledger, files_record, intent) = records(state, beside);
    let mut record = RetiredRecord {
        path: beside.as_os_str().as_bytes().to_vec(),
        live: live.as_os_str().as_bytes().to_vec(),
        shape: proof.shape.word().to_owned(),
        head: proof.head.clone(),
        tracked: proof.tracked,
        tally: proof.tally,
        rebuildable: proof.rebuildable,
        refs: proof.refs.clone(),
        bytes: proof.bytes,
        at_ns: now_ns(),
        ..RetiredRecord::default()
    };
    if proof.shape == Shape::Empty {
        landed::write(&ledger, &record)?;
        // `rmdir` removes an empty directory only: one written into since
        // the proof stays.
        if fs::remove_dir(beside).is_err() {
            return Ok(Retired::Kept("raced".to_owned()));
        }
        if let Some(parent) = beside.parent() {
            landed::sync_dir(parent)?;
        }
        record.retired = true;
        landed::write(&ledger, &record)?;
        return Ok(Retired::Retired);
    }
    let git_dir = landed::worktree_git_dir(live)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    let common = landed::common_dir(&git_dir)?;
    let _lock = landed::lock(&common, source)?;
    if proof.shape == Shape::Standalone {
        // Its own configuration as Git lists it, and its exclude bytes.
        record.config =
            output(git(beside).args(["config", "--local", "--list", "-z"])).unwrap_or_default();
        record.exclude = match fs::read(beside.join(".git/info/exclude")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        };
    }
    landed::write(&ledger, &record)?;
    // The file-level reconcile, recorded write-ahead.
    files::settle(&files_record)?;
    let salt = files_record.as_os_str().as_bytes();
    let planned = files::plan(live, suffix, &proof.wants, None)?;
    if !planned.occupied().is_empty() {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    let mut reconciled = files::FilesRecord {
        item: format!("beside-retire-{}", tag(beside)),
        capture: *blake3::hash(proof.head.as_bytes()).as_bytes(),
        live: record.live.clone(),
        suffix: suffix.to_owned(),
        tracked: proof.tracked,
        temps: files::temps(live, &planned, salt),
        besides: planned.besides(&proof.wants),
        ..files::FilesRecord::default()
    };
    landed::write(&files_record, &reconciled)?;
    reconciled.tally = files::execute(live, &planned, &proof.wants, salt, None)?;
    reconciled.done = true;
    landed::write(&files_record, &reconciled)?;
    // The refs, create-only: a name taken since keeps the tree.
    if !proof.refs.is_empty() {
        let wanted: BTreeMap<String, String> = proof.refs.iter().cloned().collect();
        let planned = super::plan(live, &wanted)?;
        if let Some((name, _, _)) = planned.kept.first() {
            return Ok(Retired::Kept(format!("refs:{name}")));
        }
        super::create(live, &planned.create)?;
    }
    // Proved again: every entry is now in the live checkout.
    let Verdict::Retirable(again) = prove(beside, live, (suffix, ns))? else {
        return Ok(Retired::Kept("raced".to_owned()));
    };
    if again.tally.placed != 0 || again.tally.beside != 0 || again.tally.dirs_placed != 0 {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    let capture = *blake3::hash(format!("{}\0{}", tag(beside), proof.head).as_bytes()).as_bytes();
    let done = retire::remove_proved(&intent, beside, (&reconciled.item, capture), again.prints)?;
    if done == Retired::Retired {
        record.retired = true;
        landed::write(&ledger, &record)?;
    }
    Ok(done)
}

/// The beside trees under `root`, sorted.
///
/// Every directory to `depth` levels below it (never following a link,
/// never into a checkout or a `.git`) whose name ends with `suffix` and
/// whose live path (the name without it) exists.
///
/// # Errors
/// Whatever reading a directory refuses.
pub fn scan(root: &Path, suffix: &str, depth: usize) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((dir, level)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.refuse_at(SITE)?;
            let name = entry.file_name();
            if name.as_bytes() == b".git" {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            if let Some(stem) = name.as_bytes().strip_suffix(suffix.as_bytes()) {
                if !stem.is_empty() && retire::lstat(&dir.join(OsStr::from_bytes(stem)))?.is_some()
                {
                    found.push(path);
                    continue;
                }
            }
            if level + 1 < depth && retire::lstat(&path.join(".git"))?.is_none() {
                pending.push((path, level + 1));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The live path of the beside tree `beside`: its name without `suffix`.
#[must_use]
pub fn live_of(beside: &Path, suffix: &str) -> Option<PathBuf> {
    let name = beside.file_name()?.as_bytes();
    let stem = name.strip_suffix(suffix.as_bytes())?;
    (!stem.is_empty()).then(|| beside.with_file_name(OsStr::from_bytes(stem)))
}
