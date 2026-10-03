#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
use super::*;
use crate::freshness::NullCache;
use crate::walk::{walk, WalkOptions};
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);

/// `transfer_store` batch size of the v4 pack, kept as a fixture size: a
/// file of this many maximal chunks plus one byte spans many credit returns.
const LARGE_CHUNKS: usize = 256;

struct Corpus {
    base: PathBuf,
}
impl Corpus {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "tcfs-native-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&base).unwrap();
        for path in ["source", "destination"] {
            std::fs::create_dir(base.join(path)).unwrap();
        }
        Self { base }
    }
    fn run(&self) -> Result<TransferStats> {
        copy(
            &self.base.join("source"),
            &self.base.join("destination"),
            &self.base.join("source-state"),
            &self.base.join("destination-state"),
        )
    }
}
impl Drop for Corpus {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Breaks the serve side's transport at a protocol event, not a byte
/// count. `write_control` and `write_data` hand each frame to `write` (or
/// `write_vectored`) and then call `flush`, so every flush closes one whole
/// frame. Once `ends` entries have had their `End` sent, the next frame is
/// cut in half and the transport fails, so exactly those entries can have
/// been applied, whatever order the capture threads finished in.
struct Interrupted<W> {
    output: W,
    frame: Vec<u8>,
    ends: usize,
    cut_after: usize,
    state: Cut,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    Passing,
    Truncate,
    Broken,
}
impl<W: Write> Write for Interrupted<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let allowed = match self.state {
            Cut::Broken => return Err(std::io::ErrorKind::BrokenPipe.into()),
            Cut::Truncate => {
                self.state = Cut::Broken;
                data.len().div_ceil(2)
            }
            Cut::Passing => data.len(),
        };
        let count = self.output.write(data.get(..allowed).unwrap())?;
        self.frame.extend_from_slice(data.get(..count).unwrap());
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.state == Cut::Passing {
            let (frame, _) = Frame::decode(&self.frame).unwrap();
            if matches!(frame, Frame::Control(Control::End { .. })) {
                self.ends += 1;
                if self.ends == self.cut_after {
                    self.state = Cut::Truncate;
                }
            }
        }
        self.frame.clear();
        self.output.flush()
    }
}

/// Breaks the serve side's transport at `SourceDone`: every capture of the
/// session is committed, and the destination never hears the source is done.
struct StopAtDone<W>(W);
impl<W: Write> Write for StopAtDone<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if Frame::decode(data).is_ok_and(|(frame, consumed)| {
            consumed == data.len() && matches!(frame, Frame::Control(Control::SourceDone { .. }))
        }) {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        self.0.write(data)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// R25, strict (OI-1001-Q15): a capture the source committed is never read
/// again. The source records a capture only once the destination holds its
/// bytes durably (`Held`), so after an interrupted transport whose every
/// capture committed, the resume reads 0 source bytes.
#[test]
fn interrupted_transport_resumes_completed_captures_without_source_reads() {
    const FILES: usize = 3;
    let corpus = Corpus::new();
    for index in 0..FILES {
        std::fs::write(
            corpus.base.join("source").join(format!("file-{index}")),
            noise(index as u64 + 1, 524_288),
        )
        .unwrap();
    }
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let served = serve(input, &mut StopAtDone(sender));
            let _ = closer.shutdown(std::net::Shutdown::Both);
            served
        });
        let mut output = receiver.try_clone().unwrap();
        let outcome = receive(
            &mut receiver,
            &mut output,
            &corpus.base.join("source"),
            &corpus.base.join("source-state"),
            &corpus.base.join("destination"),
            &corpus.base.join("destination-state"),
        );
        let _ = receiver.shutdown(std::net::Shutdown::Both);
        drop(output);
        drop(receiver);
        assert!(producer.join().unwrap().is_err());
        assert!(outcome.is_err());
    });
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.reused + resumed.completed, FILES as u64);
    assert_eq!(resumed.source_bytes_read, 0);
    for index in 0..FILES {
        let relative = format!("file-{index}");
        assert_eq!(
            std::fs::read(corpus.base.join("source").join(&relative)).unwrap(),
            std::fs::read(corpus.base.join("destination").join(relative)).unwrap()
        );
    }
}

/// This store's temporary name `<tag>-<pid>-<serial>`.
fn own_temporary(corpus: &Corpus, pid: u32, serial: u64) -> PathBuf {
    let tag = {
        let store = Store::open(&corpus.base.join("destination-state")).unwrap();
        crate::materialize::temporary_tag(&store.authority().unwrap())
    };
    let mut name = b".bulkload-".to_vec();
    name.extend_from_slice(&tag);
    name.extend_from_slice(format!("-{pid}-{serial}").as_bytes());
    corpus
        .base
        .join("destination")
        .join(std::ffi::OsString::from_vec(name))
}

/// #77 round 2, N2: a crash can leave a whole committer group and queue of
/// held temporaries unrenamed. Every one is salvaged, however many: 70
/// orphans whose captures committed cost 0 source reads.
#[test]
fn every_held_temporary_is_salvaged() {
    const FILES: u64 = 70;
    let corpus = Corpus::new();
    for index in 0..FILES {
        std::fs::write(
            corpus.base.join("source").join(format!("file-{index:03}")),
            noise(100 + index, 20_000),
        )
        .unwrap();
    }
    assert!(corpus.run().unwrap().refusals.is_empty());
    for index in 0..FILES {
        std::fs::rename(
            corpus
                .base
                .join("destination")
                .join(format!("file-{index:03}")),
            own_temporary(&corpus, 1, index),
        )
        .unwrap();
    }
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.completed, FILES);
    assert_eq!(resumed.source_bytes_read, 0);
    assert_eq!(resumed.bytes_received, 0);
    assert_eq!(resumed.temporaries_removed, FILES);
}

/// #77 round 2, N3: orphans named with this process's pid and the serials
/// it is about to use (an agent restarted with the same pid) never make a
/// stage collide: the sweep renames salvaged orphans to names of this
/// session, and a stage that still meets an occupied name takes the next.
#[test]
fn orphans_with_this_pid_never_block_a_stage() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/seed"), b"seed").unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    for index in 0..5 {
        std::fs::write(
            corpus.base.join("source").join(format!("new-{index}")),
            noise(200 + index, 10_000),
        )
        .unwrap();
    }
    let next = crate::materialize::next_temporary_serial();
    let orphans: Vec<PathBuf> = (next..next + 100)
        .map(|serial| own_temporary(&corpus, std::process::id(), serial))
        .collect();
    for orphan in &orphans {
        std::fs::write(orphan, b"left by an earlier agent").unwrap();
    }
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.completed, 5);
    assert_eq!(resumed.temporaries_removed, 100);
    for orphan in &orphans {
        assert!(!orphan.exists());
    }
}

/// #97: salvaged temporaries are removed when a session finishes, even one
/// with a destination-side refusal that staged nothing from them (#124), so
/// a path refused on every run never keeps them: no later session
/// re-indexes them or asks for a manifest of every file because of them.
#[test]
fn salvage_is_removed_even_when_a_path_is_refused_every_run() {
    let corpus = Corpus::new();
    let bytes = noise(41, 1 << 20);
    std::fs::write(corpus.base.join("source/held"), &bytes).unwrap();
    std::fs::write(corpus.base.join("source/blocked"), noise(42, 4_096)).unwrap();
    // The path is taken by a directory: the destination refuses the entry
    // on every run.
    std::fs::create_dir(corpus.base.join("destination/blocked")).unwrap();
    let first = corpus.run().unwrap();
    assert_eq!(first.refusals.len(), 1, "{:?}", first.refusals);
    let temporary = own_temporary(&corpus, 1, 7);
    std::fs::rename(corpus.base.join("destination/held"), &temporary).unwrap();
    for run in 0..2 {
        let refused = corpus.run().unwrap();
        assert_eq!(
            refused.refusals.len(),
            1,
            "run {run}: {:?}",
            refused.refusals
        );
        assert_eq!(refused.refusals[0].0, b"blocked".to_vec());
        assert_eq!(
            refused.temporaries_removed,
            u64::from(run == 0),
            "run {run}"
        );
        assert!(
            refused.temporaries_left.is_empty(),
            "run {run}: {:?}",
            refused.temporaries_left
        );
        assert!(!temporary.exists());
        let orphans = std::fs::read_dir(corpus.base.join("destination"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .as_bytes()
                    .starts_with(b".bulkload-")
            })
            .count();
        assert_eq!(orphans, 0, "run {run}");
    }
    assert_eq!(
        std::fs::read(corpus.base.join("destination/held")).unwrap(),
        bytes
    );
    std::fs::remove_dir(corpus.base.join("destination/blocked")).unwrap();
    let clean = corpus.run().unwrap();
    assert!(clean.refusals.is_empty(), "{:?}", clean.refusals);
    assert!(clean.temporaries_left.is_empty());
}

/// The canonical destination store root, which keys the group-commit fault.
fn destination_store_root(corpus: &Corpus) -> PathBuf {
    Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .root()
        .to_path_buf()
}

/// Names in this store's temporary grammar directly under the destination.
fn destination_orphans(corpus: &Corpus) -> usize {
    std::fs::read_dir(corpus.base.join("destination"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".bulkload-")
        })
        .count()
}

/// Pins a salvage bound for one destination root while it lives.
struct PinnedSalvageBound(PathBuf);
impl PinnedSalvageBound {
    fn at(corpus: &Corpus, files: usize, bytes: u64) -> Self {
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        let mut bounds = SALVAGE_BOUND_OVERRIDE
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        bounds.retain(|(pinned, _)| *pinned != root);
        bounds.push((root.clone(), (files, bytes)));
        drop(bounds);
        Self(root)
    }
}
impl Drop for PinnedSalvageBound {
    fn drop(&mut self) {
        SALVAGE_BOUND_OVERRIDE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(pinned, _)| *pinned != self.0);
    }
}

/// Turn each published output named in `files` back into this store's
/// orphaned temporary, as a crash between its seal and its rename leaves
/// it; its capture stays in the source ledger.
fn orphan_outputs(corpus: &Corpus, files: &[&str]) {
    for (serial, file) in (0_u64..).zip(files) {
        std::fs::rename(
            corpus.base.join("destination").join(file),
            own_temporary(corpus, 1, 1_000 + serial),
        )
        .unwrap();
    }
}

/// #124 (OI-1002-Q33), N4 restored for byte-touching refusals: an entry
/// that staged its chunks from a salvaged temporary and was then refused (its
/// group commit failed, `Held{false}`) keeps that temporary for the next
/// run, reported in `temporaries_left`. The retry fills from it again: 0
/// source bytes for the entry (its capture is in the ledger) and nothing on
/// the wire. The run that completes it removes the temporary.
#[test]
fn a_byte_touching_refusal_keeps_its_salvage() {
    let corpus = Corpus::new();
    let bytes = noise(43, 1 << 20);
    std::fs::write(corpus.base.join("source/held"), &bytes).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    orphan_outputs(&corpus, &["held"]);
    let store_root = destination_store_root(&corpus);
    crate::transfer_store::fail_output_commits(&store_root, true);
    let refused = corpus.run();
    crate::transfer_store::fail_output_commits(&store_root, false);
    let refused = refused.unwrap();
    assert_eq!(
        refused.refusals,
        vec![(
            b"held".to_vec(),
            "DESTINATION_SPACE_INSUFFICIENT".to_owned()
        )]
    );
    assert_eq!(refused.source_bytes_read, 0);
    assert_eq!(refused.bytes_received, 0, "filled from the salvage");
    assert_eq!(refused.temporaries_removed, 0);
    assert_eq!(
        refused.temporaries_left.len(),
        1,
        "{:?}",
        refused.temporaries_left
    );
    let left = &refused.temporaries_left[0];
    assert!(corpus
        .base
        .join("destination")
        .join(std::ffi::OsStr::from_bytes(left))
        .is_file());
    // The refused group's rename is not durably held (no output row): set
    // it aside, so the retry must fill from the kept temporary.
    std::fs::remove_file(corpus.base.join("destination/held")).unwrap();
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.completed, 1);
    assert_eq!(resumed.source_bytes_read, 0);
    assert_eq!(resumed.bytes_received, 0);
    assert_eq!(resumed.temporaries_removed, 1);
    assert!(resumed.temporaries_left.is_empty());
    assert_eq!(destination_orphans(&corpus), 0);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/held")).unwrap(),
        bytes
    );
}

/// #124 (OI-1002-Q33): an entry refused before it staged anything (its path
/// is taken by a directory at its decision) touched no bytes, so the
/// salvaged temporary holding its content is removed when the session
/// ends. The retry's manifest comes from the ledger, but its chunks are
/// read from the source and sent once more: the temporary was never a
/// durable record, so R25 does not cover it (OI-1002-Q33).
#[test]
fn a_refusal_that_staged_nothing_drops_the_salvage() {
    let corpus = Corpus::new();
    let bytes = noise(41, 1 << 20);
    std::fs::write(corpus.base.join("source/held"), &bytes).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    orphan_outputs(&corpus, &["held"]);
    std::fs::create_dir(corpus.base.join("destination/held")).unwrap();
    let refused = corpus.run().unwrap();
    assert_eq!(refused.refusals.len(), 1, "{:?}", refused.refusals);
    assert_eq!(refused.temporaries_removed, 1);
    assert!(
        refused.temporaries_left.is_empty(),
        "{:?}",
        refused.temporaries_left
    );
    assert_eq!(destination_orphans(&corpus), 0);
    std::fs::remove_dir(corpus.base.join("destination/held")).unwrap();
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.source_bytes_read, bytes.len() as u64);
    assert_eq!(resumed.bytes_received, bytes.len() as u64);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/held")).unwrap(),
        bytes
    );
}

/// #124 (OI-1002-Q33): kept salvage is bounded by count and by bytes. A
/// temporary past either bound is removed and refused as a value,
/// `SALVAGE_BOUND_EXCEEDED` under its current name.
#[test]
fn salvage_past_its_bound_is_a_typed_refusal() {
    const SIZE: usize = 300_000;
    // (files, bytes) bound, and how many of the two temporaries it keeps.
    for (files, bound_bytes, kept) in [
        (1, u64::MAX, 1_usize),
        (10, SIZE as u64 - 1, 0),
        (10, 2 * SIZE as u64, 2),
    ] {
        let corpus = Corpus::new();
        for (seed, file) in [(51, "a"), (52, "b")] {
            std::fs::write(corpus.base.join("source").join(file), noise(seed, SIZE)).unwrap();
        }
        assert!(corpus.run().unwrap().refusals.is_empty());
        orphan_outputs(&corpus, &["a", "b"]);
        let _bound = PinnedSalvageBound::at(&corpus, files, bound_bytes);
        let store_root = destination_store_root(&corpus);
        crate::transfer_store::fail_output_commits(&store_root, true);
        let refused = corpus.run();
        crate::transfer_store::fail_output_commits(&store_root, false);
        let refused = refused.unwrap();
        let space = refused
            .refusals
            .iter()
            .filter(|(_, code)| code == "DESTINATION_SPACE_INSUFFICIENT")
            .count();
        let bound: Vec<&Vec<u8>> = refused
            .refusals
            .iter()
            .filter(|(_, code)| code == "SALVAGE_BOUND_EXCEEDED")
            .map(|(path, _)| path)
            .collect();
        assert_eq!(space, 2, "{:?}", refused.refusals);
        assert_eq!(bound.len(), 2 - kept, "{:?}", refused.refusals);
        assert_eq!(refused.temporaries_left.len(), kept);
        assert_eq!(refused.temporaries_removed, (2 - kept) as u64);
        for path in bound {
            assert!(path.starts_with(b".bulkload-"), "named by its temporary");
            assert!(!corpus
                .base
                .join("destination")
                .join(std::ffi::OsStr::from_bytes(path))
                .exists());
        }
        assert_eq!(destination_orphans(&corpus), kept);
    }
}

/// #125: rows a store wrote before the racy guard (#86) are not proven
/// non-racy. On the first run after the upgrade both stores drop them: the
/// source reads each seat once more (and records it under the guard), the
/// destination verifies and adopts its existing outputs, so nothing crosses
/// the wire. The run after that reads nothing.
#[test]
fn rows_from_before_the_racy_guard_are_read_again_once() {
    const FILES: usize = 3;
    const SIZE: usize = 100_000;
    let corpus = Corpus::new();
    for index in 0..FILES {
        std::fs::write(
            corpus.base.join("source").join(format!("file-{index}")),
            noise(60 + index as u64, SIZE),
        )
        .unwrap();
    }
    assert!(corpus.run().unwrap().refusals.is_empty());
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (FILES as u64, 0));
    for state in ["source-state", "destination-state"] {
        Store::open(&corpus.base.join(state))
            .unwrap()
            .forget_racy_guard()
            .unwrap();
    }
    let upgraded = corpus.run().unwrap();
    assert!(upgraded.refusals.is_empty(), "{:?}", upgraded.refusals);
    assert_eq!(upgraded.reused, 0);
    assert_eq!(upgraded.completed, FILES as u64);
    assert_eq!(upgraded.source_bytes_read, (FILES * SIZE) as u64);
    assert_eq!(upgraded.bytes_received, 0, "existing outputs are adopted");
    assert_eq!(rows(&corpus), (FILES as u64, FILES as u64));
    let after = corpus.run().unwrap();
    assert_eq!((after.reused, after.source_bytes_read), (FILES as u64, 0));
}

/// A file the destination staged and sealed but never published (a crash
/// before its rename) is salvaged: the resume fills it from the orphaned
/// temporary against the ledger's manifest, with no source read and nothing
/// on the wire, and then removes the temporary.
#[test]
fn a_held_temporary_is_salvaged_without_source_reads() {
    let corpus = Corpus::new();
    let bytes = noise(21, 3 << 20);
    std::fs::write(corpus.base.join("source/held"), &bytes).unwrap();
    std::fs::write(corpus.base.join("source/other"), noise(22, 70_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    // Turn the published output back into this store's orphaned temporary,
    // as a crash between the seal and the rename leaves it.
    let tag = {
        let store = Store::open(&corpus.base.join("destination-state")).unwrap();
        crate::materialize::temporary_tag(&store.authority().unwrap())
    };
    let mut name = b".bulkload-".to_vec();
    name.extend_from_slice(&tag);
    name.extend_from_slice(b"-1-1");
    let temporary = corpus
        .base
        .join("destination")
        .join(std::ffi::OsString::from_vec(name));
    std::fs::rename(corpus.base.join("destination/held"), &temporary).unwrap();
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!((resumed.reused, resumed.completed), (1, 1));
    assert_eq!(resumed.source_bytes_read, 0);
    assert_eq!(resumed.bytes_received, 0);
    assert_eq!(resumed.temporaries_removed, 1);
    assert!(!temporary.exists());
    assert_eq!(
        std::fs::read(corpus.base.join("destination/held")).unwrap(),
        bytes
    );
}

/// #77 review F1: with no ledger row and no room to keep a manifest's
/// chunks, an incremental run streams the file instead of reading it once
/// for the manifest and again for the chunks.
#[test]
fn a_file_past_the_retention_budget_is_read_once() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/small"), noise(31, 100_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    let root = std::fs::canonicalize(corpus.base.join("source")).unwrap();
    RETAIN_OVERRIDE
        .lock()
        .unwrap()
        .push((root.clone(), 1 << 20));
    let large = noise(32, 4 << 20);
    std::fs::write(corpus.base.join("source/large"), &large).unwrap();
    // An existing identical output at the path is adopted against the
    // streamed chunks rather than refused.
    std::fs::write(corpus.base.join("source/copied"), noise(33, 2 << 20)).unwrap();
    std::fs::copy(
        corpus.base.join("source/copied"),
        corpus.base.join("destination/copied"),
    )
    .unwrap();
    let second = corpus.run();
    RETAIN_OVERRIDE
        .lock()
        .unwrap()
        .retain(|(other, _)| *other != root);
    let second = second.unwrap();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!((second.reused, second.completed), (1, 2));
    assert_eq!(second.source_bytes_read, (4 << 20) + (2 << 20));
    assert_eq!(
        std::fs::read(corpus.base.join("destination/large")).unwrap(),
        large
    );
}

/// #77 review F4: a stream may not carry more chunks than any manifest can.
#[test]
fn a_stream_past_the_chunk_bound_ends_the_session() {
    let corpus = Corpus::new();
    let store = Store::open(&corpus.base.join("destination-state")).unwrap();
    let target = Destination::open(&corpus.base.join("destination"), &store).unwrap();
    std::fs::write(corpus.base.join("source/file"), b"x").unwrap();
    let row = walk(
        &WalkOptions::new(std::fs::canonicalize(corpus.base.join("source")).unwrap()),
        &mut NullCache,
    )
    .unwrap()
    .rows
    .remove(0);
    let mut streaming = Streaming::new(row, Vec::new(), false);
    streaming.specs = vec![
        ChunkSpec {
            digest: [0; 32],
            size: 0,
        };
        MAX_MANIFEST_CHUNKS
    ];
    let header = DataHeader {
        entry: 0,
        index: u32::try_from(MAX_MANIFEST_CHUNKS).unwrap(),
        size: 1,
        offset: 0,
        digest: [0; 32],
    };
    let mut open = 0;
    assert_eq!(
        streaming
            .accept(&target, &mut open, &header, b"x", false)
            .unwrap_err(),
        BulkloadRefusal::BudgetExceeded
    );
    assert_eq!(streaming.specs.len(), MAX_MANIFEST_CHUNKS);
    assert_eq!(open, 0);
}

/// R25 under digest-only custody (R-N58): a resume after a broken transport
/// reuses every applied output with no source read, and reads again only the
/// file that was in flight, whose capture was never committed, exactly once.
#[test]
fn interrupted_transport_rereads_only_the_in_flight_file() {
    const FILES: usize = 3;
    const SIZE: usize = 524_288;
    let corpus = Corpus::new();
    for index in 0..FILES {
        std::fs::write(
            corpus.base.join("source").join(format!("file-{index}")),
            noise(index as u64 + 1, SIZE),
        )
        .unwrap();
    }
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    for stream in [&sender, &receiver] {
        stream
            .set_read_timeout(Some(std::time::Duration::from_mins(1)))
            .unwrap();
        stream
            .set_write_timeout(Some(std::time::Duration::from_mins(1)))
            .unwrap();
    }
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let served = serve(
                input,
                &mut Interrupted {
                    output: sender,
                    frame: Vec::new(),
                    ends: 0,
                    cut_after: FILES - 1,
                    state: Cut::Passing,
                },
            );
            // Close only the source's sending half: the destination still
            // answers every `End` it got with `Held`, deterministically.
            let _ = closer.shutdown(std::net::Shutdown::Write);
            served
        });
        let mut output = receiver.try_clone().unwrap();
        let outcome = receive(
            &mut receiver,
            &mut output,
            &corpus.base.join("source"),
            &corpus.base.join("source-state"),
            &corpus.base.join("destination"),
            &corpus.base.join("destination-state"),
        );
        let _ = receiver.shutdown(std::net::Shutdown::Both);
        drop(output);
        drop(receiver);
        assert!(producer.join().unwrap().is_err());
        assert!(outcome.is_err());
    });
    // Every file but the last was applied; the last never reached a name.
    assert_eq!(
        std::fs::read_dir(corpus.base.join("destination"))
            .unwrap()
            .filter(|entry| {
                !entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .as_bytes()
                    .starts_with(b".bulkload-")
            })
            .count(),
        FILES - 1
    );
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.reused, FILES as u64 - 1);
    assert_eq!(resumed.completed, 1);
    assert_eq!(resumed.source_bytes_read, SIZE as u64);
    for index in 0..FILES {
        let relative = format!("file-{index}");
        assert_eq!(
            std::fs::read(corpus.base.join("source").join(&relative)).unwrap(),
            std::fs::read(corpus.base.join("destination").join(relative)).unwrap()
        );
    }
}

#[test]
fn a_large_file_round_trips_and_resumes_without_reads() {
    let corpus = Corpus::new();
    let bytes = noise(1, LARGE_CHUNKS * crate::hash::CDC_MAX_BYTES as usize + 1);
    std::fs::write(corpus.base.join("source/large"), &bytes).unwrap();
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty());
    assert_eq!(first.source_bytes_read, bytes.len() as u64);
    assert_eq!(first.bytes_received, bytes.len() as u64);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/large")).unwrap(),
        bytes
    );
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty());
    assert_eq!(resumed.source_bytes_read, 0);
    assert_eq!(resumed.bytes_received, 0);
}

/// A fresh destination is streamed: every file is read once and sent whole
/// (`Send`), with no cross-file deduplication. A resume reuses every output
/// and reads only the refused file's header again.
#[test]
fn parallel_entries_resume_and_continue_after_a_refused_file() {
    let corpus = Corpus::new();
    let bytes = vec![71_u8; 65_536];
    for index in 0..70 {
        std::fs::write(
            corpus.base.join("source").join(format!("file-{index:03}")),
            &bytes,
        )
        .unwrap();
    }
    std::fs::write(
        corpus.base.join("source/file-035.db"),
        b"SQLite format 3\0refused",
    )
    .unwrap();
    let first = corpus.run().unwrap();
    assert_eq!(first.completed, 70);
    assert_eq!(first.refusals.len(), 1);
    assert_eq!(first.source_bytes_read, 70 * 65_536 + 16);
    assert_eq!(first.bytes_received, 70 * 65_536);
    for index in 0..70 {
        assert_eq!(
            std::fs::read(
                corpus
                    .base
                    .join("destination")
                    .join(format!("file-{index:03}"))
            )
            .unwrap(),
            bytes
        );
    }
    let second = corpus.run().unwrap();
    assert_eq!(second.reused, 70);
    assert_eq!(second.refusals.len(), 1);
    assert_eq!(second.source_bytes_read, 16);
    assert_eq!(second.bytes_received, 0);
}

/// A destination that holds chunks fills a new file from them, re-read and
/// re-verified, and only the rest crosses the wire. Hints keep every holder
/// of a chunk (#59 review, hint ordering): when the newest holder is gone,
/// an older one still serves it.
#[test]
fn a_lost_output_does_not_lose_reuse_of_its_chunks() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let prefix = noise(10, 1 << 20);
    let with_tail = |seed: u64| {
        let mut bytes = prefix.clone();
        bytes.extend(noise(seed, 1 << 20));
        bytes
    };
    std::fs::write(source.join("a"), with_tail(11)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    // `b` shares `a`'s prefix: a manifest first, the prefix filled locally.
    let b = with_tail(12);
    std::fs::write(source.join("b"), &b).unwrap();
    let second = corpus.run().unwrap();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!((second.reused, second.completed), (1, 1));
    assert!(second.bytes_received < b.len() as u64);
    // `b` is now the newest holder of the prefix chunks. Lose it: the resume
    // must still fill the prefix from `a`.
    std::fs::remove_file(destination.join("b")).unwrap();
    let third = corpus.run().unwrap();
    assert!(third.refusals.is_empty(), "{:?}", third.refusals);
    assert_eq!((third.reused, third.completed), (1, 1));
    assert!(
        third.bytes_received < b.len() as u64,
        "received {} of {}",
        third.bytes_received,
        b.len()
    );
    assert_eq!(std::fs::read(destination.join("b")).unwrap(), b);
}

#[test]
fn round_trip_resumes_without_source_reads_and_preserves_divergence() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    std::fs::create_dir(source.join("nested")).unwrap();
    let bytes: Vec<u8> = (0..800_000)
        .map(|value| u8::try_from(value % 251).unwrap())
        .collect();
    std::fs::write(source.join("nested/data"), &bytes).unwrap();
    std::fs::write(source.join(".credential"), b"account-file").unwrap();
    std::fs::set_permissions(
        source.join(".credential"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::os::unix::fs::symlink("../elsewhere", source.join("link")).unwrap();
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(
        std::fs::read(destination.join("nested/data")).unwrap(),
        bytes
    );
    assert_eq!(
        std::fs::read_link(destination.join("link")).unwrap(),
        Path::new("../elsewhere")
    );
    assert_eq!(
        std::fs::metadata(destination.join(".credential"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let second = corpus.run().unwrap();
    assert_eq!(second.reused, 2);
    assert_eq!(second.source_bytes_read, 0);
    assert_eq!(second.bytes_received, 0);
    std::fs::set_permissions(
        destination.join("nested"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let metadata_conflict = corpus.run().unwrap();
    assert_eq!(metadata_conflict.refusals.len(), 1);
    assert_eq!(
        std::fs::metadata(destination.join("nested"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    std::fs::set_permissions(
        destination.join("nested"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(destination.join(".credential"), b"sting-unique").unwrap();
    let third = corpus.run().unwrap();
    assert_eq!(third.refusals.len(), 1);
    // The divergent output is checked against the ledger's manifest: the
    // source reads nothing to refuse it.
    assert_eq!(third.source_bytes_read, 0);
    assert_eq!(
        std::fs::read(destination.join(".credential")).unwrap(),
        b"sting-unique"
    );
    assert_eq!(
        std::fs::read(source.join(".credential")).unwrap(),
        b"account-file"
    );
}

#[test]
fn sqlite_headers_and_destination_symlinks_are_not_raw_copied() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    std::fs::write(source.join("credentials.db"), b"SQLite format 3\0opaque").unwrap();
    std::fs::create_dir(source.join("nested")).unwrap();
    std::fs::write(source.join("nested/secret"), b"secret").unwrap();
    let outside = corpus.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, destination.join("nested")).unwrap();
    let result = corpus.run().unwrap();
    assert_eq!(result.refusals.len(), 3);
    assert!(!destination.join("credentials.db").exists());
    assert!(!outside.join("secret").exists());
}

/// Wire v5 is a hard cut (R-N59, R-N118): a peer that opens with another
/// protocol version or wire schema is refused before anything else.
#[test]
fn a_peer_on_another_wire_is_refused() {
    for (proto, id) in [(PROTO_VERSION - 1, wire_id()), (PROTO_VERSION, [0; 32])] {
        let corpus = Corpus::new();
        let (mut peer, ours) = std::os::unix::net::UnixStream::pair().unwrap();
        write_control(
            &mut peer,
            &Control::Open {
                proto,
                wire_id: id,
                root: corpus.base.join("source").as_os_str().as_bytes().to_vec(),
                state: corpus
                    .base
                    .join("source-state")
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
            },
        )
        .unwrap();
        let mut output = ours.try_clone().unwrap();
        assert_eq!(
            serve(ours, &mut output).unwrap_err(),
            BulkloadRefusal::FrameCodec
        );
        assert!(!corpus.base.join("source-state").exists());
    }
}

/// A data frame too short for its header is refused from its length alone,
/// before any of the next frame is read.
#[test]
fn a_short_data_frame_is_refused_without_reading_on() {
    let mut stream = 11_u32.to_be_bytes().to_vec();
    stream.push(TAG_DATA);
    stream.extend_from_slice(&[0; 10]);
    let next = Control::WalkDone { entries: 0 }.encode().unwrap();
    stream.extend_from_slice(&next);
    let mut input = stream.as_slice();
    assert_eq!(
        read_frame(&mut input).unwrap_err(),
        BulkloadRefusal::FrameCodec
    );
    assert_eq!(input.len(), 10 + next.len());
}

#[test]
fn credit_waiters_wake_on_grant_and_on_close() {
    let credit = Credit::new();
    credit.grant(10).unwrap();
    credit.acquire(4).unwrap();
    credit.acquire(6).unwrap();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| credit.acquire(5));
        credit.grant(5).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(()));
        let waiter = scope.spawn(|| credit.acquire(1));
        credit.close();
        assert_eq!(waiter.join().unwrap(), Err(BulkloadRefusal::Io(None)));
    });
}

/// #77 review F3: available credit never exceeds the window. A grant past
/// it is refused and changes nothing.
#[test]
fn credit_past_the_window_is_refused() {
    let credit = Credit::new();
    credit.grant(CREDIT_WINDOW).unwrap();
    assert_eq!(credit.grant(1), Err(BulkloadRefusal::BudgetExceeded));
    assert_eq!(credit.grant(u64::MAX), Err(BulkloadRefusal::BudgetExceeded));
    credit.acquire(CREDIT_WINDOW).unwrap();
    credit.grant(CREDIT_WINDOW).unwrap();
    assert_eq!(credit.grant(1), Err(BulkloadRefusal::BudgetExceeded));
}

#[test]
fn sweep_removes_only_this_stores_temporaries_and_records_the_rest() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    std::fs::create_dir(source.join("nested")).unwrap();
    std::fs::write(source.join("nested/data"), vec![3_u8; 70_000]).unwrap();
    // Payload that merely shares the prefix, or matches only the untagged
    // form, is carried; a tagged temporary in the source is recorded by
    // the walk, forwarded, and never carried.
    std::fs::write(source.join(".bulkload-notes"), b"operator notes").unwrap();
    std::fs::write(source.join(".bulkload-2026-09"), b"september").unwrap();
    std::fs::write(source.join(".bulkload-fedcba9876543210-9-9"), b"orphan").unwrap();
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(
        first.source_engine_temporaries,
        vec![b".bulkload-fedcba9876543210-9-9".to_vec()]
    );
    assert!(destination.join(".bulkload-notes").exists());
    assert_eq!(
        std::fs::read(destination.join(".bulkload-2026-09")).unwrap(),
        b"september"
    );
    assert!(!destination.join(".bulkload-fedcba9876543210-9-9").exists());

    let tag = {
        let store = Store::open(&corpus.base.join("destination-state")).unwrap();
        crate::materialize::temporary_tag(&store.authority().unwrap())
    };
    let own = |mark: &str, serial: u32| {
        let mut name = b".bulkload-".to_vec();
        name.extend_from_slice(&tag);
        name.extend_from_slice(format!("{mark}-1-{serial}").as_bytes());
        PathBuf::from(std::ffi::OsString::from_vec(name))
    };
    let foreign = if tag.as_slice() == b"0123456789abcdef" {
        ".bulkload-abcdef0123456789-1-5"
    } else {
        ".bulkload-0123456789abcdef-1-5"
    };
    let outside = corpus.base.join("outside");
    std::fs::write(&outside, b"not ours").unwrap();
    // Removed: an orphan copy, a second link to a published output, and
    // an empty directory temporary.
    std::fs::write(destination.join(own("", 1)), b"partial").unwrap();
    std::fs::hard_link(
        destination.join("nested/data"),
        destination.join("nested").join(own("", 2)),
    )
    .unwrap();
    std::fs::create_dir(destination.join(own("-d", 7))).unwrap();
    // Left and recorded: this store's file tag on a symlink and on a
    // directory, a non-empty directory temporary, another store's tag,
    // and the untagged form. `.bulkload-2026-09` is outside the grammar
    // (a leading zero), so it is not even considered.
    std::os::unix::fs::symlink(&outside, destination.join(own("", 3))).unwrap();
    std::fs::create_dir(destination.join(own("", 4))).unwrap();
    std::fs::create_dir(destination.join(own("-d", 8))).unwrap();
    std::fs::write(destination.join(own("-d", 8)).join("held"), b"kept").unwrap();
    std::fs::write(destination.join(foreign), b"another store").unwrap();
    std::fs::write(destination.join(".bulkload-77-6"), b"untagged").unwrap();

    let second = corpus.run().unwrap();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(second.temporaries_removed, 3);
    let mut left = second.temporaries_left;
    left.sort();
    let mut expected = vec![
        own("", 3).as_os_str().as_bytes().to_vec(),
        own("", 4).as_os_str().as_bytes().to_vec(),
        own("-d", 8).as_os_str().as_bytes().to_vec(),
        foreign.as_bytes().to_vec(),
        b".bulkload-77-6".to_vec(),
    ];
    expected.sort();
    assert_eq!(left, expected);
    assert!(!destination.join(own("", 1)).exists());
    assert!(std::fs::symlink_metadata(destination.join("nested").join(own("", 2))).is_err());
    assert!(std::fs::symlink_metadata(destination.join(own("-d", 7))).is_err());
    let data = std::fs::metadata(destination.join("nested/data")).unwrap();
    assert_eq!(data.nlink(), 1);
    assert_eq!(
        std::fs::read(destination.join("nested/data")).unwrap(),
        vec![3_u8; 70_000]
    );
    assert!(std::fs::symlink_metadata(destination.join(own("", 3)))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(&outside).unwrap(), b"not ours");
    assert!(destination.join(own("", 4)).is_dir());
    assert_eq!(
        std::fs::read(destination.join(own("-d", 8)).join("held")).unwrap(),
        b"kept"
    );
    assert!(destination.join(foreign).is_file());
    assert!(destination.join(".bulkload-77-6").is_file());
    assert_eq!(
        std::fs::read(destination.join(".bulkload-notes")).unwrap(),
        b"operator notes"
    );
}

#[test]
fn temporary_grammar_is_exact() {
    use crate::materialize::{temporary_name, TemporaryName};
    assert_eq!(
        temporary_name(b".bulkload-0123456789abcdef-12-0"),
        Some(TemporaryName::File(*b"0123456789abcdef"))
    );
    assert_eq!(
        temporary_name(b".bulkload-0123456789abcdef-d-12-0"),
        Some(TemporaryName::Directory(*b"0123456789abcdef"))
    );
    assert_eq!(
        temporary_name(b".bulkload-4242-7"),
        Some(TemporaryName::Untagged)
    );
    for name in [
        b".bulkload-".as_slice(),
        b".bulkload-notes",
        b".bulkload-0123456789ABCDEF-1-2",
        b".bulkload-0123456789abcde-1-2",
        b".bulkload-0123456789abcdef-1-2-3",
        b".bulkload-0123456789abcdef-x-1-2",
        b".bulkload-0123456789abcdef-1-",
        b".bulkload-0123456789abcdef-01-2",
        b".bulkload-0123456789abcdef-d-1-00",
        b".bulkload-0123456789abcdef-1-99999999999999999999",
        b".bulkload-0123456789abcdef-+1-2",
        b".bulkload-012-7",
        b".bulkload-1-2.tmp",
        b".bulkload--1",
        b"x.bulkload-1-2",
    ] {
        assert_eq!(temporary_name(name), None, "{}", name.escape_ascii());
    }
}

#[test]
fn directories_fall_back_to_mkdir_without_a_no_replace_rename() {
    // R-N119: the hook makes this thread's no-replace renames report
    // EINVAL, as on a filesystem that lacks them.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::materialize::force_rename_unsupported(false);
        }
    }
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    std::fs::create_dir_all(source.join("outer/inner")).unwrap();
    std::fs::write(source.join("outer/inner/data"), b"payload").unwrap();
    std::fs::set_permissions(source.join("outer"), std::fs::Permissions::from_mode(0o750)).unwrap();
    let first = {
        let _restore = Restore;
        crate::materialize::force_rename_unsupported(true);
        corpus.run().unwrap()
    };
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(first.directories_renamed, 0);
    assert_eq!(
        first.directories_fallback,
        vec![b"outer".to_vec(), b"outer/inner".to_vec()]
    );
    assert_eq!(
        std::fs::metadata(destination.join("outer")).unwrap().mode() & 0o7777,
        0o750
    );
    assert_eq!(
        std::fs::read(destination.join("outer/inner/data")).unwrap(),
        b"payload"
    );
    // No directory temporary survives the fallback.
    let leftovers: Vec<_> = std::fs::read_dir(&destination)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.as_bytes().starts_with(b".bulkload-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Without the hook the same shape takes the rename path.
    let renamed = Corpus::new();
    std::fs::create_dir(renamed.base.join("source/outer")).unwrap();
    let stats = renamed.run().unwrap();
    assert_eq!(stats.directories_renamed, 1);
    assert!(stats.directories_fallback.is_empty());
}

#[test]
fn interrupted_directory_mode_finalization_resumes_only_its_owned_inode() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    std::fs::create_dir(source.join("nested")).unwrap();
    let mut row = walk(&WalkOptions::new(source), &mut NullCache)
        .unwrap()
        .rows
        .remove(0);
    row.mode = 0o40_555;
    let store = Store::open(&corpus.base.join("destination-state")).unwrap();
    {
        let mut first = Destination::open(&destination, &store).unwrap();
        first.directory(&row, &store, b"authority").unwrap();
        assert_eq!(
            std::fs::metadata(destination.join("nested"))
                .unwrap()
                .mode()
                & 0o777,
            0o700
        );
    }
    let mut resumed = Destination::open(&destination, &store).unwrap();
    resumed.directory(&row, &store, b"authority").unwrap();
    resumed.finish_directories(&store).unwrap();
    assert_eq!(
        std::fs::metadata(destination.join("nested"))
            .unwrap()
            .mode()
            & 0o777,
        0o555
    );
}

/// W4 PR 3: every source read resolves component by component beneath the
/// root descriptor. An intermediate directory replaced by a symlink to a
/// hard link of the same inode outside the root would pass the stat-identity
/// check through a path open; the source refuses it before reading a byte.
#[test]
fn a_source_read_never_follows_a_swapped_directory() {
    let corpus = Corpus::new();
    let source = std::fs::canonicalize(corpus.base.join("source")).unwrap();
    let outside = corpus.base.join("outside");
    std::fs::create_dir(source.join("dir")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(source.join("dir/file"), noise(7, 4096)).unwrap();
    std::fs::hard_link(source.join("dir/file"), outside.join("file")).unwrap();
    let row = walk(&WalkOptions::new(source.clone()), &mut NullCache)
        .unwrap()
        .rows
        .into_iter()
        .find(|row| row.rel_path == b"dir/file")
        .unwrap();
    std::fs::rename(source.join("dir"), source.join("moved")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join("dir")).unwrap();
    let root_fd = crate::io::sys::open_root(&source).unwrap();
    let credit = Credit::new();
    let work = SourceWork {
        root: &source,
        root_fd: root_fd.as_fd(),
        authority: b"authority",
        state: &corpus.base,
        credit: &credit,
        retain: Arc::new(AtomicU64::new(0)),
    };
    let mut bytes_read = 0;
    let refused = capture_file(&work, &row, &mut bytes_read, |_, _, _, _| {
        panic!("no chunk may be read through the symlink")
    })
    .unwrap_err();
    assert!(
        matches!(
            refused,
            BulkloadRefusal::Io(Some(libc::ENOTDIR | libc::ELOOP))
        ),
        "{refused:?}"
    );
    assert_eq!(bytes_read, 0);
    assert!(open_source(&work, &row).is_err());
}

/// W4 PR 3: the walk runs at most `WALK_AHEAD` items ahead of the wire and
/// waits for slots; a source with more items than that still completes.
#[test]
fn a_walk_longer_than_the_walk_ahead_bound_completes() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let temporaries = WALK_AHEAD + 64;
    for serial in 0..temporaries {
        std::fs::write(
            source.join(format!(".bulkload-0123456789abcdef-1-{serial}")),
            b"",
        )
        .unwrap();
    }
    std::fs::create_dir(source.join("tail")).unwrap();
    std::fs::write(source.join("tail/file"), b"carried").unwrap();
    let stats = corpus.run().unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(stats.source_engine_temporaries.len(), temporaries);
    assert_eq!(stats.completed, 1);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/tail/file")).unwrap(),
        b"carried"
    );
}

/// #112: time the walk thread spends blocked on a walk-ahead slot is
/// accounted as `wait_ns`, not as walk work. A one-slot gate and a slow
/// consumer force a wait before every item after the first.
#[test]
fn walk_ahead_wait_is_accounted_apart_from_walk_work() {
    const FILES: usize = 8;
    const HOLD: std::time::Duration = std::time::Duration::from_millis(25);
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    for serial in 0..FILES {
        std::fs::write(source.join(format!("file-{serial}")), b"x").unwrap();
    }
    let root = crate::io::sys::open_root(&source).unwrap();
    let walker = crate::walk::Walker::new(root.as_fd(), true).unwrap();
    let gate = WalkGate::with_limit(1);
    let (sender, events) = std::sync::mpsc::channel();
    let started = Instant::now();
    let (time, items) = std::thread::scope(|scope| {
        let gate = &gate;
        let walk = scope.spawn(move || walk_source(walker, gate, &sender));
        let mut items = 0_u64;
        loop {
            match events.recv().unwrap() {
                Event::Walked(_) => {
                    items += 1;
                    std::thread::sleep(HOLD);
                    gate.give();
                }
                Event::WalkEnded => break,
                _ => panic!("unexpected event"),
            }
        }
        (walk.join().unwrap(), items)
    });
    let lifetime = elapsed_ns(started);
    assert!(items >= FILES as u64, "{FILES} files, got {items}");
    let hold = u64::try_from(HOLD.as_nanos()).unwrap();
    // Each item after the first waits for most of one hold.
    assert!(
        time.wait_ns >= (items - 1) * hold / 2,
        "wait {} for {items} items",
        time.wait_ns
    );
    // Walk work excludes the wait: listing a few seats is far below it.
    assert!(
        time.walk_ns < time.wait_ns / 4,
        "walk {} wait {}",
        time.walk_ns,
        time.wait_ns
    );
    assert!(time.walk_ns + time.wait_ns <= lifetime);
    // The same split reaches the process counters.
    let after = TransferTiming::snapshot();
    assert!(after.walk_wait_ns >= time.wait_ns);
    assert!(after.render().contains("walk_wait_ns="));
}

/// Pins the capture clock for one source root while it lives.
struct PinnedClock(PathBuf);
impl PinnedClock {
    fn at(corpus: &Corpus, clock: i128) -> Self {
        let root = std::fs::canonicalize(corpus.base.join("source")).unwrap();
        set_capture_clock(&root, Some(clock));
        Self(root)
    }
}
impl Drop for PinnedClock {
    fn drop(&mut self) {
        set_capture_clock(&self.0, None);
    }
}

fn stamp_ns(path: &Path) -> i128 {
    let identity = StatIdentity::from_metadata(&std::fs::metadata(path).unwrap());
    identity.mtime_ns.max(identity.ctime_ns)
}

/// Captures and output rows held by the source and destination stores.
fn rows(corpus: &Corpus) -> (u64, u64) {
    let (captures, _) = Store::open(&corpus.base.join("source-state"))
        .unwrap()
        .row_counts()
        .unwrap();
    let (_, outputs) = Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .row_counts()
        .unwrap();
    (captures, outputs)
}

/// #86 (R25, R-N58, R-N76): a seat stamped within one timestamp tick of its
/// capture can be rewritten at the same size without its stat identity
/// moving, so a ledger or output row for it could describe old content and
/// answer the next run with `Reuse`. Such a capture is sent but never
/// recorded on either side. A same-size rewrite with its mtime restored is
/// then never answered from a row: it is read again and, since outputs are
/// never replaced, surfaces as a typed conflict. Once the clock is past the
/// tick, the capture is recorded and a warm run reads nothing.
#[test]
fn a_racy_capture_is_sent_but_never_recorded() {
    const SIZE: usize = 300_000;
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    let first_bytes = noise(86, SIZE);
    std::fs::write(&seat, &first_bytes).unwrap();
    let mtime = std::fs::metadata(&seat).unwrap().modified().unwrap();

    // Captured inside the seat's tick: racy.
    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 500_000_000);
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(first.source_bytes_read, SIZE as u64);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        first_bytes
    );
    assert_eq!(rows(&corpus), (0, 0), "a racy capture is never recorded");

    // A same-size rewrite with its mtime restored, inside the tick.
    std::fs::write(&seat, noise(87, SIZE)).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&seat)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    drop(clock);
    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 500_000_000);
    let second = corpus.run().unwrap();
    assert_eq!(second.reused, 0, "no row answers a racy seat");
    assert_eq!(second.source_bytes_read, SIZE as u64);
    assert_eq!(
        second.refusals,
        [(b"seat".to_vec(), "GIT_DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(rows(&corpus), (0, 0));

    // The conflict resolved and the clock past the tick: read once more
    // (nothing was recorded), and recorded this time.
    drop(clock);
    std::fs::remove_file(corpus.base.join("destination/seat")).unwrap();
    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 60 * RACY_GRANULARITY_NS);
    let settled = corpus.run().unwrap();
    assert!(settled.refusals.is_empty(), "{:?}", settled.refusals);
    assert_eq!(settled.source_bytes_read, SIZE as u64);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        std::fs::read(&seat).unwrap()
    );
    assert_eq!(rows(&corpus), (1, 1));
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
    drop(clock);
}

/// #86: a seat stamped later than the capture's own clock reading comes
/// from a clock the capture cannot order against, and is racy too.
#[test]
fn a_capture_stamped_in_the_future_is_never_recorded() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(88, 50_000)).unwrap();
    let _clock = PinnedClock::at(&corpus, stamp_ns(&seat) - 60 * RACY_GRANULARITY_NS);
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(rows(&corpus), (0, 0));
    let again = corpus.run().unwrap();
    assert_eq!((again.reused, again.source_bytes_read), (0, 50_000));
}

/// Records the destination's `Held` answers as they are written.
struct HeldLog<W> {
    output: W,
    frame: Vec<u8>,
    held: Arc<Mutex<Vec<(u64, bool)>>>,
}
impl<W: Write> Write for HeldLog<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let count = self.output.write(data)?;
        self.frame.extend_from_slice(data.get(..count).unwrap());
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        while let Ok((frame, consumed)) = Frame::decode(&self.frame) {
            if let Frame::Control(Control::Held { entry, held }) = frame {
                self.held.lock().unwrap().push((entry, held));
            }
            self.frame.drain(..consumed);
        }
        self.output.flush()
    }
}

/// #100 (R25, R-N86, R-N88): a destination group commit that fails (here
/// its store commit, with `ENOSPC`) answers every entry of the group
/// `Held{false}`. No ledger row and no output row commits, the session ends
/// with the typed space refusal for each entry instead of hanging, and the
/// next run reads each file exactly once.
#[test]
fn a_failed_group_commit_answers_held_false_and_records_nothing() {
    const FILES: usize = 3;
    const SIZE: usize = 200_000;
    let corpus = Corpus::new();
    for index in 0..FILES {
        std::fs::write(
            corpus.base.join("source").join(format!("file-{index}")),
            noise(100 + index as u64, SIZE),
        )
        .unwrap();
    }
    let store_root = Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .root()
        .to_path_buf();
    crate::transfer_store::fail_output_commits(&store_root, true);
    let held = Arc::new(Mutex::new(Vec::new()));
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    for stream in [&sender, &receiver] {
        stream
            .set_read_timeout(Some(std::time::Duration::from_mins(1)))
            .unwrap();
        stream
            .set_write_timeout(Some(std::time::Duration::from_mins(1)))
            .unwrap();
    }
    let (source_outcome, destination_outcome) = std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let mut output = sender;
            let served = serve(input, &mut output);
            let _ = closer.shutdown(std::net::Shutdown::Both);
            served
        });
        let mut output = HeldLog {
            output: receiver.try_clone().unwrap(),
            frame: Vec::new(),
            held: Arc::clone(&held),
        };
        let outcome = receive(
            &mut receiver,
            &mut output,
            &corpus.base.join("source"),
            &corpus.base.join("source-state"),
            &corpus.base.join("destination"),
            &corpus.base.join("destination-state"),
        );
        let _ = receiver.shutdown(std::net::Shutdown::Both);
        (producer.join().unwrap(), outcome)
    });
    crate::transfer_store::fail_output_commits(&store_root, false);
    source_outcome.unwrap();
    let stats = destination_outcome.unwrap();
    let mut answers = held.lock().unwrap().clone();
    answers.sort_unstable();
    assert_eq!(answers.len(), FILES, "{answers:?}");
    assert!(answers.iter().all(|(_, held)| !held), "{answers:?}");
    assert_eq!(stats.completed, 0);
    assert_eq!(stats.refusals.len(), FILES, "{:?}", stats.refusals);
    assert!(
        stats
            .refusals
            .iter()
            .all(|(_, code)| code == "DESTINATION_SPACE_INSUFFICIENT"),
        "{:?}",
        stats.refusals
    );
    assert_eq!(rows(&corpus), (0, 0), "a failed group commits no row");

    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(resumed.completed, FILES as u64);
    assert_eq!(resumed.source_bytes_read, (FILES * SIZE) as u64);
    assert_eq!(rows(&corpus), (FILES as u64, FILES as u64));
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (FILES as u64, 0));
    for index in 0..FILES {
        let relative = format!("file-{index}");
        assert_eq!(
            std::fs::read(corpus.base.join("source").join(&relative)).unwrap(),
            std::fs::read(corpus.base.join("destination").join(relative)).unwrap()
        );
    }
}

/// #129: a subtree past the walk's depth cap reaches the transfer's
/// refusals by path and code, is counted as capped, and is never counted as
/// carried; an empty directory at the cap refuses nothing.
#[test]
fn a_capped_subtree_is_reported_never_carried() {
    use crate::walk::MAX_WALK_DEPTH;
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let capped = vec!["d"; MAX_WALK_DEPTH].join("/");
    std::fs::create_dir_all(source.join(&capped)).unwrap();
    std::fs::write(source.join(&capped).join("hidden"), b"beyond the cap").unwrap();
    let empty = format!("e/{}", vec!["d"; MAX_WALK_DEPTH - 1].join("/"));
    std::fs::create_dir_all(source.join(&empty)).unwrap();
    std::fs::write(source.join("kept"), b"carried").unwrap();
    let stats = corpus.run().unwrap();
    assert_eq!(
        stats.refusals,
        vec![(
            capped.clone().into_bytes(),
            "PATH_DEPTH_EXCEEDED".to_owned()
        )]
    );
    assert_eq!(stats.capped_subtrees(), 1);
    assert_eq!(stats.completed, 1, "only `kept` is a completed file");
    let destination = corpus.base.join("destination");
    assert!(!destination.join(&capped).join("hidden").exists());
    assert_eq!(std::fs::read(destination.join("kept")).unwrap(), b"carried");
}

proptest::proptest! {
    #![proptest_config(crate::test_support::prop_config(256))]

    /// #124 (OI-1002-Q33): the salvage bound partitions the candidates, in
    /// order, into kept and over; what is kept never exceeds either bound;
    /// and a candidate is over only if it would not fit beside every one
    /// kept before it (no temporary is refused that the bound had room for).
    #[test]
    fn the_salvage_bound_keeps_greedily_within_both_limits(
        sizes in proptest::collection::vec(0_u64..1_000, 0..40),
        max_files in 0_usize..12,
        max_bytes in 0_u64..6_000,
    ) {
        let candidates: Vec<(usize, u64)> = sizes.iter().copied().enumerate().collect();
        let (keep, over) = bound_salvage(candidates.iter().copied(), max_files, max_bytes);
        proptest::prop_assert!(keep.len() <= max_files);
        let kept_bytes: u64 = keep.iter().map(|&at| sizes[at]).sum();
        proptest::prop_assert!(kept_bytes <= max_bytes);
        let mut all: Vec<usize> = keep.iter().chain(&over).copied().collect();
        all.sort_unstable();
        proptest::prop_assert_eq!(all, (0..sizes.len()).collect::<Vec<_>>());
        proptest::prop_assert!(keep.windows(2).all(|pair| pair[0] < pair[1]));
        for &at in &over {
            let before: Vec<usize> = keep.iter().copied().filter(|&kept| kept < at).collect();
            let bytes: u64 = before.iter().map(|&kept| sizes[kept]).sum();
            proptest::prop_assert!(
                before.len() >= max_files || bytes + sizes[at] > max_bytes,
                "candidate {} had room", at
            );
        }
    }
}
