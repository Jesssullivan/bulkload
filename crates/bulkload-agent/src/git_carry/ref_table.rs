//! The v1 ref table (OI-1003-Q54, #178; `docs/plans/2026-10-05-v1-header.md`).
//!
//! A v1 capture bundle used to list every carried ref in its header, one
//! `<oid> refs/carry-export/<name>` line each, so its 16 MiB header cap fell
//! at about 97k carry-shaped refs and a refs-heavy repository was refused on
//! every pass. The capture's private repository now holds, in place of one
//! ref per carried ref:
//!
//! - [`TABLE_REF`], a parentless archival commit whose tree is the carried
//!   map, content-addressed: a `native` blob of `<oid> <name>` lines for
//!   every ref outside a canonical carry namespace (`name` relative to
//!   `refs/carry-export/`), and one blob of `<oid> <suffix>` lines per
//!   namespace `refs/carry/v1/<slug>/<digest>/`, at
//!   `union/<slug>/<digest>/carry-namespace-refs` ([`NAMESPACE_BLOB`]).
//!   Lines are sorted and unique, so equal maps are equal objects.
//! - one ref per distinct object the map names, under [`TIP_PREFIX`] and
//!   named by that object, so `--all` reaches exactly what the per-ref refs
//!   reached, and the edge-aggressive walk sees the same ref tips (P64).
//!
//! The header then grows with the distinct objects, not the refs. At apply,
//! [`expand`] rebuilds the old format's header lines byte for byte, so the
//! import that follows is the one it always was. A header without
//! [`TABLE_REF`] is the old format and passes through untouched.

use crate::refuse::RefuseAt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use super::{
    batch_objects::BatchObjects, canonical_tail, git, git_writer, input, oid, output, source_slug,
    ARCHIVAL_SIGNATURE,
};
use crate::{BulkloadRefusal, Result};

/// The private ref naming a capture's ref table.
pub(super) const TABLE_REF: &str = "refs/carry-export/ref-table-v1";
/// The private refs naming each distinct object the table maps a ref to.
pub(super) const TIP_PREFIX: &str = "refs/carry-export/ref-tip-v1/";
/// Every carried name lives under this prefix.
const EXPORT: &str = "refs/carry-export/";
/// Exported names of canonical carry namespaces.
const UNION: &str = "refs/carry-export/union/v1/";
/// The table's blob of refs outside every canonical carry namespace.
const NATIVE: &str = "native";
/// Each namespace blob's own name. Every namespace blob's path ends in it,
/// so pack-objects' name hash (the path's last 16 bytes) sorts them together
/// and the near-identical snapshot namespaces delta against each other.
pub(super) const NAMESPACE_BLOB: &str = "carry-namespace-refs";
const MESSAGE: &str = "bulkload ref table v1\n";
/// The most raw bytes one ref table may hold, written or read: about 1.3M
/// carry-shaped refs. Past it the inventory refuses `GIT_INVENTORY_OVER_CAP`.
pub(super) const TABLE_CAP: u64 = 256 * 1024 * 1024;

/// A ref table's blobs, before they are written.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Encoded {
    native: Vec<u8>,
    namespaces: BTreeMap<(String, String), Vec<u8>>,
}

impl Encoded {
    fn len(&self) -> u64 {
        self.namespaces
            .values()
            .fold(self.native.len(), |total, blob| {
                total.saturating_add(blob.len())
            })
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

/// Split `refs` (exported name to object name, as `capture_refs` maps an
/// inventory) into the table's blobs.
///
/// # Errors
/// `GIT_INVENTORY_MALFORMED` for a name no capture exports, and
/// `GIT_INVENTORY_OVER_CAP` for a table over [`TABLE_CAP`].
pub(super) fn encode(refs: &BTreeMap<String, String>) -> Result<Encoded> {
    let mut encoded = Encoded::default();
    // `refs` iterates by full name, so each blob's lines come out sorted by
    // the part the blob keeps: a shared prefix does not change the order.
    for (name, value) in refs {
        if !oid(value) || name.contains([' ', '\n', '\0']) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let line = |blob: &mut Vec<u8>, rest: &str| {
            blob.extend_from_slice(value.as_bytes());
            blob.push(b' ');
            blob.extend_from_slice(rest.as_bytes());
            blob.push(b'\n');
        };
        if let Some(tail) = name.strip_prefix(UNION) {
            let (namespace, suffix) = namespace_of(tail)?;
            line(encoded.namespaces.entry(namespace).or_default(), suffix);
        } else if let Some(rest) = name.strip_prefix(EXPORT).filter(|rest| native_name(rest)) {
            line(&mut encoded.native, rest);
        } else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    if encoded.len() > TABLE_CAP {
        return Err(BulkloadRefusal::GitInventoryOverCap);
    }
    Ok(encoded)
}

// A canonical tail's namespace (slug, digest) and its suffix.
fn namespace_of(tail: &str) -> Result<((String, String), &str)> {
    if !canonical_tail(tail) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut parts = tail.splitn(3, '/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(slug), Some(digest), Some(suffix)) => {
            Ok(((slug.to_owned(), digest.to_owned()), suffix))
        }
        _ => Err(BulkloadRefusal::GitInventoryMalformed),
    }
}

// What `capture_refs` exports outside a canonical namespace, relative to
// `refs/carry-export/`: a source ref (always `refs/...`) or a stash commit.
fn native_name(rest: &str) -> bool {
    rest.strip_prefix("refs/")
        .or_else(|| rest.strip_prefix("stashes/"))
        .is_some_and(|tail| !tail.is_empty())
}

/// Write `refs` into the private repository as its ref table and tip refs.
///
/// The table's objects go to the write store through one `fast-import`
/// (S2, #162: the writer sees no other store), which also creates
/// [`TABLE_REF`]; the tips are one create-only `update-ref` transaction.
///
/// # Errors
/// Whatever [`encode`] refuses, and failed Git children.
pub(super) fn write(private: &Path, refs: &BTreeMap<String, String>) -> Result<()> {
    let encoded = encode(refs)?;
    if git(private)
        .args(["show-ref", "--verify", "--quiet", TABLE_REF])
        .status()
        .refuse_at("git_carry::ref_table::write")?
        .success()
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    input(
        git_writer(private).args(["fast-import", "--quiet", "--done"]),
        &import_stream(&encoded),
    )?;
    let tips: BTreeSet<&String> = refs.values().collect();
    let mut commands = Vec::new();
    for value in tips {
        commands.extend_from_slice(b"create ");
        commands.extend_from_slice(TIP_PREFIX.as_bytes());
        commands.extend_from_slice(value.as_bytes());
        commands.push(0);
        commands.extend_from_slice(value.as_bytes());
        commands.push(0);
    }
    if !commands.is_empty() {
        input(
            git(private).args(["update-ref", "--stdin", "-z"]),
            &commands,
        )?;
    }
    Ok(())
}

// The `fast-import` stream of one table commit: the archival signature and
// instant, every blob inline, then `done`.
fn import_stream(encoded: &Encoded) -> Vec<u8> {
    fn blob(stream: &mut Vec<u8>, path: &str, bytes: &[u8]) {
        stream.extend_from_slice(
            format!("M 100644 inline {path}\ndata {}\n", bytes.len()).as_bytes(),
        );
        stream.extend_from_slice(bytes);
        stream.push(b'\n');
    }
    let mut stream = Vec::with_capacity(
        usize::try_from(encoded.len())
            .unwrap_or(0)
            .saturating_add(4096),
    );
    stream.extend_from_slice(
        format!(
            "commit {TABLE_REF}\nauthor {ARCHIVAL_SIGNATURE}\ncommitter {ARCHIVAL_SIGNATURE}\ndata {}\n{MESSAGE}\n",
            MESSAGE.len()
        )
        .as_bytes(),
    );
    blob(&mut stream, NATIVE, &encoded.native);
    for ((slug, digest), bytes) in &encoded.namespaces {
        blob(
            &mut stream,
            &format!("union/{slug}/{digest}/{NAMESPACE_BLOB}"),
            bytes,
        );
    }
    stream.extend_from_slice(b"done\n");
    stream
}

/// Whether a header names the ref table (`<oid> <name>` lines).
pub(super) fn is_table(name: &str) -> bool {
    name == TABLE_REF
}

/// The old format's header lines for a bundle whose header (or a shallow
/// envelope's inner inventory) is `heads`, read from `repository`, which
/// already holds the bundle's objects. `None` when `heads` names no ref
/// table: the old format, used as it is.
///
/// The result is every capture metadata line of `heads`, then every line the
/// table maps, as `<oid> refs/carry-export/<name>`: exactly the header the
/// old format wrote for the same capture, in another order.
///
/// # Errors
/// `GIT_INVENTORY_MALFORMED` for a table that is not exactly a capture's: a
/// tip without a table, a tip naming another object, a tip the table does
/// not use or a table line with no tip, a header ref that is neither capture
/// metadata, table nor tip, an entry the table layout does not have, or
/// lines out of order or repeated. `GIT_INVENTORY_OVER_CAP` for a table over
/// [`TABLE_CAP`]; failed Git children.
pub(super) fn expand(repository: &Path, heads: &str) -> Result<Option<String>> {
    let mut table = None;
    let mut tips = BTreeSet::new();
    let mut metadata = Vec::new();
    for line in heads.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if is_table(name) {
            if table.replace(value).is_some() {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
        } else if let Some(tip) = name.strip_prefix(TIP_PREFIX) {
            if tip != value || !tips.insert(value) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
        } else {
            metadata.push(line);
        }
    }
    let Some(table) = table else {
        return if tips.is_empty() {
            Ok(None)
        } else {
            Err(BulkloadRefusal::GitInventoryMalformed)
        };
    };
    // Beside a table, a header carries only capture metadata: one component
    // under the export prefix. Every carried ref is in the table.
    for line in &metadata {
        let rest = line
            .split_once(' ')
            .and_then(|(_, name)| name.strip_prefix(EXPORT))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if rest.is_empty() || rest.contains('/') {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let entries = read(repository, table)?;
    let used: BTreeSet<&str> = entries.iter().map(|(value, _)| value.as_str()).collect();
    if used != tips {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut expanded = String::new();
    for line in metadata {
        expanded.push_str(line);
        expanded.push('\n');
    }
    for (value, name) in &entries {
        writeln!(expanded, "{value} {name}").map_err(|_| BulkloadRefusal::FrameCodec)?;
    }
    Ok(Some(expanded))
}

// Every (object, exported name) pair the table at `table` maps, from its tree.
fn read(repository: &Path, table: &str) -> Result<Vec<(String, String)>> {
    let listing =
        output(git(repository).args(["ls-tree", "-r", "-z", &format!("{table}^{{commit}}")]))?;
    let mut blobs = Vec::new();
    let mut native = false;
    for entry in listing.split(|byte| *byte == 0).filter(|e| !e.is_empty()) {
        let entry =
            std::str::from_utf8(entry).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let (header, path) = entry
            .split_once('\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split(' ');
        let (Some("100644"), Some("blob"), Some(object), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        if !oid(object) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let prefix = if path == NATIVE {
            if std::mem::replace(&mut native, true) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            None
        } else {
            let namespace = path
                .strip_prefix("union/")
                .and_then(|rest| rest.strip_suffix(NAMESPACE_BLOB))
                .and_then(|rest| rest.strip_suffix('/'))
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            let (slug, digest) = namespace
                .split_once('/')
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            if !source_slug(slug) || digest.len() != 64 || !oid(digest) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            Some(format!("{UNION}{namespace}/"))
        };
        blobs.push((object.to_owned(), prefix));
    }
    if !native {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut reader = BatchObjects::new(repository)?;
    let mut remaining = TABLE_CAP;
    let mut entries = Vec::new();
    for (object, prefix) in blobs {
        let mut bytes = Vec::new();
        let length = reader
            .copy_into(&object, &mut bytes, Some(remaining))
            .map_err(|error| match error {
                BulkloadRefusal::BudgetExceeded => BulkloadRefusal::GitInventoryOverCap,
                other => other,
            })?;
        remaining = remaining.saturating_sub(length);
        decode(&bytes, prefix.as_deref(), &mut entries)?;
    }
    reader.finish()?;
    Ok(entries)
}

// One blob's lines, each `<oid> <rest>`, strictly ascending by `rest`. A
// namespace blob's rest is a suffix under `prefix`; the native blob's is a
// name under the export prefix.
fn decode(bytes: &[u8], prefix: Option<&str>, entries: &mut Vec<(String, String)>) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut previous: Option<&str> = None;
    for line in text.lines() {
        let (value, rest) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value)
            || rest.is_empty()
            || rest.contains([' ', '\0', '\r'])
            || previous.is_some_and(|previous| previous >= rest)
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        previous = Some(rest);
        let name = match prefix {
            Some(prefix) => {
                let name = format!("{prefix}{rest}");
                if !name.strip_prefix(UNION).is_some_and(canonical_tail) {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
                name
            }
            None if native_name(rest) => format!("{EXPORT}{rest}"),
            None => return Err(BulkloadRefusal::GitInventoryMalformed),
        };
        entries.push((value.to_owned(), name));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn encode_groups_namespaces_and_keeps_native_names_relative() {
        let digest = "a".repeat(64);
        let encoded = encode(&map(&[
            ("refs/carry-export/refs/heads/main", A),
            (
                "refs/carry-export/stashes/2222222222222222222222222222222222222222",
                B,
            ),
            (
                &format!("refs/carry-export/union/v1/neo/{digest}/refs/heads/x"),
                A,
            ),
            (&format!("refs/carry-export/union/v1/neo/{digest}/head"), B),
        ]))
        .unwrap();
        assert_eq!(
            encoded.native,
            format!("{A} refs/heads/main\n{B} stashes/{B}\n").into_bytes()
        );
        assert_eq!(
            encoded.namespaces[&("neo".to_owned(), digest)],
            format!("{B} head\n{A} refs/heads/x\n").into_bytes()
        );
        let mut entries = Vec::new();
        decode(&encoded.native, None, &mut entries).unwrap();
        assert_eq!(
            entries,
            vec![
                (A.to_owned(), "refs/carry-export/refs/heads/main".to_owned()),
                (B.to_owned(), format!("refs/carry-export/stashes/{B}")),
            ]
        );
    }

    #[test]
    fn encode_refuses_names_no_capture_exports() {
        for name in [
            "refs/carry-export/head",
            "refs/carry-export/ref-table-v1",
            "refs/heads/main",
            "refs/carry-export/refs/",
            "refs/carry-export/union/v1/neo/short/refs/heads/x",
            "refs/carry-export/refs/heads/has space",
        ] {
            assert_eq!(
                encode(&map(&[(name, A)])),
                Err(BulkloadRefusal::GitInventoryMalformed),
                "{name}"
            );
        }
    }

    #[test]
    fn decode_refuses_unsorted_repeated_and_foreign_lines() {
        let mut entries = Vec::new();
        for blob in [
            format!("{A} refs/heads/b\n{A} refs/heads/a\n"),
            format!("{A} refs/heads/a\n{A} refs/heads/a\n"),
            format!("{A} refs/heads/a"),
            format!("{A} head\n"),
            format!("{A}\n"),
            format!("{} refs/heads/a\n", "z".repeat(40)),
        ] {
            assert_eq!(
                decode(blob.as_bytes(), None, &mut entries),
                Err(BulkloadRefusal::GitInventoryMalformed),
                "{blob}"
            );
        }
        let namespace = format!("{UNION}neo/{}/", "b".repeat(64));
        decode(
            format!("{A} head\n").as_bytes(),
            Some(&namespace),
            &mut entries,
        )
        .unwrap();
        assert_eq!(entries.last().unwrap().1, format!("{namespace}head"));
    }

    #[test]
    fn expand_refuses_a_header_that_is_not_exactly_a_capture() {
        let scratch =
            std::env::temp_dir().join(format!("bulkload-ref-table-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir(&scratch).unwrap();
        // Header-only checks refuse before any object is read.
        for heads in [
            format!("{A} {TIP_PREFIX}{A}\n"),
            format!("{A} {TABLE_REF}\n{B} {TABLE_REF}\n"),
            format!("{A} {TABLE_REF}\n{B} {TIP_PREFIX}{A}\n"),
            format!("{A} {TABLE_REF}\n{B} refs/carry-export/refs/heads/main\n"),
            format!("{A} {TABLE_REF}\n{B} refs/heads/main\n"),
        ] {
            assert_eq!(
                expand(&scratch, &heads),
                Err(BulkloadRefusal::GitInventoryMalformed),
                "{heads}"
            );
        }
        assert_eq!(
            expand(&scratch, &format!("{A} refs/carry-export/head\n")),
            Ok(None)
        );
        std::fs::remove_dir_all(&scratch).unwrap();
    }
}
