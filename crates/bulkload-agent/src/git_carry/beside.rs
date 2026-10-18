//! `estate-apply --adopt --beside SUFFIX` (M2 round 2, item c, 2026-10-17).
//!
//! The in-place run's i02 batch ended 10 workspace items refused
//! `GIT_DESTINATION_DIVERGED`: the live checkout exists and differs from the
//! capture, so `--adopt` cannot prove it. Each was carried by hand as a
//! refs-only item into the live repository and a second, standalone item at
//! `<path>.neo-20261016`. `--beside` does it natively, and leaves the live
//! checkout's working tree, index, HEAD and native refs as they were:
//!
//! - **When.** Only under `--adopt`, and only for a workspace item whose
//!   planned checkout existed and whose adopt proof refused
//!   `GIT_DESTINATION_DIVERGED` (nothing written). No other refusal leads
//!   here: a first landing that refused after its writes leaves its partial
//!   checkout for review, never a second copy beside it.
//! - **Where.** The workspace path with `SUFFIX` appended verbatim (the
//!   same meaning as `estate-carry-files --beside SUFFIX`): `.neo-20261016`
//!   gives `<path>.neo-20261016`. Its ref namespace is `SUFFIX` without
//!   its leading `.`, which must be one plain ref component (letters,
//!   digits, `.`, `_`, `-`; not starting with `.` or `-`, no `..`, not
//!   ending in `.` or `.lock`), else `FIELD_DOMAIN_VIOLATION`. Labels that
//!   use the same SUFFIX share the namespace: their refs are kept, never
//!   moved, and a differing one keeps the item refused (below).
//! - **What lands there.** A first linked landing of the capture into the
//!   planned (live) repository, detached at the captured head with its
//!   branch recorded (`head-detached-beside`): no branch of the live
//!   repository is created or moved, and its native refs are never
//!   stepped, whatever its marker says. The capture's objects and
//!   provenance refs are imported, the worktree's administration is added
//!   under `worktrees/`, and the landing's own ledgers are written, as any
//!   linked landing writes them. The exclude policy is a linked landing's,
//!   except that a beside landing never appends to the shared
//!   `info/exclude` (that would change what the live checkout ignores):
//!   under `--exclude-union` it lands only when nothing is missing, and
//!   otherwise refuses `GIT_IGNORE_POLICY_CONFLICT`. A landing a crash cut
//!   short is finished forward, never converged, and refused
//!   `GIT_DESTINATION_OCCUPIED` when it held an older capture.
//! - **An existing beside path** refuses `GIT_DESTINATION_OCCUPIED` unless
//!   it is this item's own landing of this capture (its workspace ledger
//!   names both), or a standalone checkout (its own `.git` directory) no
//!   ledger names that proves equal to the capture: it is adopted in place
//!   in its own repository (A11a), and the capture is imported into the
//!   planned repository for the refs. That proof compares HEAD, the index
//!   entries, the exclude and every carried seat's kind, mode and bytes;
//!   untracked files the capture does not carry, the checkout's own stash
//!   and its extra branches and remotes are not compared, and the
//!   adoption steps the carried refs in that checkout's repository (absent
//!   ones created, equal ones adopted, differing ones kept). One that
//!   differs refuses `GIT_DESTINATION_OCCUPIED`: the beside path holds
//!   something else.
//! - **Refs in.** Before anything is landed, a ref named `refs/<ns>` or
//!   `refs/<ns>-stash` refuses `GIT_DESTINATION_OCCUPIED`. After the
//!   landing, every captured ref `refs/<name>` is wanted as
//!   `refs/<ns>/<name>`, and stash entry `n` (newest first) as
//!   `refs/<ns>-stash/<n>`, a namespace of its own. A wanted name that a
//!   ref of the repository holds the same value at is present; one held at
//!   another value is kept, never moved or deleted; one a ref of the
//!   repository would collide with as a directory or a file refuses
//!   `GIT_DESTINATION_OCCUPIED` before any ref is written. The rest are
//!   recorded in STATE as pending, then created in one compare-and-swap
//!   transaction through the converge's ref writer (synced), then recorded
//!   as created.
//! - **Recorded, counted.** STATE holds `{item}-{blake3(SOURCE)}.beside`
//!   ([`BesideRecord`]): the beside path, the suffix, the capture (from the
//!   beside checkout's own ledger), the instance, the refs pending,
//!   created and kept. A rerun after a crash moves each pending ref it
//!   finds at its value to created. The receipt counts `beside-landed` (or
//!   `beside-present` when the beside checkout was there already),
//!   `beside-linked` or `beside-standalone`, `beside-ref-created`,
//!   `beside-ref-present` and `beside-ref-kept`. With every wanted ref in,
//!   the journal and outcome are `workspace-beside`, which `closure-report`
//!   (v5) classifies `applied-beside`; a kept ref refuses the item
//!   `GIT_DESTINATION_OCCUPIED` instead, after the record, so it stays
//!   pending review until the namespace is resolved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{git, landed, native_refs, oid, text};
use crate::{BulkloadRefusal, Result};

/// What STATE records of one item's beside landing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BesideRecord {
    /// `estate::id` of the item.
    pub item: String,
    /// The beside checkout's absolute path.
    pub path: Vec<u8>,
    /// `--beside`'s suffix, as given.
    pub suffix: String,
    /// The capture the beside checkout holds (its own ledger's).
    pub capture: [u8; 32],
    /// The capture's provenance instance in the planned repository.
    pub instance: String,
    /// `(name, oid)` each ref about to be created (write-ahead).
    pub pending: Vec<(String, String)>,
    /// `(name, oid)` each ref this item's beside landing created.
    pub created: Vec<(String, String)>,
    /// `(name, existing, carried)` each ref kept with another value.
    pub kept: Vec<(String, String, String)>,
}

impl landed::Record for BesideRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-beside2";
}

/// The record at `path`, if any.
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<BesideRecord>> {
    landed::read::<BesideRecord>(path)
}

/// Write `record` at `path` (atomically, durably).
///
/// # Errors
/// IO.
pub fn write_record(path: &Path, record: &BesideRecord) -> Result<()> {
    landed::write(path, record)
}

/// The ref namespace of `suffix`: the suffix without its leading `.`, one
/// plain ref component (see the module docs).
///
/// # Errors
/// `FIELD_DOMAIN_VIOLATION` for any other suffix.
pub fn namespace(suffix: &str) -> Result<String> {
    let name = suffix.strip_prefix('.').unwrap_or(suffix);
    let plain = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !name.starts_with(['.', '-'])
        && !name.ends_with('.')
        && !name.as_bytes().ends_with(b".lock")
        && !name.contains("..");
    if plain {
        Ok(name.to_owned())
    } else {
        Err(BulkloadRefusal::FieldDomainViolation)
    }
}

/// The beside path of `workspace` under `suffix`: the path with `suffix`
/// appended verbatim.
#[must_use]
pub fn path(workspace: &Path, suffix: &str) -> PathBuf {
    let mut name = workspace.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Every ref name of `repo`.
fn names(repo: &Path) -> Result<BTreeMap<String, String>> {
    let listed = text(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
    let mut names = BTreeMap::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        names.insert(name.to_owned(), value.to_owned());
    }
    Ok(names)
}

/// Whether `repo` holds a ref named `refs/<ns>` or `refs/<ns>-stash`: the
/// namespace a beside landing writes under would collide with it.
///
/// # Errors
/// Whatever listing the refs refuses.
pub fn namespace_taken(repo: &Path, ns: &str) -> Result<bool> {
    let names = names(repo)?;
    Ok(
        names.contains_key(&format!("refs/{ns}"))
            || names.contains_key(&format!("refs/{ns}-stash")),
    )
}

/// Import `staged` into `repo` and name the instance it wrote.
///
/// Provenance only, as a refs-only item imports: for a beside checkout
/// adopted in its own repository, whose refs go in under `refs/<ns>/` of
/// the planned one.
///
/// # Errors
/// Whatever the import refuses.
pub fn import_instance(repo: &Path, staged: &super::StagedBundle, source: &str) -> Result<String> {
    super::import_verified_instance(repo, staged.path(), source).map(|(_, instance)| instance)
}

/// The refs a beside landing wants in `repo`: each captured ref of
/// `instance` under `refs/<ns>/`, each stash entry as `refs/<ns>-stash/<n>`.
///
/// # Errors
/// Whatever reading the provenance refuses.
pub fn wanted(
    repo: &Path,
    source: &str,
    instance: &str,
    ns: &str,
) -> Result<BTreeMap<String, String>> {
    let mut wanted = BTreeMap::new();
    for (name, value) in native_refs::carried(repo, source, instance)? {
        let rest = name
            .strip_prefix("refs/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        wanted.insert(format!("refs/{ns}/{rest}"), value);
    }
    let (stash, _) = native_refs::carried_stash(repo, source, instance)?;
    for (index, entry) in stash.iter().enumerate() {
        wanted.insert(format!("refs/{ns}-stash/{index}"), entry.oid.clone());
    }
    Ok(wanted)
}

/// What [`plan`] decided for the wanted refs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefsPlan {
    /// `(name, oid)` to create.
    pub create: Vec<(String, String)>,
    /// `(name, oid)` already held at their value.
    pub present: Vec<(String, String)>,
    /// `(name, existing, carried)` held at another value: kept.
    pub kept: Vec<(String, String, String)>,
}

/// Decide each wanted ref against `repo`'s refs now (see the module docs).
/// Read-only.
///
/// # Errors
/// `GIT_DESTINATION_OCCUPIED` when a ref to create collides with one of the
/// repository's as a directory or a file; `GIT_INVENTORY_MALFORMED` for a
/// malformed listing.
pub fn plan(repo: &Path, wanted: &BTreeMap<String, String>) -> Result<RefsPlan> {
    let existing = names(repo)?;
    let mut planned = RefsPlan::default();
    for (name, value) in wanted {
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        match existing.get(name) {
            Some(held) if held == value => planned.present.push((name.clone(), value.clone())),
            Some(held) => planned
                .kept
                .push((name.clone(), held.clone(), value.clone())),
            None => {
                if native_refs::name_conflict(name, existing.keys().map(String::as_str)) {
                    return Err(BulkloadRefusal::GitDestinationOccupied);
                }
                planned.create.push((name.clone(), value.clone()));
            }
        }
    }
    Ok(planned)
}

/// Create `refs` in `repo`, create-only.
///
/// One transaction of compare-and-swap `create` lines through the
/// converge's ref writer, which syncs the refs and their directories before
/// returning. A ref another writer created meanwhile refuses the whole
/// transaction, with nothing moved.
///
/// # Errors
/// Whatever the transaction refuses.
pub fn create(repo: &Path, refs: &[(String, String)]) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut transaction = Vec::new();
    for (name, value) in refs {
        transaction.extend_from_slice(b"create ");
        transaction.extend_from_slice(name.as_bytes());
        transaction.push(0);
        transaction.extend_from_slice(value.as_bytes());
        transaction.push(0);
    }
    let names: Vec<&str> = refs.iter().map(|(name, _)| name.as_str()).collect();
    native_refs::update_refs(repo, None, &transaction, &names)
}

/// Settle `record`'s pending refs against `repo`.
///
/// Each pending ref `repo` holds at its value moves to created (a crash cut
/// the record off after the refs); the rest of the pending list is dropped
/// (never written, or since changed: decided again).
///
/// # Errors
/// Whatever listing the refs refuses.
pub fn settle(repo: &Path, record: &mut BesideRecord) -> Result<()> {
    if record.pending.is_empty() {
        return Ok(());
    }
    let existing = names(repo)?;
    for (name, value) in std::mem::take(&mut record.pending) {
        if existing.get(&name) == Some(&value)
            && !record.created.iter().any(|(held, _)| *held == name)
        {
            record.created.push((name, value));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_namespace_is_one_plain_ref_component() {
        assert_eq!(namespace(".neo-20261016").unwrap(), "neo-20261016");
        assert_eq!(namespace("neo-20261016").unwrap(), "neo-20261016");
        assert_eq!(namespace("neo_1.x").unwrap(), "neo_1.x");
        for bad in [
            "", ".", "..neo", ".-neo", "neo/x", "neo..x", "neo.", "neo.lock", "neo x", "neo@{1}",
            "neo:x", "neo~1", "neo^", "neo*", "neo\\x",
        ] {
            assert_eq!(
                namespace(bad),
                Err(BulkloadRefusal::FieldDomainViolation),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_path_appends_the_suffix_verbatim() {
        assert_eq!(
            path(Path::new("/srv/git/xoxd.ai"), ".neo-20261016"),
            PathBuf::from("/srv/git/xoxd.ai.neo-20261016")
        );
        assert_eq!(
            path(Path::new("/srv/git/xoxd.ai"), "-beside"),
            PathBuf::from("/srv/git/xoxd.ai-beside")
        );
    }

    #[test]
    fn a_pending_ref_found_at_its_value_settles_created() {
        let base = std::env::temp_dir().join(format!("bulkload-beside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.name=T", "-c", "user.email=t@localhost", "-C"])
                .arg(&base)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        run(&["init", "--quiet", "--template=", "-b", "main"]);
        run(&["commit", "--quiet", "--allow-empty", "-m", "c0"]);
        let head = run(&["rev-parse", "HEAD"]).trim().to_owned();
        let wanted: BTreeMap<String, String> = [
            ("refs/ns/heads/main".to_owned(), head.clone()),
            ("refs/ns/heads/other".to_owned(), head.clone()),
            ("refs/ns-stash/0".to_owned(), head.clone()),
        ]
        .into();
        // A crash after the refs: the record still says pending.
        let first = plan(&base, &wanted).unwrap();
        assert_eq!(first.create.len(), 3);
        create(&base, &first.create).unwrap();
        let mut record = BesideRecord {
            pending: first.create.clone(),
            ..BesideRecord::default()
        };
        // One pending ref was never written (a different value).
        record
            .pending
            .push(("refs/ns/heads/never".to_owned(), head.clone()));
        settle(&base, &mut record).unwrap();
        assert!(record.pending.is_empty());
        assert_eq!(record.created, first.create);
        let again = plan(&base, &wanted).unwrap();
        assert!(again.create.is_empty());
        assert_eq!(again.present.len(), 3);
        // A ref at another value is kept; a directory/file collision refuses.
        run(&["update-ref", "refs/ns/heads/main", "HEAD"]);
        run(&["commit", "--quiet", "--allow-empty", "-m", "c1"]);
        let moved: BTreeMap<String, String> = [(
            "refs/ns/heads/main".to_owned(),
            run(&["rev-parse", "HEAD"]).trim().to_owned(),
        )]
        .into();
        assert_eq!(plan(&base, &moved).unwrap().kept.len(), 1);
        let collides: BTreeMap<String, String> =
            [("refs/ns/heads/main/deeper".to_owned(), head.clone())].into();
        assert_eq!(
            plan(&base, &collides),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        assert!(!namespace_taken(&base, "ns").unwrap());
        run(&["update-ref", "refs/taken", &head]);
        assert!(namespace_taken(&base, "taken").unwrap());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
