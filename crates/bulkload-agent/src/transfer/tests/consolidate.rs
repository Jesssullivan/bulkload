//! `--consolidate` on `copy` and `pull` (operator ruling 2026-10-20: the
//! agent homes are consolidated, never landed beside; SPEC RUL-10, C10;
//! SPEC rule 2: no payload copy on either machine).
//!
//! - A differing JSON or TOML file is merged into the live file in place,
//!   and nothing is left beside it.
//! - An append log is extended (a live prefix), covered (the live file holds
//!   the source's bytes), or held until the JSONL union lands.
//! - An opaque file, a `SQLite` store and a live path of another kind are
//!   held `HOMES_CONSOLIDATE_UNSUPPORTED`, with nothing written.
//! - An unchanged rerun reads 0 source bytes.
//! - The destination state holds no payload byte.
//! - `--plan-only` writes nothing and lists every seat.

use super::*;

/// A corpus whose destination root runs with `--consolidate`.
struct Consolidating {
    corpus: Corpus,
    root: PathBuf,
}

impl Consolidating {
    fn new(plan_only: bool) -> Self {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        let this = Self { corpus, root };
        this.set(plan_only);
        this
    }

    fn set(&self, plan_only: bool) {
        set_consolidate_for(
            &self.root,
            Some(Some(Consolidation {
                maps: crate::consolidate::Maps::default(),
                ledger: self.state().join("consolidate"),
                plan_only,
            })),
        );
    }

    fn source(&self) -> PathBuf {
        self.corpus.base.join("source")
    }

    fn destination(&self) -> PathBuf {
        self.corpus.base.join("destination")
    }

    fn state(&self) -> PathBuf {
        self.corpus.base.join("destination-state")
    }

    fn run(&self) -> TransferStats {
        self.corpus.run().unwrap()
    }

    fn settle(&self) {
        settle_racy_window(&self.source()).unwrap();
        settle_racy_window(&self.destination()).unwrap();
    }

    /// Every file under `root`, by relative path, with its bytes.
    fn files(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    pending.push(path);
                } else if meta.is_file() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.push((rel, std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// SPEC rule 2: no file of the destination state holds `payload`.
    fn no_payload_in_state(&self, payload: &[u8]) {
        for (name, bytes) in Self::files(&self.state()) {
            assert!(
                !bytes.windows(payload.len()).any(|window| window == payload),
                "{name} holds payload bytes"
            );
        }
    }

    /// No private name is left beside a live path.
    fn no_temporaries(&self) {
        for (name, _) in Self::files(&self.destination()) {
            assert!(
                !name.contains(".bulkload") && !name.contains(".tmp"),
                "{name} left beside a live path"
            );
        }
    }
}

impl Drop for Consolidating {
    fn drop(&mut self) {
        set_consolidate_for(&self.root, None);
    }
}

fn set_mtime(path: &Path, age_secs: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs))
        .unwrap();
}

fn consolidated(stats: &TransferStats) -> Vec<(String, String)> {
    stats
        .consolidated
        .iter()
        .map(|(path, action)| (String::from_utf8_lossy(path).into_owned(), action.clone()))
        .collect()
}

fn held(stats: &TransferStats) -> Vec<String> {
    let mut held: Vec<String> = stats
        .refusals
        .iter()
        .filter(|(_, code)| code == "HOMES_CONSOLIDATE_UNSUPPORTED")
        .map(|(path, _)| String::from_utf8_lossy(path).into_owned())
        .collect();
    held.sort();
    held
}

/// A marker no state file may hold (SPEC rule 2).
const MARKER: &str = "payload-marker-7f3c9e1a2b4d6f8091a3c5e7d9b1f3a5";

#[test]
fn a_differing_config_is_merged_in_place_and_nothing_lands_beside() {
    let run = Consolidating::new(false);
    let (source, destination) = (run.source(), run.destination());
    std::fs::write(
        source.join("settings.json"),
        format!("{{\n  \"model\": \"sonnet\",\n  \"allow\": [\n    \"a\"\n  ],\n  \"note\": \"{MARKER}\"\n}}\n"),
    )
    .unwrap();
    std::fs::write(
        destination.join("settings.json"),
        "{\n  \"model\": \"opus\",\n  \"allow\": [\n    \"b\"\n  ],\n  \"theme\": \"dark\"\n}\n",
    )
    .unwrap();
    std::fs::write(source.join("config.toml"), "a = 1\n[t]\nb = 2\n").unwrap();
    std::fs::write(destination.join("config.toml"), "# sting\nc = 3\n").unwrap();
    set_mtime(&destination.join("settings.json"), 7200);
    set_mtime(&source.join("settings.json"), 3600);
    run.settle();

    let stats = run.run();
    assert!(
        stats.refusals.is_empty(),
        "{:?} {:?}",
        stats.refusals,
        stats.held
    );
    assert_eq!(
        consolidated(&stats),
        [
            ("config.toml".to_owned(), "consolidate".to_owned()),
            ("settings.json".to_owned(), "consolidate".to_owned()),
        ]
    );
    // The source modified last: its model is in the file, sting's recorded.
    assert_eq!(
        std::fs::read_to_string(destination.join("settings.json")).unwrap(),
        format!("{{\n  \"model\": \"sonnet\",\n  \"allow\": [\n    \"b\",\n    \"a\"\n  ],\n  \"theme\": \"dark\",\n  \"note\": \"{MARKER}\"\n}}\n")
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("config.toml")).unwrap(),
        "# sting\nc = 3\na = 1\n\n[t]\nb = 2\n"
    );
    let ledger = crate::consolidate::Ledger::open(&run.state().join("consolidate")).unwrap();
    let recorded = ledger.recorded().unwrap();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].other, "\"opus\"");
    run.no_temporaries();
    run.no_payload_in_state(MARKER.as_bytes());

    // Unchanged: reused, 0 source bytes, nothing written.
    let before = Consolidating::files(&destination);
    let again = run.run();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(again.source_bytes_read, 0);
    assert!(again.consolidated.is_empty());
    assert_eq!(Consolidating::files(&destination), before);
}

#[test]
fn an_append_log_is_extended_covered_or_held() {
    let run = Consolidating::new(false);
    let (source, destination) = (run.source(), run.destination());
    let grown = noise(401, 400_000);
    std::fs::write(source.join("grown.jsonl"), &grown).unwrap();
    std::fs::write(destination.join("grown.jsonl"), &grown[..200_000]).unwrap();
    let continued = noise(402, 300_000);
    let mut longer = continued.clone();
    longer.extend_from_slice(&noise(403, 50_000));
    std::fs::write(source.join("continued.jsonl"), &continued).unwrap();
    std::fs::write(destination.join("continued.jsonl"), &longer).unwrap();
    let diverged = noise(404, 300_000);
    std::fs::write(source.join("diverged.jsonl"), &diverged).unwrap();
    std::fs::write(destination.join("diverged.jsonl"), noise(405, 300_000)).unwrap();
    let theirs = std::fs::read(destination.join("diverged.jsonl")).unwrap();
    run.settle();

    let stats = run.run();
    assert_eq!(held(&stats), ["diverged.jsonl"]);
    assert_eq!(
        consolidated(&stats),
        [("continued.jsonl".to_owned(), "covered".to_owned())]
    );
    assert_eq!(
        stats.prefix_extended.len(),
        1,
        "{:?}",
        stats.prefix_extended
    );
    assert_eq!(
        std::fs::read(destination.join("grown.jsonl")).unwrap(),
        grown
    );
    assert_eq!(
        std::fs::read(destination.join("continued.jsonl")).unwrap(),
        longer
    );
    assert_eq!(
        std::fs::read(destination.join("diverged.jsonl")).unwrap(),
        theirs
    );
    run.no_temporaries();
    run.no_payload_in_state(&diverged[..4096]);
    run.no_payload_in_state(&grown[200_000..204_096]);

    // Unchanged: the covered seat is reused and the held one is held again,
    // with 0 source bytes read.
    let again = run.run();
    assert_eq!(held(&again), ["diverged.jsonl"]);
    assert_eq!(again.source_bytes_read, 0);
}

#[test]
fn opaque_files_sqlite_stores_and_other_kinds_are_held_never_beside() {
    let run = Consolidating::new(false);
    let (source, destination) = (run.source(), run.destination());
    std::fs::write(source.join("blob.bin"), noise(411, 100_000)).unwrap();
    std::fs::write(destination.join("blob.bin"), noise(412, 100_000)).unwrap();
    std::fs::write(source.join("state_5.sqlite"), noise(413, 100_000)).unwrap();
    std::fs::write(source.join("state_5.sqlite-wal"), noise(414, 1000)).unwrap();
    std::fs::write(source.join("linked.json"), b"{\"a\": 1}\n").unwrap();
    std::fs::write(destination.join("elsewhere.json"), b"{\"b\": 2}\n").unwrap();
    std::os::unix::fs::symlink("elsewhere.json", destination.join("linked.json")).unwrap();
    std::fs::write(source.join("same.bin"), b"same").unwrap();
    std::fs::write(destination.join("same.bin"), b"same").unwrap();
    std::fs::write(source.join("new.bin"), b"new").unwrap();
    run.settle();

    let stats = run.run();
    assert_eq!(
        held(&stats),
        [
            "blob.bin",
            "linked.json",
            "state_5.sqlite",
            "state_5.sqlite-wal"
        ]
    );
    assert_eq!(std::fs::read(destination.join("new.bin")).unwrap(), b"new");
    assert!(!destination.join("state_5.sqlite").exists());
    assert_eq!(
        std::fs::read(destination.join("elsewhere.json")).unwrap(),
        b"{\"b\": 2}\n"
    );
    assert!(std::fs::symlink_metadata(destination.join("linked.json"))
        .unwrap()
        .file_type()
        .is_symlink());
    run.no_temporaries();
    let mut names: Vec<String> = Consolidating::files(&destination)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    names.sort();
    assert_eq!(names, ["blob.bin", "elsewhere.json", "new.bin", "same.bin"]);
}

#[test]
fn plan_only_writes_nothing_and_lists_every_seat() {
    let run = Consolidating::new(true);
    let (source, destination) = (run.source(), run.destination());
    std::fs::write(source.join("new.json"), b"{}\n").unwrap();
    std::fs::write(source.join("same.bin"), b"same").unwrap();
    std::fs::write(destination.join("same.bin"), b"same").unwrap();
    std::fs::write(source.join("settings.json"), b"{\"a\": 1}\n").unwrap();
    std::fs::write(destination.join("settings.json"), b"{\"b\": 2}\n").unwrap();
    let grown = noise(421, 400_000);
    std::fs::write(source.join("grown.jsonl"), &grown).unwrap();
    std::fs::write(destination.join("grown.jsonl"), &grown[..200_000]).unwrap();
    std::fs::write(source.join("blob.bin"), b"ours").unwrap();
    std::fs::write(destination.join("blob.bin"), b"theirs").unwrap();
    std::fs::create_dir(source.join("sub")).unwrap();
    std::fs::write(source.join("sub/inner.txt"), b"x").unwrap();
    run.settle();
    let before = Consolidating::files(&destination);

    let stats = run.run();
    assert!(stats.refusals.is_empty(), "{:?}", stats.refusals);
    assert_eq!(Consolidating::files(&destination), before);
    assert!(!destination.join("sub").exists());
    let mut planned: Vec<(String, &str)> = stats
        .planned
        .iter()
        .map(|seat| {
            (
                String::from_utf8_lossy(&seat.rel_path).into_owned(),
                seat.action,
            )
        })
        .collect();
    planned.sort();
    assert_eq!(
        planned,
        [
            ("blob.bin".to_owned(), "hold"),
            ("grown.jsonl".to_owned(), "extend"),
            ("new.json".to_owned(), "create"),
            ("same.bin".to_owned(), "adopt"),
            ("settings.json".to_owned(), "consolidate"),
            ("sub/inner.txt".to_owned(), "create"),
        ]
    );
    let blob = stats
        .planned
        .iter()
        .find(|seat| seat.rel_path == b"blob.bin")
        .unwrap();
    assert_eq!((blob.source_size, blob.live_size), (4, Some(6)));
    assert_eq!(blob.reason, Some("opaque: the bytes differ"));
    // The plan recorded nothing a real pass would reuse.
    run.set(false);
    let real = run.run();
    assert_eq!(
        consolidated(&real),
        [("settings.json".to_owned(), "consolidate".to_owned())]
    );
}
