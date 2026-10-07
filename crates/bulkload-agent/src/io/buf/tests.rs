//! Buffer pool proofs: a proptest model of take/write/return, plus blocking
//! backpressure. Written to run under Miri as well (small geometry, no FFI).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use proptest::prelude::*;

use super::{Slab, SlabPool, SLAB_ALIGN, SLAB_BYTES};

#[test]
fn default_slabs_are_zeroed_aligned_and_four_mib() {
    let pool = SlabPool::new(2).unwrap();
    let slab = pool.take().unwrap();
    assert_eq!(slab.len(), SLAB_BYTES);
    assert_eq!(pool.slab_bytes(), SLAB_BYTES);
    assert_eq!(slab.as_ptr() as usize % SLAB_ALIGN, 0);
    assert!(slab.iter().all(|byte| *byte == 0));
}

#[test]
fn geometry_is_validated() {
    assert!(SlabPool::with_geometry(0, 64, 1).is_err());
    assert!(SlabPool::with_geometry(64, 64, 0).is_err());
    assert!(
        SlabPool::with_geometry(64, 48, 1).is_err(),
        "alignment must be a power of two"
    );
}

#[test]
fn take_blocks_until_a_slab_is_returned() {
    let pool = SlabPool::with_geometry(256, 64, 1).unwrap();
    let held = pool.take().unwrap();
    assert!(pool.try_take().unwrap().is_none());
    assert!(pool
        .take_timeout(Duration::from_millis(5))
        .unwrap()
        .is_none());

    let (sender, receiver) = mpsc::channel();
    let waiter = pool.clone();
    let handle = std::thread::spawn(move || {
        let mut slab = waiter.take().unwrap();
        slab[0] = 9;
        sender.send(()).unwrap();
        slab[0]
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while pool.stats().waiting == 0 {
        assert!(Instant::now() < deadline, "the second taker never blocked");
        std::thread::yield_now();
    }
    assert!(
        receiver.try_recv().is_err(),
        "take must block while the pool is empty"
    );
    drop(held);
    receiver.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(handle.join().unwrap(), 9);
    let stats = pool.stats();
    assert_eq!(
        (
            stats.allocated,
            stats.free,
            stats.outstanding,
            stats.waiting
        ),
        (1, 1, 0, 0)
    );
}

/// Review #8: a timeout too large for an `Instant` deadline must not
/// overflow; it waits forever, and a returned slab still ends the wait.
#[test]
fn an_unrepresentable_timeout_waits_forever_without_overflow() {
    let pool = SlabPool::with_geometry(64, 64, 1).unwrap();
    assert!(pool.take_timeout(Duration::MAX).unwrap().is_some());
    let held = pool.take().unwrap();
    let waiter = pool.clone();
    let handle = std::thread::spawn(move || waiter.take_timeout(Duration::MAX).unwrap().is_some());
    let deadline = Instant::now() + Duration::from_secs(20);
    while pool.stats().waiting == 0 {
        assert!(Instant::now() < deadline, "the taker never blocked");
        std::thread::yield_now();
    }
    drop(held);
    assert!(handle.join().unwrap());
}

#[derive(Clone, Debug)]
enum Step {
    Take,
    Return(usize),
    Fill(usize, u8),
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => Just(Step::Take),
            2 => any::<usize>().prop_map(Step::Return),
            3 => (any::<usize>(), any::<u8>()).prop_map(|(index, byte)| Step::Fill(index, byte)),
        ],
        1..64,
    )
}

proptest! {
    // P1: the shared fixed-seed corpus (OI-1003-Q7); 8 cases under Miri.
    #![proptest_config(crate::test_support::prop_config(if cfg!(miri) { 8 } else { 256 }))]

    /// Model check: the pool never lends more than its capacity, never lends
    /// one slab twice, every slab is aligned and sized, a fresh slab is
    /// zeroed, and writes through one slab never show up in another.
    #[test]
    fn pool_matches_its_model(
        capacity in 1_usize..5,
        align_shift in 3_u32..9,
        len in 1_usize..512,
        steps in steps(),
    ) {
        let align = 1_usize << align_shift;
        let pool = SlabPool::with_geometry(len, align, capacity).unwrap();
        // (slab, byte every element was last filled with, or None if the
        // contents are whatever a previous holder left).
        let mut held: Vec<(Slab, Option<u8>)> = Vec::new();
        let mut ever_allocated = 0_usize;
        for step in steps {
            match step {
                Step::Take => {
                    let before = pool.stats();
                    let slab = pool.try_take().unwrap();
                    if held.len() == capacity {
                        prop_assert!(slab.is_none());
                        continue;
                    }
                    let slab = slab.expect("a slab is free or allocatable");
                    prop_assert_eq!(slab.len(), len);
                    prop_assert_eq!(slab.as_ptr() as usize % align, 0);
                    let fresh = before.free == 0;
                    if fresh {
                        ever_allocated += 1;
                        prop_assert!(slab.iter().all(|byte| *byte == 0));
                    }
                    held.push((slab, fresh.then_some(0)));
                }
                Step::Return(index) => {
                    if !held.is_empty() {
                        let index = index % held.len();
                        drop(held.swap_remove(index));
                    }
                }
                Step::Fill(index, byte) => {
                    if !held.is_empty() {
                        let index = index % held.len();
                        let entry = &mut held[index];
                        entry.0.fill(byte);
                        entry.1 = Some(byte);
                    }
                }
            }
            // No two held slabs overlap.
            let mut ranges: Vec<(usize, usize)> = held
                .iter()
                .map(|(slab, _)| (slab.as_ptr() as usize, slab.as_ptr() as usize + slab.len()))
                .collect();
            ranges.sort_unstable();
            for pair in ranges.windows(2) {
                prop_assert!(pair[0].1 <= pair[1].0, "slabs overlap: {:?}", pair);
            }
            // Every held slab still holds exactly what was last written to it.
            for (slab, expect) in &held {
                if let Some(byte) = expect {
                    prop_assert!(slab.iter().all(|value| value == byte));
                }
            }
            let stats = pool.stats();
            prop_assert_eq!(stats.outstanding, held.len());
            prop_assert_eq!(stats.allocated, ever_allocated);
            prop_assert!(stats.allocated <= capacity);
            prop_assert_eq!(stats.free + stats.outstanding, stats.allocated);
        }
        drop(held);
        let stats = pool.stats();
        prop_assert_eq!(stats.outstanding, 0);
        prop_assert_eq!(stats.free, stats.allocated);
    }
}
