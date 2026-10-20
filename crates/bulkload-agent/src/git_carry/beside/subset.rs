//! `estate-beside-retire` (operator ruling 2026-10-16 21:24Z: "refs in
//! always; a tree beside ONLY where neo has uncommitted work"; lane note
//! `docs/agent-notes/2026-10-19-clean-source-no-beside.md`).
//!
//! The beside trees an earlier pass landed (`<live>SUFFIX`) whatever the
//! source held, before the clean-source rule ([`super::super::converge::clean`]),
//! are retired here when their content is a subset of the live repository
//! by that same rule. [`prove`] reads one, read-only:
//!
//! 0. no process with its working directory or an open file inside it
//!    (`in-use`; again before the rename and in the removal);
//! 1. its shape: an empty directory (a refused carry's leftover) is
//!    retirable as it is; otherwise a checkout bulkload landed (its
//!    workspace ledger for SOURCE in its git directory, else
//!    `not-bulkload-landed`), whose live path holds a
//!    checkout: a linked worktree of the live repository whose
//!    administration directory names it back (a copy of another worktree,
//!    its gitfile naming that one's, refuses `worktree-link`), or a
//!    standalone repository whose `.git` is a directory in the tree;
//! 2. **tracked-clean**: no staged or unstaged change, no operation in
//!    progress, no hidden index flag, no index lock;
//! 3. **its HEAD reachable in the live repository** from its local
//!    branches, its tags or this item's namespace (`refs/<ns>/`,
//!    `refs/<ns>-stash/`; never provenance, a remote-tracking ref, the
//!    stash or its own worktree HEAD);
//! 4. **nothing its Git holds lost with it**: a linked tree has no lock and
//!    no per-worktree ref; a standalone one has no live worktree (a gone,
//!    prunable one's HEAD and reflogs are proved as its own) and no hook of
//!    its own, and every commit its refs and stash name is in the live
//!    repository and reachable there from those same refs
//!    (`unreachable:<name>`; a
//!    remote-tracking ref's own word, `unreachable-remote:`). Its branches,
//!    tags and other refs (remote-tracking ones aside, which a fetch
//!    rebuilds) are held by the live repository by name and value,
//!    natively or under `refs/<ns>/`, or are created there under
//!    `refs/<ns>/` (create-only; one held there at another value refuses
//!    `refs:<name>`), and each stash commit no `refs/<ns>-stash/` ref
//!    holds goes in at a free index. Its `.git` holds nothing else unproved
//!    (an allow-list: a submodule's `modules/`, `lfs/`, `git-crypt/`, other
//!    `info/` files refuse `unknown-git:<name>`), the live repository does
//!    not borrow its objects (`borrowed-by-live`), and its configuration
//!    (without an include, `config-include`) and `info/exclude` bytes are
//!    kept in the retirement's ledger record (and a bulkload landing's
//!    `.git/carry-config` receipts). A commit only its reflogs name is
//!    never dropped (operator ruling 2026-10-16, "full history. drop
//!    nothing. ever."): it is counted, and the retirement carries it in
//!    under `refs/<ns>/reflog/<tree>-<tag>/<oid>` (its objects too, packed
//!    from the tree, when the live store lacks them);
//! 5. **every untracked and ignored entry present and equal in the live
//!    checkout, or landed there as a file-level beside**: the file-level
//!    reconcile of [`super::super::converge::files`] plans each one (an
//!    entry absent at the live checkout is placed, one that differs lands
//!    at `<path>SUFFIX`); an entry whose beside path holds something else
//!    refuses `occupied:<path>`; a path the live index tracks and its
//!    worktree deleted is never undone (the entry lands beside). A
//!    rebuildable root (the capture's own
//!    [`super::super::REBUILDABLE_DIRECTORIES`], a real directory nothing
//!    tracked is under) refuses `rebuildable:<path>`, unless the run drops
//!    them as a capture omits them (`--drop-rebuildable`: each ledgered,
//!    with what the live checkout holds at its path). Derived output
//!    ([`super::super::converge::files::class_of`]) the reconcile would
//!    have to write refuses `derived-held:<path>` until the operator rules
//!    on build output (`--place-derived` writes it). A nested repository
//!    refuses `nested:<path>`.
//!
//! The tree's prints (and a linked tree's administration directory's) are
//! taken before the proof and after it (a session writing there refuses
//! `raced`). Anything not provable is a typed refusal naming why, and
//! nothing is written.
//!
//! Report only by default. With `--delete` ([`retire`]), under the live
//! repository's destination lock: the file-level reconcile runs
//! (recorded write-ahead), the refs go in, the tree is proved again (every
//! entry now present: placed or beside) and its HEAD and administration
//! looked at once more, the ledger record is written
//! (`beside-retire-<tag>-<ns>.retired` in STATE, one per retirement: path,
//! live path, shape, HEAD, counts, refs, bytes, configuration), and the
//! tree is removed by the
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
use super::super::{
    durable, git, input, landed, native_refs, oid, output, REBUILDABLE_DIRECTORIES,
};
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
    /// Entries under a rebuildable root (`--drop-rebuildable`), not
    /// reconciled.
    pub rebuildable: u64,
    /// Each of them, with what the live checkout holds at its path
    /// (`equal`, `differs`, `absent`), for the ledger.
    pub dropped: Vec<(Vec<u8>, String)>,
    /// Hand-off targets bulkload wrote there (a sealed block above a
    /// tracked file's bytes, or a target it created): bulkload's own.
    pub handoff: u64,
    /// Refs the live repository gets, create-only, as `(name, value)`.
    pub refs: Vec<(String, String)>,
    /// The bytes it holds on disk (allocated), its administration
    /// included.
    pub bytes: u64,
    /// A standalone tree's configuration and `info/exclude` bytes, for the
    /// ledger.
    pub config: Vec<u8>,
    pub exclude: Vec<u8>,
    /// A standalone bulkload landing's `.git/carry-config` receipts
    /// (relative name, bytes), for the ledger.
    pub carried: Receipts,
    /// Worktrees a standalone tree's `.git` registers that are gone on
    /// this host (prunable: a copy's, naming another host's paths).
    pub prunable: u64,
    /// The commits its reflogs (and a gone worktree's) name that no live
    /// ref reaches: carried into the live repository under
    /// `refs/<ns>/reflog/<tree>/<oid>` before it goes (operator ruling
    /// 2026-10-16, "full history. drop nothing. ever.").
    pub reflogs: Vec<String>,
    /// The commits (its reflog commits, and the values of the refs to
    /// create) the live repository's object store lacks: their objects are
    /// packed from the tree and carried too.
    pub objects_absent: u64,
    /// Every commit its refs and stash name that the live store already
    /// holds: what the pack leaves out.
    tips: Vec<String>,
    /// The prints of the tree and a linked tree's administration directory
    /// (its HEAD, index and reflogs live there), taken before the proof.
    prints: [u8; 32],
    wants: Vec<Want>,
}

/// What [`prove`] may take as given.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rules<'a> {
    /// The file-level beside suffix (`.neo-20261016`).
    pub suffix: &'a str,
    /// The ref namespace (the suffix without its leading `.`).
    pub ns: &'a str,
    /// The SOURCE label (an advance's own marker on a lock is its).
    pub source: &'a str,
    /// `--drop-rebuildable`: entries under a rebuildable root are counted
    /// and go with the tree, as a capture omits them; without it one
    /// refuses `rebuildable:<path>`.
    pub drop_rebuildable: bool,
    /// `--place-derived`: derived output is placed as any entry; without
    /// it a tree that would need one placed refuses `derived-held:<path>`.
    pub place_derived: bool,
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

/// Files by relative name, with their bytes.
type Receipts = Vec<(Vec<u8>, Vec<u8>)>;

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
/// repository: every ref (a remote-tracking one too: full history) the
/// live repository does not hold by name and value, natively or under the
/// namespace, goes in under `refs/<ns>/`, each stash commit under
/// `refs/<ns>-stash/`; `Err` with the refusal word.
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
        if name == "refs/stash" {
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

// Whether the checkout at `beside` (git directory `git_dir`) is
// tracked-clean, a sealed bulkload hand-off block at byte 0 of a top-level
// `AGENTS.md` or `CLAUDE.md` above its HEAD's bytes the one difference
// allowed (estate-handoff writes them into beside landings): `Some(n)` with
// the targets that carry one, `None` when it is not clean.
fn clean_but_blocks(beside: &Path, git_dir: &Path, source: &str) -> Result<Option<u64>> {
    if advance::status(beside, git_dir, source)?.is_some() {
        return Ok(Some(0));
    }
    if advance::in_progress(git_dir, source)? {
        return Ok(None);
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

/// The `.git` entries a standalone tree may hold, every one of which is
/// proved (refs, objects, reflogs, hooks), ledgered (its configuration and
/// `info/exclude`) or rebuilt by Git (the rest). Anything else (a
/// submodule's `modules/`, `lfs/`, `git-crypt/`, `rr-cache/`, ...) refuses
/// `unknown-git:<name>`.
const GIT_ENTRIES: &[&str] = &[
    "HEAD",
    "ORIG_HEAD",
    "FETCH_HEAD",
    "COMMIT_EDITMSG",
    CONFIG,
    "description",
    "index",
    "packed-refs",
    REFS,
    "objects",
    "logs",
    "hooks",
    "info",
    "branches",
    "bulkload",
    "shallow",
    "gitk.cache",
    "worktrees",
    "carry-config",
];

/// The `.git/info` entries a standalone tree may hold: its exclude
/// (ledgered) and what `update-server-info` rebuilds.
const INFO_ENTRIES: &[&str] = &["info/exclude", "info/refs", "info/packs"];

/// A repository's configuration file name.
const CONFIG: &str = "config";
/// A repository's ref directory name.
const REFS: &str = "refs";

// Why a standalone tree's `.git` at `git_dir` holds something its removal
// would lose unproved; `None` when it does not.
fn unknown_git(git_dir: &Path) -> Result<Option<String>> {
    for entry in fs::read_dir(git_dir).refuse_at(SITE)? {
        let name = entry.refuse_at(SITE)?.file_name();
        if !GIT_ENTRIES
            .iter()
            .any(|known| known.as_bytes() == name.as_bytes())
        {
            return Ok(Some(format!("unknown-git:{}", shown(name.as_bytes()))));
        }
    }
    let info = git_dir.join("info");
    if retire::lstat(&info)?.is_some_and(|meta| meta.is_dir()) {
        for entry in fs::read_dir(&info).refuse_at(SITE)? {
            let mut name = b"info/".to_vec();
            name.extend_from_slice(entry.refuse_at(SITE)?.file_name().as_bytes());
            if !INFO_ENTRIES.iter().any(|known| known.as_bytes() == name) {
                return Ok(Some(format!("unknown-git:{}", shown(&name))));
            }
        }
    }
    Ok(None)
}

// Whether the live repository's object store (common directory
// `live_common`) borrows from the standalone tree's (`git_dir`): removing
// it would take objects the live repository reads.
fn borrowed_by(live_common: &Path, git_dir: &Path) -> bool {
    let objects = live_common.join("objects");
    let Ok(listed) = fs::read(objects.join("info/alternates")) else {
        return false;
    };
    let Ok(own) = fs::canonicalize(git_dir.join("objects")) else {
        return false;
    };
    for line in listed.split(|byte| *byte == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let named = Path::new(OsStr::from_bytes(line));
        let named = if named.is_absolute() {
            named.to_path_buf()
        } else {
            objects.join(named)
        };
        if fs::canonicalize(&named).is_ok_and(|real| real.starts_with(&own)) {
            return true;
        }
    }
    false
}

// Whether the linked tree at `beside` owns its administration directory
// `admin`: the directory's `gitdir` names `beside/.git` back (a copy of
// another worktree, its gitfile naming that one's, does not).
fn owns_admin(beside: &Path, admin: &Path) -> bool {
    let Ok(text) = fs::read(admin.join("gitdir")) else {
        return false;
    };
    let named = Path::new(OsStr::from_bytes(text.trim_ascii_end()));
    let named = if named.is_absolute() {
        named.to_path_buf()
    } else {
        admin.join(named)
    };
    match (
        fs::canonicalize(named),
        fs::canonicalize(beside.join(".git")),
    ) {
        (Ok(named), Ok(own)) => named == own,
        _ => false,
    }
}

// The worktrees a standalone tree's `.git` registers besides its own: `Ok`
// with how many (each gone on this host, Git's `prunable`: a copy's
// administration naming another host's paths) and every commit their
// administration names (its HEAD, its reflogs), to be proved reachable;
// `Err` with the refusal word for one that is here, or holds a
// per-worktree ref of its own.
#[allow(clippy::type_complexity)]
fn registered(
    beside: &Path,
    git_dir: &Path,
) -> Result<std::result::Result<(u64, Named, Vec<String>), String>> {
    let census = super::super::converge::worktree_census(beside)?;
    if census.iter().skip(1).any(|listed| !listed.prunable) {
        return Ok(Err("worktrees".to_owned()));
    }
    let mut named = Vec::new();
    let mut reflogged = Vec::new();
    let admins = git_dir.join("worktrees");
    if retire::lstat(&admins)?.is_some_and(|meta| meta.is_dir()) {
        for entry in fs::read_dir(&admins).refuse_at(SITE)? {
            let admin = entry.refuse_at(SITE)?.path();
            if !retire::lstat(&admin)?.is_some_and(|meta| meta.is_dir()) {
                continue;
            }
            if retire::worktree_refs(&admin)? {
                return Ok(Err("worktree-refs".to_owned()));
            }
            let head = fs::read(admin.join("HEAD")).unwrap_or_default();
            let head = String::from_utf8_lossy(head.trim_ascii()).into_owned();
            if super::super::oid(&head) {
                named.push(("worktree-head".to_owned(), head));
            }
            reflogged.extend(retire::reflogs(&admin.join("logs"))?);
        }
    }
    let gone = u64::try_from(census.len().saturating_sub(1)).unwrap_or(u64::MAX);
    Ok(Ok((gone, named, reflogged)))
}

// A standalone bulkload landing's `.git/carry-config` receipts (the
// captured configuration and its activation), every file's relative name
// and bytes, for the ledger; `None` when they are not plain files or past
// 4 MiB together.
fn receipts(git_dir: &Path) -> Result<Option<Receipts>> {
    let root = git_dir.join("carry-config");
    if retire::lstat(&root)?.is_none() {
        return Ok(Some(Vec::new()));
    }
    let mut read = Vec::new();
    let mut total = 0_usize;
    for entry in retire::walk(&root, true)? {
        if entry.meta.is_dir() {
            continue;
        }
        if !entry.meta.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(root.join(OsStr::from_bytes(&entry.relative))).refuse_at(SITE)?;
        total += bytes.len();
        if total > 4 << 20 {
            return Ok(None);
        }
        read.push((entry.relative, bytes));
    }
    Ok(Some(read))
}

// Of `commits`, those the object store of the repository at `repo` holds.
fn present(repo: &Path, commits: &[String]) -> Result<BTreeSet<String>> {
    if commits.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut request = String::new();
    for commit in commits {
        request.push_str(commit);
        request.push('\n');
    }
    let listed = input(
        git(repo).args(["cat-file", "--batch-check"]),
        request.as_bytes(),
    )?;
    // `<oid> <type> <size>`, or `<oid> missing`.
    Ok(String::from_utf8_lossy(&listed)
        .lines()
        .filter(|line| !line.ends_with(" missing"))
        .filter_map(|line| line.split(' ').next())
        .map(str::to_owned)
        .collect())
}

// Of the commits `logged` (a beside tree's reflogs name them), those no
// ref of the live repository at `live` reaches, sorted, and how many of
// them its object store lacks; `Err` with the refusal word for one the
// beside tree's own store lacks too (nothing left to carry).
#[allow(clippy::type_complexity)]
fn unreached(
    beside: &Path,
    (live, tips): (&Path, &BTreeSet<String>),
    mut logged: Vec<String>,
) -> Result<std::result::Result<(Vec<String>, u64), String>> {
    logged.retain(|commit| oid(commit));
    logged.sort();
    logged.dedup();
    if logged.is_empty() {
        return Ok(Ok((Vec::new(), 0)));
    }
    let in_live = present(live, &logged)?;
    let mut absent = 0;
    let mut unreached = Vec::new();
    let missing: Vec<String> = logged
        .iter()
        .filter(|commit| !in_live.contains(*commit))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let in_beside = present(beside, &missing)?;
        if let Some(lost) = missing.iter().find(|commit| !in_beside.contains(*commit)) {
            return Ok(Err(format!("logged-missing:{lost}")));
        }
        absent = u64::try_from(missing.len()).unwrap_or(u64::MAX);
        unreached.extend(missing);
    }
    let held: Vec<String> = logged
        .iter()
        .filter(|commit| in_live.contains(*commit))
        .cloned()
        .collect();
    unreached.extend(clean::unreached_from(live, &held, tips));
    unreached.sort();
    Ok(Ok((unreached, absent)))
}

/// The namespace directory a beside tree's reflog commits go under:
/// `refs/<ns>/reflog/<name>-<tag>/`, `<name>` its live checkout's name made
/// one plain ref component.
fn reflog_dir(beside: &Path, live: &Path, ns: &str) -> String {
    let name: String = live
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let tag = tag(beside);
    format!(
        "refs/{ns}/reflog/{name}-{}/",
        tag.get(..8).unwrap_or_default()
    )
}

// Carry into the live repository at `live` what of the beside tree's Git
// it lacks (full history: nothing dropped): the objects its store lacks
// first (a pack of what the refs to create and the reflog commits reach,
// and none of `tips` does, read from the beside tree and indexed into the
// live store), then the refs, create-only, under the namespace, then one
// ref per reflog commit under [`reflog_dir`]. The reflog refs it wrote (or
// found at their value), or why it carried none.
fn carry_history(
    (beside, live): (&Path, &Path),
    ns: &str,
    proof: &Proof,
) -> Result<std::result::Result<Vec<(String, String)>, String>> {
    if proof.objects_absent > 0 {
        let mut request = String::new();
        for commit in proof
            .reflogs
            .iter()
            .chain(proof.refs.iter().map(|(_, commit)| commit))
        {
            request.push_str(commit);
            request.push('\n');
        }
        for commit in &proof.tips {
            request.push('^');
            request.push_str(commit);
            request.push('\n');
        }
        let pack = input(
            git(beside).args(["pack-objects", "--revs", "--stdout", "--quiet"]),
            request.as_bytes(),
        )?;
        if pack.len() > 1 << 30 {
            return Ok(Err("history-pack-large".to_owned()));
        }
        let mut command = git(live);
        durable(&mut command);
        input(command.args(["index-pack", "--stdin"]), &pack)?;
    }
    if !proof.refs.is_empty() {
        let wanted: BTreeMap<String, String> = proof.refs.iter().cloned().collect();
        let planned = super::plan(live, &wanted)?;
        if let Some((name, _, _)) = planned.kept.first() {
            return Ok(Err(format!("refs:{name}")));
        }
        super::create(live, &planned.create)?;
    }
    if proof.reflogs.is_empty() {
        return Ok(Ok(Vec::new()));
    }
    let dir = reflog_dir(beside, live, ns);
    let wanted: BTreeMap<String, String> = proof
        .reflogs
        .iter()
        .map(|commit| (format!("{dir}{commit}"), commit.clone()))
        .collect();
    let planned = super::plan(live, &wanted)?;
    if let Some((name, _, _)) = planned.kept.first() {
        return Ok(Err(format!("refs:{name}")));
    }
    super::create(live, &planned.create)?;
    Ok(Ok(wanted.into_iter().collect()))
}

// What the live checkout at `live` holds at `path` against the beside
// tree's entry there: `equal`, `differs` or `absent`.
fn compared(beside: &Path, live: &Path, path: &[u8]) -> Result<String> {
    let at = live.join(OsStr::from_bytes(path));
    if fs::symlink_metadata(&at).is_err() {
        return Ok("absent".to_owned());
    }
    let Some(want) = Want::on_disk(beside, path.to_vec())? else {
        return Ok("differs".to_owned());
    };
    Ok(if files::holds_want(None, &at, &want)? {
        "equal"
    } else {
        "differs"
    }
    .to_owned())
}

/// Prove the beside tree at `beside` a subset of the live checkout at
/// `live` (module docs), by `rules`. Read-only.
///
/// # Errors
/// Whatever reading either tree refuses (a reason the tree is not provable
/// is a [`Verdict::Refused`], never an error).
#[allow(clippy::too_many_lines)]
pub fn prove(beside: &Path, live: &Path, rules: Rules<'_>) -> Result<Verdict> {
    let Some(meta) = retire::lstat(beside)? else {
        return Ok(refused("absent"));
    };
    if !meta.is_dir() {
        return Ok(refused("not-a-directory"));
    }
    // No process has its working directory or an open file inside it (the
    // advance retirement's check; again before the rename, and in the
    // removal).
    if retire::in_use(beside)? {
        return Ok(refused("in-use"));
    }
    let empty_prints = retire::prints(beside, None)?;
    if fs::read_dir(beside).refuse_at(SITE)?.next().is_none() {
        return Ok(Verdict::Retirable(Box::new(Proof {
            shape: Shape::Empty,
            head: String::new(),
            tracked: 0,
            tally: Tally::default(),
            rebuildable: 0,
            dropped: Vec::new(),
            handoff: 0,
            refs: Vec::new(),
            bytes: meta.blocks() * 512,
            config: Vec::new(),
            exclude: Vec::new(),
            carried: Vec::new(),
            prunable: 0,
            reflogs: Vec::new(),
            objects_absent: 0,
            tips: Vec::new(),
            prints: empty_prints,
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
    let dotgit_dir = retire::lstat(&beside.join(".git"))?.is_some_and(|meta| meta.is_dir());
    let shape = if same_dir(&git_dir, &common) {
        // A repository of its own, its `.git` a directory in the tree (a
        // gitfile naming a git directory elsewhere is not one to remove).
        if !dotgit_dir {
            return Ok(refused("gitfile-standalone"));
        }
        Shape::Standalone
    } else if same_dir(&common, &live_common) {
        if same_dir(&git_dir, &live_git) {
            return Ok(refused("shared-admin"));
        }
        if !owns_admin(beside, &git_dir) {
            return Ok(refused("worktree-link"));
        }
        Shape::Linked
    } else {
        return Ok(refused("other-repository"));
    };
    // Only a tree bulkload landed (its workspace ledger for this SOURCE in
    // its git directory) is one this proof may retire: a copy no ledger
    // names is the operator's.
    if !landed::exists(&landed::ledger_dir(&git_dir, rules.source).join(landed::WORKSPACE_LANDED))?
    {
        return Ok(refused("not-bulkload-landed"));
    }
    let admin = (shape == Shape::Linked).then(|| git_dir.clone());
    // The prints before the proof (the tree's, and a linked tree's
    // administration's): a write while it reads them refuses `raced`.
    let before = retire::prints(beside, admin.as_deref())?;
    // An operation in progress, or a Git lock held, there.
    if advance::in_progress(&git_dir, rules.source)? {
        return Ok(refused("in-progress"));
    }
    // 2. Tracked-clean (and every hidden flag), a sealed hand-off block the
    // one difference allowed.
    let Some(blocks) = clean_but_blocks(beside, &git_dir, rules.source)? else {
        return Ok(refused("tracked-changes"));
    };
    // 3. Its HEAD, reachable from a ref of the live repository.
    let head = head_of(beside)?.oid;
    if head.is_empty() {
        return Ok(refused("unborn"));
    }
    if !clean::reachable_in(live, &head, rules.ns)? {
        return Ok(refused(format!("unreachable:head={head}")));
    }
    // 4. Its Git: every commit it names, with the name it is reported
    // under.
    let mut commits: Vec<(String, String)> = Vec::new();
    let mut logged: Vec<String> = Vec::new();
    let mut refs = Vec::new();
    let (mut config, mut exclude, mut carried) = (Vec::new(), Vec::new(), Vec::new());
    let mut prunable = 0;
    let mut bytes = allocated(beside)?;
    match shape {
        Shape::Linked => {
            if retire::lstat(&git_dir.join("locked"))?.is_some() {
                return Ok(refused("locked"));
            }
            if retire::worktree_refs(&git_dir)? {
                return Ok(refused("worktree-refs"));
            }
            logged.extend(retire::reflogs(&git_dir.join("logs"))?);
            bytes += allocated(&git_dir)?;
        }
        Shape::Standalone => {
            match registered(beside, &git_dir)? {
                Ok((gone, named, reflogged)) => {
                    prunable = gone;
                    commits.extend(named);
                    logged.extend(reflogged);
                }
                Err(why) => return Ok(refused(why)),
            }
            if let Some(why) = unknown_git(&git_dir)? {
                return Ok(refused(why));
            }
            match receipts(&git_dir)? {
                Some(read) => carried = read,
                None => return Ok(refused("carry-config")),
            }
            if borrowed_by(&live_common, &git_dir) {
                return Ok(refused("borrowed-by-live"));
            }
            let hooks = git_dir.join("hooks");
            if retire::lstat(&hooks)?.is_some_and(|meta| meta.is_dir())
                && retire::walk(&hooks, true)?
                    .iter()
                    .any(|entry| !entry.meta.is_dir() && !entry.relative.ends_with(b".sample"))
            {
                return Ok(refused("hooks"));
            }
            let Ok(raw) = fs::read(git_dir.join(CONFIG)) else {
                return Ok(refused("config-unreadable"));
            };
            if raw
                .to_ascii_lowercase()
                .windows(8)
                .any(|w| w == b"[include")
            {
                return Ok(refused("config-include"));
            }
            config = raw;
            exclude = match fs::read(git_dir.join("info/exclude")) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => return Ok(refused("exclude-unreadable")),
            };
            match standalone_refs(beside, live, rules.ns)? {
                Ok((named, wanted)) => {
                    commits.extend(named);
                    refs = wanted;
                }
                Err(why) => return Ok(refused(why)),
            }
            logged.extend(retire::reflogs(&git_dir.join("logs"))?);
        }
        Shape::Empty => {}
    }
    // Every commit it names is kept: each ref the live repository does not
    // hold by name and value goes in under the namespace (`refs`), and each
    // commit only its reflogs (or a gone worktree's) name that no ref the
    // live repository will hold reaches is carried in under
    // `refs/<ns>/reflog/` (full history: nothing dropped). The objects the
    // live store lacks are packed from the tree.
    let all: Vec<String> = commits.iter().map(|(_, commit)| commit.clone()).collect();
    let wanted: Vec<String> = refs.iter().map(|(_, commit)| commit.clone()).collect();
    let in_live = present(live, &[all.as_slice(), wanted.as_slice()].concat())?;
    if let Some(lost) = wanted.iter().find(|commit| !in_live.contains(*commit)) {
        if !present(beside, std::slice::from_ref(lost))?.contains(lost) {
            return Ok(refused(format!("missing:{lost}")));
        }
    }
    let mut live_tips = clean::live_tips(live, rules.ns)?;
    live_tips.extend(
        wanted
            .iter()
            .filter(|commit| in_live.contains(*commit))
            .cloned(),
    );
    let (mut reflogs, _) = match unreached(beside, (live, &live_tips), logged)? {
        Ok(found) => found,
        Err(why) => return Ok(refused(why)),
    };
    // A reflog commit a ref to create holds is in by that ref.
    reflogs.retain(|commit| !wanted.contains(commit));
    let carried_commits: BTreeSet<&String> = reflogs.iter().chain(wanted.iter()).collect();
    let carried_commits: Vec<String> = carried_commits.into_iter().cloned().collect();
    let objects_absent =
        u64::try_from(carried_commits.len() - present(live, &carried_commits)?.len())
            .unwrap_or(u64::MAX);
    // What the pack leaves out: every commit of its own the live store
    // already holds.
    let tips: Vec<String> = all
        .into_iter()
        .filter(|commit| in_live.contains(commit))
        .collect();
    // 5. Its untracked and ignored entries.
    let tracked = files::index_paths(beside)?;
    let mut dirs = BTreeSet::new();
    for path in &tracked {
        for (at, byte) in path.iter().enumerate() {
            if *byte == b'/' {
                dirs.insert(path.get(..at).unwrap_or_default().to_vec());
            }
        }
    }
    let mut rebuildable = 0_u64;
    let mut dropped: Vec<(Vec<u8>, String)> = Vec::new();
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
            dropped.push((path.clone(), compared(beside, live, &path)?));
            continue;
        }
        if entry.meta.is_dir()
            && REBUILDABLE_DIRECTORIES
                .iter()
                .any(|candidate| candidate.as_bytes() == name)
            && !tracks(&tracked, &path)
        {
            if !rules.drop_rebuildable {
                return Ok(refused(format!("rebuildable:{}", shown(&path))));
            }
            rebuildable += 1;
            dropped.push((path.clone(), compared(beside, live, &path)?));
            roots.push(path);
            continue;
        }
        match Want::on_disk(beside, path.clone())? {
            Some(want) => wants.push(want),
            None => return Ok(refused(format!("special:{}", shown(&path)))),
        }
    }
    wants.sort_by(|a, b| a.path.cmp(&b.path));
    let live_tracked = files::index_paths(live)?;
    let planned = files::plan(
        live,
        rules.suffix,
        (&wants, &live_tracked),
        rules.place_derived,
        None,
    )?;
    if let Some(first) = planned.occupied().first() {
        return Ok(refused(format!("occupied:{}", shown(first))));
    }
    // Derived output it would have to place waits for the operator's
    // ruling on build output: the tree keeps it meanwhile.
    if let Some(first) = planned.held(&wants).first() {
        return Ok(refused(format!(
            "derived-held:{}(+{})",
            shown(first),
            planned.held(&wants).len()
        )));
    }
    if retire::prints(beside, admin.as_deref())? != before {
        return Ok(refused("raced"));
    }
    let files = u64::try_from(tracked.iter().filter(|path| !dirs.contains(*path)).count())
        .unwrap_or(u64::MAX);
    Ok(Verdict::Retirable(Box::new(Proof {
        shape,
        head,
        tracked: files,
        tally: planned.tally(&wants),
        rebuildable,
        dropped,
        handoff,
        refs,
        bytes,
        config,
        exclude,
        carried,
        prunable,
        reflogs,
        objects_absent,
        tips,
        prints: before,
        wants,
    })))
}

/// What the ledger keeps of one retired beside tree
/// (`beside-retire-<tag>-<ns>.retired` in STATE, one per retirement).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredRecord {
    pub path: Vec<u8>,
    pub live: Vec<u8>,
    pub shape: String,
    pub head: String,
    pub tracked: u64,
    pub tally: Tally,
    pub rebuildable: u64,
    pub handoff: u64,
    pub refs: Vec<(String, String)>,
    pub bytes: u64,
    /// A standalone tree's configuration and `info/exclude` bytes.
    pub config: Vec<u8>,
    pub exclude: Vec<u8>,
    /// A standalone bulkload landing's `.git/carry-config` receipts.
    pub carried: Receipts,
    /// Prunable worktrees its `.git` registered.
    pub prunable: u64,
    /// The refs its reflog commits went in under, `(name, commit)`.
    pub reflogs: Vec<(String, String)>,
    /// Each rebuildable entry dropped with it, and what the live checkout
    /// held at its path (`equal`, `differs`, `absent`).
    pub dropped: Vec<(Vec<u8>, String)>,
    /// Its reconcile's record (every placement, by identity).
    pub files_record: Vec<u8>,
    /// Unix nanoseconds at the proof.
    pub at_ns: i128,
    /// The removal began (the tree is renamed aside or gone).
    pub deleting: bool,
    /// The tree is gone.
    pub retired: bool,
}

impl landed::Record for RetiredRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-subset2";
}

/// The tag a beside tree's records are named by in STATE.
#[must_use]
pub fn tag(beside: &Path) -> String {
    blake3::hash(beside.as_os_str().as_bytes())
        .to_hex()
        .to_string()
}

/// Where a beside tree's retirement records are in `state`: its file
/// reconcile and its removal intent. The ledger record is one per
/// retirement ([`ledger_path`]).
#[must_use]
pub fn records(state: &Path, beside: &Path) -> (PathBuf, PathBuf) {
    let tag = tag(beside);
    (
        state.join(format!("beside-retire-{tag}.files")),
        state.join(format!("beside-retire-{tag}.retire")),
    )
}

/// The ledger record of the retirement named `item`
/// (`beside-retire-<tag>-<ns>`) in `state`.
#[must_use]
pub fn ledger_path(state: &Path, item: &str) -> PathBuf {
    state.join(format!("{item}.retired"))
}

// Mark the ledger record at `path` retired.
fn mark_retired(path: &Path) -> Result<()> {
    if let Some(mut record) = landed::read::<RetiredRecord>(path)? {
        if !record.retired {
            record.retired = true;
            landed::write(path, &record)?;
        }
    }
    Ok(())
}

/// Finish every removal a crash cut short in `state`.
///
/// Each removal whose intent is there is finished and recorded in its
/// ledger record, and a ledger record whose removal had begun is marked
/// retired when its intent is gone and its tree too: the beside paths and
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
    let mut ledgers = Vec::new();
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        let name = entry.file_name();
        let name = name.as_bytes();
        if !name.starts_with(b"beside-retire-") {
            continue;
        }
        if name.ends_with(b".retire") {
            intents.push(entry.path());
        } else if name.ends_with(b".retired") {
            ledgers.push(entry.path());
        }
    }
    intents.sort();
    for intent in intents {
        let Some(read) = landed::read::<retire::RetireIntent>(&intent)? else {
            continue;
        };
        if let Some(done) = retire::settle(&intent)? {
            if done == Retired::Retired {
                mark_retired(&ledger_path(state, &read.item))?;
            }
            settled.push((PathBuf::from(OsStr::from_bytes(&read.path)), done));
        }
    }
    ledgers.sort();
    for ledger in ledgers {
        let Some(record) = landed::read::<RetiredRecord>(&ledger)? else {
            continue;
        };
        let path = PathBuf::from(OsStr::from_bytes(&record.path));
        let (_, intent) = records(state, &path);
        if record.deleting
            && !record.retired
            && retire::lstat(&path)?.is_none()
            && retire::lstat(&intent)?.is_none()
        {
            mark_retired(&ledger)?;
            settled.push((path, Retired::Retired));
        }
    }
    Ok(settled)
}

/// How many removals a crash cut short `state` holds (read-only).
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
#[allow(clippy::too_many_lines)]
pub fn retire(
    state: &Path,
    source: &str,
    (beside, live): (&Path, &Path),
    rules: Rules<'_>,
    proof: &Proof,
) -> Result<Retired> {
    let (files_record, intent) = records(state, beside);
    let at_ns = now_ns();
    let item = format!("beside-retire-{}-{at_ns}", tag(beside));
    let ledger = ledger_path(state, &item);
    let mut record = RetiredRecord {
        path: beside.as_os_str().as_bytes().to_vec(),
        live: live.as_os_str().as_bytes().to_vec(),
        shape: proof.shape.word().to_owned(),
        head: proof.head.clone(),
        tracked: proof.tracked,
        tally: proof.tally,
        rebuildable: proof.rebuildable,
        handoff: proof.handoff,
        refs: proof.refs.clone(),
        bytes: proof.bytes,
        config: proof.config.clone(),
        exclude: proof.exclude.clone(),
        carried: proof.carried.clone(),
        prunable: proof.prunable,
        dropped: proof.dropped.clone(),
        files_record: files_record.as_os_str().as_bytes().to_vec(),
        at_ns,
        ..RetiredRecord::default()
    };
    if proof.shape == Shape::Empty {
        record.deleting = true;
        landed::write(&ledger, &record)?;
        // `rmdir` removes an empty directory only: one written into since
        // the proof stays.
        if fs::remove_dir(beside).is_err() {
            return Ok(Retired::Kept("raced".to_owned()));
        }
        if let Some(parent) = beside.parent() {
            landed::sync_dir(parent)?;
        }
        mark_retired(&ledger)?;
        return Ok(Retired::Retired);
    }
    let git_dir = landed::worktree_git_dir(live)?.ok_or(BulkloadRefusal::GitDestinationOccupied)?;
    let common = landed::common_dir(&git_dir)?;
    let _lock = landed::lock(&common, source)?;
    landed::write(&ledger, &record)?;
    // The file-level reconcile, recorded write-ahead.
    files::settle(&files_record)?;
    let salt = files_record.as_os_str().as_bytes();
    let live_tracked = files::index_paths(live)?;
    let planned = files::plan(
        live,
        rules.suffix,
        (&proof.wants, &live_tracked),
        rules.place_derived,
        None,
    )?;
    if !planned.occupied().is_empty() || !planned.held(&proof.wants).is_empty() {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    let mut journal = files::Journal::begin(
        &files_record,
        files::FilesRecord {
            item: item.clone(),
            capture: *blake3::hash(proof.head.as_bytes()).as_bytes(),
            live: record.live.clone(),
            suffix: rules.suffix.to_owned(),
            tracked: proof.tracked,
            temps: files::temps(live, &planned, salt),
            besides: planned.besides(&proof.wants),
            ..files::FilesRecord::default()
        },
    )?;
    let tally = files::execute(live, &planned, &proof.wants, (salt, &mut journal), None)?;
    journal.finish(tally)?;
    // Its history, carried in (full history, nothing dropped): the
    // objects the live store lacks, its refs, its reflog commits; a name
    // taken since keeps the tree.
    match carry_history((beside, live), rules.ns, proof)? {
        Ok(written) => record.reflogs = written,
        Err(why) => return Ok(Retired::Kept(why)),
    }
    landed::write(&ledger, &record)?;
    // Proved again: every entry is now in the live checkout.
    let started = retire::proof_start_ns();
    let Verdict::Retirable(again) = prove(beside, live, rules)? else {
        return Ok(Retired::Kept("raced".to_owned()));
    };
    if again.tally.placed != 0
        || again.tally.beside != 0
        || again.tally.dirs_placed != 0
        || !again.reflogs.is_empty()
    {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    // The last look before the rename (its prints again inside the
    // removal): a commit made there since the proof keeps the tree.
    if head_of(beside)?.oid != again.head {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    record.deleting = true;
    landed::write(&ledger, &record)?;
    let capture = *blake3::hash(format!("{item}\0{}", proof.head).as_bytes()).as_bytes();
    let done = retire::remove_proved(
        &intent,
        (beside, live),
        (&item, capture),
        (again.prints, started),
    )?;
    if done == Retired::Retired {
        mark_retired(&ledger)?;
    }
    Ok(done)
}

/// The beside trees under `root`, sorted.
///
/// Every directory to `depth` levels below it (never following a link,
/// never into a `.git`, and into a checkout only through its `.worktrees`
/// directory) whose name ends with `suffix` and whose live path (the name
/// without it) exists.
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
            if level + 1 >= depth {
                continue;
            }
            // A checkout's tree is never walked, but its `.worktrees`
            // directory (the estate's convention for its linked worktrees,
            // which may hold beside trees) is.
            if retire::lstat(&path.join(".git"))?.is_none() {
                pending.push((path, level + 1));
            } else {
                let worktrees = path.join(".worktrees");
                if retire::lstat(&worktrees)?.is_some_and(|meta| meta.is_dir()) {
                    pending.push((worktrees, level + 1));
                }
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
