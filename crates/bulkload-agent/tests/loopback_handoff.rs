//! OI-1003-Q154 levers 1 and 2, proved with the process-scope counters.
//!
//! - A local `copy` hands every chunk over in process and hashes it once,
//!   on the source (`wire_handed_bytes`); with the hand-off off it hashes
//!   every byte on both sides, as before.
//! - A cross-process session (`bulkload-agent serve` in a child process,
//!   `transfer::receive` here: `pull` without `ssh`) never holds the
//!   hand-off: every frame it writes is a wire v6 frame the proto codec
//!   decodes, the destination hashes every received byte, and a chunk
//!   corrupted between the processes is refused `DIGEST_MISMATCH`.
//!
//! - The delta lever (OI-1003-Q154, off by default,
//!   `BULKLOAD_LOCAL_NEW_PATH_STREAM=1`): a local `copy` streams a new path
//!   into a destination whose published outputs hold chunks
//!   (`transfer_new_paths_streamed`), where it asked for its manifest
//!   first before; an existing path, a seat whose output a third party
//!   removed, and a seat the source's ledger captured for another
//!   destination still ask for their manifest, and so does every new path
//!   with the lever off, with the hand-off off, and in a cross-process
//!   session, even with the lever on for its destination, which still
//!   fills a new path from the destination's own outputs and verifies every
//!   byte it receives. The lever's tests turn it on for their own
//!   destination root only; the default arm follows the variable, so the
//!   suite also passes with `BULKLOAD_LOCAL_NEW_PATH_STREAM=1` exported
//!   (the `delta-lever` arm of `just fault-harness` runs it so first).
//!
//! The tests share the process's counters, so they run one at a time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::transfer::{
    copy, read_frame, receive, set_local_new_path_stream, set_loopback_handoff, settle_racy_window,
    TransferStats, LOCAL_NEW_PATH_STREAM_ENV,
};
use bulkload_proto::frame::{
    Frame, DATA_HEADER_BYTES, FRAME_HEADER_BYTES, LENGTH_PREFIX_BYTES, TAG_CONTROL, TAG_DATA,
    TAG_PACK_DATA,
};

static SERIAL: Mutex<()> = Mutex::new(());

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("bulkload-handoff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("source/nested")).unwrap();
        std::fs::create_dir(base.join("destination")).unwrap();
        Self(base)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

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

/// The source tree: three files of many chunks each. Returns its names and
/// payload bytes.
fn populate(scratch: &Scratch) -> (Vec<&'static str>, u64) {
    let files: [(&str, u64, usize); 3] = [
        ("big", 1, 3 << 20),
        ("small", 2, 700 << 10),
        ("nested/mid", 3, 3 << 19),
    ];
    let mut payload = 0;
    for (name, seed, length) in files {
        std::fs::write(scratch.path("source").join(name), noise(seed, length)).unwrap();
        payload += length as u64;
    }
    settle_racy_window(&scratch.path("source")).unwrap();
    (files.iter().map(|(name, _, _)| *name).collect(), payload)
}

fn same_files(scratch: &Scratch, names: &[&str]) {
    for name in names {
        assert_eq!(
            std::fs::read(scratch.path("source").join(name)).unwrap(),
            std::fs::read(scratch.path("destination").join(name)).unwrap(),
            "{name}"
        );
    }
}

fn local_copy(scratch: &Scratch) -> (TransferStats, Counters) {
    let before = Counters::snapshot();
    let stats = copy(
        &scratch.path("source"),
        &scratch.path("destination"),
        &scratch.path("source-state"),
        &scratch.path("destination-state"),
    )
    .unwrap();
    (stats, Counters::snapshot().since(before))
}

/// Lever 1 and 2: a local `copy` takes every chunk from its in-process
/// source and hashes none of it again; turned off, it hashes every byte.
#[test]
fn a_local_copy_hands_every_chunk_over_and_hashes_it_once() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("local");
    let (names, payload) = populate(&scratch);
    let (stats, counted) = local_copy(&scratch);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    same_files(&scratch, &names);
    assert_eq!(stats.bytes_received, payload, "every byte once");
    assert_eq!(
        counted.get(Counter::WireHandedBytes),
        payload,
        "every chunk handed over: {}",
        counted.render()
    );
    assert_eq!(
        counted.get(Counter::HashWireVerify),
        0,
        "no handed byte hashed again: {}",
        counted.render()
    );
    assert!(counted.get(Counter::HashCaptureChunk) >= payload);
    // Only headers cross the socket: far less than the payload.
    assert!(
        counted.get(Counter::WireBytesSent) < payload / 4,
        "{}",
        counted.render()
    );

    // The same copy into a fresh destination with the hand-off off: the
    // plain protocol, every byte over the socket and hashed on both sides.
    std::fs::remove_dir_all(scratch.path("destination")).unwrap();
    std::fs::remove_dir_all(scratch.path("destination-state")).unwrap();
    std::fs::create_dir(scratch.path("destination")).unwrap();
    let root = std::fs::canonicalize(scratch.path("destination")).unwrap();
    set_loopback_handoff(&root, Some(false));
    let (plain, counted) = local_copy(&scratch);
    set_loopback_handoff(&root, None);
    assert!(plain.refusals.is_empty(), "{:?}", plain.refusals);
    same_files(&scratch, &names);
    assert_eq!(plain.bytes_received, payload);
    assert_eq!(counted.get(Counter::WireHandedBytes), 0);
    assert_eq!(
        counted.get(Counter::HashWireVerify),
        payload,
        "every received byte verified: {}",
        counted.render()
    );
    assert!(counted.get(Counter::WireBytesSent) > payload);
}

/// What the relay saw of the source's stream.
#[derive(Default, Debug)]
struct Seen {
    frames: usize,
    data_frames: usize,
    data_bytes: u64,
    tags: Vec<u8>,
}

/// Carry the child `serve`'s stdout to `into`, frame by frame. Every frame
/// must decode with the wire v6 codec; the `corrupt`-th data frame (from 0)
/// has its first payload byte flipped on the way.
fn relay(mut from: impl Read, mut into: UnixStream, corrupt: Option<usize>) -> Seen {
    let mut seen = Seen::default();
    loop {
        let mut prefix = [0_u8; FRAME_HEADER_BYTES];
        if from.read_exact(&mut prefix).is_err() {
            return seen;
        }
        let length = u32::from_be_bytes(prefix[..LENGTH_PREFIX_BYTES].try_into().unwrap()) as usize;
        let mut frame = prefix.to_vec();
        frame.resize(LENGTH_PREFIX_BYTES + length, 0);
        from.read_exact(&mut frame[FRAME_HEADER_BYTES..]).unwrap();
        let tag = prefix[LENGTH_PREFIX_BYTES];
        // The wire v6 codec decodes every frame `serve` writes.
        let (decoded, used) = Frame::decode(&frame)
            .unwrap_or_else(|refusal| panic!("tag {tag:#x} is no wire v6 frame: {refusal:?}"));
        assert_eq!(used, frame.len());
        if !seen.tags.contains(&tag) {
            seen.tags.push(tag);
        }
        seen.frames += 1;
        if let Frame::Data { payload, .. } = decoded {
            if corrupt == Some(seen.data_frames) {
                frame[FRAME_HEADER_BYTES + DATA_HEADER_BYTES] ^= 0xff;
            }
            seen.data_frames += 1;
            seen.data_bytes += payload.len() as u64;
        }
        if into.write_all(&frame).is_err() {
            return seen;
        }
    }
}

/// `pull` without `ssh`: the source half in a child `serve` process, the
/// destination half here, joined by its pipes through [`relay`].
fn cross_process(scratch: &Scratch, corrupt: Option<usize>) -> (TransferStats, Counters, Seen) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let from = child.stdout.take().unwrap();
    let mut to_source = child.stdin.take().unwrap();
    let (mut input, into) = UnixStream::pair().unwrap();
    let relaying = std::thread::spawn(move || relay(from, into, corrupt));
    let before = Counters::snapshot();
    let stats = receive(
        &mut input,
        &mut to_source,
        &scratch.path("source"),
        &scratch.path("source-state"),
        &scratch.path("destination"),
        &scratch.path("destination-state"),
    )
    .unwrap();
    let counted = Counters::snapshot().since(before);
    drop(to_source);
    drop(input);
    let seen = relaying.join().unwrap();
    assert!(child.wait().unwrap().success());
    (stats, counted, seen)
}

/// Lever 1's cross-host half: a session between two processes writes only
/// wire v6 frames and the destination hashes every byte it receives.
#[test]
fn a_cross_process_session_stays_on_wire_v6_and_verifies_every_byte() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("cross");
    let (names, payload) = populate(&scratch);
    let (stats, counted, seen) = cross_process(&scratch, None);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    same_files(&scratch, &names);
    assert!(seen.data_frames > 3, "{seen:?}");
    assert_eq!(seen.data_bytes, payload, "every chunk on the wire");
    let mut tags = seen.tags;
    tags.sort_unstable();
    assert!(
        tags.iter()
            .all(|tag| [TAG_CONTROL, TAG_DATA, TAG_PACK_DATA].contains(tag)),
        "only wire v6 tags: {tags:?}"
    );
    assert_eq!(stats.bytes_received, payload);
    assert_eq!(
        counted.get(Counter::WireHandedBytes),
        0,
        "nothing handed over"
    );
    assert_eq!(
        counted.get(Counter::HashWireVerify),
        payload,
        "every received byte verified: {}",
        counted.render()
    );
}

/// Lever 1 on a cross-process delta: the chunks the destination requests
/// for a manifest it fills locally (`NeedChunks`) are hashed as they
/// arrive, every one of them, and none is handed over.
#[test]
fn a_cross_process_delta_verifies_every_requested_byte() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("delta");
    let (names, payload) = populate(&scratch);
    let (first, _, _) = cross_process(&scratch, None);
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(first.bytes_received, payload);
    // A delta: the middle of `big` changes.
    let big = scratch.path("source").join("big");
    let mut changed = std::fs::read(&big).unwrap();
    changed[1_500_000..1_600_000].copy_from_slice(&noise(4, 100_000));
    std::fs::write(&big, &changed).unwrap();
    settle_racy_window(&scratch.path("source")).unwrap();
    let (delta, counted, seen) = cross_process(&scratch, None);
    assert!(delta.refusals.is_empty(), "{:?}", delta.refusals);
    same_files(&scratch, &names);
    assert!(
        delta.bytes_received > 0 && delta.bytes_received < changed.len() as u64 / 2,
        "a manifest filled locally, the rest requested: {delta:?}"
    );
    assert_eq!(seen.data_bytes, delta.bytes_received, "{seen:?}");
    assert_eq!(
        counted.get(Counter::WireHandedBytes),
        0,
        "nothing handed over"
    );
    assert_eq!(
        counted.get(Counter::HashWireVerify),
        delta.bytes_received,
        "every requested byte verified: {}",
        counted.render()
    );
}

/// The same session with one chunk corrupted between the processes: the
/// destination's own verify refuses that file, and the rest arrive.
#[test]
fn a_chunk_corrupted_between_processes_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("corrupt");
    let (names, _) = populate(&scratch);
    let (stats, _, seen) = cross_process(&scratch, Some(1));
    assert!(seen.data_frames > 3, "{seen:?}");
    assert_eq!(stats.refusals.len(), 1, "{:?}", stats.refusals);
    let (refused, code) = &stats.refusals[0];
    assert_eq!(code, "DIGEST_MISMATCH");
    let refused = String::from_utf8(refused.clone()).unwrap();
    assert!(!scratch.path("destination").join(&refused).exists());
    let kept: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| *name != refused)
        .collect();
    assert_eq!(kept.len(), names.len() - 1, "{refused}");
    same_files(&scratch, &kept);
}

/// A handed frame (the loopback hand-off's tag, `TAG_HANDED` = 0xB1 in
/// `transfer/handoff.rs`) is no wire v6 frame: the proto codec and the
/// agent's public frame reader both refuse it, so no transport can carry
/// one into a session.
#[test]
fn a_handed_frame_is_no_wire_v6_frame() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let mut handed = u32::try_from(1 + DATA_HEADER_BYTES)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    handed.push(0xB1);
    handed.resize(FRAME_HEADER_BYTES + DATA_HEADER_BYTES, 0);
    assert!(Frame::decode(&handed).is_err());
    assert_eq!(
        read_frame(&mut handed.as_slice()).unwrap_err().code(),
        "FRAME_CODEC"
    );
    // The same bytes under the data tag are a (zero-length) v6 data frame.
    handed[LENGTH_PREFIX_BYTES] = TAG_DATA;
    assert!(Frame::decode(&handed).is_ok());
}

/// Add `name` to the source with `data`, past the racy allowance.
fn add_source_file(scratch: &Scratch, name: &str, data: &[u8]) {
    std::fs::write(scratch.path("source").join(name), data).unwrap();
    settle_racy_window(&scratch.path("source")).unwrap();
}

/// A local `copy` with every decision counter, checked to leave no refusal.
fn clean_local_copy(scratch: &Scratch) -> (TransferStats, Counters) {
    let (stats, counted) = local_copy(scratch);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    (stats, counted)
}

/// Turn the delta lever on (`Some(true)`), off, or back to its default
/// (`None`: off) for `scratch`'s destination.
fn lever(scratch: &Scratch, on: Option<bool>) -> PathBuf {
    let root = std::fs::canonicalize(scratch.path("destination")).unwrap();
    set_local_new_path_stream(&root, on);
    root
}

/// The delta lever (OI-1003-Q154): a new path copied into a destination
/// that already holds outputs is streamed, as into an empty one. The source
/// reads it once, with no manifest capture first
/// (`source_ledger_miss_reads` 0), and no destination byte is read to fill
/// it. A new file whose bytes the destination already holds (`twin`) is
/// streamed too: a local fill would save no wire byte here. An unchanged
/// rerun then reads 0 source bytes (S3, R25).
#[test]
fn a_new_path_in_a_populated_local_destination_is_streamed() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("new-path");
    let (mut names, _) = populate(&scratch);
    lever(&scratch, Some(true));
    let (first, counted) = clean_local_copy(&scratch);
    assert_eq!(first.completed, names.len() as u64);
    assert_eq!(
        counted.get(Counter::TransferManifestsAsked),
        0,
        "an empty store streams every entry: {}",
        counted.render()
    );
    let fresh = noise(5, 2 << 20);
    add_source_file(&scratch, "fresh", &fresh);
    let twin = std::fs::read(scratch.path("source").join("big")).unwrap();
    add_source_file(&scratch, "twin", &twin);
    names.extend(["fresh", "twin"]);
    let added = (fresh.len() + twin.len()) as u64;

    let (stats, counted) = clean_local_copy(&scratch);
    same_files(&scratch, &names);
    assert_eq!(
        counted.get(Counter::SourceLedgerMissReads),
        0,
        "no manifest capture before the first byte is placed: {}",
        counted.render()
    );
    assert_eq!(counted.get(Counter::TransferManifestsAsked), 0);
    assert_eq!(counted.get(Counter::TransferNewPathsStreamed), 2);
    assert_eq!((stats.completed, stats.reused), (2, 3), "{stats:?}");
    // R25: each new seat read exactly once, and nothing else read.
    assert_eq!(stats.source_bytes_read, added, "{stats:?}");
    assert_eq!(counted.get(Counter::SourceFileRead), added);
    assert_eq!(stats.bytes_received, added);
    assert_eq!(counted.get(Counter::WireHandedBytes), added);
    assert_eq!(counted.get(Counter::HashWireVerify), 0);
    assert_eq!(
        counted.get(Counter::DestLocalReuseRead),
        0,
        "no destination byte read to fill a new path: {}",
        counted.render()
    );

    // S3: the unchanged rerun reads nothing.
    let (rerun, counted) = clean_local_copy(&scratch);
    assert_eq!(
        (rerun.source_bytes_read, rerun.reused, rerun.completed),
        (0, names.len() as u64, 0),
        "{rerun:?}"
    );
    assert_eq!(counted.get(Counter::SourceFileRead), 0);
    assert_eq!(counted.get(Counter::TransferNewPathsStreamed), 0);

    // A third party removes `twin`'s output. Its reuse row is still here, so
    // it is no new seat: it is asked for by manifest, which the source
    // answers from its ledger without a read, and filled from `big`'s output
    // (P79): 0 source bytes, 0 received.
    std::fs::remove_file(scratch.path("destination").join("twin")).unwrap();
    let (lost, counted) = clean_local_copy(&scratch);
    same_files(&scratch, &names);
    assert_eq!(
        counted.get(Counter::TransferManifestsAsked),
        1,
        "{}",
        counted.render()
    );
    assert_eq!(counted.get(Counter::TransferNewPathsStreamed), 0);
    assert_eq!(counted.get(Counter::SourceLedgerMissReads), 0);
    assert_eq!(
        (lost.source_bytes_read, lost.bytes_received, lost.completed),
        (0, 0, 1),
        "{lost:?}"
    );
    lever(&scratch, None);
}

/// The lever leaves an existing path alone: a changed file whose old output
/// is this store's own still asks for its manifest, is filled from that
/// output, and takes only the changed chunks from the source (the
/// superseding publish, #187), reading the seat once.
#[test]
fn an_existing_path_in_a_local_copy_still_asks_for_its_manifest() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("existing-path");
    let (names, _) = populate(&scratch);
    lever(&scratch, Some(true));
    clean_local_copy(&scratch);
    let big = scratch.path("source").join("big");
    let mut changed = std::fs::read(&big).unwrap();
    changed[1_500_000..1_600_000].copy_from_slice(&noise(6, 100_000));
    add_source_file(&scratch, "big", &changed);

    let (delta, counted) = clean_local_copy(&scratch);
    same_files(&scratch, &names);
    assert_eq!(
        counted.get(Counter::TransferManifestsAsked),
        1,
        "{}",
        counted.render()
    );
    assert_eq!(counted.get(Counter::TransferNewPathsStreamed), 0);
    assert_eq!(counted.get(Counter::SourceLedgerMissReads), 1);
    assert_eq!(delta.source_bytes_read, changed.len() as u64, "{delta:?}");
    assert!(
        delta.bytes_received > 0 && delta.bytes_received < changed.len() as u64 / 2,
        "filled from the old output, the rest requested: {delta:?}"
    );
    assert!(counted.get(Counter::DestLocalReuseRead) > 0);
    assert_eq!(counted.get(Counter::OutputsSuperseded), 1);

    let (rerun, _) = clean_local_copy(&scratch);
    lever(&scratch, None);
    assert_eq!(
        (rerun.source_bytes_read, rerun.reused),
        (0, names.len() as u64)
    );
}

/// Whether this process runs with the delta lever on by default:
/// `BULKLOAD_LOCAL_NEW_PATH_STREAM=1` exported, as the `delta-lever` arm of
/// `just fault-harness` and an informational bench series run it.
fn lever_on_by_default() -> bool {
    std::env::var(LOCAL_NEW_PATH_STREAM_ENV).is_ok_and(|value| value == "1")
}

/// The A/B arms: by default (the lever off), with the lever turned off, and
/// with the lever on but the hand-off off, a new path in a populated
/// destination is asked for by manifest first, as before the lever. The
/// source still reads it once: the manifest's chunks are kept and served
/// from memory. The default arm follows the variable: with
/// `BULKLOAD_LOCAL_NEW_PATH_STREAM=1` exported it streams the new path,
/// which is how the `delta-lever` arm proves the variable reached the
/// engine.
#[test]
fn a_new_path_asks_for_its_manifest_with_the_lever_or_the_hand_off_off() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("new-path-off");
    let (mut names, _) = populate(&scratch);
    clean_local_copy(&scratch);
    let root = std::fs::canonicalize(scratch.path("destination")).unwrap();
    for (name, seed, stream, handoff) in [
        ("default", 10, None, None),
        ("lever-off", 7, Some(false), None),
        ("handoff-off", 8, Some(true), Some(false)),
    ] {
        let streams = name == "default" && lever_on_by_default();
        let data = noise(seed, 1 << 20);
        add_source_file(&scratch, name, &data);
        names.push(name);
        set_local_new_path_stream(&root, stream);
        set_loopback_handoff(&root, handoff);
        let (stats, counted) = local_copy(&scratch);
        set_local_new_path_stream(&root, None);
        set_loopback_handoff(&root, None);
        assert!(stats.refusals.is_empty(), "{name}: {:?}", stats.refusals);
        same_files(&scratch, &names);
        assert_eq!(
            counted.get(Counter::TransferManifestsAsked),
            u64::from(!streams),
            "{name}: {}",
            counted.render()
        );
        assert_eq!(
            counted.get(Counter::TransferNewPathsStreamed),
            u64::from(streams),
            "{name}"
        );
        assert_eq!(
            counted.get(Counter::SourceLedgerMissReads),
            u64::from(!streams),
            "{name}"
        );
        assert_eq!(
            stats.source_bytes_read,
            data.len() as u64,
            "{name}: {stats:?}"
        );
        assert_eq!(stats.bytes_received, data.len() as u64, "{name}");
    }
}

/// `pull` keeps manifest-first for a new path, with the delta lever on for
/// its destination: across processes the destination holds no hand-off,
/// so the lever never acts. It fills a new file from its own outputs
/// (`twin`, whose bytes it holds, crosses the wire as nothing), asks only
/// for what it lacks, and hashes every byte it receives.
#[test]
fn a_cross_process_new_path_keeps_its_manifest_and_verifies_every_byte() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("cross-new-path");
    let (mut names, _) = populate(&scratch);
    let (first, _, _) = cross_process(&scratch, None);
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    let fresh = noise(9, 2 << 20);
    add_source_file(&scratch, "fresh", &fresh);
    let twin = std::fs::read(scratch.path("source").join("big")).unwrap();
    add_source_file(&scratch, "twin", &twin);
    names.extend(["fresh", "twin"]);

    lever(&scratch, Some(true));
    let (stats, counted, seen) = cross_process(&scratch, None);
    lever(&scratch, None);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    same_files(&scratch, &names);
    assert_eq!(
        counted.get(Counter::TransferManifestsAsked),
        2,
        "{}",
        counted.render()
    );
    assert_eq!(counted.get(Counter::TransferNewPathsStreamed), 0);
    assert_eq!(
        stats.bytes_received,
        fresh.len() as u64,
        "the twin is filled from the destination's own output: {stats:?}"
    );
    assert!(counted.get(Counter::DestLocalReuseRead) >= twin.len() as u64);
    assert_eq!(seen.data_bytes, stats.bytes_received, "{seen:?}");
    assert_eq!(counted.get(Counter::WireHandedBytes), 0);
    assert_eq!(
        counted.get(Counter::HashWireVerify),
        stats.bytes_received,
        "every received byte verified: {}",
        counted.render()
    );
}

/// The delta lever keeps a ledger manifest when one source state is fanned
/// out (OI-1003-Q154 review): `copy` S into this destination, then carry a
/// new file `twin` (the bytes of `big`) from the same source state into a
/// second destination, which records `twin`'s capture in the source's
/// ledger. Copied here again with the lever on, `twin` is new to this
/// destination but held by the source's ledger: it is asked for by
/// manifest, which the source gives from its ledger without a read, and
/// filled from `big`'s output: 0 source bytes, 0 received. `fresh`, new to
/// both stores, is still streamed and read once. The lever off decides the
/// same for `twin`.
#[test]
fn a_new_path_the_source_ledger_holds_keeps_its_ledger_manifest() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    for on in [true, false] {
        let scratch = Scratch::new(if on { "fan-out-on" } else { "fan-out-off" });
        let (mut names, _) = populate(&scratch);
        clean_local_copy(&scratch);
        let twin = std::fs::read(scratch.path("source").join("big")).unwrap();
        add_source_file(&scratch, "twin", &twin);
        std::fs::create_dir(scratch.path("elsewhere")).unwrap();
        let elsewhere = copy(
            &scratch.path("source"),
            &scratch.path("elsewhere"),
            &scratch.path("source-state"),
            &scratch.path("elsewhere-state"),
        )
        .unwrap();
        assert!(elsewhere.refusals.is_empty(), "{:?}", elsewhere.refusals);
        let fresh = noise(11, 1 << 20);
        add_source_file(&scratch, "fresh", &fresh);
        names.extend(["twin", "fresh"]);

        lever(&scratch, Some(on));
        let (stats, counted) = clean_local_copy(&scratch);
        lever(&scratch, None);
        same_files(&scratch, &names);
        assert_eq!((stats.completed, stats.reused), (2, 3), "{on}: {stats:?}");
        assert_eq!(
            counted.get(Counter::TransferManifestsAsked),
            if on { 1 } else { 2 },
            "{on}: twin keeps its manifest: {}",
            counted.render()
        );
        assert_eq!(
            counted.get(Counter::TransferNewPathsStreamed),
            u64::from(on),
            "{on}: only fresh is streamed"
        );
        // `twin` from the ledger, not read; `fresh` read once.
        assert_eq!(
            stats.source_bytes_read,
            fresh.len() as u64,
            "{on}: {stats:?}"
        );
        assert_eq!(
            counted.get(Counter::SourceLedgerMissReads),
            u64::from(!on),
            "{on}: {}",
            counted.render()
        );
        assert_eq!(
            stats.bytes_received,
            fresh.len() as u64,
            "{on}: twin filled from big's output: {stats:?}"
        );
        assert!(counted.get(Counter::DestLocalReuseRead) >= twin.len() as u64);
    }
}
