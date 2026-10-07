//! P77 SOURCE-LOCK-TRACE (S2: never interrupt the source; OI-1003-Q5,
//! OI-1003-Q9; the bounded `SQLite` exception OI-1003-Q16, OI-1003-Q36,
//! OI-1003-Q72; no `SQLite` source read as root, OI-1003-Q76). A P34 sibling.
//!
//! The `io-trace` recorder (R-N88) records only the agent's own mutating
//! `io::sys` calls. It sees no `flock`, no `fcntl` lock, no open of any mode,
//! nothing a Git child does, and nothing `SQLite` does through its own VFS. So
//! this file does not extend it. It observes the kernel instead, with no
//! preload and no new crate, through four channels:
//!
//! - **The lock table.** `/proc/locks`, sampled in a tight loop while a verb
//!   runs, lists every `flock`, POSIX `fcntl` and OFD lock any process holds
//!   or waits for, by device, inode and byte range. It does **not** list
//!   Git's locks: Git locks by creating `*.lock` files.
//! - **This process's descriptors.** `/proc/self/fdinfo`, sampled in the same
//!   loop, gives the access mode of every descriptor the in-process verb has
//!   open. It does not see a child's.
//! - **A write watch.** An inotify watch on every source directory reports
//!   every create, delete, rename, write, re-stamp and close of a write-mode
//!   descriptor, by any process, for the whole verb (it is a queue, not a
//!   sample). This is the channel that sees a Git child's `index.lock`, a
//!   freshened object, and a child's write-mode open.
//! - **A holder.** Sampling can miss a lock taken and dropped between two
//!   samples. So the holder runs first take an exclusive `flock` and an
//!   exclusive whole-file OFD write lock on every source file (and an
//!   exclusive `flock` on every source directory), the way a live agent's
//!   writer would. Any `flock`/`fcntl` lock the verb then tries conflicts: a
//!   blocking attempt waits (a `->` line, and a verb that does not finish),
//!   and a non-blocking one fails (a refusal). The copy legs also make the
//!   tree read-only, so a write-mode open fails with `EACCES` (when not
//!   root).
//!
//! An **`lstat` census** of the whole source (every field but access time) is
//! compared before and after each verb.
//!
//! The legs: an in-process `copy` (free and held), the `serve` verb as its
//! own process (held), a v1 `export_repository` (held), an `estate::capture`
//! of two repositories (held), a local `git-carry-estimate` of two
//! repositories (held), and the `SQLite` backup against a writer process,
//! idle and committing, and against a closed WAL-mode database with no
//! sidecar. Self-tests prove the lock table, the
//! descriptor sample and the write watch each see what they claim on this
//! kernel, and that the Git fixture's index is one an unhardened `git status`
//! does rewrite.
//!
//! **The `SQLite` legs run one of two ways, and say which on stderr**
//! ([`sqlite_leg`]). As an ordinary user they run the backup and hold it to
//! the counted exception: the `-shm` (OI-1003-Q36,
//! `source_wal_index_touched`) and an empty `-wal` only where none was
//! (OI-1003-Q72, `source_wal_created`). As root (PR CI) the provider refuses
//! to read a source at all (OI-1003-Q76): the bundled `SQLite` re-applies the
//! database's owner to the `-wal` and `-shm` it opens when it runs as root,
//! which moves their ctime, and this file's write watch reported exactly
//! that as an `attrib` event on the `-wal` in CI. So as root each leg asserts
//! the typed refusal `SQLITE_SOURCE_AS_ROOT` from all five provider verbs
//! and that the refused calls left the source directory byte- and
//! metadata-identical, with no write event and no lock
//! ([`sqlite_source_read_is_refused_as_root`]). Neither run proves the
//! other's half; no privilege is dropped or gained here.
//!
//! **Not covered here**, and so still open for S2's lock property:
//! `estate::apply`, `git-carry-estimate` over ssh (the far host's probe),
//! `export_repository_with_policy` and the prerequisite and drift paths,
//! `hydrate-state`, a real `pull` over ssh, and every verb on Darwin (no
//! `/proc`, no inotify).
//!
//! What this cannot see: a non-blocking `flock`/`fcntl` attempt whose failure
//! the verb ignores, made under the holder, and missed by sampling in the
//! free run. A read-mode open by a child, and a lock on a file outside the
//! watched trees.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::provider_sqlite::{self, PathMapping};
use bulkload_agent::transfer::{copy, receive, settle_racy_window};
use bulkload_agent::BulkloadRefusal;

/// One test at a time: the samplers read process-wide tables.
static SERIAL: Mutex<()> = Mutex::new(());

/// How long a verb may run before it counts as blocked on a holder.
const DEADLINE: Duration = Duration::from_mins(2);

/// A named residual, not a ruling (found by this file, 2026-10-06,
/// bulkload#157): the backup API's read-only connection holds a source
/// `-wal` **that already existed** open read-write (`O_RDWR`).
///
/// Against the rulings as written: OI-1003-Q16 grants a bounded shared read
/// lock; OI-1003-Q36 grants the `-shm`; OI-1003-Q72 grants an empty `-wal`
/// "only where no `-wal` existed" and says "a `-wal` that existed before the
/// read stays byte-identical". None of them names a write-mode descriptor on
/// an existing `-wal`, and neither counter sees one. Q72's condition for that
/// file is on its bytes, and it holds (asserted, with its `lstat` fields).
/// So the open is neither granted nor a breach of the stated condition: it
/// is the one thing these legs see and no ruling covers.
///
/// What this constant tolerates, exactly, for an existing `-wal`: this
/// process's write-mode descriptor on it, and the one `close-write` event of
/// that descriptor. It tolerates no `modify`, no `attrib` (the root finding,
/// now refused by OI-1003-Q76), no create, delete or rename, and no lock.
/// Each run prints the count as `wal-write-open=N (#157, residual)`. Set it
/// to `false` once the provider opens an existing `-wal` read-only or a
/// ruling refuses the open; the legs then fail on any write-mode open of it.
const RESIDUAL_WAL_OPEN_READ_WRITE: bool = true;

/// The environment variable that turns [`sqlite_writer_helper`] into the
/// writer process.
const WRITER_ENV: &str = "BULKLOAD_P77_WRITER_DB";

/// With [`WRITER_ENV`]: the writer keeps committing until its stdin closes.
const WRITER_LIVE_ENV: &str = "BULKLOAD_P77_WRITER_LIVE";

/// The longest this file's snapshot may be seen holding the database's
/// shared READ lock ("bounded in duration", OI-1003-Q16). The lock lives as
/// long as the backup's connection, so this bounds the whole backup of the
/// fixture (about 2 MiB) on a loaded host. It is this test's bound, not the
/// product's: the product bound is bulkload#157's.
const SQLITE_LOCK_BOUND: Duration = Duration::from_secs(30);

/// The slowest commit the live writer may report while snapshots run.
const COMMIT_BOUND: Duration = Duration::from_secs(10);

/// How many snapshots the idle-writer leg takes before giving up on one the
/// sampler sees holding its locks. Every one of them is held to every limit;
/// only "the exception was seen" may take more than one.
const OBSERVE_ATTEMPTS: usize = 40;

/// How many snapshots the live leg tries before giving up on one that both
/// succeeds and overlaps a commit.
const LIVE_ATTEMPTS: usize = 20;

// ------------------------------------------------------------- the fixture

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "bulkload-p77-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        if root.exists() {
            make_writable(&root);
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        make_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Give every directory under `root` back its owner write bit, so a
/// read-only fixture can be removed.
fn make_writable(root: &Path) {
    let Ok(meta) = fs::symlink_metadata(root) else {
        return;
    };
    if meta.is_dir() {
        let _ = fs::set_permissions(
            root,
            fs::Permissions::from_mode(meta.mode() & 0o7777 | 0o700),
        );
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

/// Deterministic incompressible bytes.
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

/// A source tree: nested directories, small and multi-MiB files, an empty
/// file and a symlink.
fn populate(source: &Path) {
    fs::create_dir_all(source.join("nested/deeper")).unwrap();
    fs::create_dir_all(source.join("wide")).unwrap();
    let sizes = [
        ("a", 3 * 1024 * 1024),
        ("b", 17),
        ("empty", 0),
        ("nested/c", 1024 * 1024 + 3),
        ("nested/deeper/d", 64 * 1024),
        ("nested/deeper/e", 2 * 1024 * 1024),
    ];
    for (index, (path, length)) in sizes.iter().enumerate() {
        fs::write(source.join(path), noise(index as u64 + 1, *length)).unwrap();
    }
    for index in 0..24_u64 {
        fs::write(
            source.join(format!("wide/f{index:02}")),
            noise(100 + index, 4096),
        )
        .unwrap();
    }
    std::os::unix::fs::symlink("a", source.join("link")).unwrap();
}

/// Every node under `root` (itself included), relative path to identity.
fn nodes(root: &Path) -> BTreeMap<PathBuf, fs::Metadata> {
    fn visit(root: &Path, relative: &Path, out: &mut BTreeMap<PathBuf, fs::Metadata>) {
        let meta = fs::symlink_metadata(root.join(relative)).unwrap();
        let is_dir = meta.is_dir();
        out.insert(relative.to_path_buf(), meta);
        if is_dir {
            let mut names: Vec<_> = fs::read_dir(root.join(relative))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            for name in names {
                visit(root, &relative.join(name), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, Path::new(""), &mut out);
    out
}

type Census = BTreeMap<PathBuf, [i64; 9]>;

/// Every `lstat` field a write, create, rename, chmod, link or lock-file
/// would move. Access time is left out: reading the source is the point.
fn lstat_census(root: &Path) -> Census {
    nodes(root)
        .into_iter()
        .map(|(path, meta)| {
            let fields = [
                i64::from(meta.mode()),
                i64::try_from(meta.size()).unwrap(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
                i64::try_from(meta.ino()).unwrap(),
                i64::try_from(meta.nlink()).unwrap(),
                i64::from(meta.uid()),
            ];
            (path, fields)
        })
        .collect()
}

/// A device and inode as `/proc/locks` prints them: `MAJ:MIN:INODE`, the
/// major and minor in hex.
fn lock_key(meta: &fs::Metadata) -> String {
    let dev = meta.dev();
    // glibc's `gnu_dev_major`/`gnu_dev_minor` encoding.
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    format!("{major:02x}:{minor:02x}:{}", meta.ino())
}

/// The source's inodes as lock keys, each to its relative path.
fn lock_keys(root: &Path) -> BTreeMap<String, PathBuf> {
    nodes(root)
        .into_iter()
        .filter(|(_, meta)| !meta.file_type().is_symlink())
        .map(|(path, meta)| (lock_key(&meta), path))
        .collect()
}

// -------------------------------------------------------------- the holder

/// Exclusive locks on every source node, held until drop: an `flock` on
/// every file and directory, and a whole-file OFD write lock on every file.
struct Holder {
    files: Vec<File>,
    keys: BTreeSet<String>,
}

impl Holder {
    fn take(root: &Path, skip: &BTreeSet<PathBuf>) -> Self {
        let mut files = Vec::new();
        let mut keys = BTreeSet::new();
        for (relative, meta) in nodes(root) {
            if skip.contains(&relative) || meta.file_type().is_symlink() {
                continue;
            }
            let path = root.join(&relative);
            let file = if meta.is_dir() {
                File::open(&path).unwrap()
            } else {
                // An OFD write lock needs a descriptor open for writing. The
                // fixture is the test's own; a read-only file gets its owner
                // write bit back for the open only.
                let mode = meta.mode() & 0o7777;
                if mode & 0o200 == 0 {
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o200)).unwrap();
                }
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                if mode & 0o200 == 0 {
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
                }
                ofd_write_lock(&file);
                file
            };
            // SAFETY: the descriptor is live for the call; `flock` takes no
            // pointers.
            let held = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            assert_eq!(held, 0, "flock {}", path.display());
            files.push(file);
            keys.insert(lock_key(&meta));
        }
        Self { files, keys }
    }

    fn descriptors(&self) -> BTreeSet<RawFd> {
        self.files.iter().map(AsRawFd::as_raw_fd).collect()
    }

    /// What the sampler needs to tell the holder's own lines and
    /// descriptors from the verb's.
    fn shape(&self) -> Held {
        Held {
            keys: self.keys.clone(),
            descriptors: self.descriptors(),
        }
    }
}

/// A whole-file exclusive OFD lock on `file`, without blocking.
fn ofd_write_lock(file: &File) {
    // SAFETY: an all-zero `flock` is a valid value of the plain C struct.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::c_short::try_from(libc::F_WRLCK).unwrap();
    lock.l_whence = libc::c_short::try_from(libc::SEEK_SET).unwrap();
    // SAFETY: the descriptor is live and `lock` outlives the call; OFD locks
    // require `l_pid` 0, which `zeroed` set.
    let held = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &raw mut lock) };
    assert_eq!(held, 0, "F_OFD_SETLK: {}", std::io::Error::last_os_error());
}

/// Clear every write bit under `root` (directories too), so a write-mode
/// open or a create fails with `EACCES` for a non-root caller.
fn make_read_only(root: &Path) {
    for (relative, meta) in nodes(root).into_iter().rev() {
        if !meta.file_type().is_symlink() {
            let mode = meta.mode() & 0o7777 & !0o222;
            fs::set_permissions(root.join(relative), fs::Permissions::from_mode(mode)).unwrap();
        }
    }
}

// ------------------------------------------------------------- the sampler

/// One lock line from `/proc/locks`, without its ordinal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LockLine {
    blocked: bool,
    kind: String,
    mode: String,
    pid: String,
    key: String,
    /// The first locked byte, and the last (`None` for end of file).
    start: u64,
    end: Option<u64>,
}

fn read_locks() -> Vec<LockLine> {
    let text = fs::read_to_string("/proc/locks").unwrap();
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace().skip(1).peekable();
            let blocked = words.peek() == Some(&"->");
            if blocked {
                words.next();
            }
            let kind = words.next()?.to_owned();
            let _advisory = words.next()?;
            let mode = words.next()?.to_owned();
            let pid = words.next()?.to_owned();
            let key = words.next()?.to_owned();
            let start = words.next()?.parse().ok()?;
            let end = words.next()?.parse().ok();
            Some(LockLine {
                blocked,
                kind,
                mode,
                pid,
                key,
                start,
                end,
            })
        })
        .collect()
}

/// A descriptor of this process on a watched path, open for writing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WriteOpen {
    path: PathBuf,
    flags: u32,
}

/// Every descriptor of this process open for writing on a path beneath one
/// of `roots`, except `ignore`'s.
fn write_opens(roots: &[PathBuf], ignore: &BTreeSet<RawFd>) -> Vec<WriteOpen> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc/self/fd") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<RawFd>().ok())
        else {
            continue;
        };
        if ignore.contains(&fd) {
            continue;
        }
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        if !roots.iter().any(|root| target.starts_with(root)) {
            continue;
        }
        let Ok(info) = fs::read_to_string(format!("/proc/self/fdinfo/{fd}")) else {
            continue;
        };
        let flags = info
            .lines()
            .find_map(|line| line.strip_prefix("flags:"))
            .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
            .unwrap_or(0);
        if flags & 0o3 != 0 {
            out.push(WriteOpen {
                path: target,
                flags,
            });
        }
    }
    out
}

#[derive(Debug, Default)]
struct Observed {
    samples: u64,
    /// Lock lines on watched inodes that are not the holder's own, by line,
    /// with the number of samples that saw each.
    locks: BTreeMap<LockLine, u64>,
    /// Write-mode descriptors on watched paths, with the number of samples
    /// that saw each.
    opens: BTreeMap<WriteOpen, u64>,
    /// For each lock line, the longest run of consecutive samples that saw
    /// it, as the time from the run's first sample to its last. The lock was
    /// held at least that long, and at most two sample periods longer.
    longest: BTreeMap<LockLine, Duration>,
    /// How long the sampler ran.
    elapsed: Duration,
}

impl Observed {
    /// The mean time between samples.
    fn period(&self) -> Duration {
        self.elapsed / u32::try_from(self.samples.max(1)).unwrap_or(u32::MAX)
    }

    /// The mean time between samples, in microseconds.
    fn period_us(&self) -> u128 {
        self.period().as_micros()
    }
}

/// A holder's keys and descriptors, as the sampler excludes them.
#[derive(Debug, Default)]
struct Held {
    keys: BTreeSet<String>,
    descriptors: BTreeSet<RawFd>,
}

impl Held {
    /// Whether `line` is one of the holder's own locks: a granted `flock`
    /// write lock of this process or a granted OFD write lock on a held
    /// inode. While the holder holds them nobody else can be granted a
    /// lock of either shape on that inode, so excluding the shape (rather
    /// than a one-time baseline read) hides nothing the verb could take.
    /// `/proc/locks` is read one page per `read`, so a single baseline read
    /// can tear on a busy host; a shape cannot.
    fn owns(&self, line: &LockLine, ours: &str) -> bool {
        self.keys.contains(&line.key)
            && !line.blocked
            && line.mode == "WRITE"
            && ((line.kind == "FLOCK" && line.pid == ours)
                || (line.kind == "OFDLCK" && line.pid == "-1"))
    }
}

/// A thread sampling `/proc/locks` and this process's descriptors until
/// stopped.
struct Sampler {
    stop: Arc<AtomicBool>,
    taken: Arc<AtomicU64>,
    /// Samples so far that saw a granted lock of this process, the
    /// holder's own apart.
    own: Arc<AtomicU64>,
    thread: thread::JoinHandle<Observed>,
}

impl Sampler {
    /// Watch `keys` (lock keys) and paths beneath `roots`, not reporting
    /// `held`'s own locks and descriptors.
    fn start(roots: Vec<PathBuf>, keys: BTreeSet<String>, held: Held) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let taken = Arc::new(AtomicU64::new(0));
        let counting = Arc::clone(&taken);
        let own = Arc::new(AtomicU64::new(0));
        let owning = Arc::clone(&own);
        let ours = std::process::id().to_string();
        let thread = thread::spawn(move || {
            let mut observed = Observed::default();
            let started = std::time::Instant::now();
            let mut runs: BTreeMap<LockLine, std::time::Instant> = BTreeMap::new();
            loop {
                let last = stopping.load(Ordering::Acquire);
                let now = std::time::Instant::now();
                let seen: BTreeSet<LockLine> = read_locks()
                    .into_iter()
                    .filter(|line| keys.contains(&line.key) && !held.owns(line, &ours))
                    .collect();
                if seen.iter().any(|line| line.pid == ours && !line.blocked) {
                    owning.fetch_add(1, Ordering::Release);
                }
                runs.retain(|line, _| seen.contains(line));
                for line in seen {
                    let since = *runs.entry(line.clone()).or_insert(now);
                    let longest = observed.longest.entry(line.clone()).or_default();
                    *longest = (*longest).max(now.duration_since(since));
                    *observed.locks.entry(line).or_default() += 1;
                }
                for open in write_opens(&roots, &held.descriptors) {
                    *observed.opens.entry(open).or_default() += 1;
                }
                observed.samples += 1;
                counting.store(observed.samples, Ordering::Release);
                if last {
                    observed.elapsed = started.elapsed();
                    return observed;
                }
                thread::yield_now();
            }
        });
        Self {
            stop,
            taken,
            own,
            thread,
        }
    }

    /// Block until `count` more complete samples have been taken.
    fn wait_samples(&self, count: u64) {
        let target = self.taken.load(Ordering::Acquire) + count + 1;
        while self.taken.load(Ordering::Acquire) < target {
            thread::yield_now();
        }
    }

    /// How many samples so far saw a granted lock of this process.
    fn own_lock_samples(&self) -> u64 {
        self.own.load(Ordering::Acquire)
    }

    fn finish(self) -> Observed {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap()
    }
}

// --------------------------------------------------------------- the watch

/// Every way a file or directory can be written, created, removed, renamed
/// or re-stamped: the inotify events a reader never causes. A read
/// (`IN_ACCESS`, `IN_OPEN`, `IN_CLOSE_NOWRITE`) is not watched.
/// `IN_CLOSE_WRITE` fires when a descriptor that was open for writing is
/// closed, whether or not it wrote: a write-mode open by any process.
const WRITE_EVENTS: u32 = libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_MOVE_SELF;

/// One write event: the path it names and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WriteEvent {
    path: PathBuf,
    what: String,
}

/// An inotify watch on every directory of some trees, from `start` until it
/// is drained. It sees every process, so it covers what `/proc/locks` cannot:
/// Git takes its locks by creating `*.lock` files, which are never in the
/// kernel's lock table, and a child's descriptors are not in this process's
/// `fdinfo`.
struct Watch {
    fd: OwnedFd,
    directories: BTreeMap<i32, PathBuf>,
}

impl Watch {
    fn start(roots: &[&Path]) -> Self {
        // SAFETY: `inotify_init1` takes flags only and returns a new
        // descriptor or -1.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(
            raw >= 0,
            "inotify_init1: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `raw` is a descriptor this call just created and nothing
        // else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut directories = BTreeMap::new();
        for root in roots {
            for (relative, meta) in nodes(root) {
                if !meta.is_dir() {
                    continue;
                }
                let path = root.join(relative);
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: `fd` is a live inotify descriptor and `name` is a
                // NUL-terminated path that outlives the call.
                let watch =
                    unsafe { libc::inotify_add_watch(fd.as_raw_fd(), name.as_ptr(), WRITE_EVENTS) };
                assert!(
                    watch >= 0,
                    "inotify_add_watch {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                );
                directories.insert(watch, path);
            }
        }
        Self { fd, directories }
    }

    /// Every write event since the watch started (or was last drained).
    fn drain(&self) -> Vec<WriteEvent> {
        const HEADER: usize = 16;
        let mut events = Vec::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            // SAFETY: `buffer` is writable for its whole length and outlives
            // the call; `fd` is a live descriptor.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            let Ok(read) = usize::try_from(read) else {
                // `EAGAIN`: the queue is empty.
                break;
            };
            if read == 0 {
                break;
            }
            let mut at = 0;
            while at + HEADER <= read {
                let field = |offset: usize| {
                    let bytes: [u8; 4] = buffer[at + offset..at + offset + 4].try_into().unwrap();
                    bytes
                };
                let watch = i32::from_ne_bytes(field(0));
                let mask = u32::from_ne_bytes(field(4));
                let length = usize::try_from(u32::from_ne_bytes(field(12))).unwrap();
                let name = &buffer[at + HEADER..at + HEADER + length];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                at += HEADER + length;
                if mask & libc::IN_IGNORED != 0 {
                    continue;
                }
                let mut path = self
                    .directories
                    .get(&watch)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from("<queue overflow>"));
                if !name.is_empty() {
                    path.push(std::ffi::OsStr::from_bytes(name));
                }
                events.push(WriteEvent {
                    path,
                    what: describe(mask),
                });
            }
        }
        events.sort();
        events.dedup();
        events
    }
}

fn describe(mask: u32) -> String {
    let names: Vec<&str> = [
        (libc::IN_MODIFY, "modify"),
        (libc::IN_ATTRIB, "attrib"),
        (libc::IN_CLOSE_WRITE, "close-write"),
        (libc::IN_CREATE, "create"),
        (libc::IN_DELETE, "delete"),
        (libc::IN_DELETE_SELF, "delete-self"),
        (libc::IN_MOVED_FROM, "moved-from"),
        (libc::IN_MOVED_TO, "moved-to"),
        (libc::IN_MOVE_SELF, "move-self"),
        (libc::IN_Q_OVERFLOW, "queue-overflow"),
    ]
    .into_iter()
    .filter(|(bit, _)| mask & bit != 0)
    .map(|(_, name)| name)
    .collect();
    names.join("+")
}

/// Run `verb` on its own thread. If it has not returned within
/// [`DEADLINE`], drop `holders` (so a waiting lock is granted and the verb
/// can finish) and report it blocked.
fn within_deadline<T: Send + 'static>(
    holders: Vec<Holder>,
    verb: impl FnOnce() -> T + Send + 'static,
) -> (T, bool) {
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = verb();
        let _ = done.send(());
        result
    });
    let blocked = finished.recv_timeout(DEADLINE).is_err();
    drop(holders);
    (worker.join().unwrap(), blocked)
}

/// Every source file arrived with its bytes.
fn assert_carried(source: &Path, destination: &Path) {
    let mut files = 0;
    for (relative, meta) in nodes(source) {
        if meta.is_file() {
            files += 1;
            assert_eq!(
                fs::read(destination.join(&relative)).unwrap(),
                fs::read(source.join(&relative)).unwrap(),
                "{}",
                relative.display()
            );
        }
    }
    assert_eq!(files, 30);
}

fn is_root() -> bool {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

// --------------------------------------------------------------- the tests

/// The observation channels work on this kernel: a lock and a write-mode
/// open held across one sample are seen, and a holder's own lines and
/// descriptors are not reported.
#[test]
fn the_sampler_sees_a_brief_lock_and_a_write_open() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("sampler");
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    populate(&source);
    let keys: BTreeSet<String> = lock_keys(&source).into_keys().collect();
    let holder = Holder::take(&source, &BTreeSet::from([PathBuf::from("b")]));
    let sampler = Sampler::start(vec![source.clone()], keys, holder.shape());
    sampler.wait_samples(2);
    {
        let brief = File::open(source.join("b")).unwrap();
        // SAFETY: the descriptor is live for the call; `flock` takes no
        // pointers.
        assert_eq!(unsafe { libc::flock(brief.as_raw_fd(), libc::LOCK_SH) }, 0);
        sampler.wait_samples(1);
    }
    {
        let _brief = OpenOptions::new()
            .append(true)
            .open(source.join("b"))
            .unwrap();
        sampler.wait_samples(1);
    }
    sampler.wait_samples(2);
    let observed = sampler.finish();
    drop(holder);
    assert!(
        observed
            .locks
            .keys()
            .any(|line| line.kind == "FLOCK" && line.mode == "READ"),
        "a shared flock held across one sample was not seen: {observed:?}"
    );
    assert_eq!(
        observed.locks.len(),
        1,
        "only the probe's lock, none of the holder's: {observed:?}"
    );
    assert!(
        observed
            .opens
            .keys()
            .any(|open| open.path == source.join("b")),
        "a write-mode open held across one sample was not seen: {observed:?}"
    );
    assert_eq!(observed.opens.len(), 1, "{observed:?}");
    eprintln!(
        "p77 sampler: {} samples, mean period {} us",
        observed.samples,
        observed.period_us()
    );
}

/// The write watch works on this kernel: a lock file created and removed, a
/// write, a write-mode open that writes nothing, a re-stamp and a rename are
/// each seen, in a nested directory too, and reading every file is not.
#[test]
fn the_watch_sees_every_kind_of_write_and_no_read() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("watch");
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    populate(&source);
    let watch = Watch::start(&[&source]);
    for (relative, meta) in nodes(&source) {
        if meta.is_file() {
            fs::read(source.join(relative)).unwrap();
        }
    }
    let listed = fs::read_dir(source.join("wide")).unwrap().count();
    assert_eq!(listed, 24);
    assert_eq!(watch.drain(), [], "reading the tree is not a write");

    // Git's lock: an exclusive create, then a rename over the target.
    let lock = source.join("nested/deeper/index.lock");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .unwrap();
    fs::rename(&lock, source.join("nested/deeper/index")).unwrap();
    let seen = watch.drain();
    let saw = |events: &[WriteEvent], path: &Path, what: &str| {
        events
            .iter()
            .any(|event| event.path == path && event.what.contains(what))
    };
    assert!(saw(&seen, &lock, "create"), "{seen:?}");
    assert!(saw(&seen, &lock, "moved-from"), "{seen:?}");
    assert!(
        saw(&seen, &source.join("nested/deeper/index"), "moved-to"),
        "{seen:?}"
    );

    // A write-mode open that writes nothing.
    drop(
        OpenOptions::new()
            .append(true)
            .open(source.join("b"))
            .unwrap(),
    );
    let seen = watch.drain();
    assert_eq!(
        seen,
        [WriteEvent {
            path: source.join("b"),
            what: "close-write".to_owned()
        }]
    );

    // A re-stamp (what Git's object freshening does: the kernel reports a
    // modification-time-only change as `modify` and a change of both times
    // as `attrib`), a mode change, a write and a removal.
    let stamped = File::open(source.join("a")).unwrap();
    stamped
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    stamped
        .set_times(
            fs::FileTimes::new()
                .set_accessed(std::time::SystemTime::UNIX_EPOCH)
                .set_modified(std::time::SystemTime::UNIX_EPOCH),
        )
        .unwrap();
    fs::set_permissions(source.join("empty"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(source.join("wide/f00"), b"changed").unwrap();
    fs::remove_file(source.join("wide/f01")).unwrap();
    let seen = watch.drain();
    assert!(saw(&seen, &source.join("a"), "modify"), "{seen:?}");
    assert!(saw(&seen, &source.join("a"), "attrib"), "{seen:?}");
    assert!(saw(&seen, &source.join("empty"), "attrib"), "{seen:?}");
    assert!(saw(&seen, &source.join("wide/f00"), "modify"), "{seen:?}");
    assert!(saw(&seen, &source.join("wide/f01"), "delete"), "{seen:?}");
}

/// P77, free run: a `copy` takes no lock on any source inode and opens no
/// source path for writing, and the source's `lstat` census is unchanged.
#[test]
fn a_copy_takes_no_source_lock_and_opens_nothing_for_write() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("copy-free");
    let source = scratch.path("source");
    let destination = scratch.path("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    populate(&source);
    settle_racy_window(&source).unwrap();
    let census = lstat_census(&source);
    let keys: BTreeSet<String> = lock_keys(&source).into_keys().collect();
    let watch = Watch::start(&[&source]);
    let sampler = Sampler::start(vec![source.clone()], keys, Held::default());
    let stats = copy(
        &source,
        &destination,
        &scratch.path("source-state"),
        &scratch.path("destination-state"),
    )
    .unwrap();
    let observed = sampler.finish();
    let written = watch.drain();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_carried(&source, &destination);
    assert!(observed.samples > 0);
    assert!(
        observed.locks.is_empty(),
        "source locks during a copy: {:?}",
        observed.locks
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens of source paths during a copy: {:?}",
        observed.opens
    );
    assert_eq!(written, [], "write events on the source during a copy");
    assert_eq!(lstat_census(&source), census, "the copy changed the source");
    eprintln!(
        "p77 copy free run: {} samples, mean period {} us",
        observed.samples,
        observed.period_us()
    );
}

/// P77, holder run: with an exclusive `flock` and OFD write lock held on
/// every source node and the tree read-only, a `copy` neither waits on a
/// lock nor fails at one, opens nothing for writing, and carries every byte.
#[test]
fn a_copy_never_waits_on_or_fails_at_a_source_lock_holder() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("copy-held");
    let source = scratch.path("source");
    let destination = scratch.path("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    populate(&source);
    let holder = Holder::take(&source, &BTreeSet::new());
    make_read_only(&source);
    settle_racy_window(&source).unwrap();
    let census = lstat_census(&source);
    let keys: BTreeSet<String> = lock_keys(&source).into_keys().collect();
    let watch = Watch::start(&[&source]);
    let sampler = Sampler::start(vec![source.clone()], keys, holder.shape());
    let ((stats, written), blocked) = {
        let (source, destination) = (source.clone(), destination.clone());
        let (source_state, destination_state) = (
            scratch.path("source-state"),
            scratch.path("destination-state"),
        );
        within_deadline(vec![holder], move || {
            let stats = copy(&source, &destination, &source_state, &destination_state);
            // Before the holder's own descriptors close.
            (stats, watch.drain())
        })
    };
    let observed = sampler.finish();
    assert!(
        !blocked,
        "the copy waited on a source lock: {:?}",
        observed.locks
    );
    let stats = stats.unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert!(
        observed.locks.is_empty(),
        "source lock attempts during a copy: {:?}",
        observed.locks
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens of source paths during a copy: {:?}",
        observed.opens
    );
    assert_eq!(written, [], "write events on the source during a copy");
    eprintln!(
        "p77 copy holder run: {} samples, mean period {} us, read-only enforced: {}",
        observed.samples,
        observed.period_us(),
        !is_root()
    );
    assert_carried(&source, &destination);
    assert_eq!(lstat_census(&source), census, "the copy changed the source");
}

/// P77, serve leg: the source reader as the `serve` verb of the real binary,
/// in its own process, the way `pull` starts it over ssh. With every source
/// node held and the tree read-only, the served transfer finishes, no
/// process takes or waits for a source lock, no process writes, creates or
/// opens for writing anything in the source, and every byte arrives.
#[test]
fn a_served_source_in_its_own_process_takes_no_lock() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("serve-held");
    let source = scratch.path("source");
    let destination = scratch.path("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    populate(&source);
    let holder = Holder::take(&source, &BTreeSet::new());
    make_read_only(&source);
    settle_racy_window(&source).unwrap();
    let census = lstat_census(&source);
    let keys: BTreeSet<String> = lock_keys(&source).into_keys().collect();

    let stderr = scratch.path("serve.stderr");
    let mut child = Command::new(env!("CARGO_BIN_EXE_bulkload-agent"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let mut output = child.stdin.take().unwrap();
    let mut input = child.stdout.take().unwrap();

    let watch = Watch::start(&[&source]);
    let sampler = Sampler::start(vec![source.clone()], keys, holder.shape());
    let ((stats, written), blocked) = {
        let (source, destination) = (source.clone(), destination.clone());
        let (source_state, destination_state) = (
            scratch.path("source-state"),
            scratch.path("destination-state"),
        );
        within_deadline(vec![holder], move || {
            let stats = receive(
                &mut input,
                &mut output,
                &source,
                &source_state,
                &destination,
                &destination_state,
            );
            // Closing its stdin ends the serve process.
            drop(output);
            (stats, watch.drain())
        })
    };
    let observed = sampler.finish();
    let served = child.wait().unwrap();
    let said = fs::read_to_string(&stderr).unwrap_or_default();
    assert!(
        !blocked,
        "the served transfer waited on a source lock: {:?}",
        observed.locks
    );
    let stats = stats.unwrap_or_else(|refusal| panic!("receive refused: {refusal}; serve: {said}"));
    assert!(served.success(), "serve failed: {said}");
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert!(
        observed.locks.is_empty(),
        "source lock attempts during a served transfer: {:?}",
        observed.locks
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens of source paths by the receiver: {:?}",
        observed.opens
    );
    assert_eq!(
        written,
        [],
        "write events on the source during a served transfer"
    );
    assert_carried(&source, &destination);
    assert_eq!(
        lstat_census(&source),
        census,
        "the served transfer changed the source"
    );
    eprintln!(
        "p77 serve process holder run: {} samples, mean period {} us",
        observed.samples,
        observed.period_us()
    );
}

/// A fixture Git child (test identity, no user configuration).
fn fixture_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Test", "-c", "user.email=test@localhost"])
        .args(["-c", "commit.gpgsign=false", "-C"])
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository whose index Git would rewrite if it were allowed to: a
/// commit, a staged change, a worktree change, an untracked file, and one
/// tracked file re-stamped with the same bytes. The re-stamp makes its index
/// entry stale, so an unhardened `git status` refreshes the index: it takes
/// `.git/index.lock` and renames it over `.git/index`.
fn stale_index_repository(repo: &Path) {
    fs::create_dir(repo).unwrap();
    fixture_git(repo, &["init", "--quiet", "--template=", "-b", "main"]);
    populate(repo);
    fixture_git(repo, &["add", "."]);
    fixture_git(repo, &["commit", "--quiet", "-m", "one"]);
    fs::write(repo.join("b"), b"staged\n").unwrap();
    fixture_git(repo, &["add", "b"]);
    fs::write(repo.join("b"), b"worktree\n").unwrap();
    fs::write(repo.join("untracked"), b"untracked\n").unwrap();
    settle_racy_window(repo).unwrap();
    let same = fs::read(repo.join("wide/f00")).unwrap();
    fs::write(repo.join("wide/f00"), same).unwrap();
}

/// The fixture is as stale as it claims: an ordinary `git status` rewrites
/// the copy's index (seen by the watch), so a capture that leaves the
/// original's alone is hardened, not lucky.
#[test]
fn the_git_fixture_makes_an_unhardened_status_rewrite_the_index() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("git-stale");
    let repo = scratch.path("repo");
    stale_index_repository(&repo);
    settle_racy_window(&repo).unwrap();
    let census = lstat_census(&repo);
    let watch = Watch::start(&[&repo]);
    fixture_git(&repo, &["--no-optional-locks", "status", "--porcelain"]);
    assert_eq!(watch.drain(), [], "a hardened status wrote the repository");
    assert_eq!(lstat_census(&repo), census);
    let output = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let written = watch.drain();
    assert!(
        written.iter().any(
            |event| event.path == repo.join(".git/index.lock") && event.what.contains("create")
        ),
        "an unhardened status took no index.lock: {written:?}"
    );
    assert_ne!(
        lstat_census(&repo),
        census,
        "an unhardened status left the census alone"
    );
}

/// P77, Git leg: with every node of a repository (worktree and `.git`) held
/// under exclusive locks, a v1 capture of it finishes, and no process (the
/// agent or any Git child) writes, creates, renames, re-stamps or opens for
/// writing anything in it. Git locks with `*.lock` files, which the kernel's
/// lock table never lists, so the write watch and the `lstat` census carry
/// this leg; the lock table and the holder add that no `flock` or `fcntl`
/// lock is taken or waited for either.
#[test]
fn a_git_capture_never_waits_on_a_source_lock_holder() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("git-held");
    let repo = scratch.path("repo");
    stale_index_repository(&repo);
    let holder = Holder::take(&repo, &BTreeSet::new());
    settle_racy_window(&repo).unwrap();
    let census = lstat_census(&repo);
    let keys: BTreeSet<String> = lock_keys(&repo).into_keys().collect();
    let watch = Watch::start(&[&repo]);
    let sampler = Sampler::start(vec![repo.clone()], keys, holder.shape());
    let ((bundle, written), blocked) = {
        let (repo, capture) = (repo.clone(), scratch.path("capture"));
        within_deadline(vec![holder], move || {
            let bundle = bulkload_agent::git_carry::export_repository(&repo, &capture);
            (bundle, watch.drain())
        })
    };
    let observed = sampler.finish();
    assert!(
        !blocked,
        "the capture waited on a source lock: {:?}",
        observed.locks
    );
    let bundle = bundle.unwrap();
    assert!(bundle.is_file());
    assert_eq!(
        written,
        [],
        "write events in the repository during a capture (a `.lock` file is \
         how Git locks)"
    );
    assert!(
        observed.locks.is_empty(),
        "source lock attempts during a capture: {:?}",
        observed.locks
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens of source paths during a capture: {:?}",
        observed.opens
    );
    assert_eq!(
        lstat_census(&repo),
        census,
        "the capture changed the source"
    );
    eprintln!(
        "p77 git capture holder run: {} samples, mean period {} us",
        observed.samples,
        observed.period_us()
    );
}

/// P77, estate leg: `estate::capture` of a plan of two repositories, each
/// with a stale index, with every node of both held. The pass finishes, no
/// process writes, creates, renames, re-stamps or opens for writing anything
/// in either, no `flock` or `fcntl` lock is taken or waited for on either,
/// and the estate's own exclusive lock (`estate.lock`) and the plan's lie
/// outside both.
#[test]
// One linear scenario: two repositories, their holders, one pass.
#[allow(clippy::too_many_lines)]
fn an_estate_capture_never_locks_or_writes_its_sources() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("estate-held");
    let repos = [scratch.path("first"), scratch.path("second")];
    for repo in &repos {
        stale_index_repository(repo);
    }
    let plan = scratch.path("plan.json");
    let state = scratch.path("state");
    let corpus = scratch.path("corpus");
    for private in [&state, &corpus] {
        fs::create_dir(private).unwrap();
        fs::set_permissions(private, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let items: Vec<bulkload_agent::estate::Item> = repos
        .iter()
        .enumerate()
        .map(|(index, repo)| bulkload_agent::estate::Item {
            source: repo.clone(),
            repository: scratch.path(&format!("landed-{index}")),
            workspace: None,
        })
        .collect();
    bulkload_agent::estate::add_batch(&plan, &items).unwrap();

    let holders: Vec<Holder> = repos
        .iter()
        .map(|repo| Holder::take(repo, &BTreeSet::new()))
        .collect();
    let mut held = Held::default();
    let mut keys = BTreeSet::new();
    let mut census = Vec::new();
    for (repo, holder) in repos.iter().zip(&holders) {
        settle_racy_window(repo).unwrap();
        let shape = holder.shape();
        held.keys.extend(shape.keys);
        held.descriptors.extend(shape.descriptors);
        keys.extend(lock_keys(repo).into_keys());
        census.push(lstat_census(repo));
    }
    let watch = Watch::start(&[&repos[0], &repos[1]]);
    let sampler = Sampler::start(repos.to_vec(), keys, held);
    let receipts = Arc::new(Mutex::new(Vec::new()));
    let ((captured, written), blocked) = {
        let (plan, state) = (plan.clone(), state.clone());
        let receipts = Arc::clone(&receipts);
        within_deadline(holders, move || {
            let captured = bulkload_agent::estate::capture(
                &plan,
                &state,
                &corpus,
                2,
                &|receipt: &bulkload_agent::estate::Receipt| {
                    receipts.lock().unwrap().push((
                        receipt.source.clone(),
                        receipt.outcome,
                        receipt.reason.clone(),
                    ));
                    Ok(())
                },
            );
            (captured, watch.drain())
        })
    };
    let observed = sampler.finish();
    assert!(
        !blocked,
        "the estate capture waited on a source lock: {:?}",
        observed.locks
    );
    captured.unwrap();
    let receipts = receipts.lock().unwrap().clone();
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    for (source, outcome, reason) in &receipts {
        assert!(
            reason.is_none(),
            "{} was not captured: {outcome} {reason:?}",
            source.display()
        );
    }
    assert_eq!(
        written,
        [],
        "write events in the sources during an estate capture"
    );
    assert!(
        observed.locks.is_empty(),
        "source lock attempts during an estate capture: {:?}",
        observed.locks
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens of source paths during an estate capture: {:?}",
        observed.opens
    );
    for (repo, before) in repos.iter().zip(&census) {
        assert_eq!(
            &lstat_census(repo),
            before,
            "the estate capture changed {}",
            repo.display()
        );
    }
    // The estate's own exclusive locks are files of its state and its plan.
    assert!(state.join("estate.lock").is_file());
    assert!(plan.with_extension("lock").is_file());
    eprintln!(
        "p77 estate capture holder run: {} samples, mean period {} us, receipts {:?}",
        observed.samples,
        observed.period_us(),
        receipts
            .iter()
            .map(|(_, outcome, _)| *outcome)
            .collect::<Vec<_>>()
    );
}

/// P77, estimate leg: a local `git-carry-estimate` reads two live
/// repositories, the source and the destination, through the `bash` probe
/// and the hardened Git children behind it. With every node of both held and
/// both indexes stale, the estimate finishes, and no process writes,
/// creates, renames, re-stamps or opens for writing anything in either, and
/// no `flock` or `fcntl` lock is taken or waited for on either. The estimate
/// over ssh runs the same probe on the far host and is not run here.
#[test]
fn a_git_carry_estimate_never_locks_or_writes_either_repository() {
    use bulkload_agent::git_carry::estimate::{estimate, Destination};
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("estimate-held");
    let repos = [scratch.path("source"), scratch.path("destination")];
    for repo in &repos {
        stale_index_repository(repo);
    }
    let holders: Vec<Holder> = repos
        .iter()
        .map(|repo| Holder::take(repo, &BTreeSet::new()))
        .collect();
    let mut held = Held::default();
    let mut keys = BTreeSet::new();
    let mut census = Vec::new();
    for (repo, holder) in repos.iter().zip(&holders) {
        settle_racy_window(repo).unwrap();
        let shape = holder.shape();
        held.keys.extend(shape.keys);
        held.descriptors.extend(shape.descriptors);
        keys.extend(lock_keys(repo).into_keys());
        census.push(lstat_census(repo));
    }
    let watch = Watch::start(&[&repos[0], &repos[1]]);
    let sampler = Sampler::start(repos.to_vec(), keys, held);
    let ((estimated, written), blocked) = {
        let (source, destination) = (repos[0].clone(), repos[1].clone());
        within_deadline(holders, move || {
            let estimated = estimate(&source, &Destination::Local(destination))
                .map(|estimate| estimate.lines())
                .map_err(|refused| refused.lines());
            (estimated, watch.drain())
        })
    };
    let observed = sampler.finish();
    assert!(
        !blocked,
        "the estimate waited on a source lock: {:?}",
        observed.locks
    );
    let lines = estimated.unwrap();
    assert!(!lines.is_empty(), "the estimate reported nothing");
    assert_eq!(
        written,
        [],
        "write events in a repository during an estimate (a `.lock` file is \
         how Git locks)"
    );
    assert!(
        observed.locks.is_empty(),
        "lock attempts on a repository during an estimate: {:?}",
        observed.locks
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens of repository paths during an estimate: {:?}",
        observed.opens
    );
    for (repo, before) in repos.iter().zip(&census) {
        assert_eq!(
            &lstat_census(repo),
            before,
            "the estimate changed {}",
            repo.display()
        );
    }
    eprintln!(
        "p77 estimate holder run: {} samples, mean period {} us, {} report lines",
        observed.samples,
        observed.period_us(),
        lines.len()
    );
}

// ------------------------------------------------------------ the SQLite legs

/// The live writer for the `SQLite` legs, run as a separate process: with
/// [`WRITER_ENV`] set, it opens the database in WAL mode, commits rows that
/// stay in the `-wal` (no checkpoint), prints a ready line, and holds the
/// connection open until its stdin closes. With [`WRITER_LIVE_ENV`] set as
/// well it keeps committing one row every few milliseconds until then, with
/// no busy timeout, and reports each commit, how many found the database
/// busy, and its slowest commit. Without [`WRITER_ENV`] this test does
/// nothing.
#[test]
fn sqlite_writer_helper() {
    let Some(db) = std::env::var_os(WRITER_ENV) else {
        return;
    };
    let live = std::env::var_os(WRITER_LIVE_ENV).is_some();
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .execute_batch(
            "PRAGMA wal_autocheckpoint=0;
             CREATE TABLE rows(id INTEGER PRIMARY KEY, body BLOB NOT NULL);",
        )
        .unwrap();
    for batch in 0..4_u64 {
        let transaction = connection.unchecked_transaction().unwrap();
        for row in 0..500_u64 {
            transaction
                .execute(
                    "INSERT INTO rows(body) VALUES (?1)",
                    [noise(batch * 1000 + row, 1024)],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }
    println!("p77-writer-ready");
    std::io::stdout().flush().unwrap();
    let closed = Arc::new(AtomicBool::new(false));
    let closing = Arc::clone(&closed);
    let stdin = thread::spawn(move || {
        let mut rest = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut rest);
        closing.store(true, Ordering::Release);
    });
    let (mut commits, mut busy, mut slowest) = (0_u64, 0_u64, Duration::ZERO);
    while live && !closed.load(Ordering::Acquire) {
        let began = std::time::Instant::now();
        match connection.execute(
            "INSERT INTO rows(body) VALUES (?1)",
            [noise(90_000 + commits, 256)],
        ) {
            Ok(_) => {
                commits += 1;
                slowest = slowest.max(began.elapsed());
                println!("p77-writer-commits {commits}");
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                busy += 1;
            }
            Err(other) => panic!("the live writer failed: {other}"),
        }
        thread::sleep(Duration::from_millis(2));
    }
    stdin.join().unwrap();
    println!(
        "p77-writer-done commits={commits} busy={busy} slowest_us={}",
        slowest.as_micros()
    );
    std::io::stdout().flush().unwrap();
    drop(connection);
}

/// The writer process and what its stdout has said so far.
struct Writer {
    child: std::process::Child,
    reader: thread::JoinHandle<()>,
    commits: Arc<AtomicU64>,
    done: Arc<Mutex<Option<String>>>,
}

/// What the writer reported when it ended.
#[derive(Debug)]
struct WriterReport {
    commits: u64,
    busy: u64,
    slowest: Duration,
}

impl Writer {
    /// Start the writer on `db` and wait until its rows are committed.
    fn start(db: &Path, live: bool) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "sqlite_writer_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WRITER_ENV, db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if live {
            command.env(WRITER_LIVE_ENV, "1");
        }
        let mut child = command.spawn().unwrap();
        // libtest prints `test NAME ... ` before the test's own output, so a
        // marker ends a line rather than being one. The reader drains the
        // writer's stdout to its end, so the writer never blocks on it.
        let (ready, became_ready) = mpsc::channel();
        let commits = Arc::new(AtomicU64::new(0));
        let done = Arc::new(Mutex::new(None));
        let stdout = child.stdout.take().unwrap();
        let (counting, finishing) = (Arc::clone(&commits), Arc::clone(&done));
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.ends_with("p77-writer-ready") {
                    let _ = ready.send(());
                } else if let Some((_, count)) = line.split_once("p77-writer-commits ") {
                    counting.store(count.trim().parse().unwrap_or(0), Ordering::Release);
                } else if let Some((_, report)) = line.split_once("p77-writer-done ") {
                    *finishing.lock().unwrap() = Some(report.to_owned());
                }
            }
        });
        let mut writer = Self {
            child,
            reader,
            commits,
            done,
        };
        if became_ready.recv_timeout(DEADLINE).is_err() {
            // Closing its stdin ends the writer whatever state it is in.
            drop(writer.child.stdin.take());
            let _ = writer.child.wait();
            panic!("the writer never became ready");
        }
        writer
    }

    fn commits(&self) -> u64 {
        self.commits.load(Ordering::Acquire)
    }

    /// Close the writer's stdin, wait for it, and return its report.
    fn finish(mut self) -> WriterReport {
        drop(self.child.stdin.take());
        self.reader.join().unwrap();
        assert!(self.child.wait().unwrap().success(), "the writer failed");
        let report = self
            .done
            .lock()
            .unwrap()
            .clone()
            .expect("the writer's report");
        let field = |name: &str| -> u64 {
            report
                .split_whitespace()
                .find_map(|word| word.strip_prefix(name))
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("no {name} in {report:?}"))
        };
        WriterReport {
            commits: field("commits="),
            busy: field("busy="),
            slowest: Duration::from_micros(field("slowest_us=")),
        }
    }
}

/// The lock keys of a database and its two sidecars.
struct DatabaseKeys {
    db: String,
    wal: String,
    shm: String,
}

/// What a snapshot's sampled locks and write-mode opens came to: how many
/// samples saw each part of the Q16/Q36 exception, and everything outside
/// it.
#[derive(Debug, Default)]
struct Accounting {
    /// Samples that saw this process's shared READ lock on the database.
    db_read: u64,
    /// Samples that saw this process's READ locks on the `-shm`.
    shm_read: u64,
    /// Samples that saw this process's one-byte WRITE lock on a `-shm`
    /// read-mark slot.
    shm_mark: u64,
    /// Samples that saw this process hold the `-shm` open for writing.
    shm_open: u64,
    /// Samples that saw this process hold the `-wal` open for writing: the
    /// empty `-wal` it creates (OI-1003-Q72), or an existing one
    /// ([`RESIDUAL_WAL_OPEN_READ_WRITE`], bulkload#157).
    wal_open: u64,
    /// The longest this process was seen holding its database READ lock.
    db_hold: Duration,
    breaches: Vec<String>,
}

/// `SQLite`'s wal-index lock bytes in the `-shm`: 120 is the WRITE lock, 121
/// the checkpoint lock, 122 the recovery lock, 123 to 127 the five read
/// marks, and 128 the "dead man's switch" every connection holds shared.
const SHM_WRITE_LOCK: u64 = 120;
const SHM_READ_MARK_1: u64 = 124;
const SHM_DMS: u64 = 128;

/// Sort what the sampler saw of a snapshot into the exception and breaches.
///
/// The exception, exactly (OI-1003-Q16, OI-1003-Q36, OI-1003-Q72): this process's granted
/// READ locks on the database; its granted READ locks on the `-shm`'s lock
/// bytes; and its granted one-byte WRITE lock on a read-mark slot other than
/// slot 0 (`SQLite` takes it for an instant to move a read mark; it excludes
/// no writer, which never locks a read mark). Not in the exception: any
/// WRITE lock on the `-shm`'s write, checkpoint or recovery byte or across
/// several bytes; any lock on the `-wal`; any lock this process waits for;
/// and any lock *another* process waits for on any of the three, which is
/// the live writer being held up. Write-mode descriptors: the `-shm` (Q36);
/// a `-wal` that did not exist before the read (Q72, `wal_existed` false);
/// and, as a named residual only, one that did.
fn account(
    observed: &Observed,
    keys: &DatabaseKeys,
    paths: &BTreeMap<String, PathBuf>,
    shm: &Path,
    wal: &Path,
    wal_existed: bool,
) -> Accounting {
    let ours = std::process::id().to_string();
    let mut accounting = Accounting::default();
    for (line, samples) in &observed.locks {
        let on = paths.get(&line.key);
        if line.blocked {
            accounting
                .breaches
                .push(format!("a lock waited for on {on:?}: {line:?}"));
            continue;
        }
        if line.pid != ours {
            // The writer's own granted locks on its own database.
            if line.key != keys.db && line.key != keys.wal && line.key != keys.shm {
                accounting
                    .breaches
                    .push(format!("another process's lock on {on:?}: {line:?}"));
            }
            continue;
        }
        let end = line.end.unwrap_or(u64::MAX);
        let on_lock_bytes = line.start >= SHM_WRITE_LOCK && end <= SHM_DMS;
        if line.key == keys.db && line.mode == "READ" {
            accounting.db_read += samples;
            let held = observed.longest.get(line).copied().unwrap_or_default();
            accounting.db_hold = accounting.db_hold.max(held);
        } else if line.key == keys.shm && line.mode == "READ" && on_lock_bytes {
            accounting.shm_read += samples;
        } else if line.key == keys.shm
            && line.mode == "WRITE"
            && line.start == end
            && (SHM_READ_MARK_1..SHM_DMS).contains(&line.start)
        {
            accounting.shm_mark += samples;
        } else {
            accounting
                .breaches
                .push(format!("outside the exception, on {on:?}: {line:?}"));
        }
    }
    for (open, samples) in &observed.opens {
        if open.path == shm {
            accounting.shm_open += samples;
        } else if open.path == wal && (!wal_existed || RESIDUAL_WAL_OPEN_READ_WRITE) {
            // Where no `-wal` existed, creating the empty one is Q72's
            // exception; on one that existed it is the named residual.
            accounting.wal_open += samples;
        } else {
            accounting
                .breaches
                .push(format!("write-mode open {open:?}"));
        }
    }
    accounting
}

/// The names in `directory`, sorted.
fn listing(directory: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

/// The lstat census of `directory` without the named entries.
fn census_without(directory: &Path, without: &[&str]) -> Census {
    let mut census = lstat_census(directory);
    for name in without {
        census.remove(Path::new(name));
    }
    census
}

/// This process's locks on any of `keys`, right now, without the holder's.
fn our_locks(keys: &BTreeMap<String, PathBuf>, held: &Held) -> Vec<LockLine> {
    let ours = std::process::id().to_string();
    read_locks()
        .into_iter()
        .filter(|line| line.pid == ours && keys.contains_key(&line.key) && !held.owns(line, &ours))
        .collect()
}

/// Say on stderr which `SQLite` leg this run proves, and return whether it
/// is the root one. One line per test, so a log shows which half ran.
fn sqlite_leg(test: &str) -> bool {
    let root = is_root();
    if root {
        eprintln!(
            "p77 sqlite leg [{test}]: ROOT (euid 0). Proving the typed refusal \
             SQLITE_SOURCE_AS_ROOT and an untouched source (OI-1003-Q76). The \
             Q16/Q36/Q72 exception leg is NOT run as root."
        );
    } else {
        eprintln!(
            "p77 sqlite leg [{test}]: USER (euid {}). Running the backup under \
             the Q16/Q36/Q72 exception. The root refusal (OI-1003-Q76) is NOT \
             proved by this run.",
            // SAFETY: `geteuid` takes no arguments and cannot fail.
            unsafe { libc::geteuid() }
        );
    }
    root
}

/// A WAL-mode database with `rows` rows that was checkpointed and closed:
/// no `-wal` and no `-shm` sit beside it.
fn closed_wal_database(db: &Path, rows: u64) {
    let connection = rusqlite::Connection::open(db).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .execute_batch("CREATE TABLE rows(id INTEGER PRIMARY KEY, body BLOB NOT NULL);")
        .unwrap();
    let transaction = connection.unchecked_transaction().unwrap();
    for row in 0..rows {
        transaction
            .execute("INSERT INTO rows(body) VALUES (?1)", [noise(row, 1024)])
            .unwrap();
    }
    transaction.commit().unwrap();
    connection.close().map_err(|(_, error)| error).unwrap();
    for suffix in ["-wal", "-shm"] {
        assert!(
            !sidecar(db, suffix).exists(),
            "a closed database kept {suffix}"
        );
    }
}

/// `db` with `suffix` appended to its file name, as `SQLite` names sidecars.
fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The bytes of every regular file directly in `directory`.
fn file_bytes(directory: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    listing(directory)
        .into_iter()
        .filter(|name| directory.join(name).is_file())
        .map(|name| {
            let bytes = fs::read(directory.join(&name)).unwrap();
            (name, bytes)
        })
        .collect()
}

/// The source a root leg aims the provider at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSource {
    /// A writer process holds the database open with frames in its `-wal`
    /// and commits nothing more: the directory is still.
    IdleWriter,
    /// A WAL-mode database, checkpointed and closed: no sidecar.
    Closed,
    /// A writer process commits a row every few milliseconds.
    LiveWriter,
}

/// P77, the `SQLite` legs as root (OI-1003-Q76; S2).
///
/// Every provider verb that reads a database (`snapshot`, both compose
/// verbs, `apply_state_candidate`, `hydrate_state`) is called on the source
/// with the real effective uid of 0, under the same holder, write watch and
/// sampler as the user legs. Asserted:
///
/// - each returns the typed refusal, code `SQLITE_SOURCE_AS_ROOT`, and the
///   output directory stays empty;
/// - **no write event**: the inotify queue (complete, every process) is
///   empty for a still source; under the live writer it holds events on that
///   writer's own `-wal` and `-shm` and nothing else;
/// - **no lock**: this process has no lock line on any source inode in any
///   sample or in a read made after the calls return, nothing waits, and no
///   sample sees a write-mode descriptor on a source path;
/// - **the directory is identical**: its listing, the `lstat` census of every
///   node (the directory, the database, the `-wal` and `-shm` included; the
///   live writer's two sidecars excepted) and every file's bytes. A closed
///   database gets no `-wal` and no `-shm`.
///
/// The refused calls return in microseconds, so the sampler may see few
/// samples; the complete channels here are the write watch, the census, the
/// bytes and the lock read after return. This leg runs no backup: what a
/// permitted read does to its source is the user legs', not proved as root.
// One linear scenario: fixture, the five verbs, then the accounting.
#[allow(clippy::too_many_lines)]
fn sqlite_source_read_is_refused_as_root(shape: RootSource) {
    assert!(is_root(), "the root leg runs with an effective uid of 0");
    let scratch = Scratch::new(match shape {
        RootSource::IdleWriter => "sqlite-root-idle",
        RootSource::Closed => "sqlite-root-closed",
        RootSource::LiveWriter => "sqlite-root-live",
    });
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sibling"), noise(9, 8192)).unwrap();
    let db = source.join("state.db");
    let writer = match shape {
        RootSource::IdleWriter => Some(Writer::start(&db, false)),
        RootSource::LiveWriter => Some(Writer::start(&db, true)),
        RootSource::Closed => {
            closed_wal_database(&db, 200);
            None
        }
    };
    let (wal, shm) = (sidecar(&db, "-wal"), sidecar(&db, "-shm"));
    let live = shape == RootSource::LiveWriter;
    // The live writer rewrites its own two sidecars throughout.
    let moving: &[&str] = if live {
        &["state.db-shm", "state.db-wal"]
    } else {
        &[]
    };
    let still = |directory: &Path| {
        let mut bytes = file_bytes(directory);
        for name in moving {
            bytes.remove(std::ffi::OsStr::new(name));
        }
        (listing(directory), census_without(directory, moving), bytes)
    };
    let before = still(&source);

    let paths = lock_keys(&source);
    let database: BTreeSet<&Path> = ["state.db", "state.db-wal", "state.db-shm"]
        .into_iter()
        .map(Path::new)
        .collect();
    // The writer holds its own locks on its database; a closed database has
    // no writer, so the holder takes it too.
    let skip: BTreeSet<PathBuf> = if writer.is_some() {
        database.iter().map(|path| path.to_path_buf()).collect()
    } else {
        BTreeSet::new()
    };
    let holder = Holder::take(&source, &skip);
    let watch = Watch::start(&[&source]);
    let sampler = Sampler::start(
        vec![source.clone()],
        paths.keys().cloned().collect(),
        holder.shape(),
    );
    sampler.wait_samples(1);

    let output_dir = scratch.path("out");
    fs::create_dir(&output_dir).unwrap();
    fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700)).unwrap();
    // The watch is drained before the holder is dropped: closing the
    // holder's own write-mode descriptors is a write event too.
    let ((refused, written, left), blocked) = {
        let (output_dir, paths) = (output_dir.clone(), paths.clone());
        let held = holder.shape();
        within_deadline(vec![holder], move || {
            let output = output_dir.join("snapshot.db");
            let candidate = output_dir.join("candidate.db");
            let mapping = PathMapping {
                source_home: Path::new("/Users/p77"),
                destination_home: Path::new("/home/p77"),
            };
            let refused: Vec<(&str, Option<BulkloadRefusal>)> = vec![
                (
                    "snapshot",
                    provider_sqlite::snapshot(&db, &output, 10_000).err(),
                ),
                (
                    "compose",
                    provider_sqlite::compose_snapshots(&db, &db, &candidate, "p77", 100).err(),
                ),
                (
                    "compose-state",
                    provider_sqlite::compose_state_snapshots(
                        &db, &db, &candidate, "p77", 100, &mapping,
                    )
                    .err(),
                ),
                (
                    "apply-state-candidate",
                    provider_sqlite::online::apply_state_candidate(&output, &db, &db, 1, &|_| {
                        Ok(())
                    })
                    .err(),
                ),
                (
                    "hydrate-state",
                    provider_sqlite::hydrate::hydrate_state(
                        &db,
                        &mapping,
                        1,
                        1,
                        Path::new("gzip"),
                        Path::new("zstd"),
                        &|_| Ok(()),
                    )
                    .err(),
                ),
            ];
            (refused, watch.drain(), our_locks(&paths, &held))
        })
    };
    sampler.wait_samples(1);
    let observed = sampler.finish();
    let after = still(&source);
    let produced = listing(&output_dir);
    let report = writer.map(Writer::finish);

    assert!(!blocked, "a refused verb waited: {:?}", observed.locks);
    for (verb, refusal) in &refused {
        assert_eq!(
            refusal.as_ref().map(BulkloadRefusal::code),
            Some("SQLITE_SOURCE_AS_ROOT"),
            "{verb} as root (OI-1003-Q76): {refusal:?}"
        );
    }
    assert_eq!(refused.len(), 5);
    assert_eq!(produced, Vec::<std::ffi::OsString>::new(), "an output");
    // No write event. The live writer's own sidecar writes are not ours.
    let outside: Vec<&WriteEvent> = written
        .iter()
        .filter(|event| !(live && (event.path == wal || event.path == shm)))
        .collect();
    assert_eq!(
        outside,
        Vec::<&WriteEvent>::new(),
        "write events on the source of a refused read"
    );
    // No lock and no write-mode descriptor of ours, in any sample or after.
    let ours = std::process::id().to_string();
    let breaches: Vec<String> = observed
        .locks
        .keys()
        .filter(|line| {
            line.blocked
                || line.pid == ours
                || !paths
                    .get(&line.key)
                    .is_some_and(|path| database.contains(path.as_path()))
        })
        .map(|line| format!("{:?}: {line:?}", paths.get(&line.key)))
        .collect();
    assert_eq!(
        breaches,
        Vec::<String>::new(),
        "locks during a refused read"
    );
    assert!(
        observed.opens.is_empty(),
        "write-mode opens during a refused read: {:?}",
        observed.opens
    );
    assert_eq!(left, [], "locks left after the refused calls returned");
    // Byte- and metadata-identical.
    assert_eq!(after.0, before.0, "the source directory's listing changed");
    assert_eq!(after.1, before.1, "a refused read moved source metadata");
    assert!(after.2 == before.2, "a refused read changed source bytes");
    if shape == RootSource::Closed {
        assert!(
            !wal.exists() && !shm.exists(),
            "a refused read left a sidecar"
        );
    }
    if let Some(report) = &report {
        assert_eq!(report.busy, 0, "the writer found the database busy");
        if !live {
            assert_eq!(report.commits, 0, "{report:?}");
        }
    }
    eprintln!(
        "p77 sqlite root refusal ({shape:?}, OI-1003-Q76): 5 verbs refused \
         SQLITE_SOURCE_AS_ROOT; write events {}; our locks 0; write-mode opens 0; \
         {} nodes lstat-identical, {} files byte-identical; {} samples; writer {:?}",
        outside.len(),
        before.1.len(),
        before.2.len(),
        observed.samples,
        report
    );
}

/// P77, `SQLite` leg (OI-1003-Q16, OI-1003-Q36, OI-1003-Q72): the backup API
/// against a WAL database a separate writer holds open. As root it is the
/// refusal leg instead ([`sqlite_source_read_is_refused_as_root`],
/// OI-1003-Q76).
///
/// - This process's source locks fall only under the exception as
///   [`account`] states it, and the exception is observed, not assumed: the
///   database READ lock, the `-shm` READ locks and the `-shm` open are each
///   seen in one snapshot. A 2 MiB backup can finish between two samples, so
///   the leg takes up to [`OBSERVE_ATTEMPTS`] snapshots; every one of them
///   is held to every limit here, seen or not.
/// - Nobody waits: not the snapshot, and not the writer on anything the
///   snapshot holds.
/// - The database READ lock is bounded: it is seen for at most
///   [`SQLITE_LOCK_BOUND`], and it is gone when `snapshot` returns.
/// - No other source write occurs (Q36, Q72): the directory's listing and
///   the `lstat` census of everything but the `-shm` are unchanged (the
///   `-wal`'s included: it existed, so Q72 grants nothing on it), the write
///   watch saw events on the `-shm` only, and the database, the `-wal` and
///   the sibling file are byte-identical.
/// - The counters say the same: `source_wal_index_touched` moves by 1 and
///   `source_wal_created` by 0.
///
/// **Residual, not met:** the acceptance criterion "no write-mode open but
/// the `-shm`". The backup's read-only connection holds the existing `-wal`
/// open `O_RDWR`, which no ruling grants. It is counted under
/// [`RESIDUAL_WAL_OPEN_READ_WRITE`] and tracked on bulkload#157; this test
/// does not pass that criterion, it records the residual.
#[test]
// One linear scenario: writer, holder, snapshot, then the accounting.
#[allow(clippy::too_many_lines)]
fn a_sqlite_snapshot_locks_only_under_the_q16_q36_exception() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    if sqlite_leg("idle writer") {
        sqlite_source_read_is_refused_as_root(RootSource::IdleWriter);
        return;
    }
    let scratch = Scratch::new("sqlite");
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    let sibling = noise(9, 8192);
    fs::write(source.join("sibling"), &sibling).unwrap();
    let db = source.join("state.db");
    let writer = Writer::start(&db, false);
    let wal = source.join("state.db-wal");
    let shm = source.join("state.db-shm");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "the rows are in the -wal"
    );
    let db_bytes = fs::read(&db).unwrap();
    let wal_bytes = fs::read(&wal).unwrap();
    let names = listing(&source);
    let census = census_without(&source, &["state.db-shm"]);

    let paths = lock_keys(&source);
    let key_of = |path: &Path| lock_key(&fs::symlink_metadata(path).unwrap());
    let keys = DatabaseKeys {
        db: key_of(&db),
        wal: key_of(&wal),
        shm: key_of(&shm),
    };
    let skip: BTreeSet<PathBuf> = ["state.db", "state.db-wal", "state.db-shm"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let output_dir = scratch.path("snapshot");
    fs::create_dir(&output_dir).unwrap();
    fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let wal_closed = WriteEvent {
        path: wal.clone(),
        what: "close-write".to_owned(),
    };
    // A 2 MiB backup can finish between two samples of `/proc/locks`. Every
    // snapshot is held to every limit below; the leg repeats until one of
    // them is also *seen* under the exception, so it never passes by seeing
    // nothing.
    let mut seen = None;
    let mut tried = Vec::new();
    for attempt in 0..OBSERVE_ATTEMPTS {
        let holder = Holder::take(&source, &skip);
        let watch = Watch::start(&[&source]);
        let sampler = Sampler::start(
            vec![source.clone()],
            paths.keys().cloned().collect(),
            holder.shape(),
        );
        let output = output_dir.join(format!("state-{attempt}.db"));
        let ((result, counted, written, left), blocked) = {
            let (db, output, paths) = (db.clone(), output.clone(), paths.clone());
            let held = holder.shape();
            within_deadline(vec![holder], move || {
                let counters = Counters::snapshot();
                let result = provider_sqlite::snapshot(&db, &output, 10_000);
                let counted = Counters::snapshot().since(counters);
                // Released on return: nothing of ours is left on the source.
                let left = our_locks(&paths, &held);
                (result, counted, watch.drain(), left)
            })
        };
        let observed = sampler.finish();
        assert!(!blocked, "the snapshot waited: {:?}", observed.locks);
        result.unwrap();
        let accounting = account(&observed, &keys, &paths, &shm, &wal, true);
        assert!(
            accounting.breaches.is_empty(),
            "source access outside the Q16/Q36/Q72 exception: {:#?}",
            accounting.breaches
        );
        assert!(
            accounting.db_hold + observed.period() * 2 <= SQLITE_LOCK_BOUND,
            "the database READ lock was held for {:?}, over the bound {SQLITE_LOCK_BOUND:?}",
            accounting.db_hold
        );
        assert_eq!(
            left,
            [],
            "locks left on the source after the snapshot returned"
        );
        // The counters match what the directory shows: a `-shm` beside the
        // source (Q36) and no `-wal` created, since one existed (Q72).
        assert!(shm.exists(), "the -shm is beside the source");
        assert_eq!(
            (
                counted.get(Counter::SourceWalIndexTouched),
                counted.get(Counter::SourceWalCreated)
            ),
            (1, 0),
            "source_wal_index_touched, source_wal_created"
        );
        // Q36, Q72: no other source write. The existing `-wal`'s one
        // tolerated event is the close of the read-write descriptor (the
        // named residual, bulkload#157); a write or a re-stamp of it is not
        // tolerated.
        let outside: Vec<&WriteEvent> = written
            .iter()
            .filter(|event| {
                event.path != shm && !(RESIDUAL_WAL_OPEN_READ_WRITE && **event == wal_closed)
            })
            .collect();
        assert_eq!(
            outside,
            Vec::<&WriteEvent>::new(),
            "write events outside the -shm"
        );
        // Compared while the writer still holds the database open: its own
        // close checkpoints the `-wal` into the database, which is the
        // writer's write, not ours.
        assert_eq!(
            listing(&source),
            names,
            "the source directory's listing changed"
        );
        assert_eq!(
            census_without(&source, &["state.db-shm"]),
            census,
            "the source changed outside the -shm"
        );
        assert!(
            fs::read(&db).unwrap() == db_bytes,
            "the snapshot changed the database"
        );
        assert!(
            fs::read(&wal).unwrap() == wal_bytes,
            "the snapshot changed the -wal"
        );
        assert!(
            fs::read(source.join("sibling")).unwrap() == sibling,
            "the snapshot changed a sibling file"
        );
        let snapshot = rusqlite::Connection::open(&output).unwrap();
        let rows: i64 = snapshot
            .query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2000, "the snapshot holds the -wal's rows");
        tried.push(format!(
            "db-read-lock={} shm-read-lock={} shm-write-open={} over {} samples",
            accounting.db_read, accounting.shm_read, accounting.shm_open, observed.samples
        ));
        // The exception was observed: the database READ lock, the `-shm`
        // READ locks and the `-shm` write-mode open, all in this snapshot.
        if accounting.db_read > 0 && accounting.shm_read > 0 && accounting.shm_open > 0 {
            seen = Some((observed, accounting, counted, written));
            break;
        }
    }
    let report = writer.finish();
    assert_eq!((report.commits, report.busy), (0, 0), "{report:?}");
    let (observed, accounting, counted, written) = seen.unwrap_or_else(|| {
        panic!(
            "no snapshot in {OBSERVE_ATTEMPTS} was seen holding the database READ lock \
             and the -shm: {tried:#?}"
        )
    });
    eprintln!(
        "p77 sqlite lock lines: {:#?}",
        observed
            .locks
            .iter()
            .map(|(line, samples)| format!(
                "{samples}x {} {} {} pid={} {:?} {}..{:?}",
                if line.blocked { "->" } else { "  " },
                line.kind,
                line.mode,
                if line.pid == std::process::id().to_string() {
                    "ours"
                } else {
                    "other"
                },
                paths.get(&line.key),
                line.start,
                line.end
            ))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "p77 sqlite exception (sample counts, Q16/Q36/Q72): snapshot {} of at most \
         {OBSERVE_ATTEMPTS}, {} samples, mean period {} us, \
         db-read-lock={} shm-read-lock={} shm-read-mark-write={} shm-write-open={} \
         wal-write-open={} (#157, residual), db-read-lock held {:?} \
         (bound {SQLITE_LOCK_BOUND:?}), source_wal_index_touched=+{} \
         source_wal_created=+{}, write events {:?}",
        tried.len(),
        observed.samples,
        observed.period_us(),
        accounting.db_read,
        accounting.shm_read,
        accounting.shm_mark,
        accounting.shm_open,
        accounting.wal_open,
        accounting.db_hold,
        counted.get(Counter::SourceWalIndexTouched),
        counted.get(Counter::SourceWalCreated),
        written
            .iter()
            .map(|event| event.what.as_str())
            .collect::<BTreeSet<_>>()
    );
}

/// P77, `SQLite` leg for the empty `-wal` (OI-1003-Q72, which extends
/// OI-1003-Q36): the backup API against a WAL-mode database that was
/// checkpointed and closed, so it has no `-wal` and no `-shm`. As root it is
/// the refusal leg instead (OI-1003-Q76).
///
/// The first snapshot's whole write set is the two counted sidecars:
///
/// - `source_wal_created` moves by 1 and a zero-byte `-wal` sits where none
///   was; `source_wal_index_touched` moves by 1 and a `-shm` sits there;
/// - the write watch saw events on those two names only, and on the `-wal`
///   only its creation and the close of the creating descriptor (no
///   `modify`, no `attrib`);
/// - the listing gained exactly those two names; the database and the
///   sibling are byte- and `lstat`-identical (the directory's own times move
///   with the two creations, and are left out);
/// - whatever the sampler saw of this process falls under the exception,
///   nothing waits, and no lock of ours is left when `snapshot` returns.
///   The creation happens once, so this leg cannot repeat until the sampler
///   sees the READ lock: seeing it is the idle-writer leg's.
///
/// A second snapshot finds the empty `-wal` in place: `source_wal_created`
/// moves by 0, and that `-wal` is byte- and `lstat`-identical afterwards
/// ("a `-wal` that existed before the read stays byte-identical").
///
/// Not seen here: lock lines on the two created sidecars, whose inodes do
/// not exist when the sampler starts. The idle-writer leg accounts for the
/// `-shm`'s locks.
#[test]
// One linear scenario: fixture, holder, two snapshots, then the accounting.
#[allow(clippy::too_many_lines)]
fn a_sqlite_snapshot_creates_only_an_empty_wal_where_none_existed() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    if sqlite_leg("closed database") {
        sqlite_source_read_is_refused_as_root(RootSource::Closed);
        return;
    }
    let scratch = Scratch::new("sqlite-closed");
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    let sibling = noise(9, 8192);
    fs::write(source.join("sibling"), &sibling).unwrap();
    let db = source.join("state.db");
    closed_wal_database(&db, 2000);
    let (wal, shm) = (sidecar(&db, "-wal"), sidecar(&db, "-shm"));
    let db_bytes = fs::read(&db).unwrap();
    let names = listing(&source);
    // Without the directory itself: creating the two sidecars moves its
    // mtime and ctime, which is part of the same counted exception.
    let apart = ["", "state.db-shm", "state.db-wal"];
    let census = census_without(&source, &apart);

    let paths = lock_keys(&source);
    let keys = DatabaseKeys {
        db: lock_key(&fs::symlink_metadata(&db).unwrap()),
        // Neither sidecar has an inode yet, so no sampled line can match.
        wal: String::new(),
        shm: String::new(),
    };
    let skip: BTreeSet<PathBuf> = std::iter::once(PathBuf::from("state.db")).collect();
    let holder = Holder::take(&source, &skip);
    let watch = Watch::start(&[&source]);
    let sampler = Sampler::start(
        vec![source.clone()],
        paths.keys().cloned().collect(),
        holder.shape(),
    );

    let output_dir = scratch.path("snapshot");
    fs::create_dir(&output_dir).unwrap();
    fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let output = output_dir.join("state.db");
    // The watch is drained before the holder is dropped: closing the
    // holder's own write-mode descriptors is a write event too.
    let ((result, counted, written, left), blocked) = {
        let (db, paths) = (db.clone(), paths.clone());
        let held = holder.shape();
        within_deadline(vec![holder], move || {
            let counters = Counters::snapshot();
            let result = provider_sqlite::snapshot(&db, &output, 10_000);
            let counted = Counters::snapshot().since(counters);
            (result, counted, watch.drain(), our_locks(&paths, &held))
        })
    };
    let observed = sampler.finish();

    assert!(!blocked, "the snapshot waited: {:?}", observed.locks);
    result.unwrap();
    let accounting = account(&observed, &keys, &paths, &shm, &wal, false);
    assert!(
        accounting.breaches.is_empty(),
        "source access outside the Q16/Q36/Q72 exception: {:#?}",
        accounting.breaches
    );
    assert_eq!(left, [], "locks left on the source after the snapshot");
    // Each exception matches its counter and its file.
    assert_eq!(
        (
            counted.get(Counter::SourceWalIndexTouched),
            counted.get(Counter::SourceWalCreated)
        ),
        (1, 1),
        "source_wal_index_touched, source_wal_created"
    );
    assert!(shm.exists(), "the -shm the counter reports");
    assert_eq!(
        fs::symlink_metadata(&wal).unwrap().len(),
        0,
        "the created -wal is empty (OI-1003-Q72)"
    );
    // The write set, exactly: the `-shm`, and the `-wal`'s creation.
    let outside: Vec<&WriteEvent> = written
        .iter()
        .filter(|event| event.path != shm && event.path != wal)
        .collect();
    assert_eq!(
        outside,
        Vec::<&WriteEvent>::new(),
        "write events outside the two counted sidecars"
    );
    let on_wal: BTreeSet<&str> = written
        .iter()
        .filter(|event| event.path == wal)
        .map(|event| event.what.as_str())
        .collect();
    assert_eq!(
        on_wal,
        BTreeSet::from(["close-write", "create"]),
        "the empty -wal is created and closed, never written or re-stamped"
    );
    let mut expected = names;
    expected.extend(["state.db-shm".into(), "state.db-wal".into()]);
    expected.sort();
    assert_eq!(listing(&source), expected, "the source directory's listing");
    assert_eq!(
        census_without(&source, &apart),
        census,
        "the source changed outside the two counted sidecars"
    );
    assert!(fs::read(&db).unwrap() == db_bytes, "the database changed");
    assert!(
        fs::read(source.join("sibling")).unwrap() == sibling,
        "a sibling file changed"
    );

    // Again, with the empty `-wal` now in place: nothing is created, and
    // the `-wal` that existed is left exactly as it was.
    let wal_only = |directory: &Path| {
        let mut census = lstat_census(directory);
        census.retain(|path, _| path == Path::new("state.db-wal"));
        census
    };
    let wal_before = wal_only(&source);
    let census_second = census_without(&source, &["state.db-shm"]);
    let watch = Watch::start(&[&source]);
    let counters = Counters::snapshot();
    provider_sqlite::snapshot(&db, &output_dir.join("again.db"), 10_000).unwrap();
    let again = Counters::snapshot().since(counters);
    let written_again = watch.drain();
    assert_eq!(
        (
            again.get(Counter::SourceWalIndexTouched),
            again.get(Counter::SourceWalCreated)
        ),
        (1, 0),
        "a -wal that existed is not counted as created"
    );
    assert_eq!(wal_before.len(), 1);
    assert_eq!(wal_only(&source), wal_before, "the existing -wal moved");
    assert_eq!(fs::symlink_metadata(&wal).unwrap().len(), 0);
    assert_eq!(
        census_without(&source, &["state.db-shm"]),
        census_second,
        "the second snapshot changed the source outside the -shm"
    );
    let wal_closed = WriteEvent {
        path: wal,
        what: "close-write".to_owned(),
    };
    let outside: Vec<&WriteEvent> = written_again
        .iter()
        .filter(|event| {
            event.path != shm && !(RESIDUAL_WAL_OPEN_READ_WRITE && **event == wal_closed)
        })
        .collect();
    assert_eq!(
        outside,
        Vec::<&WriteEvent>::new(),
        "write events outside the -shm on the second snapshot"
    );
    for name in ["state.db", "again.db"] {
        let snapshot = rusqlite::Connection::open(output_dir.join(name)).unwrap();
        let rows: i64 = snapshot
            .query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2000, "{name}");
    }
    eprintln!(
        "p77 sqlite empty -wal (Q72): first snapshot source_wal_index_touched=+{} \
         source_wal_created=+{}, -wal events {on_wal:?}, -wal 0 bytes, db-read-lock={} \
         shm-write-open={} wal-write-open={} over {} samples; second snapshot \
         source_wal_created=+{}, existing -wal lstat-identical, its events {:?} \
         (close-write is the #157 residual)",
        counted.get(Counter::SourceWalIndexTouched),
        counted.get(Counter::SourceWalCreated),
        accounting.db_read,
        accounting.shm_open,
        accounting.wal_open,
        observed.samples,
        again.get(Counter::SourceWalCreated),
        written_again
            .iter()
            .filter(|event| event.path == wal_closed.path)
            .map(|event| event.what.as_str())
            .collect::<BTreeSet<_>>()
    );
}

/// P77, `SQLite` leg with a live writer: the same backup while the writer
/// process commits a row every few milliseconds with no busy timeout.
///
/// - The writer is never refused (`SQLITE_BUSY`) and never waits: no blocked
///   lock line on the database, the `-wal` or the `-shm`, and its slowest
///   commit is within [`COMMIT_BOUND`].
/// - Its commits go on during the snapshot (the commit count moves).
/// - This process's locks fall under the exception, as above.
/// - The backup writes nothing: the database is byte-identical (the writer
///   never checkpoints, so any change is the snapshot's), no file appears in
///   the directory, and the write watch sees events only on the `-wal` and
///   the `-shm`, which the writer itself is writing.
///
/// A snapshot that loses a race with a commit refuses with a typed value and
/// is retried on a new output path, as the provider's contract says.
///
/// As root it is the refusal leg instead (OI-1003-Q76): the five verbs are
/// refused beside the committing writer, which is never found busy.
#[test]
// One linear scenario, as above.
#[allow(clippy::too_many_lines)]
fn a_sqlite_snapshot_does_not_hold_up_a_committing_writer() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    if sqlite_leg("live writer") {
        sqlite_source_read_is_refused_as_root(RootSource::LiveWriter);
        return;
    }
    let scratch = Scratch::new("sqlite-live");
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    let sibling = noise(9, 8192);
    fs::write(source.join("sibling"), &sibling).unwrap();
    let db = source.join("state.db");
    let writer = Writer::start(&db, true);
    let wal = source.join("state.db-wal");
    let shm = source.join("state.db-shm");
    let db_bytes = fs::read(&db).unwrap();
    let names = listing(&source);
    let census = census_without(&source, &["state.db-shm", "state.db-wal"]);

    let paths = lock_keys(&source);
    let key_of = |path: &Path| lock_key(&fs::symlink_metadata(path).unwrap());
    let keys = DatabaseKeys {
        db: key_of(&db),
        wal: key_of(&wal),
        shm: key_of(&shm),
    };
    let skip: BTreeSet<PathBuf> = ["state.db", "state.db-wal", "state.db-shm"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let holder = Holder::take(&source, &skip);
    let watch = Watch::start(&[&source]);
    let sampler = Sampler::start(
        vec![source.clone()],
        paths.keys().cloned().collect(),
        holder.shape(),
    );

    let output_dir = scratch.path("snapshot");
    fs::create_dir(&output_dir).unwrap();
    fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700)).unwrap();
    // Until one snapshot succeeds, overlaps a commit and is seen by the
    // sampler holding a lock (a short backup can fall between two samples).
    let mut attempts = Vec::new();
    let mut overlapped = None;
    for attempt in 0..LIVE_ATTEMPTS {
        let output = output_dir.join(format!("state-{attempt}.db"));
        let before = writer.commits();
        let result = provider_sqlite::snapshot(&db, &output, 10_000);
        let during = writer.commits() - before;
        let locked = sampler.own_lock_samples();
        attempts.push(format!(
            "{result:?} during {during} commits, {locked} samples with our lock so far"
        ));
        if result.is_ok() && during > 0 && locked > 0 {
            overlapped = Some(output);
            break;
        }
    }
    let left = our_locks(&paths, &holder.shape());
    let written = watch.drain();
    drop(holder);
    let observed = sampler.finish();
    let db_unchanged = fs::read(&db).unwrap() == db_bytes;
    let sibling_unchanged = fs::read(source.join("sibling")).unwrap() == sibling;
    let names_after = listing(&source);
    let census_after = census_without(&source, &["state.db-shm", "state.db-wal"]);
    let report = writer.finish();

    let output = overlapped.unwrap_or_else(|| {
        panic!("no snapshot overlapped a commit, succeeded and was sampled: {attempts:#?}")
    });
    let accounting = account(&observed, &keys, &paths, &shm, &wal, true);
    assert!(
        accounting.breaches.is_empty(),
        "source access outside the Q16/Q36 exception, or the writer held up: {:#?}",
        accounting.breaches
    );
    assert!(accounting.db_read > 0, "{accounting:?}");
    assert_eq!(
        left,
        [],
        "locks left on the source after the snapshots returned"
    );
    assert_eq!(
        report.busy, 0,
        "the writer found the database busy: {report:?}"
    );
    assert!(
        report.slowest <= COMMIT_BOUND,
        "a commit took {:?}, over the bound {COMMIT_BOUND:?}",
        report.slowest
    );
    let outside: Vec<&WriteEvent> = written
        .iter()
        .filter(|event| event.path != shm && event.path != wal)
        .collect();
    assert_eq!(
        outside,
        Vec::<&WriteEvent>::new(),
        "write events outside the -wal and the -shm"
    );
    assert_eq!(names_after, names, "the source directory's listing changed");
    assert_eq!(
        census_after, census,
        "the source changed outside the -wal and the -shm"
    );
    assert!(
        db_unchanged,
        "the database changed under a writer that never checkpoints"
    );
    assert!(sibling_unchanged, "the snapshot changed a sibling file");
    let snapshot = rusqlite::Connection::open(&output).unwrap();
    let rows: i64 = snapshot
        .query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
        .unwrap();
    assert!(rows >= 2000, "the snapshot holds {rows} rows");
    eprintln!(
        "p77 sqlite live writer: {} attempt(s) {attempts:?}; writer commits={} busy={} \
         slowest commit {:?} (bound {COMMIT_BOUND:?}); db-read-lock={} shm-read-lock={} \
         shm-read-mark-write={} over {} samples, mean period {} us",
        attempts.len(),
        report.commits,
        report.busy,
        report.slowest,
        accounting.db_read,
        accounting.shm_read,
        accounting.shm_mark,
        observed.samples,
        observed.period_us()
    );
}
