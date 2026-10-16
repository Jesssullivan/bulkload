//! The directory finish folded into the session's last output group
//! (OI-1003-Q154 lever 3, the SOTA study's lever 2; `DirectoryRecords.tla`
//! `GroupFinish`). The counter-exact proof, which needs a process of its
//! own, is `tests/directory_batch_counters.rs`; the power-loss proof and its
//! teeth are in `tests/power_loss.rs`.

use super::*;

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

/// Four sibling directories under the root and a child under one, each
/// with a file, every mode one this user can still read, write and
/// search.
const TREE: [(&str, u32); 5] = [
    ("a", 0o750),
    ("a/inner", 0o755),
    ("b", 0o751),
    ("c", 0o755),
    ("d", 0o710),
];

fn populate(source: &Path, tree: &[(&str, u32)]) {
    for (index, (directory, _)) in tree.iter().enumerate() {
        std::fs::create_dir_all(source.join(directory)).unwrap();
        std::fs::write(
            source.join(directory).join("f"),
            noise(900 + index as u64, 2_000 + 37 * index),
        )
        .unwrap();
    }
    for (directory, mode) in tree.iter().rev() {
        std::fs::set_permissions(
            source.join(directory),
            std::fs::Permissions::from_mode(*mode),
        )
        .unwrap();
    }
    settle_racy_window(source).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().mode() & 0o7777
}

/// Every directory of `tree` under `root` has its source mode, and every
/// file its source bytes.
fn finished(corpus: &Corpus, tree: &[(&str, u32)]) {
    for (directory, mode) in tree {
        let at = corpus.base.join("destination").join(directory);
        assert_eq!(mode_of(&at), *mode, "{directory}");
        assert_eq!(
            std::fs::read(at.join("f")).unwrap(),
            std::fs::read(corpus.base.join("source").join(directory).join("f")).unwrap(),
            "{directory}/f"
        );
    }
}

fn directory_records(corpus: &Corpus) -> u64 {
    Store::open(&corpus.base.join("destination-state"))
        .unwrap()
        .directory_records()
        .unwrap()
}

/// Give back this user's write permission on every directory of the
/// corpus, so its `Drop` can remove it.
fn writable(corpus: &Corpus) {
    for side in ["source", "destination"] {
        let mut stack = vec![corpus.base.join(side)];
        while let Some(directory) = stack.pop() {
            let _ = std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700));
            if let Ok(entries) = std::fs::read_dir(&directory) {
                stack.extend(
                    entries
                        .filter_map(std::result::Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                        .map(|entry| entry.path()),
                );
            }
        }
    }
}

/// The fold: every directory the copy created is finished by its last
/// group's commit, with its source mode, and no record is left pending;
/// the same copy with the fold off finishes them as before. Either way a
/// rerun reuses every output and reads no source byte.
#[test]
fn the_directory_finish_rides_the_last_group_commit() {
    for fold in [true, false] {
        let corpus = Corpus::new();
        populate(&corpus.base.join("source"), &TREE);
        let root = canonical(&corpus.base.join("destination"));
        set_directory_fold(&root, Some(fold));
        let first = corpus.run();
        let rerun = corpus.run();
        set_directory_fold(&root, None);
        let first = first.unwrap();
        assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        assert_eq!(first.directories_renamed, TREE.len() as u64);
        assert_eq!(
            first.directories_folded,
            if fold { TREE.len() as u64 } else { 0 },
            "fold {fold}"
        );
        finished(&corpus, &TREE);
        assert_eq!(directory_records(&corpus), 0, "fold {fold}: a record left");
        let rerun = rerun.unwrap();
        assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
        assert_eq!(
            (
                rerun.reused,
                rerun.source_bytes_read,
                rerun.directories_folded
            ),
            (TREE.len() as u64, 0, 0),
            "fold {fold}"
        );
    }
}

/// A final mode that takes this user's write permission away is never
/// folded: the last group's renames, or a retry after it failed, still
/// enter the directory. It is finished after the last group, as before.
/// The read bit's rule is proved at the materializer
/// (`materialize::fold_tests`): a source directory without owner read is
/// one its owner cannot walk, so a test that owns its source cannot copy
/// one.
#[test]
fn a_directory_this_user_cannot_write_is_finished_on_its_own() {
    if crate::io::sys::effective_uid() == 0 {
        return;
    }
    let corpus = Corpus::new();
    let tree = [("a", 0o755), ("ro", 0o555)];
    populate(&corpus.base.join("source"), &tree);
    let stats = corpus.run();
    let records = directory_records(&corpus);
    let modes = (
        mode_of(&corpus.base.join("destination/a")),
        mode_of(&corpus.base.join("destination/ro")),
    );
    writable(&corpus);
    let stats = stats.unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(stats.directories_folded, 0);
    assert_eq!(records, 0);
    assert_eq!(modes, (0o755, 0o555));
}

/// A tree of directories alone has no output group to ride: it is finished
/// on its own.
#[test]
fn directories_with_no_output_are_finished_on_their_own() {
    let corpus = Corpus::new();
    for (directory, mode) in [("x", 0o750), ("x/y", 0o755)] {
        let at = corpus.base.join("source").join(directory);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let stats = corpus.run().unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(
        (stats.directories_renamed, stats.directories_folded),
        (2, 0)
    );
    assert_eq!(mode_of(&corpus.base.join("destination/x")), 0o750);
    assert_eq!(mode_of(&corpus.base.join("destination/x/y")), 0o755);
    assert_eq!(directory_records(&corpus), 0);
}

/// The last group's commit fails: no directory completes (`GroupFinish`
/// commits the outputs and the completions together or neither), every
/// record stays pending, and the session refuses its outputs. The resume
/// converges: every directory finished, nothing left pending.
#[test]
fn a_failed_last_group_completes_no_directory() {
    let corpus = Corpus::new();
    populate(&corpus.base.join("source"), &TREE);
    let store_root = destination_store_root(&corpus);
    crate::transfer_store::fail_next_group_commit(&store_root);
    let failed = corpus.run().unwrap();
    assert_eq!(failed.directories_folded, 0);
    // One group under a quiet host; a loaded one may split it at the idle,
    // and only the first commit fails.
    assert!(!failed.refusals.is_empty());
    assert!(
        failed
            .refusals
            .iter()
            .all(|(_, code)| code == "DESTINATION_SPACE_INSUFFICIENT"),
        "{:?}",
        failed.refusals
    );
    assert_eq!(
        directory_records(&corpus),
        TREE.len() as u64,
        "a directory completed beside a failed group"
    );
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    finished(&corpus, &TREE);
    assert_eq!(directory_records(&corpus), 0);
}

/// Files enough for two destination groups: the first closes at its file
/// bound while the stream still runs, and the drain's group carries the
/// rest with the finish.
const MANY: usize = 70;

/// An earlier group refused its outputs: the drain's group, which itself
/// commits, folds no finish (the sink checks every outcome it has
/// reported, which the receiving side has not all seen yet), so every
/// directory stays unfinished, as a session that refuses leaves them. The
/// resume converges.
#[test]
fn a_refusal_in_an_earlier_group_folds_no_finish() {
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    std::fs::create_dir(source.join("many")).unwrap();
    for index in 0..MANY {
        std::fs::write(
            source.join(format!("many/{index:03}")),
            noise(1_200 + index as u64, 1_000 + index),
        )
        .unwrap();
    }
    std::fs::set_permissions(source.join("many"), std::fs::Permissions::from_mode(0o750)).unwrap();
    settle_racy_window(&source).unwrap();
    let store_root = destination_store_root(&corpus);
    crate::transfer_store::fail_next_group_commit(&store_root);
    let failed = corpus.run().unwrap();
    assert!(!failed.refusals.is_empty());
    assert_eq!(failed.directories_folded, 0);
    assert_eq!(
        directory_records(&corpus),
        1,
        "a directory completed in a session that refused"
    );
    let resumed = corpus.run().unwrap();
    assert!(resumed.refusals.is_empty(), "{:?}", resumed.refusals);
    assert_eq!(mode_of(&corpus.base.join("destination/many")), 0o750);
    assert_eq!(directory_records(&corpus), 0);
}

/// The last output closes its group at the file bound, so the drain's
/// group is empty and drops the finish it was handed (`close_last`) after
/// its modes were set: the separate finish then completes every directory
/// over those modes, with no refusal (S1 fold review, 2026-10-16, finding
/// 1). A loaded host may split the stream at an idle and fold after all;
/// either way the copy ends finished.
#[test]
fn a_drain_whose_group_is_already_empty_finishes_on_its_own() {
    let staged =
        (crate::io::limits::descriptor_budget() / 8).clamp(2, crate::io::durable::GROUP_FILES);
    let corpus = Corpus::new();
    let source = corpus.base.join("source");
    std::fs::create_dir_all(source.join("x/y")).unwrap();
    for index in 0..staged {
        let directory = if index % 2 == 0 { "x" } else { "x/y" };
        std::fs::write(
            source.join(format!("{directory}/{index:03}")),
            noise(1_500 + index, 500),
        )
        .unwrap();
    }
    std::fs::set_permissions(source.join("x/y"), std::fs::Permissions::from_mode(0o710)).unwrap();
    std::fs::set_permissions(source.join("x"), std::fs::Permissions::from_mode(0o750)).unwrap();
    settle_racy_window(&source).unwrap();
    let stats = corpus.run().unwrap();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(stats.directories_renamed, 2);
    assert_eq!(mode_of(&corpus.base.join("destination/x")), 0o750);
    assert_eq!(mode_of(&corpus.base.join("destination/x/y")), 0o710);
    assert_eq!(directory_records(&corpus), 0);
    let rerun = corpus.run().unwrap();
    assert!(rerun.refusals.is_empty(), "{:?}", rerun.refusals);
    assert_eq!((rerun.reused, rerun.source_bytes_read), (staged, 0));
}
