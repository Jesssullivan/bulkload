//! PR #59 review: a copy of many small files must complete at macOS's default
//! soft limit of 256 open files. Its own test binary, because it lowers the
//! process-wide limit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bulkload_agent::io::limits::{descriptor_limit, set_soft_descriptor_limit};
use bulkload_agent::transfer::copy;

#[test]
fn seven_hundred_files_copy_under_a_256_descriptor_limit() {
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
