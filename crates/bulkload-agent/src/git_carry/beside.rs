//! `estate-apply --beside` (M2 round 2, item c, 2026-10-17).
//!
//! The in-place run's i02 batch ended 12 workspace items refused: 10
//! `GIT_DESTINATION_DIVERGED` (the live checkout differs from the capture,
//! so `--adopt` cannot prove it) and 2 `GIT_AUTHORITY_CHANGED`. Each was
//! carried by hand as a refs-only item into the live repository and a
//! second, standalone item at `<path>.neo-20261016`. `--beside` does it
//! natively, and leaves the live checkout exactly as it was:
//!
//! - **Where.** The beside path is the planned workspace's path with
//!   `.<suffix>` appended, `suffix` being `<prefix>-<YYYYMMDD>`: `prefix`
//!   the SOURCE label up to its first `-` (`neo-inplace-20261016` gives
//!   `neo`), and the date the label's first dash-separated component that is
//!   a calendar date (`20261016`), else the capture's own instant (its
//!   `started_ns`, UTC). A label with neither, against a legacy capture with
//!   no instant, refuses `REQUIRED_FIELD_MISSING`.
//! - **What lands there.** A first linked landing of the capture into the
//!   planned (live) repository, detached at the captured head with its
//!   branch recorded (`head-detached-beside`): no branch of the live
//!   repository is created or moved, and its native refs are never
//!   stepped, whatever its marker says. The capture's objects are imported
//!   (its provenance instance, as any landing imports it). The exclude
//!   policy is a linked landing's (`--exclude-union` applies). A beside path
//!   that exists refuses `GIT_DESTINATION_OCCUPIED` unless it is this
//!   item's own landing of this capture (its workspace ledger says so), a
//!   landing of it a crash cut short (its landing intent), which is
//!   finished forward, or, under `--adopt`, a standalone checkout (its own
//!   `.git` directory, no ledger; the in-place lane made
//!   `<path>.neo-20261016` by hand) that proves equal to the capture: it is
//!   adopted in place in its own repository (A11a), and the capture is
//!   imported into the planned repository for the refs below. One that
//!   differs refuses `GIT_DESTINATION_DIVERGED`.
//! - **Refs in.** Every captured ref `refs/<name>` is created as
//!   `refs/<suffix>/<name>`, and stash entry `n` (newest first) as
//!   `refs/<suffix>/stashes/<n>`, in one `update-ref` transaction of
//!   `create` lines: create-only. A name that holds the same value already
//!   is counted present; one that holds another value is kept as it is
//!   (never moved or deleted) and counted. Nothing outside
//!   `refs/<suffix>/` is written.
//! - **Recorded, counted.** After the refs (and the landing's own
//!   ledgers), STATE records `{item}-{blake3(SOURCE)}.beside`
//!   ([`BesideRecord`]): the beside path, the suffix, the capture, the
//!   instance, the refs created and the refs kept. The receipt counts
//!   `beside-landed`, `beside-ref-created=<n>`, `beside-ref-present=<n>`
//!   and `beside-ref-kept=<n>`; the journal and outcome are
//!   `workspace-beside`, which `closure-report` (v5) classifies
//!   `applied-beside`.
//!
//! Only the item's own refusals `GIT_DESTINATION_DIVERGED` and
//! `GIT_AUTHORITY_CHANGED`, returned by the planned workspace's apply, lead
//! to a beside landing; nothing else does, and without the flag nothing
//! changes.

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
    pub suffix: String,
    /// The capture's bundle digest.
    pub capture: [u8; 32],
    /// The capture's provenance instance in the repository.
    pub instance: String,
    /// `(name, oid)` each ref this landing created.
    pub created: Vec<(String, String)>,
    /// `(name, existing, carried)` each ref kept with another value.
    pub kept: Vec<(String, String, String)>,
}

impl landed::Record for BesideRecord {
    const MAGIC: &'static [u8; 16] = b"bulkload-beside1";
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

/// Days since 1970-01-01 to a civil (year, month, day), proleptic
/// Gregorian (Hinnant's `civil_from_days`).
const fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// Whether `text` is a `YYYYMMDD` calendar date.
fn date_like(text: &str) -> bool {
    if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let field = |range: std::ops::Range<usize>| {
        text.get(range)
            .and_then(|digits| digits.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let (year, month, day) = (field(0..4), field(4..6), field(6..8));
    (1970..=9999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// The beside suffix for SOURCE label `source` and a capture taken at
/// `observed_ns` (see the module docs).
///
/// # Errors
/// `REQUIRED_FIELD_MISSING` when neither the label nor the capture names a
/// date; `PATH_NOT_PORTABLE` when the label's prefix is not a plain name.
pub fn suffix(source: &str, observed_ns: i128) -> Result<String> {
    let prefix = source.split('-').next().unwrap_or_default();
    if prefix.is_empty()
        || prefix.starts_with('.')
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
    {
        return Err(BulkloadRefusal::PathNotPortable);
    }
    let date = if let Some(date) = source.split('-').skip(1).find(|part| date_like(part)) {
        date.to_owned()
    } else if observed_ns > 0 {
        let days = i64::try_from(observed_ns.div_euclid(86_400 * 1_000_000_000))
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
        let (year, month, day) = civil(days);
        format!("{year:04}{month:02}{day:02}")
    } else {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    };
    Ok(format!("{prefix}-{date}"))
}

/// The beside path of `workspace` under `suffix`: `<workspace>.<suffix>`.
///
/// # Errors
/// `PATH_NOT_ABSOLUTE` for a workspace with no final name.
pub fn path(workspace: &Path, suffix: &str) -> Result<PathBuf> {
    let name = workspace
        .file_name()
        .ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let mut beside = name.to_owned();
    beside.push(".");
    beside.push(suffix);
    Ok(workspace.with_file_name(beside))
}

/// Import `staged` into `repo` (provenance only, as a refs-only item does)
/// and name the instance it wrote: for a beside checkout adopted in its own
/// repository, whose refs go in under `refs/<suffix>/` of the planned one.
///
/// # Errors
/// Whatever the import refuses.
pub fn import_instance(repo: &Path, staged: &super::StagedBundle, source: &str) -> Result<String> {
    super::import_verified_instance(repo, staged.path(), source).map(|(_, instance)| instance)
}

/// What [`refs_in`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefsIn {
    pub created: Vec<(String, String)>,
    pub present: u64,
    pub kept: Vec<(String, String, String)>,
}

/// The refs a beside landing wants in `repo`: each captured ref of
/// `instance` under `refs/<suffix>/`, each stash entry as
/// `refs/<suffix>/stashes/<n>`.
///
/// # Errors
/// Whatever reading the provenance refuses.
pub fn wanted(
    repo: &Path,
    source: &str,
    instance: &str,
    suffix: &str,
) -> Result<BTreeMap<String, String>> {
    let mut wanted = BTreeMap::new();
    for (name, value) in native_refs::carried(repo, source, instance)? {
        let rest = name
            .strip_prefix("refs/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        wanted.insert(format!("refs/{suffix}/{rest}"), value);
    }
    let (stash, _) = native_refs::carried_stash(repo, source, instance)?;
    for (index, entry) in stash.iter().enumerate() {
        wanted.insert(format!("refs/{suffix}/stashes/{index}"), entry.oid.clone());
    }
    Ok(wanted)
}

/// Create the refs [`wanted`] names in `repo`, create-only (see the module
/// docs).
///
/// # Errors
/// `GIT_INVENTORY_MALFORMED` for a malformed listing; whatever the
/// transaction refuses (a ref created by another writer meanwhile refuses
/// the whole transaction, with nothing moved).
pub fn refs_in(repo: &Path, wanted: &BTreeMap<String, String>, suffix: &str) -> Result<RefsIn> {
    let listed = text(git(repo).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        &format!("refs/{suffix}/"),
    ]))?;
    let mut existing = BTreeMap::new();
    for line in listed.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        existing.insert(name.to_owned(), value.to_owned());
    }
    let mut done = RefsIn::default();
    let mut transaction = Vec::new();
    for (name, value) in wanted {
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        match existing.get(name) {
            Some(held) if held == value => done.present += 1,
            Some(held) => done.kept.push((name.clone(), held.clone(), value.clone())),
            None => {
                transaction.extend_from_slice(b"create ");
                transaction.extend_from_slice(name.as_bytes());
                transaction.push(0);
                transaction.extend_from_slice(value.as_bytes());
                transaction.push(0);
                done.created.push((name.clone(), value.clone()));
            }
        }
    }
    if !transaction.is_empty() {
        // The converge's one destination ref writer: the lines are
        // compare-and-swap creates, and the refs and their directories are
        // synced before any record names them.
        let names: Vec<&str> = done.created.iter().map(|(name, _)| name.as_str()).collect();
        native_refs::update_refs(repo, None, &transaction, &names)?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_suffix_takes_the_label_prefix_and_its_date() {
        assert_eq!(suffix("neo-inplace-20261016", 0).unwrap(), "neo-20261016");
        assert_eq!(suffix("neo", 0), Err(BulkloadRefusal::RequiredFieldMissing));
        // 2026-10-17T00:51:00Z from the capture when the label has no date.
        let ns = 1_792_198_260_i128 * 1_000_000_000;
        assert_eq!(suffix("neo", ns).unwrap(), "neo-20261017");
        assert_eq!(suffix("neo-inplace", ns).unwrap(), "neo-20261017");
        // Not a date: 13th month, wrong width.
        assert_eq!(suffix("neo-20261316", ns).unwrap(), "neo-20261017");
        assert_eq!(suffix("neo-2026101", ns).unwrap(), "neo-20261017");
        assert_eq!(
            suffix("../x-20261016", 0),
            Err(BulkloadRefusal::PathNotPortable)
        );
        assert_eq!(
            suffix("-20261016", 0),
            Err(BulkloadRefusal::PathNotPortable)
        );
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_743), (2026, 10, 17));
    }

    #[test]
    fn the_path_appends_the_suffix_to_the_final_name() {
        assert_eq!(
            path(Path::new("/srv/git/xoxd.ai"), "neo-20261016").unwrap(),
            PathBuf::from("/srv/git/xoxd.ai.neo-20261016")
        );
        assert_eq!(
            path(Path::new("/"), "neo-20261016"),
            Err(BulkloadRefusal::PathNotAbsolute)
        );
    }
}
