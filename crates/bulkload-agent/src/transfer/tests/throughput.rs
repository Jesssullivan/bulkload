//! S1 throughput (OI-1003-Q143): the stores opened in parallel (item 1b),
//! write-back started while data streams (item 1a), directory batching
//! (item 2) and the receive workers (item 3), with the #217 review's
//! findings. Every item must leave the carry exactly as it was: the same
//! tree, the same rows, the same `TransferStats` (refusals compared as a
//! sorted list: content refusals arrive in timing order with or without
//! these items), the same decisions.

use super::*;
use crate::counters::{Counter, Counters};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A destination as compared: every path's kind, mode and content digest,
/// and the count of this store's temporaries.
type Tree = (BTreeMap<Vec<u8>, (char, u32, Vec<u8>)>, usize);

/// A destination as the determinism checks compare it: every path's kind,
/// mode and bytes. This store's temporaries are counted, not named (their
/// names hold a pid and serial).
fn tree(root: &Path) -> Tree {
    fn visit(
        root: &Path,
        directory: &Path,
        out: &mut BTreeMap<Vec<u8>, (char, u32, Vec<u8>)>,
        temporaries: &mut usize,
    ) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec();
            if path
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b".bulkload-"))
            {
                *temporaries += 1;
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let mode = meta.mode() & 0o7777;
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                out.insert(rel, ('l', 0, target.as_os_str().as_bytes().to_vec()));
            } else if meta.is_dir() {
                out.insert(rel, ('d', mode, Vec::new()));
                visit(root, &path, out, temporaries);
            } else {
                let digest: [u8; 32] = blake3::hash(&std::fs::read(&path).unwrap()).into();
                out.insert(rel, ('f', mode, digest.to_vec()));
            }
        }
    }
    let mut out = BTreeMap::new();
    let mut temporaries = 0;
    visit(root, root, &mut out, &mut temporaries);
    (out, temporaries)
}

/// The fields of a `TransferStats` the items must keep, refusals sorted.
fn outcome(stats: &TransferStats) -> String {
    let mut refusals: Vec<(String, String)> = stats
        .refusals
        .iter()
        .map(|(rel, code)| (String::from_utf8_lossy(rel).into_owned(), code.clone()))
        .collect();
    refusals.sort();
    let mut left = stats.temporaries_left.clone();
    left.sort();
    let mut fallback = stats.directories_fallback.clone();
    fallback.sort();
    format!(
        "completed={} reused={} bytes_received={} source_bytes_read={} refusals={refusals:?} \
         temporaries_removed={} left={} directories_renamed={} fallback={fallback:?} \
         unrowed_adopted={} unrowed_unproven={} engine={:?}",
        stats.completed,
        stats.reused,
        stats.bytes_received,
        stats.source_bytes_read,
        stats.temporaries_removed,
        left.len(),
        stats.directories_renamed,
        stats.unrowed_adopted,
        stats.unrowed_unproven,
        stats.source_engine_temporaries,
    )
}

/// The destination's control frames, recorded as it sends them: each
/// `write_control` hands one whole frame over and then flushes.
struct Recording<W> {
    output: W,
    frame: Vec<u8>,
    sent: Arc<Mutex<Vec<Control>>>,
}

impl<W: Write> Write for Recording<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let count = self.output.write(data)?;
        self.frame.extend_from_slice(&data[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok((Frame::Control(control), _)) = Frame::decode(&self.frame) {
            self.sent.lock().unwrap().push(control);
        }
        self.frame.clear();
        self.output.flush()
    }
}

/// What one recorded session decided: its `Decide` frames in order, and
/// each entry's chunk request.
#[derive(Debug, PartialEq, Eq)]
struct Decided {
    decides: Vec<(u64, String)>,
    needs: BTreeMap<u64, Vec<u32>>,
    credit: u64,
}

/// `copy`, with the destination's control frames recorded.
fn recorded_copy(corpus: &Corpus) -> (Result<TransferStats>, Decided) {
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let outcome = std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let mut sender = sender;
            let served = serve(input, &mut sender);
            let _ = closer.shutdown(std::net::Shutdown::Both);
            served
        });
        let mut output = Recording {
            output: receiver.try_clone().unwrap(),
            frame: Vec::new(),
            sent: Arc::clone(&sent),
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
        let served = producer.join().unwrap();
        outcome.and_then(|stats| served.map(|()| stats))
    });
    let mut decided = Decided {
        decides: Vec::new(),
        needs: BTreeMap::new(),
        credit: 0,
    };
    for control in sent.lock().unwrap().drain(..) {
        match control {
            Control::Decide { entry, decision } => {
                decided.decides.push((entry, format!("{decision:?}")));
            }
            Control::NeedChunks { entry, indices } => {
                decided.needs.insert(entry, indices);
            }
            Control::Credit { bytes } => decided.credit += bytes,
            _ => {}
        }
    }
    (outcome, decided)
}

/// The digests of `data`'s chunks as a capture sends them.
fn chunk_digests(data: &[u8]) -> Vec<[u8; 32]> {
    fastcdc::v2020::FastCDC::new(
        data,
        crate::hash::CDC_MIN_BYTES,
        crate::hash::CDC_AVG_BYTES,
        crate::hash::CDC_MAX_BYTES,
    )
    .map(|chunk| blake3::hash(&data[chunk.offset..chunk.offset + chunk.length]).into())
    .collect()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

// ---- Item 1b: the two stores opened in parallel ---------------------------

/// #217 review (item 1b): the stores open at once, so `copy` refuses two
/// state roots that overlap each other or a source state inside the
/// destination root, before either store exists.
#[test]
fn copy_refuses_state_roots_that_overlap_each_other_or_the_destination() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/file"), b"bytes").unwrap();
    let (source, destination) = (corpus.base.join("source"), corpus.base.join("destination"));
    let shared = corpus.base.join("state");
    assert_eq!(
        copy(&source, &destination, &shared, &shared).unwrap_err(),
        BulkloadRefusal::SnapshotRootsOverlap
    );
    assert_eq!(
        copy(
            &source,
            &destination,
            &destination.join("state"),
            &corpus.base.join("destination-state")
        )
        .unwrap_err(),
        BulkloadRefusal::SnapshotRootsOverlap
    );
    assert!(std::fs::symlink_metadata(&shared).is_err());
    assert!(std::fs::read_dir(&destination).unwrap().next().is_none());
}

/// Item 1b: `Open` goes out before the destination's setup. A destination
/// whose store refuses to open (a state root others may write) fails with
/// that refusal, not the source's broken stream, and the source, which
/// opened its store meanwhile, has read and recorded nothing.
#[test]
fn a_destination_refused_at_setup_reads_no_source_byte() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/file"), noise(9, 70_000)).unwrap();
    let shared = corpus.base.join("destination-state");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        corpus.run().unwrap_err(),
        BulkloadRefusal::PathEscapesRoot,
        "the destination's own refusal"
    );
    let source_store = Store::open(&corpus.base.join("source-state")).unwrap();
    assert_eq!(source_store.row_counts().unwrap(), (0, 0));
    assert!(std::fs::read_dir(corpus.base.join("destination"))
        .unwrap()
        .next()
        .is_none());
}

// ---- Item 1a: write-back while the data streams ----------------------------

/// The kick fires exactly when the placed bytes cross a multiple of the
/// interval.
#[test]
fn a_kick_follows_each_crossing_of_the_interval() {
    let interval = WRITEBACK_KICK_BYTES;
    for (before, placed, kicks) in [
        (0, interval - 1, false),
        (interval - 1, 1, true),
        (interval, interval - 1, false),
        (0, 3 * interval, true),
        (5, 0, false),
        (interval + 1, interval, true),
    ] {
        assert_eq!(
            crosses(before, placed, interval),
            kicks,
            "{before} + {placed}"
        );
    }
    assert!(!crosses(0, 10, 0), "no interval, no kick");
}

/// Item 1a: a file past the interval kicks its write-back while it streams,
/// and a manifest-filled one right after its local fill, before
/// `NeedChunks` (#217 review, finding 3). Linux group mode; the counter is
/// the process's, so this shows each path kicks, not how often.
#[test]
fn streamed_and_locally_filled_files_kick_their_write_back() {
    if !cfg!(target_os = "linux")
        || crate::io::durable::durability() != crate::io::durable::Durability::Group
    {
        return;
    }
    let corpus = Corpus::new();
    let mut big = noise(
        43,
        usize::try_from(WRITEBACK_KICK_BYTES).unwrap() + 2 * 1024 * 1024,
    );
    std::fs::write(corpus.base.join("source/bigger"), &big).unwrap();
    settle_racy_window(&corpus.base.join("source")).unwrap();
    let before = Counters::snapshot().get(Counter::WritebackKicks);
    let first = corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert!(
        Counters::snapshot().get(Counter::WritebackKicks) > before,
        "a file past the interval kicks while it streams"
    );
    // The delta: its tail changed, the rest filled locally and kicked.
    let len = big.len();
    big[len - 1000..].copy_from_slice(&noise(44, 1000));
    std::fs::write(corpus.base.join("source/bigger"), &big).unwrap();
    settle_racy_window(&corpus.base.join("source")).unwrap();
    let before = Counters::snapshot().get(Counter::WritebackKicks);
    let delta = corpus.run().unwrap();
    assert!(delta.refusals.is_empty(), "{:?}", delta.refusals);
    assert!(
        delta.bytes_received < (len / 4) as u64,
        "the delta is filled locally: {} bytes received",
        delta.bytes_received
    );
    assert!(
        Counters::snapshot().get(Counter::WritebackKicks) > before,
        "the local fill is kicked"
    );
    assert_eq!(
        std::fs::read(corpus.base.join("destination/bigger")).unwrap(),
        big
    );
}

// ---- Item 3: the receive workers -------------------------------------------

/// The determinism fixture: a streamed multi-chunk file, a file of one
/// block repeated (a chunk placed at several offsets), an empty file, a
/// small one, a file whose third chunk arrives corrupted, and one whose
/// output a rerun salvages.
struct Workload {
    big: Vec<u8>,
    bad: Vec<u8>,
}

fn workload(corpus: &Corpus) -> Workload {
    let source = corpus.base.join("source");
    let big = noise(51, 3 * 1024 * 1024 + 77);
    std::fs::write(source.join("big"), &big).unwrap();
    let block = noise(52, 300 * 1024);
    std::fs::write(source.join("dup"), block.repeat(4)).unwrap();
    std::fs::write(source.join("empty"), b"").unwrap();
    std::fs::write(source.join("small"), b"ten bytes!").unwrap();
    let bad = noise(53, 2 * 1024 * 1024);
    std::fs::write(source.join("bad"), &bad).unwrap();
    std::fs::write(source.join("salv"), noise(54, 500 * 1024)).unwrap();
    settle_racy_window(&source).unwrap();
    Workload { big, bad }
}

/// One W's two sessions over the fixture: a first copy, then a rerun with
/// `big`'s tail changed (a manifest filled locally) and `salv`'s output
/// turned back into an orphaned temporary (salvage reuse).
fn sessions(workers: usize) -> Vec<(String, Tree, Decided)> {
    let corpus = Corpus::new();
    let work = workload(&corpus);
    set_recv_workers(&canonical(&corpus.base.join("destination")), Some(workers));
    let mut runs = Vec::new();
    let (first, decided) = recorded_copy(&corpus);
    let first = first.unwrap();
    runs.push((
        outcome(&first),
        tree(&corpus.base.join("destination")),
        decided,
    ));
    let mut big = work.big;
    let len = big.len();
    big[len - 4096..].copy_from_slice(&noise(55, 4096));
    std::fs::write(corpus.base.join("source/big"), &big).unwrap();
    settle_racy_window(&corpus.base.join("source")).unwrap();
    std::fs::rename(
        corpus.base.join("destination/salv"),
        super::own_temporary(&corpus, 1, 7),
    )
    .unwrap();
    let (second, decided) = recorded_copy(&corpus);
    let second = second.unwrap();
    runs.push((
        outcome(&second),
        tree(&corpus.base.join("destination")),
        decided,
    ));
    set_recv_workers(&canonical(&corpus.base.join("destination")), None);
    let _ = work.bad;
    runs
}

/// Item 3 (#217 review, finding 6): with 0, 1, 2 and 4 receive workers, the
/// same fixture gives the same tree, the same stats, the same `Decide`
/// sequence and the same chunk requests, and returns all its credit but the
/// last partial return, a corrupted chunk mid-stream included.
#[test]
fn any_number_of_receive_workers_gives_the_same_carry() {
    let bad = noise(53, 2 * 1024 * 1024);
    let digests = chunk_digests(&bad);
    assert!(digests.len() > 3, "the bad file spans several chunks");
    CORRUPT_DIGESTS.lock().unwrap().push(digests[2]);
    let reference = sessions(0);
    assert!(
        reference[0].0.contains("(\"bad\", \"DIGEST_MISMATCH\")"),
        "the corrupted chunk refuses its file: {}",
        reference[0].0
    );
    assert!(
        reference[1].0.contains("bytes_received="),
        "{}",
        reference[1].0
    );
    for workers in [1, 2, 4] {
        let runs = sessions(workers);
        for (run, (got, want)) in runs.iter().zip(&reference).enumerate() {
            assert_eq!(got.0, want.0, "{workers} workers, run {run}: stats");
            assert_eq!(got.1, want.1, "{workers} workers, run {run}: tree");
            assert_eq!(
                got.2.decides, want.2.decides,
                "{workers} workers, run {run}: decides"
            );
            assert_eq!(
                got.2.needs, want.2.needs,
                "{workers} workers, run {run}: chunk requests"
            );
            // Credit returns at each 1 MiB received, whatever the entry, so
            // its total depends on the frames' arrival order across entries
            // (the capture threads'), never on the workers: every byte but
            // the last partial return is returned.
            let received: u64 = got
                .0
                .split("bytes_received=")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .and_then(|bytes| bytes.parse().ok())
                .unwrap();
            assert!(
                got.2.credit <= CREDIT_WINDOW + received
                    && got.2.credit + CREDIT_RETURN > CREDIT_WINDOW + received,
                "{workers} workers, run {run}: credit {} for {received} bytes",
                got.2.credit
            );
        }
    }
    CORRUPT_DIGESTS
        .lock()
        .unwrap()
        .retain(|digest| *digest != digests[2]);
}

/// #217 review, finding 6: within one chunk, a verify failure comes before
/// a stage failure. A corrupt first chunk of a file whose directory refuses
/// its temporary is refused `DIGEST_MISMATCH`, as before the workers; the
/// same file sent whole is refused by its stage.
#[test]
fn a_corrupt_first_chunk_is_refused_before_its_stage_fails() {
    if crate::io::sys::effective_uid() == 0 {
        return;
    }
    for workers in [0, 2] {
        let corpus = Corpus::new();
        let (source, destination) = (corpus.base.join("source"), corpus.base.join("destination"));
        std::fs::create_dir(source.join("ro")).unwrap();
        let data = noise(61 + workers as u64, 4096);
        std::fs::write(source.join("ro/f"), &data).unwrap();
        std::fs::set_permissions(source.join("ro"), std::fs::Permissions::from_mode(0o500))
            .unwrap();
        std::fs::create_dir(destination.join("ro")).unwrap();
        std::fs::set_permissions(
            destination.join("ro"),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        settle_racy_window(&source).unwrap();
        set_recv_workers(&canonical(&destination), Some(workers));
        let digest = chunk_digests(&data)[0];
        CORRUPT_DIGESTS.lock().unwrap().push(digest);
        let corrupt = corpus.run().unwrap();
        CORRUPT_DIGESTS
            .lock()
            .unwrap()
            .retain(|found| *found != digest);
        let whole = corpus.run().unwrap();
        set_recv_workers(&canonical(&destination), None);
        for directory in [source.join("ro"), destination.join("ro")] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            corrupt.refusals,
            [(b"ro/f".to_vec(), "DIGEST_MISMATCH".to_owned())],
            "{workers} workers"
        );
        assert_eq!(whole.refusals.len(), 1, "{workers} workers");
        assert_ne!(whole.refusals[0].1, "DIGEST_MISMATCH", "{workers} workers");
    }
}

/// Item 3: the receive workers never hold more payload than their budget,
/// each job charged at least its minimum.
#[test]
fn the_receive_workers_stay_within_their_byte_budget() {
    let corpus = Corpus::new();
    std::fs::write(
        corpus.base.join("source/large"),
        noise(71, 24 * 1024 * 1024),
    )
    .unwrap();
    for index in 0..200 {
        std::fs::write(
            corpus.base.join(format!("source/tiny-{index}")),
            [u8::try_from(index).unwrap()],
        )
        .unwrap();
    }
    settle_racy_window(&corpus.base.join("source")).unwrap();
    set_recv_workers(&canonical(&corpus.base.join("destination")), Some(2));
    let stats = corpus.run().unwrap();
    set_recv_workers(&canonical(&corpus.base.join("destination")), None);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    let high = POOL_HIGH_WATER.load(Ordering::Relaxed);
    assert!(high > 0, "the pool was used");
    assert!(high <= RECV_POOL_BYTES, "{high} bytes held at once");
}

/// Rewrites the `nth` data frame the source sends with its index moved on,
/// a protocol violation the destination must end the session for.
struct Misindex<W> {
    output: W,
    frame: Vec<u8>,
    seen: usize,
    nth: usize,
}

impl<W: Write> Write for Misindex<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.frame.extend_from_slice(data);
        Ok(data.len())
    }
    fn write_vectored(&mut self, data: &[IoSlice<'_>]) -> std::io::Result<usize> {
        let mut count = 0;
        for slice in data {
            self.frame.extend_from_slice(slice);
            count += slice.len();
        }
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let frame = std::mem::take(&mut self.frame);
        let bytes = match Frame::decode(&frame) {
            Ok((
                Frame::Data {
                    mut header,
                    payload,
                },
                _,
            )) => {
                self.seen += 1;
                if self.seen == self.nth {
                    header.index += 1;
                }
                [data_prefix(&header).unwrap().as_slice(), &payload].concat()
            }
            _ => frame,
        };
        self.output.write_all(&bytes)?;
        self.output.flush()
    }
}

/// Item 3: a protocol violation with chunk jobs in flight ends the session
/// with that refusal, every worker joined, and a rerun completes.
#[test]
fn a_protocol_violation_with_jobs_in_flight_ends_the_session() {
    let corpus = Corpus::new();
    std::fs::write(corpus.base.join("source/large"), noise(81, 6 * 1024 * 1024)).unwrap();
    settle_racy_window(&corpus.base.join("source")).unwrap();
    set_recv_workers(&canonical(&corpus.base.join("destination")), Some(2));
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    let outcome = std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            let input = sender.try_clone().unwrap();
            let closer = sender.try_clone().unwrap();
            let served = serve(
                input,
                &mut Misindex {
                    output: sender,
                    frame: Vec::new(),
                    seen: 0,
                    nth: 12,
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
    });
    assert_eq!(
        outcome.unwrap_err(),
        BulkloadRefusal::ProtocolStateViolation
    );
    let resumed = corpus.run().unwrap();
    set_recv_workers(&canonical(&corpus.base.join("destination")), None);
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(
        std::fs::read(corpus.base.join("destination/large")).unwrap(),
        noise(81, 6 * 1024 * 1024)
    );
}

/// #217 review, finding 7: a receive worker that panics on its job ends the
/// session with `WORKER_LOST`; nothing waits on it forever.
#[test]
fn a_receive_worker_lost_mid_job_ends_the_session() {
    let corpus = Corpus::new();
    let data = noise(91, 3 * 1024 * 1024);
    std::fs::write(corpus.base.join("source/large"), &data).unwrap();
    settle_racy_window(&corpus.base.join("source")).unwrap();
    set_recv_workers(&canonical(&corpus.base.join("destination")), Some(2));
    let digest = chunk_digests(&data)[3];
    PANIC_DIGESTS.lock().unwrap().push(digest);
    let outcome = corpus.run();
    PANIC_DIGESTS
        .lock()
        .unwrap()
        .retain(|found| *found != digest);
    set_recv_workers(&canonical(&corpus.base.join("destination")), None);
    assert_eq!(outcome.unwrap_err(), BulkloadRefusal::WorkerLost);
}

proptest::proptest! {
    #![proptest_config(crate::test_support::prop_config(6))]

    /// Item 3: whatever the sizes, the repeats and the corrupted chunk, any
    /// number of receive workers gives the inline path's carry.
    #[test]
    fn receive_workers_match_the_inline_path(
        files in proptest::collection::vec((0_usize..1_500_000, 1_usize..4), 1..5),
        corrupt in proptest::option::of((0_usize..5, 0_usize..6)),
        workers in 1_usize..5,
    ) {
        let run = |workers: usize| {
            let corpus = Corpus::new();
            let mut digests = Vec::new();
            for (index, (size, repeats)) in files.iter().enumerate() {
                let block = noise(100 + index as u64, size / repeats);
                let data = block.repeat(*repeats);
                if let Some((file, chunk)) = corrupt {
                    if file == index {
                        if let Some(digest) = chunk_digests(&data).get(chunk) {
                            digests.push(*digest);
                        }
                    }
                }
                std::fs::write(corpus.base.join(format!("source/f{index}")), data).unwrap();
            }
            settle_racy_window(&corpus.base.join("source")).unwrap();
            set_recv_workers(&canonical(&corpus.base.join("destination")), Some(workers));
            CORRUPT_DIGESTS.lock().unwrap().extend(digests.iter().copied());
            let stats = corpus.run().unwrap();
            CORRUPT_DIGESTS
                .lock()
                .unwrap()
                .retain(|digest| !digests.contains(digest));
            set_recv_workers(&canonical(&corpus.base.join("destination")), None);
            (outcome(&stats), tree(&corpus.base.join("destination")))
        };
        proptest::prop_assert_eq!(run(workers), run(0));
    }
}

// ---- Item 2: directory batching --------------------------------------------

/// A directory tree for the batching checks: `(path, mode)` of each
/// directory, a file in some, and directories already at the destination:
/// one this store made and finished in an earlier copy (`old`), a third
/// party's with the source's mode, and one with another mode (refused).
fn batch_tree(corpus: &Corpus, directories: &[(String, u32)], files: &[String]) {
    let source = corpus.base.join("source");
    for (path, _) in directories {
        std::fs::create_dir_all(source.join(path)).unwrap();
    }
    for (index, path) in files.iter().enumerate() {
        std::fs::write(source.join(path), noise(200 + index as u64, 700 + index)).unwrap();
    }
    for (path, mode) in directories.iter().rev() {
        std::fs::set_permissions(source.join(path), std::fs::Permissions::from_mode(*mode))
            .unwrap();
    }
    settle_racy_window(&source).unwrap();
}

/// Build with `build`, then copy at batch cap `cap`: the comparable
/// outcome.
fn batched_run(cap: usize, build: &dyn Fn(&Corpus)) -> (String, Tree) {
    let corpus = Corpus::new();
    build(&corpus);
    let destination = canonical(&corpus.base.join("destination"));
    crate::materialize::set_directory_batch(&destination, Some(cap));
    let stats = corpus.run().unwrap();
    crate::materialize::set_directory_batch(&destination, None);
    (outcome(&stats), tree(&corpus.base.join("destination")))
}

/// Three copies at batch cap `cap`. The first refuses `other` (a third
/// party's directory with another mode), so it finishes no directory and
/// leaves `old` and `old/inner` owned and unfinished; the second, with new
/// directories below them and beside them, adopts them (each an existing
/// directory held while a batch is open, so a batch closes before anything
/// is made inside it, #217 review finding 1); the third, once `other` has
/// the source's mode, finishes every directory.
fn three_runs(cap: usize) -> Vec<(String, Tree)> {
    let corpus = Corpus::new();
    let first: Vec<(String, u32)> = [
        ("old", 0o755),
        ("old/inner", 0o751),
        ("other", 0o755),
        ("same", 0o755),
    ]
    .into_iter()
    .map(|(path, mode)| (path.to_owned(), mode))
    .collect();
    batch_tree(&corpus, &first, &["old/five".to_owned()]);
    for (path, mode) in [("same", 0o755), ("other", 0o700)] {
        let at = corpus.base.join("destination").join(path);
        std::fs::create_dir(&at).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let destination = canonical(&corpus.base.join("destination"));
    crate::materialize::set_directory_batch(&destination, Some(cap));
    let mut runs = Vec::new();
    let stats = corpus.run().unwrap();
    runs.push((outcome(&stats), tree(&corpus.base.join("destination"))));
    let second: Vec<(String, u32)> = [
        ("a", 0o750),
        ("a/b", 0o755),
        ("a/b/c", 0o700),
        ("a/d", 0o751),
        ("e", 0o755),
        ("e/f", 0o755),
        ("old/new", 0o755),
        ("old/inner/deeper", 0o755),
        ("same/new", 0o755),
        ("z", 0o755),
    ]
    .into_iter()
    .map(|(path, mode)| (path.to_owned(), mode))
    .collect();
    let files: Vec<String> = ["a/b/c/one", "a/d/two", "e/three", "z/four", "old/new/six"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    batch_tree(&corpus, &second, &files);
    let stats = corpus.run().unwrap();
    runs.push((outcome(&stats), tree(&corpus.base.join("destination"))));
    std::fs::set_permissions(
        corpus.base.join("destination/other"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let stats = corpus.run().unwrap();
    runs.push((outcome(&stats), tree(&corpus.base.join("destination"))));
    crate::materialize::set_directory_batch(&destination, None);
    runs
}

/// Item 2: batches of 1 (no batching), 2, 3 and 64 new directories give the
/// same trees and the same stats over three copies: owned directories left
/// unfinished and adopted, third-party directories with the source's mode
/// and with another, and the final modes once nothing is refused.
#[test]
fn directory_batches_of_any_size_give_the_same_carry() {
    let reference = three_runs(1);
    assert!(
        reference[0]
            .0
            .contains("(\"other\", \"DESTINATION_OCCUPIED\")"),
        "{}",
        reference[0].0
    );
    assert_eq!(
        reference[2]
            .1
             .0
            .get(b"a/b/c".as_slice())
            .map(|entry| entry.1),
        Some(0o700)
    );
    assert_eq!(
        reference[2]
            .1
             .0
            .get(b"old/inner".as_slice())
            .map(|entry| entry.1),
        Some(0o751),
        "the adopted directory is finished"
    );
    for cap in [2, 3, 64] {
        assert_eq!(three_runs(cap), reference, "cap {cap}");
    }
}

/// #217 review, finding 8: a third party's directory made at a member's
/// name after the member was admitted (just before its rename) refuses the
/// member `DESTINATION_OCCUPIED`; its child is then decided against the
/// file system, as one at a time would: made inside that directory. Caps 1
/// and 64 agree.
#[test]
fn a_member_refused_at_its_rename_leaves_its_children_to_the_file_system() {
    let build = |corpus: &Corpus| {
        let directories: Vec<(String, u32)> = [("x", 0o755), ("x/y", 0o755), ("w", 0o755)]
            .into_iter()
            .map(|(path, mode)| (path.to_owned(), mode))
            .collect();
        batch_tree(corpus, &directories, &["x/y/f".to_owned()]);
    };
    let run = |cap: usize| {
        let corpus = Corpus::new();
        build(&corpus);
        let destination = canonical(&corpus.base.join("destination"));
        let foreign = destination.join("x");
        let _hook =
            crate::materialize::set_before_directory_rename(&destination, b"x", move || {
                let _ = std::fs::create_dir(&foreign);
                let _ = std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755));
            })
            .unwrap();
        crate::materialize::set_directory_batch(&destination, Some(cap));
        let stats = corpus.run().unwrap();
        crate::materialize::set_directory_batch(&destination, None);
        (outcome(&stats), tree(&corpus.base.join("destination")))
    };
    let one = run(1);
    assert!(
        one.0.contains("(\"x\", \"DESTINATION_OCCUPIED\")"),
        "{}",
        one.0
    );
    assert!(
        one.1 .0.contains_key(b"x/y/f".as_slice()),
        "{:?}",
        one.1 .0.keys()
    );
    assert_eq!(run(64), one);
}

fn directory_node() -> impl proptest::strategy::Strategy<Value = (Vec<u8>, u32, u8)> {
    use proptest::prelude::*;
    (
        proptest::collection::vec(0_u8..4, 1..4),
        prop::sample::select(vec![0o700_u32, 0o750, 0o755]),
        0_u8..6,
    )
}

proptest::proptest! {
    #![proptest_config(crate::test_support::prop_config(8))]

    /// Item 2: random trees (depth up to 3, fan-out up to 4), some
    /// directories already at the destination with the source's mode or
    /// another, at a random batch cap: the carry equals one directory at a
    /// time.
    #[test]
    fn directory_batches_match_one_at_a_time(
        nodes in proptest::collection::vec(directory_node(), 1..12),
        cap in proptest::sample::select(vec![2_usize, 3, 5, 64]),
    ) {
        let build = |corpus: &Corpus| {
            let mut directories: BTreeMap<String, u32> = BTreeMap::new();
            let mut existing = Vec::new();
            let mut files = Vec::new();
            for (path, mode, kind) in &nodes {
                let mut at = String::new();
                for part in path {
                    if !at.is_empty() {
                        at.push('/');
                    }
                    write!(at, "d{part}").unwrap();
                    directories.entry(at.clone()).or_insert(*mode);
                }
                match kind {
                    0 => existing.push((at.clone(), *mode)),
                    1 => existing.push((at.clone(), 0o711)),
                    2 | 3 => files.push(format!("{at}/f{kind}")),
                    _ => {}
                }
            }
            let directories: Vec<(String, u32)> = directories.into_iter().collect();
            batch_tree(corpus, &directories, &files);
            for (path, mode) in existing {
                let at = corpus.base.join("destination").join(&path);
                std::fs::create_dir_all(&at).unwrap();
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode)).unwrap();
            }
        };
        proptest::prop_assert_eq!(batched_run(cap, &build), batched_run(1, &build));
    }
}

/// The source's walk with the directory entry at `path` offered a second
/// time: as entry `n`, just before `WalkDone { n }` (sent as `n + 1`), and
/// counted in `SourceDone`. `repeated` holds `n` once it is sent, so the
/// relay can drop the destination's answer to it.
struct Repeat<W> {
    output: W,
    frame: Vec<u8>,
    path: Vec<u8>,
    row: Option<RowSchema>,
    repeated: Arc<AtomicU64>,
}

impl<W: Write> Write for Repeat<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.frame.extend_from_slice(data);
        Ok(data.len())
    }
    fn write_vectored(&mut self, data: &[IoSlice<'_>]) -> std::io::Result<usize> {
        let mut count = 0;
        for slice in data {
            self.frame.extend_from_slice(slice);
            count += slice.len();
        }
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let frame = std::mem::take(&mut self.frame);
        let encode = |control: &Control| control.encode().unwrap();
        let bytes = match Frame::decode(&frame) {
            Ok((Frame::Control(Control::Entry { row, .. }), _)) if row.rel_path == self.path => {
                self.row = Some(row);
                frame
            }
            Ok((Frame::Control(Control::WalkDone { entries }), _)) => match self.row.take() {
                Some(row) => {
                    self.repeated.store(entries, Ordering::SeqCst);
                    [
                        encode(&Control::Entry {
                            entry: entries,
                            row,
                        }),
                        encode(&Control::WalkDone {
                            entries: entries + 1,
                        }),
                    ]
                    .concat()
                }
                None => frame,
            },
            Ok((
                Frame::Control(Control::SourceDone {
                    entries,
                    source_bytes_read,
                }),
                _,
            )) if self.repeated.load(Ordering::SeqCst) != u64::MAX => {
                encode(&Control::SourceDone {
                    entries: entries + 1,
                    source_bytes_read,
                })
            }
            _ => frame,
        };
        self.output.write_all(&bytes)?;
        self.output.flush()
    }
}

/// A copy whose source offers the directory `d` twice, at batch cap `cap`:
/// the comparable outcome, then the outcome of a plain rerun.
fn repeated_directory_run(cap: usize) -> ((String, Tree), (String, Tree)) {
    let corpus = Corpus::new();
    let directories = vec![("d".to_owned(), 0o755)];
    batch_tree(&corpus, &directories, &["d/f".to_owned()]);
    let destination = canonical(&corpus.base.join("destination"));
    crate::materialize::set_directory_batch(&destination, Some(cap));
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    let (relayed, source_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let repeated = Arc::new(AtomicU64::new(u64::MAX));
    let stats = std::thread::scope(|scope| {
        // The destination's frames to the source, its answer to the repeated
        // entry dropped: that entry is the test's, not the source's.
        let from_destination = sender.try_clone().unwrap();
        let dropped = Arc::clone(&repeated);
        let relay = scope.spawn(move || {
            let mut input = from_destination;
            let mut output = relayed;
            while let Ok(Frame::Control(control)) = read_frame(&mut input) {
                if matches!(control, Control::Decide { entry, .. }
                    if entry == dropped.load(Ordering::SeqCst))
                {
                    continue;
                }
                if write_control(&mut output, &control).is_err() {
                    break;
                }
            }
            let _ = output.shutdown(std::net::Shutdown::Both);
        });
        let into_source = Arc::clone(&repeated);
        let producer = scope.spawn(move || {
            let closer = sender.try_clone().unwrap();
            let served = serve(
                source_input,
                &mut Repeat {
                    output: sender,
                    frame: Vec::new(),
                    path: b"d".to_vec(),
                    row: None,
                    repeated: into_source,
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
        let served = producer.join().unwrap();
        relay.join().unwrap();
        outcome.and_then(|stats| served.map(|()| stats))
    })
    .unwrap();
    assert_ne!(
        repeated.load(Ordering::SeqCst),
        u64::MAX,
        "the directory was offered twice"
    );
    let first = (outcome(&stats), tree(&corpus.base.join("destination")));
    let rerun = corpus.run().unwrap();
    crate::materialize::set_directory_batch(&destination, None);
    (
        first,
        (outcome(&rerun), tree(&corpus.base.join("destination"))),
    )
}

/// S1 throughput review (2026-10-09), finding 2: a source that offers the
/// same directory twice while a batch is open gets what one at a time
/// gives it: the first offer creates the directory, the second adopts it by
/// its record, and the directory is finished with the source's mode. Two
/// members at one key would bind two records to it, the second rename's
/// discard would clear the only one, and the directory this copy made
/// would be refused on this run and on every rerun.
#[test]
fn a_directory_offered_twice_in_a_batch_is_decided_as_one_at_a_time() {
    let one = repeated_directory_run(1);
    assert!(
        one.0 .0.contains("refusals=[]"),
        "one at a time adopts the repeated directory: {}",
        one.0 .0
    );
    assert_eq!(
        one.0 .1 .0.get(b"d".as_slice()).map(|entry| entry.1),
        Some(0o755)
    );
    assert_eq!(repeated_directory_run(64), one);
}
