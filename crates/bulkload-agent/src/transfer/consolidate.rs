//! Consolidation of append logs: one file at the path, never a beside copy.
//!
//! Operator ruling 2026-10-20: "neither WIN. they must be consolidated";
//! "no dupes, no duplicated besides, no stage dupes"; "a duplicate is
//! inherently not path for path / byte for byte"; SPEC RUL-10, lossless
//! union.
//!
//! Under `--beside` with consolidation on ([`super::set_consolidate`]), an
//! append-only JSON Lines file (`.jsonl`: Claude transcripts and their
//! subagents, Codex rollouts, `history.jsonl`, any other) whose live copy is
//! neither the seat's bytes, nor a prefix of them (extended), nor holds them
//! (covered, `dest-ahead`) is never landed beside. It is consolidated in
//! place: one file at the path, the seat's bytes whole as its prefix (so
//! where the histories are shared the source's bytes stand, and a later pass
//! finds the file covering the seat), then every record of the live file
//! past the two files' common prefix that the seat does not hold, in the live
//! file's order (an append log's order is its time order).
//!
//! A record's identity:
//! - a Claude transcript record (under `projects/`) with a top-level `uuid`:
//!   that `uuid`, so a record both sides hold is kept once, the seat's bytes;
//! - any other line (Codex rollouts, whose records carry their timestamp and
//!   payload, `history.jsonl`, a transcript record with no `uuid`): the exact
//!   line, counted, so a line the live file holds more often than the seat's
//!   continuation is kept that many more times.
//!
//! Both branches of a session continued on both hosts are kept: Claude Code
//! links each record to its parent by `parentUuid`, and a resume follows a
//! leaf, so the second branch is one more leaf of the same tree (record
//! shape relied on: each line one JSON object, `uuid` and `parentUuid` top
//! level; not checked against a live `claude --resume` here).
//!
//! Either input ending inside a line (a writer at it) is not merged: the
//! caller refuses the entry `DESTINATION_UNSETTLED`, and the next pass
//! decides it.

use std::collections::{HashMap, HashSet};

/// What a merge adds after the seat's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// The live file's records the seat does not hold, whole lines, in the
    /// live file's order.
    pub extra: Vec<u8>,
    /// The seat's records (lines).
    pub seat_records: u64,
    /// The live file's records kept after the seat's bytes.
    pub live_extra: u64,
}

fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

fn uuid(line: &[u8]) -> Option<String> {
    crate::reseat::json::nested_string(line, &["uuid"])
}

/// The merge of the live file `live` into the seat's bytes `seat` (see the
/// module), `by_uuid` for a Claude transcript; `None` when either ends
/// inside a line.
#[must_use]
pub fn merge_jsonl(seat: &[u8], live: &[u8], by_uuid: bool) -> Option<Merged> {
    let whole = |bytes: &[u8]| bytes.is_empty() || bytes.ends_with(b"\n");
    if !whole(seat) || !whole(live) {
        return None;
    }
    // The common prefix, cut back to its last line end.
    let shared = seat
        .iter()
        .zip(live)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| seat.len().min(live.len()));
    let common = seat
        .get(..shared)
        .and_then(|head| head.iter().rposition(|byte| *byte == b'\n'))
        .map_or(0, |at| at + 1);
    let mut seat_uuids: HashSet<String> = HashSet::new();
    let mut seat_lines: HashMap<&[u8], u64> = HashMap::new();
    let mut seat_records = 0_u64;
    let mut at = 0_usize;
    for line in lines(seat) {
        seat_records += 1;
        let id = if by_uuid { uuid(line) } else { None };
        match id {
            Some(id) => {
                seat_uuids.insert(id);
            }
            None if at >= common => *seat_lines.entry(line).or_default() += 1,
            None => {}
        }
        at += line.len();
    }
    let mut extra = Vec::new();
    let mut live_extra = 0_u64;
    let mut kept: HashSet<String> = HashSet::new();
    for line in lines(live.get(common..).unwrap_or_default()) {
        let id = if by_uuid { uuid(line) } else { None };
        let keep = id.map_or_else(
            || match seat_lines.get_mut(line) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    false
                }
                _ => true,
            },
            |id| !seat_uuids.contains(&id) && kept.insert(id),
        );
        if keep {
            extra.extend_from_slice(line);
            live_extra += 1;
        }
    }
    Some(Merged {
        extra,
        seat_records,
        live_extra,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::merge_jsonl;

    fn record(uuid: &str, parent: Option<&str>, text: &str) -> String {
        let parent = parent.map_or_else(|| "null".to_owned(), |p| format!("\"{p}\""));
        format!("{{\"parentUuid\":{parent},\"uuid\":\"{uuid}\",\"message\":\"{text}\"}}\n")
    }

    /// One session continued on both hosts: one file, the seat's bytes as
    /// its prefix, then the live branch; a record both hold kept once.
    #[test]
    fn a_session_continued_on_both_sides_keeps_both_branches() {
        let shared = format!(
            "{}{}",
            record("a", None, "hi"),
            record("b", Some("a"), "hello")
        );
        let neo = format!("{shared}{}", record("n1", Some("b"), "on neo"));
        let sting = format!(
            "{shared}{}{}",
            record("s1", Some("b"), "on sting"),
            record("s2", Some("s1"), "more sting")
        );
        let merged = merge_jsonl(neo.as_bytes(), sting.as_bytes(), true).unwrap();
        assert_eq!(
            String::from_utf8(merged.extra).unwrap(),
            format!(
                "{}{}",
                record("s1", Some("b"), "on sting"),
                record("s2", Some("s1"), "more sting")
            )
        );
        assert_eq!((merged.seat_records, merged.live_extra), (3, 2));
        // A record both sides hold (same uuid), bytes differing: once, the
        // seat's.
        let sting = format!("{shared}{}", record("n1", Some("b"), "on neo, rewritten"));
        let merged = merge_jsonl(neo.as_bytes(), sting.as_bytes(), true).unwrap();
        assert!(merged.extra.is_empty());
    }

    /// `history.jsonl` and other logs: the exact line, counted.
    #[test]
    fn lines_are_unioned_by_their_bytes_and_counted() {
        let neo = "{\"display\":\"a\"}\n{\"display\":\"n\"}\n";
        let sting =
            "{\"display\":\"a\"}\n{\"display\":\"s\"}\n{\"display\":\"n\"}\n{\"display\":\"n\"}\n";
        let merged = merge_jsonl(neo.as_bytes(), sting.as_bytes(), false).unwrap();
        assert_eq!(
            String::from_utf8(merged.extra).unwrap(),
            "{\"display\":\"s\"}\n{\"display\":\"n\"}\n"
        );
        assert_eq!(merged.live_extra, 2);
    }

    /// A file a writer is at (ending inside a line) is not merged.
    #[test]
    fn an_input_ending_inside_a_line_is_not_merged() {
        assert!(merge_jsonl(b"{\"a\":1}\n{\"b\"", b"{\"a\":1}\n", false).is_none());
        assert!(merge_jsonl(b"{\"a\":1}\n", b"{\"a\":1}\n{\"c\"", false).is_none());
        assert!(merge_jsonl(b"", b"", false).is_some());
    }
}
