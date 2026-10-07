//! WP0(g) (OI-1003-Q20, adopted with conditions by OI-1003-Q37, built on
//! OI-1003-Q104): the source ledger's row commits run
//! `synchronous=NORMAL`, `fullfsync=OFF`.
//!
//! P79 RELAXED-LEDGER-LOSS runs real copies, then takes the source store
//! through a power loss as `SQLite` recovers from one: the store's WAL is
//! cut, torn or garbled at a generated point past the bytes its
//! `synchronous=FULL` creation commit synced, and its wal-index is removed.
//! Recovery keeps the commits before the damage and drops every later one.
//!
//! What is proven on each generated case:
//!
//! - the store's authority is the one its creation commit made (a lost
//!   authority re-keys every row, `MC_wp0g_authority`);
//! - the ledger holds exactly the rows committed before the damage, none
//!   torn (the model's loss is any subset, a superset of this suffix);
//! - the resume reads no byte of a seat the destination holds, whatever the
//!   ledger lost (R25's committed-row reading, OI-1003-Q40: the destination
//!   answers `Reuse` from its own row and the ledger is never asked);
//! - a lost row costs a read only at a seat a third party removed from the
//!   destination, and at most that seat's bytes once: the seat whose
//!   content the destination could have filled from another output, had
//!   the row survived;
//! - every destination byte is the source's, and the next run reads 0.

use super::*;
use crate::counters::{Counter, Counters};
use crate::io::durable::LedgerSync;

/// Fixed seeds of P79 (no fuzzing): each generates a corpus, its crash
/// point and what a third party removed.
const P79_SEEDS: [u64; 16] = [
    1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987, 1597,
];

/// How the power loss left the WAL past the last surviving commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Damage {
    /// The file ends at a commit boundary: the later commits never reached
    /// the disk.
    Cut,
    /// The file ends inside the next commit's frames.
    Torn,
    /// The next commit's frames hold garbage, and every later commit's
    /// bytes are intact behind them (writes that reached the disk out of
    /// order). Recovery must stop at the garbage.
    Garbled,
}

/// One generated P79 case.
#[derive(Debug)]
struct P79Case {
    /// Each seat's size.
    sizes: Vec<usize>,
    /// The earlier seat whose content this seat repeats, if any.
    twin_of: Vec<Option<usize>>,
    /// Ledger row commits that survive: seats `0..kept` keep their rows.
    kept: usize,
    damage: Damage,
    /// Seats whose output a third party removed before the resume.
    deleted: Vec<bool>,
}

impl P79Case {
    /// The seat whose content `seat` holds.
    fn origin(&self, seat: usize) -> usize {
        self.twin_of[seat].unwrap_or(seat)
    }

    /// Whether another seat's output, still at the destination, holds
    /// `seat`'s content.
    fn fillable(&self, seat: usize) -> bool {
        self.sizes[seat] > 0
            && (0..self.sizes.len()).any(|other| {
                other != seat && self.origin(other) == self.origin(seat) && !self.deleted[other]
            })
    }

    /// Whether `seat`'s ledger row was lost.
    const fn lost(&self, seat: usize) -> bool {
        seat >= self.kept
    }
}

fn p79_case(seed: u64) -> P79Case {
    const SIZES: [usize; 5] = [0, 1, 4_096, 70_000, 300_000];
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % bound as u64).unwrap()
    };
    let seats = 3 + next(5);
    let mut sizes: Vec<usize> = Vec::with_capacity(seats);
    let mut twin_of: Vec<Option<usize>> = Vec::with_capacity(seats);
    for seat in 0..seats {
        // A third of the later seats repeat an earlier original's content.
        let twin = (seat > 0 && next(3) == 0).then(|| next(seat));
        let twin: Option<usize> = twin.map(|earlier| twin_of[earlier].unwrap_or(earlier));
        sizes.push(twin.map_or_else(|| SIZES[next(5)], |earlier| sizes[earlier]));
        twin_of.push(twin);
    }
    let kept = next(seats + 1);
    let damage = if kept == seats {
        Damage::Cut
    } else {
        [Damage::Cut, Damage::Torn, Damage::Garbled][next(3)]
    };
    // At most one removed output per content, so what a removed seat can be
    // filled from does not depend on the order the resume takes them in.
    let mut deleted = vec![false; seats];
    for seat in 0..seats {
        let origin = twin_of[seat].unwrap_or(seat);
        let taken =
            (0..seat).any(|other| deleted[other] && twin_of[other].unwrap_or(other) == origin);
        deleted[seat] = !taken && next(2) == 0;
    }
    P79Case {
        sizes,
        twin_of,
        kept,
        damage,
        deleted,
    }
}

/// The seeds generate every damage, both ends of the crash range, and a
/// removed seat in each of the four cells (row lost or kept, by content
/// held elsewhere or not), so no leg of the property is vacuous.
#[test]
fn p79_seeds_cover_every_loss() {
    let cases: Vec<P79Case> = P79_SEEDS.iter().map(|seed| p79_case(*seed)).collect();
    for damage in [Damage::Cut, Damage::Torn, Damage::Garbled] {
        assert!(cases.iter().any(|case| case.damage == damage), "{damage:?}");
    }
    assert!(cases.iter().any(|case| case.kept == 0), "every row lost");
    assert!(
        cases.iter().any(|case| case.kept == case.sizes.len()),
        "no row lost"
    );
    for (lost, fillable) in [(true, true), (true, false), (false, true), (false, false)] {
        assert!(
            cases.iter().any(|case| {
                (0..case.sizes.len()).any(|seat| {
                    case.deleted[seat]
                        && case.sizes[seat] > 0
                        && case.lost(seat) == lost
                        && case.fillable(seat) == fillable
                })
            }),
            "a removed seat with lost={lost} fillable={fillable}"
        );
    }
    assert!(
        cases.iter().any(|case| {
            (0..case.sizes.len())
                .any(|seat| !case.deleted[seat] && case.lost(seat) && case.sizes[seat] > 0)
        }),
        "a held seat whose row was lost"
    );
}

fn wal(state: &Path) -> PathBuf {
    state.join("transfer.sqlite-wal")
}

fn wal_len(state: &Path) -> u64 {
    std::fs::metadata(wal(state)).unwrap().len()
}

/// A power loss as the source store's files show it afterwards: the WAL
/// keeps its first `keep` bytes, `damage` is what follows them, and the
/// wal-index, which is never durable state, is gone.
fn power_loss(state: &Path, keep: u64, next_commit: u64, damage: Damage) {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(wal(state))
        .unwrap();
    match damage {
        Damage::Cut => file.set_len(keep).unwrap(),
        Damage::Torn => file.set_len(keep + (next_commit - keep) / 2).unwrap(),
        Damage::Garbled => {
            let mut file = file;
            let at = keep + (next_commit - keep) / 2;
            let mut bytes = [0_u8; 64];
            file.seek(SeekFrom::Start(at)).unwrap();
            file.read_exact(&mut bytes).unwrap();
            for byte in &mut bytes {
                *byte ^= 0xff;
            }
            file.seek(SeekFrom::Start(at)).unwrap();
            file.write_all(&bytes).unwrap();
        }
    }
    let _ = std::fs::remove_file(state.join("transfer.sqlite-shm"));
}

/// P79 RELAXED-LEDGER-LOSS (WP0(g), OI-1003-Q20, Q37, Q40, R-N58), over
/// generated crash points of the relaxed source ledger.
#[test]
fn p79_a_lost_relaxed_ledger_row_costs_at_most_its_seat() {
    for seed in P79_SEEDS {
        p79_check(seed, &p79_case(seed));
    }
}

#[allow(clippy::too_many_lines)]
fn p79_check(seed: u64, case: &P79Case) {
    let corpus = Corpus::new();
    let seats = case.sizes.len();
    let name = |seat: usize| format!("p79-{seat}");
    let source = corpus.base.join("source");
    let destination = corpus.base.join("destination");
    let state = corpus.base.join("source-state");
    let content = |seat: usize| {
        let origin = case.origin(seat);
        noise(seed * 1_000 + origin as u64, case.sizes[origin])
    };

    // The store's creation: its schema and authority, committed and synced
    // (`synchronous=FULL`) before any row. What a power loss cannot take is
    // what that commit synced; were it relaxed, nothing would be synced, the
    // whole WAL could go, and the case that loses every row would lose the
    // authority with them (the check below, and `MC_wp0g_authority`).
    let created = Store::open(&state).unwrap();
    let authority = created.authority().unwrap();
    let synced = if created.synchronous().unwrap() == 2 {
        wal_len(&state)
    } else {
        0
    };
    drop(created);

    // One seat a run, so each seat's row is one relaxed commit, in order.
    // Every capture is non-racy: the capture clock is far past every stamp.
    std::fs::write(source.join(name(0)), content(0)).unwrap();
    let _clock = PinnedClock::at(
        &corpus,
        stamp_ns(&source.join(name(0))) + 86_400 * 1_000_000_000,
    );
    let before = Counters::snapshot();
    let mut bounds = vec![synced];
    for seat in 0..seats {
        if seat > 0 {
            std::fs::write(source.join(name(seat)), content(seat)).unwrap();
        }
        let carried = corpus.run().unwrap();
        assert!(carried.refusals.is_empty(), "seed {seed}: {carried:?}");
        assert_eq!(carried.completed, 1, "seed {seed}: seat {seat}");
        bounds.push(wal_len(&state));
        assert!(
            bounds[seat + 1] > bounds[seat],
            "seed {seed}: seat {seat}'s row is in the WAL"
        );
    }
    let committed = Counters::snapshot().since(before);
    assert!(
        committed.get(Counter::SourceLedgerRelaxedCommits) >= seats as u64,
        "seed {seed}: every row commit was relaxed"
    );
    assert_eq!(
        Store::open(&state).unwrap().row_counts().unwrap().0,
        seats as u64
    );

    // The power loss, and what a third party did to the destination.
    let next_commit = bounds
        .get(case.kept + 1)
        .copied()
        .unwrap_or(bounds[case.kept]);
    power_loss(&state, bounds[case.kept], next_commit, case.damage);
    let recovered = Store::open(&state).unwrap();
    assert_eq!(
        recovered.authority().unwrap(),
        authority,
        "seed {seed}: the authority survives whatever the ledger lost"
    );
    assert_eq!(
        recovered.row_counts().unwrap().0,
        case.kept as u64,
        "seed {seed}: exactly the rows committed before the damage"
    );
    drop(recovered);

    let (mut want_read, mut want_miss) = (0_u64, 0_u64);
    for seat in 0..seats {
        if !case.deleted[seat] {
            continue;
        }
        std::fs::remove_file(destination.join(name(seat))).unwrap();
        if case.sizes[seat] == 0 {
            continue;
        }
        if case.lost(seat) {
            // Read once to build the manifest the lost row held.
            want_read += case.sizes[seat] as u64;
            want_miss += 1;
        } else if !case.fillable(seat) {
            // The row survived, and the destination holds none of its
            // chunks: they are read whatever the ledger says.
            want_read += case.sizes[seat] as u64;
        }
    }

    let before = Counters::snapshot();
    let resumed = corpus.run().unwrap();
    let counted = Counters::snapshot().since(before);
    assert!(resumed.refusals.is_empty(), "seed {seed}: {resumed:?}");
    assert_eq!(
        resumed.source_bytes_read, want_read,
        "seed {seed}: {case:?}: only removed seats are read, and a lost row costs its seat once"
    );
    assert_eq!(
        resumed.reused,
        case.deleted.iter().filter(|deleted| !**deleted).count() as u64,
        "seed {seed}: every seat the destination holds is reused, row or no row"
    );
    // Process-wide, so other tests may add to it: a lower bound here.
    assert!(
        counted.get(Counter::SourceLedgerMissReads) >= want_miss,
        "seed {seed}: each lost row that cost a read is counted"
    );
    for seat in 0..seats {
        assert_eq!(
            std::fs::read(destination.join(name(seat))).unwrap(),
            content(seat),
            "seed {seed}: seat {seat} holds the source's bytes"
        );
    }
    assert_eq!(Store::open(&state).unwrap().authority().unwrap(), authority);

    let settled = corpus.run().unwrap();
    assert!(settled.refusals.is_empty(), "seed {seed}: {settled:?}");
    assert_eq!(settled.source_bytes_read, 0, "seed {seed}: {settled:?}");
    assert_eq!(settled.reused, seats as u64, "seed {seed}: {settled:?}");
}

/// OI-1003-Q37: "a corrupt or absent source ledger is treated as empty". A
/// ledger read that fails is a miss under the relaxed ledger, and the
/// failure itself under the strict one.
#[test]
fn an_unreadable_relaxed_ledger_reads_as_a_miss() {
    let failed = || Err::<Option<u8>, _>(BulkloadRefusal::SqliteIntegrityCheckFailed);
    let before = Counters::snapshot();
    assert_eq!(ledger_read(LedgerSync::Relaxed, failed()), Ok(None));
    assert!(
        Counters::snapshot()
            .since(before)
            .get(Counter::SourceLedgerUnreadable)
            >= 1
    );
    assert_eq!(
        ledger_read(LedgerSync::Full, failed()),
        Err(BulkloadRefusal::SqliteIntegrityCheckFailed)
    );
    for mode in [LedgerSync::Relaxed, LedgerSync::Full] {
        assert_eq!(ledger_read(mode, Ok(Some(7_u8))), Ok(Some(7)));
        assert_eq!(ledger_read(mode, Ok(None::<u8>)), Ok(None));
    }
}
