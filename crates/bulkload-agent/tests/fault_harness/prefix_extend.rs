//! M2r2 A-103: a crash at each step of a grown byte-prefix file's
//! extension (the `receive.prefix_*` points, and the adoption's group
//! commit after them).
//!
//! Each scenario copies three files, loses the destination store (a
//! landing moved by hand), grows every file on the source, and crashes the
//! rerun that extends them at one point (the child ends itself with
//! `_exit`; nothing here sends a signal, R-N11). It asserts:
//!
//! - **a prefix, never other bytes.** At the crash every file at the
//!   destination is a byte prefix of its grown source (the old one, a
//!   longer one, or all of it); only `.bulkload-*` temporaries are extra;
//! - **a rerun finishes.** The resume converges with no refusal, every file
//!   is its source, no temporary survives (the sweep removed exactly the
//!   crash's), and a further run reuses every file with 0 bytes read and 0
//!   received.

use std::collections::BTreeMap;
use std::fs;

use bulkload_agent::fault::Point;

use super::{crash_child, is_temporary, noise, relative, tree, Scenario, Scratch};

fn settle(source: &std::path::Path) {
    bulkload_agent::transfer::settle_racy_window(source).unwrap();
}

/// The files: name, held length, grown length. Each held length is past
/// the first chunk (the most a chunk can be is 256 KiB) and stops inside
/// one; the tails are long and short.
const FILES: [(&str, usize, usize); 3] = [
    ("a.jsonl", 1_500_001, 2_600_000),
    ("b.jsonl", 400_000, 900_000),
    ("c.jsonl", 300_000, 300_123),
];

/// A crash in the rerun that extends grown files.
#[derive(Clone, Copy)]
pub struct PrefixExtend;

impl Scenario for PrefixExtend {
    fn run(self, point: Point, nth: u64) {
        scenario(point, nth);
    }
}

fn grown(index: usize) -> Vec<u8> {
    noise(0x00A1_0300 + index as u64, FILES[index].2)
}

/// Every non-temporary leaf is a byte prefix of its grown source. Returns
/// the temporaries.
fn assert_prefixes(label: &str, scratch: &Scratch, full: &BTreeMap<&str, Vec<u8>>) -> usize {
    let mut temporaries = 0;
    for (path, metadata) in tree(&scratch.destination()) {
        if is_temporary(&path) {
            assert!(metadata.is_file(), "{label}: a temporary that is no file");
            temporaries += 1;
            continue;
        }
        let name = String::from_utf8(path.clone()).unwrap();
        let source = full
            .get(name.as_str())
            .unwrap_or_else(|| panic!("{label}: unexpected leaf {name}"));
        let held = fs::read(scratch.destination().join(relative(&path))).unwrap();
        assert!(
            held.len() <= source.len() && held[..] == source[..held.len()],
            "{label}: {name} holds {} bytes that are not a prefix of its source",
            held.len()
        );
    }
    temporaries
}

fn scenario(point: Point, nth: u64) {
    let label = format!("{}:{nth} prefix extend", point.name());
    let scratch = Scratch::new(&format!("{}-prefix", point.name().replace('.', "-")));
    let mut full = BTreeMap::new();
    for (index, (name, held, _)) in FILES.iter().enumerate() {
        let bytes = grown(index);
        fs::write(scratch.source().join(name), &bytes[..*held]).unwrap();
        full.insert(*name, bytes);
    }
    assert!(scratch.run().refusals.is_empty(), "{label}: first copy");
    fs::remove_dir_all(scratch.destination_state()).unwrap();
    for (name, bytes) in &full {
        fs::write(scratch.source().join(name), bytes).unwrap();
    }
    settle(&scratch.source());

    let spec = format!("{}:{nth}", point.name());
    let group = crash_child(&scratch, point, &spec, nth > 1, &[]);
    let crash_temporaries = assert_prefixes(&label, &scratch, &full);
    if matches!(
        point,
        Point::ReceivePrefixBeforeAppend
            | Point::ReceivePrefixAfterAppend
            | Point::ReceivePrefixAfterFlush
    ) {
        assert!(
            crash_temporaries >= 1,
            "{label}: the staged tail is still beside its file"
        );
    }

    let resumed = scratch.run();
    assert!(
        resumed.refusals.is_empty(),
        "{label}: {:?}",
        resumed.refusals
    );
    assert_eq!(
        assert_prefixes(&format!("{label} resumed"), &scratch, &full),
        0,
        "{label}: a temporary survived the resume"
    );
    for (name, bytes) in &full {
        assert_eq!(
            &fs::read(scratch.destination().join(name)).unwrap(),
            bytes,
            "{label}: {name} is its source after the resume"
        );
    }
    assert_eq!(
        resumed.temporaries_removed, crash_temporaries as u64,
        "{label}: the sweep must remove exactly the crash's temporaries"
    );
    let again = scratch.run();
    assert!(again.refusals.is_empty(), "{label}: {:?}", again.refusals);
    assert_eq!(again.source_bytes_read, 0, "{label}: a further run reads 0");
    assert_eq!(again.bytes_received, 0, "{label}: a further run receives 0");
    assert_eq!(again.reused, FILES.len() as u64, "{label}: all reused");
    println!(
        "{label}: temporaries_at_crash={crash_temporaries} resume_extended={} resume_completed={} \
         resume_reused={} resume_unrowed_adopted={} resume_bytes_received={}{}",
        resumed.prefix_extended.len(),
        resumed.completed,
        resumed.reused,
        resumed.unrowed_adopted,
        resumed.bytes_received,
        group
            .map(|group| format!(" crash_{group}"))
            .unwrap_or_default(),
    );
}
