//! The union of a source repository's history into a live destination
//! repository that already holds a related one (SPEC rule 2 reset; B2).
//!
//! One repository holds both sides' refs, and nothing picks a winner:
//!
//! - a ref only the source has is created as it is;
//! - a ref one side holds an ancestor of the other's is fast-forwarded to
//!   the descendant, except a branch checked out in a worktree with a dirty
//!   tree; a clean checked-out branch is advanced with its checkout
//!   ("switch when clean", the operator's ruling);
//! - a ref the two sides hold diverged keeps the destination's tip, and the
//!   source's goes in as a remote-tracking ref, `refs/remotes/SOURCE/NAME`
//!   (ordinary Git, no duplicate bytes), listed for the operator;
//! - the source's stash entries join the destination's stash, and its
//!   reflog entries the destination's reflogs, so every source id stays
//!   reachable.
//!
//! The objects are already in the live store ([`super::stream`]). Every
//! write is a destination write ([`super::durable`]): one `update-ref`
//! transaction per ref, compare-and-swap on the value it read.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::{durable, git, input, oid, output, text};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "git_carry::union";

/// What [`refs`] did to one ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefAction {
    /// The destination had no such ref: created at the source's value.
    Created,
    /// The values were equal.
    Same,
    /// The destination's was an ancestor of the source's: moved to it.
    Forwarded,
    /// As `Forwarded`, a clean checkout of it advanced with it.
    Switched(PathBuf),
    /// The source's was an ancestor of the destination's: kept.
    Kept,
    /// Diverged, or checked out dirty: the destination's kept, the
    /// source's at this remote-tracking ref.
    Tracked(String),
}

impl RefAction {
    /// Its report word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Same => "same",
            Self::Forwarded => "forwarded",
            Self::Switched(_) => "switched",
            Self::Kept => "kept",
            Self::Tracked(_) => "tracked",
        }
    }
}

/// The remote-tracking name the source's `name` takes when it cannot be
/// the destination's: `refs/heads/X` as `refs/remotes/SOURCE/X`, any other
/// `refs/Y` as `refs/remotes/SOURCE/Y`.
#[must_use]
pub fn tracking_name(name: &str, source: &str) -> String {
    let rest = name
        .strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/"))
        .unwrap_or(name);
    format!("refs/remotes/{source}/{rest}")
}

fn values(repo: &Path) -> Result<BTreeMap<String, String>> {
    let listed = text(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
    let mut refs = BTreeMap::new();
    for line in listed.lines().filter(|line| !line.is_empty()) {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        refs.insert(name.to_owned(), value.to_owned());
    }
    Ok(refs)
}

fn is_ancestor(repo: &Path, ancestor: &str, of: &str) -> Result<bool> {
    let ran = git(repo)
        .args(["merge-base", "--is-ancestor", ancestor, of])
        .output()
        .refuse_at(SITE)?;
    match ran.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(super::estimate::child_failed(&ran.stderr)),
    }
}

// The worktrees of `repo` with the branch each has checked out.
fn checkouts(repo: &Path) -> Result<Vec<(PathBuf, String)>> {
    use std::os::unix::ffi::OsStrExt as _;
    let listed = output(git(repo).args(["worktree", "list", "--porcelain", "-z"]))?;
    let mut found = Vec::new();
    let mut path: Option<PathBuf> = None;
    for field in listed.split(|byte| *byte == 0) {
        if let Some(at) = field.strip_prefix(b"worktree ") {
            path = Some(PathBuf::from(std::ffi::OsStr::from_bytes(at)));
        } else if let Some(branch) = field.strip_prefix(b"branch ") {
            if let (Some(at), Ok(branch)) = (path.clone(), std::str::from_utf8(branch)) {
                found.push((at, branch.to_owned()));
            }
        }
    }
    Ok(found)
}

fn clean(worktree: &Path) -> Result<bool> {
    let status = output(git(worktree).args([
        "status",
        "--porcelain",
        "--untracked-files=no",
        "--ignore-submodules=all",
    ]))?;
    Ok(status.is_empty())
}

fn update(repo: &Path, name: &str, new: &str, old: Option<&str>) -> Result<()> {
    let mut command = git(repo);
    durable(&mut command);
    let old = old.unwrap_or("");
    let transaction = if old.is_empty() {
        format!("create {name}\0{new}\0")
    } else {
        format!("update {name}\0{new}\0{old}\0")
    };
    input(
        command.args([
            "update-ref",
            "--no-deref",
            "--stdin",
            "-z",
            "-m",
            "bulkload: union",
        ]),
        transaction.as_bytes(),
    )
    .map(drop)
}

/// Union `source_refs` into the live repository `repo`.
///
/// The refs are `(name, value)`, every object already in the store; the source's
/// label is `source` (its remote-tracking namespace). `refs/stash` is the
/// stash's ([`stash`]), never a ref here.
///
/// # Errors
/// A Git child's refusal; a compare-and-swap that lost to a live writer
/// refuses as the child failed (the next pass unions again).
pub fn refs(
    repo: &Path,
    source_refs: &[(String, String)],
    source: &str,
) -> Result<Vec<(String, RefAction)>> {
    let held = values(repo)?;
    let checked_out = checkouts(repo)?;
    let mut done = Vec::new();
    for (name, value) in source_refs {
        if name == "refs/stash" || name.starts_with("refs/remotes/") {
            continue;
        }
        let action = match held.get(name) {
            None => {
                update(repo, name, value, None)?;
                RefAction::Created
            }
            Some(ours) if ours == value => RefAction::Same,
            Some(ours) if is_ancestor(repo, ours, value)? => {
                match checked_out.iter().find(|(_, branch)| branch == name) {
                    None => {
                        update(repo, name, value, Some(ours))?;
                        RefAction::Forwarded
                    }
                    Some((worktree, _)) if clean(worktree)? => {
                        let mut command = git(worktree);
                        durable(&mut command);
                        output(command.args(["merge", "--ff-only", "--quiet", value]))?;
                        RefAction::Switched(worktree.clone())
                    }
                    Some(_) => {
                        let tracking = tracking_name(name, source);
                        track(repo, &held, &tracking, value)?;
                        RefAction::Tracked(tracking)
                    }
                }
            }
            Some(ours) if is_ancestor(repo, value, ours)? => RefAction::Kept,
            Some(_) => {
                let tracking = tracking_name(name, source);
                track(repo, &held, &tracking, value)?;
                RefAction::Tracked(tracking)
            }
        };
        done.push((name.clone(), action));
    }
    Ok(done)
}

// Point the remote-tracking ref at the source's value (ours to move).
fn track(repo: &Path, held: &BTreeMap<String, String>, name: &str, value: &str) -> Result<()> {
    match held.get(name) {
        Some(at) if at == value => Ok(()),
        Some(at) => update(repo, name, value, Some(at)),
        None => update(repo, name, value, None),
    }
}

/// Add the source's stash entries (newest first) the destination's stash
/// does not hold, oldest first, so the newest ends on top: `git stash
/// store`. Returns how many were added.
///
/// # Errors
/// A Git child's refusal.
pub fn stash(repo: &Path, entries: &[(String, String)]) -> Result<u64> {
    let held: Vec<String> = match git(repo)
        .args(["reflog", "show", "--format=%H", "refs/stash"])
        .output()
        .refuse_at(SITE)?
    {
        listed if listed.status.success() => String::from_utf8_lossy(&listed.stdout)
            .lines()
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let mut added = 0;
    for (commit, message) in entries.iter().rev() {
        if !oid(commit) || held.contains(commit) {
            continue;
        }
        let mut command = git(repo);
        durable(&mut command);
        output(command.args(["stash", "store", "-q", "-m", message, commit]))?;
        added += 1;
    }
    Ok(added)
}

/// Append the source's reflog entries the destination's reflogs lack.
///
/// Ref by ref, into the destination's reflog of that ref (or of its remote-tracking name, when the ref was
/// tracked) lacks, so every id they name stays reachable. Written whole and
/// synced. Returns how many entries were added.
///
/// # Errors
/// IO.
pub fn reflogs(
    repo: &Path,
    source_logs: &[(String, Vec<u8>)],
    actions: &[(String, RefAction)],
) -> Result<u64> {
    let common = super::common_repository(repo)?;
    let mut ids: Vec<String> = source_logs
        .iter()
        .flat_map(|(_, lines)| lines.split(|byte| *byte == b'\n'))
        .flat_map(entry_ids)
        .collect();
    ids.sort();
    ids.dedup();
    let present = super::preview::held(repo, &ids)?;
    let mut added = 0u64;
    for (name, lines) in source_logs {
        if name == "refs/stash" || name == "HEAD" || name.contains("..") {
            continue;
        }
        let at = match actions.iter().find(|(ref_name, _)| ref_name == name) {
            Some((_, RefAction::Tracked(tracking))) => tracking.clone(),
            _ => name.clone(),
        };
        let path = common.join("logs").join(&at);
        let held = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(crate::refuse::io(&error, SITE)),
        };
        let held_lines: std::collections::HashSet<&[u8]> =
            held.split(|byte| *byte == b'\n').collect();
        let missing: Vec<&[u8]> = lines
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty() && !held_lines.contains(line))
            // An entry naming an object the source no longer holds (its gc
            // pruned it; the fetch could not bring it) is history neither
            // side has: never written, so the live repository never names a
            // missing object (2026-10-22, tinyland.dev's CAPTURE_DRIFTED).
            .filter(|line| entry_ids(line).iter().all(|id| present.contains(id)))
            .collect();
        if missing.is_empty() {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).refuse_at(SITE)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .refuse_at(SITE)?;
        for line in &missing {
            file.write_all(line).refuse_at(SITE)?;
            file.write_all(b"\n").refuse_at(SITE)?;
        }
        file.sync_all().refuse_at(SITE)?;
        added += u64::try_from(missing.len()).unwrap_or(u64::MAX);
    }
    Ok(added)
}

// The object ids a reflog line names (its old and new values), the
// null id left out.
fn entry_ids(line: &[u8]) -> Vec<String> {
    line.split(|byte| *byte == b' ')
        .take(2)
        .filter_map(|word| std::str::from_utf8(word).ok())
        .filter(|word| oid(word) && word.bytes().any(|byte| byte != b'0'))
        .map(str::to_owned)
        .collect()
}

/// Whether the live repository `repo` holds any of `roots` (the source's
/// root commits): a related repository, which a union may join, unlike an
/// unrelated one at the same path.
///
/// # Errors
/// A Git child's refusal.
pub fn related(repo: &Path, roots: &[String]) -> Result<bool> {
    if roots.is_empty() {
        return Ok(false);
    }
    let request = roots.iter().fold(String::new(), |mut all, root| {
        all.push_str(root);
        all.push('\n');
        all
    });
    let answered = input(
        git(repo).args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
        request.as_bytes(),
    )?;
    Ok(String::from_utf8_lossy(&answered)
        .lines()
        .any(|line| line.ends_with(" commit")))
}

/// The digest of an index's entries (mode, id, stage, path).
///
/// Its stat data and extensions are left out: what "the index changed" means to a later pass
/// (SPEC rule 8: a destination `git status` refreshing the stat data is no
/// change). `index` names an index file; `worktree` any worktree of its
/// repository.
///
/// # Errors
/// A Git child's refusal.
pub fn index_entries(worktree: &Path, index: &Path) -> Result<[u8; 32]> {
    let mut command = git(worktree);
    super::with_index(&mut command, index);
    let listed = output(command.args(["ls-files", "-s", "-z"]))?;
    Ok(*blake3::hash(&listed).as_bytes())
}

/// `repo`'s checked-out branch (`refs/heads/NAME`), or `None` for a
/// detached HEAD; and HEAD's commit (empty when unborn).
///
/// # Errors
/// A Git child's refusal.
pub fn head(repo: &Path) -> Result<(Option<String>, String)> {
    let symbolic = git(repo)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .refuse_at(SITE)?;
    let branch = symbolic
        .status
        .success()
        .then(|| String::from_utf8_lossy(&symbolic.stdout).trim().to_owned());
    let commit = git(repo)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .refuse_at(SITE)?;
    Ok((
        branch,
        String::from_utf8_lossy(&commit.stdout).trim().to_owned(),
    ))
}

/// The value of the ref `name` in `repo`, if it exists.
///
/// # Errors
/// A Git child's refusal.
pub fn value(repo: &Path, name: &str) -> Result<Option<String>> {
    Ok(values(repo)?.remove(name))
}

/// Whether `worktree`'s tracked files are clean (untracked files aside).
///
/// # Errors
/// A Git child's refusal.
pub fn is_clean(worktree: &Path) -> Result<bool> {
    clean(worktree)
}

/// Switch the clean `worktree` to `branch` (`refs/heads/NAME`): its HEAD,
/// index and files ("switch when clean", the operator's ruling).
///
/// # Errors
/// A Git child's refusal.
pub fn switch(worktree: &Path, branch: &str) -> Result<()> {
    let name = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let mut command = git(worktree);
    durable(&mut command);
    output(command.args(["checkout", "--quiet", name])).map(drop)
}

/// The blob at `commit:rel` in `repo`, if the commit's tree holds a file
/// there.
///
/// # Errors
/// A Git child's refusal other than "no such path".
pub fn blob_at(repo: &Path, commit: &str, rel: &[u8]) -> Result<Option<Vec<u8>>> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut spec = format!("{commit}:").into_bytes();
    spec.extend_from_slice(rel);
    let ran = git(repo)
        .args(["cat-file", "blob"])
        .arg(std::ffi::OsString::from_vec(spec))
        .output()
        .refuse_at(SITE)?;
    if ran.status.success() {
        Ok(Some(ran.stdout))
    } else if ran.status.code() == Some(128) {
        Ok(None)
    } else {
        Err(super::estimate::child_failed(&ran.stderr))
    }
}

/// Git's three-way merge of `ours` and `theirs` against `base`
/// (`merge-file -p`), its inputs written apart under `scratch` and removed:
/// the merged bytes, or `None` when a hunk conflicts.
///
/// # Errors
/// IO; a Git child's failure.
pub fn merge_file(
    scratch: &Path,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
) -> Result<Option<Vec<u8>>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fs::create_dir_all(scratch).refuse_at(SITE)?;
    let at = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let paths: Vec<PathBuf> = ["ours", "base", "theirs"]
        .iter()
        .map(|side| scratch.join(format!("merge-{}-{at}.{side}", std::process::id())))
        .collect();
    let cleanup = |paths: &[PathBuf]| {
        for path in paths {
            let _ = fs::remove_file(path);
        }
    };
    for (path, bytes) in paths.iter().zip([ours, base, theirs]) {
        if let Err(error) = fs::write(path, bytes) {
            cleanup(&paths);
            return Err(crate::refuse::io(&error, SITE));
        }
    }
    let ran = git(scratch)
        .args(["merge-file", "-p", "-q"])
        .args(&paths)
        .output();
    cleanup(&paths);
    let ran = ran.refuse_at(SITE)?;
    match ran.status.code() {
        Some(0) => Ok(Some(ran.stdout)),
        Some(code) if (1..=127).contains(&code) => Ok(None),
        _ => Err(super::estimate::child_failed(&ran.stderr)),
    }
}

/// The merge base of `ours` and `theirs` in `repo`, if they share history.
///
/// # Errors
/// A Git child's refusal.
pub fn merge_base(repo: &Path, ours: &str, theirs: &str) -> Result<Option<String>> {
    let ran = git(repo)
        .args(["merge-base", ours, theirs])
        .output()
        .refuse_at(SITE)?;
    match ran.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&ran.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(super::estimate::child_failed(&ran.stderr)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_that_cannot_be_the_destinations_is_tracked_under_the_source() {
        assert_eq!(
            tracking_name("refs/heads/main", "neo"),
            "refs/remotes/neo/main"
        );
        assert_eq!(
            tracking_name("refs/heads/feat/x", "neo"),
            "refs/remotes/neo/feat/x"
        );
        assert_eq!(
            tracking_name("refs/tags/v1", "neo"),
            "refs/remotes/neo/tags/v1"
        );
        assert_eq!(
            tracking_name("refs/notes/commits", "neo"),
            "refs/remotes/neo/notes/commits"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod merge_tests {
    use super::*;

    #[test]
    fn a_clean_three_way_merge_and_a_conflict() {
        let scratch = std::env::temp_dir().join(format!("bulkload-merge3-{}", std::process::id()));
        let base = b"one\ntwo\nthree\nfour\nfive\n";
        let ours = b"ONE\ntwo\nthree\nfour\nfive\n";
        let theirs = b"one\ntwo\nthree\nfour\nFIVE\n";
        assert_eq!(
            merge_file(&scratch, base, ours, theirs).unwrap().unwrap(),
            b"ONE\ntwo\nthree\nfour\nFIVE\n".to_vec()
        );
        let clash = b"uno\ntwo\nthree\nfour\nfive\n";
        assert_eq!(merge_file(&scratch, base, ours, clash).unwrap(), None);
        // The inputs written apart are gone.
        assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&scratch);
    }
}
