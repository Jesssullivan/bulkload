//! The rebuildable roots a capture omitted, read from what the capture
//! carries in band, for a capture that has no `.omissions` sidecar (M2
//! round 2, d).
//!
//! The `{bundle}.omissions` sidecar is newer than the omission itself: every
//! capture since `bf10700c` (2026-09-23) omits the rebuildable set and names
//! what it omitted in its own bundle, as the metadata ref
//! `refs/carry-export/rebuildable-omissions-v1` (a parentless commit whose
//! tree's `value` blob is the postcard list), and only when it omitted
//! something. The sidecar arrived with `3fe1f075` (2026-10-16). A corpus
//! captured by an engine between the two (the 2026-10-16 in-place batches,
//! captured on neo by `5beb0314`) has no sidecar for any item, and
//! `closure-report` listed every item `unknown`.
//!
//! This reader answers from the bundle's header, which every capture bundle
//! lists its metadata refs in (a ref table moves only native refs out of it,
//! and a thin bundle lists every private ref beside its prerequisites):
//!
//! - no omission ref: the capture omitted nothing (an engine before
//!   `bf10700c` never omitted, and a later one names what it omits);
//! - the ref: its value, read from a repository that holds that object,
//!   the item's destination repository once its import ran, by the oid the
//!   header names, never from the pack (which would mean indexing the whole
//!   bundle);
//! - a shallow custody envelope: its inventory is inside the custody
//!   manifest, read from the same repository, then as above.
//!
//! Reads only: the header from the corpus, objects with `rev-parse` and
//! `cat-file --batch` in the holder (`--no-optional-locks`); nothing is
//! fetched, unbundled or written. An answer that needs an object the holder
//! does not have is `Unknown`, with its reason, never zero.

use crate::refuse::RefuseAt as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use super::batch_objects::BatchObjects;
use super::{git, oid, shallow, text, RebuildableOmission, REBUILDABLE_METADATA};
use crate::{BulkloadRefusal, Result};

/// The most bytes an omission list's `value` blob may hold.
const VALUE_CAP: u64 = 16 * 1024 * 1024;

/// What a capture's bundle says it omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InBand {
    /// The omitted roots (empty: none).
    Known(Vec<RebuildableOmission>),
    /// Not answerable from what is at hand; the reason.
    Unknown(&'static str),
}

/// Why an item's omissions are unknown when its bundle header cannot be
/// read.
pub const HEADER_UNREADABLE: &str = "header-unreadable";
/// The omission ref's value was not read: no holder was offered.
pub const VALUE_NOT_READ: &str = "value-not-read";
/// The omission ref's value is not in the holder (its import has not run).
pub const VALUE_NOT_HELD: &str = "value-not-held";
/// A shallow custody envelope's manifest was not read: no holder offered.
pub const ENVELOPE_NOT_READ: &str = "envelope-not-read";
/// A shallow custody envelope whose manifest is not in the holder.
pub const ENVELOPE_NOT_HELD: &str = "envelope-not-held";

/// The rebuildable roots the capture in `bundle` omitted, from its header
/// and, when the header names an omission list or a custody envelope, from
/// `holder`'s object store. Reads only.
#[must_use]
pub fn omissions(bundle: &Path, holder: Option<&Path>) -> InBand {
    let Ok(heads) = header(bundle) else {
        return InBand::Unknown(HEADER_UNREADABLE);
    };
    let heads = match value_of(&heads, shallow::CUSTODY) {
        None => heads,
        Some(custody) => {
            let Some(holder) = holder else {
                return InBand::Unknown(ENVELOPE_NOT_READ);
            };
            match shallow::custody_manifest(holder, custody) {
                Ok((_, inventory, _)) => inventory,
                Err(_) => return InBand::Unknown(ENVELOPE_NOT_HELD),
            }
        }
    };
    let name = format!("refs/carry-export/{REBUILDABLE_METADATA}");
    let Some(commit) = value_of(&heads, &name) else {
        return InBand::Known(Vec::new());
    };
    let Some(holder) = holder else {
        return InBand::Unknown(VALUE_NOT_READ);
    };
    read_value(holder, commit).map_or(InBand::Unknown(VALUE_NOT_HELD), InBand::Known)
}

/// The value of `name` in `<oid> <name>` lines.
fn value_of<'a>(heads: &'a str, name: &str) -> Option<&'a str> {
    heads.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(value, found)| *found == name && oid(value))
            .map(|(value, _)| value)
    })
}

/// A bundle's ref lines (`<oid> <name>`), read from its header without the
/// pack: the signature, any capability lines and prerequisites skipped, up
/// to the blank line, under the readers' header cap.
///
/// # Errors
/// An unreadable file, a header that is not a bundle's or over the cap.
fn header(bundle: &Path) -> Result<String> {
    const SITE: &str = "git_carry::in_band::header";
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(bundle)
        .refuse_at(SITE)?;
    let cap = u64::try_from(super::shared::HEADER_CAP).unwrap_or(u64::MAX);
    let mut source = BufReader::new(file.take(cap.saturating_add(1)));
    let mut heads = String::new();
    let mut line = Vec::new();
    let mut first = true;
    loop {
        line.clear();
        let read = source.read_until(b'\n', &mut line).refuse_at(SITE)?;
        if read == 0 || !line.ends_with(b"\n") {
            // No blank line within the cap: not a header this reader takes.
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if first {
            if line != b"# v2 git bundle\n" && line != b"# v3 git bundle\n" {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            first = false;
            continue;
        }
        match line.first() {
            Some(b'\n') => return Ok(heads),
            Some(b'-' | b'@') => {}
            _ => heads.push_str(
                std::str::from_utf8(&line).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?,
            ),
        }
    }
}

/// The omission list a metadata commit's `value` blob holds, read from
/// `holder`'s object store.
fn read_value(holder: &Path, commit: &str) -> Result<Vec<RebuildableOmission>> {
    if !oid(commit) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let object = text(git(holder).args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{commit}:value"),
    ]))?;
    let mut reader = BatchObjects::new(holder)?;
    let mut bytes = Vec::new();
    reader.copy_into(&object, &mut bytes, Some(VALUE_CAP))?;
    reader.finish()?;
    postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn bundle(dir: &Path, name: &str, body: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_header_without_the_omission_ref_is_known_empty() {
        let dir = tempfile::tempdir().unwrap();
        let oid_a = "a".repeat(40);
        let path = bundle(
            dir.path(),
            "b.bundle",
            format!(
                "# v2 git bundle\n-{} shared base\n{oid_a} refs/carry-export/head\n\nPACK",
                "b".repeat(40)
            )
            .as_bytes(),
        );
        assert_eq!(omissions(&path, None), InBand::Known(Vec::new()));
    }

    #[test]
    fn a_named_list_without_a_holder_is_unknown_never_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle(
            dir.path(),
            "b.bundle",
            format!(
                "# v2 git bundle\n{} refs/carry-export/rebuildable-omissions-v1\n\nPACK",
                "c".repeat(40)
            )
            .as_bytes(),
        );
        assert_eq!(omissions(&path, None), InBand::Unknown(VALUE_NOT_READ));
        // A holder without the object: unknown as well.
        assert_eq!(
            omissions(&path, Some(dir.path())),
            InBand::Unknown(VALUE_NOT_HELD)
        );
        let envelope = bundle(
            dir.path(),
            "e.bundle",
            format!(
                "# v2 git bundle\n{} refs/carry-export/shallow-custody-v1\n\nPACK",
                "d".repeat(40)
            )
            .as_bytes(),
        );
        assert_eq!(
            omissions(&envelope, None),
            InBand::Unknown(ENVELOPE_NOT_READ)
        );
        assert_eq!(
            omissions(&envelope, Some(dir.path())),
            InBand::Unknown(ENVELOPE_NOT_HELD)
        );
    }

    #[test]
    fn a_header_that_is_not_a_bundle_s_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        for body in [&b"not a bundle\n\n"[..], b"# v2 git bundle\nno blank line"] {
            let path = bundle(dir.path(), "x.bundle", body);
            assert_eq!(omissions(&path, None), InBand::Unknown(HEADER_UNREADABLE));
        }
        assert_eq!(
            omissions(&dir.path().join("absent.bundle"), None),
            InBand::Unknown(HEADER_UNREADABLE)
        );
    }
}
