//! `worktree-fold` and `worktree-reattach` (TIN-7491).
//!
//! They fold the linked worktrees of a kept clone into the canonical
//! repository on the same host, and give a worktree whose admin directory
//! is gone a new one.
//!
//! Operator rulings, Linear TIN-3692: `ae17f1bd` (BL-2 selected "Worktree
//! fold/reattach verbs"), and `36925e2d`: BL2-Q2 "Dated exception, verbs
//! next" (later carries go through bulkload verbs), BL2-Q3 "10% default,
//! lower by your word" (the free-space floor), and BL2-small (a rollback
//! may delete a branch the same pass created, and nothing else). The steps
//! productize the KC-1 scripts (`2aa6f9f4`, `297b6576`, `a2c52e93`).
//!
//! - `plan` reads both repositories and writes `STATE/plan.tsv`: per
//!   worktree of the kept clone, its `.git` pointer, HEAD, branch, the
//!   admin index's SHA-256 and the SHA-256 of `status -z`, the admin name
//!   it takes in the canonical repository, and what HEAD becomes there
//!   (`create`, `attach`, `detached` or `detach`). A live worktree is
//!   planned and marked.
//! - `refs` fetches every ref of the kept clone into the canonical
//!   repository under a ruled namespace (`refs/kept-HOST-DATE`), at most
//!   25,000 refs a batch, and packs each batch into `packed-refs`. Every
//!   fetch is [`super::git_fetch`]'s: no prune, no automatic gc or
//!   maintenance.
//! - `objects` packs into the canonical repository every object the kept
//!   clone reaches from its refs, reflogs, indexes and worktree HEADs that
//!   the canonical repository lacks. One new pack; nothing is repacked.
//! - `attach` moves each planned worktree: the kept admin directory is
//!   copied into the canonical `.git/worktrees`, HEAD set by the plan, the
//!   worktree's `.git` repointed, and the kept admin directory parked at
//!   `.git/worktrees-parked-TAG/NAME`, outside `.git/worktrees`. Then HEAD,
//!   branch, index and `status -z` must equal the plan. A miss is undone by
//!   renames; a branch this pass created is deleted, and nothing else is.
//! - `verify` checks every planned worktree against the plan, and runs
//!   `fsck --connectivity-only` on the canonical repository.
//! - `reattach` gives one worktree whose admin directory is missing a new
//!   one in the canonical repository, with its index read from HEAD.
//!
//! No step runs `git worktree repair`, `prune` or `gc`, and no step deletes
//! a file. Every worktree step first proves the worktree quiet: no process
//! with its working directory or an open file inside (`procs::inside`), and
//! no file written in the last 30 minutes. Every writing step checks the
//! free-space floor of the canonical repository's filesystem (or its
//! project quota) first. Every step appends its receipt lines, with the
//! floor and the operator's ruling, to `STATE/worktree-fold.receipts`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use super::{git, git_fetch, input, oid, output, text, without_graph};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "git_carry::fold";

/// The fold verbs' free-space floor unless `--min-free-percent` is given
/// (BL2-Q3, operator 2026-10-27: "10% default, lower by your word").
pub const DEFAULT_MIN_FREE_PERCENT: u8 = 10;

/// How long a worktree must have gone unwritten before it moves.
const QUIET: Duration = Duration::from_mins(30);

/// The most refs one fetch batch carries (TIN-7491).
const REF_BATCH: usize = 25_000;

const PLAN: &str = "plan.tsv";
const RECEIPTS: &str = "worktree-fold.receipts";
const PLAN_HEADER: &str = "# bulkload worktree-fold plan v1";

/// The floor a step runs at, and whether a flag set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floor {
    pub percent: u8,
    pub from_flag: bool,
}

impl Floor {
    /// The floor as the receipts and the summary line record it.
    #[must_use]
    pub fn field(self) -> String {
        let from = if self.from_flag { "flag" } else { "default" };
        format!("min_free_percent={} from={from}", self.percent)
    }
}

/// What every `worktree-fold` step is given.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The repository the worktrees move into.
    pub canonical: &'a Path,
    /// The clone whose worktrees move.
    pub kept: &'a Path,
    /// This fold's private directory: the plan, its before records and the
    /// receipts.
    pub state: &'a Path,
    pub floor: Floor,
    /// The operator's ruling the receipts cite.
    pub ruling: &'a str,
}

/// Why a worktree, or a step, was held. Nothing is written for a held
/// item, or what was written is undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// Its `.git` does not point at the kept clone's admin directory.
    NotBound,
    /// A path or name that is not UTF-8, or holds a tab or a newline.
    NotPortable,
    /// A process has its working directory or an open file inside.
    LiveProcess,
    /// This platform cannot say which processes are inside.
    LiveUnknown,
    /// A file inside was written in the last 30 minutes, or the tree could
    /// not be read to tell.
    RecentWrite,
    /// HEAD does not resolve.
    HeadUnreadable,
    /// `status` failed.
    StatusFailed,
    /// HEAD, branch, index, status or `.git` changed since the plan.
    Changed,
    /// The planned admin name is taken in the canonical repository.
    AdminTaken,
    /// The planned branch could not be created.
    BranchCreate,
    /// The copied admin directory's index differs from the plan.
    CopyMismatch,
    /// The moved worktree did not verify; the move was undone.
    Verify,
    /// The free-space floor would be crossed.
    Floor,
}

impl Hold {
    const fn word(self) -> &'static str {
        match self {
            Self::NotBound => "not-bound",
            Self::NotPortable => "not-portable",
            Self::LiveProcess => "live-process",
            Self::LiveUnknown => "live-unknown",
            Self::RecentWrite => "recent-write",
            Self::HeadUnreadable => "head-unreadable",
            Self::StatusFailed => "status-failed",
            Self::Changed => "changed-since-plan",
            Self::AdminTaken => "admin-name-taken",
            Self::BranchCreate => "branch-create-failed",
            Self::CopyMismatch => "copy-mismatch",
            Self::Verify => "verify-failed",
            Self::Floor => "floor",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [
            Self::NotBound,
            Self::NotPortable,
            Self::LiveProcess,
            Self::LiveUnknown,
            Self::RecentWrite,
            Self::HeadUnreadable,
            Self::StatusFailed,
        ]
        .into_iter()
        .find(|hold| hold.word() == word)
    }

    /// Whether a plan row held this way has a before record to move by.
    const fn recorded(self) -> bool {
        matches!(
            self,
            Self::LiveProcess | Self::LiveUnknown | Self::RecentWrite
        )
    }
}

/// What HEAD becomes in the canonical repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The branch is new to the canonical repository: create it at HEAD,
    /// then attach.
    Create,
    /// The canonical branch is at HEAD and checked out nowhere else.
    Attach,
    /// HEAD was detached already.
    Detached,
    /// The canonical branch is elsewhere, or checked out by another
    /// worktree: detach at HEAD, so no branch moves.
    Detach,
    /// Not planned (held at plan time without a record).
    Skip,
}

impl Kind {
    const fn word(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Attach => "attach",
            Self::Detached => "detached",
            Self::Detach => "detach",
            Self::Skip => "skip",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [
            Self::Create,
            Self::Attach,
            Self::Detached,
            Self::Detach,
            Self::Skip,
        ]
        .into_iter()
        .find(|kind| kind.word() == word)
    }
}

/// One worktree of the kept clone, as planned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    /// Its admin directory's name in the kept clone.
    name: String,
    /// The worktree's path.
    path: String,
    /// The admin name it takes in the canonical repository.
    target: String,
    /// The canonical repository already has an admin directory for this
    /// path under `target`, which is parked first.
    park: bool,
    /// The worktree's `.git` file.
    dotgit: String,
    /// The kept admin directory's `HEAD` file.
    head_file: String,
    head: String,
    /// The symbolic ref HEAD names, or empty when detached.
    branch: String,
    /// SHA-256 of the admin index, or `none`.
    index: String,
    /// SHA-256 of `status --porcelain=v1 -z -uall`.
    status: String,
    entries: usize,
    kind: Kind,
    hold: Option<Hold>,
}

impl Row {
    fn held(name: &str, path: &str, hold: Hold) -> Self {
        Self {
            name: name.to_owned(),
            path: path.to_owned(),
            target: String::new(),
            park: false,
            dotgit: String::new(),
            head_file: String::new(),
            head: String::new(),
            branch: String::new(),
            index: String::new(),
            status: String::new(),
            entries: 0,
            kind: Kind::Skip,
            hold: Some(hold),
        }
    }

    fn line(&self) -> String {
        let fields = [
            self.name.as_str(),
            self.path.as_str(),
            self.target.as_str(),
            if self.park { "1" } else { "0" },
            self.dotgit.as_str(),
            self.head_file.as_str(),
            self.head.as_str(),
            self.branch.as_str(),
            self.index.as_str(),
            self.status.as_str(),
        ];
        let mut line: Vec<String> = fields.iter().map(|field| cell(field).to_owned()).collect();
        line.push(self.entries.to_string());
        line.push(self.kind.word().to_owned());
        line.push(self.hold.map_or("-", Hold::word).to_owned());
        line.join("\t")
    }

    fn parse(line: &str) -> Result<Self> {
        let mut fields = line.split('\t');
        let mut next = || fields.next().ok_or(BulkloadRefusal::SchemaMismatch);
        let mut text = || {
            next().map(|field| {
                if field == "-" {
                    String::new()
                } else {
                    field.to_owned()
                }
            })
        };
        let (name, path, target) = (text()?, text()?, text()?);
        let park = text()? == "1";
        let (dotgit, head_file, head, branch) = (text()?, text()?, text()?, text()?);
        let (index, status, entries) = (text()?, text()?, text()?);
        let (kind, hold) = (text()?, text()?);
        if text().is_ok() {
            return Err(BulkloadRefusal::SchemaMismatch);
        }
        Ok(Self {
            name,
            path,
            target,
            park,
            dotgit,
            head_file,
            head,
            branch,
            index,
            status,
            entries: entries
                .parse()
                .map_err(|_| BulkloadRefusal::SchemaMismatch)?,
            kind: Kind::parse(&kind).ok_or(BulkloadRefusal::SchemaMismatch)?,
            hold: if hold.is_empty() {
                None
            } else {
                Some(Hold::parse(&hold).ok_or(BulkloadRefusal::SchemaMismatch)?)
            },
        })
    }

    /// Whether `attach` moves this row.
    fn planned(&self) -> bool {
        self.kind != Kind::Skip && self.hold.is_none_or(Hold::recorded)
    }
}

/// A plan field: `-` for empty.
const fn cell(field: &str) -> &str {
    if field.is_empty() {
        "-"
    } else {
        field
    }
}

/// A field fit for the plan and the receipts: UTF-8, with no tab, newline
/// or NUL.
fn portable(value: &Path) -> Option<&str> {
    value
        .to_str()
        .filter(|text| !text.contains(['\t', '\n', '\r', '\0']))
}

// ---------------------------------------------------------------------------
// Receipts
// ---------------------------------------------------------------------------

/// `STATE/worktree-fold.receipts`, appended and synced line by line.
struct Receipts {
    file: fs::File,
    step: &'static str,
    repo: String,
    floor: Floor,
    ruling: String,
}

impl Receipts {
    fn open(cx: &Context<'_>, step: &'static str) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(cx.state.join(RECEIPTS))
            .refuse_at(SITE)?;
        Ok(Self {
            file,
            step,
            repo: cx.canonical.display().to_string(),
            floor: cx.floor,
            ruling: cx.ruling.to_owned(),
        })
    }

    fn row(
        &mut self,
        item: &str,
        action: &str,
        before: &str,
        after: &str,
        result: &str,
    ) -> Result<()> {
        let at_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let flat = |field: &str| field.replace(['\t', '\n', '\r'], " ");
        let line = format!(
            "{at_ns}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.step,
            flat(&self.repo),
            flat(item),
            flat(action),
            flat(before),
            flat(after),
            flat(result),
            self.floor.field(),
            flat(&self.ruling),
        );
        self.file.write_all(line.as_bytes()).refuse_at(SITE)?;
        self.file.sync_data().refuse_at(SITE)
    }
}

// ---------------------------------------------------------------------------
// Reading the repositories
// ---------------------------------------------------------------------------

/// `REPO/.git`, which must be a directory: a non-bare repository at its
/// root.
fn git_dir(repo: &Path) -> Result<PathBuf> {
    let dir = repo.join(".git");
    if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        Ok(dir)
    } else {
        Err(BulkloadRefusal::GitRepositoryNotAtPath)
    }
}

/// Each admin directory of `COMMON/worktrees`, with its `gitdir` file
/// (empty when it has none).
fn admin_dirs(common: &Path) -> Result<BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    let entries = match fs::read_dir(common.join("worktrees")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let entry = entry.refuse_at(SITE)?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let gitdir = fs::read_to_string(entry.path().join("gitdir"))
            .map(|text| text.trim_end().to_owned())
            .unwrap_or_default();
        found.insert(name, gitdir);
    }
    Ok(found)
}

/// One `worktree list --porcelain -z` record.
#[derive(Debug, Default)]
struct Listed {
    path: PathBuf,
    branch: Option<String>,
    prunable: bool,
}

fn listing(repo: &Path) -> Result<Vec<Listed>> {
    use std::os::unix::ffi::OsStrExt as _;
    let raw = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut all: Vec<Listed> = Vec::new();
    for field in raw.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            all.push(Listed {
                path: PathBuf::from(std::ffi::OsStr::from_bytes(path)),
                ..Listed::default()
            });
            continue;
        }
        let Some(last) = all.last_mut() else {
            continue;
        };
        if let Some(name) = field.strip_prefix(b"branch ") {
            last.branch = Some(String::from_utf8_lossy(name).into_owned());
        }
        if field.starts_with(b"prunable") {
            last.prunable = true;
        }
    }
    Ok(all)
}

/// Whether two paths name one place: equal, or equal once resolved.
fn same_place(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (fs::canonicalize(left), fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

/// The commit `name` resolves to in `repo`, if any.
fn resolve(repo: &Path, name: &str) -> Option<String> {
    text(
        git(repo)
            .args(["rev-parse", "-q", "--verify"])
            .arg(format!("{name}^{{commit}}")),
    )
    .ok()
    .filter(|value| oid(value))
}

/// The ref HEAD names in the worktree at `path`, or empty when detached.
fn symbolic_head(path: &Path) -> Result<String> {
    let out = git(path)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .refuse_at(SITE)?;
    Ok(if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
    } else {
        String::new()
    })
}

/// `status --porcelain=v1 -z -uall` of the worktree at `path`, and whether
/// it succeeded. The builder's `GIT_OPTIONAL_LOCKS=0` keeps it from
/// refreshing the index.
fn status(path: &Path) -> Result<(Vec<u8>, bool)> {
    let out = git(path)
        .args(["status", "--porcelain=v1", "-z", "-uall"])
        .output()
        .refuse_at(SITE)?;
    Ok((out.stdout, out.status.success()))
}

/// How many records a `-z` listing holds: its NUL bytes.
fn nul_count(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0, |count, byte| count + usize::from(*byte == 0))
}

/// SHA-256 of a file's bytes, or `none` when it does not exist.
fn file_sha256(path: &Path) -> Result<String> {
    match fs::read(path) {
        Ok(bytes) => Ok(sha256_hex(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("none".to_owned()),
        Err(error) => Err(crate::refuse::io(&error, SITE)),
    }
}

// ---------------------------------------------------------------------------
// Live and floor checks
// ---------------------------------------------------------------------------

/// Why `paths` are not quiet, if they are not: a process inside, a
/// platform that cannot tell, or a file written within [`QUIET`].
fn live(paths: &[&Path]) -> Result<Option<Hold>> {
    match crate::procs::inside(paths)? {
        None => return Ok(Some(Hold::LiveUnknown)),
        Some(pids) if !pids.is_empty() => return Ok(Some(Hold::LiveProcess)),
        Some(_) => {}
    }
    let since = SystemTime::now()
        .checked_sub(QUIET)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for path in paths {
        if written_since(path, since) {
            return Ok(Some(Hold::RecentWrite));
        }
    }
    Ok(None)
}

/// Whether anything at or under `root` was modified after `since`, links
/// not followed. A node that cannot be read counts as written: quiet must
/// be proved.
fn written_since(root: &Path, since: SystemTime) -> bool {
    let newer = |meta: &fs::Metadata| meta.modified().map_or(true, |at| at > since);
    match fs::symlink_metadata(root) {
        Ok(meta) if newer(&meta) => return true,
        Ok(meta) if !meta.is_dir() => return false,
        Ok(_) => {}
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            return true;
        };
        for entry in entries {
            let Ok(meta) = entry.and_then(|entry| {
                let meta = entry.metadata()?;
                if meta.is_dir() {
                    pending.push(entry.path());
                }
                Ok(meta)
            }) else {
                return true;
            };
            if newer(&meta) {
                return true;
            }
        }
    }
    false
}

/// Whether `planned` more bytes under `path` keep the floor, on its
/// filesystem and on its project quota.
fn floor_holds(path: &Path, planned: u64, floor: Floor) -> Result<bool> {
    let want = crate::space::locate(path, planned.max(1))?;
    match crate::space::check_planned(&[want], floor.percent) {
        Ok(()) => Ok(true),
        Err(BulkloadRefusal::DestinationSpaceInsufficient) => {
            crate::space::forget_refusal();
            Ok(false)
        }
        Err(other) => Err(other),
    }
}

/// The state directory, made private when it is new.
fn state_dir(state: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state.join("before"))
        .refuse_at(SITE)
}

// ---------------------------------------------------------------------------
// plan
// ---------------------------------------------------------------------------

/// `worktree-fold plan`: the census and the move plan, read-only on both
/// repositories. `tag` names this fold's parked and staging directories.
///
/// # Errors
/// A repository that is not one at its path, a tag that is not
/// `[a-z0-9-]+`, a Git or IO failure. A held worktree is planned as held,
/// not an error.
pub fn plan(cx: &Context<'_>, tag: &str) -> Result<String> {
    if tag.is_empty()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let canonical_git = git_dir(cx.canonical)?;
    let kept_git = git_dir(cx.kept)?;
    let (Some(canonical), Some(kept)) = (portable(cx.canonical), portable(cx.kept)) else {
        return Err(BulkloadRefusal::PathNotPortable);
    };
    state_dir(cx.state)?;
    let mut receipts = Receipts::open(cx, "plan")?;
    let mut taken = admin_dirs(&canonical_git)?;
    let mut checked_out: BTreeMap<String, PathBuf> = listing(cx.canonical)?
        .into_iter()
        .filter_map(|entry| entry.branch.map(|branch| (branch, entry.path)))
        .collect();
    let mut created: BTreeMap<String, String> = BTreeMap::new();
    let mut rows = Vec::new();
    for (name, gitdir) in admin_dirs(&kept_git)? {
        let row = census(
            cx,
            (&kept_git, &name, &gitdir),
            tag,
            (&mut taken, &mut checked_out, &mut created),
        )?;
        rows.push(row);
    }
    let mut text = format!("{PLAN_HEADER}\tcanonical={canonical}\tkept={kept}\ttag={tag}\n");
    for row in &rows {
        text.push_str(&row.line());
        text.push('\n');
    }
    write_atomic(&cx.state.join(PLAN), text.as_bytes())?;
    let summary = summarize(&rows);
    receipts.row(PLAN, "census", "", &summary, "ok")?;
    Ok(summary)
}

/// What the canonical repository holds as the plan is made: admin names
/// taken, branches checked out (and where), and branches this plan creates.
type Taken<'a> = (
    &'a mut BTreeMap<String, String>,
    &'a mut BTreeMap<String, PathBuf>,
    &'a mut BTreeMap<String, String>,
);

/// One kept worktree's census and plan.
fn census(
    cx: &Context<'_>,
    (kept_git, name, gitdir): (&Path, &str, &str),
    tag: &str,
    (taken, checked_out, created): Taken<'_>,
) -> Result<Row> {
    let path = gitdir.strip_suffix("/.git").unwrap_or(gitdir);
    if path.contains(['\t', '\n', '\r']) || name.contains(['\t', '\n', '\r']) {
        return Ok(Row::held(name, "-", Hold::NotPortable));
    }
    let admin = kept_git.join("worktrees").join(name);
    let worktree = Path::new(path);
    let dotgit = fs::read_to_string(worktree.join(".git"))
        .map(|text| text.trim_end().to_owned())
        .unwrap_or_default();
    let bound = dotgit
        .strip_prefix("gitdir: ")
        .is_some_and(|pointer| same_place(Path::new(pointer), &admin));
    if !bound {
        return Ok(Row::held(name, path, Hold::NotBound));
    }
    let hold = live(&[worktree, &admin])?;
    let Some(head) = resolve(worktree, "HEAD") else {
        return Ok(Row::held(name, path, Hold::HeadUnreadable));
    };
    let branch = symbolic_head(worktree)?;
    let (bytes, ok) = status(worktree)?;
    if !ok {
        return Ok(Row::held(name, path, Hold::StatusFailed));
    }
    write_atomic(
        &cx.state.join("before").join(format!("{name}.status")),
        &bytes,
    )?;
    let head_file = fs::read_to_string(admin.join("HEAD"))
        .map(|text| text.trim_end().to_owned())
        .unwrap_or_default();
    let canonical_admin = git_dir(cx.canonical)?.join("worktrees");
    let mut target = name.to_owned();
    let mut park = false;
    if let Some(other) = taken.get(name) {
        if same_place(Path::new(other), &worktree.join(".git")) {
            park = true;
        } else {
            let mut next = 1;
            while taken.contains_key(&format!("{name}-{tag}-{next}"))
                || canonical_admin
                    .join(format!("{name}-{tag}-{next}"))
                    .exists()
            {
                next += 1;
            }
            target = format!("{name}-{tag}-{next}");
        }
    }
    taken.insert(target.clone(), format!("{path}/.git"));
    let kind = if branch.is_empty() {
        Kind::Detached
    } else {
        let tip = created
            .get(&branch)
            .cloned()
            .or_else(|| resolve(cx.canonical, &branch));
        let elsewhere = checked_out
            .get(&branch)
            .is_some_and(|other| !same_place(other, worktree));
        match tip {
            None => {
                created.insert(branch.clone(), head.clone());
                checked_out.insert(branch.clone(), worktree.to_path_buf());
                Kind::Create
            }
            Some(tip) if tip != head || elsewhere => Kind::Detach,
            Some(_) => {
                checked_out.insert(branch.clone(), worktree.to_path_buf());
                Kind::Attach
            }
        }
    };
    Ok(Row {
        name: name.to_owned(),
        path: path.to_owned(),
        target,
        park,
        dotgit,
        head_file,
        head,
        branch,
        index: file_sha256(&admin.join("index"))?,
        status: sha256_hex(&bytes),
        entries: nul_count(&bytes),
        kind,
        hold,
    })
}

fn summarize(rows: &[Row]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        let key = row.hold.map_or_else(
            || row.kind.word().to_owned(),
            |hold| format!("{}+{}", row.kind.word(), hold.word()),
        );
        *counts.entry(key).or_insert(0) += 1;
    }
    let mut line = format!("worktrees={}", rows.len());
    for (key, count) in counts {
        let _ = write!(line, " {key}={count}");
    }
    line
}

/// The plan `plan` wrote for these repositories, and its tag.
fn read_plan(cx: &Context<'_>) -> Result<(String, Vec<Row>)> {
    let text = fs::read_to_string(cx.state.join(PLAN)).refuse_at(SITE)?;
    let mut lines = text.lines();
    let header = lines.next().ok_or(BulkloadRefusal::SchemaMismatch)?;
    let mut fields = header.split('\t');
    if fields.next() != Some(PLAN_HEADER) {
        return Err(BulkloadRefusal::SchemaMismatch);
    }
    let canonical = fields
        .next()
        .and_then(|field| field.strip_prefix("canonical="));
    let kept = fields.next().and_then(|field| field.strip_prefix("kept="));
    let tag = fields.next().and_then(|field| field.strip_prefix("tag="));
    let (Some(canonical), Some(kept), Some(tag)) = (canonical, kept, tag) else {
        return Err(BulkloadRefusal::SchemaMismatch);
    };
    if Some(canonical) != cx.canonical.to_str() || Some(kept) != cx.kept.to_str() {
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let rows = lines.map(Row::parse).collect::<Result<Vec<_>>>()?;
    Ok((tag.to_owned(), rows))
}

// ---------------------------------------------------------------------------
// refs
// ---------------------------------------------------------------------------

/// Every ref of `repo`: (object, name), sorted by name.
fn all_refs(repo: &Path) -> Result<Vec<(String, String)>> {
    let listed = text(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
    let mut refs: Vec<(String, String)> = listed
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(object, name)| (object.to_owned(), name.to_owned()))
        .collect();
    refs.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(refs)
}

/// A ruled carry namespace: `refs/kept-` and then `[a-z0-9-]+`.
fn ruled_namespace(namespace: &str) -> bool {
    namespace.strip_prefix("refs/kept-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

/// `worktree-fold refs`: every ref of the kept clone into the canonical
/// repository under `namespace`, in packed batches. Additive only. A rerun
/// fetches only the batches not yet complete.
///
/// # Errors
/// A namespace that is not ruled, or that holds a ref this fold would not
/// write; the floor; a Git or IO failure.
pub fn fold_refs(cx: &Context<'_>, namespace: &str) -> Result<String> {
    if !ruled_namespace(namespace) {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    git_dir(cx.canonical)?;
    git_dir(cx.kept)?;
    let Some(kept_url) = portable(cx.kept) else {
        return Err(BulkloadRefusal::PathNotPortable);
    };
    state_dir(cx.state)?;
    let mut receipts = Receipts::open(cx, "fold-refs")?;
    let kept = all_refs(cx.kept)?;
    let before = all_refs(cx.canonical)?;
    let inside = |name: &str| name.starts_with(&format!("{namespace}/"));
    let expected: BTreeMap<String, String> = kept
        .iter()
        .filter_map(|(object, name)| {
            name.strip_prefix("refs/")
                .map(|rest| (format!("{namespace}/{rest}"), object.clone()))
        })
        .collect();
    let mut present: BTreeMap<String, String> = before
        .iter()
        .filter(|(_, name)| inside(name))
        .map(|(object, name)| (name.clone(), object.clone()))
        .collect();
    if present
        .iter()
        .any(|(name, object)| expected.get(name) != Some(object))
    {
        receipts.row(
            namespace,
            "carry-refs",
            "",
            "namespace holds other refs",
            "held",
        )?;
        return Err(BulkloadRefusal::DestinationOccupied);
    }
    write_lines(&cx.state.join("refs-kept.txt"), &kept)?;
    write_lines(&cx.state.join("refs-canonical-before.txt"), &before)?;
    let wanted: Vec<(&String, &String)> = expected.iter().collect();
    for (index, batch) in wanted.chunks(REF_BATCH).enumerate() {
        let item = format!("batch-{index}");
        if batch
            .iter()
            .all(|(name, object)| present.get(*name) == Some(*object))
        {
            receipts.row(&item, "carry-refs", "", "already present", "ok")?;
            continue;
        }
        let planned = u64::try_from(batch.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(256);
        if !floor_holds(cx.canonical, planned, cx.floor)? {
            receipts.row(&item, "carry-refs", "", "", Hold::Floor.word())?;
            return Err(BulkloadRefusal::DestinationSpaceInsufficient);
        }
        let specs = batch.iter().fold(String::new(), |mut all, (name, _)| {
            let source = name
                .strip_prefix(&format!("{namespace}/"))
                .map_or_else(String::new, |rest| format!("refs/{rest}"));
            let _ = writeln!(all, "+{source}:{name}");
            all
        });
        input(
            git_fetch(&mut git(cx.canonical))
                .args(["--no-tags", "--quiet", "--stdin"])
                .arg(kept_url),
            specs.as_bytes(),
        )?;
        output(
            git(cx.canonical)
                .args(["pack-refs", "--include"])
                .arg(format!("{namespace}/*"))
                .args(["--exclude", "refs/tags/*"]),
        )?;
        present.extend(
            batch
                .iter()
                .map(|(name, object)| ((*name).clone(), (*object).clone())),
        );
        receipts.row(
            &item,
            "carry-refs",
            &format!("refs={}", batch.len()),
            "packed",
            "ok",
        )?;
    }
    verify_refs(cx, namespace, (&expected, &before), &mut receipts)
}

/// Every kept ref under `namespace` at its object, and how many refs
/// outside it changed while the step ran (a live repository's own work).
fn verify_refs(
    cx: &Context<'_>,
    namespace: &str,
    (expected, before): (&BTreeMap<String, String>, &[(String, String)]),
    receipts: &mut Receipts,
) -> Result<String> {
    let inside = |name: &str| name.starts_with(&format!("{namespace}/"));
    let after = all_refs(cx.canonical)?;
    let got: BTreeMap<&String, &String> = after
        .iter()
        .filter(|(_, name)| inside(name))
        .map(|(object, name)| (name, object))
        .collect();
    let missing = expected
        .iter()
        .filter(|(name, object)| got.get(name) != Some(object))
        .count();
    let extra = got
        .keys()
        .filter(|name| !expected.contains_key(**name))
        .count();
    let others = |refs: &[(String, String)]| -> BTreeSet<(String, String)> {
        refs.iter()
            .filter(|(_, name)| !inside(name))
            .cloned()
            .collect()
    };
    let moved = others(before).symmetric_difference(&others(&after)).count();
    let summary = format!(
        "kept={} namespace={} missing={missing} extra={extra} other_refs_changed={moved}",
        expected.len(),
        got.len()
    );
    let ok = missing == 0 && extra == 0;
    receipts.row(
        namespace,
        "verify-refs",
        "",
        &summary,
        if ok { "ok" } else { "check" },
    )?;
    if ok {
        Ok(summary)
    } else {
        Err(BulkloadRefusal::ClosureUnaccounted)
    }
}

fn write_lines(path: &Path, refs: &[(String, String)]) -> Result<()> {
    let text = refs.iter().fold(String::new(), |mut all, (object, name)| {
        let _ = writeln!(all, "{object} {name}");
        all
    });
    write_atomic(path, text.as_bytes())
}

// ---------------------------------------------------------------------------
// objects
// ---------------------------------------------------------------------------

/// The objects the kept clone reaches from refs, reflogs, indexes and
/// worktree HEADs that the canonical repository does not hold.
fn missing_objects(cx: &Context<'_>) -> Result<(usize, Vec<String>)> {
    let mut walk = git(cx.kept);
    without_graph(&mut walk);
    let listed = output(walk.args([
        "rev-list",
        "--objects",
        "--all",
        "--reflog",
        "--indexed-objects",
    ]))?;
    let reached: BTreeSet<&[u8]> = listed
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.split(|byte| *byte == b' ').next())
        .filter(|id| !id.is_empty())
        .collect();
    let request = reached.iter().fold(Vec::new(), |mut all, id| {
        all.extend_from_slice(id);
        all.push(b'\n');
        all
    });
    let answers = input(
        git(cx.canonical).args(["cat-file", "--batch-check=%(objectname)"]),
        &request,
    )?;
    let missing = String::from_utf8_lossy(&answers)
        .lines()
        .filter_map(|line| line.strip_suffix(" missing"))
        .map(str::to_owned)
        .collect();
    Ok((reached.len(), missing))
}

/// `worktree-fold objects`: one new pack in the canonical repository with
/// every object only the kept clone holds. Nothing is repacked or removed.
///
/// # Errors
/// The floor; a Git or IO failure; objects still missing afterwards.
pub fn fold_objects(cx: &Context<'_>) -> Result<String> {
    git_dir(cx.canonical)?;
    git_dir(cx.kept)?;
    state_dir(cx.state)?;
    let mut receipts = Receipts::open(cx, "fold-objects")?;
    let (reached, missing) = missing_objects(cx)?;
    let list = missing.iter().fold(String::new(), |mut all, id| {
        all.push_str(id);
        all.push('\n');
        all
    });
    write_atomic(&cx.state.join("objects-missing.txt"), list.as_bytes())?;
    let before = format!("reached={reached} missing={}", missing.len());
    if missing.is_empty() {
        receipts.row("objects", "carry-objects", &before, "nothing to pack", "ok")?;
        return Ok(before);
    }
    let sizes = input(
        git(cx.kept).args(["cat-file", "--batch-check=%(objectsize:disk)"]),
        list.as_bytes(),
    )?;
    let planned = String::from_utf8_lossy(&sizes)
        .lines()
        .filter_map(|line| line.parse::<u64>().ok())
        .fold(0_u64, u64::saturating_add);
    if !floor_holds(cx.canonical, planned, cx.floor)? {
        receipts.row("objects", "carry-objects", &before, "", Hold::Floor.word())?;
        return Err(BulkloadRefusal::DestinationSpaceInsufficient);
    }
    let pack = pack_into(cx, list.as_bytes())?;
    let (_, still) = missing_objects(cx)?;
    let after = format!("pack={pack} still_missing={}", still.len());
    let ok = still.is_empty();
    receipts.row(
        "objects",
        "carry-objects",
        &before,
        &after,
        if ok { "ok" } else { "check" },
    )?;
    if ok {
        Ok(format!("{before} {after}"))
    } else {
        Err(BulkloadRefusal::ClosureUnaccounted)
    }
}

/// `pack-objects --stdout` of `ids` in the kept clone, straight into
/// `index-pack --stdin` in the canonical repository. Returns the new
/// pack's name.
fn pack_into(cx: &Context<'_>, ids: &[u8]) -> Result<String> {
    let mut reader = git(cx.kept);
    without_graph(&mut reader);
    let mut pack = reader
        .args(["pack-objects", "--stdout", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .refuse_at(SITE)?;
    let stream = pack.stdout.take().ok_or(BulkloadRefusal::WorkerLost)?;
    let writer = git(cx.canonical)
        .args(["index-pack", "--stdin"])
        .stdin(Stdio::from(stream))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    // `pack-objects` reads every id before it writes, and `index-pack`
    // drains its output, so writing here cannot block on a full pipe.
    let fed = pack
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(ids));
    let packed = pack.wait().refuse_at(SITE)?;
    let indexed = writer.refuse_at(SITE)?.wait_with_output().refuse_at(SITE)?;
    if fed.is_err() || !packed.success() || !indexed.status.success() {
        return Err(BulkloadRefusal::ClosureUnaccounted);
    }
    Ok(String::from_utf8_lossy(&indexed.stdout).trim().to_owned())
}

// ---------------------------------------------------------------------------
// attach
// ---------------------------------------------------------------------------

/// What one `attach` did with a row.
enum Outcome {
    Moved(Kind),
    Already,
    Held(Hold),
}

/// `worktree-fold attach`: move every planned worktree (or only those in
/// `only`) from the kept clone to the canonical repository.
///
/// # Errors
/// `ITEMS_REFUSED` when any worktree was held; a plan that does not match;
/// a Git or IO failure.
pub fn attach(cx: &Context<'_>, only: &[String]) -> Result<String> {
    let (tag, rows) = read_plan(cx)?;
    let canonical_git = git_dir(cx.canonical)?;
    let kept_git = git_dir(cx.kept)?;
    let mut receipts = Receipts::open(cx, "attach")?;
    let dirs = Dirs {
        canonical_git: &canonical_git,
        kept_git: &kept_git,
        tag: &tag,
    };
    for dir in [dirs.canonical_park(), dirs.kept_park(), dirs.staging()] {
        fs::create_dir_all(dir).refuse_at(SITE)?;
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.planned()) {
        if !only.is_empty() && !only.contains(&row.name) {
            continue;
        }
        let word = match attach_row(cx, &dirs, row, &mut receipts)? {
            Outcome::Moved(kind) => format!("moved-{}", kind.word()),
            Outcome::Already => "already-moved".to_owned(),
            Outcome::Held(hold) => format!("held-{}", hold.word()),
        };
        *counts.entry(word).or_insert(0) += 1;
    }
    let _ = fs::remove_dir(dirs.staging());
    let mut summary = String::from("attach");
    for (word, count) in &counts {
        let _ = write!(summary, " {word}={count}");
    }
    let held = counts.keys().any(|word| word.starts_with("held-"));
    receipts.row(
        "worktrees",
        "attach-summary",
        PLAN,
        &summary,
        if held { "held" } else { "ok" },
    )?;
    if held {
        Err(BulkloadRefusal::ItemsRefused)
    } else {
        Ok(summary)
    }
}

/// The directories a fold writes beside the admin directories.
struct Dirs<'a> {
    canonical_git: &'a Path,
    kept_git: &'a Path,
    tag: &'a str,
}

impl Dirs<'_> {
    fn canonical_park(&self) -> PathBuf {
        self.canonical_git
            .join(format!("worktrees-parked-{}", self.tag))
    }

    fn kept_park(&self) -> PathBuf {
        self.kept_git.join(format!("worktrees-parked-{}", self.tag))
    }

    fn staging(&self) -> PathBuf {
        self.canonical_git
            .join(format!("fold-staging-{}", self.tag))
    }
}

/// What a move did so far, so a miss can be undone exactly.
#[derive(Default)]
struct Done {
    /// The branch this pass created, and its tip.
    created: Option<(String, String)>,
    /// A stale canonical admin directory parked: (from, to).
    parked_canonical: Option<(PathBuf, PathBuf)>,
    /// The admin directory installed in the canonical repository.
    installed: Option<PathBuf>,
    /// The worktree's `.git` before it was repointed.
    dotgit: Option<String>,
    /// The kept admin directory parked: (from, to).
    parked_kept: Option<(PathBuf, PathBuf)>,
}

fn attach_row(
    cx: &Context<'_>,
    dirs: &Dirs<'_>,
    row: &Row,
    receipts: &mut Receipts,
) -> Result<Outcome> {
    let item = format!("{} [{}->{}]", row.path, row.name, row.target);
    let worktree = Path::new(&row.path);
    let kept_admin = dirs.kept_git.join("worktrees").join(&row.name);
    if !kept_admin.exists() && dirs.kept_park().join(&row.name).exists() {
        return Ok(Outcome::Already);
    }
    if let Some(hold) = precheck(cx, row, worktree, &kept_admin)? {
        receipts.row(&item, "move", "", "not run", hold.word())?;
        return Ok(Outcome::Held(hold));
    }
    let mut done = Done::default();
    let moved = move_row(dirs, row, &kept_admin, &mut done, receipts);
    let failed = match moved {
        Ok(Ok(kind)) => match verify_row(dirs, row, kind) {
            Ok(None) => {
                receipts.row(
                    &item,
                    &format!("move-{}", kind.word()),
                    &format!(
                        "{} {} index={} status={}",
                        row.dotgit, row.head, row.index, row.status
                    ),
                    &format!("parked {}", dirs.kept_park().join(&row.name).display()),
                    "ok",
                )?;
                return Ok(Outcome::Moved(kind));
            }
            Ok(Some(missed)) => (Hold::Verify, missed),
            Err(refusal) => (Hold::Verify, refusal.to_string()),
        },
        Ok(Err(hold)) => (hold, String::new()),
        Err(refusal) => (Hold::Verify, refusal.to_string()),
    };
    let undone = undo(dirs, row, &done)?;
    receipts.row(&item, "move", &failed.1, &undone, failed.0.word())?;
    Ok(Outcome::Held(failed.0))
}

/// The checks before anything is written: quiet, the floor, and the
/// worktree as the plan recorded it.
fn precheck(
    cx: &Context<'_>,
    row: &Row,
    worktree: &Path,
    kept_admin: &Path,
) -> Result<Option<Hold>> {
    if let Some(hold) = live(&[worktree, kept_admin])? {
        return Ok(Some(hold));
    }
    let index_bytes = fs::metadata(kept_admin.join("index")).map_or(0, |meta| meta.len());
    if !floor_holds(cx.canonical, index_bytes.saturating_add(65_536), cx.floor)? {
        return Ok(Some(Hold::Floor));
    }
    let dotgit = fs::read_to_string(worktree.join(".git"))
        .map(|text| text.trim_end().to_owned())
        .unwrap_or_default();
    let head_file = fs::read_to_string(kept_admin.join("HEAD"))
        .map(|text| text.trim_end().to_owned())
        .unwrap_or_default();
    let (bytes, ok) = status(worktree)?;
    let same = ok
        && dotgit == row.dotgit
        && head_file == row.head_file
        && resolve(worktree, "HEAD").as_deref() == Some(row.head.as_str())
        && symbolic_head(worktree)? == row.branch
        && file_sha256(&kept_admin.join("index"))? == row.index
        && sha256_hex(&bytes) == row.status;
    Ok((!same).then_some(Hold::Changed))
}

/// The writes of one move, in order. `Ok(Err(hold))` is a planned refusal
/// before or during the move; the caller undoes what `done` records.
fn move_row(
    dirs: &Dirs<'_>,
    row: &Row,
    kept_admin: &Path,
    done: &mut Done,
    receipts: &mut Receipts,
) -> Result<std::result::Result<Kind, Hold>> {
    let canonical = dirs
        .canonical_git
        .parent()
        .ok_or(BulkloadRefusal::GitRepositoryNotAtPath)?;
    let worktree = Path::new(&row.path);
    let mut kind = row.kind;
    if kind == Kind::Create {
        if resolve(canonical, &row.branch).is_some() {
            kind = Kind::Detach;
        } else {
            let made = git(canonical)
                .args(["update-ref", "-m", "bulkload worktree-fold"])
                .arg(&row.branch)
                .arg(&row.head)
                .arg("")
                .output()
                .refuse_at(SITE)?;
            if !made.status.success() {
                return Ok(Err(Hold::BranchCreate));
            }
            done.created = Some((row.branch.clone(), row.head.clone()));
            receipts.row(
                &row.path,
                "create-branch",
                "",
                &format!("{} {}", row.branch, row.head),
                "ok",
            )?;
        }
    } else if kind == Kind::Attach {
        let elsewhere = listing(canonical)?.iter().any(|entry| {
            entry.branch.as_deref() == Some(row.branch.as_str())
                && !same_place(&entry.path, worktree)
        });
        if elsewhere || resolve(canonical, &row.branch).as_deref() != Some(row.head.as_str()) {
            kind = Kind::Detach;
        }
    }
    let target = dirs.canonical_git.join("worktrees").join(&row.target);
    if row.park {
        let gitdir = fs::read_to_string(target.join("gitdir")).unwrap_or_default();
        let parked = dirs.canonical_park().join(&row.target);
        if !same_place(Path::new(gitdir.trim_end()), &worktree.join(".git")) || parked.exists() {
            return Ok(Err(Hold::Changed));
        }
        fs::rename(&target, &parked).refuse_at(SITE)?;
        done.parked_canonical = Some((target.clone(), parked));
    }
    if target.exists() {
        return Ok(Err(Hold::AdminTaken));
    }
    let staged = dirs.staging().join(&row.target);
    if staged.exists() {
        return Ok(Err(Hold::AdminTaken));
    }
    copy_tree(kept_admin, &staged)?;
    if kind == Kind::Detach {
        write_atomic(&staged.join("HEAD"), format!("{}\n", row.head).as_bytes())?;
    }
    if file_sha256(&staged.join("index"))? != row.index {
        let aside = dirs.staging().join(format!("{}.copy-mismatch", row.target));
        fs::rename(staged.as_path(), aside).refuse_at(SITE)?;
        return Ok(Err(Hold::CopyMismatch));
    }
    // A canonical repository with no linked worktree yet has no
    // `.git/worktrees`; `git worktree add` makes it the same way.
    fs::create_dir_all(dirs.canonical_git.join("worktrees")).refuse_at(SITE)?;
    fs::rename(staged.as_path(), target.as_path()).refuse_at(SITE)?;
    sync_dir(&dirs.canonical_git.join("worktrees"))?;
    done.installed = Some(target.clone());
    let before = fs::read_to_string(worktree.join(".git")).refuse_at(SITE)?;
    write_atomic(&dirs_state_dotgit(dirs, row), before.as_bytes())?;
    done.dotgit = Some(before);
    write_atomic(
        &worktree.join(".git"),
        format!("gitdir: {}\n", target.display()).as_bytes(),
    )?;
    let parked = dirs.kept_park().join(&row.name);
    fs::rename(kept_admin, &parked).refuse_at(SITE)?;
    done.parked_kept = Some((kept_admin.to_path_buf(), parked));
    Ok(Ok(kind))
}

/// Where the worktree's old `.git` is kept: beside the parked kept admin
/// directory, so the record travels with it.
fn dirs_state_dotgit(dirs: &Dirs<'_>, row: &Row) -> PathBuf {
    dirs.kept_park().join(format!("{}.dotgit", row.name))
}

/// Check a moved worktree against its plan. `None` when it verifies, or
/// what missed.
fn verify_row(dirs: &Dirs<'_>, row: &Row, kind: Kind) -> Result<Option<String>> {
    let canonical = dirs
        .canonical_git
        .parent()
        .ok_or(BulkloadRefusal::GitRepositoryNotAtPath)?;
    let kept = dirs
        .kept_git
        .parent()
        .ok_or(BulkloadRefusal::GitRepositoryNotAtPath)?;
    let worktree = Path::new(&row.path);
    let admin = dirs.canonical_git.join("worktrees").join(&row.target);
    let want_branch = if matches!(kind, Kind::Create | Kind::Attach) {
        row.branch.as_str()
    } else {
        ""
    };
    let gitdir = text(git(worktree).args(["rev-parse", "--absolute-git-dir"])).unwrap_or_default();
    let (bytes, ok) = status(worktree)?;
    let listed = listing(canonical)?;
    let mine: Vec<&Listed> = listed
        .iter()
        .filter(|entry| same_place(&entry.path, worktree))
        .collect();
    let kept_lists = listing(kept)?
        .iter()
        .any(|entry| same_place(&entry.path, worktree));
    let checks = [
        (
            "head",
            resolve(worktree, "HEAD").as_deref() == Some(row.head.as_str()),
        ),
        ("on-branch", symbolic_head(worktree)? == want_branch),
        ("gitdir", same_place(Path::new(&gitdir), &admin)),
        ("index", file_sha256(&admin.join("index"))? == row.index),
        ("status-z", ok && sha256_hex(&bytes) == row.status),
        ("listed-once", mine.len() == 1),
        ("not-prunable", mine.iter().all(|entry| !entry.prunable)),
        ("kept-unlisted", !kept_lists),
    ];
    let missed: Vec<&str> = checks
        .iter()
        .filter(|(_, passed)| !passed)
        .map(|(name, _)| *name)
        .collect();
    Ok((!missed.is_empty()).then(|| format!("missed {}", missed.join(","))))
}

/// Undo a move by renames, in reverse. A branch this pass created is
/// deleted at the tip it was created with; nothing else is deleted.
fn undo(dirs: &Dirs<'_>, row: &Row, done: &Done) -> Result<String> {
    let mut steps = Vec::new();
    if let Some((from, to)) = &done.parked_kept {
        fs::rename(to, from).refuse_at(SITE)?;
        steps.push("kept admin back");
    }
    if let Some(before) = &done.dotgit {
        write_atomic(&Path::new(&row.path).join(".git"), before.as_bytes())?;
        steps.push(".git restored");
    }
    if let Some(installed) = &done.installed {
        let aside = dirs
            .canonical_park()
            .join(format!("rollback-{}", row.target));
        fs::rename(installed, aside).refuse_at(SITE)?;
        steps.push("canonical copy parked as rollback");
    }
    if let Some((from, to)) = &done.parked_canonical {
        fs::rename(to, from).refuse_at(SITE)?;
        steps.push("canonical admin back");
    }
    if let Some((branch, tip)) = &done.created {
        let canonical = dirs
            .canonical_git
            .parent()
            .ok_or(BulkloadRefusal::GitRepositoryNotAtPath)?;
        output(
            git(canonical)
                .args(["update-ref", "-m", "bulkload worktree-fold rollback", "-d"])
                .arg(branch)
                .arg(tip),
        )?;
        steps.push("created branch deleted");
    }
    Ok(if steps.is_empty() {
        "nothing written".to_owned()
    } else {
        steps.join("; ")
    })
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// `worktree-fold verify`: every planned worktree against the plan, and
/// `fsck --connectivity-only` of the canonical repository.
///
/// # Errors
/// `ITEMS_REFUSED` when a moved worktree does not match its plan or fsck
/// fails; a plan that does not match; a Git or IO failure.
pub fn verify(cx: &Context<'_>) -> Result<String> {
    let (tag, rows) = read_plan(cx)?;
    let canonical_git = git_dir(cx.canonical)?;
    let kept_git = git_dir(cx.kept)?;
    let mut receipts = Receipts::open(cx, "verify")?;
    let dirs = Dirs {
        canonical_git: &canonical_git,
        kept_git: &kept_git,
        tag: &tag,
    };
    let (mut good, mut bad, mut unmoved) = (0_usize, 0_usize, 0_usize);
    for row in rows.iter().filter(|row| row.planned()) {
        let admin = canonical_git.join("worktrees").join(&row.target);
        let dotgit = fs::read_to_string(Path::new(&row.path).join(".git")).unwrap_or_default();
        let pointer = dotgit
            .trim_end()
            .strip_prefix("gitdir: ")
            .unwrap_or_default();
        if !same_place(Path::new(pointer), &admin) {
            unmoved += 1;
            continue;
        }
        let kind = if symbolic_head(Path::new(&row.path))?.is_empty() {
            Kind::Detach
        } else {
            Kind::Attach
        };
        match verify_row(&dirs, row, kind)? {
            None => good += 1,
            Some(missed) => {
                bad += 1;
                receipts.row(&row.path, "verify-worktree", "", &missed, "check")?;
            }
        }
    }
    let fsck = git(cx.canonical)
        .args(["fsck", "--connectivity-only"])
        .output()
        .refuse_at(SITE)?;
    write_atomic(&cx.state.join("fsck-connectivity.txt"), &fsck.stdout)?;
    let fsck_rc = fsck.status.code().unwrap_or(-1);
    let summary = format!("verified={good} mismatched={bad} not_moved={unmoved} fsck_rc={fsck_rc}");
    let ok = bad == 0 && fsck.status.success();
    receipts.row(
        "worktrees",
        "verify",
        PLAN,
        &summary,
        if ok { "ok" } else { "check" },
    )?;
    if ok {
        Ok(summary)
    } else {
        Err(BulkloadRefusal::ItemsRefused)
    }
}

// ---------------------------------------------------------------------------
// reattach
// ---------------------------------------------------------------------------

/// `worktree-reattach`: what it is given.
#[derive(Debug, Clone, Copy)]
pub struct Reattach<'a> {
    pub canonical: &'a Path,
    /// The worktree whose `.git` names a missing admin directory.
    pub worktree: &'a Path,
    /// The admin name it takes in the canonical repository.
    pub name: &'a str,
    /// HEAD: `ref: refs/heads/NAME` or a commit.
    pub head: &'a str,
    pub state: &'a Path,
    pub floor: Floor,
    pub ruling: &'a str,
}

/// `worktree-reattach`: a new admin directory in the canonical repository
/// for a worktree whose own is gone, its index read from HEAD. The
/// worktree's files are not touched; only its `.git` is repointed.
///
/// # Errors
/// `ITEMS_REFUSED` when the worktree is held (live, its admin directory
/// still present, the name taken, HEAD unresolved, the floor); a Git or IO
/// failure.
pub fn reattach(request: &Reattach<'_>) -> Result<String> {
    let canonical_git = git_dir(request.canonical)?;
    let cx = Context {
        canonical: request.canonical,
        kept: request.canonical,
        state: request.state,
        floor: request.floor,
        ruling: request.ruling,
    };
    state_dir(request.state)?;
    let mut receipts = Receipts::open(&cx, "reattach")?;
    let (Some(_), true) = (portable(request.worktree), valid_name(request.name)) else {
        return Err(BulkloadRefusal::PathNotPortable);
    };
    if request.head.contains(['\t', '\n', '\r', '\0']) {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let item = format!("{} [{}]", request.worktree.display(), request.name);
    let dotgit = fs::read_to_string(request.worktree.join(".git")).refuse_at(SITE)?;
    let old = dotgit
        .trim_end()
        .strip_prefix("gitdir: ")
        .unwrap_or_default();
    let admin = canonical_git.join("worktrees").join(request.name);
    let held = |hold: Hold, receipts: &mut Receipts| -> Result<String> {
        receipts.row(&item, "reattach", dotgit.trim_end(), "not run", hold.word())?;
        Err(BulkloadRefusal::ItemsRefused)
    };
    if admin.exists() || old.is_empty() || Path::new(old).exists() {
        return held(Hold::AdminTaken, &mut receipts);
    }
    let branch = request.head.strip_prefix("ref: ");
    let Some(tip) = resolve(request.canonical, branch.unwrap_or(request.head)) else {
        return held(Hold::HeadUnreadable, &mut receipts);
    };
    if let Some(hold) = live(&[request.worktree])? {
        return held(hold, &mut receipts);
    }
    if !floor_holds(request.canonical, 1 << 20, request.floor)? {
        return held(Hold::Floor, &mut receipts);
    }
    write_atomic(
        &request
            .state
            .join("before")
            .join(format!("{}.dotgit", request.name)),
        dotgit.as_bytes(),
    )?;
    let staged = canonical_git.join(format!("fold-staging-reattach-{}", request.name));
    fs::create_dir_all(&staged).refuse_at(SITE)?;
    let gitdir = format!("{}\n", request.worktree.join(".git").display());
    write_atomic(&staged.join("gitdir"), gitdir.as_bytes())?;
    write_atomic(&staged.join("commondir"), b"../..\n")?;
    write_atomic(
        &staged.join("HEAD"),
        format!("{}\n", request.head).as_bytes(),
    )?;
    fs::create_dir_all(canonical_git.join("worktrees")).refuse_at(SITE)?;
    fs::rename(staged.as_path(), admin.as_path()).refuse_at(SITE)?;
    write_atomic(
        &request.worktree.join(".git"),
        format!("gitdir: {}\n", admin.display()).as_bytes(),
    )?;
    output(git(request.worktree).args(["read-tree", "HEAD"]))?;
    let landed = resolve(request.worktree, "HEAD").unwrap_or_default();
    let (bytes, ok) = status(request.worktree)?;
    let listed = listing(request.canonical)?;
    let mine: Vec<&Listed> = listed
        .iter()
        .filter(|entry| same_place(&entry.path, request.worktree))
        .collect();
    let verified =
        landed == tip && ok && mine.len() == 1 && mine.iter().all(|entry| !entry.prunable);
    let after = format!(
        "gitdir: {} HEAD={landed} {} status_entries={} listed={} prunable={}",
        admin.display(),
        branch.unwrap_or("detached"),
        nul_count(&bytes),
        mine.len(),
        mine.iter().filter(|entry| entry.prunable).count()
    );
    receipts.row(
        &item,
        "reattach",
        dotgit.trim_end(),
        &after,
        if verified { "ok" } else { "check" },
    )?;
    if verified {
        Ok(after)
    } else {
        Err(BulkloadRefusal::ItemsRefused)
    }
}

/// An admin directory name: `[A-Za-z0-9._-]+`, not `.` or `..`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` through a synced sibling temporary and a rename,
/// then sync the parent.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let name = path.file_name().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let mut temporary = name.to_os_string();
    temporary.push(".fold-tmp");
    let temporary = parent.join(temporary);
    let mut file = fs::File::create(&temporary).refuse_at(SITE)?;
    file.write_all(bytes).refuse_at(SITE)?;
    file.sync_all().refuse_at(SITE)?;
    fs::rename(temporary.as_path(), path).refuse_at(SITE)?;
    sync_dir(parent)
}

fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)
        .refuse_at(SITE)?
        .sync_all()
        .refuse_at(SITE)
}

/// Copy an admin directory: directories, regular files (mode and
/// modification time kept, each synced) and links. Anything else refuses.
/// Each file is read and written, never `fs::copy`: on a reflink file
/// system that clones its input, and the kept clone's files are a source
/// (P84, OI-1003-Q165).
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let mode = fs::symlink_metadata(from)
        .refuse_at(SITE)?
        .permissions()
        .mode();
    fs::DirBuilder::new()
        .mode(mode & 0o7777)
        .create(to)
        .refuse_at(SITE)?;
    for entry in fs::read_dir(from).refuse_at(SITE)? {
        let entry = entry.refuse_at(SITE)?;
        let kind = entry.file_type().refuse_at(SITE)?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let (source, target) = (source.as_path(), target.as_path());
        if kind.is_dir() {
            copy_tree(source, target)?;
        } else if kind.is_symlink() {
            let link = fs::read_link(source).refuse_at(SITE)?;
            std::os::unix::fs::symlink(link, target).refuse_at(SITE)?;
        } else if kind.is_file() {
            let meta = fs::symlink_metadata(source).refuse_at(SITE)?;
            let bytes = fs::read(source).refuse_at(SITE)?;
            let mut copy = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(meta.permissions().mode() & 0o7777)
                .open(target)
                .refuse_at(SITE)?;
            copy.write_all(&bytes).refuse_at(SITE)?;
            copy.set_times(fs::FileTimes::new().set_modified(meta.modified().refuse_at(SITE)?))
                .refuse_at(SITE)?;
            copy.sync_all().refuse_at(SITE)?;
        } else {
            return Err(BulkloadRefusal::SourceRootKindMismatch);
        }
    }
    sync_dir(to)
}

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4), for the census records KC-1 kept in SHA-256
// ---------------------------------------------------------------------------

const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// SHA-256 of `bytes`, in lower-case hex.
fn sha256_hex(bytes: &[u8]) -> String {
    let length = u64::try_from(bytes.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&length.to_be_bytes());
    let mut state = INITIAL;
    for block in message.chunks_exact(64) {
        let mut schedule: Vec<u32> = block
            .chunks_exact(4)
            .map(|word| <[u8; 4]>::try_from(word).map_or(0, u32::from_be_bytes))
            .collect();
        for at in 16..64 {
            let word = |back: usize| schedule.get(at - back).copied().unwrap_or(0);
            let (near, far) = (word(2), word(15));
            let far_mix = far.rotate_right(7) ^ far.rotate_right(18) ^ (far >> 3);
            let near_mix = near.rotate_right(17) ^ near.rotate_right(19) ^ (near >> 10);
            let next = word(16)
                .wrapping_add(far_mix)
                .wrapping_add(word(7))
                .wrapping_add(near_mix);
            schedule.push(next);
        }
        let mut work = state;
        for (round, word) in ROUND.iter().zip(&schedule) {
            let [alpha, bravo, charlie, delta, echo, foxtrot, golf, hotel] = work;
            let sigma_choice = echo.rotate_right(6) ^ echo.rotate_right(11) ^ echo.rotate_right(25);
            let choose = (echo & foxtrot) ^ (!echo & golf);
            let step = hotel
                .wrapping_add(sigma_choice)
                .wrapping_add(choose)
                .wrapping_add(*round)
                .wrapping_add(*word);
            let sigma_major =
                alpha.rotate_right(2) ^ alpha.rotate_right(13) ^ alpha.rotate_right(22);
            let majority = (alpha & bravo) ^ (alpha & charlie) ^ (bravo & charlie);
            let carry = sigma_major.wrapping_add(majority);
            work = [
                step.wrapping_add(carry),
                alpha,
                bravo,
                charlie,
                delta.wrapping_add(step),
                echo,
                foxtrot,
                golf,
            ];
        }
        for (slot, add) in state.iter_mut().zip(work) {
            *slot = slot.wrapping_add(add);
        }
    }
    state
        .iter()
        .fold(String::with_capacity(64), |mut hex, word| {
            let _ = write!(hex, "{word:08x}");
            hex
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_fips_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 1000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn a_plan_row_round_trips_and_an_odd_one_refuses() {
        let row = Row {
            name: "wt".to_owned(),
            path: "/x/wt".to_owned(),
            target: "wt-kc2-1".to_owned(),
            park: true,
            dotgit: "gitdir: /k/.git/worktrees/wt".to_owned(),
            head_file: "ref: refs/heads/b".to_owned(),
            head: "a".repeat(40),
            branch: "refs/heads/b".to_owned(),
            index: "none".to_owned(),
            status: sha256_hex(b""),
            entries: 0,
            kind: Kind::Detach,
            hold: Some(Hold::RecentWrite),
        };
        assert_eq!(Row::parse(&row.line()), Ok(row.clone()));
        assert!(row.planned());
        let held = Row::held("odd", "/x/odd", Hold::NotBound);
        assert_eq!(Row::parse(&held.line()), Ok(held.clone()));
        assert!(!held.planned());
        assert_eq!(Row::parse("a\tb"), Err(BulkloadRefusal::SchemaMismatch));
    }

    #[test]
    fn only_a_ruled_namespace_and_a_plain_name_pass() {
        assert!(ruled_namespace("refs/kept-sting-20261026"));
        assert!(!ruled_namespace("refs/heads"));
        assert!(!ruled_namespace("refs/kept-"));
        assert!(!ruled_namespace("refs/kept-Sting/x"));
        assert!(valid_name("wt-kc1-1"));
        assert!(!valid_name(".."));
        assert!(!valid_name("a/b"));
    }
}
