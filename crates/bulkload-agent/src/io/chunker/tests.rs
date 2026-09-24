//! Chunker proofs: boundaries and digests are bit-identical to
//! `fastcdc::v2020::FastCDC` plus `blake3`, sequentially and through the
//! speculative-segment stitcher, on random inputs and on every segment-edge
//! case. The micro-bench against the current `hash.rs` path is an ignored test
//! (run it in release; see `just bench-io-chunker`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use proptest::prelude::*;

use super::{chunk, chunk_segmented, Chunk, SEGMENT_BYTES};
use crate::hash::{chunk_boundaries, CDC_AVG_BYTES, CDC_MAX_BYTES, CDC_MIN_BYTES};

const MAX: usize = CDC_MAX_BYTES as usize;
const MIN: usize = CDC_MIN_BYTES as usize;

/// The oracle: the fastcdc iterator plus a separate BLAKE3 per chunk.
fn oracle(data: &[u8]) -> Vec<Chunk> {
    fastcdc::v2020::FastCDC::new(data, CDC_MIN_BYTES, CDC_AVG_BYTES, CDC_MAX_BYTES)
        .map(|cut| Chunk {
            offset: cut.offset as u64,
            len: u32::try_from(cut.length).unwrap(),
            digest: *blake3::hash(&data[cut.offset..cut.offset + cut.length]).as_bytes(),
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    Random,
    Zeros,
    /// Random runs separated by zero runs.
    Runs,
    /// A short random pattern repeated.
    Periodic,
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn make(seed: u64, len: usize, shape: Shape) -> Vec<u8> {
    let mut state = seed | 1;
    let mut data = vec![0_u8; len];
    match shape {
        Shape::Random => data
            .iter_mut()
            .for_each(|byte| *byte = xorshift(&mut state) as u8),
        Shape::Zeros => {}
        Shape::Runs => {
            let mut at = 0;
            while at < len {
                let run = (xorshift(&mut state) % (3 * MAX as u64)) as usize + 1;
                let end = (at + run).min(len);
                if !xorshift(&mut state).is_multiple_of(3) {
                    data[at..end]
                        .iter_mut()
                        .for_each(|byte| *byte = xorshift(&mut state) as u8);
                }
                at = end;
            }
        }
        Shape::Periodic => {
            let period = (xorshift(&mut state) % 4096) as usize + 1;
            let pattern: Vec<u8> = (0..period).map(|_| xorshift(&mut state) as u8).collect();
            for (index, byte) in data.iter_mut().enumerate() {
                *byte = pattern[index % period];
            }
        }
    }
    data
}

fn assert_all_paths_match(data: &[u8], segment: usize) {
    let expected = oracle(data);
    let (sequential, stats) = chunk(data).unwrap();
    assert_eq!(sequential, expected, "sequential fused path");
    assert_eq!(stats.chunks, expected.len() as u64);
    assert_eq!(stats.bytes, data.len() as u64);
    for parallel in [false, true] {
        let (stitched, stats) = chunk_segmented(data, segment, parallel).unwrap();
        assert_eq!(
            stitched,
            expected,
            "segmented path (segment {segment}, parallel {parallel}, len {})",
            data.len()
        );
        assert_eq!(stats.chunks, expected.len() as u64);
        assert!(stats.cdc_redo_bytes <= data.len() as u64);
    }
}

#[test]
fn boundaries_match_the_hash_rs_path() {
    let data = make(7, 3 * 1024 * 1024 + 99, Shape::Runs);
    let (chunks, _) = chunk(&data).unwrap();
    let boundaries: Vec<(usize, usize)> = chunks
        .iter()
        .map(|chunk| (chunk.offset as usize, chunk.len as usize))
        .collect();
    assert_eq!(boundaries, chunk_boundaries(&data));
}

#[test]
fn tiny_and_empty_inputs() {
    // Speculation costs about one chunk per segment, so one- and two-byte
    // segments are only affordable on the smallest inputs.
    for len in [0, 1, 2, 3, 64] {
        let data = make(len as u64 + 3, len, Shape::Random);
        for segment in [1, 2, 3, MIN] {
            assert_all_paths_match(&data, segment);
        }
    }
    for len in [MIN - 1, MIN, MIN + 1, MAX - 1, MAX, MAX + 1] {
        let data = make(len as u64 + 3, len, Shape::Random);
        for segment in [MIN - 1, MIN, len / 2 + 1, MAX] {
            assert_all_paths_match(&data, segment);
        }
    }
}

#[test]
fn segment_edges_on_true_cut_points() {
    let data = make(11, 2 * 1024 * 1024 + 12_345, Shape::Random);
    let cuts = oracle(&data);
    assert!(cuts.len() > 4);
    // A segment size equal to a true cut offset makes segment 1 start on a
    // real cut; the chains meet at once.
    for chunk in cuts.iter().take(4).skip(1) {
        assert_all_paths_match(&data, chunk.offset as usize);
    }
    // Segment ends one byte either side of a cut.
    let first = cuts[1].offset as usize;
    assert_all_paths_match(&data, first - 1);
    assert_all_paths_match(&data, first + 1);
}

#[test]
fn segments_smaller_than_a_chunk_are_skipped_not_adopted_wrongly() {
    let data = make(5, 400_000, Shape::Random);
    for segment in [4_000, MIN - 1, MIN, MIN + 1, 100_000] {
        assert_all_paths_match(&data, segment);
    }
}

#[test]
fn input_lengths_around_segment_multiples() {
    let segment = 300_000;
    for len in [
        segment - 1,
        segment,
        segment + 1,
        3 * segment,
        3 * segment + 1,
        3 * segment - 1,
    ] {
        assert_all_paths_match(&make(len as u64, len, Shape::Random), segment);
    }
}

#[test]
fn zeros_cut_at_max_and_aligned_segments_need_no_redo() {
    let data = make(0, 8 * MAX + 5, Shape::Zeros);
    let expected = oracle(&data);
    assert!(expected
        .iter()
        .rev()
        .skip(1)
        .all(|chunk| chunk.len as usize == MAX));
    let (stitched, stats) = chunk_segmented(&data, MAX, true).unwrap();
    assert_eq!(stitched, expected);
    assert_eq!(
        stats.cdc_redo_bytes, 0,
        "every segment starts on a true cut"
    );
    assert_eq!(stats.segments_adopted, stats.segments);
    assert_eq!(stats.spec_discard_bytes, 0);
    assert_all_paths_match(&data, MAX + 1);
    assert_all_paths_match(&data, MAX - 1);
}

#[test]
fn default_segment_size_on_a_large_input_meets_quickly() {
    let data = make(99, 3 * SEGMENT_BYTES + 777, Shape::Random);
    let (stitched, stats) = chunk_segmented(&data, SEGMENT_BYTES, true).unwrap();
    assert_eq!(stitched, oracle(&data));
    eprintln!("segmented stats {stats:?}");
    // FastCDC resynchronizes within a few chunks: far less than a segment.
    assert!(stats.cdc_redo_bytes < SEGMENT_BYTES as u64);
    // The 777-byte tail segment can lie wholly inside the previous segment's
    // last chunk, in which case it is never met; every other segment is.
    assert!(stats.segments_adopted + 1 >= stats.segments);
}

#[test]
fn zero_segment_is_refused() {
    assert!(chunk_segmented(b"abc", 0, false).is_err());
}

fn shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        4 => Just(Shape::Random),
        1 => Just(Shape::Zeros),
        3 => Just(Shape::Runs),
        2 => Just(Shape::Periodic),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn fused_and_segmented_match_the_oracle(
        seed in any::<u64>(),
        len in 0_usize..1_300_000,
        segment in prop_oneof![4_096_usize..65_536, 65_536_usize..700_000, Just(MIN), Just(MAX)],
        shape in shape(),
    ) {
        let data = make(seed, len, shape);
        let expected = oracle(&data);
        let (sequential, _) = chunk(&data).unwrap();
        prop_assert_eq!(&sequential, &expected);
        for parallel in [false, true] {
            let (stitched, stats) = chunk_segmented(&data, segment, parallel).unwrap();
            prop_assert_eq!(&stitched, &expected);
            prop_assert_eq!(stats.chunks, expected.len() as u64);
            prop_assert!(stats.cdc_redo_bytes <= len as u64);
        }
    }
}

// --- micro-bench -------------------------------------------------------------

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// The v3 source's CPU work per file, as `hash.rs` does it: a boundary pass
/// (`chunk_boundaries`), a second pass hashing each chunk, and a whole-file
/// BLAKE3.
fn current_path(data: &[u8], whole_file: bool) -> (usize, [u8; 32]) {
    let boundaries = chunk_boundaries(data);
    let mut last = [0_u8; 32];
    for (offset, length) in &boundaries {
        last = crate::hash::hash_bytes(&data[*offset..offset + length]);
    }
    if whole_file {
        last = crate::hash::hash_bytes(data);
    }
    (boundaries.len(), last)
}

/// `micro chunker …` lines in the W2 `bulkload-bench micro` format. Run with
/// `just bench-io-chunker` (release build). Size and repetitions come from
/// `BULKLOAD_CHUNKER_BENCH_MIB` (default 256) and
/// `BULKLOAD_CHUNKER_BENCH_REPS` (default 5).
#[test]
#[ignore = "micro-bench; run in release with just bench-io-chunker"]
fn chunker_micro_bench() {
    let mib: usize = std::env::var("BULKLOAD_CHUNKER_BENCH_MIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(256);
    let reps: usize = std::env::var("BULKLOAD_CHUNKER_BENCH_REPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let bytes = mib * 1024 * 1024;
    let data = make(0x5eed, bytes, Shape::Random);
    let uptime = std::process::Command::new("uptime")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default();
    println!(
        "micro_header item=chunker bytes={bytes} reps={reps} threads={} profile={} uptime=\"{uptime}\"",
        rayon::current_num_threads(),
        if cfg!(debug_assertions) { "debug" } else { "release" }
    );
    let expected = chunk(&data).unwrap().0;
    let arms: [(&str, &dyn Fn() -> u64); 5] = [
        ("current-hash-rs+whole-file", &|| {
            assert_eq!(current_path(&data, true).0, expected.len());
            0
        }),
        ("current-hash-rs-chunks-only", &|| {
            assert_eq!(current_path(&data, false).0, expected.len());
            0
        }),
        ("fused-sequential", &|| {
            let (chunks, _) = chunk(&data).unwrap();
            assert_eq!(chunks.len(), expected.len());
            0
        }),
        ("fused-segmented-serial", &|| {
            let (chunks, stats) = chunk_segmented(&data, SEGMENT_BYTES, false).unwrap();
            assert_eq!(chunks, expected);
            stats.cdc_redo_bytes
        }),
        ("fused-segmented-parallel", &|| {
            let (chunks, stats) = chunk_segmented(&data, SEGMENT_BYTES, true).unwrap();
            assert_eq!(chunks, expected);
            stats.cdc_redo_bytes
        }),
    ];
    for (arm, run) in arms {
        let mut rates = Vec::with_capacity(reps);
        for rep in 1..=reps {
            let start = std::time::Instant::now();
            let redo = run();
            let elapsed = start.elapsed();
            let mb_s = bytes as f64 / 1e6 / elapsed.as_secs_f64();
            rates.push(mb_s);
            println!(
                "micro item=chunker arm={arm} rep={rep} bytes={bytes} ns={} mb_s={mb_s:.1} cdc_redo_bytes={redo}",
                elapsed.as_nanos()
            );
        }
        println!(
            "micro_median item=chunker arm={arm} bytes={bytes} reps={reps} mb_s={:.1}",
            median(&mut rates)
        );
    }
}
