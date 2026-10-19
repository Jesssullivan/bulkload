//! S1 levers (OI-1003-Q154): verify once in loopback (lever 1) and the
//! loosened in-process wire (lever 2). The counter-exact proofs, which need
//! a process of their own, are in `tests/loopback_handoff.rs`.

use super::*;

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

/// A source half's output that corrupts the first payload byte of its
/// `nth` data frame (from 1): a transport fault between two hosts. `serve`
/// flushes each frame as it writes it, so a flush ends a frame.
struct Corrupting<W> {
    output: W,
    frame: Vec<u8>,
    seen: usize,
    nth: usize,
}

impl<W: Write> Write for Corrupting<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.frame.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut frame = std::mem::take(&mut self.frame);
        if let Ok((Frame::Data { .. }, _)) = Frame::decode(&frame) {
            self.seen += 1;
            if self.seen == self.nth {
                frame[FRAME_HEADER_BYTES + DATA_HEADER_BYTES] ^= 0xff;
            }
        }
        self.output.write_all(&frame)?;
        self.output.flush()
    }
}

/// A session between the public `serve` and `receive`, as `pull` runs one
/// across hosts (no hand-off), with the `nth` data frame corrupted.
fn corrupted_session(corpus: &Corpus, nth: usize) -> Result<TransferStats> {
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let served = serve(
                input,
                &mut Corrupting {
                    output: sender,
                    frame: Vec::new(),
                    seen: 0,
                    nth,
                },
            );
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
        let _ = producer.join().unwrap();
        outcome
    })
}

/// The cross-host property: the corrupted file is refused
/// `DIGEST_MISMATCH` and nothing of it is published.
fn corruption_refused(corpus: &Corpus, stats: &TransferStats) -> std::result::Result<(), String> {
    let refused = stats.refusals == [(b"f".to_vec(), "DIGEST_MISMATCH".to_owned())];
    let published = corpus.base.join("destination/f").exists();
    if refused && !published {
        Ok(())
    } else {
        Err(format!(
            "refusals {:?}, published {published}",
            stats.refusals
        ))
    }
}

/// Lever 1 (OI-1003-Q154): a session between processes or hosts never
/// holds the hand-off, so the destination hashes every byte and refuses a
/// chunk corrupted in transit. The mutation that takes transport bytes as
/// vouched for (a verify-once applied off the loopback) lands the corrupt
/// byte, and the same property catches it: the proof has teeth.
#[test]
fn a_cross_host_session_verifies_every_byte_and_its_mutation_is_caught() {
    let data = noise(301, 2 * 1024 * 1024);
    let run = |mutated: bool| {
        let corpus = Corpus::new();
        std::fs::write(corpus.base.join("source/f"), &data).unwrap();
        settle_racy_window(&corpus.base.join("source")).unwrap();
        let root = canonical(&corpus.base.join("destination"));
        if mutated {
            TRUST_WIRE.lock().unwrap().push(root.clone());
        }
        let stats = corrupted_session(&corpus, 3).unwrap();
        TRUST_WIRE.lock().unwrap().retain(|at| *at != root);
        let landed = std::fs::read(corpus.base.join("destination/f")).ok();
        (corruption_refused(&corpus, &stats), landed)
    };
    let (verified, _) = run(false);
    assert_eq!(verified, Ok(()), "the destination's verify refuses it");
    let (mutated, landed) = run(true);
    assert!(mutated.is_err(), "the mutation must fail the property");
    assert!(
        landed.is_some(),
        "under the mutation the corrupt file is published"
    );
    let landed = landed.unwrap();
    assert_ne!(landed, data, "and it holds the corrupted byte");
    assert_eq!(landed.len(), data.len());
}

/// Lever 1 on a delta (review of OI-1003-Q154): the chunks a cross-host
/// destination requests (`NeedChunks`, for a manifest it fills from its
/// own outputs) are hashed as they arrive, like streamed ones. The first
/// data frame of the second session answers `NeedChunks`; corrupted in
/// transit, it is refused `DIGEST_MISMATCH` and the previous output stays.
/// Under `TRUST_WIRE` (a verify-once applied off the loopback) the corrupt
/// file lands and the property fails: the manifest-fill path's verify has
/// a proof of its own.
#[test]
fn a_cross_host_delta_verifies_every_requested_chunk_and_its_mutation_is_caught() {
    let old = noise(307, 2 * 1024 * 1024);
    let mut new = old.clone();
    new[900_000..1_000_000].copy_from_slice(&noise(308, 100_000));
    let run = |mutated: bool| {
        let corpus = Corpus::new();
        std::fs::write(corpus.base.join("source/f"), &old).unwrap();
        settle_racy_window(&corpus.base.join("source")).unwrap();
        let first = corrupted_session(&corpus, usize::MAX).unwrap();
        assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        assert_eq!(
            std::fs::read(corpus.base.join("destination/f")).unwrap(),
            old
        );
        std::fs::write(corpus.base.join("source/f"), &new).unwrap();
        settle_racy_window(&corpus.base.join("source")).unwrap();
        let root = canonical(&corpus.base.join("destination"));
        if mutated {
            TRUST_WIRE.lock().unwrap().push(root.clone());
        }
        let delta = corrupted_session(&corpus, 1).unwrap();
        TRUST_WIRE.lock().unwrap().retain(|at| *at != root);
        let landed = std::fs::read(corpus.base.join("destination/f")).unwrap();
        (delta, landed)
    };
    let (delta, landed) = run(false);
    assert!(
        delta.bytes_received > 0 && delta.bytes_received < new.len() as u64 / 2,
        "a manifest filled locally, the rest requested: {delta:?}"
    );
    assert_eq!(
        delta.refusals,
        [(b"f".to_vec(), "DIGEST_MISMATCH".to_owned())],
        "the destination's verify refuses the requested chunk"
    );
    assert_eq!(landed, old, "nothing of the corrupt delta is published");
    let (delta, landed) = run(true);
    assert!(
        delta.refusals.is_empty(),
        "the mutation must fail the property: {:?}",
        delta.refusals
    );
    assert_eq!(landed.len(), new.len());
    assert_ne!(landed, new, "under the mutation the corrupt delta lands");
    assert_ne!(landed, old);
}

/// Lever 2: a handed frame is no wire v6 frame. A receiver without the
/// hand-off (any `receive`, `pull` included) refuses it `FRAME_CODEC`, so a
/// peer that claims the hand-off over a transport ends the session.
#[test]
fn a_receiver_without_the_hand_off_refuses_a_handed_frame() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/f"), noise(302, 300_000)).unwrap();
    settle_racy_window(&corpus.base.join("source")).unwrap();
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    let (hand, taken) = handoff::pair();
    let outcome = std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let mut sender = sender;
            let served = serve_session(input, &mut sender, Some(hand), RootKind::Tree);
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
        let _ = producer.join().unwrap();
        outcome
    });
    drop(taken);
    assert_eq!(outcome.unwrap_err(), BulkloadRefusal::FrameCodec);
    assert!(!corpus.base.join("destination/f").exists());
}

/// Lever 1's rule: only a handed chunk whose own digest is the frame's is
/// vouched for; bytes read from a transport never are, and a chunk can only
/// be made by hashing its bytes.
#[test]
fn only_a_handed_chunk_of_the_frames_digest_is_vouched_for() {
    let data = noise(303, 70_000);
    let chunk = handoff::SourceChunk::hashed(data.clone());
    let digest = *blake3::hash(&data).as_bytes();
    assert_eq!(*chunk.digest(), digest);
    assert_eq!(chunk.bytes(), data.as_slice());
    assert!(handoff::SourceChunk::checked(data.clone(), &digest).is_some());
    assert!(handoff::SourceChunk::checked(data.clone(), &[0; 32]).is_none());
    assert!(Payload::Handed(chunk.clone()).vouched(&digest));
    assert!(!Payload::Handed(chunk).vouched(&[0; 32]));
    assert!(!Payload::Wire(data).vouched(&digest));
}

/// Levers 1 and 2 change no outcome: a local copy with the hand-off and
/// with the plain protocol carry the same tree, the same stats and the
/// same chunk counts, for a first copy (streamed) and for a delta whose
/// changed file is filled from local chunks and requested ones (a manifest,
/// then handed chunks from the retained capture).
#[test]
fn the_hand_off_and_the_plain_protocol_carry_the_same_tree() {
    let run = |handoff: bool| {
        let corpus = Corpus::new();
        let shared = noise(304, 600_000);
        std::fs::write(corpus.base.join("source/a"), &shared).unwrap();
        std::fs::write(
            corpus.base.join("source/b"),
            [shared.as_slice(); 2].concat(),
        )
        .unwrap();
        std::fs::write(corpus.base.join("source/empty"), b"").unwrap();
        std::fs::create_dir(corpus.base.join("source/d")).unwrap();
        std::fs::write(corpus.base.join("source/d/c"), noise(305, 1_500_000)).unwrap();
        settle_racy_window(&corpus.base.join("source")).unwrap();
        let root = canonical(&corpus.base.join("destination"));
        set_loopback_handoff(&root, Some(handoff));
        let first = corpus.run().unwrap();
        // A delta: the middle of `d/c` changes.
        let mut changed = noise(305, 1_500_000);
        changed[700_000..760_000].copy_from_slice(&noise(306, 60_000));
        std::fs::write(corpus.base.join("source/d/c"), &changed).unwrap();
        settle_racy_window(&corpus.base.join("source")).unwrap();
        let delta = corpus.run().unwrap();
        set_loopback_handoff(&root, None);
        let mut files = Vec::new();
        for name in ["a", "b", "empty", "d/c"] {
            let carried = std::fs::read(corpus.base.join("destination").join(name)).unwrap();
            assert_eq!(
                carried,
                std::fs::read(corpus.base.join("source").join(name)).unwrap(),
                "{name}, hand-off {handoff}"
            );
            files.push(carried);
        }
        let summary = |stats: &TransferStats| {
            (
                stats.completed,
                stats.reused,
                stats.bytes_received,
                stats.source_bytes_read,
                stats.refusals.clone(),
            )
        };
        (summary(&first), summary(&delta), files)
    };
    let handed = run(true);
    assert!(
        handed.1 .2 > 0,
        "the delta requested chunks: {:?}",
        handed.1
    );
    assert!(
        handed.1 .2 < 1_500_000 / 2,
        "the delta was filled locally: {:?}",
        handed.1
    );
    assert_eq!(handed, run(false));
}

/// The delta lever (OI-1003-Q154 review) keeps the manifest in a session
/// that salvaged a temporary: a temporary's name does not say which path
/// it was staged for. A crashed session staged and sealed a new seat
/// whole, but neither its rename nor its reuse row committed; the resume,
/// with the lever on, asks for that seat's manifest and fills it from the
/// salvaged temporary: nothing received, the source read once for the
/// manifest. Streamed, it would have been received whole.
#[test]
fn the_delta_lever_keeps_the_manifest_when_a_temporary_was_salvaged() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/other"), noise(41, 300_000)).unwrap();
    assert!(corpus.run().unwrap().refusals.is_empty());
    let held = noise(42, 2 << 20);
    std::fs::write(corpus.base.join("source/held"), &held).unwrap();
    std::fs::write(own_temporary(&corpus, 1, 1), &held).unwrap();
    let root = canonical(&corpus.base.join("destination"));
    set_local_new_path_stream(&root, Some(true));
    let (resumed, decided) = decisions(|| corpus.run());
    set_local_new_path_stream(&root, None);
    let resumed = resumed.unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(decided, (1, 0), "asked by manifest, none streamed");
    assert_eq!((resumed.reused, resumed.completed), (1, 1), "{resumed:?}");
    assert_eq!(
        resumed.bytes_received, 0,
        "filled from the salvaged temporary: {resumed:?}"
    );
    assert_eq!(resumed.source_bytes_read, held.len() as u64, "{resumed:?}");
    assert_eq!(
        std::fs::read(corpus.base.join("destination/held")).unwrap(),
        held
    );
}
