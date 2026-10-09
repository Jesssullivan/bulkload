//! R25's strict reading (#169, OI-1003-Q40): adopt a durable output that has
//! no row, without reading its source again.
//!
//! A crash between an output's publish (its seal, rename and directory seal)
//! and its group's row commit, or a group whose store commit failed (#100),
//! leaves the output's bytes durable at the final path with no row. Before
//! #169 the next run asked the source for a manifest, which reads the seat
//! once more (`MC_r25_unrowed_no_adopt`).
//!
//! The capture record closes that gap. The destination writes on the output,
//! as an extended attribute, which capture its bytes are: a digest of the
//! walked row (the seat's path and stat identity, [`record_key`]), the
//! capture's manifest root and its size. Only a non-racy capture gets one: a
//! racy capture's stat identity cannot vouch for its bytes (#86). There are
//! two writers:
//!
//! - a staged file gets its record before its seal (`Inbound::publish`), so
//!   the group's file seal makes the record durable with the data (class
//!   meta, like the mode), and wherever a crash leaves the published bytes
//!   it leaves the record too (`materialize::adoption_power_loss`);
//! - an existing output adopted against a manifest gets, or has refreshed,
//!   its record once its bytes are verified (`Inbound::adopt`, [`refresh`]),
//!   and its row records the identity after that write. Its adoption's file
//!   seal makes the record durable before its row commits.
//!
//! The key holds no store authority. The walked row's device, inode, size
//! and times already name the seat, and the output's own hash proves the
//! bytes, so a source store that was recreated (a new authority) still finds
//! its outputs' records.
//!
//! On resume, an entry whose output exists with no matching row is adopted
//! when its record names this entry's row (the seat's stat identity has not
//! moved since that non-racy capture, so its content has not either), and
//! the output's own bytes, chunked as the source chunks and hashed, give the
//! recorded root and size under an unchanged identity and the row's mode.
//! The entry is then answered `Reuse`: the source reads nothing. The output
//! is queued as an adopted publication, so its file and directory are sealed
//! before its row commits, as for any adoption (#77 round 2, N4).
//!
//! What cannot be proven falls back to the manifest path of before, which
//! reads the seat:
//!
//! - counted `transfer_unrowed_unproven`: an existing output with no record
//!   (a racy capture's, one published before #169, one on a file system
//!   without extended attributes, one whose record could not be written, or
//!   a file bulkload did not publish) or with a record that does not verify
//!   (bytes rewritten in place, a changed mode);
//! - not counted: a record that names another row. The seat's row changed
//!   since that capture (its stat identity moved, or the walk now fills
//!   other row fields), and reading it again is required.
//!
//! A record that could not be written is counted when the write fails
//! (`transfer_capture_records_unset`): a file system without extended
//! attributes, or an adopted output whose mode gives its owner no write
//! permission (a `user.` attribute needs it; a staged file is still 0600
//! when its record is written).
//!
//! The record stays on the output after its row commits: removing it would
//! move the output's ctime, which is part of the identity its row records.
//!
//! **`SQLite` snapshot seats (#218, review R1).** A snapshot's bytes are not
//! the walked main file's: a WAL-mode commit moves only the `-wal`, so a
//! record keyed on the main file's row alone would let a stale snapshot be
//! adopted as current after a commit with no checkpoint. A snapshot output's
//! record is keyed in its own domain on the walked row **and** the settled
//! `-wal` identity ([`sqlite_record_key`]), and its size is the snapshot's.
//! Only a settled snapshot gets one (an unsettled capture is treated as a
//! racy one). [`prove`] compares a record against both keys, so a file's
//! record never proves a snapshot or the reverse.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};

use bulkload_proto::frame::{manifest_root, ChunkSpec};

use crate::counters::{self, Counter};
use crate::freshness::StatIdentity;
use crate::refuse::RefuseAt as _;
use crate::transfer_store::{row_key, ChunkHint};
use crate::{BulkloadRefusal, Result, RowSchema};

/// Encoding version of a capture record.
const RECORD_VERSION: u8 = 1;

/// A capture record's length: version, key digest, root and size.
const RECORD_BYTES: usize = 1 + 32 + 32 + 8;

/// Which capture a published output's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureRecord {
    /// BLAKE3 of the source-side row key ([`record_key`]).
    pub key: [u8; 32],
    /// The capture's manifest root.
    pub root: [u8; 32],
    /// The capture's size in bytes.
    pub size: u64,
}

impl CaptureRecord {
    const fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut bytes = [0_u8; RECORD_BYTES];
        let (version, rest) = bytes.split_at_mut(1);
        let (key, rest) = rest.split_at_mut(32);
        let (root, size) = rest.split_at_mut(32);
        version.copy_from_slice(&[RECORD_VERSION]);
        key.copy_from_slice(&self.key);
        root.copy_from_slice(&self.root);
        size.copy_from_slice(&self.size.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; RECORD_BYTES] = bytes.try_into().ok()?;
        let (version, rest) = bytes.split_first()?;
        if *version != RECORD_VERSION {
            return None;
        }
        let (key, rest) = rest.split_at(32);
        let (root, size) = rest.split_at(32);
        Some(Self {
            key: key.try_into().ok()?,
            root: root.try_into().ok()?,
            size: u64::from_le_bytes(size.try_into().ok()?),
        })
    }
}

/// Domain of a capture record's key, in the place a row key has a store
/// authority.
const RECORD_DOMAIN: &[u8] = b"bulkload capture record";

/// The capture record's key for a walked `row`: a digest of the row under
/// [`RECORD_DOMAIN`]. It names the seat by its path and stat identity alone,
/// so it is the same for every source store, session and destination root.
///
/// # Errors
/// Refuses a row that does not encode.
pub fn record_key(row: &RowSchema) -> Result<[u8; 32]> {
    Ok(crate::hash::hash_bytes(&row_key(RECORD_DOMAIN, row)?))
}

/// Domain of a snapshot seat's capture record key (#218): never a file's.
const SQLITE_RECORD_DOMAIN: &[u8] = b"bulkload sqlite snapshot capture record";

/// The capture record's key for a `SQLite` snapshot of the walked `row`
/// whose `-wal` had the identity `wal` when the capture settled (`None`:
/// no `-wal`). Like [`record_key`] it holds no store authority.
///
/// # Errors
/// Refuses a row that does not encode.
pub fn sqlite_record_key(
    row: &RowSchema,
    wal: Option<&bulkload_proto::frame::SidecarId>,
) -> Result<[u8; 32]> {
    let encoded = postcard::to_stdvec(&(SQLITE_RECORD_DOMAIN, row, wal))
        .refuse_at("unrowed::sqlite_record_key")?;
    Ok(crate::hash::hash_bytes(&encoded))
}

/// Write `record` on a staged file, before its seal. A file system without
/// extended attributes only costs the counted fallback on a later resume, so
/// a failure is counted (`transfer_capture_records_unset`), never refused.
pub fn write_record(file: &File, record: &CaptureRecord) {
    if crate::io::sys::set_capture_record(file, &record.encode()).is_err() {
        counters::bump(Counter::TransferCaptureRecordsUnset);
    }
}

/// The capture record on `file`, if it has a well-formed one.
pub fn read_record(file: &File) -> Option<CaptureRecord> {
    crate::io::sys::get_capture_record(file)
        .ok()
        .flatten()
        .and_then(|bytes| CaptureRecord::decode(&bytes))
}

/// Give an existing output, whose bytes were just verified against a
/// non-racy capture's manifest under the identity `verified`, that capture's
/// `record`, and return the identity its row must record.
///
/// An output that already carries exactly this record is left alone, so its
/// ctime does not move. Otherwise the record is written (replacing a stale
/// one), which moves the ctime: the identity is taken again, and it must
/// differ from `verified` in nothing else, or the file changed after its
/// bytes were checked. A record that cannot be written is counted
/// ([`write_record`]) and the output keeps its verified identity.
///
/// # Errors
/// Refuses an output that changed between its verification and its record.
pub fn refresh(
    file: &File,
    record: &CaptureRecord,
    verified: StatIdentity,
) -> Result<StatIdentity> {
    if read_record(file) == Some(*record) {
        return Ok(verified);
    }
    write_record(file, record);
    let after = StatIdentity::from_metadata(&file.metadata().refuse_at("unrowed::refresh")?);
    if (StatIdentity {
        ctime_ns: verified.ctime_ns,
        ..after
    }) == verified
    {
        Ok(after)
    } else {
        Err(BulkloadRefusal::DestinationOccupied)
    }
}

/// What an existing output with no matching row proves.
#[derive(Debug)]
pub enum Verdict {
    /// Its bytes are the capture a key names: the identity they were proven
    /// under, the first occurrence of each chunk, for the output row, and
    /// whether the key was the snapshot key ([`sqlite_record_key`]).
    Proven(StatIdentity, Vec<ChunkHint>, bool),
    /// Its record names another row: the seat's row changed since that
    /// capture, so read it again (not counted).
    Other,
    /// Nothing proves it: the manifest path of before (counted).
    Unproven,
}

/// Prove the existing output `file` is the non-racy capture that `key`
/// ([`record_key`]) names, or, given `snapshot`, the settled snapshot that
/// key ([`sqlite_record_key`]) names, reading only the destination. A file
/// capture's size must be the row's; a snapshot's is its record's.
pub fn prove(file: &File, row: &RowSchema, key: &[u8; 32], snapshot: Option<&[u8; 32]>) -> Verdict {
    let Some(record) = read_record(file) else {
        return Verdict::Unproven;
    };
    let is_snapshot = if record.key == *key {
        false
    } else if snapshot == Some(&record.key) {
        true
    } else {
        return Verdict::Other;
    };
    let expected = (!is_snapshot).then_some(row.size);
    match chunk_existing(file, row, record.size, expected) {
        Some((identity, specs, hints)) if manifest_root(&specs) == record.root => {
            Verdict::Proven(identity, hints, is_snapshot)
        }
        _ => Verdict::Unproven,
    }
}

/// Chunk an existing output as the source chunks a capture (`FastCDC` with
/// [`crate::hash`]'s bounds, BLAKE3 per chunk), through `pread`, checking
/// its size (against `expected`, when given) and mode against the row and
/// its identity before and after.
fn chunk_existing(
    file: &File,
    row: &RowSchema,
    size: u64,
    expected: Option<u64>,
) -> Option<(StatIdentity, Vec<ChunkSpec>, Vec<ChunkHint>)> {
    let before = file.metadata().ok()?;
    if expected.is_some_and(|expected| expected != size)
        || before.len() != size
        || before.mode() & 0o7777 != row.mode & 0o7777
    {
        return None;
    }
    let identity = StatIdentity::from_metadata(&before);
    let mut specs = Vec::new();
    let mut hints = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut offset = 0_u64;
    for chunk in fastcdc::v2020::StreamCDC::new(
        Pread { file, offset: 0 },
        crate::hash::CDC_MIN_BYTES,
        crate::hash::CDC_AVG_BYTES,
        crate::hash::CDC_MAX_BYTES,
    ) {
        let chunk = chunk.ok()?;
        if specs.len() >= super::MAX_MANIFEST_CHUNKS {
            return None;
        }
        counters::add_len(Counter::DestVerifyRead, chunk.data.len());
        let digest = counters::hash(Counter::HashVerifyExisting, &chunk.data);
        let length = chunk.data.len() as u64;
        specs.push(ChunkSpec {
            digest,
            size: length,
        });
        if seen.insert(digest) {
            hints.push(ChunkHint {
                digest,
                offset,
                size: length,
            });
        }
        offset = offset.saturating_add(length);
    }
    let after = StatIdentity::from_metadata(&file.metadata().ok()?);
    (offset == size && after == identity).then_some((identity, specs, hints))
}

/// Sequential reads of an output through `pread`, leaving its offset alone.
struct Pread<'a> {
    file: &'a File,
    offset: u64,
}

impl Read for Pread<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buffer, self.offset)?;
        self.offset = self.offset.saturating_add(read as u64);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn a_capture_record_round_trips_and_refuses_other_shapes() {
        let record = CaptureRecord {
            key: [7; 32],
            root: [9; 32],
            size: 123_456,
        };
        let bytes = record.encode();
        assert_eq!(CaptureRecord::decode(&bytes), Some(record));
        assert_eq!(CaptureRecord::decode(&bytes[..RECORD_BYTES - 1]), None);
        let mut other = bytes;
        other[0] = RECORD_VERSION + 1;
        assert_eq!(CaptureRecord::decode(&other), None);
    }
}
