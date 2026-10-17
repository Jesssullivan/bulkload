//! OI-1003-Q143 item 2: four new sibling directories cost one store commit
//! to bind their records and one to complete them, against four each one
//! at a time. OI-1003-Q154 lever 3: with the finish folded into the last
//! output group, they complete in that group's commit, with no completion
//! commit and no seal of their own. Its own test binary: the counters are
//! the process's, so no other test may run beside it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;

use bulkload_agent::counters::{Counter, Counters};
use bulkload_agent::transfer::copy;

#[test]
fn four_sibling_directories_take_one_pending_commit_and_ride_the_last_group() {
    // The group counts below assume each run's four outputs share one
    // destination group. Under heavy host load the default 250 ms idle can
    // close a group between two of them (seen at load ~90: batched
    // `DurableGroups` 2, folded 1). A long idle keeps the count about the
    // fold, not the scheduler; the 1 s group age still bounds it. This test
    // is its own binary, so the variable reaches no other test.
    std::env::set_var(bulkload_agent::transfer::DEST_GROUP_IDLE_ENV, "60000");
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
    let run = |tag: &str, cap: Option<usize>, fold: bool| {
        let destination = base.join(tag);
        std::fs::create_dir_all(&destination).unwrap();
        let canonical = std::fs::canonicalize(&destination).unwrap();
        bulkload_agent::materialize::set_directory_batch(&canonical, cap);
        bulkload_agent::transfer::set_directory_fold(&canonical, Some(fold));
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
        bulkload_agent::transfer::set_directory_fold(&canonical, None);
        assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
        assert_eq!(stats.directories_renamed, 4);
        assert_eq!(
            stats.directories_folded,
            counted.get(Counter::DirectoriesFolded)
        );
        (
            (
                counted.get(Counter::SqliteDirectoryPending),
                counted.get(Counter::SqliteDirectoryComplete),
                counted.get(Counter::DirectoriesFinished),
                counted.get(Counter::DirectoriesFolded),
            ),
            (
                counted.get(Counter::FlushDirBarrier) + counted.get(Counter::FlushDir),
                counted.get(Counter::SqliteGroupDest),
                counted.get(Counter::DurableGroups),
            ),
        )
    };
    let (folded, folded_seals) = run("folded", Some(64), true);
    let (batched, batched_seals) = run("batched", Some(64), false);
    let (alone, _) = run("alone", Some(1), true);
    let _ = std::fs::remove_dir_all(&base);
    let _ = destination;
    assert_eq!(
        folded,
        (1, 0, 4, 4),
        "one commit binds; the last group's commit completes"
    );
    assert_eq!(batched, (1, 1, 4, 0), "one commit binds, one completes");
    assert_eq!(
        alone,
        (4, 4, 4, 0),
        "one at a time: a commit each, and no fold"
    );
    // The fold takes away the finish's own seal of each directory, and
    // adds no group, no group commit and no seal of its own: the last
    // group's seal covers the directories (each holds an output of it).
    assert_eq!(
        (folded_seals.0 + 4, folded_seals.1, folded_seals.2),
        batched_seals,
        "folded {folded_seals:?}, batched {batched_seals:?}"
    );
}
