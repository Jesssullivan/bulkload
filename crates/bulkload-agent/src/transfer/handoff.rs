//! The loopback hand-off between the two halves of one `copy`
//! (OI-1003-Q154, levers 1 and 2).
//!
//! A local `copy` runs the source half ([`super::serve`]) and the
//! destination half ([`super::receive_with`]) in one process, from one
//! build, over a socket pair that `copy` itself creates. Only there does a
//! [`pair`] exist: a [`HandOut`] for the source and a [`HandIn`] for the
//! destination, joined by an in-process queue. With them:
//!
//! - **Direct buffer hand-off (lever 2).** A chunk's bytes never cross the
//!   socket. The source moves the chunk's own allocation into the queue and
//!   writes only its 64-byte header on the socket, as a handed frame
//!   ([`TAG_HANDED`]); the destination takes the allocation from the queue
//!   when it reads that header. The socket keeps the stream's order, so
//!   the frame's position among the control frames is the one a data frame
//!   would have had. A handed frame is no wire v6 frame: its tag is outside
//!   the codec's, so any reader without a [`HandIn`] (`pull`, a `receive`
//!   over any transport, the proto codec) refuses it `FRAME_CODEC`.
//! - **Verify once (lever 1).** A [`SourceChunk`] can only be made by
//!   hashing its bytes ([`SourceChunk::hashed`]) or by checking them
//!   against a digest ([`SourceChunk::checked`]), and its bytes sit behind
//!   an [`Arc`] that nothing can write through. So a handed chunk whose
//!   digest equals its frame's is the very allocation the source hashed to
//!   that digest, unchanged: the destination writes it without hashing it a
//!   second time ([`Payload::vouched`]). Bytes that arrive over a
//!   transport ([`Payload::Wire`]) are never vouched for: every one of them
//!   is hashed by the destination, as before.
//!
//! The capability is unforgeable from outside the process. [`pair`] is
//! private to the transfer module and called only by `copy`; the public
//! `serve`, `receive` and `receive_with` take no hand and never make one;
//! neither half can be serialised, so nothing that arrives over a pipe,
//! a socket or `ssh` can stand for one. Cross-process and cross-host
//! sessions therefore verify every byte (`tests/loopback_handoff.rs`, and
//! the mutation proof in `transfer/tests/levers.rs`).

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use bulkload_proto::frame::{DataHeader, DATA_HEADER_BYTES, FRAME_HEADER_BYTES, MAX_DATA_PAYLOAD};

use crate::counters::{self, Counter};
use crate::{BulkloadRefusal, Result};

/// The tag of a handed frame: a [`DataHeader`] whose payload was handed
/// over in process. Never a wire v6 tag (v6 has 0x01 to 0x03), so a v6
/// reader refuses it.
pub(super) const TAG_HANDED: u8 = 0xB1;

/// The body length of a handed frame: its tag and its data header.
pub(super) const HANDED_BODY_BYTES: usize = 1 + DATA_HEADER_BYTES;

/// One source chunk: its bytes and the BLAKE3 digest the source computed
/// over exactly these bytes. The fields are private to this module, and
/// both constructors hash, so a value always carries its bytes' digest.
#[derive(Clone)]
pub(super) struct SourceChunk {
    digest: [u8; 32],
    data: Arc<Vec<u8>>,
}

impl SourceChunk {
    /// Hash freshly captured bytes (counted as `blake3_capture_chunk_bytes`).
    pub(super) fn hashed(data: Vec<u8>) -> Self {
        let digest = counters::hash(Counter::HashCaptureChunk, &data);
        Self {
            digest,
            data: Arc::new(data),
        }
    }

    /// Bytes read again for a manifest's chunk request, kept only when they
    /// hash to `digest` (counted as `blake3_capture_chunk_bytes`).
    pub(super) fn checked(data: Vec<u8>, digest: &[u8; 32]) -> Option<Self> {
        let found = Self::hashed(data);
        (found.digest == *digest).then_some(found)
    }

    /// The digest the source computed over [`Self::bytes`].
    pub(super) const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// The chunk's bytes.
    pub(super) fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The chunk's length in bytes.
    pub(super) fn len(&self) -> usize {
        self.data.len()
    }
}

/// A received chunk's bytes: read from the transport, or handed over in
/// process.
pub(super) enum Payload {
    /// Bytes read from the transport. The destination hashes them.
    Wire(Vec<u8>),
    /// A chunk the in-process source handed over with its frame.
    Handed(SourceChunk),
}

impl Payload {
    /// The payload's bytes.
    pub(super) fn bytes(&self) -> &[u8] {
        match self {
            Self::Wire(data) => data,
            Self::Handed(chunk) => chunk.bytes(),
        }
    }

    /// The payload's length in bytes.
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Wire(data) => data.len(),
            Self::Handed(chunk) => chunk.len(),
        }
    }

    /// Whether the source already hashed these exact bytes to `digest`:
    /// only a handed chunk whose own digest is `digest`. Then the
    /// destination need not hash them again (verify once, OI-1003-Q154
    /// lever 1). Bytes read from a transport never are.
    pub(super) fn vouched(&self, digest: &[u8; 32]) -> bool {
        match self {
            Self::Wire(_) => false,
            Self::Handed(chunk) => chunk.digest() == digest,
        }
    }
}

/// The source half of the hand-off: hands each chunk's allocation over.
pub(super) struct HandOut {
    chunks: Sender<SourceChunk>,
}

/// The destination half of the hand-off: takes each handed chunk, in the
/// order the source handed them.
pub(super) struct HandIn {
    chunks: Receiver<SourceChunk>,
}

/// A joined hand-off for one in-process session. Only `copy` makes one.
pub(super) fn pair() -> (HandOut, HandIn) {
    let (chunks, taken) = std::sync::mpsc::channel();
    (HandOut { chunks }, HandIn { chunks: taken })
}

impl HandOut {
    /// Hand `chunk` over. The caller then writes its frame: the chunk is in
    /// the queue before the destination can read the frame that names it.
    ///
    /// # Errors
    /// `WORKER_LOST` once the destination half is gone.
    pub(super) fn give(&self, chunk: SourceChunk) -> Result<()> {
        self.chunks
            .send(chunk)
            .map_err(|_| BulkloadRefusal::WorkerLost)
    }
}

impl HandIn {
    /// The chunk a handed frame with `header` names: the next one handed.
    ///
    /// # Errors
    /// `PROTOCOL_STATE_VIOLATION` when the source half is gone with nothing
    /// handed, `FRAME_CODEC` when the chunk's length is not the header's.
    pub(super) fn take(&self, header: &DataHeader) -> Result<SourceChunk> {
        let chunk = self
            .chunks
            .recv()
            .map_err(|_| BulkloadRefusal::ProtocolStateViolation)?;
        if usize::try_from(header.size).ok() != Some(chunk.len()) {
            return Err(BulkloadRefusal::FrameCodec);
        }
        counters::add_len(Counter::WireHandedBytes, chunk.len());
        Ok(chunk)
    }
}

/// The frame header and data header of a handed frame: the chunk's bytes
/// are not on the socket.
///
/// # Errors
/// `BUDGET_EXCEEDED` for a chunk over `MAX_DATA_PAYLOAD`.
pub(super) fn handed_prefix(
    header: &DataHeader,
) -> Result<[u8; FRAME_HEADER_BYTES + DATA_HEADER_BYTES]> {
    if !usize::try_from(header.size).is_ok_and(|size| size <= MAX_DATA_PAYLOAD) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let length = u32::try_from(HANDED_BODY_BYTES).map_err(|_| BulkloadRefusal::FrameCodec)?;
    let mut out = [0_u8; FRAME_HEADER_BYTES + DATA_HEADER_BYTES];
    out.get_mut(..FRAME_HEADER_BYTES - 1)
        .ok_or(BulkloadRefusal::FrameCodec)?
        .copy_from_slice(&length.to_be_bytes());
    *out.get_mut(FRAME_HEADER_BYTES - 1)
        .ok_or(BulkloadRefusal::FrameCodec)? = TAG_HANDED;
    out.get_mut(FRAME_HEADER_BYTES..)
        .ok_or(BulkloadRefusal::FrameCodec)?
        .copy_from_slice(&header.to_bytes());
    Ok(out)
}
