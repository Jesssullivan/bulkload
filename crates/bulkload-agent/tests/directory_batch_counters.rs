//! OI-1003-Q143 item 2: four new sibling directories cost one store commit
//! to bind their records and one to complete them, against four each one
//! at a time. Its own test binary: the counters are the process's, so no
//! other test may run beside it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::transfer::copy;

#[test]
fn four_sibling_directories_take_one_pending_and_one_complete_commit() {
    let base = std::env::temp_dir().join(format!("bulkload-dir-batch-{}", std::process::id()));
    let source = base.join("source");
    let destination = base.join("destination");
    std::fs::create_dir_all(&destination).unwrap();
    for (index, name) in ["big", "copies", "delta", "small"].into_iter().enumerate() {
        let directory = source.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("f"),
            vec![u8::try_from(index).unwrap(); 4096],
        )
        .unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |tag: &str, cap: Option<usize>| {
        let destination = base.join(tag);
        std::fs::create_dir_all(&destination).unwrap();
        let canonical = std::fs::canonicalize(&destination).unwrap();
        bulkload_agent::materialize::set_directory_batch(&canonical, cap);
        let before = Counters::snapshot();
        let stats = copy(
            &source,
            &destination,
            &base.join(format!("{tag}-source-state")),
            &base.join(format!("{tag}-destination-state")),
        )
        .unwrap();
        let counted = Counters::snapshot().since(before);
        bulkload_agent::materialize::set_directory_batch(&canonical, None);
        assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
        assert_eq!(stats.directories_renamed, 4);
        (
            counted.get(Counter::SqliteDirectoryPending),
            counted.get(Counter::SqliteDirectoryComplete),
            counted.get(Counter::DirectoriesFinished),
        )
    };
    let batched = run("batched", Some(64));
    let alone = run("alone", Some(1));
    let _ = std::fs::remove_dir_all(&base);
    let _ = destination;
    assert_eq!(batched, (1, 1, 4), "one commit binds, one completes");
    assert_eq!(alone, (4, 4, 4), "one at a time: a commit each");
}
