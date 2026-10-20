//! `--map-prefix SRC_REL=DST_REL` on `pull` and `copy` (#280 finding 9, the
//! operator's "no duplicates"): a source path lands once, at its mapped
//! destination path, decided, keyed and recorded there; a grown append-only
//! file is extended in place under the mapped path; an unchanged rerun reads
//! nothing; a source path that would reach a map's destination any other
//! way is refused with everything under it, whatever the walk order, so
//! nothing merges; and a set the rules refuse refuses before either store
//! exists.

use super::*;

const SUFFIX: &[u8] = b".neo-20261019";
const NEO: &str = "projects/-Users-jess-git-lab";
const STING: &str = "projects/-srv-fast-local-jess-git-lab";

fn pair(source: &str, destination: &str) -> map::Map {
    (source.as_bytes().to_vec(), destination.as_bytes().to_vec())
}

/// A corpus whose destination root runs with `maps` (and `--beside`).
struct Mapped {
    corpus: Corpus,
    root: PathBuf,
}

impl Mapped {
    fn new(maps: Vec<map::Map>, beside: bool) -> Self {
        let corpus = Corpus::new();
        let root = std::fs::canonicalize(corpus.base.join("destination")).unwrap();
        set_map_prefix(&root, Some(maps));
        if beside {
            set_beside(&root, Some(Some(SUFFIX.to_vec())));
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
        set_beside(&self.root, None);
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
    assert!(
        first.applied_beside.is_empty(),
        "{:?}",
        first.applied_beside
    );
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
    assert!(
        first.applied_beside.is_empty(),
        "{:?}",
        first.applied_beside
    );
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
    let mut beside = at.clone().into_os_string();
    beside.push(std::ffi::OsStr::from_bytes(SUFFIX));
    assert!(!Path::new(&beside).exists(), "nothing beside");
    assert!(!run.destination(NEO).exists());

    // It grows again: extended again, under the mapped key.
    let mut grown = full;
    grown.extend_from_slice(&noise(912, 50_000));
    Mapped::write(&run.source(&format!("{NEO}/t.jsonl")), &grown);
    run.settle();
    let second = run.corpus.run().unwrap();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert!(
        second.applied_beside.is_empty(),
        "{:?}",
        second.applied_beside
    );
    assert_eq!(std::fs::read(&at).unwrap(), grown);
    assert_eq!(std::fs::metadata(&at).unwrap().ino(), inode, "in place");
    assert!(!Path::new(&beside).exists(), "nothing beside");

    run.settle();
    let third = run.corpus.run().unwrap();
    assert!(third.refusals.is_empty(), "{:?}", third.refusals);
    assert_eq!(
        third.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
}

/// A transcript smaller than its first chunk that grew is extended in
/// place under `--beside` (its head judged whole against the staged first
/// chunk), not copied beside; one whose head differs lands whole beside.
#[test]
fn a_small_transcript_that_grew_is_extended_and_a_small_fork_lands_beside() {
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
    let inode = std::fs::metadata(&at).unwrap().ino();

    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(
        first
            .prefix_extended
            .iter()
            .map(|(path, held, _, size)| (path.clone(), *held, *size))
            .collect::<Vec<_>>(),
        [(
            format!("{STING}/s.jsonl").into_bytes(),
            small.len() as u64,
            grown.len() as u64
        )]
    );
    assert_eq!(std::fs::read(&at).unwrap(), grown);
    assert_eq!(std::fs::metadata(&at).unwrap().ino(), inode, "in place");
    let beside = |path: &Path| {
        let mut beside = path.as_os_str().to_owned();
        beside.push(std::ffi::OsStr::from_bytes(SUFFIX));
        PathBuf::from(beside)
    };
    assert!(!beside(&at).exists(), "nothing beside");
    assert_eq!(std::fs::read(&fork_at).unwrap(), b"{\"z\":0}\n");
    assert_eq!(std::fs::read(beside(&fork_at)).unwrap(), grown);

    run.settle();
    let again = run.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
}

/// The destination continued the session (resumed there): its live file
/// holds the seat's bytes and more. Nothing lands, beside or in place; the
/// seat is accounted `dest-ahead`, and an unchanged rerun reads nothing.
/// One that holds other bytes still lands beside.
#[test]
fn a_live_file_that_holds_the_seat_and_more_is_dest_ahead_and_nothing_lands() {
    let run = Mapped::new(vec![pair(NEO, STING)], true);
    let neo = noise(921, 400_000);
    let mut sting = neo.clone();
    sting.extend_from_slice(b"{\"sting\":\"continued\"}\n");
    let at = run.destination(&format!("{STING}/r.jsonl"));
    Mapped::write(&at, &sting);
    Mapped::write(&run.source(&format!("{NEO}/r.jsonl")), &neo);
    // Both sides changed: kept both.
    let mut forked = neo;
    forked.extend_from_slice(b"{\"neo\":\"continued\"}\n");
    Mapped::write(&run.source(&format!("{NEO}/f.jsonl")), &forked);
    let fork_at = run.destination(&format!("{STING}/f.jsonl"));
    Mapped::write(&fork_at, &sting);
    run.settle();
    let live = std::fs::metadata(&at).unwrap();

    let first = run.corpus.run().unwrap();
    assert!(first.refusals.is_empty(), "{:?}", first.refusals);
    assert_eq!(first.dest_ahead, [format!("{STING}/r.jsonl").into_bytes()]);
    assert_eq!(
        first.applied_beside,
        [(
            format!("{STING}/f.jsonl").into_bytes(),
            format!("{STING}/f.jsonl{}", String::from_utf8_lossy(SUFFIX)).into_bytes()
        )]
    );
    assert_eq!(std::fs::read(&at).unwrap(), sting, "untouched");
    let now = std::fs::metadata(&at).unwrap();
    assert_eq!(
        (now.ino(), now.mtime_nsec()),
        (live.ino(), live.mtime_nsec())
    );
    let mut beside = at.clone().into_os_string();
    beside.push(std::ffi::OsStr::from_bytes(SUFFIX));
    assert!(!Path::new(&beside).exists(), "nothing beside");
    assert_eq!(std::fs::read(&fork_at).unwrap(), sting);
    let record = crate::transfer_outcome::TransferRecord::ended(
        "copy",
        "source",
        &run.root,
        &Ok(first),
        "transfer::copy",
    );
    assert!(record.entries.contains(&crate::transfer_outcome::Entry {
        rel_path: format!("{STING}/r.jsonl").into_bytes(),
        outcome: crate::transfer_outcome::EntryOutcome::DestAhead,
    }));

    run.settle();
    let again = run.corpus.run().unwrap();
    assert!(again.refusals.is_empty(), "{:?}", again.refusals);
    assert_eq!(
        again.source_bytes_read, 0,
        "an unchanged rerun reads nothing"
    );
    assert_eq!(again.dest_ahead, [format!("{STING}/r.jsonl").into_bytes()]);
    assert_eq!(std::fs::read(&at).unwrap(), sting);
    assert!(!Path::new(&beside).exists());
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

/// #294 review 2: an append-only live file written within the racy window
/// (a session writing there now) is refused `DESTINATION_UNSETTLED`, typed,
/// nothing landed beside it and nothing remembered: once it is quiet the
/// next pass decides it (here: both continued, so it lands beside).
#[test]
fn a_live_file_written_now_is_unsettled_and_the_next_pass_decides() {
    let run = Mapped::new(vec![pair(NEO, STING)], true);
    let at = run.destination(&format!("{STING}/w.jsonl"));
    Mapped::write(
        &run.source(&format!("{NEO}/w.jsonl")),
        b"{\"a\":1}\n{\"neo\":3}\n",
    );
    settle_racy_window(&run.corpus.base.join("source")).unwrap();
    // The destination's session writes now, and its clock is now.
    Mapped::write(&at, b"{\"a\":1}\n{\"sting\":2}\n");
    set_capture_clock(&run.root, Some(crate::git_carry::pass_start_ns()));
    let first = run.corpus.run();
    set_capture_clock(&run.root, None);
    let first = first.unwrap();
    assert_eq!(
        refused(&first),
        [(
            format!("{STING}/w.jsonl"),
            "DESTINATION_UNSETTLED".to_owned()
        )]
    );
    let mut beside = at.clone().into_os_string();
    beside.push(std::ffi::OsStr::from_bytes(SUFFIX));
    assert!(!Path::new(&beside).exists(), "never a beside copy");
    assert_eq!(std::fs::read(&at).unwrap(), b"{\"a\":1}\n{\"sting\":2}\n");

    // Quiet now (the default test clock is an hour ahead): decided again,
    // not remembered.
    let second = run.corpus.run().unwrap();
    assert!(second.refusals.is_empty(), "{:?}", second.refusals);
    assert_eq!(
        second.applied_beside.len(),
        1,
        "{:?}",
        second.applied_beside
    );
    assert_eq!(
        std::fs::read(Path::new(&beside)).unwrap(),
        b"{\"a\":1}\n{\"neo\":3}\n"
    );
}
