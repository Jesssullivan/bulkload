//! P77 SOURCE-LOCK-TRACE (S2: never interrupt the source; OI-1003-Q5,
//! OI-1003-Q9; the bounded `SQLite` exception OI-1003-Q16, OI-1003-Q36). A P34
//! sibling.
//!
//! The `io-trace` recorder (R-N88) records only the agent's own mutating
//! `io::sys` calls. It sees no `flock`, no `fcntl` lock, no open of any mode,
//! nothing a Git child does, and nothing `SQLite` does through its own VFS. So
//! this file does not extend it. It observes the kernel instead, with no
//! preload and no new crate:
//!
//! - **`/proc/locks`**, sampled in a tight loop while a verb runs, lists every
//!   `flock`, POSIX `fcntl` and OFD lock any process holds or waits for,
//!   keyed by device and inode. A line on a source inode is a source lock.
//! - **`/proc/self/fdinfo`**, sampled in the same loop, gives the access mode
//!   of every descriptor the (in-process) verb has open. A descriptor on a
//!   source path open for writing is a write-mode open.
//! - **A holder.** Sampling can miss a lock taken and dropped between two
//!   samples. So a second run first takes an exclusive `flock` and an
//!   exclusive whole-file OFD write lock on every source file (and an
//!   exclusive `flock` on every source directory), the way a live agent's
//!   writer would, and makes the tree read-only. Any lock the verb then
//!   tries conflicts: a blocking attempt waits (a `->` line in `/proc/locks`,
//!   and a verb that does not finish), and a non-blocking one fails (a
//!   refusal). Any write-mode open fails with `EACCES` (when not root).
//! - **An `lstat` census** of the whole source (every field but access time)
//!   is equal before and after.
//!
//! A sampler self-test proves both channels see a lock and a write-mode open
//! of a few milliseconds on this kernel. The `SQLite` leg runs the backup API
//! against a WAL database a separate writer process holds open, and counts
//! the locks and write-mode opens that fall under the Q16/Q36 exception (a
//! shared read lock on the database, any lock or open on its `-shm`); none
//! may fall anywhere else.
//!
//! What this cannot see: a non-blocking lock attempt whose failure the verb
//! ignores, taken during the holder run (nothing is held, so nothing
//! interrupts the source) and missed by sampling in the free run. Git
//! children are covered for locks (`/proc/locks` is system-wide) but not for
//! write-mode opens (their descriptors are in their own `/proc/<pid>`).
//! Linux only: Darwin has neither `/proc/locks` nor `fdinfo`.

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
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use bulkload_agent::transfer::{copy, settle_racy_window};

/// One test at a time: the samplers read process-wide tables.
static SERIAL: Mutex<()> = Mutex::new(());

/// How long a verb may run before it counts as blocked on a holder.
const DEADLINE: Duration = Duration::from_mins(2);

/// Found by this file (2026-10-06, bulkload#157): the backup API's
/// read-only connection holds the source's `-wal` open read-write
/// (`O_RDWR`), which Q16/Q36 do not name. The bytes stay identical (asserted
/// below), so it is counted and tracked rather than refused here. Set it to
/// `false` once the provider opens the `-wal` read-only, and the test then
/// refuses any write-mode open of it.
const WAL_OPENED_READ_WRITE: bool = true;

/// The environment variable that turns [`sqlite_writer_helper`] into the
/// live writer.
const WRITER_ENV: &str = "BULKLOAD_P77_WRITER_DB";

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
            Some(LockLine {
                blocked,
                kind,
                mode,
                pid,
                key,
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

/// Every descriptor of this process open for writing on a path beneath
/// `root`, except `ignore`'s.
fn write_opens(root: &Path, ignore: &BTreeSet<RawFd>) -> Vec<WriteOpen> {
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
        if !target.starts_with(root) {
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
    /// How long the sampler ran.
    elapsed: Duration,
}

impl Observed {
    /// The mean time between samples, in microseconds.
    fn period_us(&self) -> u128 {
        self.elapsed.as_micros() / u128::from(self.samples.max(1))
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
    thread: thread::JoinHandle<Observed>,
}

impl Sampler {
    /// Watch `keys` (lock keys) and paths beneath `root`, not reporting
    /// `held`'s own locks and descriptors.
    fn start(root: PathBuf, keys: BTreeSet<String>, held: Held) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let taken = Arc::new(AtomicU64::new(0));
        let counting = Arc::clone(&taken);
        let ours = std::process::id().to_string();
        let thread = thread::spawn(move || {
            let mut observed = Observed::default();
            let started = std::time::Instant::now();
            loop {
                let last = stopping.load(Ordering::Acquire);
                let seen: BTreeSet<LockLine> = read_locks()
                    .into_iter()
                    .filter(|line| keys.contains(&line.key) && !held.owns(line, &ours))
                    .collect();
                for line in seen {
                    *observed.locks.entry(line).or_default() += 1;
                }
                for open in write_opens(&root, &held.descriptors) {
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

    fn finish(self) -> Observed {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap()
    }
}

/// Run `verb` on its own thread. If it has not returned within
/// [`DEADLINE`], drop `holder` (so a waiting lock is granted and the verb can
/// finish) and report it blocked.
fn within_deadline<T: Send + 'static>(
    holder: Option<Holder>,
    verb: impl FnOnce() -> T + Send + 'static,
) -> (T, bool) {
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = verb();
        let _ = done.send(());
        result
    });
    let blocked = finished.recv_timeout(DEADLINE).is_err();
    drop(holder);
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
/// open of a few milliseconds are seen, and a holder's own lines and
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
    let sampler = Sampler::start(source.clone(), keys, holder.shape());
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
    let sampler = Sampler::start(source.clone(), keys, Held::default());
    let stats = copy(
        &source,
        &destination,
        &scratch.path("source-state"),
        &scratch.path("destination-state"),
    )
    .unwrap();
    let observed = sampler.finish();
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
    let sampler = Sampler::start(source.clone(), keys, holder.shape());
    let (stats, blocked) = {
        let (source, destination) = (source.clone(), destination.clone());
        let (source_state, destination_state) = (
            scratch.path("source-state"),
            scratch.path("destination-state"),
        );
        within_deadline(Some(holder), move || {
            copy(&source, &destination, &source_state, &destination_state)
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
    eprintln!(
        "p77 copy holder run: {} samples, mean period {} us, read-only enforced: {}",
        observed.samples,
        observed.period_us(),
        !is_root()
    );
    assert_carried(&source, &destination);
    assert_eq!(lstat_census(&source), census, "the copy changed the source");
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

/// P77, Git leg: with every node of a repository (worktree and `.git`)
/// held under exclusive locks, a v1 capture of it finishes, no process
/// (the agent or any Git child) takes or waits for a lock on any of its
/// inodes, and the repository's `lstat` census is unchanged.
#[test]
fn a_git_capture_never_waits_on_a_source_lock_holder() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("git-held");
    let repo = scratch.path("repo");
    fs::create_dir(&repo).unwrap();
    fixture_git(&repo, &["init", "--quiet", "--template=", "-b", "main"]);
    populate(&repo);
    fixture_git(&repo, &["add", "."]);
    fixture_git(&repo, &["commit", "--quiet", "-m", "one"]);
    fs::write(repo.join("b"), b"staged\n").unwrap();
    fixture_git(&repo, &["add", "b"]);
    fs::write(repo.join("b"), b"worktree\n").unwrap();
    fs::write(repo.join("untracked"), b"untracked\n").unwrap();
    let holder = Holder::take(&repo, &BTreeSet::new());
    settle_racy_window(&repo).unwrap();
    let census = lstat_census(&repo);
    let keys: BTreeSet<String> = lock_keys(&repo).into_keys().collect();
    let sampler = Sampler::start(repo.clone(), keys, holder.shape());
    let (bundle, blocked) = {
        let (repo, capture) = (repo.clone(), scratch.path("capture"));
        within_deadline(Some(holder), move || {
            bulkload_agent::git_carry::export_repository(&repo, &capture)
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

/// The live writer for the `SQLite` leg, run as a separate process: with
/// [`WRITER_ENV`] set, it opens the database in WAL mode, commits rows that
/// stay in the `-wal` (no checkpoint), prints a ready line, and holds the
/// connection open until its stdin closes. Without it this test does
/// nothing.
#[test]
fn sqlite_writer_helper() {
    let Some(db) = std::env::var_os(WRITER_ENV) else {
        return;
    };
    let connection = rusqlite::Connection::open(&db).unwrap();
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
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    drop(connection);
}

/// P77, `SQLite` leg (OI-1003-Q16, OI-1003-Q36): the backup API against a WAL
/// database a separate writer holds open. This process's locks on source
/// inodes fall only under the exception (a shared READ lock on the database;
/// anything on its `-shm`), and so do its write-mode opens (the `-shm`
/// only). Nothing else in the source directory is locked or opened for
/// writing, the database and its `-wal` are byte-identical before and after,
/// and the exception is counted.
#[test]
// One linear scenario: writer, holder, snapshot, then the accounting.
#[allow(clippy::too_many_lines)]
fn a_sqlite_snapshot_locks_only_under_the_q16_q36_exception() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let scratch = Scratch::new("sqlite");
    let source = scratch.path("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sibling"), noise(9, 8192)).unwrap();
    let db = source.join("state.db");

    let mut writer = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sqlite_writer_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(WRITER_ENV, &db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // libtest prints `test NAME ... ` before the test's own output, so the
    // ready marker ends a line rather than being one. The reader drains the
    // writer's stdout to its end, so the writer never blocks on it.
    let (ready, became_ready) = mpsc::channel();
    let stdout = writer.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.ends_with("p77-writer-ready") {
                let _ = ready.send(());
            }
        }
    });
    if became_ready.recv_timeout(DEADLINE).is_err() {
        // Closing its stdin ends the writer whatever state it is in.
        drop(writer.stdin.take());
        let _ = writer.wait();
        panic!("the writer never became ready");
    }
    let wal = source.join("state.db-wal");
    let shm = source.join("state.db-shm");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "the rows are in the -wal"
    );
    let db_bytes = fs::read(&db).unwrap();
    let wal_bytes = fs::read(&wal).unwrap();

    let keys = lock_keys(&source);
    let key_of = |path: &Path| lock_key(&fs::symlink_metadata(path).unwrap());
    let (db_key, wal_key, shm_key) = (key_of(&db), key_of(&wal), key_of(&shm));
    let skip: BTreeSet<PathBuf> = ["state.db", "state.db-wal", "state.db-shm"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let holder = Holder::take(&source, &skip);
    let watched: BTreeSet<String> = keys.keys().cloned().collect();
    let sampler = Sampler::start(source, watched, holder.shape());

    let output_dir = scratch.path("snapshot");
    fs::create_dir(&output_dir).unwrap();
    fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let output = output_dir.join("state.db");
    let (result, blocked) = {
        let (db, output) = (db.clone(), output.clone());
        within_deadline(Some(holder), move || {
            bulkload_agent::provider_sqlite::snapshot(&db, &output, 10_000)
        })
    };
    let observed = sampler.finish();
    // Compared while the writer still holds the database open: its own
    // close checkpoints the `-wal` into the database, which is the writer's
    // write, not ours.
    let db_unchanged = fs::read(&db).unwrap() == db_bytes;
    let wal_unchanged = fs::read(&wal).unwrap() == wal_bytes;

    drop(writer.stdin.take());
    reader.join().unwrap();
    assert!(writer.wait().unwrap().success(), "the writer failed");

    assert!(!blocked, "the snapshot waited: {:?}", observed.locks);
    result.unwrap();
    let ours = std::process::id().to_string();
    let mut counted = BTreeMap::new();
    let mut breaches = Vec::new();
    for (line, samples) in &observed.locks {
        if line.pid != ours {
            // The writer's own locks, or a holder's waiter, never ours.
            if line.key != db_key && line.key != wal_key && line.key != shm_key {
                breaches.push(format!("{line:?} (another process, a non-database inode)"));
            }
            continue;
        }
        let allowed =
            (line.key == db_key && line.mode == "READ" && !line.blocked) || line.key == shm_key;
        if allowed {
            let name = if line.key == db_key { "db-read" } else { "shm" };
            *counted.entry(name).or_insert(0_u64) += samples;
        } else {
            breaches.push(format!("{line:?} on {:?}", keys.get(&line.key)));
        }
    }
    let mut shm_opens = 0_u64;
    let mut wal_opens = 0_u64;
    for (open, samples) in &observed.opens {
        if open.path == shm {
            shm_opens += samples;
        } else if open.path == wal && WAL_OPENED_READ_WRITE {
            wal_opens += samples;
        } else {
            breaches.push(format!("write-mode open {open:?}"));
        }
    }
    assert!(
        breaches.is_empty(),
        "source access outside the Q16/Q36 exception: {breaches:#?}"
    );
    assert!(db_unchanged, "the snapshot changed the database");
    assert!(wal_unchanged, "the snapshot changed the -wal");
    let snapshot = rusqlite::Connection::open(&output).unwrap();
    let rows: i64 = snapshot
        .query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2000, "the snapshot holds the -wal's rows");
    eprintln!(
        "p77 sqlite exception (sample counts, Q16/Q36): {} samples, mean period {} us, \
         db-read-lock={} shm-lock={} shm-write-open={} wal-write-open={} (#157)",
        observed.samples,
        observed.period_us(),
        counted.get("db-read").copied().unwrap_or(0),
        counted.get("shm").copied().unwrap_or(0),
        shm_opens,
        wal_opens
    );
}
