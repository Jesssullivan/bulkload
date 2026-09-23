//! Aligned slab pool (plan D3 `buf.rs`).
//!
//! Reads land in reused 4 MiB slabs aligned to 16 KiB (the Apple Silicon page
//! size), so the steady state allocates nothing and a slab can later feed
//! `pread`, `pwrite` or a direct-I/O path unchanged. Every slab is
//! zero-initialized when it is allocated, so no uninitialized byte is ever
//! exposed through a `&[u8]`. A reused slab holds whatever its previous user
//! left in it; callers track how much of it they filled.
//!
//! [`SlabPool::take`] blocks while every slab is out, which is the engine's
//! backpressure: a reader cannot run ahead of the writers by more than the
//! pool. A [`Slab`] returns itself to the pool when dropped.
//!
//! The only `unsafe` here is the raw allocation (`alloc_zeroed`/`dealloc`),
//! the slice views over it, and the `Send`/`Sync` impls that say a slab is an
//! owned buffer; each carries a `SAFETY` comment.

use std::alloc::{self, Layout};
use std::io;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Default slab size.
pub const SLAB_BYTES: usize = 4 * 1024 * 1024;
/// Default slab alignment: one 16 KiB page on Apple Silicon.
pub const SLAB_ALIGN: usize = 16 * 1024;

/// One aligned, zero-initialized heap block that this value owns outright.
struct Block {
    ptr: NonNull<u8>,
    layout: Layout,
}

// SAFETY: a `Block` is the sole owner of its allocation, like `Box<[u8]>`;
// sending it to another thread moves that ownership with it.
unsafe impl Send for Block {}
// SAFETY: through `&Block` only `as_slice` (shared reads) is reachable; writes
// need `&mut Block`, so sharing a reference cannot race.
unsafe impl Sync for Block {}

impl Block {
    /// `layout.size()` must be non-zero; [`SlabPool::with_geometry`] checks.
    fn new(layout: Layout) -> io::Result<Self> {
        if layout.size() == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        // SAFETY: `layout` has a non-zero size (checked above) and a
        // power-of-two alignment (guaranteed by `Layout`).
        let raw = unsafe { alloc::alloc_zeroed(layout) };
        NonNull::new(raw)
            .map(|ptr| Self { ptr, layout })
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
    }

    const fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `layout.size()` bytes for as long as
        // `self` lives; every byte was initialized by `alloc_zeroed` and is
        // only ever written through `as_mut_slice`; the shared borrow of
        // `self` rules out a live mutable view.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.layout.size()) }
    }

    const fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as in `as_slice`, and the exclusive borrow of `self` makes
        // this the only view of the allocation while it lives.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed` with exactly `self.layout`,
        // and `drop` runs once, so the block is freed exactly once.
        unsafe { alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

struct State {
    free: Vec<Block>,
    allocated: usize,
    waiting: usize,
}

struct Shared {
    state: Mutex<State>,
    returned: Condvar,
    layout: Layout,
    capacity: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock cannot leave `State` inconsistent:
        // every mutation is a single push, pop or counter step.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A fixed-capacity pool of aligned, reusable slabs. Cloning shares the pool.
#[derive(Clone)]
pub struct SlabPool {
    shared: Arc<Shared>,
}

/// Point-in-time pool counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolStats {
    pub capacity: usize,
    pub allocated: usize,
    pub free: usize,
    /// Slabs currently held by callers.
    pub outstanding: usize,
    /// Callers blocked in [`SlabPool::take`].
    pub waiting: usize,
}

/// A slab on loan from a [`SlabPool`]; it goes back when dropped.
pub struct Slab {
    block: Option<Block>,
    shared: Arc<Shared>,
}

impl SlabPool {
    /// A pool of up to `capacity` slabs of [`SLAB_BYTES`], aligned to
    /// [`SLAB_ALIGN`]. Slabs are allocated on first use.
    ///
    /// # Errors
    /// `InvalidInput` for a zero capacity.
    pub fn new(capacity: usize) -> io::Result<Self> {
        Self::with_geometry(SLAB_BYTES, SLAB_ALIGN, capacity)
    }

    /// A pool with a custom slab size and alignment.
    ///
    /// # Errors
    /// `InvalidInput` for a zero size or capacity, or an alignment that is not
    /// a power of two.
    pub fn with_geometry(slab_bytes: usize, align: usize, capacity: usize) -> io::Result<Self> {
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        if slab_bytes == 0 || capacity == 0 {
            return Err(invalid());
        }
        let layout = Layout::from_size_align(slab_bytes, align).map_err(|_| invalid())?;
        Ok(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    free: Vec::with_capacity(capacity),
                    allocated: 0,
                    waiting: 0,
                }),
                returned: Condvar::new(),
                layout,
                capacity,
            }),
        })
    }

    /// Bytes per slab.
    pub fn slab_bytes(&self) -> usize {
        self.shared.layout.size()
    }

    /// Take a slab, blocking until one is returned if all are out.
    ///
    /// # Errors
    /// `OutOfMemory` if a first-use allocation fails.
    pub fn take(&self) -> io::Result<Slab> {
        self.acquire(Wait::Forever).and_then(|slab| {
            slab.ok_or_else(|| io::Error::other("slab pool wait ended without a slab"))
        })
    }

    /// Take a slab if one is free or can still be allocated; never blocks.
    ///
    /// # Errors
    /// `OutOfMemory` if a first-use allocation fails.
    pub fn try_take(&self) -> io::Result<Option<Slab>> {
        self.acquire(Wait::Never)
    }

    /// Take a slab, waiting at most `timeout` for one to be returned.
    ///
    /// # Errors
    /// `OutOfMemory` if a first-use allocation fails.
    pub fn take_timeout(&self, timeout: Duration) -> io::Result<Option<Slab>> {
        self.acquire(Wait::Until(Instant::now() + timeout))
    }

    pub fn stats(&self) -> PoolStats {
        let state = self.shared.lock();
        PoolStats {
            capacity: self.shared.capacity,
            allocated: state.allocated,
            free: state.free.len(),
            outstanding: state.allocated.saturating_sub(state.free.len()),
            waiting: state.waiting,
        }
    }

    fn lend(&self, block: Block) -> Slab {
        Slab {
            block: Some(block),
            shared: Arc::clone(&self.shared),
        }
    }

    fn acquire(&self, wait: Wait) -> io::Result<Option<Slab>> {
        let mut state = self.shared.lock();
        loop {
            if let Some(block) = state.free.pop() {
                return Ok(Some(self.lend(block)));
            }
            if state.allocated < self.shared.capacity {
                state.allocated += 1;
                drop(state);
                return match Block::new(self.shared.layout) {
                    Ok(block) => Ok(Some(self.lend(block))),
                    Err(error) => {
                        let mut state = self.shared.lock();
                        state.allocated -= 1;
                        drop(state);
                        self.shared.returned.notify_one();
                        Err(error)
                    }
                };
            }
            let deadline = match wait {
                Wait::Never => return Ok(None),
                Wait::Forever => None,
                Wait::Until(deadline) => Some(deadline),
            };
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(None);
            }
            state.waiting += 1;
            state = match deadline {
                None => self
                    .shared
                    .returned
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    self.shared
                        .returned
                        .wait_timeout(state, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
            state.waiting -= 1;
            // Loop: take what was returned, or give up once the deadline has
            // passed and nothing is free.
        }
    }
}

#[derive(Clone, Copy)]
enum Wait {
    Never,
    Forever,
    Until(Instant),
}

impl std::ops::Deref for Slab {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.block.as_ref().map_or(&[], Block::as_slice)
    }
}

impl std::ops::DerefMut for Slab {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.block.as_mut().map_or(&mut [], Block::as_mut_slice)
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        if let Some(block) = self.block.take() {
            self.shared.lock().free.push(block);
            self.shared.returned.notify_one();
        }
    }
}

impl std::fmt::Debug for Slab {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Slab")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
