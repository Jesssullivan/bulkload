//! Whether a rollout's events are within the file its live link holds.
//!
//! homes-20261021 held 7,844 Codex rollouts (`*.jsonl.zst`) whose live
//! paths on sting are links into an archive root (`--replace-archived-dest`):
//! "the live path is a link or another kind". An archive is never modified
//! (A-106, TIN-5268) and a link is never replaced by a picked winner (SPEC
//! rule 4), so such a seat is judged here and nothing is written:
//!
//! - **covered**: every event of the source is in the linked file, each as
//!   often as the source holds it (a multiset subset);
//! - otherwise held, naming how many of the source's events the linked
//!   file lacks.
//!
//! An event is one line, compared as its JSON value after normalizing:
//! - the top-level `"ordinal"` key is dropped (Codex 0.16x numbers every
//!   line it writes, and its rollout migration rewrites a legacy file with
//!   them, so the same event carries an ordinal on one side only or a
//!   different one);
//! - objects compare by key, in any order (Codex writes some maps in hash
//!   order);
//! - host paths are translated through the `--map` set on both sides, so a
//!   path one side wrote as the source host's and the other as the
//!   destination's is the same path (SPEC: host paths translated through
//!   the maps).
//!
//! A line that is not a JSON document is compared by its bytes. Each side
//! is read as written or, when it starts with a zstd frame, decompressed;
//! both are streamed. The source's events are kept as their BLAKE3 with a
//! count, never the lines, and its decompressed bytes are bounded by
//! [`super::append_log::SIZE_LIMIT`]; the linked file is read line by line
//! (one line bounded the same way) and only until every source event is
//! found. A linked line whose leading `"timestamp"` no source event holds
//! is skipped without parsing: two equal events hold the same timestamp.
//! A raw line whose leading timestamp is not UTF-8 makes no skip.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};

use super::json;
use super::value::{Maps, Value};
use crate::{BulkloadRefusal, Result};

/// The zstd frame magic, little-endian `0xFD2FB528`.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// The top-level key dropped from every event before it is compared.
const ORDINAL: &str = "ordinal";

/// What the linked file holds of the source's events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Every one of the source's `events` is in the linked file.
    Covered { events: u64 },
    /// The linked file lacks `lacking` of the source's `events`.
    /// `ordinal_rewrite`: every source line carries an ordinal and no
    /// linked line read does (Codex's rollout migration rewrote the source
    /// from a legacy file such as the linked one).
    Lacks {
        lacking: u64,
        events: u64,
        ordinal_rewrite: bool,
    },
    /// A side is over [`super::append_log::SIZE_LIMIT`] decompressed (the
    /// source), or holds a line over it.
    OverLimit,
}

/// The reason a covered seat is recorded with.
pub const COVERED: &str = "covered: source events within the linked file, ordinal-normalized";

impl Coverage {
    /// Why a seat that is not covered is held, for the closure.
    #[must_use]
    pub fn held_reason(&self) -> Option<String> {
        match *self {
            Self::Covered { .. } => None,
            Self::Lacks {
                lacking,
                events,
                ordinal_rewrite,
            } => Some(format!(
                "source holds {lacking} of its {events} events the linked file lacks{}",
                if ordinal_rewrite {
                    " (the source is Codex's ordinal rewrite of a legacy rollout; the link holds the legacy one)"
                } else {
                    ""
                }
            )),
            Self::OverLimit => Some(format!(
                "a rollout linked into an archive root: over the {} MiB its event comparison reads",
                super::append_log::SIZE_LIMIT >> 20
            )),
        }
    }
}

/// Reads a file from offset 0 by position, never moving its shared offset
/// (the staged file and the archive target are both read this way).
struct At<'f> {
    file: &'f File,
    at: u64,
}

impl Read for At<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::unix::fs::FileExt as _;
        let read = self.file.read_at(buf, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

/// `file`'s lines, decompressed when it starts with a zstd frame.
fn reader(file: &File) -> Result<Box<dyn BufRead + '_>> {
    use std::os::unix::fs::FileExt as _;
    let mut head = [0_u8; 4];
    let mut got = 0;
    while let Some(rest) = head.get_mut(got..) {
        if rest.is_empty() {
            break;
        }
        let read = file
            .read_at(rest, got as u64)
            .map_err(|error| crate::refuse::io(&error, "consolidate::rollout"))?;
        if read == 0 {
            break;
        }
        got += read;
    }
    let plain = At { file, at: 0 };
    if got == head.len() && head == ZSTD_MAGIC {
        let decoder = zstd::stream::read::Decoder::new(plain)
            .map_err(|error| crate::refuse::io(&error, "consolidate::rollout"))?;
        Ok(Box::new(BufReader::with_capacity(1 << 20, decoder)))
    } else {
        Ok(Box::new(BufReader::with_capacity(1 << 20, plain)))
    }
}

/// The next line of `lines` into `line` (its `\n` dropped): `Ok(false)` at
/// the end, `Ok(true)` with `over` set when the line is over `limit`.
fn next_line(
    lines: &mut dyn BufRead,
    line: &mut Vec<u8>,
    limit: u64,
    over: &mut bool,
) -> Result<bool> {
    line.clear();
    let read = lines
        .take(limit.saturating_add(1))
        .read_until(b'\n', line)
        .map_err(|error| crate::refuse::io(&error, "consolidate::rollout"))?;
    if read == 0 {
        return Ok(false);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if read as u64 > limit {
        *over = true;
    }
    Ok(true)
}

/// The `"timestamp"` a line leads with, as written, when it starts
/// `{"timestamp":"` and the value has no escape.
fn leading_timestamp(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"{\"timestamp\":\"")?;
    let end = rest
        .iter()
        .position(|byte| *byte == b'"' || *byte == b'\\')?;
    let (value, after) = rest.split_at(end);
    (after.first() == Some(&b'"')).then_some(value)
}

/// One line's event: its identity, and every top-level `"timestamp"`
/// string it holds (a parsed object's, or a raw line's leading one), and
/// whether it carried an ordinal.
struct Event {
    identity: [u8; 32],
    timestamps: Vec<Vec<u8>>,
    ordinal: bool,
}

fn event(line: &[u8], maps: &Maps) -> Event {
    let parsed = std::str::from_utf8(line)
        .ok()
        .and_then(|text| json::parse(text).ok());
    let Some(value) = parsed else {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"raw\0");
        hasher.update(line);
        return Event {
            identity: *hasher.finalize().as_bytes(),
            timestamps: leading_timestamp(line)
                .and_then(|stamp| std::str::from_utf8(stamp).ok())
                .map(|stamp| vec![maps.string(stamp).into_bytes()])
                .unwrap_or_default(),
            ordinal: false,
        };
    };
    let (value, ordinal) = match value {
        Value::Object(mut entries) => {
            let before = entries.len();
            entries.retain(|(key, _)| key != ORDINAL);
            let ordinal = entries.len() != before;
            (Value::Object(entries), ordinal)
        }
        other => (other, false),
    };
    let value = maps.value(&value).unwrap_or(value);
    let timestamps = match &value {
        Value::Object(entries) => entries
            .iter()
            .filter_map(|(key, value)| match value {
                Value::Str(stamp) if key == "timestamp" => Some(stamp.as_bytes().to_vec()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"json\0");
    feed(&mut hasher, &value);
    Event {
        identity: *hasher.finalize().as_bytes(),
        timestamps,
        ordinal,
    }
}

/// Feed `value` to `hasher` in a canonical form: an object's keys sorted,
/// every string and token length-prefixed, an integer by its value.
fn feed(hasher: &mut blake3::Hasher, value: &Value) {
    let text = |hasher: &mut blake3::Hasher, tag: &[u8], text: &[u8]| {
        hasher.update(tag);
        hasher.update(&(text.len() as u64).to_le_bytes());
        hasher.update(text);
    };
    match value {
        Value::Object(entries) => {
            let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            hasher.update(b"o");
            hasher.update(&(sorted.len() as u64).to_le_bytes());
            for (key, value) in sorted {
                text(hasher, b"k", key.as_bytes());
                feed(hasher, value);
            }
        }
        Value::Array(items) => {
            hasher.update(b"a");
            hasher.update(&(items.len() as u64).to_le_bytes());
            for item in items {
                feed(hasher, item);
            }
        }
        Value::Str(value) => text(hasher, b"s", value.as_bytes()),
        Value::Int(token) => match super::value::integer(token) {
            Some(number) => text(hasher, b"i", number.to_string().as_bytes()),
            None => text(hasher, b"I", token.as_bytes()),
        },
        Value::Float(token) => text(hasher, b"f", token.as_bytes()),
        Value::Bool(true) => {
            hasher.update(b"T");
        }
        Value::Bool(false) => {
            hasher.update(b"F");
        }
        Value::Null => {
            hasher.update(b"n");
        }
        Value::Datetime(token) => text(hasher, b"d", token.as_bytes()),
    }
}

/// Whether the file `linked` holds every event of the file `source` (see
/// the module): both read from offset 0, neither written.
///
/// # Errors
/// The I/O refusal of a read, or a zstd stream that does not decode.
pub fn coverage(source: &File, linked: &File, maps: &Maps) -> Result<Coverage> {
    let limit = super::append_log::SIZE_LIMIT;
    let mut wanted: HashMap<[u8; 32], u64> = HashMap::new();
    let mut stamps: HashSet<Vec<u8>> = HashSet::new();
    let mut untimed = false;
    let (mut events, mut ordinals, mut total) = (0_u64, 0_u64, 0_u64);
    let mut line = Vec::new();
    let mut over = false;
    {
        let mut lines = reader(source)?;
        while next_line(&mut *lines, &mut line, limit, &mut over)? {
            total = total.saturating_add(line.len() as u64 + 1);
            if over || total > limit {
                return Ok(Coverage::OverLimit);
            }
            if line.is_empty() {
                continue;
            }
            let found = event(&line, maps);
            events += 1;
            ordinals += u64::from(found.ordinal);
            if found.timestamps.is_empty() {
                untimed = true;
            }
            stamps.extend(found.timestamps);
            *wanted.entry(found.identity).or_default() += 1;
        }
    }
    let mut lacking = events;
    let mut linked_ordinal = false;
    let mut lines = reader(linked)?;
    while lacking > 0 && next_line(&mut *lines, &mut line, limit, &mut over)? {
        if over {
            return Ok(Coverage::OverLimit);
        }
        if line.is_empty() {
            continue;
        }
        if !untimed {
            let stamp = leading_timestamp(&line).and_then(|stamp| std::str::from_utf8(stamp).ok());
            if stamp.is_some_and(|stamp| !stamps.contains(maps.string(stamp).as_bytes())) {
                continue;
            }
        }
        let found = event(&line, maps);
        linked_ordinal |= found.ordinal;
        if let Some(count) = wanted.get_mut(&found.identity) {
            if *count > 0 {
                *count -= 1;
                lacking -= 1;
            }
        }
    }
    if lacking == 0 {
        return Ok(Coverage::Covered { events });
    }
    Ok(Coverage::Lacks {
        lacking,
        events,
        ordinal_rewrite: events > 0 && ordinals == events && !linked_ordinal,
    })
}

/// The ledger's record of each linked rollout held, so an unchanged seat
/// against an unchanged linked file is held again, with its reason, and
/// nothing read.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rollout_link_held(
    file BLOB NOT NULL,
    seat_key BLOB NOT NULL,
    linked BLOB NOT NULL,
    reason TEXT NOT NULL,
    at_ns INTEGER NOT NULL,
    PRIMARY KEY(file, seat_key)
);";

/// Record that the seat `seat_key` at `rel` was held with `reason`
/// against the linked file whose identity digest is `linked`.
///
/// # Errors
/// The `SQLite` refusal.
pub fn remember_held(
    ledger: &super::Ledger,
    rel: &[u8],
    seat_key: &[u8],
    linked: &[u8; 32],
    reason: &str,
) -> Result<()> {
    let conn = ledger.connection();
    conn.execute_batch(SCHEMA)
        .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    let at_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        });
    conn.execute(
        "INSERT INTO rollout_link_held VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(file, seat_key) DO UPDATE SET
             linked = excluded.linked, reason = excluded.reason, at_ns = excluded.at_ns",
        rusqlite::params![rel, seat_key, &linked[..], reason, at_ns],
    )
    .map(|_| ())
    .map_err(|_| BulkloadRefusal::SqliteStateChanged)
}

/// The reason the seat `seat_key` at `rel` was held against the linked
/// file `linked`, when it was and that file is unchanged.
///
/// # Errors
/// The `SQLite` refusal.
pub fn held_before(
    ledger: &super::Ledger,
    rel: &[u8],
    seat_key: &[u8],
    linked: &[u8; 32],
) -> Result<Option<String>> {
    use rusqlite::OptionalExtension as _;
    let conn = ledger.connection();
    conn.execute_batch(SCHEMA)
        .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    conn.query_row(
        "SELECT reason FROM rollout_link_held WHERE file = ?1 AND seat_key = ?2 AND linked = ?3",
        rusqlite::params![rel, seat_key, &linked[..]],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|_| BulkloadRefusal::SqliteStateChanged)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn file(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    fn zst(bytes: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(bytes, 3).unwrap()
    }

    const LEGACY: &[u8] = b"{\"timestamp\":\"t0\",\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"cwd\":\"/home/jess/git/x\"}}\n\
{\"timestamp\":\"t1\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hi\"}}\n\
{\"timestamp\":\"t2\",\"type\":\"event_msg\",\"payload\":{\"changes\":{\"a\":1,\"b\":2}}}\n\
{\"timestamp\":\"t3\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n";

    #[test]
    fn ordinals_key_order_and_maps_are_normalized() {
        // The source: ordinals, one map written in the other order, and the
        // source host's path; it is the linked file's first three events.
        let source = b"{\"timestamp\":\"t0\",\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"cwd\":\"/Users/jess/git/x\"}}\n\
{\"timestamp\":\"t1\",\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hi\"}}\n\
{\"timestamp\":\"t2\",\"ordinal\":2,\"type\":\"event_msg\",\"payload\":{\"changes\":{\"b\":2,\"a\":1}}}\n";
        let maps = Maps::new([("/Users/jess".to_owned(), "/home/jess".to_owned())]);
        let (source, linked) = (file(source), file(LEGACY));
        assert_eq!(
            coverage(&source, &linked, &maps).unwrap(),
            Coverage::Covered { events: 3 }
        );
        // Without the map the session_meta's cwd is an event the link lacks.
        assert_eq!(
            coverage(&source, &linked, &Maps::default()).unwrap(),
            Coverage::Lacks {
                lacking: 1,
                events: 3,
                ordinal_rewrite: true
            }
        );
    }

    #[test]
    fn zst_on_either_side_is_decompressed_and_counted_as_a_multiset() {
        let source = b"{\"timestamp\":\"t1\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hi\"}}\n";
        let twice = [&source[..], &source[..]].concat();
        let linked = file(&zst(LEGACY));
        let maps = Maps::default();
        assert_eq!(
            coverage(&file(&zst(source)), &linked, &maps).unwrap(),
            Coverage::Covered { events: 1 }
        );
        assert_eq!(
            coverage(&file(source), &linked, &maps).unwrap(),
            Coverage::Covered { events: 1 }
        );
        // The link holds the event once; the source twice.
        assert_eq!(
            coverage(&file(&zst(&twice)), &linked, &maps).unwrap(),
            Coverage::Lacks {
                lacking: 1,
                events: 2,
                ordinal_rewrite: false
            }
        );
    }

    #[test]
    fn an_event_the_link_lacks_is_counted_and_raw_lines_compare_by_bytes() {
        let source = [
            LEGACY,
            b"{\"timestamp\":\"t4\",\"type\":\"event_msg\",\"payload\":{\"type\":\"new\"}}\n",
            b"not json \xff\n",
        ]
        .concat();
        let maps = Maps::default();
        assert_eq!(
            coverage(&file(&zst(&source)), &file(&zst(LEGACY)), &maps).unwrap(),
            Coverage::Lacks {
                lacking: 2,
                events: 6,
                ordinal_rewrite: false
            }
        );
        let linked = [LEGACY, b"not json \xff\n"].concat();
        assert_eq!(
            coverage(&file(b"not json \xff\n"), &file(&linked), &maps).unwrap(),
            Coverage::Covered { events: 1 }
        );
        // A rewritten event (other fields, other timestamp) is not the same.
        let rewritten = b"{\"timestamp\":\"t9\",\"ordinal\":0,\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n";
        let lacks = coverage(&file(rewritten), &file(LEGACY), &maps).unwrap();
        assert_eq!(
            lacks,
            Coverage::Lacks {
                lacking: 1,
                events: 1,
                ordinal_rewrite: true
            }
        );
        assert!(lacks.held_reason().unwrap().starts_with(
            "source holds 1 of its 1 events the linked file lacks (the source is Codex's ordinal rewrite"
        ));
    }

    #[test]
    fn leading_timestamps_are_read_only_when_plain() {
        assert_eq!(
            leading_timestamp(b"{\"timestamp\":\"2026-07-22T04:39:31.644Z\",\"type\":1}"),
            Some(&b"2026-07-22T04:39:31.644Z"[..])
        );
        assert_eq!(leading_timestamp(b"{\"timestamp\":\"a\\\"b\"}"), None);
        assert_eq!(leading_timestamp(b"{\"type\":\"x\"}"), None);
    }
}
