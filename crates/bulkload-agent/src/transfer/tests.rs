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
    // Such stores' outputs predate the capture record too (#169): rewrite
    // each as a fresh file with the same bytes and mode and no record.
    for index in 0..FILES {
        let output = corpus
            .base
            .join("destination")
            .join(format!("file-{index}"));
        let fresh = corpus.base.join("destination").join("fresh");
        std::fs::write(&fresh, std::fs::read(&output).unwrap()).unwrap();
        std::fs::set_permissions(&fresh, std::fs::metadata(&output).unwrap().permissions())
            .unwrap();
        std::fs::rename(&fresh, &output).unwrap();
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

/// #187 (WP0(d), OI-1003-Q18): a changed seat whose output this store wrote
/// is superseded. The new file is filled from the old output's own chunks,
/// so only the absent ones cross the wire; the old row is dropped with the
/// new one committed; and the run after it reads nothing.
#[test]
fn a_changed_seat_supersedes_its_own_output_and_fills_from_it() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    let mut bytes = noise(187, 800_000);
    std::fs::write(&seat, &bytes).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    assert_eq!(rows(&corpus), (1, 1));
    let before = std::fs::metadata(corpus.base.join("destination/seat")).unwrap();

    bytes.extend(noise(188, 10));
    std::fs::write(&seat, &bytes).unwrap();
    let rerun = corpus.run().unwrap();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!((rerun.reused, rerun.completed), (0, 1));
    assert_eq!(rerun.source_bytes_read, bytes.len() as u64, "read once");
    assert!(
        rerun.bytes_received < 300_000,
        "only the changed tail crosses: {}",
        rerun.bytes_received
    );
    let after = std::fs::metadata(corpus.base.join("destination/seat")).unwrap();
    assert_ne!(after.ino(), before.ino(), "a new file took the path");
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        bytes
    );
    assert_eq!(rows(&corpus).1, 1, "the old output row is dropped");
    assert_eq!(
        std::fs::read_dir(corpus.base.join("destination"))
            .unwrap()
            .count(),
        1,
        "the displaced output is removed"
    );

    let warm = corpus.run().unwrap();
    assert!(warm.refusals.is_empty());
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
    assert_eq!(warm.bytes_received, 0);
}

/// WP0(c), inequality 2, across a supersede: a chunk hint names an output
/// by path, and a superseded output's path holds its new bytes. A seat
/// planned afterwards that carries the old output's chunks (a file moved
/// out of the changed one) still fills them here, from the output the
/// publish displaced, and asks the source for none; without that output it
/// would ask for every one.
#[test]
fn a_chunk_of_a_superseded_output_is_still_filled_locally() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let old = noise(201, 700_000);
    std::fs::write(source.join("moved-from"), &old).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    // The old output, as its superseding publish holds it open.
    let displaced =
        Arc::new(std::fs::File::open(corpus.base.join("destination/moved-from")).unwrap());
    std::fs::write(source.join("moved-from"), noise(202, 650_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    assert_ne!(
        std::fs::read(corpus.base.join("destination/moved-from")).unwrap(),
        old,
        "the output was superseded"
    );

    // A seat with the old bytes, planned after that supersede.
    std::fs::write(source.join("moved-to"), &old).unwrap();
    let row = walk(
        &WalkOptions::new(std::fs::canonicalize(&source).unwrap()),
        &mut NullCache,
    )
    .unwrap()
    .rows
    .into_iter()
    .find(|row| row.rel_path == b"moved-to")
    .unwrap();
    let manifest = Manifest::new(
        crate::hash::chunk_boundaries(&old)
            .into_iter()
            .map(|(offset, length)| ChunkSpec {
                digest: crate::hash::hash_bytes(&old[offset..offset + length]),
                size: length as u64,
            })
            .collect(),
    );
    assert!(manifest.chunks.len() > 3, "several chunks");
    let store = Store::open(&corpus.base.join("destination-state")).unwrap();
    let target = Destination::open(&corpus.base.join("destination"), &store).unwrap();
    let session = SessionChunks::with_capacity(4);
    let salvage = Salvage::default();
    let missing = |displaced: &SharedDisplaced| {
        let plan = plan_file(
            &ReceiveContext {
                target: &target,
                store: &store,
                authority: b"",
                displaced,
                session: &session,
                salvage: &salvage,
            },
            &row,
            &manifest,
            &mut Vec::new(),
        );
        let Plan::Write(staging) = plan else {
            panic!("the path is free, so the plan stages a file");
        };
        staging.staged.discard().unwrap();
        staging.missing.len()
    };
    assert_eq!(
        missing(&Displaced::shared(4)),
        manifest.chunks.len(),
        "the path alone no longer holds the old chunks"
    );
    let held = Displaced::shared(4);
    held.lock()
        .unwrap()
        .insert(b"moved-from".to_vec(), displaced);
    assert_eq!(
        missing(&held),
        0,
        "every chunk is filled from the old output"
    );

    // Bounded: the oldest descriptor goes first.
    let bounded = Displaced::shared(2);
    for name in [b"a", b"b", b"c"] {
        let file = Arc::new(std::fs::File::open(source.join("moved-to")).unwrap());
        bounded.lock().unwrap().insert(name.to_vec(), file);
    }
    let kept = |name: &[u8]| bounded.lock().unwrap().get(name).is_some();
    assert!(!kept(b"a"));
    assert!(kept(b"b") && kept(b"c"));
}

/// A seat that was only touched (a new stat identity, the same bytes) has
/// nothing to replace: its output is verified where it is and adopted, the
/// same inode under a new row.
#[test]
fn a_touched_seat_with_the_same_bytes_is_adopted_not_superseded() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    let bytes = noise(189, 300_000);
    std::fs::write(&seat, &bytes).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    let before = std::fs::metadata(corpus.base.join("destination/seat")).unwrap();
    std::fs::write(&seat, &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&seat)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(90))
        .unwrap();
    let rerun = corpus.run().unwrap();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!(rerun.bytes_received, 0);
    let after = std::fs::metadata(corpus.base.join("destination/seat")).unwrap();
    assert_eq!(after.ino(), before.ino(), "the output is adopted in place");
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
}

/// #187 on the streamed path (#77 review F1): a changed seat past the
/// retention budget is streamed in place of its manifest. Its staged copy
/// supersedes this store's own output; beside it, a file the store has no
/// row for is refused and kept.
#[test]
fn a_streamed_changed_seat_supersedes_only_its_own_output() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    std::fs::write(source.join("ours"), noise(41, 2 << 20)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());

    let ours = noise(42, 2 << 20);
    std::fs::write(source.join("ours"), &ours).unwrap();
    std::fs::write(source.join("theirs"), noise(43, 2 << 20)).unwrap();
    let theirs = noise(44, 2 << 20);
    std::fs::write(destination.join("theirs"), &theirs).unwrap();
    let root = std::fs::canonicalize(&source).unwrap();
    RETAIN_OVERRIDE
        .lock()
        .unwrap()
        .push((root.clone(), 1 << 20));
    let rerun = corpus.run();
    RETAIN_OVERRIDE
        .lock()
        .unwrap()
        .retain(|(other, _)| *other != root);
    let rerun = rerun.unwrap();
    assert_eq!(
        rerun.refusals,
        [(b"theirs".to_vec(), "DESTINATION_OCCUPIED".to_owned())]
    );
    assert_eq!(rerun.source_bytes_read, 2 * (2 << 20), "each read once");
    assert_eq!(std::fs::read(destination.join("ours")).unwrap(), ours);
    assert_eq!(std::fs::read(destination.join("theirs")).unwrap(), theirs);
    assert_eq!(
        std::fs::read_dir(&destination).unwrap().count(),
        2,
        "no staged or displaced file is left"
    );
}

/// #186: a refusal is remembered only when the seat's stat identity vouches
/// for the header that was sniffed. A seat sniffed inside its own timestamp
/// tick is refused, but not remembered: it is sniffed again until the clock
/// is past the tick, and then once.
#[test]
fn a_racy_sniff_is_refused_but_not_remembered() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/state.db");
    std::fs::write(&seat, b"SQLite format 3\0provider state").unwrap();
    let refused = [(b"state.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned())];
    let remembered = || {
        Store::open(&corpus.base.join("source-state"))
            .unwrap()
            .refused_seats()
            .unwrap()
    };

    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 500_000_000);
    for _ in 0..2 {
        let racy = corpus.run().unwrap();
        assert_eq!(racy.refusals, refused);
        assert_eq!(racy.source_bytes_read, 0, "a sniff is never content");
        assert_eq!(remembered(), 0, "a racy sniff is not remembered");
    }
    drop(clock);

    for _ in 0..2 {
        let settled = corpus.run().unwrap();
        assert_eq!(settled.refusals, refused);
        assert_eq!(settled.source_bytes_read, 0);
        assert_eq!(remembered(), 1);
    }

    // A changed seat has another row key: sniffed and remembered anew.
    std::fs::write(&seat, b"SQLite format 3\0other provider state").unwrap();
    assert_eq!(corpus.run().unwrap().refusals, refused);
    assert_eq!(remembered(), 2);
}

/// #186 (review): the refusal is remembered under the row key the walk
/// gave, so it may be remembered only if the seat still has that stat
/// identity after the sniff. A seat a writer moves between the sniff and
/// that check is refused, but nothing is remembered under the identity that
/// was not the one sniffed; the next run sniffs it again under its new one.
#[test]
fn a_seat_moved_under_its_sniff_is_refused_but_not_remembered() {
    struct Unhook(PathBuf);
    impl Drop for Unhook {
        fn drop(&mut self) {
            set_after_sniff(&self.0, None);
        }
    }
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/state.db");
    std::fs::write(&seat, b"SQLite format 3\0provider state").unwrap();
    let refused = [(b"state.db".to_vec(), "SQLITE_STATE_CHANGED".to_owned())];
    let remembered = || {
        Store::open(&corpus.base.join("source-state"))
            .unwrap()
            .refused_seats()
            .unwrap()
    };
    let root = std::fs::canonicalize(corpus.base.join("source")).unwrap();
    let moved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook = {
        let moved = Arc::clone(&moved);
        Arc::new(move || {
            // Once: a writer's stamp lands between the sniff and the stat.
            if !moved.swap(true, Ordering::SeqCst) {
                let earlier = std::time::SystemTime::now() - std::time::Duration::from_hours(2);
                std::fs::File::options()
                    .write(true)
                    .open(&seat)
                    .unwrap()
                    .set_modified(earlier)
                    .unwrap();
            }
        })
    };
    set_after_sniff(&root, Some(hook));
    let _unhook = Unhook(root);

    let first = corpus.run().unwrap();
    assert!(moved.load(Ordering::SeqCst), "the hook ran under the sniff");
    assert_eq!(first.refusals, refused);
    assert_eq!(first.source_bytes_read, 0);
    assert_eq!(
        remembered(),
        0,
        "nothing is remembered under an identity the seat no longer has"
    );

    // Sniffed again under the identity it has now, and remembered once.
    for _ in 0..2 {
        let settled = corpus.run().unwrap();
        assert_eq!(settled.refusals, refused);
        assert_eq!(remembered(), 1);
    }
}

/// Pins the destination's clock (the one a refused output's stamps are
/// judged against) for one destination root while it lives.
struct PinnedDestinationClock(PathBuf);
impl PinnedDestinationClock {
    fn at(corpus: &Corpus, clock: i128) -> Self {
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        set_capture_clock(&root, Some(clock));
        Self(root)
    }
}
impl Drop for PinnedDestinationClock {
    fn drop(&mut self) {
        set_capture_clock(&self.0, None);
    }
}

/// How many ownership rows the destination store holds.
fn owned_outputs(corpus: &Corpus) -> u64 {
    Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .conn_count("owned_outputs")
        .unwrap()
}

/// How many refusals the destination store remembers.
fn refused_outputs(corpus: &Corpus) -> u64 {
    Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .conn_count("refused_outputs")
        .unwrap()
}

/// #187 review (R25): a seat standing refused `DESTINATION_OCCUPIED` is not
/// read again on every run. The destination remembers the refusal under the
/// seat's row key and the identity of the file it found at the path, and
/// refuses the entry when it is offered while both are unchanged: 0 source
/// bytes. It is remembered only for a file that was settled when it was
/// read, as a capture is recorded only for a settled seat (#86). When the
/// file at the path goes, or the seat changes, the record no longer
/// answers.
#[test]
fn an_occupied_seat_is_read_once_and_then_refused_from_its_record() {
    const SIZE: usize = 50_000;
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    let output = corpus.base.join("destination/seat");
    std::fs::write(&seat, noise(1871, 40_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());

    // Another writer replaces the output; the seat changes.
    let theirs = noise(1872, 12_345);
    let aside = corpus.base.join("aside");
    std::fs::write(&aside, &theirs).unwrap();
    std::fs::rename(&aside, &output).unwrap();
    std::fs::write(&seat, noise(1873, SIZE)).unwrap();
    let occupied = [(b"seat".to_vec(), "DESTINATION_OCCUPIED".to_owned())];

    // A racy capture's refusal is not remembered: its stat identity does
    // not vouch for the bytes its manifest was built from (#86).
    let source_clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 500_000_000);
    let racy = corpus.run().unwrap();
    assert_eq!(racy.refusals, occupied);
    assert_eq!(racy.source_bytes_read, SIZE as u64);
    assert_eq!(
        refused_outputs(&corpus),
        0,
        "a racy capture is not remembered"
    );
    drop(source_clock);

    // Read inside the foreign file's own tick: refused, not remembered.
    let clock = PinnedDestinationClock::at(&corpus, stamp_ns(&output) + 500_000_000);
    for _ in 0..2 {
        let racy = corpus.run().unwrap();
        assert_eq!(racy.refusals, occupied);
        assert_eq!(racy.source_bytes_read, SIZE as u64);
        assert_eq!(racy.bytes_received, 0);
        assert_eq!(
            refused_outputs(&corpus),
            0,
            "an unsettled file vouches for nothing"
        );
    }
    drop(clock);

    // Settled: read once more, and remembered.
    let read = corpus.run().unwrap();
    assert_eq!(read.refusals, occupied);
    assert_eq!(read.source_bytes_read, SIZE as u64);
    assert_eq!(refused_outputs(&corpus), 1);
    for _ in 0..2 {
        let unchanged = corpus.run().unwrap();
        assert_eq!(
            unchanged.refusals, occupied,
            "the refusal stands, and is reported"
        );
        assert_eq!(
            (unchanged.source_bytes_read, unchanged.bytes_received),
            (0, 0),
            "an unchanged refused seat is not opened"
        );
        assert_eq!(std::fs::read(&output).unwrap(), theirs);
    }

    // The seat changes: another row key, read once and refused again.
    std::fs::write(&seat, noise(1874, SIZE + 1)).unwrap();
    let changed = corpus.run().unwrap();
    assert_eq!(changed.refusals, occupied);
    assert_eq!(changed.source_bytes_read, SIZE as u64 + 1);
    assert_eq!(corpus.run().unwrap().source_bytes_read, 0);

    // The operator removes the foreign file: the record answers nothing.
    std::fs::remove_file(&output).unwrap();
    let converged = corpus.run().unwrap();
    assert!(converged.refusals.is_empty(), "{:?}", converged.refusals);
    assert_eq!(
        std::fs::read(&output).unwrap(),
        std::fs::read(&seat).unwrap()
    );
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
}

/// #187 review: a destination file system the first publish reaches through
/// its link fallback may have no atomic exchange (NFS, SMB, exFAT). A
/// changed seat there is refused under its own code as soon as its manifest
/// shows the output holds other bytes: nothing is staged, no chunk is asked
/// of the source, and the old output keeps its row. The refusal is
/// remembered, so a rerun reads 0 source bytes for it instead of paying the
/// whole copy again; once the file system has an exchange it converges.
#[test]
fn a_changed_seat_without_an_exchange_is_refused_before_anything_is_staged() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::materialize::force_rename_unsupported(false);
        }
    }
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    let output = corpus.base.join("destination/seat");
    let mut bytes = noise(1875, 400_000);
    std::fs::write(&seat, &bytes).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    assert_eq!(rows(&corpus), (1, 1));
    let old = bytes.clone();
    let before = std::fs::metadata(&output).unwrap().ino();

    bytes.extend(noise(1876, 10));
    std::fs::write(&seat, &bytes).unwrap();
    let unsupported = [(
        b"seat".to_vec(),
        "DESTINATION_EXCHANGE_UNSUPPORTED".to_owned(),
    )];
    {
        let _restore = Restore;
        crate::materialize::force_rename_unsupported(true);
        let refused = corpus.run().unwrap();
        assert_eq!(refused.refusals, unsupported);
        assert_eq!(
            refused.source_bytes_read,
            bytes.len() as u64,
            "the manifest is one read of the changed seat"
        );
        assert_eq!(refused.bytes_received, 0, "no chunk is asked of the source");
        assert_eq!(std::fs::read(&output).unwrap(), old);
        assert_eq!(std::fs::metadata(&output).unwrap().ino(), before);
        assert_eq!(rows(&corpus), (1, 1), "the old output keeps its row");
        assert_eq!(
            std::fs::read_dir(corpus.base.join("destination"))
                .unwrap()
                .count(),
            1,
            "the probe leaves nothing behind"
        );
        assert_eq!(refused_outputs(&corpus), 1);

        // Every later run there: the same refusal, and nothing read.
        for _ in 0..2 {
            let rerun = corpus.run().unwrap();
            assert_eq!(rerun.refusals, unsupported);
            assert_eq!(
                (rerun.source_bytes_read, rerun.bytes_received),
                (0, 0),
                "a rerun on a file system without an exchange reads nothing"
            );
            assert_eq!(std::fs::read(&output).unwrap(), old);
        }
    }

    // The file system gained an exchange: the record no longer answers.
    let converged = corpus.run().unwrap();
    assert!(converged.refusals.is_empty(), "{:?}", converged.refusals);
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
}

/// #187 review: a superseding publish whose exchange took effect and whose
/// row never committed (here the group's store commit fails; a crash at the
/// same place leaves the same state) leaves the new file at the path with
/// no row. The seat then changes once more before any run adopted it. The
/// next session's sweep finds the staged file its record names at the leaf
/// and gives it an ownership row, so the changed seat supersedes it and
/// converges, instead of being refused `DESTINATION_OCCUPIED` on every run.
#[test]
fn a_seat_changed_again_after_an_unrecorded_supersede_still_converges() {
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    let output = corpus.base.join("destination/seat");
    std::fs::write(&seat, noise(1877, 300_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());

    let second = noise(1878, 310_000);
    std::fs::write(&seat, &second).unwrap();
    let store_root = destination_store_root(&corpus);
    crate::transfer_store::fail_group_commits(&store_root, true);
    let failed = corpus.run();
    crate::transfer_store::fail_group_commits(&store_root, false);
    let failed = failed.unwrap();
    assert_eq!(
        failed.refusals,
        [(
            b"seat".to_vec(),
            "DESTINATION_SPACE_INSUFFICIENT".to_owned()
        )]
    );
    assert_eq!(
        std::fs::read(&output).unwrap(),
        second,
        "the exchange took effect; only the row is missing"
    );
    assert_eq!(rows(&corpus).1, 0, "the old rows left with the intent");

    // The seat changes again before the next run.
    let third = noise(1879, 320_000);
    std::fs::write(&seat, &third).unwrap();
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(std::fs::read(&output).unwrap(), third);
    assert_eq!(
        std::fs::read_dir(corpus.base.join("destination"))
            .unwrap()
            .count(),
        1
    );
    let warm = corpus.run().unwrap();
    assert!(warm.refusals.is_empty());
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
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
/// and refuses the refused file again from its remembered refusal, reading
/// nothing (#186): its header was sniffed once, and never as content.
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
    assert_eq!(first.source_bytes_read, 70 * 65_536);
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
    assert_eq!(second.refusals, first.refusals);
    assert_eq!(second.source_bytes_read, 0);
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
        assert_eq!(waiter.join().unwrap(), Err(BulkloadRefusal::WorkerLost));
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
        ledger: LedgerSync::Relaxed,
    };
    let mut bytes_read = 0;
    let refused = capture_file(&work, &row, &mut bytes_read, &mut None, |_, _, _, _| {
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
/// recorded as a reuse key on either side. A same-size rewrite with its
/// mtime restored is then never answered from a row: it is read again. The
/// racy capture's output has an ownership row, and no reuse row, so it is
/// this store's own (WP0(d), #187 review) and the changed seat supersedes
/// it: an actively written file converges instead of standing refused.
/// Once the clock is past the tick, the capture is recorded and a warm run
/// reads nothing.
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
    assert_eq!(
        owned_outputs(&corpus),
        1,
        "the output it published has an ownership row, and no reuse row"
    );
    let published = std::fs::metadata(corpus.base.join("destination/seat"))
        .unwrap()
        .ino();

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
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/seat")).unwrap(),
        std::fs::read(&seat).unwrap(),
        "the racy publish's own output is superseded"
    );
    assert_ne!(
        std::fs::metadata(corpus.base.join("destination/seat"))
            .unwrap()
            .ino(),
        published,
        "a new file took the path"
    );
    assert_eq!(rows(&corpus), (0, 0), "racy again: no reuse row");
    assert_eq!(owned_outputs(&corpus), 1);

    // The clock past the tick: read once more (no capture was recorded),
    // adopted in place, and recorded this time.
    drop(clock);
    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 60 * RACY_GRANULARITY_NS);
    let settled = corpus.run().unwrap();
    assert!(settled.refusals.is_empty(), "{:?}", settled.refusals);
    assert_eq!(settled.source_bytes_read, SIZE as u64);
    assert_eq!(settled.bytes_received, 0, "the output is adopted in place");
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
/// with the typed space refusal for each entry instead of hanging. The
/// group's files were published before its store commit failed, so the next
/// run adopts each one from its capture record without a source read
/// (#169), and commits its row.
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
    assert_eq!(
        (resumed.source_bytes_read, resumed.unrowed_adopted),
        (0, FILES as u64)
    );
    assert_eq!(rows(&corpus), (0, FILES as u64));
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

// ---- WP1 PR 3 (S2): a run never changes the source's lstat census ---------

/// Every node under `root`, itself included, by relative path, with every
/// `lstat` field a write, create, rename, chmod or link would move. Access
/// time is left out: reading the source is the point of a run.
fn lstat_census(root: &Path) -> Vec<(PathBuf, [i64; 9])> {
    use std::os::unix::fs::MetadataExt as _;
    fn visit(root: &Path, relative: &Path, rows: &mut Vec<(PathBuf, [i64; 9])>) {
        let meta = std::fs::symlink_metadata(root.join(relative)).unwrap();
        rows.push((
            relative.to_path_buf(),
            [
                i64::from(meta.mode()),
                i64::try_from(meta.size()).unwrap(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
                i64::try_from(meta.ino()).unwrap(),
                i64::try_from(meta.nlink()).unwrap(),
                i64::from(meta.uid()),
            ],
        ));
        if meta.is_dir() {
            for entry in std::fs::read_dir(root.join(relative)).unwrap() {
                visit(root, &relative.join(entry.unwrap().file_name()), rows);
            }
        }
    }
    let mut rows = Vec::new();
    visit(root, Path::new(""), &mut rows);
    rows.sort();
    rows
}

#[derive(Clone, Debug)]
enum SourceNode {
    File(u16),
    Link(&'static str),
    Directory,
}

fn source_node() -> impl proptest::strategy::Strategy<Value = SourceNode> {
    use proptest::prelude::*;
    prop_oneof![
        4 => (0_u16..20_000).prop_map(SourceNode::File),
        1 => prop::sample::select(vec!["n0", "../outside", "missing", "d1"])
            .prop_map(SourceNode::Link),
        1 => Just(SourceNode::Directory),
    ]
}

/// Build `entries` under `source`: each lands in the root, `d1` or `d1/d2`
/// (created on demand) under the name `n<index>`; a name already taken is
/// skipped.
fn build_source(source: &Path, entries: &[(u8, u8, SourceNode)]) {
    for (depth, name, node) in entries {
        let parent = match depth {
            0 => source.to_path_buf(),
            1 => source.join("d1"),
            _ => source.join("d1/d2"),
        };
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!("n{name}"));
        if std::fs::symlink_metadata(&path).is_ok() {
            continue;
        }
        match node {
            SourceNode::File(length) => {
                std::fs::write(&path, noise(u64::from(*length), usize::from(*length))).unwrap();
            }
            SourceNode::Link(target) => std::os::unix::fs::symlink(target, &path).unwrap(),
            SourceNode::Directory => std::fs::create_dir(&path).unwrap(),
        }
    }
}

proptest::proptest! {
    #![proptest_config(crate::test_support::prop_config(12))]

    /// P-S2: whatever the tree, a copy, its warm rerun, and a copy refused
    /// because a state root lies inside the source leave every source node's
    /// lstat identity exactly as it was, and the refused run creates nothing.
    #[test]
    fn a_run_leaves_the_source_lstat_census_unchanged(
        entries in proptest::collection::vec((0_u8..3, 0_u8..6, source_node()), 0..10),
        nested_state in proptest::bool::ANY,
        state_in_source_for_source_side in proptest::bool::ANY,
    ) {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        build_source(&source, &entries);
        let before = lstat_census(&source);
        let first = corpus.run().unwrap();
        proptest::prop_assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        proptest::prop_assert_eq!(&before, &lstat_census(&source));
        let warm = corpus.run().unwrap();
        proptest::prop_assert_eq!(warm.source_bytes_read, 0);
        proptest::prop_assert_eq!(&before, &lstat_census(&source));
        // A state root inside the source: at the top, or under `d1` when the
        // tree has it.
        let inside = if nested_state && source.join("d1").is_dir() {
            source.join("d1/state")
        } else {
            source.join("state")
        };
        let fresh = corpus.base.join("fresh");
        std::fs::create_dir(&fresh).unwrap();
        let (source_state, destination_state) = if state_in_source_for_source_side {
            (inside.clone(), corpus.base.join("fresh-destination-state"))
        } else {
            (corpus.base.join("fresh-source-state"), inside.clone())
        };
        proptest::prop_assert_eq!(
            copy(&source, &fresh, &source_state, &destination_state).unwrap_err(),
            BulkloadRefusal::SnapshotRootsOverlap
        );
        proptest::prop_assert!(std::fs::symlink_metadata(&inside).is_err());
        proptest::prop_assert_eq!(&before, &lstat_census(&source));
    }
}

/// `serve` refuses a state root inside its source before `Store::open`
/// creates it (WP1 PR 3): previously the store was created first.
#[test]
fn serve_refuses_a_state_inside_the_source_before_creating_it() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    std::fs::write(source.join("file"), b"bytes").unwrap();
    let before = lstat_census(&source);
    let state = source.join("state");
    let mut request = Vec::new();
    write_control(
        &mut request,
        &Control::Open {
            proto: PROTO_VERSION,
            wire_id: wire_id(),
            root: source.as_os_str().as_bytes().to_vec(),
            state: state.as_os_str().as_bytes().to_vec(),
        },
    )
    .unwrap();
    let mut answer = Vec::new();
    assert_eq!(
        serve(std::io::Cursor::new(request), &mut answer).unwrap_err(),
        BulkloadRefusal::SnapshotRootsOverlap
    );
    assert!(answer.is_empty(), "nothing is sent before the refusal");
    assert!(std::fs::symlink_metadata(&state).is_err());
    assert_eq!(before, lstat_census(&source));
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

// ---- #169 (R25 strict, OI-1003-Q40): P74 R25-STRICT-ADOPT ------------------

/// Fixed seeds of P74 (no fuzzing): each one generates a corpus and its
/// crash points.
const P74_SEEDS: [u64; 12] = [1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233];

/// What a crash and the time before the resume left at one unrowed output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unrowed {
    /// Durable at the final path with its bytes and its capture record.
    Intact,
    /// Removed by a third party: the resume reads it again.
    Deleted,
    /// Rewritten in place at the same size by a third party: its record no
    /// longer proves it, so the resume reads it and refuses it as occupied.
    Tampered,
}

/// One generated P74 case: the corpus, the crash point (files before it
/// carried and rowed, files from it published by a group whose row commit
/// never lands), and each unrowed output's fate.
#[derive(Debug)]
struct P74Case {
    sizes: Vec<usize>,
    crash: usize,
    fates: Vec<Unrowed>,
}

fn p74_case(seed: u64) -> P74Case {
    const SIZES: [usize; 5] = [0, 1, 4_096, 70_000, 300_000];
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % bound as u64).unwrap()
    };
    let files = 1 + next(6);
    let sizes: Vec<usize> = (0..files).map(|_| SIZES[next(5)]).collect();
    let crash = next(files);
    let fates = sizes[crash..]
        .iter()
        .map(|size| match next(4) {
            0 => Unrowed::Deleted,
            1 if *size > 0 => Unrowed::Tampered,
            _ => Unrowed::Intact,
        })
        .collect();
    P74Case {
        sizes,
        crash,
        fates,
    }
}

/// P74 R25-STRICT-ADOPT (#169, OI-1003-Q40, R-N58), over generated crash
/// points: files before the crash point are carried and rowed; files from it
/// are published by a group whose row commit never lands (the store commit
/// fails after the files were sealed, renamed and their directory sealed:
/// `MC_r25_unrowed_bytes`'s state). Every capture is non-racy (the capture
/// clock is pinned past every stamp). The resume then reads exactly the
/// deleted and tampered outputs' seats: each intact unrowed output is
/// adopted from its capture record with 0 source bytes, no wire bytes, and
/// its row committed, so the next run reuses it.
#[test]
fn p74_unrowed_outputs_are_adopted_without_source_reads() {
    for seed in P74_SEEDS {
        p74_check(seed, &p74_case(seed));
    }
}

#[allow(clippy::too_many_lines)]
fn p74_check(seed: u64, case: &P74Case) {
    let corpus = Corpus::new();
    let name = |index: usize| format!("p74-{index}");
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let content = |index: usize| noise(seed * 1_000 + index as u64, case.sizes[index]);
    for index in 0..case.sizes.len() {
        std::fs::write(source.join(name(index)), content(index)).unwrap();
    }
    let latest = (0..case.sizes.len())
        .map(|index| stamp_ns(&source.join(name(index))))
        .max()
        .unwrap();
    let _clock = PinnedClock::at(&corpus, latest + 60 * RACY_GRANULARITY_NS);
    // Before the crash point: carried and rowed. The rest is hidden.
    let hidden = corpus.base.join("hidden");
    std::fs::create_dir(&hidden).unwrap();
    for index in case.crash..case.sizes.len() {
        std::fs::rename(source.join(name(index)), hidden.join(name(index))).unwrap();
    }
    let carried = corpus.run().unwrap();
    assert!(carried.refusals.is_empty(), "seed {seed}: {carried:?}");
    for index in case.crash..case.sizes.len() {
        std::fs::rename(hidden.join(name(index)), source.join(name(index))).unwrap();
    }
    // From the crash point: published, sealed, renamed; no row commits.
    let store_root = destination_store_root(&corpus);
    crate::transfer_store::fail_output_commits(&store_root, true);
    let crashed = corpus.run();
    crate::transfer_store::fail_output_commits(&store_root, false);
    let crashed = crashed.unwrap();
    assert_eq!(
        crashed.refusals.len(),
        case.sizes.len() - case.crash,
        "seed {seed}: {crashed:?}"
    );
    let (mut want_read, mut want_adopted, mut want_tampered) = (0_u64, 0_u64, 0_u64);
    for (offset, fate) in case.fates.iter().enumerate() {
        let index = case.crash + offset;
        let output = destination.join(name(index));
        assert_eq!(
            std::fs::read(&output).unwrap(),
            content(index),
            "seed {seed}: the crashed run published {index}"
        );
        match fate {
            Unrowed::Intact => want_adopted += 1,
            Unrowed::Deleted => {
                std::fs::remove_file(&output).unwrap();
                want_read += case.sizes[index] as u64;
            }
            Unrowed::Tampered => {
                let mut bytes = content(index);
                bytes[0] ^= 0xff;
                let mode = std::fs::metadata(&output).unwrap().permissions();
                std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::write(&output, &bytes).unwrap();
                std::fs::set_permissions(&output, mode).unwrap();
                want_read += case.sizes[index] as u64;
                want_tampered += 1;
            }
        }
    }

    let resumed = corpus.run().unwrap();
    assert_eq!(
        resumed.source_bytes_read, want_read,
        "seed {seed} {case:?}: only deleted and tampered seats are read: {resumed:?}"
    );
    assert_eq!(
        resumed.unrowed_adopted, want_adopted,
        "seed {seed}: {resumed:?}"
    );
    assert_eq!(
        resumed.unrowed_unproven, want_tampered,
        "seed {seed}: {resumed:?}"
    );
    assert_eq!(
        resumed.reused, case.crash as u64,
        "seed {seed}: {resumed:?}"
    );
    assert_eq!(
        resumed.refusals.len() as u64,
        want_tampered,
        "seed {seed}: {resumed:?}"
    );
    assert!(
        resumed
            .refusals
            .iter()
            .all(|(_, code)| code == "DESTINATION_OCCUPIED"),
        "seed {seed}: {resumed:?}"
    );
    for (offset, fate) in case.fates.iter().enumerate() {
        let index = case.crash + offset;
        if *fate != Unrowed::Tampered {
            assert_eq!(
                std::fs::read(destination.join(name(index))).unwrap(),
                content(index),
                "seed {seed}: {index}"
            );
        }
    }
    // The adopted rows committed, and each refusal is remembered against
    // the tampered file it found (#187 review): a warm run reads nothing,
    // and reports the same refusals.
    let warm = corpus.run().unwrap();
    assert_eq!(warm.unrowed_adopted, 0, "seed {seed}: {warm:?}");
    assert_eq!(warm.source_bytes_read, 0, "seed {seed}: {warm:?}");
    assert_eq!(warm.bytes_received, 0, "seed {seed}: {warm:?}");
    let sorted = |stats: &TransferStats| {
        let mut refusals = stats.refusals.clone();
        refusals.sort();
        refusals
    };
    assert_eq!(sorted(&warm), sorted(&resumed), "seed {seed}: {warm:?}");
    assert_eq!(
        warm.reused,
        case.sizes.len() as u64 - want_tampered,
        "seed {seed}: {warm:?}"
    );
}

/// P74's generator covers every fate and both sides of the crash point
/// across its fixed seeds, so the property is not vacuous.
#[test]
fn p74_seeds_cover_every_fate() {
    let cases: Vec<P74Case> = P74_SEEDS.iter().map(|seed| p74_case(*seed)).collect();
    for fate in [Unrowed::Intact, Unrowed::Deleted, Unrowed::Tampered] {
        assert!(
            cases.iter().any(|case| case.fates.contains(&fate)),
            "{fate:?} never generated"
        );
    }
    assert!(cases.iter().any(|case| case.crash > 0));
    assert!(cases
        .iter()
        .any(|case| case.sizes[case.crash..].contains(&300_000)));
}

/// #169: a racy capture writes no capture record (#86: its stat identity
/// cannot vouch for its bytes), so its unrowed output is not adopted; the
/// resume reads it once, as before, and counts it unproven.
#[test]
fn a_racy_unrowed_output_is_read_again_and_counted() {
    const SIZE: usize = 90_000;
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(169, SIZE)).unwrap();
    let store_root = destination_store_root(&corpus);
    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 500_000_000);
    crate::transfer_store::fail_output_commits(&store_root, true);
    let crashed = corpus.run();
    crate::transfer_store::fail_output_commits(&store_root, false);
    assert_eq!(crashed.unwrap().refusals.len(), 1);
    drop(clock);
    let output = std::fs::File::open(corpus.base.join("destination/seat")).unwrap();
    assert_eq!(
        unrowed::read_record(&output),
        None,
        "a racy capture has no record"
    );
    let _clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 60 * RACY_GRANULARITY_NS);
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{resumed:?}");
    assert_eq!(
        (
            resumed.source_bytes_read,
            resumed.unrowed_adopted,
            resumed.unrowed_unproven
        ),
        (SIZE as u64, 0, 1)
    );
    let warm = corpus.run().unwrap();
    assert_eq!((warm.reused, warm.source_bytes_read), (1, 0));
}

/// #169 review: only a staged publish used to write a capture record, so an
/// output adopted against a manifest had none (or kept a stale one), and a
/// group commit that then failed left it durable with no row and no proof:
/// the next run read its source again. Run 1 captures the seat racily (no
/// record, no row kept). Run 2 captures it non-racily: the destination
/// verifies the existing output against the manifest, gives it that
/// capture's record and queues its adoption, and the group's store commit
/// fails. Run 3 finds durable bytes with no row and adopts them from the
/// record: 0 source bytes.
#[test]
fn an_output_adopted_against_a_manifest_carries_its_capture_record() {
    const SIZE: usize = 90_000;
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(1691, SIZE)).unwrap();
    let store_root = destination_store_root(&corpus);
    let record = || {
        unrowed::read_record(&std::fs::File::open(corpus.base.join("destination/seat")).unwrap())
    };

    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 500_000_000);
    let racy = corpus.run().unwrap();
    assert!(racy.refusals.is_empty(), "{racy:?}");
    drop(clock);
    assert_eq!(record(), None, "a racy capture has no record");
    assert_eq!(rows(&corpus), (0, 0), "a racy capture keeps no row");

    let _clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 60 * RACY_GRANULARITY_NS);
    crate::transfer_store::fail_output_commits(&store_root, true);
    let failed = corpus.run();
    crate::transfer_store::fail_output_commits(&store_root, false);
    let failed = failed.unwrap();
    assert_eq!(failed.refusals.len(), 1, "{failed:?}");
    assert_eq!(
        (
            failed.source_bytes_read,
            failed.bytes_received,
            failed.unrowed_unproven
        ),
        (SIZE as u64, 0, 1),
        "the manifest path reads the seat once and adopts the output: {failed:?}"
    );
    assert_eq!(rows(&corpus), (0, 0), "the failed group recorded nothing");
    assert!(
        record().is_some(),
        "the adopted output carries the capture's record"
    );

    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{resumed:?}");
    assert_eq!(
        (
            resumed.source_bytes_read,
            resumed.unrowed_adopted,
            resumed.unrowed_unproven
        ),
        (0, 1, 0),
        "durable bytes with no row are adopted from the record: {resumed:?}"
    );
    let warm = corpus.run().unwrap();
    assert_eq!(
        (warm.reused, warm.source_bytes_read, warm.unrowed_adopted),
        (1, 0, 0)
    );
}

/// #169 review: a seat whose stat identity moved with its bytes unchanged (a
/// touch) leaves its output with the old capture's record. The manifest
/// adoption refreshes it to the new capture's, so when that adoption's row
/// never commits, the next run still reads 0 source bytes. A record of
/// another row is not counted unproven.
#[test]
fn a_stale_capture_record_is_refreshed_by_a_manifest_adoption() {
    const SIZE: usize = 70_000;
    let corpus = Corpus::new();
    let seat = corpus.base.join("source/seat");
    std::fs::write(&seat, noise(1692, SIZE)).unwrap();
    let store_root = destination_store_root(&corpus);
    let record = || {
        unrowed::read_record(&std::fs::File::open(corpus.base.join("destination/seat")).unwrap())
            .unwrap()
    };
    let clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 60 * RACY_GRANULARITY_NS);
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{first:?}");
    drop(clock);
    let stale = record();

    let touched = std::time::SystemTime::now() - std::time::Duration::from_hours(1);
    std::fs::File::options()
        .write(true)
        .open(&seat)
        .unwrap()
        .set_modified(touched)
        .unwrap();
    let _clock = PinnedClock::at(&corpus, stamp_ns(&seat) + 60 * RACY_GRANULARITY_NS);
    crate::transfer_store::fail_output_commits(&store_root, true);
    let failed = corpus.run();
    crate::transfer_store::fail_output_commits(&store_root, false);
    let failed = failed.unwrap();
    assert_eq!(failed.refusals.len(), 1, "{failed:?}");
    assert_eq!(
        (
            failed.source_bytes_read,
            failed.unrowed_adopted,
            failed.unrowed_unproven
        ),
        (SIZE as u64, 0, 0),
        "a moved seat is read once, and its old record is not counted: {failed:?}"
    );
    let refreshed = record();
    assert_ne!(refreshed.key, stale.key, "the record names the new row");
    assert_eq!((refreshed.root, refreshed.size), (stale.root, stale.size));

    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{resumed:?}");
    assert_eq!(
        (resumed.source_bytes_read, resumed.unrowed_adopted),
        (0, 1),
        "{resumed:?}"
    );
}

/// #169 review (`AdoptOnlyUnrowed` in docs/formal): the record's adoption is
/// for outputs with no row. A clean rerun answers every rowed output `Reuse`
/// from its row: it adopts nothing, proves nothing, and so hashes no
/// destination byte, whatever the source-read counter says. A regression of
/// `Store::output_matches` would otherwise hide behind the adoption, which
/// also reads 0 source bytes.
#[test]
fn a_clean_rerun_reuses_rowed_outputs_and_adopts_none() {
    const SIZES: [usize; 4] = [0, 1, 70_000, 300_000];
    let corpus = Corpus::new();
    for (index, size) in SIZES.iter().enumerate() {
        std::fs::write(
            corpus.base.join(format!("source/file-{index}")),
            noise(1_693 + index as u64, *size),
        )
        .unwrap();
    }
    let latest = (0..SIZES.len())
        .map(|index| stamp_ns(&corpus.base.join(format!("source/file-{index}"))))
        .max()
        .unwrap();
    let _clock = PinnedClock::at(&corpus, latest + 60 * RACY_GRANULARITY_NS);
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{first:?}");
    assert_eq!(rows(&corpus), (SIZES.len() as u64, SIZES.len() as u64));
    for _ in 0..2 {
        let rerun = corpus.run().unwrap();
        assert!(rerun.refusals.is_empty(), "{rerun:?}");
        assert_eq!(
            (
                rerun.reused,
                rerun.unrowed_adopted,
                rerun.unrowed_unproven,
                rerun.source_bytes_read,
                rerun.bytes_received
            ),
            (SIZES.len() as u64, 0, 0, 0, 0),
            "{rerun:?}"
        );
    }
}

/// #169 review: the record's key holds no store authority, so a source store
/// that was recreated (a new authority, which re-keys every row on both
/// sides) still finds its outputs' records. Unrowed outputs, and rowed ones
/// whose rows the new authority no longer matches, are adopted from their
/// records: 0 source bytes, where every seat was read again before.
#[test]
fn a_recreated_source_store_adopts_from_capture_records() {
    const FILES: usize = 3;
    const SIZE: usize = 80_000;
    let corpus = Corpus::new();
    for index in 0..FILES {
        std::fs::write(
            corpus.base.join(format!("source/file-{index}")),
            noise(1_697 + index as u64, SIZE),
        )
        .unwrap();
    }
    let latest = (0..FILES)
        .map(|index| stamp_ns(&corpus.base.join(format!("source/file-{index}"))))
        .max()
        .unwrap();
    let _clock = PinnedClock::at(&corpus, latest + 60 * RACY_GRANULARITY_NS);
    let store_root = destination_store_root(&corpus);
    let recreate = || std::fs::remove_dir_all(corpus.base.join("source-state")).unwrap();

    // Unrowed: published by a group whose store commit failed.
    crate::transfer_store::fail_output_commits(&store_root, true);
    let failed = corpus.run();
    crate::transfer_store::fail_output_commits(&store_root, false);
    assert_eq!(failed.unwrap().refusals.len(), FILES);
    recreate();
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{resumed:?}");
    assert_eq!(
        (resumed.source_bytes_read, resumed.unrowed_adopted),
        (0, FILES as u64),
        "{resumed:?}"
    );

    // Rowed, under the authority that is about to be lost.
    recreate();
    let rekeyed = corpus.run().unwrap();
    assert!(rekeyed.refusals.is_empty(), "{rekeyed:?}");
    assert_eq!(
        (
            rekeyed.source_bytes_read,
            rekeyed.reused,
            rekeyed.unrowed_adopted
        ),
        (0, 0, FILES as u64),
        "{rekeyed:?}"
    );
    let warm = corpus.run().unwrap();
    assert_eq!(
        (warm.reused, warm.source_bytes_read, warm.unrowed_adopted),
        (FILES as u64, 0, 0)
    );
}

// ---- WP0(g) (OI-1003-Q20, Q37, Q104): P79 RELAXED-LEDGER-LOSS --------------
mod wp0g;
