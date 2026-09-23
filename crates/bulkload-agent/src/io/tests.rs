//! Tests for the `sys` wrappers. Linux-only paths (`O_TMPFILE`, `renameat2`,
//! `fdatasync`, `sync_file_range`) are compiled and run on the Linux CI
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
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{sys, OpenMode, TempFile};

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
    let clobber = sys::rename_noreplace(&d, &c("one"), &c("taken"));
    assert_eq!(clobber.unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert_eq!(fs::read(dir.path().join("d/taken")).unwrap(), b"keep");
    sys::rename_noreplace(&d, &c("one"), &c("three")).unwrap();
    assert_eq!(fs::read(dir.path().join("d/three")).unwrap(), b"one");
    sys::rename_noreplace_at(&d, &c("three"), &root, &c("four")).unwrap();
    assert_eq!(fs::read(dir.path().join("four")).unwrap(), b"one");

    sys::unlinkat(&root, &c("four"), false).unwrap();
    sys::unlinkat(&root, &c("two"), false).unwrap();
    sys::unlinkat(&d, &c("taken"), false).unwrap();
    sys::unlinkat(&root, &c("d"), true).unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn every_sync_kind_succeeds_on_files_and_directories() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
    sys::pwrite_all(&fd, &vec![7_u8; 65_536], 0).unwrap();
    sys::kick(&fd, 0, 65_536).unwrap();
    sys::barrier(&fd).unwrap();
    sys::full_flush(&fd).unwrap();
    sys::barrier_dir(&root).unwrap();
    sys::full_flush(&root).unwrap();
    #[cfg(target_os = "linux")]
    sys::data_sync(&fd).unwrap();
}

#[test]
fn preallocation_and_read_advice_keep_the_size() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
    let reserved = sys::preallocate(&fd, 8 * 1024 * 1024).unwrap();
    eprintln!("preallocate reserved={reserved}");
    assert_eq!(sys::fstat(&fd).unwrap().size, 0, "KEEP_SIZE semantics");
    sys::pwrite_all(&fd, b"tail", 1024).unwrap();
    sys::read_advise(&fd, 0, 1 << 40).unwrap();
    assert_eq!(sys::fstat(&fd).unwrap().size, 1028);
}

#[test]
fn temp_files_publish_without_clobbering() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    let temp = TempFile::create(&root, 0o600).unwrap();
    eprintln!("staged anonymous={}", temp.is_anonymous());
    if temp.is_anonymous() {
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            0,
            "O_TMPFILE has no name"
        );
    }
    sys::pwrite_all(temp.fd(), b"staged", 0).unwrap();
    sys::barrier(temp.fd()).unwrap();
    temp.publish(&c("final")).unwrap();
    sys::barrier_dir(&root).unwrap();
    assert_eq!(fs::read(dir.path().join("final")).unwrap(), b"staged");

    let second = TempFile::create(&root, 0o600).unwrap();
    sys::pwrite_all(second.fd(), b"other", 0).unwrap();
    let clobber = second.publish(&c("final")).unwrap_err();
    assert_eq!(clobber.error.kind(), ErrorKind::AlreadyExists);
    // The staged file comes back and can still be published elsewhere.
    clobber.temp.publish(&c("final.2")).unwrap();
    assert_eq!(fs::read(dir.path().join("final.2")).unwrap(), b"other");
    assert_eq!(fs::read(dir.path().join("final")).unwrap(), b"staged");
}

#[test]
fn named_temp_fallback_is_private_and_exclusive() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    let (fd, name) = sys::create_temp_named(&root, 0o600).unwrap();
    let stat = sys::fstatat_nofollow(&root, &name).unwrap();
    assert_eq!(stat.node, sys::fstat(&fd).unwrap().node);
    assert_eq!(stat.permissions(), 0o600);
    let (_, other) = sys::create_temp_named(&root, 0o600).unwrap();
    assert_ne!(name, other);
}

/// A scratch directory on tmpfs (`/dev/shm`), where Linux supports
/// `O_TMPFILE`. Tests on it must run in CI, so a missing or unwritable
/// `/dev/shm` fails the test; `BULKLOAD_ALLOW_NO_DEV_SHM=1` turns that into a
/// printed skip for hosts that really have none.
#[cfg(target_os = "linux")]
pub fn shm_dir(test: &str) -> Option<tempfile::TempDir> {
    match tempfile::TempDir::new_in("/dev/shm") {
        Ok(dir) => {
            println!("RAN {test}: O_TMPFILE staging under /dev/shm");
            Some(dir)
        }
        Err(error) if std::env::var_os("BULKLOAD_ALLOW_NO_DEV_SHM").is_some() => {
            println!(
                "SKIPPED {test}: /dev/shm unusable ({error}); allowed by BULKLOAD_ALLOW_NO_DEV_SHM"
            );
            None
        }
        Err(error) => panic!("{test} must run on Linux CI but /dev/shm is unusable: {error}"),
    }
}

/// On Linux the `O_TMPFILE` + `linkat(/proc/self/fd)` path must actually run
/// in CI: tmpfs supports it, so the test runs under `/dev/shm`.
#[cfg(target_os = "linux")]
#[test]
fn linux_o_tmpfile_publishes_through_proc_self_fd() {
    let Some(dir) = shm_dir("linux_o_tmpfile_publishes_through_proc_self_fd") else {
        return;
    };
    let root = sys::open_root(dir.path()).unwrap();
    let temp = TempFile::create(&root, 0o600).unwrap();
    assert!(temp.is_anonymous(), "tmpfs supports O_TMPFILE");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    sys::pwrite_all(temp.fd(), b"anonymous", 0).unwrap();
    sys::data_sync(temp.fd()).unwrap();
    temp.publish(&c("linked")).unwrap();
    assert_eq!(fs::read(dir.path().join("linked")).unwrap(), b"anonymous");
    assert_eq!(
        fs::metadata(dir.path().join("linked"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn socket_buffers_are_raised_and_files_are_left_alone() {
    let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
    let before = sys::socket_buffers(&left).unwrap();
    assert!(sys::set_socket_buffers(&left, 4 * 1024 * 1024).unwrap());
    assert!(sys::set_socket_buffers(right.as_fd(), 4 * 1024 * 1024).unwrap());
    let after = sys::socket_buffers(&left).unwrap();
    eprintln!("socket buffers before={before:?} after={after:?}");
    assert!(after.0 >= before.0 && after.1 >= before.1);
    #[cfg(target_vendor = "apple")]
    assert!(after.0 >= 4 * 1024 * 1024 && after.1 >= 4 * 1024 * 1024);
    let dir = tempfile::TempDir::new().unwrap();
    let root = sys::open_root(dir.path()).unwrap();
    assert!(!sys::set_socket_buffers(&root, 4 * 1024 * 1024).unwrap());
}

#[test]
fn thread_qos_applies_on_darwin_and_is_a_no_op_on_linux() {
    std::thread::spawn(|| {
        let applied = sys::set_thread_qos(super::Qos::UserInitiated).unwrap();
        let now = sys::thread_qos().unwrap();
        if cfg!(target_vendor = "apple") {
            assert!(applied);
            assert_eq!(now, Some(super::Qos::UserInitiated));
            assert!(sys::set_thread_qos(super::Qos::Background).unwrap());
            assert_eq!(sys::thread_qos().unwrap(), Some(super::Qos::Background));
            assert!(sys::set_thread_qos(super::Qos::Utility).unwrap());
            assert_eq!(sys::thread_qos().unwrap(), Some(super::Qos::Utility));
        } else {
            assert!(!applied);
            assert_eq!(now, None);
        }
    })
    .join()
    .unwrap();
}

#[cfg(feature = "io-trace")]
mod traced {
    use super::*;
    use crate::io::trace::recorder::Recorder;
    use crate::io::trace::{Event, SyncKind};

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
        sys::rename_noreplace(&root, &c("f"), &c("g")).unwrap();
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
        let temp = TempFile::create(&root, 0o777).unwrap();
        drop(attached);
        let file_mode = sys::fstat(&fd).unwrap().permissions();
        let dir_mode = sys::fstatat_nofollow(&root, &c("d")).unwrap().permissions();
        let temp_mode = sys::fstat(temp.fd()).unwrap().permissions();
        let modes: Vec<u32> = recorder
            .take()
            .iter()
            .filter_map(|event| match event {
                Event::Create { mode, .. } | Event::Mkdir { mode, .. } => Some(*mode),
                _ => None,
            })
            .collect();
        assert_eq!(modes, vec![file_mode, dir_mode, temp_mode]);
    }

    /// Review #3: two threads race overlapping writes into one file. With
    /// the serial lock, replaying the recorded writes in trace order must
    /// rebuild exactly the bytes on disk; an event order that differed from
    /// the syscall order would leave the wrong writer's bytes on top.
    #[test]
    fn concurrent_traced_writes_replay_to_the_file_on_disk() {
        const ROUNDS: usize = 400;
        let dir = tempfile::TempDir::new().unwrap();
        let root = sys::open_root(dir.path()).unwrap();
        let fd = sys::openat_beneath(&root, Path::new("f"), OpenMode::CreateExcl(0o600)).unwrap();
        let recorder = Recorder::new();
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for (byte, offset, len) in [(b'a', 0_u64, 64_usize), (b'b', 16, 32)] {
                let (recorder, fd, start) = (&recorder, &fd, &start);
                scope.spawn(move || {
                    let _attached = recorder.attach();
                    let payload = vec![byte; len];
                    start.wait();
                    for _ in 0..ROUNDS {
                        sys::pwrite_all(fd, &payload, offset).unwrap();
                    }
                });
            }
        });
        let events = recorder.take();
        assert_eq!(events.len(), 2 * ROUNDS, "one event per write");
        let mut replay = vec![0_u8; 64];
        for event in &events {
            let Event::Write { offset, data, .. } = event else {
                panic!("unexpected event {event:?}");
            };
            let start = usize::try_from(*offset).unwrap();
            replay[start..start + data.len()].copy_from_slice(data);
        }
        assert_eq!(fs::read(dir.path().join("f")).unwrap(), replay);
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
