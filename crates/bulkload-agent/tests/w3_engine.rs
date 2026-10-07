//! M2 W3 engine properties, proved with the process-scope counters.
//!
//! One test function, so no other test in this binary moves the counters.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::durable::{set_durability, Durability};
use bulkload_agent::transfer::{copy, settle_racy_window, TransferStats};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn run(base: &Path, destination: &str, destination_state: &str) -> (TransferStats, Counters) {
    let before = Counters::snapshot();
    let stats = copy(
        &base.join("source"),
        &base.join(destination),
        &base.join("source-state"),
        &base.join(destination_state),
    )
    .unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    (stats, Counters::snapshot().since(before))
}

fn same_tree(base: &Path, destination: &str, names: &[&str]) {
    for name in names {
        assert_eq!(
            std::fs::read(base.join("source").join(name)).unwrap(),
            std::fs::read(base.join(destination).join(name)).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn w3_engine_properties() {
    let base = Scratch(std::env::temp_dir().join(format!("bulkload-w3-{}", std::process::id())));
    let base = &base.0;
    let source = base.join("source");
    std::fs::create_dir_all(&source).unwrap();
    for destination in ["destination", "adopted", "strict"] {
        std::fs::create_dir(base.join(destination)).unwrap();
    }
    let shared = noise(1, 3 << 20);
    let mut partial = shared.get(..1 << 20).unwrap().to_vec();
    partial.extend(noise(2, 1 << 20));
    let piece = noise(3, 300 << 10);
    let mut repeated = piece.clone();
    repeated.extend(&piece);
    std::fs::write(source.join("shared"), &shared).unwrap();
    std::fs::write(source.join("partial"), &partial).unwrap();
    std::fs::write(source.join("repeated"), &repeated).unwrap();
    std::fs::write(source.join("empty"), b"").unwrap();
    let names = ["shared", "partial", "repeated", "empty"];
    let payload = (shared.len() + partial.len() + repeated.len()) as u64;
    // Fresh seats are racy for one timestamp tick and never recorded (#86);
    // settle them so the warm run below can show 0 source reads.
    settle_racy_window(&source).unwrap();

    // Initial copy: outputs are written from verified wire chunks.
    let (first, counted) = run(base, "destination", "destination-state");
    same_tree(base, "destination", &names);
    assert_eq!(first.completed, 4);
    assert_eq!(first.source_bytes_read, payload);
    // Wire v5: a fresh destination has nothing to fill from, so every file is
    // streamed (`Send`) in one read, with no cross-file deduplication.
    assert_eq!(
        first.bytes_received, payload,
        "single pass, every byte once"
    );
    assert!(
        counted.blake3_destination() <= payload,
        "destination hashed {} of {payload}",
        counted.blake3_destination()
    );
    assert_eq!(
        counted.get(Counter::HashWireVerify),
        first.bytes_received,
        "every received byte is verified exactly once"
    );
    assert_eq!(counted.get(Counter::DestVerifyRead), 0);
    assert_eq!(counted.get(Counter::HashVerifyExisting), 0);
    assert_eq!(counted.get(Counter::SourcePackReadback), 0);
    assert_eq!(counted.get(Counter::FilesMaterialized), 4);
    // Each file sealed: a barrier each, or a batched Linux group's two
    // device seals, one before its renames and one after (S1, Q107).
    assert!(
        counted.get(Counter::FlushBarrier) >= 4 || counted.get(Counter::FlushFs) >= 2,
        "{}",
        counted.render()
    );
    assert_eq!(counted.get(Counter::FlushFull), 0);
    // One commit per group, plus, per freshly created store, its schema
    // commit and the full flush that seals its state root (#161).
    assert_eq!(
        counted.get(Counter::FlushDir),
        2,
        "one state root seal per fresh store: {}",
        counted.render()
    );
    assert!(
        counted.full_flushes_total() <= counted.get(Counter::DurableGroups) + 4,
        "{}",
        counted.render()
    );
    assert!(counted.get(Counter::TransportTuned) >= 2);

    // Warm: nothing is read or received, and a sealed store root is not
    // flushed again (#161).
    let (warm, counted) = run(base, "destination", "destination-state");
    assert_eq!((warm.reused, warm.completed), (4, 0));
    assert_eq!((warm.source_bytes_read, warm.bytes_received), (0, 0));
    assert_eq!(counted.get(Counter::FlushDir), 0, "{}", counted.render());

    // Adopt: identical outputs without records are verified, never re-sent.
    for name in names {
        std::fs::copy(source.join(name), base.join("adopted").join(name)).unwrap();
    }
    let (adopted, counted) = run(base, "adopted", "adopted-state");
    assert_eq!(adopted.completed, 4);
    assert_eq!((adopted.source_bytes_read, adopted.bytes_received), (0, 0));
    assert_eq!(counted.get(Counter::DestVerifyRead), payload);
    assert_eq!(counted.get(Counter::FilesMaterialized), 0);
    // Adopted outputs are sealed, and their directory too, before the record:
    // file by file, or by a batched group's two device seals.
    assert!(
        counted.get(Counter::FlushBarrier) >= 4 || counted.get(Counter::FlushFs) >= 2,
        "{}",
        counted.render()
    );
    assert!(
        counted.get(Counter::FlushDirBarrier) >= 1 || counted.get(Counter::FlushFs) >= 2,
        "{}",
        counted.render()
    );

    // Strict: a full flush per file in place of the barrier.
    set_durability(Durability::Strict);
    let (strict, counted) = run(base, "strict", "strict-state");
    set_durability(Durability::Group);
    same_tree(base, "strict", &names);
    assert_eq!(strict.completed, 4);
    assert!(counted.get(Counter::FlushFull) >= 4);
    assert_eq!(counted.get(Counter::FlushBarrier), 0);
    assert_eq!(counted.get(Counter::FlushFs), 0, "strict never batches");

    // A new file sharing a prefix with published outputs is built from their
    // chunks, re-read and re-verified through committed hints; only its new
    // tail crosses the wire. (Hints keep every holder of a digest, newest
    // first, and a miss falls through to the next.)
    let mut extended = shared.get(..1 << 20).unwrap().to_vec();
    extended.extend(noise(4, 1 << 20));
    std::fs::write(source.join("extended"), &extended).unwrap();
    let (grown, counted) = run(base, "destination", "destination-state");
    same_tree(base, "destination", &["extended"]);
    assert_eq!((grown.reused, grown.completed), (4, 1));
    assert_eq!(grown.source_bytes_read, extended.len() as u64);
    assert!(grown.bytes_received < extended.len() as u64);
    assert!(counted.get(Counter::DestLocalReuseRead) > 0);
}
