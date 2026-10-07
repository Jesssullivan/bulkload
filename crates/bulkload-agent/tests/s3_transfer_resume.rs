//! S3 headline properties through the public transfer verbs (OI-1003-Q6,
//! OI-1003-Q10, WP0(c) of OI-1003-Q18; R25, R-N58; property-test plan §1).
//!
//! Every property drives `transfer::copy` (or `serve` against `receive` over
//! a socket pair, to cut the transport) on generated corpora under the
//! system temp dir, and asserts the per-run counters of [`TransferStats`]
//! (`source_bytes_read`, `bytes_received`, the S3 evidence's
//! `transferred_content_bytes`) plus the process counters of one measured run
//! (`read_source_file_bytes`, `source_sniff_bytes`, `read_hash_file_bytes`,
//! `transfer_racy_captures`). Measured runs hold [`SERIAL`], so those
//! process-scope deltas are exact inside this test binary.
//!
//! - **P23 RESUME + READ-ONCE.** ∀ small corpora and cut points (after k
//!   `End` frames, at `SourceDone`, none): exactly the k entries whose `End`
//!   went out are applied; the resume reads exactly Σ size of the files that
//!   were not applied, converges byte-identical with only the fixture's
//!   refusals, and a further rerun reads 0 source bytes and receives 0. No
//!   session reads a byte twice.
//! - **P21 (transfer leg) and the WP0(c) inequalities.** ∀ corpora, ∀ change
//!   sets (in-place patches, appends, added seats, some sharing chunks with
//!   carried outputs) applied after a settled first pass: the rerun's walk is
//!   metadata only, (1) source content bytes read ≤ Σ size of changed or
//!   racy seats, (2) wire content bytes ≤ Σ size of the changed seats'
//!   chunks the destination holds no verified copy of, the rerun converges,
//!   and a rerun after it reads 0 and receives 0.
//! - **P18 HINTS (end to end).** ∀ ≤ 3 holders and ∀ fates (kept, deleted,
//!   overwritten) of each on the destination: a new file built from slices of
//!   the holders crosses the wire as exactly the bytes of its distinct chunks
//!   that no surviving holder still verifies (its size less what the
//!   survivors and its own repeats supply), and it is read once.
//! - **P19 RACY (file half, end to end).** ∀ corpora and racy seats (written
//!   just before the run, or stamped later than the clock): the capture is
//!   sent and counted racy, no row commits under its key, so the next run
//!   reads it again (never a Reuse) until it is settled.
//!
//! **Refused seats (#186).** A seat refused for its `SQLite` header is
//! sniffed once: its [`SNIFF_BYTES`] are counted as `source_sniff_bytes`,
//! never as content (`source_bytes_read`, `read_source_file_bytes`), and its
//! refusal is remembered under its stat identity. Every run still reports
//! the refusal, with the same code; a run over an unchanged refused seat
//! opens nothing and reads 0 bytes of either kind. P23 and P21 hold with
//! refused seats in the corpus, and a pinned row shows a changed refused
//! seat sniffed again while its unchanged neighbour is not.
//!
//! **Changed seats (#187, WP0(d)).** A changed seat whose output this store
//! published is superseded, so every clause of P21 holds over in-place
//! changes too: both inequalities, convergence, and a rerun at 0 and 0. A
//! pinned row shows the limit of that: a destination file this store does
//! not own is refused `DESTINATION_OCCUPIED` and left as it is. Such a seat
//! is read once: the destination remembers the refusal against the file it
//! found, and an unchanged rerun reads 0 bytes for it. An output published
//! from a racy capture is this store's own too (an ownership row, no reuse
//! row), so an actively written seat is superseded, not refused.
//!
//! One test is ignored, for another reason: inequality 2 read strictly (a
//! chunk absent once crosses once per run) is unstable when two seats added
//! in one run carry the same absent chunk ([`Twins`]). The green properties
//! allow that chunk once per seat, the per-file reading of the S3 evidence
//! harness; which reading is the contract is unruled.
//!
//! **Corpus.** CI runs a fixed seed and a small case count per property
//! (`test_support::prop_config`, compiled here from the library's source
//! file through `#[path]`; `tests/prop_seed_guard.rs` holds every property
//! to it). `BULKLOAD_PROPTEST_DEEP=1` runs twenty times the cases from the
//! same fixed seed (OI-1003-Q78). No failure-persistence file. The library's
//! test-only clock and retention overrides are `#[cfg(test)]`, out of an
//! integration test's reach, so captures are made non-racy for real with
//! `settle_racy_window` (about 2 s per settle).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::hash::{chunk_boundaries, hash_bytes};
use bulkload_agent::transfer::{copy, receive, serve, settle_racy_window, TransferStats};
use bulkload_agent::{Control, Frame};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Corpus configuration
// ---------------------------------------------------------------------------

/// The shared helper itself (OI-1003-Q7): the same source file as the
/// library's `test_support`, so the seed and the deep switch cannot drift.
#[path = "../src/test_support.rs"]
mod test_support;

/// The transfer's credit window (`transfer::CREDIT_WINDOW`, private).
const CREDIT_WINDOW: usize = 16 * 1024 * 1024;

/// The header bytes a source capture reads of a seat before it refuses a
/// `SQLite` header: counted as `source_sniff_bytes`, once per stat identity
/// of the seat, and never as content (#186).
const SNIFF_BYTES: u64 = 16;

/// How inequality 2 counts an absent chunk that several seats changed or
/// added in one run all carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Twins {
    /// The strict reading: a digest is absent once, so it crosses once per
    /// run. Unstable when two seats of one run share an absent chunk.
    Once,
    /// What main does, and the per-file reading the S3 evidence harness
    /// uses: the chunk may cross once for each seat that carries it.
    PerSeat,
}

/// Directories a file may sit in; the empty prefix is the root.
const DIRECTORIES: [&str; 3] = ["", "d0", "d0/e"];

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Measured runs take this lock, so process counter deltas are theirs alone.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A source root, a destination root and both state roots under one scratch
/// directory in the system temp dir, removed on drop.
struct Fixture {
    base: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "bulkload-s3-transfer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        for path in ["source", "destination"] {
            std::fs::create_dir(base.join(path)).unwrap();
        }
        Self { base }
    }

    fn source(&self) -> PathBuf {
        self.base.join("source")
    }

    fn destination(&self) -> PathBuf {
        self.base.join("destination")
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let path = self.source().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Wait until no source seat is racy, so the next capture records.
    fn settle(&self) {
        settle_racy_window(&self.source()).unwrap();
    }

    /// One `copy`, with the process counters it moved.
    fn run(&self) -> (TransferStats, Counters) {
        self.run_after(|| ())
    }

    /// `prepare`, then one `copy`, both under [`SERIAL`]: nothing else in
    /// this binary runs between the preparation and the copy.
    fn run_after(&self, prepare: impl FnOnce()) -> (TransferStats, Counters) {
        let _serial = serial();
        prepare();
        let before = Counters::snapshot();
        let stats = copy(
            &self.source(),
            &self.destination(),
            &self.base.join("source-state"),
            &self.base.join("destination-state"),
        )
        .unwrap();
        (stats, Counters::snapshot().since(before))
    }

    /// One session whose serve-side transport breaks at `cut`; returns the
    /// process counters it moved. Both halves must fail.
    fn cut_session(&self, cut: Cut) -> Counters {
        let _serial = serial();
        let before = Counters::snapshot();
        let (sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        for stream in [&sender, &receiver] {
            stream
                .set_read_timeout(Some(Duration::from_mins(1)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_mins(1)))
                .unwrap();
        }
        std::thread::scope(|scope| {
            let producer = scope.spawn(move || {
                let input = sender.try_clone().unwrap();
                let closer = sender.try_clone().unwrap();
                let served = match cut {
                    Cut::AfterEnds(count) => serve(
                        input,
                        &mut Interrupted {
                            output: sender,
                            frame: Vec::new(),
                            ends: 0,
                            cut_after: count,
                            state: if count == 0 {
                                CutState::Truncate
                            } else {
                                CutState::Passing
                            },
                        },
                    ),
                    Cut::AtSourceDone => serve(input, &mut StopAtDone(sender)),
                };
                // Close only the source's sending half: the destination still
                // answers every `End` it got with `Held`, deterministically.
                let _ = closer.shutdown(std::net::Shutdown::Write);
                served
            });
            let mut output = receiver.try_clone().unwrap();
            let outcome = receive(
                &mut receiver,
                &mut output,
                &self.source(),
                &self.base.join("source-state"),
                &self.destination(),
                &self.base.join("destination-state"),
            );
            let _ = receiver.shutdown(std::net::Shutdown::Both);
            drop(output);
            drop(receiver);
            assert!(producer.join().unwrap().is_err(), "serve survived {cut:?}");
            assert!(outcome.is_err(), "receive survived {cut:?}");
        });
        Counters::snapshot().since(before)
    }

    /// Whether the destination holds `rel` with exactly `content`.
    fn holds(&self, rel: &str, content: &[u8]) -> bool {
        std::fs::read(self.destination().join(rel)).is_ok_and(|held| held == content)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Where the serve side's transport breaks.
#[derive(Debug, Clone, Copy)]
enum Cut {
    /// The frame after the k-th `End` is cut in half (k = 0: the first frame).
    AfterEnds(usize),
    /// `SourceDone` is never delivered: every capture already committed.
    AtSourceDone,
}

/// Breaks the serve side's transport at a protocol event, not a byte count
/// (the in-crate harness of `transfer/tests.rs`, mirrored). Every `flush`
/// closes one whole frame; once `cut_after` `End` frames went out, the next
/// frame is cut in half and the transport fails.
struct Interrupted<W> {
    output: W,
    frame: Vec<u8>,
    ends: usize,
    cut_after: usize,
    state: CutState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CutState {
    Passing,
    Truncate,
    Broken,
}

impl<W: Write> Write for Interrupted<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let allowed = match self.state {
            CutState::Broken => return Err(std::io::ErrorKind::BrokenPipe.into()),
            CutState::Truncate => {
                self.state = CutState::Broken;
                data.len().div_ceil(2)
            }
            CutState::Passing => data.len(),
        };
        let count = self.output.write(&data[..allowed])?;
        self.frame.extend_from_slice(&data[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.state == CutState::Passing {
            let (frame, _) = Frame::decode(&self.frame).unwrap();
            if matches!(frame, Frame::Control(Control::End { .. })) {
                self.ends += 1;
                if self.ends == self.cut_after {
                    self.state = CutState::Truncate;
                }
            }
        }
        self.frame.clear();
        self.output.flush()
    }
}

/// Breaks the serve side's transport at `SourceDone`.
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

/// A refused seat: a `SQLite` header the capture sniffs and refuses.
fn sqlite_seat(seed: u64) -> Vec<u8> {
    let mut bytes = b"SQLite format 3\0".to_vec();
    bytes.extend(noise(seed, 4_096));
    bytes
}

const fn size_of(content: &[u8]) -> u64 {
    content.len() as u64
}

fn file_rel(directory: usize, name: &str) -> String {
    let prefix = DIRECTORIES[directory % DIRECTORIES.len()];
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Every chunk of `content`, as (digest, size), cut as the transfer cuts.
fn chunks(content: &[u8]) -> Vec<([u8; 32], u64)> {
    chunk_boundaries(content)
        .into_iter()
        .map(|(offset, length)| (hash_bytes(&content[offset..offset + length]), length as u64))
        .collect()
}

/// The refusals of a run, as (relative path, code), sorted.
fn refusals(stats: &TransferStats) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = stats
        .refusals
        .iter()
        .map(|(path, code)| (String::from_utf8_lossy(path).into_owned(), code.clone()))
        .collect();
    found.sort();
    found
}

/// The refusals a fixture's `SQLite` seats earn, sorted.
fn sqlite_refusals(names: &[String]) -> Vec<(String, String)> {
    let mut expected: Vec<(String, String)> = names
        .iter()
        .map(|name| (name.clone(), "SQLITE_STATE_CHANGED".to_owned()))
        .collect();
    expected.sort();
    expected
}

fn length() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(0_usize),
        Just(1_usize),
        2_usize..16_384,
        16_384_usize..262_144,
        Just(262_144_usize),
    ]
}

// ---------------------------------------------------------------------------
// P23 RESUME + READ-ONCE
// ---------------------------------------------------------------------------

/// Where a P23 case stops its first session.
#[derive(Debug, Clone, Copy)]
enum CutAt {
    Ends(usize),
    SourceDone,
    Never,
}

/// A P23 case's first session, cut or whole: no session reads a byte twice,
/// and a refused seat's header is never content. Returns the sniff bytes it
/// read.
fn first_session(fixture: &Fixture, cut: CutAt, total: u64, sqlite: &[String]) -> u64 {
    let sniffed = SNIFF_BYTES * sqlite.len() as u64;
    match cut {
        CutAt::Never => {
            let (first, counters) = fixture.run();
            assert_eq!(refusals(&first), sqlite_refusals(sqlite));
            assert_eq!(first.source_bytes_read, total);
            assert_eq!(counters.get(Counter::SourceFileRead), total);
            assert_eq!(
                counters.get(Counter::SourceSniff),
                sniffed,
                "each refused seat is sniffed once"
            );
            sniffed
        }
        CutAt::Ends(count) => {
            let counters = fixture.cut_session(Cut::AfterEnds(count));
            assert!(counters.get(Counter::SourceFileRead) <= total);
            assert!(counters.get(Counter::SourceSniff) <= sniffed);
            counters.get(Counter::SourceSniff)
        }
        CutAt::SourceDone => {
            let counters = fixture.cut_session(Cut::AtSourceDone);
            assert!(counters.get(Counter::SourceFileRead) <= total);
            // Every entry was settled before `SourceDone`: every refused
            // seat was sniffed, and its refusal committed with the ledger.
            assert_eq!(counters.get(Counter::SourceSniff), sniffed);
            sniffed
        }
    }
}

/// One P23 case: files by (directory, length, seed), `refused` `SQLite`
/// seats, and the cut.
///
/// Content and sniff bytes are asserted apart (#186). A refused seat's
/// header is sniffed at most once in any session, never as content; a run
/// that follows a completed one reads nothing at all, of either kind.
fn check_p23(files: &[(usize, usize, u64)], refused: usize, cut: CutAt) {
    let fixture = Fixture::new();
    let mut corpus: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (index, (directory, length, seed)) in files.iter().enumerate() {
        let rel = file_rel(*directory, &format!("f{index}"));
        let content = noise(*seed, *length);
        fixture.write(&rel, &content);
        corpus.insert(rel, content);
    }
    let sqlite: Vec<String> = (0..refused)
        .map(|index| format!("db{index}.sqlite"))
        .collect();
    for (index, rel) in sqlite.iter().enumerate() {
        fixture.write(rel, &sqlite_seat(index as u64));
    }
    let total: u64 = corpus.values().map(|content| size_of(content)).sum();
    let sniffed = SNIFF_BYTES * refused as u64;
    fixture.settle();

    let first_sniff = first_session(&fixture, cut, total, &sqlite);
    let applied: BTreeSet<&String> = corpus
        .iter()
        .filter(|(rel, content)| fixture.holds(rel, content))
        .map(|(rel, _)| rel)
        .collect();
    let expected_applied = match cut {
        CutAt::Ends(count) => count.min(corpus.len()),
        CutAt::SourceDone | CutAt::Never => corpus.len(),
    };
    assert_eq!(applied.len(), expected_applied, "applied after {cut:?}");
    let unapplied: u64 = corpus
        .iter()
        .filter(|(rel, _)| !applied.contains(rel))
        .map(|(_, content)| size_of(content))
        .sum();

    // The resume reads exactly the files that were not applied, once, and
    // not one content byte of a refused seat.
    let (resumed, counters) = fixture.run();
    assert_eq!(refusals(&resumed), sqlite_refusals(&sqlite));
    assert_eq!(resumed.reused, applied.len() as u64, "reused");
    assert_eq!(
        resumed.completed,
        (corpus.len() - applied.len()) as u64,
        "completed"
    );
    assert_eq!(
        resumed.source_bytes_read, unapplied,
        "resume source_bytes_read (unapplied {unapplied})"
    );
    assert_eq!(
        counters.get(Counter::SourceFileRead),
        resumed.source_bytes_read
    );
    // A refused seat the first session finished with is not sniffed again.
    // One it was cut before is sniffed now, once.
    let resume_sniff = counters.get(Counter::SourceSniff);
    if matches!(cut, CutAt::Never | CutAt::SourceDone) {
        assert_eq!(resume_sniff, 0, "a remembered refusal reads nothing");
        assert_eq!(
            counters.get(Counter::TransferRefusedSeatsRemembered),
            refused as u64,
            "each refusal of a completed session is answered from its record"
        );
    } else {
        assert!(
            resume_sniff <= sniffed && resume_sniff.is_multiple_of(SNIFF_BYTES),
            "resume sniffed {resume_sniff} bytes of {refused} refused seats"
        );
        assert!(
            first_sniff + resume_sniff >= sniffed,
            "every refused seat was sniffed by now ({first_sniff} + {resume_sniff})"
        );
    }
    assert!(resumed.bytes_received <= unapplied, "resume bytes_received");
    assert_eq!(
        counters.get(Counter::HashFileRead),
        0,
        "walk is metadata only"
    );
    for (rel, content) in &corpus {
        assert!(fixture.holds(rel, content), "{rel} did not converge");
    }

    // A further rerun reads nothing and receives nothing: no content byte,
    // and no sniff byte of a refused seat, which it still reports.
    let (rerun, counters) = fixture.run();
    assert_eq!(refusals(&rerun), sqlite_refusals(&sqlite));
    assert_eq!(rerun.reused, corpus.len() as u64);
    assert_eq!(rerun.source_bytes_read, 0, "rerun source_bytes_read");
    assert_eq!(counters.get(Counter::SourceFileRead), 0);
    assert_eq!(
        counters.get(Counter::SourceSniff),
        0,
        "rerun sniffed a refused seat again (#186)"
    );
    assert_eq!(
        counters.get(Counter::TransferRefusedSeatsRemembered),
        refused as u64,
        "each refusal is answered from its record"
    );
    assert_eq!(rerun.bytes_received, 0, "rerun bytes_received");
}

fn p23_case() -> impl Strategy<Value = (Vec<(usize, usize, u64)>, CutAt)> {
    prop::collection::vec((0..DIRECTORIES.len(), length(), any::<u64>()), 1..=8).prop_flat_map(
        |files| {
            let count = files.len();
            (
                Just(files),
                prop_oneof![
                    (0..=count).prop_map(CutAt::Ends),
                    Just(CutAt::SourceDone),
                    Just(CutAt::Never),
                ],
            )
        },
    )
}

proptest! {
    #![proptest_config(test_support::prop_config(8))]

    /// P23: a resume after any cut reads exactly the unapplied files, once,
    /// converges, and a further rerun reads and receives nothing.
    #[test]
    fn p23_a_resume_after_any_cut_reads_only_the_unapplied_files(case in p23_case()) {
        let (files, cut) = case;
        check_p23(&files, 0, cut);
    }
}

proptest! {
    #![proptest_config(test_support::prop_config(4))]

    /// P23 with the fixture's refused seats (#186): after any cut the
    /// refusal set is the fixture's on every run, the other files resume
    /// exactly as without them, a refused seat is sniffed at most once in a
    /// session and never as content, and the further rerun reads nothing.
    #[test]
    fn p23_with_refused_seats_a_further_rerun_reads_nothing(
        case in p23_case(),
        refused in 1_usize..=2,
    ) {
        let (files, cut) = case;
        check_p23(&files, refused, cut);
    }
}

/// P23 PINNED (#186): two refused seats beside three files, cut after the
/// first `End`. Every later run reports both refusals, once each, and a run
/// that follows a completed one reads 0 bytes of the two 4112-byte seats.
#[test]
fn p23_pinned_two_refused_seats_across_a_cut_are_not_sniffed_again() {
    check_p23(
        &[(0, 70_000, 31), (1, 4_096, 32), (2, 200_000, 33)],
        2,
        CutAt::Ends(1),
    );
}

/// P23 PINNED (#186, review): a corpus of refused seats only. Nothing is
/// ever published, so the destination holds no chunk to fill a manifest
/// from and answers every entry `Send`, not `WantManifest`: the one path on
/// which a remembered refusal is consulted ahead of a streamed capture.
/// Every other refused-seat row has carried files, so its reruns ask for
/// manifests. The rerun sniffs nothing and answers both from their records.
#[test]
fn p23_pinned_only_refused_seats_are_answered_from_their_records() {
    check_p23(&[], 2, CutAt::Never);
    check_p23(&[], 1, CutAt::SourceDone);
}

/// P23 PINNED: one file past the credit window, cut after the first `End`
/// (at most one such file per CI run, property-test plan P23).
#[test]
fn p23_pinned_a_file_past_the_credit_window() {
    check_p23(
        &[
            (0, 4_096, 1),
            (1, CREDIT_WINDOW + 4_097, 2),
            (2, 300_000, 3),
        ],
        0,
        CutAt::Ends(1),
    );
}

// ---------------------------------------------------------------------------
// P21 transfer leg: the S3 delta inequalities (WP0(c))
// ---------------------------------------------------------------------------

/// How one carried file changes before the rerun.
#[derive(Debug, Clone, Copy)]
enum Change {
    Same,
    /// Overwrite `length` bytes at a fraction of the file (same size).
    Patch {
        at: u16,
        length: usize,
        seed: u64,
    },
    /// Append fresh bytes.
    Append {
        length: usize,
        seed: u64,
    },
}

/// A seat added before the rerun.
#[derive(Debug, Clone, Copy)]
enum Added {
    Fresh {
        length: usize,
        seed: u64,
    },
    /// A carried file's first pass content plus a fresh tail: most of its
    /// chunks are already held by the destination.
    CopyOf {
        file: usize,
        tail: usize,
        seed: u64,
    },
    /// `length` bytes of a carried file's first pass content from a fraction
    /// of it, plus a fresh tail: the chunker resynchronises after the
    /// slice's first cut point, so the chunks after it are already held.
    SliceOf {
        file: usize,
        at: u16,
        length: usize,
        tail: usize,
        seed: u64,
    },
}

#[derive(Debug, Clone)]
struct DeltaCase {
    files: Vec<(usize, usize, u64)>,
    changes: Vec<Change>,
    added: Vec<Added>,
    refused: usize,
}

/// What a delta case asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clauses {
    /// Inequality 1 and the metadata-only walk on the rerun.
    ReadsOnly,
    /// Every clause: inequalities 1 and 2, convergence, and the rerun after
    /// it reading and receiving nothing.
    All,
}

fn apply_change(content: &[u8], change: Change) -> Vec<u8> {
    match change {
        Change::Same => content.to_vec(),
        Change::Patch { at, length, seed } => {
            let mut changed = content.to_vec();
            if changed.is_empty() {
                return noise(seed, length.max(1));
            }
            let start = changed.len() * usize::from(at) / usize::from(u16::MAX);
            let start = start.min(changed.len() - 1);
            let end = (start + length.max(1)).min(changed.len());
            let patch = noise(seed, end - start);
            for (byte, new) in changed[start..end].iter_mut().zip(patch) {
                *byte = !new;
            }
            changed
        }
        Change::Append { length, seed } => {
            let mut changed = content.to_vec();
            changed.extend(noise(seed, length.max(1)));
            changed
        }
    }
}

/// A delta case worked out before anything touches a disk.
struct DeltaModel {
    /// The first pass: (relative path, content), in `DeltaCase::files` order.
    first: Vec<(String, Vec<u8>)>,
    /// What is written before the rerun: (relative path, new content), the
    /// in-place changes and then the added seats.
    writes: Vec<(String, Vec<u8>)>,
    /// Σ size of the changed and added seats: the bound of inequality 1.
    changed_bytes: u64,
    /// Σ size of the distinct chunks of the changed and added seats that the
    /// destination holds no verified copy of after the first pass: the bound
    /// of inequality 2. A digest counts once across the whole run, however
    /// many seats (or places in one seat) carry it.
    absent_bytes: u64,
    /// The absent chunks counted once per seat that carries them, less
    /// `absent_bytes`: what the second and later seats of one run repeat.
    /// 0 unless two seats of the run share an absent chunk.
    twin_bytes: u64,
}

fn delta_model(case: &DeltaCase) -> DeltaModel {
    let first: Vec<(String, Vec<u8>)> = case
        .files
        .iter()
        .enumerate()
        .map(|(index, (directory, length, seed))| {
            (
                file_rel(*directory, &format!("f{index}")),
                noise(*seed, *length),
            )
        })
        .collect();
    // Everything the destination holds a verified copy of after pass one.
    let held: HashSet<[u8; 32]> = first
        .iter()
        .flat_map(|(_, content)| chunks(content))
        .map(|(digest, _)| digest)
        .collect();
    let mut writes = Vec::new();
    for (index, change) in case.changes.iter().enumerate() {
        if matches!(change, Change::Same) {
            continue;
        }
        let (rel, content) = &first[index];
        writes.push((rel.clone(), apply_change(content, *change)));
    }
    for (index, added) in case.added.iter().enumerate() {
        let content = match *added {
            Added::Fresh { length, seed } => noise(seed, length),
            Added::CopyOf { file, tail, seed } => {
                let mut content = first[file].1.clone();
                content.extend(noise(seed, tail));
                content
            }
            Added::SliceOf {
                file,
                at,
                length,
                tail,
                seed,
            } => {
                let carried = &first[file].1;
                let start = carried.len() * usize::from(at) / usize::from(u16::MAX);
                let start = start.min(carried.len());
                let end = (start + length).min(carried.len());
                let mut content = carried[start..end].to_vec();
                content.extend(noise(seed, tail));
                content
            }
        };
        writes.push((file_rel(index, &format!("added{index}")), content));
    }
    let changed_bytes = writes.iter().map(|(_, content)| size_of(content)).sum();
    let mut absent: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    let mut per_seat = 0_u64;
    for (_, content) in &writes {
        let lacking: BTreeMap<[u8; 32], u64> = chunks(content)
            .into_iter()
            .filter(|(digest, _)| !held.contains(digest))
            .collect();
        per_seat += lacking.values().sum::<u64>();
        absent.extend(lacking);
    }
    let absent_bytes = absent.values().sum();
    DeltaModel {
        first,
        writes,
        changed_bytes,
        absent_bytes,
        twin_bytes: per_seat - absent_bytes,
    }
}

fn check_delta(case: &DeltaCase, clauses: Clauses, twins: Twins) {
    let fixture = Fixture::new();
    let DeltaModel {
        first: carried,
        writes,
        changed_bytes,
        absent_bytes,
        twin_bytes,
    } = delta_model(case);
    let wire_bound = match twins {
        Twins::Once => absent_bytes,
        Twins::PerSeat => absent_bytes + twin_bytes,
    };
    let mut corpus: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (rel, content) in carried {
        fixture.write(&rel, &content);
        corpus.insert(rel, content);
    }
    let sqlite: Vec<String> = (0..case.refused)
        .map(|index| format!("d0/db{index}.sqlite"))
        .collect();
    for (index, rel) in sqlite.iter().enumerate() {
        fixture.write(rel, &sqlite_seat(index as u64));
    }
    fixture.settle();
    let carried_bytes: u64 = corpus.values().map(|content| size_of(content)).sum();
    let (first, counters) = fixture.run();
    assert_eq!(refusals(&first), sqlite_refusals(&sqlite), "first pass");
    assert_eq!(
        counters.get(Counter::SourceSniff),
        SNIFF_BYTES * case.refused as u64,
        "the first pass sniffs each refused seat once"
    );
    assert_eq!(
        (
            first.source_bytes_read,
            counters.get(Counter::SourceFileRead)
        ),
        (carried_bytes, carried_bytes),
        "each carried file is read once, and sniff bytes are never content"
    );

    for (rel, content) in writes {
        fixture.write(&rel, &content);
        corpus.insert(rel, content);
    }
    // Settled: no seat is racy, so the bound of inequality 1 is the changed
    // seats alone.
    fixture.settle();

    let (rerun, counters) = fixture.run();
    let context = format!(
        "changed_bytes={changed_bytes} absent_bytes={absent_bytes} twin_bytes={twin_bytes} source_bytes_read={} sniffed={} bytes_received={} superseded={} refusals={:?}",
        rerun.source_bytes_read,
        counters.get(Counter::SourceSniff),
        rerun.bytes_received,
        counters.get(Counter::OutputsSuperseded),
        refusals(&rerun)
    );
    assert_eq!(
        counters.get(Counter::HashFileRead),
        0,
        "walk is metadata only: {context}"
    );
    assert_eq!(
        counters.get(Counter::SourceFileRead),
        rerun.source_bytes_read,
        "{context}"
    );
    assert!(
        rerun.source_bytes_read <= changed_bytes,
        "inequality 1: {context}"
    );
    // No refused seat changed, so none is opened: its remembered refusal
    // answers, and not one sniff byte is read (#186).
    assert_eq!(
        counters.get(Counter::SourceSniff),
        0,
        "an unchanged refused seat is not sniffed again: {context}"
    );
    if clauses == Clauses::ReadsOnly {
        return;
    }
    assert!(
        rerun.bytes_received <= wire_bound,
        "inequality 2 ({twins:?}): {context}"
    );
    assert_eq!(
        refusals(&rerun),
        sqlite_refusals(&sqlite),
        "only the fixture's refusals: {context}"
    );
    for (rel, content) in &corpus {
        assert!(
            fixture.holds(rel, content),
            "{rel} did not converge: {context}"
        );
    }
    assert_unchanged_estate_reads_nothing(&fixture, &sqlite);
}

/// The unchanged-estate clause: a run with nothing changed reads 0 content
/// bytes and 0 sniff bytes, receives 0, replaces nothing, and still reports
/// the fixture's refusals.
fn assert_unchanged_estate_reads_nothing(fixture: &Fixture, sqlite: &[String]) {
    let (again, counters) = fixture.run();
    assert_eq!(refusals(&again), sqlite_refusals(sqlite));
    assert_eq!(again.source_bytes_read, 0, "unchanged estate reads 0");
    assert_eq!(counters.get(Counter::SourceFileRead), 0);
    assert_eq!(
        counters.get(Counter::SourceSniff),
        0,
        "unchanged estate sniffs nothing"
    );
    assert_eq!(again.bytes_received, 0, "unchanged estate receives 0");
    assert_eq!(
        counters.get(Counter::OutputsSuperseded),
        0,
        "nothing is replaced on an unchanged estate"
    );
}

fn change() -> impl Strategy<Value = Change> {
    prop_oneof![
        2 => Just(Change::Same),
        1 => (any::<u16>(), 1_usize..70_000, any::<u64>())
            .prop_map(|(at, length, seed)| Change::Patch { at, length, seed }),
        1 => (1_usize..70_000, any::<u64>()).prop_map(|(length, seed)| Change::Append { length, seed }),
    ]
}

/// The fresh tail of a copied or sliced seat: none, or up to a chunk or so.
fn tail() -> impl Strategy<Value = usize> {
    prop_oneof![Just(0_usize), 1_usize..70_000]
}

/// An added seat that takes its bytes from carried file `file`.
fn sharing_seat(file: impl Strategy<Value = usize>) -> impl Strategy<Value = Added> {
    (
        file,
        any::<bool>(),
        any::<u16>(),
        65_536_usize..262_144,
        tail(),
        any::<u64>(),
    )
        .prop_map(|(file, whole, at, length, tail, seed)| {
            if whole {
                Added::CopyOf { file, tail, seed }
            } else {
                Added::SliceOf {
                    file,
                    at,
                    length,
                    tail,
                    seed,
                }
            }
        })
}

/// Any added seat over `count` carried files.
fn added_seat(count: usize) -> impl Strategy<Value = Added> {
    prop_oneof![
        (length(), any::<u64>()).prop_map(|(length, seed)| Added::Fresh { length, seed }),
        sharing_seat(0..count),
    ]
}

/// The lengths of the carried file a sharing case copies from: several CDC
/// chunks (16 KiB minimum, 64 KiB average, 256 KiB maximum).
const HOLDER_LENGTHS: std::ops::RangeInclusive<usize> = 196_608..=393_216;

/// A delta case. `changes`: whether in-place changes are drawn; `refused`:
/// the range of `SQLite` seats; `sharing`: carried file 0 is several chunks
/// long and the first added seat copies or slices it, so the case reaches
/// chunk sharing (the other added seats are free, and may copy it again).
fn delta_case(
    changes: bool,
    refused: std::ops::RangeInclusive<usize>,
    sharing: bool,
) -> impl Strategy<Value = DeltaCase> {
    let holder_length = if sharing {
        HOLDER_LENGTHS.boxed()
    } else {
        length().boxed()
    };
    (
        (0..DIRECTORIES.len(), holder_length, any::<u64>()),
        prop::collection::vec((0..DIRECTORIES.len(), length(), any::<u64>()), 0..=5),
    )
        .prop_flat_map(move |(holder, others)| {
            let mut files = vec![holder];
            files.extend(others);
            let count = files.len();
            let change = if changes {
                change().boxed()
            } else {
                Just(Change::Same).boxed()
            };
            let added = if sharing {
                (
                    sharing_seat(Just(0_usize)),
                    prop::collection::vec(added_seat(count), 0..=2),
                )
                    .prop_map(|(shared, others)| {
                        let mut added = vec![shared];
                        added.extend(others);
                        added
                    })
                    .boxed()
            } else {
                prop::collection::vec(added_seat(count), 0..=3).boxed()
            };
            (
                Just(files),
                prop::collection::vec(change, count),
                added,
                refused.clone(),
            )
                .prop_map(|(files, changes, added, refused)| DeltaCase {
                    files,
                    changes,
                    added,
                    refused,
                })
        })
}

/// Whether a case reaches chunk sharing: the destination already holds some
/// chunk of the changed and added seats, so the bound of inequality 2 is
/// strictly below their size.
fn shares_a_chunk(case: &DeltaCase) -> bool {
    let model = delta_model(case);
    model.absent_bytes < model.changed_bytes
}

proptest! {
    #![proptest_config(test_support::prop_config(6))]

    /// P21 transfer leg, inequality 1: after in-place changes and added
    /// seats, the rerun reads at most the changed seats' bytes, and its walk
    /// is metadata only.
    #[test]
    fn p21_a_rerun_reads_at_most_the_changed_seats(case in delta_case(true, 0..=0, false)) {
        check_delta(&case, Clauses::ReadsOnly, Twins::PerSeat);
    }

    /// P21 transfer leg, every clause, over added seats (no in-place change):
    /// both inequalities, convergence, and an unchanged rerun at 0 and 0.
    /// Every case shares at least one chunk with a carried output, so
    /// inequality 2 is never the trivial "at most the added size".
    #[test]
    fn p21_added_seats_cross_as_absent_chunks_and_converge(case in delta_case(false, 0..=0, true)) {
        prop_assume!(shares_a_chunk(&case));
        check_delta(&case, Clauses::All, Twins::PerSeat);
    }
}

proptest! {
    #![proptest_config(test_support::prop_config(4))]

    /// P21 transfer leg, every clause, over in-place changes (#187,
    /// WP0(d)): a changed seat whose old output this store wrote is
    /// superseded, so it crosses as its absent chunks, converges, and the
    /// rerun after it reads and receives nothing.
    #[test]
    fn p21_changed_seats_cross_as_absent_chunks_and_converge(case in delta_case(true, 0..=0, true)) {
        check_delta(&case, Clauses::All, Twins::PerSeat);
    }

    /// P21 transfer leg, every clause, with refused `SQLite` seats (#186):
    /// each is sniffed once and refused from its record afterwards, so an
    /// unchanged estate reads 0 bytes, content or sniff.
    #[test]
    fn p21_with_refused_seats_an_unchanged_rerun_reads_nothing(case in delta_case(false, 1..=2, true)) {
        check_delta(&case, Clauses::All, Twins::PerSeat);
    }
}

/// P21 PINNED (#187's counterexample, with #186's beside it): after a first
/// pass, one byte is appended to a 256 KiB file and 60 229 bytes to a
/// smaller one, a third file is copied with a tail, and two refused seats
/// stay as they are. Both changed seats are superseded and converge, the
/// wire carries only their absent chunks, and no refused seat is opened.
#[test]
fn p21_pinned_changed_seats_are_superseded_beside_unchanged_refused_seats() {
    let case = DeltaCase {
        files: vec![
            (0, 262_144, 51),
            (0, 36_675, 52),
            (2, 10_370, 53),
            (1, 579, 54),
            (1, 7_682, 55),
            (2, 0, 56),
        ],
        changes: vec![
            Change::Append {
                length: 1,
                seed: 57,
            },
            Change::Append {
                length: 60_229,
                seed: 58,
            },
            Change::Same,
            Change::Same,
            Change::Same,
            Change::Same,
        ],
        added: vec![Added::CopyOf {
            file: 2,
            tail: 3_624,
            seed: 59,
        }],
        refused: 2,
    };
    let model = delta_model(&case);
    assert!(
        model.absent_bytes < model.changed_bytes,
        "the appended file keeps its leading chunks: absent {} of {}",
        model.absent_bytes,
        model.changed_bytes
    );
    check_delta(&case, Clauses::All, Twins::PerSeat);
}

/// P21 PINNED (#186): a refused seat is remembered by its stat identity. A
/// rewritten one is sniffed again, exactly once; its unchanged neighbour is
/// not opened. One rewritten as an ordinary file is carried, and the bytes
/// its sniff read are then content, not sniff bytes.
#[test]
fn p21_pinned_a_changed_refused_seat_is_sniffed_again_and_an_unchanged_one_is_not() {
    let fixture = Fixture::new();
    let carried = noise(61, 30_000);
    fixture.write("f0", &carried);
    fixture.write("db0.sqlite", &sqlite_seat(0));
    fixture.write("d0/db1.sqlite", &sqlite_seat(1));
    let both = sqlite_refusals(&["db0.sqlite".to_owned(), "d0/db1.sqlite".to_owned()]);
    fixture.settle();
    let (first, counters) = fixture.run();
    assert_eq!(refusals(&first), both);
    assert_eq!(first.source_bytes_read, size_of(&carried));
    assert_eq!(counters.get(Counter::SourceSniff), 2 * SNIFF_BYTES);

    // One refused seat is rewritten, still a `SQLite` database.
    fixture.write("db0.sqlite", &sqlite_seat(7));
    fixture.settle();
    let (second, counters) = fixture.run();
    assert_eq!(refusals(&second), both, "the same refusals, the same code");
    assert_eq!(second.source_bytes_read, 0);
    assert_eq!(counters.get(Counter::SourceFileRead), 0);
    assert_eq!(
        counters.get(Counter::SourceSniff),
        SNIFF_BYTES,
        "only the changed seat is sniffed"
    );
    assert_eq!(counters.get(Counter::TransferRefusedSeatsRemembered), 1);
    assert_eq!(second.bytes_received, 0);

    // Unchanged again: nothing is opened.
    let (third, counters) = fixture.run();
    assert_eq!(refusals(&third), both);
    assert_eq!(third.source_bytes_read, 0);
    assert_eq!(counters.get(Counter::SourceSniff), 0);
    assert_eq!(counters.get(Counter::TransferRefusedSeatsRemembered), 2);

    // The other seat becomes an ordinary file: carried, read once as content.
    let ordinary = noise(62, 5_000);
    fixture.write("d0/db1.sqlite", &ordinary);
    fixture.settle();
    let (fourth, counters) = fixture.run();
    assert_eq!(
        refusals(&fourth),
        sqlite_refusals(&["db0.sqlite".to_owned()])
    );
    assert_eq!(fourth.source_bytes_read, size_of(&ordinary));
    assert_eq!(counters.get(Counter::SourceFileRead), size_of(&ordinary));
    assert_eq!(counters.get(Counter::SourceSniff), 0);
    assert!(fixture.holds("d0/db1.sqlite", &ordinary));
}

/// P21 PINNED (WP0(d), no-clobber): a rerun supersedes only an output whose
/// identity is this store's own row. Of two changed seats, one whose output
/// another writer replaced, and one whose output was rewritten in place, are
/// refused `DESTINATION_OCCUPIED`, left byte for byte as they are, and cost
/// no wire bytes; the third, untouched, is superseded.
#[test]
fn p21_pinned_a_changed_seat_never_supersedes_a_file_this_store_does_not_own() {
    let fixture = Fixture::new();
    let first: Vec<Vec<u8>> = (0..3).map(|index| noise(70 + index, 40_000)).collect();
    for (index, content) in first.iter().enumerate() {
        fixture.write(&format!("f{index}"), content);
    }
    fixture.settle();
    let (pass, _) = fixture.run();
    assert!(pass.refusals.is_empty(), "{:?}", pass.refusals);

    // Another writer replaces f0's output (a new inode) and rewrites f1's
    // in place (the same inode, other bytes).
    let theirs = noise(80, 12_345);
    let aside = fixture.base.join("aside");
    std::fs::write(&aside, &theirs).unwrap();
    std::fs::rename(&aside, fixture.destination().join("f0")).unwrap();
    let rewritten = noise(81, 40_000);
    std::fs::write(fixture.destination().join("f1"), &rewritten).unwrap();
    // Every seat changes on the source.
    let second: Vec<Vec<u8>> = (0..3).map(|index| noise(90 + index, 50_000)).collect();
    for (index, content) in second.iter().enumerate() {
        fixture.write(&format!("f{index}"), content);
    }
    fixture.settle();

    let (rerun, counters) = fixture.run();
    let occupied = |name: &str| (name.to_owned(), "DESTINATION_OCCUPIED".to_owned());
    assert_eq!(refusals(&rerun), [occupied("f0"), occupied("f1")]);
    assert!(
        fixture.holds("f0", &theirs),
        "another writer's file is kept"
    );
    assert!(
        fixture.holds("f1", &rewritten),
        "a rewritten output is kept"
    );
    assert!(
        fixture.holds("f2", &second[2]),
        "its own output is superseded"
    );
    assert_eq!(counters.get(Counter::OutputsSuperseded), 1);
    assert!(
        rerun.source_bytes_read <= 150_000,
        "inequality 1: {}",
        rerun.source_bytes_read
    );
    assert!(
        rerun.bytes_received <= size_of(&second[2]),
        "no wire bytes for a refused seat: {}",
        rerun.bytes_received
    );
    // Nothing of this store's is left beside them.
    let mut names: Vec<String> = std::fs::read_dir(fixture.destination())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["f0", "f1", "f2"]);

    // The refusals stand on every run, and the superseded seat is reused.
    // A refused seat is not read again (#187 review, R25): the destination
    // remembers each refusal under the seat's row key and the identity of
    // the file it found, and refuses the entry when it is offered.
    for _ in 0..2 {
        let (again, counters) = fixture.run();
        assert_eq!(refusals(&again), [occupied("f0"), occupied("f1")]);
        assert_eq!(again.reused, 1);
        assert_eq!(again.bytes_received, 0);
        assert_eq!(
            again.source_bytes_read, 0,
            "an unchanged seat standing refused is not opened"
        );
        assert_eq!(counters.get(Counter::SourceFileRead), 0);
        assert_eq!(
            counters.get(Counter::TransferRefusedOutputsRemembered),
            2,
            "each refusal is answered from the destination's record"
        );
        assert_eq!(counters.get(Counter::OutputsSuperseded), 0);
        assert!(fixture.holds("f0", &theirs));
        assert!(fixture.holds("f1", &rewritten));
    }

    // The other writer's file changes: the record no longer answers, the
    // seat is read once more, and the new refusal is remembered.
    let theirs = noise(82, 23_456);
    std::fs::write(fixture.destination().join("f0"), &theirs).unwrap();
    settle_racy_window(&fixture.destination()).unwrap();
    let (moved, _) = fixture.run();
    assert_eq!(refusals(&moved), [occupied("f0"), occupied("f1")]);
    assert_eq!(moved.source_bytes_read, size_of(&second[0]));
    let (after, _) = fixture.run();
    assert_eq!(refusals(&after), [occupied("f0"), occupied("f1")]);
    assert_eq!(after.source_bytes_read, 0);
    assert!(fixture.holds("f0", &theirs));
}

/// P21 PINNED (WP0(c) inequality 2, across a supersede): a file is replaced
/// by other bytes and its old bytes reappear under another name, 150 seats
/// behind it in the walk (a rename with a new file in its place). The
/// destination held those bytes when the run began, so they cross no wire,
/// whether the new name is planned before the old output is exchanged away
/// (they are read at its path) or after (they are read from the output the
/// superseding publish displaced). Which of the two a run takes depends on
/// when its group commits; `transfer::tests::
/// a_chunk_of_a_superseded_output_is_still_filled_locally` pins the second.
#[test]
fn p21_pinned_a_seat_moved_out_of_a_changed_file_crosses_no_bytes() {
    let fixture = Fixture::new();
    let moved = noise(101, 600_000);
    fixture.write("a", &moved);
    fixture.settle();
    let (first, _) = fixture.run();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);

    let replaced = noise(102, 500_000);
    let mut corpus: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    corpus.insert("a".to_owned(), replaced);
    for index in 0..150_u64 {
        corpus.insert(format!("m{index:03}"), noise(1_000 + index, 1_000));
    }
    corpus.insert("z".to_owned(), moved.clone());
    for (rel, content) in &corpus {
        fixture.write(rel, content);
    }
    fixture.settle();

    let held: HashSet<[u8; 32]> = chunks(&moved)
        .into_iter()
        .map(|(digest, _)| digest)
        .collect();
    let absent: BTreeMap<[u8; 32], u64> = corpus
        .values()
        .flat_map(|content| chunks(content))
        .filter(|(digest, _)| !held.contains(digest))
        .collect();
    let absent_bytes: u64 = absent.values().sum();
    let changed_bytes: u64 = corpus.values().map(|content| size_of(content)).sum();
    assert_eq!(absent_bytes, changed_bytes - size_of(&moved));

    let (rerun, counters) = fixture.run();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!(counters.get(Counter::OutputsSuperseded), 1);
    assert_eq!(rerun.source_bytes_read, changed_bytes, "each read once");
    assert!(
        rerun.bytes_received <= absent_bytes,
        "inequality 2: received {} of {absent_bytes} absent bytes (the moved seat holds {})",
        rerun.bytes_received,
        moved.len()
    );
    for (rel, content) in &corpus {
        assert!(fixture.holds(rel, content), "{rel} did not converge");
    }
    assert_unchanged_estate_reads_nothing(&fixture, &[]);
}

/// P21 PINNED: an added seat that copies a large carried file plus a tail
/// shares most of its chunks with the destination, so inequality 2 bounds
/// its wire bytes well below its size.
#[test]
fn p21_pinned_an_added_copy_crosses_as_little_more_than_its_tail() {
    let case = DeltaCase {
        files: vec![(0, 262_144, 21), (1, 200_000, 22)],
        changes: vec![Change::Same, Change::Same],
        added: vec![
            Added::CopyOf {
                file: 0,
                tail: 40_000,
                seed: 23,
            },
            Added::CopyOf {
                file: 1,
                tail: 1,
                seed: 24,
            },
        ],
        refused: 0,
    };
    assert!(shares_a_chunk(&case));
    check_delta(&case, Clauses::All, Twins::PerSeat);
}

/// The sharing shapes one run can mix: a holder in a nested directory is
/// copied whole twice with no tail (nothing absent for either), its first
/// 150 000 bytes are sliced out once with a tail, and two added seats carry
/// the same fresh bytes.
fn twins_case() -> DeltaCase {
    DeltaCase {
        files: vec![(2, 300_000, 41), (0, 9_106, 42)],
        changes: vec![Change::Same, Change::Same],
        added: vec![
            Added::CopyOf {
                file: 0,
                tail: 0,
                seed: 43,
            },
            Added::CopyOf {
                file: 0,
                tail: 0,
                seed: 44,
            },
            Added::SliceOf {
                file: 0,
                at: 0,
                length: 150_000,
                tail: 5_000,
                seed: 45,
            },
            Added::Fresh {
                length: 200_000,
                seed: 46,
            },
            Added::Fresh {
                length: 200_000,
                seed: 46,
            },
        ],
        refused: 0,
    }
}

/// P21 PINNED: two whole copies and a slice of one nested holder, and twin
/// fresh seats, in one run. The copies put nothing on the wire; the twins'
/// bytes are absent once and may cross once per seat.
#[test]
fn p21_pinned_copies_slices_and_twins_of_one_holder() {
    let case = twins_case();
    let model = delta_model(&case);
    assert_eq!(model.twin_bytes, 200_000, "the twins share every chunk");
    // The twins' bytes once, and less than the slice and its tail: the
    // slice shares its leading chunks with the holder.
    assert!(
        model.absent_bytes < 200_000 + 150_000 + 5_000,
        "absent {} of {} changed",
        model.absent_bytes,
        model.changed_bytes
    );
    check_delta(&case, Clauses::All, Twins::PerSeat);
}

/// P21 PINNED, inequality 2 read strictly: a chunk two added seats of one
/// run both carry is absent once, so it crosses once. Today that depends
/// on whether the first twin is staged before the second is planned: of two
/// runs of this row, one received the twins' 200 000 bytes once and one
/// received them twice.
#[test]
#[ignore = "unstable: twin added seats may cross their shared absent chunks once per seat (no issue yet; needs a ruling on the reading of inequality 2)"]
fn p21_pinned_twin_seats_cross_their_shared_chunks_once() {
    check_delta(&twins_case(), Clauses::All, Twins::Once);
}

// ---------------------------------------------------------------------------
// P18 HINTS, end to end
// ---------------------------------------------------------------------------

/// What happens to a holder on the destination before the new file arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Kept,
    Deleted,
    /// Overwritten in place with other bytes of the same length.
    Overwritten,
}

#[derive(Debug, Clone)]
struct HintCase {
    /// Holders by (length, seed, fate).
    holders: Vec<(usize, u64, Fate)>,
    /// The new file: slices (holder, start fraction, length), then a tail.
    slices: Vec<(usize, u16, usize)>,
    tail: (usize, u64),
}

fn check_hints(case: &HintCase) {
    let fixture = Fixture::new();
    let holders: Vec<Vec<u8>> = case
        .holders
        .iter()
        .map(|(length, seed, _)| noise(*seed, *length))
        .collect();
    for (index, content) in holders.iter().enumerate() {
        fixture.write(&format!("holder{index}"), content);
    }
    let (first, _) = fixture.run();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);

    // Only the new file is offered next: the holders leave the source, and
    // their fates are applied on the destination.
    let mut surviving: HashSet<[u8; 32]> = HashSet::new();
    for (index, (content, (_, seed, fate))) in holders.iter().zip(&case.holders).enumerate() {
        let name = format!("holder{index}");
        std::fs::remove_file(fixture.source().join(&name)).unwrap();
        let held = fixture.destination().join(&name);
        match fate {
            Fate::Kept => surviving.extend(chunks(content).into_iter().map(|(digest, _)| digest)),
            Fate::Deleted => std::fs::remove_file(held).unwrap(),
            Fate::Overwritten => {
                std::fs::write(held, noise(seed ^ 0xFFFF, content.len())).unwrap();
            }
        }
    }
    let mut new = Vec::new();
    for (holder, at, length) in &case.slices {
        let content = &holders[*holder];
        let start = content.len() * usize::from(*at) / usize::from(u16::MAX);
        let end = (start + length).min(content.len());
        new.extend_from_slice(&content[start..end]);
    }
    new.extend(noise(case.tail.1, case.tail.0));
    fixture.write("new", &new);
    // A chunk the file repeats is requested once (the fill plan is by
    // digest), so the wire carries each absent digest once.
    let absent: BTreeMap<[u8; 32], u64> = chunks(&new)
        .into_iter()
        .filter(|(digest, _)| !surviving.contains(digest))
        .collect();
    let expected: u64 = absent.values().sum();

    let (second, _) = fixture.run();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(second.completed, 1);
    assert_eq!(second.source_bytes_read, size_of(&new), "read once");
    assert_eq!(
        second.bytes_received,
        expected,
        "wire bytes == size {} less the verified survivors' bytes",
        new.len()
    );
    assert!(fixture.holds("new", &new));
}

fn hint_case() -> impl Strategy<Value = HintCase> {
    prop::collection::vec(
        (
            131_072_usize..786_432,
            any::<u64>(),
            prop_oneof![
                Just(Fate::Kept),
                Just(Fate::Deleted),
                Just(Fate::Overwritten)
            ],
        ),
        1..=3,
    )
    .prop_flat_map(|holders| {
        let count = holders.len();
        (
            Just(holders),
            prop::collection::vec((0..count, any::<u16>(), 65_536_usize..524_288), 1..=4),
            (0_usize..131_072, any::<u64>()),
        )
            .prop_map(|(holders, slices, tail)| HintCase {
                holders,
                slices,
                tail,
            })
    })
}

proptest! {
    #![proptest_config(test_support::prop_config(6))]

    /// P18 end to end: wire bytes are exactly the new file's size less the
    /// bytes its surviving holders still verify.
    #[test]
    fn p18_wire_bytes_are_the_size_less_verified_survivors(case in hint_case()) {
        check_hints(&case);
    }
}

/// P18 PINNED: the newest holder of the shared chunks is lost, an older one
/// still serves them (#59 review, hint ordering).
#[test]
fn p18_pinned_a_lost_newest_holder_falls_back_to_an_older_one() {
    let fixture = Fixture::new();
    let prefix = noise(10, 1 << 20);
    let with_tail = |seed: u64| {
        let mut bytes = prefix.clone();
        bytes.extend(noise(seed, 1 << 20));
        bytes
    };
    let a = with_tail(11);
    fixture.write("a", &a);
    assert!(fixture.run().0.refusals.is_empty());
    let b = with_tail(12);
    fixture.write("b", &b);
    assert!(fixture.run().0.refusals.is_empty());
    // `b` is now the newest holder of the prefix chunks. Lose it on both
    // sides and offer a third file over the same prefix.
    std::fs::remove_file(fixture.destination().join("b")).unwrap();
    std::fs::remove_file(fixture.source().join("b")).unwrap();
    let c = with_tail(13);
    fixture.write("c", &c);
    let held: HashSet<[u8; 32]> = chunks(&a).into_iter().map(|(digest, _)| digest).collect();
    let absent: BTreeMap<[u8; 32], u64> = chunks(&c)
        .into_iter()
        .filter(|(digest, _)| !held.contains(digest))
        .collect();
    let expected: u64 = absent.values().sum();
    let (third, _) = fixture.run();
    assert!(third.refusals.is_empty(), "{:?}", third.refusals);
    assert_eq!(third.bytes_received, expected);
    assert!(fixture.holds("c", &c));
}

// ---------------------------------------------------------------------------
// P19 RACY, file half, end to end
// ---------------------------------------------------------------------------

/// How a P19 case makes its new seats racy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Racy {
    /// Written just before the run: stamped inside the tick before it.
    Recent,
    /// Stamped an hour later than the clock.
    Future,
}

/// The longest a `Recent` case may take from its first racy write to the end
/// of the copy that must see it racy: well inside the 2 s racy window.
const RECENT_BUDGET: Duration = Duration::from_millis(1_500);

/// A `Recent` case can only be judged when its copy ran inside the racy
/// window of the seats it wrote. On a host too loaded for that, the case is
/// built again from scratch; it is never judged on a stale window.
fn check_racy(files: &[(usize, u64)], racy: &[(usize, u64)], how: Racy) {
    for _ in 0..8 {
        if racy_attempt(files, racy, how) {
            return;
        }
    }
    panic!("no attempt copied its racy seats within {RECENT_BUDGET:?} of writing them");
}

/// One attempt at a P19 case; `false` when a `Recent` copy overran
/// [`RECENT_BUDGET`] and nothing was asserted about it.
fn racy_attempt(files: &[(usize, u64)], racy: &[(usize, u64)], how: Racy) -> bool {
    let fixture = Fixture::new();
    let mut corpus: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (index, (length, seed)) in files.iter().enumerate() {
        let rel = format!("f{index}");
        let content = noise(*seed, *length);
        fixture.write(&rel, &content);
        corpus.insert(rel, content);
    }
    fixture.settle();
    let (first, _) = fixture.run();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);

    let mut racy_bytes = 0_u64;
    let contents: Vec<(String, Vec<u8>)> = racy
        .iter()
        .enumerate()
        .map(|(index, (length, seed))| (format!("racy{index}"), noise(*seed, *length)))
        .collect();
    for (rel, content) in &contents {
        racy_bytes += size_of(content);
        corpus.insert(rel.clone(), content.clone());
    }
    let racy_count = racy.len() as u64;

    // Sent, counted racy, and never recorded: the next run reads it again.
    let mut started = None;
    let (second, counters) = fixture.run_after(|| {
        started = Some(Instant::now());
        for (rel, content) in &contents {
            fixture.write(rel, content);
            if how == Racy::Future {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(fixture.source().join(rel))
                    .unwrap()
                    .set_modified(SystemTime::now() + Duration::from_hours(1))
                    .unwrap();
            }
        }
    });
    if how == Racy::Recent && started.unwrap().elapsed() > RECENT_BUDGET {
        return false;
    }
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(second.source_bytes_read, racy_bytes, "racy seats are read");
    assert_eq!(counters.get(Counter::TransferRacyCaptures), racy_count);
    for (rel, content) in &corpus {
        assert!(fixture.holds(rel, content), "{rel}");
    }
    if how == Racy::Recent {
        fixture.settle();
    }
    let (third, counters) = fixture.run();
    assert!(third.refusals.is_empty(), "{:?}", third.refusals);
    assert_eq!(
        third.source_bytes_read, racy_bytes,
        "a racy capture is never a Reuse"
    );
    assert_eq!(third.bytes_received, 0, "the existing output is adopted");
    let still_racy = if how == Racy::Future { racy_count } else { 0 };
    assert_eq!(counters.get(Counter::TransferRacyCaptures), still_racy);
    let (fourth, _) = fixture.run();
    let expected = if how == Racy::Future { racy_bytes } else { 0 };
    assert_eq!(
        fourth.source_bytes_read, expected,
        "settled seats are reused"
    );
    assert_eq!(fourth.bytes_received, 0);
    true
}

/// Stamp a source seat an hour ahead of the clock: racy on every run (#86).
fn stamp_ahead(fixture: &Fixture, rel: &str) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.source().join(rel))
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_hours(1))
        .unwrap();
}

/// P19 PINNED (#187 review): an output published from a racy capture is
/// this store's own. It has no reuse row (#86), so it is read again on
/// every run until its seat settles; but when the seat changes again
/// first, as an actively written file does, the output is superseded: it
/// is not refused `DESTINATION_OCCUPIED` for ever.
#[test]
fn p19_pinned_a_racy_publish_is_superseded_when_its_seat_changes_again() {
    let fixture = Fixture::new();
    fixture.write("settled", &noise(190, 30_000));
    fixture.settle();
    let (first, _) = fixture.run();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);

    let written = noise(191, 60_000);
    let (racy, counters) = fixture.run_after(|| {
        fixture.write("live", &written);
        stamp_ahead(&fixture, "live");
    });
    assert!(racy.refusals.is_empty(), "{:?}", racy.refusals);
    assert_eq!(counters.get(Counter::TransferRacyCaptures), 1);
    assert!(fixture.holds("live", &written));

    // Written again before any settled run adopted the output.
    let appended = noise(192, 70_000);
    let (changed, counters) = fixture.run_after(|| {
        fixture.write("live", &appended);
        stamp_ahead(&fixture, "live");
    });
    assert!(
        changed.refusals.is_empty(),
        "a racy publish's output is this store's own: {:?}",
        changed.refusals
    );
    assert_eq!(counters.get(Counter::OutputsSuperseded), 1);
    assert_eq!(counters.get(Counter::TransferRacyCaptures), 1);
    assert_eq!(changed.source_bytes_read, size_of(&appended));
    assert!(fixture.holds("live", &appended));

    // Unchanged and still racy: read again (no reuse row), adopted in
    // place, replaced by nothing and refused by nothing.
    let (again, counters) = fixture.run();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(again.source_bytes_read, size_of(&appended));
    assert_eq!(again.bytes_received, 0);
    assert_eq!(counters.get(Counter::OutputsSuperseded), 0);

    // Settled at last: read once more, recorded, and then reused.
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.source().join("live"))
        .unwrap()
        .set_modified(SystemTime::now())
        .unwrap();
    fixture.settle();
    let (settled, counters) = fixture.run();
    assert!(settled.refusals.is_empty(), "{:?}", settled.refusals);
    assert_eq!(settled.source_bytes_read, size_of(&appended));
    assert_eq!(settled.bytes_received, 0);
    assert_eq!(counters.get(Counter::TransferRacyCaptures), 0);
    let (warm, _) = fixture.run();
    assert_eq!((warm.reused, warm.source_bytes_read), (2, 0));
    assert!(fixture.holds("live", &appended));
}

/// P19 PINNED (#186, review): a refused seat that is racy when it is
/// sniffed is refused, never as content, and not remembered: every run
/// sniffs it again, 16 bytes, until it has settled; then it is sniffed once
/// more, remembered, and not opened again.
#[test]
fn p19_pinned_a_racy_refused_seat_is_sniffed_on_every_run_until_it_settles() {
    let fixture = Fixture::new();
    let sqlite = ["db0.sqlite".to_owned()];
    fixture.write(&sqlite[0], &sqlite_seat(0));
    stamp_ahead(&fixture, &sqlite[0]);
    for _ in 0..2 {
        let (racy, counters) = fixture.run();
        assert_eq!(refusals(&racy), sqlite_refusals(&sqlite));
        assert_eq!(racy.source_bytes_read, 0, "a sniff is never content");
        assert_eq!(counters.get(Counter::SourceFileRead), 0);
        assert_eq!(
            counters.get(Counter::SourceSniff),
            SNIFF_BYTES,
            "a racy refused seat is sniffed again"
        );
        assert_eq!(counters.get(Counter::TransferRefusedSeatsRemembered), 0);
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.source().join(&sqlite[0]))
        .unwrap()
        .set_modified(SystemTime::now())
        .unwrap();
    fixture.settle();
    let (settled, counters) = fixture.run();
    assert_eq!(refusals(&settled), sqlite_refusals(&sqlite));
    assert_eq!(counters.get(Counter::SourceSniff), SNIFF_BYTES);
    assert_eq!(counters.get(Counter::TransferRefusedSeatsRemembered), 0);
    let (rerun, counters) = fixture.run();
    assert_eq!(refusals(&rerun), sqlite_refusals(&sqlite));
    assert_eq!(counters.get(Counter::SourceSniff), 0);
    assert_eq!(counters.get(Counter::TransferRefusedSeatsRemembered), 1);
}

/// A P19 case: settled files and racy seats by (length, seed), and how the
/// racy seats are made racy.
type RacyCase = (Vec<(usize, u64)>, Vec<(usize, u64)>, Racy);

fn racy_case() -> impl Strategy<Value = RacyCase> {
    (
        prop::collection::vec((length(), any::<u64>()), 1..=4),
        prop::collection::vec((length(), any::<u64>()), 1..=3),
        prop_oneof![Just(Racy::Recent), Just(Racy::Future)],
    )
}

proptest! {
    #![proptest_config(test_support::prop_config(4))]

    /// P19: a racy capture is sent but never recorded, so every later run
    /// reads it again until it is no longer racy.
    #[test]
    fn p19_a_racy_capture_is_sent_and_never_a_reuse(case in racy_case()) {
        let (files, racy, how) = case;
        check_racy(&files, &racy, how);
    }
}
