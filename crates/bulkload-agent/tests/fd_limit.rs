//! P30 FD: a copy must complete with more files, and with more new
//! directories, than the soft open-file limit.
//!
//! - PR #59 review: a copy of many small files must complete at macOS's
//!   default soft limit of 256 open files.
//! - PR #59 round-2 review (N1): finishing more new directories than the soft
//!   open-file limit must not run out of descriptors.
//!
//! Its own test binary, because both rows lower the process-wide limit. They
//! were one binary each (`fd_limit.rs`, `fd_limit_directories.rs`) until
//! OI-1003-Q81 merged them to save a link and a run. Both lower the limit to
//! the same 256, and [`LIMIT`] runs them one at a time, so neither row's
//! descriptors count against the other's.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Mutex, PoisonError};

use bulkload_agent::limits::{descriptor_limit, set_soft_descriptor_limit};
use bulkload_agent::transfer::copy;

/// One row at a time: the descriptor limit is the whole process's.
static LIMIT: Mutex<()> = Mutex::new(());

#[test]
fn seven_hundred_files_copy_under_a_256_descriptor_limit() {
    let _alone = LIMIT.lock().unwrap_or_else(PoisonError::into_inner);
    let base = std::env::temp_dir().join(format!("bulkload-fd-limit-{}", std::process::id()));
    let source = base.join("source");
    let destination = base.join("destination");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    for index in 0..700 {
        let bytes: Vec<u8> = (0..2000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed.to_le_bytes()[0]
            })
            .collect();
        std::fs::write(source.join(format!("f{index:04}")), bytes).unwrap();
    }
    let (_, hard) = descriptor_limit().unwrap();
    set_soft_descriptor_limit(hard.min(256)).unwrap();
    assert!(descriptor_limit().unwrap().0 <= 256);
    let stats = copy(
        &source,
        &destination,
        &base.join("source-state"),
        &base.join("destination-state"),
    )
    .unwrap();
    let published = std::fs::read_dir(&destination).unwrap().count();
    let _ = std::fs::remove_dir_all(&base);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals.first());
    assert_eq!(stats.completed, 700);
    assert_eq!(published, 700);
}

#[test]
fn more_new_directories_than_the_descriptor_limit_all_finish() {
    const DIRECTORIES: usize = 400;
    let _alone = LIMIT.lock().unwrap_or_else(PoisonError::into_inner);
    let base = std::env::temp_dir().join(format!("bulkload-fd-dirs-{}", std::process::id()));
    let source = base.join("source");
    let destination = base.join("destination");
    std::fs::create_dir_all(&destination).unwrap();
    for index in 0..DIRECTORIES {
        let directory = source.join(format!("d{index:04}"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("f"), [u8::try_from(index % 251).unwrap()]).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (_, hard) = descriptor_limit().unwrap();
    set_soft_descriptor_limit(hard.min(256)).unwrap();
    assert!(descriptor_limit().unwrap().0 < DIRECTORIES as u64);
    let stats = copy(
        &source,
        &destination,
        &base.join("source-state"),
        &base.join("destination-state"),
    )
    .unwrap();
    let modes: Vec<u32> = (0..DIRECTORIES)
        .map(|index| {
            std::fs::metadata(destination.join(format!("d{index:04}")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        })
        .collect();
    let _ = std::fs::remove_dir_all(&base);
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals.first());
    assert_eq!(stats.completed, DIRECTORIES as u64);
    assert!(
        modes.iter().all(|mode| *mode == 0o755),
        "{} directories left unfinished",
        modes.iter().filter(|mode| **mode != 0o755).count()
    );
}
