//! `pull-retire-source` (operator ruling 2026-10-23): only a source file
//! the destination provably holds goes, re-verified at removal; a changed
//! one, one whose proof no longer holds, one held open, a `SQLite` store
//! and an outcome that proves no copy are skipped and left; nothing on the
//! destination or in an archive changes; a dry run removes nothing; a
//! rerun is idempotent.

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};

struct World {
    _dir: tempfile::TempDir,
    source: PathBuf,
    destination: PathBuf,
    archive: PathBuf,
    state: PathBuf,
    ledger: PathBuf,
}

fn private(path: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let world = Self {
            source: base.join("source"),
            destination: base.join("destination"),
            archive: base.join("archive"),
            state: base.join("state"),
            ledger: base.join("ledger"),
            _dir: dir,
        };
        for path in [
            &world.source,
            &world.destination,
            &world.archive,
            &world.state,
        ] {
            private(path);
        }
        private(&world.source.join("sessions"));
        private(&world.destination.join("sessions"));
        drop(crate::transfer_store::Store::open(&world.state).unwrap());
        drop(crate::consolidate::Ledger::open(&world.ledger).unwrap());
        world
    }

    /// The source file at `rel`'s row, as the walk records it.
    fn row(&self, rel: &str) -> RowSchema {
        let meta = std::fs::symlink_metadata(self.source.join(rel)).unwrap();
        let ns = |secs: i64, nanos: i64| i128::from(secs) * 1_000_000_000 + i128::from(nanos);
        RowSchema {
            rel_path: rel.as_bytes().to_vec(),
            kind: bulkload_proto::FileKind::Regular,
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
            mode: meta.mode(),
            nlink: meta.nlink(),
            link_target: None,
            blake3: None,
        }
    }

    fn key(&self, rel: &str) -> Vec<u8> {
        crate::transfer_store::row_key(b"authority", &self.row(rel)).unwrap()
    }

    /// Record `rel` in the store's `table` under its current row.
    fn record(&self, table: &str, rel: &str) -> Vec<u8> {
        let key = self.key(rel);
        let conn = rusqlite::Connection::open(self.state.join("transfer.sqlite")).unwrap();
        if table == "outputs" {
            conn.execute(
                "INSERT INTO outputs VALUES (?1, ?2)",
                rusqlite::params![key, b"identity".to_vec()],
            )
            .unwrap();
        } else {
            conn.execute("INSERT INTO consolidated VALUES (?1)", [&key])
                .unwrap();
        }
        key
    }

    fn closure(&self, actions: &[(&str, &str)]) {
        let mut record = crate::transfer_outcome::TransferRecord::running(
            "pull",
            &format!("-:{}", self.source.display()),
            &self.destination,
        );
        record.entries = actions
            .iter()
            .map(|(rel, action)| crate::transfer_outcome::Entry {
                rel_path: rel.as_bytes().to_vec(),
                outcome: crate::transfer_outcome::EntryOutcome::Consolidated {
                    action: (*action).to_owned(),
                },
            })
            .collect();
        assert!(crate::transfer_outcome::write(&self.state, &record).unwrap());
    }

    fn run(&self, dry_run: bool) -> Vec<Done> {
        let plan = plan(&self.state, Some(&self.ledger), dry_run).unwrap();
        assert_eq!(plan.host, "-");
        let mut done = plan.skipped.clone();
        done.extend(answer(&plan.ask));
        write_receipts(&self.state, &done).unwrap();
        done.sort_by(|a, b| a.rel.cmp(&b.rel));
        done
    }
}

/// Every entry under `root`: its path, kind, bytes or link text, and mtime.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let content = if meta.is_dir() {
                pending.push(path.clone());
                b"dir".to_vec()
            } else if meta.file_type().is_symlink() {
                use std::os::unix::ffi::OsStrExt as _;
                std::fs::read_link(&path)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            } else {
                std::fs::read(&path).unwrap()
            };
            out.push((rel, content, meta.modified().unwrap()));
        }
    }
    out.sort();
    out
}

fn verdicts(done: &[Done]) -> Vec<(String, Verdict)> {
    done.iter()
        .map(|one| {
            (
                String::from_utf8_lossy(&one.rel).into_owned(),
                one.verdict.clone(),
            )
        })
        .collect()
}

fn skipped(why: &str) -> Verdict {
    Verdict::Skipped(why.to_owned())
}

#[test]
fn only_a_provably_held_unchanged_source_file_goes_and_nothing_else_changes() {
    let world = World::new();
    let (source, destination, archive) = (&world.source, &world.destination, &world.archive);
    let write =
        |root: &Path, rel: &str, bytes: &[u8]| std::fs::write(root.join(rel), bytes).unwrap();
    // Landed equal (an `outputs` row): goes.
    write(source, "sessions/equal.jsonl", b"{\"a\":1}\n");
    write(destination, "sessions/equal.jsonl", b"{\"a\":1}\n");
    world.record("outputs", "sessions/equal.jsonl");
    // Covered by its linked archive file (#325): goes on the ledger's proof.
    let linked = "sessions/linked.jsonl.zst";
    write(source, linked, b"neo's rewrite");
    write(archive, "linked.jsonl.zst", b"sting's legacy");
    std::os::unix::fs::symlink(archive.join("linked.jsonl.zst"), destination.join(linked)).unwrap();
    let linked_key = world.record("consolidated", linked);
    // The same, but the link now names another file: the proof is gone.
    let moved = "sessions/moved.jsonl.zst";
    write(source, moved, b"neo's");
    write(archive, "moved.jsonl.zst", b"sting's");
    write(archive, "other.jsonl.zst", b"another");
    std::os::unix::fs::symlink(archive.join("other.jsonl.zst"), destination.join(moved)).unwrap();
    let moved_key = world.record("consolidated", moved);
    let ledger = crate::consolidate::Ledger::open(&world.ledger).unwrap();
    for (rel, key, file) in [
        (linked, &linked_key, "linked.jsonl.zst"),
        (moved, &moved_key, "moved.jsonl.zst"),
    ] {
        crate::consolidate::rollout::remember_outcome(
            &ledger,
            rel.as_bytes(),
            key,
            &crate::transfer::linked_identity(&archive.join(file)).unwrap(),
            crate::consolidate::rollout::COVERED,
        )
        .unwrap();
    }
    drop(ledger);
    // Adopted equal, then changed on the source: skipped, left.
    write(source, "sessions/changed.jsonl", b"{\"b\":1}\n");
    write(destination, "sessions/changed.jsonl", b"{\"b\":1}\n");
    world.record("consolidated", "sessions/changed.jsonl");
    // Recorded equal, but the destination's bytes differ now: skipped.
    write(source, "sessions/differs.jsonl", b"{\"c\":1}\n");
    write(destination, "sessions/differs.jsonl", b"{\"c\":2}\n");
    world.record("outputs", "sessions/differs.jsonl");
    // Merged (a union): proves no copy of the source's bytes.
    write(source, "config.json", b"{\"d\":1}\n");
    write(destination, "config.json", b"{\"d\":1,\"e\":2}\n");
    world.record("consolidated", "config.json");
    // A SQLite store is never a candidate.
    write(source, "state_5.sqlite", b"SQLite format 3\0");
    write(destination, "state_5.sqlite", b"SQLite format 3\0");
    world.record("outputs", "state_5.sqlite");
    world.closure(&[
        ("config.json", "consolidate"),
        ("sessions/changed.jsonl", "adopt"),
        (linked, crate::consolidate::rollout::COVERED),
        (moved, crate::consolidate::rollout::COVERED),
    ]);
    {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(source.join("sessions/changed.jsonl"))
            .unwrap()
            .write_all(b"{\"b\":2}\n")
            .unwrap();
    }
    let destination_before = snapshot(destination);
    let archive_before = snapshot(archive);
    let source_before = snapshot(source);

    // A dry run lists exactly what would go and removes nothing.
    let dry = world.run(true);
    let expected = |gone: Verdict| {
        vec![
            (
                "config.json".to_owned(),
                skipped("destination proof: its outcome (consolidate) proves no copy"),
            ),
            (
                "sessions/changed.jsonl".to_owned(),
                skipped("the source changed since its outcome was recorded"),
            ),
            (
                "sessions/differs.jsonl".to_owned(),
                skipped("the source's bytes differ from the destination's"),
            ),
            ("sessions/equal.jsonl".to_owned(), gone.clone()),
            (linked.to_owned(), gone),
            (
                moved.to_owned(),
                skipped("destination proof: the destination link or its linked file changed"),
            ),
        ]
    };
    assert_eq!(verdicts(&dry), expected(Verdict::WouldRemove));
    assert_eq!(snapshot(source), source_before);

    let done = world.run(false);
    assert_eq!(verdicts(&done), expected(Verdict::Removed));
    let proofs: Vec<Option<&str>> = done.iter().map(|one| one.proof.as_deref()).collect();
    assert!(proofs[3].unwrap().starts_with("blake3:"));
    assert_eq!(
        proofs[4],
        Some(format!("recorded:{}", crate::consolidate::rollout::COVERED).as_str())
    );
    assert!(!source.join("sessions/equal.jsonl").exists());
    assert!(!source.join(linked).exists());
    for rel in [
        "config.json",
        "sessions/changed.jsonl",
        "sessions/differs.jsonl",
        moved,
        "state_5.sqlite",
    ] {
        assert!(source.join(rel).exists(), "{rel}");
    }
    // Nothing on the destination or in the archive changed.
    assert_eq!(snapshot(destination), destination_before);
    assert_eq!(snapshot(archive), archive_before);

    // A rerun removes nothing more: the removed ones are absent.
    let again = world.run(false);
    let absent = skipped("the source is absent");
    assert_eq!(
        verdicts(&again)[3],
        ("sessions/equal.jsonl".to_owned(), absent.clone())
    );
    assert_eq!(verdicts(&again)[4], (linked.to_owned(), absent));
    assert_eq!(snapshot(destination), destination_before);
    assert_eq!(snapshot(archive), archive_before);
    let receipts = std::fs::read_to_string(world.state.join("retire-source.receipts")).unwrap();
    assert_eq!(receipts.lines().count(), 3 * 6);
    assert!(receipts.contains("\tremoved\tsessions/equal.jsonl\t"));
    assert!(receipts.contains("\twould-remove\tsessions/linked.jsonl.zst\t"));
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "whether a process holds a file is told on Linux (and Darwin's lsof)"
)]
fn a_file_another_process_holds_open_is_skipped_and_left() {
    let world = World::new();
    std::fs::write(world.source.join("sessions/open.jsonl"), b"{\"a\":1}\n").unwrap();
    std::fs::write(
        world.destination.join("sessions/open.jsonl"),
        b"{\"a\":1}\n",
    )
    .unwrap();
    world.record("outputs", "sessions/open.jsonl");
    world.closure(&[]);
    // A child that holds the file open until its stdin closes.
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exec 3<\"$1\"; read line")
        .arg("sh")
        .arg(world.source.join("sessions/open.jsonl"))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut opened = false;
    for _ in 0..200 {
        let fds = std::fs::read_dir(format!("/proc/{}/fd", child.id())).unwrap();
        if fds.filter_map(std::result::Result::ok).any(|fd| {
            std::fs::read_link(fd.path())
                .is_ok_and(|to| to == world.source.join("sessions/open.jsonl"))
        }) {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(opened);
    let done = world.run(false);
    drop(child.stdin.take());
    child.wait().unwrap();
    assert_eq!(
        verdicts(&done),
        [(
            "sessions/open.jsonl".to_owned(),
            skipped("a process holds it open")
        )]
    );
    assert!(world.source.join("sessions/open.jsonl").exists());
}

#[test]
fn a_state_a_pass_holds_is_refused_and_nothing_is_read() {
    use std::os::fd::AsRawFd as _;
    let world = World::new();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(world.state.join("writer.lock"))
        .unwrap();
    // SAFETY: `lock` is open for the call; `flock` takes no pointer.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    assert!(matches!(
        plan(&world.state, None, true),
        Err(BulkloadRefusal::MoveStateBusy(_))
    ));
}

#[test]
fn checked_handles_leave_a_reserve_without_making_large_runs_probe_one_file_at_a_time() {
    assert_eq!(check_batch(0), 1);
    assert_eq!(check_batch(3), 1);
    assert_eq!(check_batch(64), 16);
    assert_eq!(check_batch(256), 64);
    assert_eq!(check_batch(10_240), 200);
    assert_eq!(check_batch(u64::MAX), 200);
}

/// The regression runs alone in a subprocess: lowering the soft descriptor
/// limit must not affect unrelated tests. Every candidate has its own parent
/// directory, so keeping all checked directory handles exhausts the limit.
#[test]
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn more_candidates_than_the_descriptor_limit_remain_provable() {
    use std::os::unix::ffi::OsStrExt as _;
    const CHILD: &str = "BULKLOAD_RETIRE_LOW_NOFILE_CHILD";
    const TEST: &str = "retire::tests::more_candidates_than_the_descriptor_limit_remain_provable";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("test {TEST} ... ok")));
        return;
    }

    crate::io::limits::set_soft_descriptor_limit(64).unwrap();
    let world = World::new();
    let content = b"a proven source file\n";
    let mut files = Vec::new();
    for index in 0..256 {
        let rel = format!("sessions/{index}/rollout.jsonl");
        private(world.source.join(&rel).parent().unwrap());
        std::fs::write(world.source.join(&rel), content).unwrap();
        let row = world.row(&rel);
        files.push(Want {
            rel: rel.into_bytes(),
            allowed: vec![(
                (row.dev, row.ino, row.size, row.mtime_ns, row.ctime_ns),
                Proof::Blake3(*blake3::hash(content).as_bytes()),
            )],
        });
    }
    let mut ask = Ask {
        root: world.source.as_os_str().as_bytes().to_vec(),
        dry_run: true,
        files,
    };
    let dry = answer(&ask);
    assert_eq!(dry.len(), ask.files.len());
    assert!(
        dry.iter().all(|one| one.verdict == Verdict::WouldRemove),
        "{dry:?}"
    );
    for want in &ask.files {
        assert!(path_of(&world.source, &want.rel).is_file());
    }

    ask.dry_run = false;
    let removed = answer(&ask);
    assert_eq!(removed.len(), ask.files.len());
    assert!(
        removed.iter().all(|one| one.verdict == Verdict::Removed),
        "{removed:?}"
    );
    for want in &ask.files {
        assert!(!path_of(&world.source, &want.rel).exists());
    }
    let again = answer(&ask);
    assert_eq!(again.len(), ask.files.len());
    assert!(again
        .iter()
        .all(|one| one.verdict == skipped("the source is absent")));
    assert_eq!(crate::io::limits::descriptor_limit().unwrap().0, 64);
}
