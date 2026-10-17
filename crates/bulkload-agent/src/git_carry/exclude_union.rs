//! `estate-apply --exclude-union` (M2 round 2, item 2, 2026-10-17).
//!
//! A first linked landing keeps the destination repository's common
//! `info/exclude` only when it is the capture's (or a newer one the main
//! item set, finding 12); anything else refuses
//! `GIT_IGNORE_POLICY_CONFLICT` and the file is never overwritten. The
//! in-place run of 2026-10-16 refused 250 new linked worktrees this way, in
//! repositories whose source exclude was a pure superset of the
//! destination's. `--exclude-union` is the opt-in, recorded write that
//! lets those land:
//!
//! - **Append only.** The captured lines the destination's exclude lacks
//!   (exact bytes; blank lines skipped; each once, in captured order) are
//!   appended after its last line. No existing byte is removed, changed or
//!   reordered: the old file is a byte prefix of the new one. When nothing
//!   is missing nothing is written, so a rerun, and every later linked
//!   item of the same repository, is a no-op.
//! - **Only where order cannot matter.** Git decides an ignore by the last
//!   matching pattern, so appending is a pure union of the two policies
//!   only when neither holds a negation (`!pattern`; `\!` is a literal and
//!   does not count): an appended pattern could re-ignore a path an earlier
//!   destination negation re-included, and an appended negation could
//!   re-include what a destination pattern ignores. Either side holding a
//!   negation keeps the refusal.
//! - **Never a ledgered exclude.** An exclude a bulkload ledger at the
//!   repository records (any label's main workspace ledger, or a ref
//!   ledger's exclude row) belongs to that main item's converge; appending
//!   would make its proof fail. Those keep the refusal too, as do an
//!   exclude that is not a regular file (a symlink is never written
//!   through) and one that changed between the decision and the write.
//! - **Recorded, write-ahead.** The append is recorded in STATE
//!   (`{item}-{blake3(source)}.exclude-union`, [`ExcludeUnion`]) as an
//!   intent naming the path, the digests before and after and the lines,
//!   before the file is replaced (temporary, fsync, rename, directory
//!   fsync, the file's mode kept); then marked `appended`. A crash between
//!   leaves an intent the next run under the flag settles by the file's
//!   digest: `appended` when it holds the after digest, `not-written` when
//!   it still holds the before digest, otherwise left an intent (someone
//!   else changed the file since; the record never guesses).
//! - **Counted.** The landing's receipt carries `exclude-appended=<lines>`.
//!
//! Without the flag nothing here runs and the refusal is unchanged.
//! `estate-diff` reports the item `ignore-policy-conflict` as before and,
//! where the flag would land it, the component `exclude-union=<lines>`.

use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::landed;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// One append's state in the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppendState {
    /// Recorded before the write; not yet known to have happened.
    Intent,
    /// The file held the after digest when last settled.
    Appended,
    /// The file still held the before digest: the write never happened.
    NotWritten,
}

/// One append to one exclude file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Append {
    /// The exclude file's absolute path.
    pub path: Vec<u8>,
    /// blake3 of the file before the append (of nothing when it was absent).
    pub before: [u8; 32],
    /// blake3 of the file after it.
    pub after: [u8; 32],
    /// The lines appended, without their newline.
    pub lines: Vec<Vec<u8>>,
    pub state: AppendState,
}

/// The appends one item's applies made (`{item}-{source}.exclude-union` in
/// STATE). Appended to, never rewritten but for settling a state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludeUnion {
    /// `estate::id` of the item.
    pub item: String,
    pub appends: Vec<Append>,
}

impl landed::Record for ExcludeUnion {
    const MAGIC: &'static [u8; 16] = b"bulkload-exunion";
}

/// The record at `path`, if any (for reports and tests).
///
/// # Errors
/// IO; a record that does not decode.
pub fn read_record(path: &Path) -> Result<Option<ExcludeUnion>> {
    landed::read::<ExcludeUnion>(path)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// The lines of an exclude, their newlines removed (the one a final
/// newline ends is not a line of its own).
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let empty = bytes.is_empty();
    body.split(|byte| *byte == b'\n').filter(move |_| !empty)
}

fn blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

fn negation(line: &[u8]) -> bool {
    line.first() == Some(&b'!')
}

/// The captured lines `existing` lacks, in captured order, each once;
/// empty when it holds them all.
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` when either side holds a negation (an
/// append would not be a pure union) and a line is missing; with nothing
/// missing nothing is written, so a negation is no obstacle.
pub fn missing(existing: &[u8], captured: &[u8]) -> Result<Vec<Vec<u8>>> {
    let held: std::collections::BTreeSet<&[u8]> = lines(existing).collect();
    let mut seen = std::collections::BTreeSet::new();
    let missing: Vec<Vec<u8>> = lines(captured)
        .filter(|line| !blank(line) && !held.contains(line) && seen.insert(*line))
        .map(<[u8]>::to_vec)
        .collect();
    if !missing.is_empty() && lines(existing).chain(lines(captured)).any(negation) {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(missing)
}

/// `existing` with `append` after its last line (a newline added first
/// when it does not end in one).
#[must_use]
pub fn appended(existing: &[u8], append: &[Vec<u8>]) -> Vec<u8> {
    let mut out = existing.to_vec();
    if !out.is_empty() && out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    for line in append {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// Whether a bulkload ledger at `common` records its exclude: any label's
/// main workspace ledger, or a ref ledger's exclude row.
///
/// # Errors
/// IO; a ref ledger that does not decode.
pub fn ledgered(common: &Path) -> Result<bool> {
    const SITE: &str = "git_carry::exclude_union::ledgered";
    let root = common.join(landed::LEDGER_ROOT);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    for entry in entries {
        let label = entry.refuse_at(SITE)?.path();
        if landed::exists(&label.join(landed::WORKSPACE_LANDED))? {
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

/// The union's lines for `path` (`existing` read from it): what
/// [`missing`] says, after the policy checks a write needs (a regular file
/// or absent, never a ledgered exclude).
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` where the union does not apply.
pub fn plan(common: &Path, path: &Path, existing: &[u8], captured: &[u8]) -> Result<Vec<Vec<u8>>> {
    let append = missing(existing, captured)?;
    if append.is_empty() {
        return Ok(append);
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err(BulkloadRefusal::GitIgnorePolicyConflict),
        Err(error) => return Err(crate::refuse::io(&error, "git_carry::exclude_union::plan")),
    }
    if ledgered(common)? {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(append)
}

fn read_exclude(path: &Path) -> Result<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(crate::refuse::io(
            &error,
            "git_carry::exclude_union::read_exclude",
        )),
    }
}

/// Settle the intents of `record` that name `path` by the file's digest
/// now (see the module docs).
///
/// # Errors
/// IO; a record that does not decode.
pub fn settle(record: &Path, path: &Path) -> Result<()> {
    let Some(mut kept) = landed::read::<ExcludeUnion>(record)? else {
        return Ok(());
    };
    let name = path.as_os_str().as_bytes();
    let now = digest(&read_exclude(path)?);
    let mut changed = false;
    for append in &mut kept.appends {
        if append.state != AppendState::Intent || append.path != name {
            continue;
        }
        if now == append.after {
            append.state = AppendState::Appended;
            changed = true;
        } else if now == append.before {
            append.state = AppendState::NotWritten;
            changed = true;
        }
    }
    if changed {
        landed::write(record, &kept)?;
    }
    Ok(())
}

/// Append `lines` to the exclude at `path`, which must still hold
/// `existing`, recorded in `record` for `item`; the new bytes.
///
/// # Errors
/// `GIT_IGNORE_POLICY_CONFLICT` when the file changed since the decision;
/// IO.
pub fn apply(
    (record, item): (&Path, &str),
    path: &Path,
    existing: &[u8],
    lines: &[Vec<u8>],
) -> Result<Vec<u8>> {
    const SITE: &str = "git_carry::exclude_union::apply";
    let bytes = appended(existing, lines);
    let mut kept = landed::read::<ExcludeUnion>(record)?.unwrap_or_default();
    item.clone_into(&mut kept.item);
    kept.appends.push(Append {
        path: path.as_os_str().as_bytes().to_vec(),
        before: digest(existing),
        after: digest(&bytes),
        lines: lines.to_vec(),
        state: AppendState::Intent,
    });
    landed::write(record, &kept)?;
    fault_point!(EstateApplyExcludeIntent);
    let mode = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => meta.permissions().mode() & 0o7777,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o644,
        Ok(_) => return Err(BulkloadRefusal::GitIgnorePolicyConflict),
        Err(error) => return Err(crate::refuse::io(&error, SITE)),
    };
    // A concurrent writer since the decision: nothing is written, and the
    // intent says so.
    if read_exclude(path)? != existing {
        if let Some(last) = kept.appends.last_mut() {
            last.state = AppendState::NotWritten;
        }
        landed::write(record, &kept)?;
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).refuse_at(SITE)?;
    }
    landed::write_bytes_mode(path, &bytes, mode)?;
    fault_point!(EstateApplyExcludeAppended);
    if let Some(last) = kept.appends.last_mut() {
        last.state = AppendState::Appended;
    }
    landed::write(record, &kept)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn v(lines: &[&str]) -> Vec<Vec<u8>> {
        lines.iter().map(|line| line.as_bytes().to_vec()).collect()
    }

    #[test]
    fn a_superset_appends_only_the_missing_lines_in_captured_order() {
        let existing = b"*.log\nbuild/\n";
        let captured = b"# neo\n*.log\n\nscratch/\nbuild/\n.envrc\nscratch/\n";
        let lines = missing(existing, captured).unwrap();
        assert_eq!(lines, v(&["# neo", "scratch/", ".envrc"]));
        let out = appended(existing, &lines);
        assert!(out.starts_with(existing));
        assert_eq!(out, b"*.log\nbuild/\n# neo\nscratch/\n.envrc\n");
        assert!(missing(&out, captured).unwrap().is_empty());
    }

    #[test]
    fn a_file_without_a_final_newline_keeps_its_bytes() {
        let out = appended(b"a", &v(&["b"]));
        assert_eq!(out, b"a\nb\n");
        assert_eq!(missing(b"a", b"a\n").unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(appended(b"", &v(&["x"])), b"x\n");
    }

    #[test]
    fn a_negation_on_either_side_refuses_when_lines_are_missing() {
        assert_eq!(
            missing(b"*.log\n", b"*.log\n!keep.log\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert_eq!(
            missing(b"*.log\n!keep.log\n", b"*.log\nbuild/\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        // A literal `\!` is a pattern, not a negation.
        assert_eq!(missing(b"a\n", b"a\n\\!bang\n").unwrap(), v(&["\\!bang"]));
        // Nothing missing: nothing is written, so a negation is no obstacle.
        assert!(missing(b"!a\nb\n", b"b\n!a\n").unwrap().is_empty());
    }

    #[test]
    fn a_ledgered_or_linked_exclude_is_never_planned() {
        let base = std::env::temp_dir().join(format!(
            "bulkload-exunion-{}-{}",
            std::process::id(),
            line!()
        ));
        let common = base.join("repo.git");
        fs::create_dir_all(common.join("info")).unwrap();
        let path = common.join("info/exclude");
        fs::write(&path, b"a\n").unwrap();
        assert_eq!(plan(&common, &path, b"a\n", b"a\nb\n").unwrap(), v(&["b"]));
        // A symlinked exclude is never written through.
        let target = base.join("elsewhere");
        fs::write(&target, b"a\n").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(
            plan(&common, &path, b"a\n", b"a\nb\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"a\n").unwrap();
        // A main workspace ledger under any label owns the exclude.
        let label = common.join(landed::LEDGER_ROOT).join("other");
        fs::create_dir_all(&label).unwrap();
        assert!(!ledgered(&common).unwrap());
        fs::write(label.join(landed::WORKSPACE_LANDED), b"").unwrap();
        assert!(ledgered(&common).unwrap());
        assert_eq!(
            plan(&common, &path, b"a\n", b"a\nb\n"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        fs::remove_file(label.join(landed::WORKSPACE_LANDED)).unwrap();
        // So does a ref ledger's exclude row; one without it does not.
        let mut refs = landed::RefLedger::default();
        landed::write(&label.join(landed::REFS_LANDED), &refs).unwrap();
        assert!(!ledgered(&common).unwrap());
        refs.exclude = Some(landed::LandedExclude {
            digest: [0; 32],
            observed_ns: 1,
        });
        landed::write(&label.join(landed::REFS_LANDED), &refs).unwrap();
        assert!(ledgered(&common).unwrap());
        // Nothing missing never needs the checks.
        assert!(plan(&common, &path, b"a\nb\n", b"b\n").unwrap().is_empty());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn apply_records_an_intent_then_the_append_and_settles_by_digest() {
        let base = std::env::temp_dir().join(format!(
            "bulkload-exunion-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(base.join("info")).unwrap();
        let path = base.join("info/exclude");
        fs::write(&path, b"a\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let record = base.join("item.exclude-union");
        let out = apply((&record, "item"), &path, b"a\n", &v(&["b", "c"])).unwrap();
        assert_eq!(out, b"a\nb\nc\n");
        assert_eq!(fs::read(&path).unwrap(), out);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o640
        );
        let kept = read_record(&record).unwrap().unwrap();
        assert_eq!(kept.item, "item");
        assert_eq!(kept.appends.len(), 1);
        assert_eq!(kept.appends[0].state, AppendState::Appended);
        assert_eq!(kept.appends[0].lines, v(&["b", "c"]));
        // A changed file since the decision: nothing written, refused, and
        // recorded not-written.
        assert_eq!(
            apply((&record, "item"), &path, b"a\n", &v(&["d"])),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert_eq!(fs::read(&path).unwrap(), b"a\nb\nc\n");
        let kept = read_record(&record).unwrap().unwrap();
        assert_eq!(kept.appends.len(), 2);
        assert_eq!(kept.appends[1].state, AppendState::NotWritten);
        // Intents a crash left settle by the file's digest: the after
        // digest is appended, the before digest not written, any other
        // stays an intent.
        let mut forged = kept;
        for (before, after) in [
            (b"x".as_slice(), b"a\nb\nc\n".as_slice()),
            (b"a\nb\nc\n", b"y"),
            (b"x", b"y"),
        ] {
            forged.appends.push(Append {
                path: path.as_os_str().as_bytes().to_vec(),
                before: digest(before),
                after: digest(after),
                lines: v(&["z"]),
                state: AppendState::Intent,
            });
        }
        landed::write(&record, &forged).unwrap();
        settle(&record, &path).unwrap();
        let states: Vec<AppendState> = read_record(&record)
            .unwrap()
            .unwrap()
            .appends
            .iter()
            .map(|append| append.state)
            .collect();
        assert_eq!(
            states,
            [
                AppendState::Appended,
                AppendState::NotWritten,
                AppendState::Appended,
                AppendState::NotWritten,
                AppendState::Intent
            ]
        );
        fs::remove_dir_all(&base).unwrap();
    }
}
