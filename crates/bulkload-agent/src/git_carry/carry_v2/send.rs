//! Packing one segment into a sink.

use std::io::{Read, Write};

use super::{pinned, run_child, Outcome, PackPlan, Source};
use crate::BulkloadRefusal;

/// What one segment put on the wire. The digest is of the bytes as sent;
/// resume never requires a regenerated segment to match it (spike D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentReceipt {
    /// Segment index within its plan.
    pub index: usize,
    /// Objects, from the pack header (equal to the segment's list lines).
    pub objects: u32,
    /// Pack bytes: header, entries and trailer.
    pub bytes: u64,
    /// BLAKE3 of those bytes.
    pub blake3: String,
}

impl SegmentReceipt {
    /// `key=value` receipt lines, with the git build and pins the bytes
    /// depend on (spike plan change 6).
    #[must_use]
    pub fn lines(&self, pack_id: &str, git_build: &str) -> Vec<String> {
        vec![
            format!("pack_id={pack_id}"),
            format!("segment={}", self.index),
            format!("segment_objects={}", self.objects),
            format!("segment_bytes={}", self.bytes),
            format!("segment_blake3={}", self.blake3),
            format!("git_build={git_build}"),
            format!("pack_pins={}", super::PACK_PINS),
        ]
    }
}

impl PackPlan {
    /// Pack segment `index` from `source` and stream it into `sink`, as
    /// `pack-objects --stdout --delta-base-offset` in list mode fed the edge
    /// lines and the segment's object lines under the M1 pins. No `--thin`:
    /// in list mode it would read the edge lines as revisions; the edge
    /// lines make the pack thin (spike Q1 caveat 1). No `--missing`: an
    /// object that vanished since the list was made refuses here instead of
    /// being left out.
    ///
    /// # Errors
    /// Refuses an index past the last segment (`FIELD_DOMAIN_VIOLATION`), a
    /// pack whose header does not count exactly the segment's objects
    /// (`CONTRACT_SELF_INCONSISTENT`), a failing child (classified stderr),
    /// and any sink failure.
    pub fn send_segment(
        &self,
        source: &Source,
        index: usize,
        sink: &mut dyn Write,
        store: Option<&super::StderrStore>,
    ) -> Outcome<SegmentReceipt> {
        let (Some(input), Some(expected)) =
            (self.segment_input(index), self.segment_objects(index))
        else {
            return Err(BulkloadRefusal::FieldDomainViolation.into());
        };
        // The header is judged only once the child's status is known, so a
        // failing pack-objects refuses with its stderr class, not as a short
        // stream; a read failure here is the sink's or the pipe's.
        let (bytes, header, digest) = run_child(
            pinned(source).args(["pack-objects", "--stdout", "--delta-base-offset", "-q"]),
            &input,
            store,
            "pack_objects_failed",
            |stdout| stream(stdout, sink),
        )?;
        let [b'P', b'A', b'C', b'K', 0, 0, 0, 2, a, b, c, d] = header else {
            return Err(BulkloadRefusal::GitInventoryMalformed.into());
        };
        let objects = u32::from_be_bytes([a, b, c, d]);
        if usize::try_from(objects).ok() != Some(expected) {
            return Err(BulkloadRefusal::ContractSelfInconsistent.into());
        }
        Ok(SegmentReceipt {
            index,
            objects,
            bytes,
            blake3: digest,
        })
    }
}

/// Copy a pack stream into `sink` through one reused buffer, hashing it and
/// keeping its first 12 bytes (the header; zeros past a shorter stream).
fn stream(stdout: &mut dyn Read, sink: &mut dyn Write) -> crate::Result<(u64, [u8; 12], String)> {
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut header = [0_u8; 12];
    let mut seen = 0_usize;
    let mut bytes = 0_u64;
    let mut hasher = blake3::Hasher::new();
    loop {
        let read = match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        let chunk = buffer.get(..read).ok_or(BulkloadRefusal::Io(None))?;
        if seen < header.len() {
            let take = (header.len() - seen).min(chunk.len());
            let (Some(to), Some(from)) = (header.get_mut(seen..seen + take), chunk.get(..take))
            else {
                return Err(BulkloadRefusal::Io(None));
            };
            to.copy_from_slice(from);
            seen += take;
        }
        hasher.update(chunk);
        sink.write_all(chunk)?;
        bytes += u64::try_from(read).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
    }
    sink.flush()?;
    Ok((bytes, header, hasher.finalize().to_hex().to_string()))
}
