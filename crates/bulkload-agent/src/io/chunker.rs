//! Fused slice-FastCDC v2020 and per-chunk BLAKE3 (plan D1 "Source", D3
//! `chunker.rs`).
//!
//! Each chunk is cut and then hashed immediately, while its bytes are still in
//! L1/L2, so a byte is read from memory once for both jobs. The cut is
//! `fastcdc::v2020::cut` over the slice, with the parameters the agent already
//! uses (`hash::CDC_*`: 16/64/256 KiB, normalization level 1, the crate's
//! default gear table), so the boundaries are bit-identical to
//! `fastcdc::v2020::FastCDC` and to `hash::chunk_boundaries`.
//!
//! # Speculative segments
//!
//! `FastCDC`'s cut at position `p` depends only on the bytes from `p` to
//! `p + max` (and on the end of the input). So the chain of cuts started at
//! any position is deterministic, and two chains that ever share a cut point
//! agree from then on. [`chunk_segmented`] exploits that: it splits the input
//! into fixed segments, chunks every segment from its own start in parallel
//! (a *speculative* chain), and then stitches sequentially. The stitcher
//! follows the true chain from the end of the previous segment. As soon as
//! the true position equals a speculative cut, it adopts the rest of that
//! segment's chunks and digests as they are. Otherwise it re-chunks and
//! re-hashes one chunk at a time; those bytes are counted in
//! [`ChunkStats::cdc_redo_bytes`]. The result is exact by construction, and a
//! proptest checks it against the oracle.

use std::io;

use rayon::prelude::*;

use crate::hash::{CDC_AVG_BYTES, CDC_MAX_BYTES, CDC_MIN_BYTES};

/// Speculative segment size for large inputs.
pub const SEGMENT_BYTES: usize = 4 * 1024 * 1024;

/// One content-defined chunk and its BLAKE3 digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u64,
    pub len: u32,
    pub digest: [u8; 32],
}

impl Chunk {
    fn end(&self) -> u64 {
        self.offset.saturating_add(u64::from(self.len))
    }
}

/// Work counters for one chunking call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChunkStats {
    pub chunks: u64,
    pub bytes: u64,
    /// Segments chunked speculatively (0 on the sequential path).
    pub segments: u64,
    /// Segments whose speculative chain the true chain met and adopted.
    pub segments_adopted: u64,
    /// Bytes the stitcher chunked and hashed a second time, sequentially.
    pub cdc_redo_bytes: u64,
    /// Speculative bytes chunked and hashed but thrown away because they lie
    /// before the point where the chains met.
    pub spec_discard_bytes: u64,
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The v2020 cut with the agent's parameters.
#[derive(Clone, Copy, Debug)]
struct Cutter {
    min: usize,
    avg: usize,
    max: usize,
    mask_s: u64,
    mask_l: u64,
}

impl Cutter {
    /// Mirrors `FastCDC::with_level(.., Normalization::Level1)`.
    fn v2020() -> io::Result<Self> {
        let bits = fastcdc::v2020::logarithm2(CDC_AVG_BYTES);
        let mask = |index: u32| {
            usize::try_from(index)
                .ok()
                .and_then(|index| fastcdc::v2020::MASKS.get(index).copied())
                .ok_or_else(|| invalid("fastcdc mask index"))
        };
        let size = |value: u32| usize::try_from(value).map_err(|_| invalid("fastcdc size"));
        Ok(Self {
            min: size(CDC_MIN_BYTES)?,
            avg: size(CDC_AVG_BYTES)?,
            max: size(CDC_MAX_BYTES)?,
            mask_s: mask(bits + 1)?,
            mask_l: mask(bits.saturating_sub(1))?,
        })
    }

    /// Length of the chunk that starts at `start`; at least 1 before the end.
    fn next_len(self, data: &[u8], start: usize) -> io::Result<usize> {
        let rest = data
            .get(start..)
            .ok_or_else(|| invalid("cut past the end"))?;
        let (_, len) = fastcdc::v2020::cut(
            rest,
            self.min,
            self.avg,
            self.max,
            self.mask_s,
            self.mask_l,
            self.mask_s << 1,
            self.mask_l << 1,
        );
        if len == 0 && !rest.is_empty() {
            return Err(io::Error::other("fastcdc returned an empty chunk"));
        }
        Ok(len)
    }

    /// Cut and hash one chunk at `start`.
    fn chunk_at(self, data: &[u8], start: usize) -> io::Result<Chunk> {
        let len = self.next_len(data, start)?;
        let end = start.checked_add(len).ok_or_else(|| invalid("chunk end"))?;
        let bytes = data.get(start..end).ok_or_else(|| invalid("chunk range"))?;
        Ok(Chunk {
            offset: to_u64(start),
            len: u32::try_from(len).map_err(|_| invalid("chunk length"))?,
            digest: *blake3::hash(bytes).as_bytes(),
        })
    }

    /// Chunks from `start` until the position reaches `stop` or the end.
    /// Returns the position after the last chunk.
    fn chain(
        self,
        data: &[u8],
        start: usize,
        stop: usize,
        out: &mut Vec<Chunk>,
    ) -> io::Result<usize> {
        let mut at = start;
        while at < data.len() && at < stop {
            let chunk = self.chunk_at(data, at)?;
            at =
                at.saturating_add(usize::try_from(chunk.len).map_err(|_| invalid("chunk length"))?);
            out.push(chunk);
        }
        Ok(at)
    }
}

/// Chunk `data` sequentially, hashing each chunk as soon as it is cut.
///
/// # Errors
/// Only on an internal inconsistency in the cut (never for valid input).
pub fn chunk(data: &[u8]) -> io::Result<(Vec<Chunk>, ChunkStats)> {
    let cutter = Cutter::v2020()?;
    let mut chunks = Vec::with_capacity(data.len() / cutter.avg + 1);
    cutter.chain(data, 0, data.len(), &mut chunks)?;
    let stats = ChunkStats {
        chunks: to_u64(chunks.len()),
        bytes: to_u64(data.len()),
        ..ChunkStats::default()
    };
    Ok((chunks, stats))
}

/// Chunk `data` in speculative segments of `segment` bytes, in parallel when
/// `parallel` is set, then stitch. Inputs of at most one segment take the
/// sequential path.
///
/// # Errors
/// `InvalidInput` for a zero segment size.
pub fn chunk_segmented(
    data: &[u8],
    segment: usize,
    parallel: bool,
) -> io::Result<(Vec<Chunk>, ChunkStats)> {
    if segment == 0 {
        return Err(invalid("segment size must be positive"));
    }
    if data.len() <= segment {
        return chunk(data);
    }
    let cutter = Cutter::v2020()?;
    let starts: Vec<usize> = (0..data.len()).step_by(segment).collect();
    let speculate = |start: &usize| -> io::Result<(usize, Vec<Chunk>)> {
        let stop = start.saturating_add(segment).min(data.len());
        let mut chunks = Vec::with_capacity(segment / cutter.avg + 2);
        cutter.chain(data, *start, stop, &mut chunks)?;
        Ok((stop, chunks))
    };
    let speculative: Vec<io::Result<(usize, Vec<Chunk>)>> = if parallel {
        starts.par_iter().map(speculate).collect()
    } else {
        starts.iter().map(speculate).collect()
    };

    let mut counters = ChunkStats {
        bytes: to_u64(data.len()),
        segments: to_u64(starts.len()),
        ..ChunkStats::default()
    };
    let mut out: Vec<Chunk> = Vec::with_capacity(data.len() / cutter.avg + 1);
    let mut at = 0_usize;
    for result in speculative {
        let (stop, chunks) = result?;
        let offered: u64 = chunks.iter().map(|chunk| u64::from(chunk.len)).sum();
        loop {
            let here = to_u64(at);
            if let Ok(index) = chunks.binary_search_by_key(&here, |chunk| chunk.offset) {
                let adopted = chunks.get(index..).unwrap_or(&[]);
                let adopted_bytes: u64 = adopted.iter().map(|chunk| u64::from(chunk.len)).sum();
                counters.spec_discard_bytes += offered - adopted_bytes;
                counters.segments_adopted += 1;
                if let Some(last) = adopted.last() {
                    at = usize::try_from(last.end()).map_err(|_| invalid("chunk end"))?;
                }
                out.extend_from_slice(adopted);
                break;
            }
            if at >= stop || at >= data.len() {
                counters.spec_discard_bytes += offered;
                break;
            }
            let chunk = cutter.chunk_at(data, at)?;
            counters.cdc_redo_bytes += u64::from(chunk.len);
            at = usize::try_from(chunk.end()).map_err(|_| invalid("chunk end"))?;
            out.push(chunk);
        }
    }
    // Unreachable by construction: the last segment's `stop` is the end of
    // the input, and its loop ends only by adopting that segment's chain
    // (which runs to the end) or by re-chunking until `at >= stop`. Checked,
    // not assumed: a stitcher that stopped early is an internal error, never
    // a silently short chunk list.
    debug_assert_eq!(
        at,
        data.len(),
        "stitcher stopped before the end of the input"
    );
    if at != data.len() {
        return Err(io::Error::other(
            "stitcher stopped before the end of the input",
        ));
    }
    counters.chunks = to_u64(out.len());
    Ok((out, counters))
}

#[cfg(test)]
mod tests;
