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
//!    retirable as it is; otherwise a checkout, whose live path holds a
//!    checkout: a linked worktree of the live repository whose
//!    administration directory names it back (a copy of another worktree,
//!    its gitfile naming that one's, refuses `worktree-link`), or a
//!    standalone repository whose `.git` is a directory in the tree;
//! 2. **tracked-clean**: no staged or unstaged change, no operation in
//!    progress, no hidden index flag, no index lock;
//! 3. **its HEAD reachable in the live repository** from a ref (provenance
//!    `refs/carry*` aside, and never through its own worktree HEAD);
//! 4. **nothing its Git holds lost with it**: a linked tree has no lock and
//!    no per-worktree ref, and every commit its reflogs name is reachable
//!    in the live repository; a standalone one has no worktree and no hook
//!    of its own, and every commit its refs, stash and reflogs name is in
//!    the live repository and reachable there (`unreachable:<name>`; a
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
//!    kept in the retirement's ledger record;
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
//!    them as a capture omits them (`--drop-rebuildable`: counted, never
//!    reconciled). A nested repository refuses `nested:<path>`.
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
    /// Entries under a rebuildable root (`--drop-rebuildable`), not
    /// reconciled.
    pub rebuildable: u64,
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
    prints: [u8; 32],
    admin_prints: [u8; 32],
    wants: Vec<Want>,
}

/// What [`prove`] may take as given.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rules<'a> {
    /// The file-level beside suffix (`.neo-20261016`).
    pub suffix: &'a str,
    /// The ref namespace (the suffix without its leading `.`).
    pub ns: &'a str,
    /// `--drop-rebuildable`: entries under a rebuildable root are counted
    /// and go with the tree, as a capture omits them; without it one
    /// refuses `rebuildable:<path>`.
    pub drop_rebuildable: bool,
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

// The prints of a linked tree's administration directory (its HEAD, index
// and reflog live there): a commit made in the tree while it is proved
// changes them.
fn admin_prints(admin: Option<&Path>) -> Result<[u8; 32]> {
    admin.map_or(Ok([0; 32]), retire::prints)
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
            config: Vec::new(),
            exclude: Vec::new(),
            prints: before,
            admin_prints: [0; 32],
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
    let admin = (shape == Shape::Linked).then(|| git_dir.clone());
    let admin_before = admin_prints(admin.as_deref())?;
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
    let (mut config, mut exclude) = (Vec::new(), Vec::new());
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
            if let Some(why) = unknown_git(&git_dir)? {
                return Ok(refused(why));
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
            commits.extend(labelled("logged", retire::reflogs(&git_dir.join("logs"))?));
        }
        Shape::Empty => {}
    }
    if let Some(why) = unreachable(live, commits) {
        return Ok(refused(why));
    }
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
            if !rules.drop_rebuildable {
                return Ok(refused(format!("rebuildable:{}", shown(&path))));
            }
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
    let live_tracked = files::index_paths(live)?;
    let planned = files::plan(live, rules.suffix, (&wants, &live_tracked), None)?;
    if let Some(first) = planned.occupied().first() {
        return Ok(refused(format!("occupied:{}", shown(first))));
    }
    if retire::prints(beside)? != before || admin_prints(admin.as_deref())? != admin_before {
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
        config,
        exclude,
        prints: before,
        admin_prints: admin_before,
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
    let planned = files::plan(live, rules.suffix, (&proof.wants, &live_tracked), None)?;
    if !planned.occupied().is_empty() {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    let mut reconciled = files::FilesRecord {
        item: item.clone(),
        capture: *blake3::hash(proof.head.as_bytes()).as_bytes(),
        live: record.live.clone(),
        suffix: rules.suffix.to_owned(),
        tracked: proof.tracked,
        temps: files::temps(live, &planned, salt),
        dirs: files::dirs(live, &planned, &proof.wants)?,
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
    let Verdict::Retirable(again) = prove(beside, live, rules)? else {
        return Ok(Retired::Kept("raced".to_owned()));
    };
    if again.tally.placed != 0 || again.tally.beside != 0 || again.tally.dirs_placed != 0 {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    // The last look before the rename: a commit made there since the
    // proof (its HEAD, index or reflog) keeps the tree.
    let admin = (again.shape == Shape::Linked)
        .then(|| landed::worktree_git_dir(beside).ok().flatten())
        .flatten();
    if head_of(beside)?.oid != again.head || admin_prints(admin.as_deref())? != again.admin_prints {
        return Ok(Retired::Kept("raced".to_owned()));
    }
    record.deleting = true;
    landed::write(&ledger, &record)?;
    let capture = *blake3::hash(format!("{item}\0{}", proof.head).as_bytes()).as_bytes();
    let done = retire::remove_proved(&intent, beside, (&item, capture), again.prints)?;
    if done == Retired::Retired {
        mark_retired(&ledger)?;
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
