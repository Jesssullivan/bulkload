//! PR #59 round-2 review (N1): finishing more new directories than the soft
//! open-file limit must not run out of descriptors. Its own test binary,
//! because it lowers the process-wide limit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;

use bulkload_agent::limits::{descriptor_limit, set_soft_descriptor_limit};
use bulkload_agent::transfer::copy;

#[test]
fn more_new_directories_than_the_descriptor_limit_all_finish() {
    const DIRECTORIES: usize = 400;
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
