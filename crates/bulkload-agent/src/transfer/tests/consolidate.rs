//! `--consolidate` on `copy` and `pull` (operator ruling 2026-10-20: the
//! agent homes are consolidated, never landed beside; SPEC RUL-10, C10;
//! SPEC rule 2: no payload copy on either machine).
//!
//! - A differing JSON or TOML file is merged into the live file in place,
//!   and nothing is left beside it.
//! - An append log is extended (a live prefix), covered (the live file holds
//!   the source's bytes), or consolidated by the #294 lane's JSONL union:
//!   one file, the source's bytes then the live file's lines it lacks.
//! - An opaque file and a live path of another kind are held
//!   `HOMES_CONSOLIDATE_UNSUPPORTED`, with nothing written. A `SQLite`
//!   store and its sidecars are routed to the row union
//!   (`sqlite_union::union_routed`), nothing read or written on the file
//!   path.
//! - An unchanged rerun reads 0 source bytes.
//! - The destination state holds no payload byte.
//! - `--plan-only` writes nothing and lists every seat.
//! - A Codex rollout whose live path links into an archive root is covered
//!   when the linked file holds every one of its events (ordinals dropped,
//!   host paths mapped, `.zst` or plain on either side), else held with how
//!   many it lacks; the link and the archive are untouched (A-106).

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
                git: None,
                archived: Vec::new(),
            })),
        );
    }

    /// Run with the archive roots `archived` (`--replace-archived-dest`)
    /// and the `--map` set `maps`.
    fn set_archived(&self, archived: Vec<PathBuf>, maps: crate::consolidate::Maps) {
        set_consolidate_for(
            &self.root,
            Some(Some(Consolidation {
                maps,
                ledger: self.state().join("consolidate"),
                plan_only: false,
                git: None,
                archived,
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a union that writes needs the open-file check (Linux only; elsewhere HOMES_CONSOLIDATE_HOLDER_UNKNOWN)"
)]
fn an_append_log_is_extended_covered_or_consolidated() {
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
    let lines = |tag: &str, count: usize| -> Vec<u8> {
        (0..count).fold(Vec::new(), |mut bytes, at| {
            bytes.extend_from_slice(format!("{{\"{tag}\":{at}}}\n").as_bytes());
            bytes
        })
    };
    let mut diverged = lines("shared", 10_000);
    let mut theirs = diverged.clone();
    diverged.extend_from_slice(&lines("neo", 100));
    theirs.extend_from_slice(&lines("sting", 100));
    std::fs::write(source.join("diverged.jsonl"), &diverged).unwrap();
    std::fs::write(destination.join("diverged.jsonl"), &theirs).unwrap();
    run.settle();

    let stats = run.run();
    assert!(held(&stats).is_empty(), "{:?}", held(&stats));
    assert_eq!(
        consolidated(&stats),
        [
            ("continued.jsonl".to_owned(), "covered".to_owned()),
            ("diverged.jsonl".to_owned(), "consolidate".to_owned()),
        ]
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
    let mut union = diverged.clone();
    union.extend_from_slice(&lines("sting", 100));
    assert_eq!(
        std::fs::read(destination.join("diverged.jsonl")).unwrap(),
        union,
        "one file: the source's bytes, then the live file's lines"
    );
    run.no_temporaries();
    run.no_payload_in_state(&diverged[..4096]);
    run.no_payload_in_state(&grown[200_000..204_096]);

    // Unchanged: every seat is reused, with 0 source bytes read.
    let again = run.run();
    assert!(held(&again).is_empty());
    assert_eq!(again.source_bytes_read, 0);
}

#[test]
fn opaque_files_and_other_kinds_are_held_and_sqlite_stores_routed_never_beside() {
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
    assert_eq!(held(&stats), ["blob.bin", "linked.json"]);
    let mut routed = stats.sqlite_routed;
    routed.sort();
    assert_eq!(
        routed,
        [b"state_5.sqlite".to_vec(), b"state_5.sqlite-wal".to_vec()]
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

/// Rollout lines as Codex writes them: `ordinal` after `timestamp` when
/// `ordinals`, the session's cwd under `home`.
fn rollout(home: &str, ordinals: bool, events: &[&str]) -> Vec<u8> {
    let mut lines = vec![format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"019f0000\",\"cwd\":\"{home}/git/x\"}}}}"
    )];
    lines.extend(events.iter().map(|event| (*event).to_owned()));
    lines
        .iter()
        .enumerate()
        .fold(Vec::new(), |mut bytes, (at, line)| {
            let body = line.strip_prefix('{').unwrap();
            let ordinal = if ordinals {
                format!("\"ordinal\":{at},")
            } else {
                String::new()
            };
            bytes.extend_from_slice(
                format!("{{\"timestamp\":\"2026-07-22T04:39:{at:02}.000Z\",{ordinal}{body}\n")
                    .as_bytes(),
            );
            bytes
        })
}

fn zst(bytes: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(bytes, 3).unwrap()
}

/// Every file under `root` with its bytes and mtime.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    Consolidating::files(root)
        .into_iter()
        .map(|(name, bytes)| {
            let modified = std::fs::metadata(root.join(&name))
                .unwrap()
                .modified()
                .unwrap();
            (name, bytes, modified)
        })
        .collect()
}

#[test]
fn a_rollout_linked_into_an_archive_is_covered_by_its_events_or_held_never_written() {
    let run = Consolidating::new(false);
    let (source, destination) = (run.source(), run.destination());
    let archive = run.corpus.base.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    run.set_archived(
        vec![archive.clone()],
        crate::consolidate::Maps::new([("/Users/jess".to_owned(), "/home/jess".to_owned())]),
    );
    let events = [
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#,
        r#"{"type":"event_msg","payload":{"type":"patch","changes":{"a":1,"b":2}}}"#,
        r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
    ];
    // sting's archive: the legacy rollouts, no ordinals, sting's home.
    let legacy = rollout("/home/jess", false, &events);
    let dir = "sessions/2026/07/22";
    std::fs::create_dir_all(source.join(dir)).unwrap();
    std::fs::create_dir_all(destination.join(dir)).unwrap();
    let name = |tag: &str| format!("{dir}/rollout-2026-07-22T00-39-31-019f0000-{tag}.jsonl.zst");
    // The four seats and the archived file each live path links to.
    let subset = rollout(
        "/Users/jess",
        true,
        &[
            events[0],
            r#"{"type":"event_msg","payload":{"type":"patch","changes":{"b":2,"a":1}}}"#,
        ],
    );
    let mut more = events.to_vec();
    more.push(r#"{"type":"event_msg","payload":{"type":"user_message","message":"neo only"}}"#);
    let lacking = rollout("/Users/jess", true, &more);
    let cases: [(&str, Vec<u8>, &str, Vec<u8>); 4] = [
        // Covered: neo's zst rewrite holds a subset of the archive's events.
        ("zst", zst(&subset), "zst.jsonl.zst", zst(&legacy)),
        // Covered: a plain JSONL source against a zst archive file.
        ("plain", subset.clone(), "plain.jsonl.zst", zst(&legacy)),
        // Covered: a zst source against a plain archive file.
        ("tojsonl", zst(&subset), "tojsonl.jsonl", legacy.clone()),
        // Held: neo holds one event the archive lacks.
        ("lacks", zst(&lacking), "lacks.jsonl.zst", zst(&legacy)),
    ];
    for (tag, seat, archived, held_bytes) in &cases {
        std::fs::write(source.join(name(tag)), seat).unwrap();
        std::fs::write(archive.join(archived), held_bytes).unwrap();
        std::os::unix::fs::symlink(archive.join(archived), destination.join(name(tag))).unwrap();
    }
    run.settle();
    settle_racy_window(&archive).unwrap();
    let before = snapshot(&archive);

    let stats = run.run();
    let covered = crate::consolidate::rollout::COVERED.to_owned();
    assert_eq!(
        consolidated(&stats),
        [
            (name("plain"), covered.clone()),
            (name("tojsonl"), covered.clone()),
            (name("zst"), covered),
        ]
    );
    assert_eq!(held(&stats), [name("lacks")]);
    let reasons: Vec<(String, String)> = stats
        .held
        .iter()
        .map(|(path, why)| (String::from_utf8_lossy(path).into_owned(), why.to_string()))
        .collect();
    assert_eq!(
        reasons,
        [(
            name("lacks"),
            "source holds 1 of its 5 events the linked file lacks (the source is Codex's \
             ordinal rewrite of a legacy rollout; the link holds the legacy one)"
                .to_owned()
        )]
    );
    // Nothing written: the archive is as it was, byte for byte and mtime,
    // and every live path is still the same link.
    assert_eq!(snapshot(&archive), before);
    for (tag, _, archived, _) in &cases {
        let live = destination.join(name(tag));
        assert!(std::fs::symlink_metadata(&live)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(&live).unwrap(), archive.join(archived));
    }
    run.no_temporaries();
    run.no_payload_in_state(&subset[..64]);

    // Unchanged: the covered seats are reused and the held one held again
    // with its reason, 0 source bytes read; nothing written still.
    let again = run.run();
    assert_eq!(again.source_bytes_read, 0);
    assert_eq!(held(&again), [name("lacks")]);
    assert_eq!(
        again
            .held
            .iter()
            .map(|(_, why)| why.to_string())
            .collect::<Vec<_>>(),
        [reasons[0].1.clone()]
    );
    assert_eq!(snapshot(&archive), before);
}
