//! R25's strict reading (#169, OI-1003-Q40): adopt a durable output that has
//! no row, without reading its source again.
//!
//! A crash between an output's publish (its seal, rename and directory seal)
//! and its group's row commit, or a group whose store commit failed (#100),
//! leaves the output's bytes durable at the final path with no row. Before
//! #169 the next run asked the source for a manifest, which reads the seat
//! once more (`MC_r25_unrowed_bytes`).
//!
//! The capture record closes that gap. Before a staged file is sealed, the
//! destination writes on it, as an extended attribute, which capture its
//! bytes are: a digest of the source-side row key (the source store's
//! authority from `Start` and the walked row, stat identity included), the
//! capture's manifest root and its size. Only a non-racy capture gets one: a
//! racy capture's stat identity cannot vouch for its bytes (#86). The group's
//! file seal makes the record durable with the data (class meta, like the
//! mode), so wherever a crash leaves the published bytes, it leaves the
//! record too (`materialize::adoption_power_loss`).
//!
//! On resume, an entry whose output exists with no matching row is adopted
//! when its record names this entry's row key (the seat's stat identity has
//! not moved since that non-racy capture, so its content has not either),
//! and the output's own bytes, chunked as the source chunks and hashed, give
//! the recorded root and size under an unchanged identity and the row's
//! mode. The entry is then answered `Reuse`: the source reads nothing. The
//! output is queued as an adopted publication, so its file and directory are
//! sealed before its row commits, as for any adoption (#77 round 2, N4).
//!
//! What cannot be proven falls back to the manifest path of before and is
//! counted on the counters line (`transfer_unrowed_unproven`): an existing
//! output with no record (a racy capture's, one published before #169, one
//! on a file system without extended attributes, or a file bulkload did not
//! publish) or with a record that does not verify (bytes rewritten in place,
//! a changed mode). A record of another row key is not counted: the seat
//! moved since that capture, and reading it again is required. A staged file
//! whose record could not be written is counted when it is written
//! (`transfer_capture_records_unset`).
//!
//! The record stays on the output after its row commits: removing it would
//! move the output's ctime, which is part of the identity its row records.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};

use bulkload_proto::frame::{manifest_root, ChunkSpec};

use crate::counters::{self, Counter};
use crate::freshness::StatIdentity;
use crate::transfer_store::{row_key, ChunkHint};
use crate::{Result, RowSchema};

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

/// The capture record's key for `row` walked under the source store's
/// `authority` (from `Start`): a digest of [`row_key`] over them, so it is
/// the same wherever the destination root lives.
///
/// # Errors
/// Refuses a row that does not encode.
pub fn record_key(authority: &[u8], row: &RowSchema) -> Result<[u8; 32]> {
    Ok(crate::hash::hash_bytes(&row_key(authority, row)?))
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

/// What an existing output with no matching row proves.
#[derive(Debug)]
pub enum Verdict {
    /// Its bytes are the capture `key` names: the identity they were proven
    /// under, and the first occurrence of each chunk, for the output row.
    Proven(StatIdentity, Vec<ChunkHint>),
    /// Its record names another capture of the seat: read it again.
    Other,
    /// Nothing proves it: the manifest path of before (counted).
    Unproven,
}

/// Prove the existing output `file` is the non-racy capture that `key`
/// ([`record_key`]) names, reading only the destination.
pub fn prove(file: &File, row: &RowSchema, key: &[u8; 32]) -> Verdict {
    let Some(record) = read_record(file) else {
        return Verdict::Unproven;
    };
    if record.key != *key {
        return Verdict::Other;
    }
    match chunk_existing(file, row, record.size) {
        Some((identity, specs, hints)) if manifest_root(&specs) == record.root => {
            Verdict::Proven(identity, hints)
        }
        _ => Verdict::Unproven,
    }
}

/// Chunk an existing output as the source chunks a capture (`FastCDC` with
/// [`crate::hash`]'s bounds, BLAKE3 per chunk), through `pread`, checking
/// its size and mode against the row and its identity before and after.
fn chunk_existing(
    file: &File,
    row: &RowSchema,
    size: u64,
) -> Option<(StatIdentity, Vec<ChunkSpec>, Vec<ChunkHint>)> {
    let before = file.metadata().ok()?;
    if size != row.size || before.len() != size || before.mode() & 0o7777 != row.mode & 0o7777 {
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
