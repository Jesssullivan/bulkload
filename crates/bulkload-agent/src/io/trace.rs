//! Syscall trace for the crash-state checker (R-N88).
//!
//! With the `io-trace` feature on, every mutating call in `io::sys` (write,
//! sync, rename, link, unlink, mkdir, symlink, fchmod, create) appends one
//! [`Event`] to the recorder attached to the calling thread, or to the
//! process-wide recorder when none is. The stores add one [`Event::Commit`]
//! per `SQLite` commit, naming what the commit made durable. The checker
//! (`io::crash_check`) replays a trace into every crash state its persistence
//! model allows. With `io-trace` both modules are public, so the fault
//! harness can check a real `copy`; a default build exports neither. The
//! event types also compile under `cfg(test)` without the feature, so the
//! checker can be proven on hand-written traces.
//!
//! An event names files and directories by inode identity ([`NodeId`], from
//! `fstat` after the call) and entries by `(directory node, name bytes)`, so a
//! trace is independent of the descriptors and paths that produced it. A
//! write keeps its bytes and their BLAKE3 digest, since materializing a crash
//! state needs the bytes. That costs memory equal to the bytes written, which
//! is why the recorder exists for tests and diagnostics only.

use super::NodeId;

/// The persistence effect of a sync call, as the checker models it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncKind {
    /// Sends the object's pending operations to the device. Neither durable
    /// nor ordered: Darwin plain `fsync`, Linux `sync_file_range(WRITE)`.
    Kick,
    /// Darwin `F_BARRIERFSYNC`: sends the object's pending operations to the
    /// device and orders them before operations issued after it. Not durable.
    Barrier,
    /// Linux `fdatasync`: the object's writes are durable (its mode is not).
    DataSync,
    /// Linux `fsync`: every pending operation on the object is durable; for a
    /// directory, its entry operations.
    Fsync,
    /// Darwin `F_FULLFSYNC`: every pending operation on the object is
    /// durable, and the device cache is drained, so every operation already
    /// sent to the device by an earlier sync of any object is durable too.
    FullFlush,
    /// Linux `syncfs`: every pending operation on every object of the file
    /// system holding the object is durable, data, metadata and entries, and
    /// the device cache is flushed (S1, OI-1003-Q107).
    FsSync,
}

/// One mutating syscall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A new file. `name` is `None` for an `O_TMPFILE` inode.
    Create {
        dir: Option<NodeId>,
        name: Option<Vec<u8>>,
        node: NodeId,
        mode: u32,
    },
    /// A new directory `name` in `dir`.
    Mkdir {
        dir: NodeId,
        name: Vec<u8>,
        node: NodeId,
        mode: u32,
    },
    /// `data` written at `offset`; `digest` is its BLAKE3.
    Write {
        node: NodeId,
        offset: u64,
        data: Vec<u8>,
        digest: [u8; 32],
    },
    /// Permission bits set with `fchmod`.
    SetMode { node: NodeId, mode: u32 },
    /// The capture record (#169) set with `fsetxattr`: metadata, like a mode.
    SetCaptureRecord { node: NodeId, record: Vec<u8> },
    /// A sync call on `node`.
    Sync { node: NodeId, kind: SyncKind },
    /// A new symbolic link `name` in `dir` holding `target` literally.
    Symlink {
        dir: NodeId,
        name: Vec<u8>,
        node: NodeId,
        target: Vec<u8>,
    },
    /// A `SQLite` commit of the store rooted at `store` returned: every record
    /// in `records` is durable from here on (`synchronous=FULL`). On Darwin
    /// the commit's `F_FULLFSYNC` also drains the drive holding the store.
    Commit {
        store: NodeId,
        records: Vec<CommitRecord>,
    },
    /// A new name for an existing inode.
    Link {
        node: NodeId,
        dir: NodeId,
        name: Vec<u8>,
    },
    /// `node` moved from `(from_dir, from)` to `(to_dir, to)`.
    Rename {
        node: NodeId,
        from_dir: NodeId,
        from: Vec<u8>,
        to_dir: NodeId,
        to: Vec<u8>,
    },
    /// Entry `name` removed from `dir`.
    Unlink { dir: NodeId, name: Vec<u8> },
    /// The call succeeded but its event could not be built (the identity
    /// `fstat` failed). The checker refuses a trace that holds one.
    Untraced { call: &'static str, error: String },
}

/// What one store commit recorded, as the checker's invariants read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitRecord {
    /// A destination output: `rel_path` holds the source's bytes.
    Output { rel_path: Vec<u8> },
    /// A source capture (manifest) under the store's row key.
    Capture { key: Vec<u8> },
    /// A directory record bound to inode `node` with final `mode`, or, with
    /// `node` `None`, an intent recorded before a fallback `mkdirat`.
    DirectoryCreated {
        key: Vec<u8>,
        node: Option<NodeId>,
        mode: u32,
    },
    /// A directory's final mode is durable and its record retired.
    DirectoryComplete { key: Vec<u8> },
    /// The store's `root_sealed` setting (#161): `Store::open` sealed the
    /// state root's entry and its database's entry before this commit, and
    /// every later open trusts the marker without sealing again.
    RootSealed,
}

#[cfg(feature = "io-trace")]
pub use recorder::{record, serialize};

#[cfg(feature = "io-trace")]
pub mod recorder {
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Condvar, Mutex, PoisonError};

    use super::Event;

    #[derive(Debug, Default)]
    struct Shared {
        events: Mutex<Vec<Event>>,
        /// True while some thread is inside a traced call.
        busy: Mutex<bool>,
        idle: Condvar,
    }

    /// The process-wide recorder, used by threads with none attached.
    static PROCESS: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

    /// The recorder for this thread: its own, else the process-wide one.
    fn current() -> Option<Arc<Shared>> {
        CURRENT
            .with(|current| current.borrow().clone())
            .or_else(|| {
                PROCESS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
    }

    thread_local! {
        static CURRENT: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
        /// Nesting depth of traced calls on this thread (`barrier_dir` calls
        /// `barrier`; the rename fallback calls `linkat` and `unlinkat`).
        static DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// A shared, ordered event log.
    ///
    /// Attach it to every thread whose calls belong to the trace (a
    /// committer thread included), or process-wide. While attached, each
    /// traced call holds the recorder's serial lock from before its syscall
    /// until its event is appended, so the trace order is the order in which
    /// the syscalls took effect, across threads.
    #[derive(Clone, Debug, Default)]
    pub struct Recorder {
        shared: Arc<Shared>,
    }

    /// Detaches the recorder from the thread on drop, restoring whatever was
    /// attached before.
    #[must_use = "the recorder detaches when this guard drops"]
    pub struct Attached {
        previous: Option<Arc<Shared>>,
    }

    impl Drop for Attached {
        fn drop(&mut self) {
            let previous = self.previous.take();
            CURRENT.with(|current| *current.borrow_mut() = previous);
        }
    }

    impl Recorder {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Record this thread's calls into `self` until the guard drops.
        pub fn attach(&self) -> Attached {
            let previous =
                CURRENT.with(|current| current.borrow_mut().replace(Arc::clone(&self.shared)));
            Attached { previous }
        }

        /// Record every thread's calls that has no recorder of its own,
        /// including threads the engine spawns, until the guard drops.
        pub fn attach_process(&self) -> ProcessAttached {
            let previous = PROCESS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .replace(Arc::clone(&self.shared));
            ProcessAttached { previous }
        }

        /// Remove and return every event recorded so far.
        pub fn take(&self) -> Vec<Event> {
            std::mem::take(
                &mut *self
                    .shared
                    .events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            )
        }
    }

    /// Detaches the process-wide recorder on drop, restoring the previous one.
    #[must_use = "the process-wide recorder detaches when this guard drops"]
    pub struct ProcessAttached {
        previous: Option<Arc<Shared>>,
    }

    impl Drop for ProcessAttached {
        fn drop(&mut self) {
            let previous = self.previous.take();
            *PROCESS.lock().unwrap_or_else(PoisonError::into_inner) = previous;
        }
    }

    /// Holds the serial lock of the attached recorder; see [`serialize`].
    #[must_use = "the serial lock is released when this guard drops"]
    pub struct Serial {
        held: Option<Arc<Shared>>,
    }

    /// Enter a traced call.
    ///
    /// With a recorder attached, the outermost call on a thread waits for the
    /// recorder's serial lock and holds it until the guard drops; nested
    /// calls on the same thread pass through. With no recorder attached this
    /// is one thread-local read and one lock of the process-wide slot.
    pub fn serialize() -> Serial {
        let Some(shared) = current() else {
            return Serial { held: None };
        };
        let depth = DEPTH.with(Cell::get);
        if depth == 0 {
            let mut busy = shared.busy.lock().unwrap_or_else(PoisonError::into_inner);
            while *busy {
                busy = shared
                    .idle
                    .wait(busy)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            *busy = true;
        }
        DEPTH.with(|cell| cell.set(depth + 1));
        Serial { held: Some(shared) }
    }

    impl Drop for Serial {
        fn drop(&mut self) {
            let Some(shared) = self.held.take() else {
                return;
            };
            let depth = DEPTH.with(Cell::get).saturating_sub(1);
            DEPTH.with(|cell| cell.set(depth));
            if depth == 0 {
                *shared.busy.lock().unwrap_or_else(PoisonError::into_inner) = false;
                shared.idle.notify_one();
            }
        }
    }

    /// Append the event `make` builds, if a recorder is attached to this
    /// thread. `make` runs only then, so an unattached thread pays one
    /// thread-local read per call.
    pub fn record(call: &'static str, make: impl FnOnce() -> std::io::Result<Event>) {
        let Some(shared) = current() else {
            return;
        };
        let event = make().unwrap_or_else(|error| Event::Untraced {
            call,
            error: error.to_string(),
        });
        shared
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}
