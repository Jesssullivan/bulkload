//! Tests for the `sys` wrappers. Linux-only paths (`renameat2`,
//! `fdatasync`) are compiled and run on the Linux CI
//! runner; Darwin-only paths (`F_BARRIERFSYNC`, `F_FULLFSYNC`,
//! `renameatx_np`, `QoS`) run on Darwin.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::ffi::CString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use super::{sys, OpenMode};

fn c(name: &str) -> CString {
    CString::new(name).unwrap()
}

fn tree() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("a/b")).unwrap();
    fs::write(dir.path().join("a/b/file"), b"payload").unwrap();
    dir
}

#[test]
fn openat_beneath_reads_a_nested_file() {
    let dir = tree();
    let root = sys::open_root(dir.path()).unwrap();
    for rel in ["a/b/file", "./a/b/file", "a/./b/file"] {
        let fd = sys::openat_beneath(&root, Path::new(rel), OpenMode::Read).unwrap();
        let mut buf = [0_u8; 16];
        let read = sys::pread_full(&fd, &mut buf, 0).unwrap();
        assert_eq!(&buf[..read], b"payload", "{rel}");
    }
    let sub = sys::openat_beneath(&root, Path::new("a/b"), OpenMode::Directory).unwrap();
    assert!(sys::fstat(&sub).unwrap().is_dir());
}

#[test]
fn openat_beneath_refuses_symlinks_at_every_component() {
    let dir = tree();
    let outside = tree();
    std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("alias")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("a/b/file"), dir.path().join("a/b/last")).unwrap();
    let root = sys::open_root(dir.path()).unwrap();

    // Intermediate symlink.
    assert!(sys::openat_beneath(&root, Path::new("alias/b/file"), OpenMode::Read).is_err());
    // Terminal symlink.
    assert!(sys::openat_beneath(&root, Path::new("a/b/last"), OpenMode::Read).is_err());

    // A live swap of an intermediate directory for a symlink that points
    // outside the root cannot carry the open out of the root.
    fs::rename(dir.path().join("a"), dir.path().join("a.moved")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("a"), dir.path().join("a")).unwrap();
    let escaped = sys::openat_beneath(&root, Path::new("a/b/file"), OpenMode::Read);
    let error = escaped.expect_err("an intermediate symlink must not be followed");
    assert!(
        matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)),
        "unexpected error {error:?}"
    );
}

#[test]
fn openat_beneath_rejects_paths_that_leave_the_root() {
    let dir = tree();
    let root = sys::open_root(dir.path()).unwrap();
    for rel in ["", ".", "../a", "a/../a/b/file", "/a/b/file"] {
        let error = sys::openat_beneath(&root, Path::new(rel), OpenMode::Read).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{rel}");
    }
}

#[test]
fn create_excl_never_reuses_a_name() {
    let dir = tree();
    let root = sys::open_root(dir.path()).unwrap();
    let fd = sys::openat_beneath(&root, Path::new("a/new"), OpenMode::CreateExcl(0o600)).unwrap();
    let stat = sys::fstat(&fd).unwrap();
    assert!(stat.is_file());
    assert_eq!(stat.permissions(), 0o600);
    let again = sys::openat_beneath(&root, Path::new("a/new"), OpenMode::CreateExcl(0o600));
    assert_eq!(again.unwrap_err().kind(), ErrorKind::AlreadyExists);
    let over = sys::openat_beneath(&root, Path::new("a/b/file"), OpenMode::CreateExcl(0o600));
    assert_eq!(over.unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert_eq!(fs::read(dir.path().join("a/b/file")).unwrap(), b"payload");
}

#[test]
fn fstat_identity_matches_std_metadata() {
    let dir = tree();
    let root = sys::open_root(dir.path()).unwrap();
    let fd = sys::openat_beneath(&root, Path::new("a/b/file"), OpenMode::Read).unwrap();
    let stat = sys::fstat(&fd).unwrap();
    let meta = fs::metadata(dir.path().join("a/b/file")).unwrap();
    assert_eq!(stat.node.dev, meta.dev());
    assert_eq!(stat.node.ino, meta.ino());
    assert_eq!(stat.size, 7);
    assert_eq!(stat.nlink, 1);
    assert_eq!(stat.mode, meta.mode());
    let at = sys::fstatat_nofollow(&root, &c("a")).unwrap();
    assert!(at.is_dir());
}

#[test]
fn pwrite_and_pread_round_trip_across_offsets_and_short_reads() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
    let big: Vec<u8> = (0..3 * 1024 * 1024 + 17)
        .map(|i: u32| u8::try_from(i % 251).unwrap())
        .collect();
    sys::pwrite_all(&fd, &big, 4096).unwrap();
    assert_eq!(sys::fstat(&fd).unwrap().size, 4096 + big.len() as u64);

    let mut back = vec![0xff_u8; big.len() + 4096 + 100];
    let read = sys::pread_full(&fd, &mut back, 0).unwrap();
    assert_eq!(
        read,
        big.len() + 4096,
        "a read past the end is short, not an error"
    );
    assert!(
        back[..4096].iter().all(|byte| *byte == 0),
        "the hole reads as zeros"
    );
    assert_eq!(&back[4096..read], &big[..]);

    // A live truncate turns into a short read, never a signal.
    fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("f"))
        .unwrap()
        .set_len(10_000)
        .unwrap();
    let read = sys::pread_full(&fd, &mut back, 0).unwrap();
    assert_eq!(read, 10_000);
    let read = sys::pread_full(&fd, &mut back, 1 << 30).unwrap();
    assert_eq!(read, 0);

    let too_far = sys::pwrite_all(&fd, b"x", u64::MAX);
    assert_eq!(too_far.unwrap_err().kind(), ErrorKind::InvalidInput);
}

#[test]
fn namespace_calls_create_link_rename_and_remove() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    sys::mkdirat(&root, &c("d"), 0o700).unwrap();
    assert_eq!(
        sys::mkdirat(&root, &c("d"), 0o700).unwrap_err().kind(),
        ErrorKind::AlreadyExists
    );
    let d = sys::openat_beneath(&root, Path::new("d"), OpenMode::Directory).unwrap();

    let fd = sys::openat_beneath(&root, Path::new("d/one"), OpenMode::CreateExcl(0o644)).unwrap();
    sys::pwrite_all(&fd, b"one", 0).unwrap();
    sys::fchmod(&fd, 0o640).unwrap();
    assert_eq!(sys::fstat(&fd).unwrap().permissions(), 0o640);

    sys::linkat(&d, &c("one"), &root, &c("two")).unwrap();
    assert_eq!(sys::fstat(&fd).unwrap().nlink, 2);
    assert_eq!(
        sys::linkat(&d, &c("one"), &root, &c("two"))
            .unwrap_err()
            .kind(),
        ErrorKind::AlreadyExists
    );

    // No-clobber rename: an occupied target is EEXIST and keeps its bytes.
    fs::write(dir.path().join("d/taken"), b"keep").unwrap();
    let clobber = sys::rename_exclusive_at(&d, &c("one"), &d, &c("taken"));
    assert_eq!(clobber.unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert_eq!(fs::read(dir.path().join("d/taken")).unwrap(), b"keep");
    sys::rename_exclusive_at(&d, &c("one"), &d, &c("three")).unwrap();
    assert_eq!(fs::read(dir.path().join("d/three")).unwrap(), b"one");
    sys::rename_exclusive_at(&d, &c("three"), &root, &c("four")).unwrap();
    assert_eq!(fs::read(dir.path().join("four")).unwrap(), b"one");

    sys::unlinkat(&root, &c("four"), false).unwrap();
    sys::unlinkat(&root, &c("two"), false).unwrap();
    sys::unlinkat(&d, &c("taken"), false).unwrap();
    sys::unlinkat(&root, &c("d"), true).unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// PR #59 round 5, D2: under the rename-unsupported hook the bare
/// exclusive rename reports `EINVAL`, which `rename_unsupported` recognizes,
/// and makes no link: both names stay as they were.
#[test]
fn an_unsupported_exclusive_rename_reports_einval_and_links_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(dir.path().join("a"), b"x").unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    crate::io::force_rename_unsupported(true);
    let result = sys::rename_exclusive(&root, &c("a"), &c("b"));
    crate::io::force_rename_unsupported(false);
    let error = result.unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    assert!(crate::io::rename_unsupported(&error));
    assert!(dir.path().join("a").exists());
    assert!(!dir.path().join("b").exists(), "no link was made");
    sys::rename_exclusive(&root, &c("a"), &c("b")).unwrap();
    assert!(dir.path().join("b").exists(), "without the hook it renames");
}

#[test]
fn every_sync_kind_succeeds_on_files_and_directories() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
    sys::pwrite_all(&fd, &vec![7_u8; 65_536], 0).unwrap();
    sys::barrier(&fd).unwrap();
    sys::full_flush(&fd).unwrap();
    sys::barrier_dir(&root).unwrap();
    sys::full_flush(&root).unwrap();
    #[cfg(target_os = "linux")]
    sys::data_sync(&fd).unwrap();
}

// WP0(f): background priority is entered on one thread and inherited by
// every thread and child it creates afterwards. Run on a fresh thread, so the
// test harness's own threads keep their class (Linux keeps nice and the IO
// class per thread).
#[test]
fn background_priority_is_inherited_by_threads_and_children() {
    std::thread::spawn(|| {
        sys::enter_background().unwrap();
        assert!(sys::in_background().unwrap());
        assert!(std::thread::spawn(|| sys::in_background().unwrap())
            .join()
            .unwrap());
        // A child process inherits the lowered nice value: Linux reports a
        // process's own nice as field 19 of `/proc/self/stat`, Darwin's `ps`
        // reports the shell's.
        #[cfg(target_os = "linux")]
        let child = std::process::Command::new("cat")
            .arg("/proc/self/stat")
            .output()
            .unwrap();
        #[cfg(target_vendor = "apple")]
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "exec ps -o nice= -p $$"])
            .output()
            .unwrap();
        assert!(child.status.success());
        let report = String::from_utf8(child.stdout).unwrap();
        #[cfg(target_os = "linux")]
        let nice = report
            .rsplit_once(") ")
            .unwrap()
            .1
            .split(' ')
            .nth(16)
            .unwrap()
            .to_owned();
        #[cfg(target_vendor = "apple")]
        let nice = report.trim().to_owned();
        assert_eq!(nice, sys::BACKGROUND_NICE.to_string());
    })
    .join()
    .unwrap();
}

#[cfg(feature = "io-trace")]
mod traced {
    use super::*;
    use crate::io::trace::recorder::Recorder;
    use crate::io::trace::{Event, SyncKind};

    /// Set only by `just rust-check`'s isolated partial-write step.
    const PARTIAL_WRITE_ALONE: &str = "BULKLOAD_IO_PARTIAL_WRITE_ALONE";

    #[test]
    fn mutating_calls_record_events_only_while_attached() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let recorder = Recorder::new();

        // Not attached: nothing recorded.
        let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
        sys::pwrite_all(&fd, b"unseen", 0).unwrap();
        assert!(recorder.take().is_empty());

        let guard = recorder.attach();
        sys::pwrite_all(&fd, b"seen", 6).unwrap();
        sys::fchmod(&fd, 0o640).unwrap();
        sys::full_flush(&fd).unwrap();
        sys::mkdirat(&root, &c("d"), 0o700).unwrap();
        sys::rename_exclusive_at(&root, &c("f"), &root, &c("g")).unwrap();
        sys::linkat(&root, &c("g"), &root, &c("h")).unwrap();
        sys::unlinkat(&root, &c("h"), false).unwrap();
        let mut read = [0_u8; 4];
        sys::pread_full(&fd, &mut read, 0).unwrap();
        drop(guard);
        sys::pwrite_all(&fd, b"after", 0).unwrap();

        let events = recorder.take();
        let node = sys::fstat(&fd).unwrap().node;
        let root_node = sys::fstat(&root).unwrap().node;
        assert_eq!(events.len(), 7, "{events:#?}");
        assert_eq!(
            events[0],
            Event::Write {
                node,
                offset: 6,
                data: b"seen".to_vec(),
                digest: *blake3::hash(b"seen").as_bytes(),
            }
        );
        assert_eq!(events[1], Event::SetMode { node, mode: 0o640 });
        let flush = if cfg!(target_vendor = "apple") {
            SyncKind::FullFlush
        } else {
            SyncKind::Fsync
        };
        assert_eq!(events[2], Event::Sync { node, kind: flush });
        assert!(
            matches!(&events[3], Event::Mkdir { dir, name, .. } if *dir == root_node && name == b"d")
        );
        assert_eq!(
            events[4],
            Event::Rename {
                node,
                from_dir: root_node,
                from: b"f".to_vec(),
                to_dir: root_node,
                to: b"g".to_vec(),
            }
        );
        assert_eq!(
            events[5],
            Event::Link {
                node,
                dir: root_node,
                name: b"h".to_vec(),
            }
        );
        assert_eq!(
            events[6],
            Event::Unlink {
                dir: root_node,
                name: b"h".to_vec(),
            }
        );
    }

    /// Review #12: Create and Mkdir events carry the effective mode (after
    /// the umask), as `fstat` reports it, not the requested bits.
    #[test]
    fn create_and_mkdir_events_record_the_effective_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let recorder = Recorder::new();
        let attached = recorder.attach();
        let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o777)).unwrap();
        sys::mkdirat(&root, &c("d"), 0o777).unwrap();
        drop(attached);
        let file_mode = sys::fstat(&fd).unwrap().permissions();
        let dir_mode = sys::fstatat_nofollow(&root, &c("d")).unwrap().permissions();
        let modes: Vec<u32> = recorder
            .take()
            .iter()
            .filter_map(|event| match event {
                Event::Create { mode, .. } | Event::Mkdir { mode, .. } => Some(*mode),
                _ => None,
            })
            .collect();
        assert_eq!(modes, vec![file_mode, dir_mode]);
    }

    /// Review r2 finding 1 (reviewer probe P3a): two threads race
    /// create-exclusive and unlink of one name. The kernel forces the
    /// successful Create and Unlink of that name to alternate, so any trace
    /// order that differs from the syscall order shows up as two Creates or
    /// two Unlinks in a row, at any step. Without the serial lock this finds
    /// order violations in every run (reviewer: 30/72/54 over 3 runs).
    #[test]
    fn create_and_unlink_of_one_name_alternate_in_the_trace() {
        const ROUNDS: usize = 20_000;
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let recorder = Recorder::new();
        let start = std::sync::Barrier::new(2);
        let wins = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let (recorder, root, start, wins) = (&recorder, &root, &start, &wins);
                scope.spawn(move || {
                    let _attached = recorder.attach();
                    start.wait();
                    for _ in 0..ROUNDS {
                        match sys::openat_beneath(root, Path::new("x"), OpenMode::CreateExcl(0o600))
                        {
                            Ok(fd) => {
                                wins.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                drop(fd);
                                sys::unlinkat(root, &c("x"), false).unwrap();
                            }
                            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                            Err(error) => panic!("{error}"),
                        }
                    }
                });
            }
        });
        let events = recorder.take();
        let mut present = false;
        let mut violations = Vec::new();
        for (index, event) in events.iter().enumerate() {
            match event {
                Event::Create {
                    name: Some(name), ..
                } if name == b"x" => {
                    if present {
                        violations.push(index);
                    }
                    present = true;
                }
                Event::Unlink { name, .. } if name == b"x" => {
                    if !present {
                        violations.push(index);
                    }
                    present = false;
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        let wins = wins.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(events.len(), 2 * wins);
        assert!(wins > 0, "no create ever succeeded");
        assert!(
            violations.is_empty(),
            "trace order differs from syscall order at events {:?} of {}",
            violations.iter().take(8).collect::<Vec<_>>(),
            events.len()
        );
    }

    /// Review r2 finding 1 (reviewer probe P3b): each round, thread A writes a
    /// 1 MiB block (its event build copies and hashes it: a long window) and
    /// thread B writes one byte at a jittered point inside that window. After
    /// every round the file on disk is compared with the replay of the trace
    /// so far, so each round is an independent chance to catch a misordered
    /// pair. Without the serial lock this mismatches in most rounds
    /// (reviewer: 195/188/157 of 400).
    #[test]
    fn every_round_the_trace_replays_to_the_bytes_on_disk() {
        const ROUNDS: usize = 400;
        const BIG: usize = 1 << 20;
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
        let recorder = Recorder::new();
        let meet = std::sync::Barrier::new(3);
        let mut replay = vec![0_u8; BIG];
        let mut mismatches = Vec::new();
        std::thread::scope(|scope| {
            let (recorder_a, fd_a, meet_a) = (&recorder, &fd, &meet);
            scope.spawn(move || {
                let _attached = recorder_a.attach();
                for round in 0..ROUNDS {
                    let payload = vec![u8::try_from(round % 200).unwrap() + 1; BIG];
                    meet_a.wait();
                    sys::pwrite_all(fd_a, &payload, 0).unwrap();
                    meet_a.wait();
                }
            });
            let (recorder_b, fd_b, meet_b) = (&recorder, &fd, &meet);
            scope.spawn(move || {
                let _attached = recorder_b.attach();
                for round in 0..ROUNDS {
                    meet_b.wait();
                    let spin = std::time::Instant::now();
                    let delay = std::time::Duration::from_micros(
                        u64::try_from((round * 37) % 3000).unwrap(),
                    );
                    while spin.elapsed() < delay {
                        std::hint::spin_loop();
                    }
                    sys::pwrite_all(fd_b, &[0xEE], 0).unwrap();
                    meet_b.wait();
                }
            });
            for round in 0..ROUNDS {
                meet.wait();
                meet.wait();
                for event in recorder.take() {
                    let Event::Write { offset, data, .. } = event else {
                        panic!("unexpected event {event:?}");
                    };
                    let start = usize::try_from(offset).unwrap();
                    replay[start..start + data.len()].copy_from_slice(&data);
                }
                let mut disk = [0_u8; 1];
                sys::pread_full(&fd, &mut disk, 0).unwrap();
                if disk[0] != replay[0] {
                    mismatches.push(round);
                }
            }
        });
        assert!(
            mismatches.is_empty(),
            "trace replay differs from disk after rounds {:?} ({} of {ROUNDS})",
            mismatches.iter().take(8).collect::<Vec<_>>(),
            mismatches.len()
        );
    }

    /// Review r2 finding 2 (reviewer probe P5): a write the kernel accepts
    /// only in part is traced as exactly the accepted prefix. `RLIMIT_FSIZE`
    /// makes the kernel accept 10 000 bytes of a 20 000-byte write and then
    /// fail with `EFBIG`. The limit is process-wide, so this test is ignored
    /// in the normal run, and it acts only when `PARTIAL_WRITE_ALONE` is
    /// set, which only `just rust-check`'s isolated step (`--exact
    /// --test-threads=1`) sets. A `--include-ignored` run beside the other
    /// write tests therefore does nothing instead of failing them with
    /// spurious `EFBIG`.
    #[test]
    #[ignore = "sets the process-wide RLIMIT_FSIZE; just rust-check runs it alone"]
    fn partial_write_prefix_is_traced() {
        const LIMIT: u64 = 10_000;
        if std::env::var_os(PARTIAL_WRITE_ALONE).is_none() {
            println!("SKIPPED partial_write_prefix_is_traced: {PARTIAL_WRITE_ALONE} is unset");
            return;
        }
        let before = sys::FileSizeLimit::current().unwrap();
        let ignored_before = sys::FileSizeLimit::sigxfsz_ignored().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
        let payload: Vec<u8> = (0..20_000_u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let recorder = Recorder::new();
        let result = {
            let _limit = sys::FileSizeLimit::set(LIMIT).unwrap();
            assert_eq!(sys::FileSizeLimit::current().unwrap().0, LIMIT);
            assert!(sys::FileSizeLimit::sigxfsz_ignored().unwrap());
            let _attached = recorder.attach();
            sys::pwrite_all(&fd, &payload, 0)
        };
        // Review r3 note 2: the guard's drop restores the limit and the
        // SIGXFSZ disposition; an empty drop fails here.
        assert_eq!(sys::FileSizeLimit::current().unwrap(), before);
        assert_eq!(
            sys::FileSizeLimit::sigxfsz_ignored().unwrap(),
            ignored_before
        );
        let events = recorder.take();
        let on_disk = fs::read(dir.path().join("f")).unwrap();
        let error = result.expect_err("a write past RLIMIT_FSIZE must fail");
        assert_eq!(error.raw_os_error(), Some(libc::EFBIG), "{error}");
        assert_eq!(
            on_disk.len(),
            usize::try_from(LIMIT).unwrap(),
            "the kernel accepts the prefix"
        );
        assert_eq!(on_disk.as_slice(), &payload[..on_disk.len()]);
        assert_eq!(events.len(), 1, "the accepted prefix is traced: {events:?}");
        let Event::Write {
            offset,
            data,
            digest,
            ..
        } = &events[0]
        else {
            panic!("not a write: {:?}", events[0]);
        };
        assert_eq!(*offset, 0);
        assert_eq!(data.as_slice(), on_disk.as_slice());
        assert_eq!(*digest, *blake3::hash(&on_disk).as_bytes());
    }

    #[test]
    fn a_recorder_attached_on_two_threads_keeps_one_order() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let recorder = Recorder::new();
        let _guard = recorder.attach();
        let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
        sys::pwrite_all(&fd, b"main", 0).unwrap();
        let shared = recorder.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _guard = shared.attach();
                sys::barrier(&fd).unwrap();
            });
        });
        let events = recorder.take();
        assert_eq!(events.len(), 3, "{events:#?}");
        assert!(matches!(events[0], Event::Create { .. }));
        assert!(matches!(events[1], Event::Write { .. }));
        assert!(matches!(events[2], Event::Sync { .. }));
    }
}
