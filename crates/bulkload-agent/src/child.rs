//! Child-process stream draining, shared by every caller that runs a child
//! and must read one of its pipes to the end (WP10, OI-1003-Q14).
//!
//! A child blocks once a pipe it writes fills, so each pipe is read to its
//! end whatever the caller wants from it. [`drain_bounded`] is that loop,
//! written once: it keeps a bounded head for classification or evidence,
//! counts every byte, and hands every chunk to a sink (a private capture
//! file, or nothing). `git_carry::estimate` drains Git stderr through it, and
//! the `bulkload-handoff` binary drains its probes' stdout and stderr.

use std::io::{ErrorKind, Read};

/// The read buffer: one allocation per drain, reused for every chunk.
const CHUNK: usize = 64 * 1024;

/// A stream read to its end (or to its first read error).
#[derive(Debug, Default)]
pub struct Drained {
    /// The first `keep` bytes of the stream.
    pub head: Vec<u8>,
    /// Every byte read, including those past the head.
    pub total: u64,
    /// The read error that ended the stream early, if one did. `Interrupted`
    /// is retried and never ends a drain.
    pub error: Option<std::io::Error>,
}

/// Read `stream` to its end: keep its first `keep` bytes, count them all, and
/// hand every chunk read, in order, to `sink`.
///
/// The drain stops at end of stream or at the first read error other than
/// `Interrupted`; that error is returned in [`Drained::error`], never raised.
pub fn drain_bounded<R: Read>(mut stream: R, keep: usize, mut sink: impl FnMut(&[u8])) -> Drained {
    let mut drained = Drained::default();
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => {
                drained.error = Some(error);
                break;
            }
        };
        let chunk = buffer.get(..read).unwrap_or_default();
        let room = keep.saturating_sub(drained.head.len()).min(chunk.len());
        drained
            .head
            .extend_from_slice(chunk.get(..room).unwrap_or_default());
        drained.total = drained
            .total
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        sink(chunk);
    }
    drained
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::drain_bounded;
    use crate::test_support::prop_config;
    use proptest::prelude::*;
    use std::io::{Error, ErrorKind, Read};

    /// A reader that returns `bytes` in the given chunk sizes, with an
    /// `Interrupted` error before every chunk whose step is odd, and an
    /// optional hard error once `fail_after` bytes have been read.
    struct Scripted {
        bytes: Vec<u8>,
        at: usize,
        steps: Vec<usize>,
        step: usize,
        interrupt: bool,
        fail_after: Option<usize>,
    }

    impl Read for Scripted {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.fail_after.is_some_and(|limit| self.at >= limit) {
                return Err(Error::other("scripted"));
            }
            let size = self
                .steps
                .get(self.step % self.steps.len().max(1))
                .copied()
                .unwrap_or(1);
            self.step += 1;
            if self.step % 2 == 1 && !self.interrupt {
                self.interrupt = true;
                return Err(Error::from(ErrorKind::Interrupted));
            }
            self.interrupt = false;
            let mut end = self
                .bytes
                .len()
                .min(self.at + size.max(1))
                .min(self.at + out.len());
            if let Some(limit) = self.fail_after {
                end = end.min(limit.max(self.at));
            }
            let chunk = &self.bytes[self.at..end];
            out[..chunk.len()].copy_from_slice(chunk);
            self.at = end;
            Ok(chunk.len())
        }
    }

    proptest! {
        #![proptest_config(prop_config(64))]

        /// The head is the stream's prefix, the count is exact, the sink sees
        /// every byte in order, and an `Interrupted` read never ends a drain.
        #[test]
        fn head_count_and_sink_agree_with_the_stream(
            bytes in proptest::collection::vec(any::<u8>(), 0..300_000),
            steps in proptest::collection::vec(1_usize..100_000, 1..8),
            keep in 0_usize..200_000,
        ) {
            let reader = Scripted {
                bytes: bytes.clone(),
                at: 0,
                steps,
                step: 0,
                interrupt: false,
                fail_after: None,
            };
            let mut seen = Vec::new();
            let drained = drain_bounded(reader, keep, |chunk| seen.extend_from_slice(chunk));
            prop_assert!(drained.error.is_none());
            prop_assert_eq!(drained.total, bytes.len() as u64);
            prop_assert_eq!(&drained.head[..], &bytes[..keep.min(bytes.len())]);
            prop_assert_eq!(seen, bytes);
        }

        /// A hard read error ends the drain, is returned rather than raised,
        /// and everything read before it is still counted and passed on.
        #[test]
        fn a_read_error_is_returned_with_the_prefix(
            bytes in proptest::collection::vec(any::<u8>(), 1..100_000),
            cut in 0_usize..100_000,
            keep in 0_usize..100_000,
        ) {
            let cut = cut.min(bytes.len());
            let reader = Scripted {
                bytes: bytes.clone(),
                at: 0,
                steps: vec![4096],
                step: 0,
                interrupt: false,
                fail_after: Some(cut),
            };
            let mut seen = Vec::new();
            let drained = drain_bounded(reader, keep, |chunk| seen.extend_from_slice(chunk));
            prop_assert!(drained.error.is_some());
            prop_assert_eq!(drained.total, cut as u64);
            prop_assert_eq!(&drained.head[..], &bytes[..keep.min(cut)]);
            prop_assert_eq!(seen, bytes[..cut].to_vec());
        }
    }
}
