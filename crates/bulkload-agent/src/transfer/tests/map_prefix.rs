//! `--map-prefix SRC_REL=DST_REL` on `pull` and `copy` (#280 finding 9, the
//! operator's "no duplicates"): a source path lands once, at its mapped
//! destination path, decided, keyed and recorded there; a grown append-only
//! file is extended in place under the mapped path; an unchanged rerun reads
//! nothing; a source path that would reach a map's destination any other
//! way is refused with everything under it, whatever the walk order, so
//! nothing merges; and a set the rules refuse refuses before either store
//! exists.

use super::*;

const NEO: &str = "projects/-Users-jess-git-lab";
const STING: &str = "projects/-srv-fast-local-jess-git-lab";

fn pair(source: &str, destination: &str) -> map::Map {
    (source.as_bytes().to_vec(), destination.as_bytes().to_vec())
}

/// A corpus whose destination root runs with `maps` (and `--consolidate`).
struct Mapped {
    corpus: Corpus,
    root: PathBuf,
}

impl Mapped {
    fn new(maps: Vec<map::Map>, consolidate: bool) -> Self {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        set_map_prefix(&root, Some(maps));
        if consolidate {
            set_consolidate_for(
                &root,
                Some(Some(Consolidation {
                    maps: crate::consolidate::Maps::new(Vec::new()),
                    ledger: corpus.base.join("consolidate"),
                    plan_only: false,
                    archived: Vec::new(),
                })),
            );
        }
        Self { corpus, root }
    }

    fn source(&self, rel: &str) -> PathBuf {
        self.corpus.base.join("source").join(rel)
    }

    fn destination(&self, rel: &str) -> PathBuf {
        self.corpus.base.join("destination").join(rel)
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn settle(&self) {
        settle_racy_window(&self.corpus.base.join("source")).unwrap();
        settle_racy_window(&self.corpus.base.join("destination")).unwrap();
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        set_map_prefix(&self.root, None);
        set_consolidate_for(&self.root, None);
    }
}

fn mapped(stats: &TransferStats) -> Vec<(String, String)> {
    stats
        .mapped
        .iter()
        .map(|(source, destination)| {
            (
                String::from_utf8_lossy(source).into_owned(),
                String::from_utf8_lossy(destination).into_owned(),
            )
        })
        .collect()
}

fn refused(stats: &TransferStats) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = stats
        .refusals
        .iter()
        .map(|(path, code)| (String::from_utf8_lossy(path).into_owned(), code.clone()))
        .collect();
    out.sort();
    out
}

#[test]
fn a_mapped_key_lands_once_under_its_destination_and_a_rerun_reads_nothing() {
    let run = Mapped::new(vec![pair(NEO, STING)], true);
    let transcript = noise(901, 300_000);
    let subagent = noise(902, 20_000);
    Mapped::write(&run.source(&format!("{NEO}/a.jsonl")), &transcript);
    Mapped::write(
        &run.source(&format!("{NEO}/a/subagents/s.jsonl")),
        &subagent,
    );
    Mapped::write(&run.source(&format!("{NEO}/memory/m.md")), b"a note\n");
    Mapped::write(
        &run.source("projects/-Users-jess-other/o.jsonl"),
        b"other\n",
    );
    // The destination's own session under the mapped key.
    Mapped::write(
        &run.destination(&format!("{STING}/own.jsonl")),
        b"sting's\n",
    );
    run.settle();

    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert!(first.held.is_empty(), "{:?}", first.held);
    assert_eq!(mapped(&first), [(NEO.to_owned(), STING.to_owned())]);
    assert_eq!(
        std::fs::read(run.destination(&format!("{STING}/a.jsonl"))).unwrap(),
        transcript
    );
    assert_eq!(
        std::fs::read(run.destination(&format!("{STING}/a/subagents/s.jsonl"))).unwrap(),
        subagent
    );
    assert_eq!(
        std::fs::read(run.destination(&format!("{STING}/memory/m.md"))).unwrap(),
        b"a note\n"
    );
    assert_eq!(
        std::fs::read(run.destination(&format!("{STING}/own.jsonl"))).unwrap(),
        b"sting's\n"
    );
    assert!(
        !run.destination(NEO).exists(),
        "nothing lands under the source's key"
    );
    assert_eq!(
        std::fs::read(run.destination("projects/-Users-jess-other/o.jsonl")).unwrap(),
        b"other\n",
        "an unmapped path lands as it is"
    );
    // The record names the map, accounted like `applied`.
    let record = crate::transfer_outcome::TransferRecord::ended(
        "copy",
        "source",
        &run.root,
        &Ok(first),
        "transfer::copy",
    );
    assert!(
        record.entries.contains(&crate::transfer_outcome::Entry {
            rel_path: NEO.as_bytes().to_vec(),
            outcome: crate::transfer_outcome::EntryOutcome::Mapped {
                to: STING.as_bytes().to_vec()
            },
        }),
        "{:?}",
        record.entries
    );

    run.settle();
    let again = run.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
    assert_eq!(again.reused, 4, "{again:?}");
    assert_eq!(mapped(&again), [(NEO.to_owned(), STING.to_owned())]);
    assert!(!run.destination(NEO).exists());
}

#[test]
fn a_growing_transcript_is_extended_in_place_under_its_mapped_key() {
    let run = Mapped::new(vec![pair(NEO, STING)], true);
    let full = noise(911, 3 << 20);
    let prior = (2 << 20) + 4_321;
    // The destination holds a byte prefix it does not own (an earlier
    // pass's re-seated copy), the source the grown transcript.
    let at = run.destination(&format!("{STING}/t.jsonl"));
    Mapped::write(&at, full.get(..prior).unwrap());
    Mapped::write(&run.source(&format!("{NEO}/t.jsonl")), &full);
    run.settle();
    let inode = std::fs::metadata(&at).unwrap().ino();

    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert!(first.held.is_empty(), "{:?}", first.held);
    assert_eq!(
        first
            .prefix_extended
            .iter()
            .map(|(path, held, _, size)| (path.clone(), *held, *size))
            .collect::<Vec<_>>(),
        [(
            format!("{STING}/t.jsonl").into_bytes(),
            prior as u64,
            full.len() as u64
        )]
    );
    assert_eq!(std::fs::read(&at).unwrap(), full);
    assert_eq!(std::fs::metadata(&at).unwrap().ino(), inode, "in place");
    assert!(!run.destination(NEO).exists());

    // It grows again: extended again, under the mapped key.
    let mut grown = full;
    grown.extend_from_slice(&noise(912, 50_000));
    Mapped::write(&run.source(&format!("{NEO}/t.jsonl")), &grown);
    run.settle();
    let second = run.corpus.run().unwrap();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert!(second.held.is_empty(), "{:?}", second.held);
    assert_eq!(std::fs::read(&at).unwrap(), grown);
    assert_eq!(std::fs::metadata(&at).unwrap().ino(), inode, "in place");

    run.settle();
    let third = run.corpus.run().unwrap();
    assert!(third.refusals.is_empty(), "{:?}", third.refusals);
    assert_eq!(
        third.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
}

/// A transcript smaller than its first chunk that grew is extended in place
/// under its mapped key; one whose head differs is consolidated into the
/// live file, the source's bytes then the live file's line, never beside
/// (SPEC rule 3).
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a union that writes needs the open-file check (Linux only; elsewhere HOMES_CONSOLIDATE_HOLDER_UNKNOWN)"
)]
fn a_small_transcript_that_grew_is_extended_and_a_small_fork_is_consolidated() {
    let run = Mapped::new(vec![pair(NEO, STING)], true);
    let small: &[u8] = b"{\"a\":1}\n";
    let grown: &[u8] = b"{\"a\":1}\n{\"b\":2}\n";
    let at = run.destination(&format!("{STING}/s.jsonl"));
    Mapped::write(&at, small);
    Mapped::write(&run.source(&format!("{NEO}/s.jsonl")), grown);
    let fork_at = run.destination(&format!("{STING}/k.jsonl"));
    Mapped::write(&fork_at, b"{\"z\":0}\n");
    Mapped::write(&run.source(&format!("{NEO}/k.jsonl")), grown);
    run.settle();

    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert!(first.held.is_empty(), "{:?}", first.held);
    assert_eq!(std::fs::read(&at).unwrap(), grown);
    let mut union = grown.to_vec();
    union.extend_from_slice(b"{\"z\":0}\n");
    assert_eq!(std::fs::read(&fork_at).unwrap(), union);
    let names: Vec<_> = std::fs::read_dir(at.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "nothing beside: {names:?}");

    run.settle();
    let again = run.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
}

/// The destination continued the session (resumed there): its live file
/// holds the seat's records and more. It is covered and never rewritten
/// (SPEC rule 7); one both sides continued is consolidated into one file;
/// an unchanged rerun reads nothing.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a union that writes needs the open-file check (Linux only; elsewhere HOMES_CONSOLIDATE_HOLDER_UNKNOWN)"
)]
fn a_live_file_that_holds_the_seat_and_more_is_covered_and_never_rewritten() {
    let run = Mapped::new(vec![pair(NEO, STING)], true);
    let neo: Vec<u8> = (0..20_000).fold(Vec::new(), |mut bytes, at| {
        bytes.extend_from_slice(format!("{{\"line\":{at}}}\n").as_bytes());
        bytes
    });
    let mut sting = neo.clone();
    sting.extend_from_slice(b"{\"sting\":\"continued\"}\n");
    let at = run.destination(&format!("{STING}/r.jsonl"));
    Mapped::write(&at, &sting);
    Mapped::write(&run.source(&format!("{NEO}/r.jsonl")), &neo);
    let mut forked = neo;
    forked.extend_from_slice(b"{\"neo\":\"continued\"}\n");
    Mapped::write(&run.source(&format!("{NEO}/f.jsonl")), &forked);
    let fork_at = run.destination(&format!("{STING}/f.jsonl"));
    Mapped::write(&fork_at, &sting);
    run.settle();
    let live = std::fs::metadata(&at).unwrap();

    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    let mut consolidated = first.consolidated;
    consolidated.sort();
    assert_eq!(
        consolidated,
        [
            (
                format!("{STING}/f.jsonl").into_bytes(),
                "consolidate".to_owned()
            ),
            (
                format!("{STING}/r.jsonl").into_bytes(),
                "covered".to_owned()
            ),
        ]
    );
    assert_eq!(std::fs::read(&at).unwrap(), sting, "untouched");
    let now = std::fs::metadata(&at).unwrap();
    assert_eq!(
        (now.ino(), now.mtime_nsec()),
        (live.ino(), live.mtime_nsec())
    );
    let mut union = forked;
    union.extend_from_slice(b"{\"sting\":\"continued\"}\n");
    assert_eq!(std::fs::read(&fork_at).unwrap(), union);

    run.settle();
    let again = run.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
    assert_eq!(std::fs::read(&at).unwrap(), sting);
}

/// The source holds a mapped destination's name itself: those paths would
/// merge into the mapped ones. Refused, typed, with everything under them,
/// whichever the walk offers first; the mapped paths land.
#[test]
fn a_source_path_at_a_mapped_destination_is_refused_and_nothing_merges() {
    // The mapped source walks first ("-U" sorts before "-s") ...
    let first = Mapped::new(vec![pair(NEO, STING)], false);
    // ... and here the source's own copy of the destination's name does.
    let second = Mapped::new(vec![pair("projects/b", "projects/a")], false);
    for (run, from, to) in [(&first, NEO, STING), (&second, "projects/b", "projects/a")] {
        Mapped::write(&run.source(&format!("{from}/x.jsonl")), b"mapped\n");
        Mapped::write(&run.source(&format!("{to}/y.jsonl")), b"native\n");
        Mapped::write(&run.source(&format!("{to}/sub/z.txt")), b"native too\n");
        Mapped::write(&run.source(&format!("{to}/x.jsonl")), b"same name\n");
        run.settle();
        let stats = run.corpus.run().unwrap();
        let occupied = |rel: &str| (format!("{to}{rel}"), "DESTINATION_MAP_CONFLICT".to_owned());
        assert_eq!(
            refused(&stats),
            [
                occupied(""),
                occupied("/sub"),
                occupied("/sub/z.txt"),
                occupied("/x.jsonl"),
                occupied("/y.jsonl"),
            ],
            "{from} -> {to}"
        );
        assert_eq!(
            std::fs::read(run.destination(&format!("{to}/x.jsonl"))).unwrap(),
            b"mapped\n"
        );
        assert!(!run.destination(&format!("{to}/y.jsonl")).exists());
        assert!(!run.destination(&format!("{to}/sub")).exists());
        assert!(!run.destination(from).exists());
        // Refused for what it is, unchanged: the same answer again.
        run.settle();
        let again = run.corpus.run().unwrap();
        assert_eq!(refused(&again), refused(&stats));
    }
}

#[test]
fn a_set_the_rules_refuse_refuses_before_either_store_exists() {
    let run = Mapped::new(
        vec![
            pair("projects/a", "projects/c"),
            pair("projects/b", "projects/c"),
        ],
        false,
    );
    Mapped::write(&run.source("projects/a/x"), b"x\n");
    assert_eq!(
        run.corpus.run().map(drop),
        Err(BulkloadRefusal::FieldDomainViolation)
    );
    assert!(!run.corpus.base.join("destination-state").exists());
    assert!(std::fs::read_dir(run.destination(""))
        .unwrap()
        .next()
        .is_none());
}

/// #280 N4 with the map: a source path the owner rule refuses is still the
/// source's, listed at its own path; a mapped one is listed where it lands.
#[test]
fn a_refused_second_route_is_listed_as_the_sources() {
    let run = Mapped::new(vec![pair(NEO, STING)], false);
    Mapped::write(&run.source(&format!("{NEO}/x.jsonl")), b"mapped\n");
    Mapped::write(&run.source(&format!("{STING}/y.jsonl")), b"native\n");
    run.settle();
    set_listing(vec![b"projects/".to_vec()]);
    let stats = run.corpus.run();
    set_listing(Vec::new());
    let stats = stats.unwrap();
    assert_eq!(
        refused(&stats),
        [
            (STING.to_owned(), "DESTINATION_MAP_CONFLICT".to_owned()),
            (
                format!("{STING}/y.jsonl"),
                "DESTINATION_MAP_CONFLICT".to_owned()
            ),
        ]
    );
    let mut listed = stats.listed;
    listed.sort();
    assert!(
        listed.contains(&format!("{STING}/x.jsonl").into_bytes()),
        "{listed:?}"
    );
    assert!(
        listed.contains(&format!("{STING}/y.jsonl").into_bytes()),
        "{listed:?}"
    );
    assert!(!listed.iter().any(|path| path.starts_with(NEO.as_bytes())));
}

/// #280 finding 9 with `--consolidate`: the destination holds the same
/// session at the map's source key (an earlier hand copy). Absent at the
/// mapped key, it is moved there (the same inode, never copied); present,
/// the source-key copy is unioned into the mapped one and removed. Both
/// are ledgered (`key_fold`); `--plan-only` lists them and does nothing.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a union that writes needs the open-file check (Linux only; elsewhere HOMES_CONSOLIDATE_HOLDER_UNKNOWN)"
)]
fn a_source_key_copy_is_folded_into_the_mapped_key() {
    use std::os::unix::fs::MetadataExt as _;
    let shared: &[u8] = b"{\"uuid\":\"a\"}\n";
    let mut neo_moved = shared.to_vec();
    neo_moved.extend_from_slice(b"{\"uuid\":\"n\"}\n");
    let mut old_union = shared.to_vec();
    old_union.extend_from_slice(b"{\"uuid\":\"o\"}\n");
    let mut sting_union = shared.to_vec();
    sting_union.extend_from_slice(b"{\"uuid\":\"s\"}\n");
    let setup = |run: &Mapped| {
        Mapped::write(&run.source(&format!("{NEO}/m.jsonl")), &neo_moved);
        Mapped::write(&run.source(&format!("{NEO}/u.jsonl")), shared);
        // The hand copies at the source key on the destination.
        Mapped::write(&run.destination(&format!("{NEO}/m.jsonl")), shared);
        Mapped::write(&run.destination(&format!("{NEO}/u.jsonl")), &old_union);
        // The mapped key holds u's own continuation.
        Mapped::write(&run.destination(&format!("{STING}/u.jsonl")), &sting_union);
        run.settle();
    };

    // Planned: listed, nothing done.
    let plan = Mapped::new(vec![pair(NEO, STING)], true);
    set_consolidate_for(
        &plan.root,
        Some(Some(Consolidation {
            maps: crate::consolidate::Maps::new(Vec::new()),
            ledger: plan.corpus.base.join("consolidate"),
            plan_only: true,
            archived: Vec::new(),
        })),
    );
    setup(&plan);
    let planned = plan.corpus.run().unwrap();
    let mut folds: Vec<(String, &str)> = planned
        .planned
        .iter()
        .filter(|seat| seat.action.starts_with("fold-"))
        .map(|seat| {
            (
                String::from_utf8_lossy(&seat.rel_path).into_owned(),
                seat.action,
            )
        })
        .collect();
    folds.sort();
    assert_eq!(
        folds,
        [
            (format!("{NEO}/m.jsonl"), "fold-move"),
            (format!("{NEO}/u.jsonl"), "fold-union"),
        ]
    );
    assert!(plan.destination(&format!("{NEO}/m.jsonl")).exists());

    let run = Mapped::new(vec![pair(NEO, STING)], true);
    setup(&run);
    let hand = std::fs::metadata(run.destination(&format!("{NEO}/m.jsonl")))
        .unwrap()
        .ino();
    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    let mut folded = first.folded;
    folded.sort();
    assert_eq!(
        folded
            .iter()
            .map(|(_, _, action)| action.as_str())
            .collect::<Vec<_>>(),
        ["rmdir", "move", "union"]
    );
    // m: moved (the same inode), then extended in place by neo's line.
    let m = run.destination(&format!("{STING}/m.jsonl"));
    assert_eq!(
        std::fs::metadata(&m).unwrap().ino(),
        hand,
        "moved, never copied"
    );
    assert_eq!(std::fs::read(&m).unwrap(), neo_moved);
    // u: the hand copy's record unioned into the mapped file, then neo's.
    let u = std::fs::read(run.destination(&format!("{STING}/u.jsonl"))).unwrap();
    for record in [
        &b"{\"uuid\":\"a\"}\n"[..],
        b"{\"uuid\":\"o\"}\n",
        b"{\"uuid\":\"s\"}\n",
    ] {
        assert_eq!(
            u.windows(record.len())
                .filter(|window| *window == record)
                .count(),
            1,
            "{}",
            String::from_utf8_lossy(&u)
        );
    }
    assert!(!run.destination(&format!("{NEO}/m.jsonl")).exists());
    assert!(!run.destination(&format!("{NEO}/u.jsonl")).exists());
    // The key directory the fold emptied is removed (plain rmdir).
    assert!(!run.destination(NEO).exists());
    let ledger =
        rusqlite::Connection::open(run.corpus.base.join("consolidate/consolidate.sqlite")).unwrap();
    let rows: i64 = ledger
        .query_row("SELECT count(*) FROM key_fold", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
}

/// The same fold for a session's subdirectory under the source key
/// (subagents, tool results): an append log is moved or unioned, any
/// other file moved when absent, its source-key copy removed when the
/// mapped key holds the same bytes, and held (both kept) when it holds
/// other bytes. `--plan-only` lists every one.
/// A file under the session: its path, the hand copy's bytes, the mapped
/// key's (if any).
type SubFile = (&'static str, &'static [u8], Option<&'static [u8]>);

#[test]
#[allow(clippy::too_many_lines)] // Plan, then run, then each file's end.
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "a union that writes needs the open-file check (Linux only; elsewhere HOMES_CONSOLIDATE_HOLDER_UNKNOWN)"
)]
fn a_sessions_subfiles_at_the_source_key_fold_the_same_way() {
    let files: [SubFile; 5] = [
        // (path under the session, the hand copy's bytes, the mapped key's)
        (
            "subagents/a1.jsonl",
            b"{\"uuid\":\"a\"}\n{\"uuid\":\"o\"}\n",
            Some(b"{\"uuid\":\"a\"}\n{\"uuid\":\"s\"}\n"),
        ),
        ("subagents/a2.jsonl", b"{\"uuid\":\"b\"}\n", None),
        ("tool-results/same.txt", b"same\n", Some(b"same\n")),
        ("tool-results/diff.txt", b"neo's\n", Some(b"sting's\n")),
        ("tool-results/moved.txt", b"moved\n", None),
    ];
    let setup = |run: &Mapped| {
        for (rel, hand, mapped) in files {
            let rel = format!("sid/{rel}");
            Mapped::write(&run.source(&format!("{NEO}/{rel}")), hand);
            Mapped::write(&run.destination(&format!("{NEO}/{rel}")), hand);
            if let Some(mapped) = mapped {
                Mapped::write(&run.destination(&format!("{STING}/{rel}")), mapped);
            }
        }
        run.settle();
    };
    let plan = Mapped::new(vec![pair(NEO, STING)], true);
    set_consolidate_for(
        &plan.root,
        Some(Some(Consolidation {
            maps: crate::consolidate::Maps::new(Vec::new()),
            ledger: plan.corpus.base.join("consolidate"),
            plan_only: true,
            archived: Vec::new(),
        })),
    );
    setup(&plan);
    let planned = plan.corpus.run().unwrap();
    let mut folds: Vec<(String, &str)> = planned
        .planned
        .iter()
        .filter(|seat| seat.action.starts_with("fold-"))
        .map(|seat| {
            (
                String::from_utf8_lossy(&seat.rel_path).into_owned(),
                seat.action,
            )
        })
        .collect();
    folds.sort();
    assert_eq!(
        folds,
        [
            (format!("{NEO}/sid/subagents/a1.jsonl"), "fold-union"),
            (format!("{NEO}/sid/subagents/a2.jsonl"), "fold-move"),
            (format!("{NEO}/sid/tool-results/diff.txt"), "fold-hold"),
            (format!("{NEO}/sid/tool-results/moved.txt"), "fold-move"),
            (format!("{NEO}/sid/tool-results/same.txt"), "fold-remove"),
        ]
    );

    let run = Mapped::new(vec![pair(NEO, STING)], true);
    setup(&run);
    let first = run.corpus.run().unwrap();
    let mut actions: Vec<(String, String)> = first
        .folded
        .iter()
        .filter(|(_, _, action)| action != "rmdir")
        .map(|(from, _, action)| (String::from_utf8_lossy(from).into_owned(), action.clone()))
        .collect();
    actions.sort();
    assert_eq!(
        actions,
        [
            (format!("{NEO}/sid/subagents/a1.jsonl"), "union".to_owned()),
            (format!("{NEO}/sid/subagents/a2.jsonl"), "move".to_owned()),
            (
                format!("{NEO}/sid/tool-results/moved.txt"),
                "move".to_owned()
            ),
            (
                format!("{NEO}/sid/tool-results/same.txt"),
                "remove".to_owned()
            ),
        ]
    );
    assert_eq!(
        refused(&first),
        [(
            format!("{STING}/sid/tool-results/diff.txt"),
            "HOMES_CONSOLIDATE_UNSUPPORTED".to_owned()
        )]
    );
    for gone in [
        "subagents/a1.jsonl",
        "subagents/a2.jsonl",
        "tool-results/same.txt",
        "tool-results/moved.txt",
    ] {
        assert!(
            !run.destination(&format!("{NEO}/sid/{gone}")).exists(),
            "{gone}"
        );
    }
    // Held: both kept, untouched.
    assert_eq!(
        std::fs::read(run.destination(&format!("{NEO}/sid/tool-results/diff.txt"))).unwrap(),
        b"neo's\n"
    );
    assert_eq!(
        std::fs::read(run.destination(&format!("{STING}/sid/tool-results/diff.txt"))).unwrap(),
        b"sting's\n"
    );
    let a1 = std::fs::read(run.destination(&format!("{STING}/sid/subagents/a1.jsonl"))).unwrap();
    for record in [
        &b"{\"uuid\":\"a\"}\n"[..],
        b"{\"uuid\":\"o\"}\n",
        b"{\"uuid\":\"s\"}\n",
    ] {
        assert_eq!(
            a1.windows(record.len())
                .filter(|window| *window == record)
                .count(),
            1
        );
    }
    assert_eq!(
        std::fs::read(run.destination(&format!("{STING}/sid/tool-results/moved.txt"))).unwrap(),
        b"moved\n"
    );
    // The emptied source-key directory goes (plain rmdir); one with the
    // held file left in it stays, and so do its parents.
    assert!(!run.destination(&format!("{NEO}/sid/subagents")).exists());
    assert!(run.destination(&format!("{NEO}/sid/tool-results")).is_dir());
    assert!(run.destination(NEO).is_dir());
    let removed: Vec<String> = first
        .folded
        .iter()
        .filter(|(_, _, action)| action == "rmdir")
        .map(|(dir, _, _)| String::from_utf8_lossy(dir).into_owned())
        .collect();
    assert_eq!(removed, [format!("{NEO}/sid/subagents")]);
}
