//! The append-log [`Union`](super::Union), the #294 lane's.
//!
//! Claude transcripts and their subagents, Codex rollouts, `history.jsonl`,
//! any `*.jsonl` (operator ruling 2026-10-20: "neither WIN. they must be
//! consolidated"; "no dupes, no duplicated besides, no stage dupes").
//!
//! One file at the path, never a beside copy. The live file and the
//! source's are compared:
//! - equal: adopted;
//! - the live file holds the source's bytes as its prefix, or every one of
//!   its records: covered, nothing written;
//! - the live file is a prefix of the source's, at a line end: extended to
//!   the source's bytes;
//! - else consolidated: the source's bytes whole, then the live file's
//!   records the source lacks, in the live file's order. A record's
//!   identity is [`crate::transfer::consolidate`]'s (the receive path writes
//!   the same union): a Claude transcript record's `uuid`, else the exact
//!   line, counted. Both branches of a session continued on both hosts are
//!   kept.
//!
//! Nothing is held wholly in memory (a transcript can be over 100 MB, and
//! passes run in parallel): the comparison streams both files, a record's
//! identity is kept as its BLAKE3, and the output is written by streaming.
//! - **Extension** (the live file a prefix of the source's): the missing
//!   tail is streamed onto the same inode through `O_APPEND`.
//! - **Union** (the live file holds records the source lacks): the source's
//!   bytes are the output's head. The receive streams the wire straight
//!   into the result's private temporary in the live file's directory
//!   ([`Feed::Staged`]); a fold copies its source there. The live file's
//!   extra records are appended, and the temporary is made durable and
//!   traded in for the live file, checked against the live file as read (a
//!   writer that changed it is merged again; one that changes it on every
//!   read is `HOMES_CONSOLIDATE_RACED`).
//! - **Covered and equal** are decided by the streamed comparison: nothing
//!   is written.
//!
//! A live file a
//! process holds open ([`crate::procs::inside`], or where that cannot be
//! said) or that ends inside a line (a writer at it) is
//! `HOMES_CONSOLIDATE_RACED` too, retryable, nothing written: a writer's
//! open handle would otherwise write on into the file traded out. Each
//! extension and consolidation is ledgered (`append_log_union`: both
//! inputs' and the output's BLAKE3, the record counts) once its bytes are
//! durable.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::fd::AsFd as _;
use std::path::Path;

use super::config;
use super::{Action, Ledger, Live, Outcome, Source};
use crate::io::{self, sys, Stat};
use crate::refuse::RefuseAt as _;
use crate::transfer::consolidate::merge_jsonl;
use crate::{BulkloadRefusal, Result};

const SITE: &str = "consolidate::append_log";

/// The largest append log merged, either side; a larger one is held.
pub const SIZE_LIMIT: u64 = 1 << 30;

/// The ledger's record of each append log written.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS append_log_union(
    file BLOB NOT NULL,
    action TEXT NOT NULL,
    live_blake3 BLOB NOT NULL,
    source_blake3 BLOB NOT NULL,
    output_blake3 BLOB NOT NULL,
    source_records INTEGER NOT NULL,
    live_extra INTEGER NOT NULL,
    output_size INTEGER NOT NULL,
    at_ns INTEGER NOT NULL,
    PRIMARY KEY(file, live_blake3, source_blake3)
);";

/// A union of two append logs, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub action: Action,
    /// What the live file is to hold ([`Action::Extended`],
    /// [`Action::Consolidated`]).
    pub bytes: Option<Vec<u8>>,
    /// The source's records, and the live file's records kept after them.
    pub source_records: u64,
    pub live_extra: u64,
}

/// Whether the records of the append log at `rel` are identified by their
/// `uuid`: a Claude transcript, under `.claude/projects/` (or `projects/`
/// of a `.claude` root).
#[must_use]
pub fn by_uuid(rel: &[u8]) -> bool {
    const UNDER: &[u8] = b".claude/projects/";
    rel.starts_with(b"projects/")
        || rel.starts_with(UNDER)
        || rel
            .windows(UNDER.len() + 1)
            .any(|window| window.first() == Some(&b'/') && window.get(1..) == Some(UNDER))
}

/// The union of `source` into `live` (see the module), nothing read or
/// written; `None` when either ends inside a line and neither holds the
/// other.
#[must_use]
pub fn plan(live: &[u8], source: &[u8], by_uuid: bool) -> Option<Plan> {
    let nothing = |action| Plan {
        action,
        bytes: None,
        source_records: 0,
        live_extra: 0,
    };
    if live == source {
        return Some(nothing(Action::Adopted));
    }
    if live.starts_with(source) {
        return Some(nothing(Action::Covered));
    }
    let whole = |bytes: &[u8]| bytes.is_empty() || bytes.ends_with(b"\n");
    if !whole(live) || !whole(source) {
        return None;
    }
    if source.starts_with(live) {
        return Some(Plan {
            action: Action::Extended,
            bytes: Some(source.to_vec()),
            source_records: source.split_inclusive(|byte| *byte == b'\n').count() as u64,
            live_extra: 0,
        });
    }
    // Every record of the source's already held: nothing to add.
    if merge_jsonl(live, source, by_uuid)?.extra.is_empty() {
        return Some(nothing(Action::Covered));
    }
    let merged = merge_jsonl(source, live, by_uuid)?;
    let mut bytes = source.to_vec();
    bytes.extend_from_slice(&merged.extra);
    Some(Plan {
        action: Action::Consolidated,
        bytes: Some(bytes),
        source_records: merged.seat_records,
        live_extra: merged.live_extra,
    })
}

/// The `--plan-only` row of the append log at `rel`.
///
/// #297's homes listing (class, action, sizes) takes it; the #294 lane's
/// would-write listing is this row, not a listing of its own. It is
/// [`plan`] keyed as [`union`] keys it, and a writer at either side's last
/// line is held `append-log: ends inside a line` (the union refuses that
/// `HOMES_CONSOLIDATE_RACED`, retryable).
#[must_use]
pub fn plan_at(rel: &[u8], live: &[u8], source: &[u8]) -> Plan {
    plan(live, source, by_uuid(rel)).unwrap_or(Plan {
        action: Action::Held("append-log: ends inside a line"),
        bytes: None,
        source_records: 0,
        live_extra: 0,
    })
}

/// Where a streaming union's source bytes are.
#[derive(Clone, Copy)]
pub enum Feed<'a> {
    /// The receive's staged file: a private temporary in the live file's
    /// directory, `name` there, holding the source's bytes. A union is
    /// written into it (its extra records appended) and it is traded in, so
    /// the source's bytes are never copied a second time. It is the caller's
    /// to discard after.
    Staged { file: &'a File, name: &'a CStr },
    /// An existing file (a fold's source): copied into a private temporary
    /// only when a union is written.
    File(&'a File),
    /// Bytes in memory (#297's [`Source`]).
    Bytes(&'a [u8]),
}

impl Feed<'_> {
    fn read_at(&self, buffer: &mut [u8], at: u64) -> Result<usize> {
        match self {
            Self::Staged { file, .. } | Self::File(file) => {
                sys::pread_full(*file, buffer, at).refuse_at(SITE)
            }
            Self::Bytes(bytes) => {
                let start = usize::try_from(at).unwrap_or(usize::MAX).min(bytes.len());
                let piece = bytes.get(start..).unwrap_or_default();
                let length = piece.len().min(buffer.len());
                if let (Some(to), Some(from)) = (buffer.get_mut(..length), piece.get(..length)) {
                    to.copy_from_slice(from);
                }
                Ok(length)
            }
        }
    }
}

/// Bytes read per step.
const PIECE: usize = 1 << 20;

/// A positioned read of one side: bytes into the buffer from an offset.
type ReadAt<'a> = &'a dyn Fn(&mut [u8], u64) -> Result<usize>;

/// Read `[at, end)` of `read` piece by piece.
fn pieces(
    read: &dyn Fn(&mut [u8], u64) -> Result<usize>,
    (mut at, end): (u64, u64),
    each: &mut dyn FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    let mut buffer = vec![0_u8; PIECE];
    while at < end {
        let want = usize::try_from(end - at).unwrap_or(PIECE).min(PIECE);
        let slot = buffer.get_mut(..want).unwrap_or_default();
        let got = read(slot, at)?;
        if got == 0 {
            return Err(BulkloadRefusal::HomesConsolidateRaced);
        }
        each(at, slot.get(..got).unwrap_or_default())?;
        at += got as u64;
    }
    Ok(())
}

/// Every line of `[from, end)`, with its offset.
fn lines(
    read: &dyn Fn(&mut [u8], u64) -> Result<usize>,
    (from, end): (u64, u64),
    each: &mut dyn FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    let mut carry: Vec<u8> = Vec::new();
    let mut start = from;
    pieces(read, (from, end), &mut |_, piece| {
        let mut rest = piece;
        while let Some(at) = rest.iter().position(|byte| *byte == b'\n') {
            let (line, after) = rest.split_at(at + 1);
            carry.extend_from_slice(line);
            each(start, &carry)?;
            start += carry.len() as u64;
            carry.clear();
            rest = after;
        }
        carry.extend_from_slice(rest);
        Ok(())
    })?;
    if !carry.is_empty() {
        each(start, &carry)?;
    }
    Ok(())
}

/// A record's identity: its `uuid` (keyed logs), else its exact line.
enum Id {
    Uuid([u8; 32]),
    Line([u8; 32]),
}

fn id(line: &[u8], keyed: bool) -> Id {
    keyed
        .then(|| crate::reseat::json::nested_string(line, &["uuid"]))
        .flatten()
        .map_or_else(
            || Id::Line(*blake3::hash(line).as_bytes()),
            |uuid| Id::Uuid(*blake3::hash(uuid.as_bytes()).as_bytes()),
        )
}

/// One side's records: every `uuid`, and the exact lines past the common
/// prefix, counted.
#[derive(Default)]
struct Records {
    uuids: HashSet<[u8; 32]>,
    lines: HashMap<[u8; 32], u64>,
    count: u64,
}

impl Records {
    fn of(
        read: &dyn Fn(&mut [u8], u64) -> Result<usize>,
        size: u64,
        common: u64,
        keyed: bool,
    ) -> Result<Self> {
        let mut records = Self::default();
        lines(read, (0, size), &mut |at, line| {
            records.count += 1;
            match id(line, keyed) {
                Id::Uuid(uuid) => {
                    records.uuids.insert(uuid);
                }
                Id::Line(line) if at >= common => *records.lines.entry(line).or_default() += 1,
                Id::Line(_) => {}
            }
            Ok(())
        })?;
        Ok(records)
    }

    /// Whether `line` (past the common prefix of the other side) is one of
    /// these records; a counted line is taken.
    fn holds(&mut self, line: &[u8], keyed: bool) -> bool {
        match id(line, keyed) {
            Id::Uuid(uuid) => self.uuids.contains(&uuid),
            Id::Line(line) => match self.lines.get_mut(&line) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    true
                }
                _ => false,
            },
        }
    }
}

/// What the streamed comparison decided.
enum Decided {
    Adopted,
    Covered,
    Extended,
    /// The live file's records past `common` the source lacks, as `(offset,
    /// length)`, with the counts.
    Union {
        keep: Vec<(u64, u64)>,
        source_records: u64,
    },
}

/// Compare the live file and the source, streaming (see the module).
/// `None`: either ends inside a line and neither holds the other.
fn decide(
    live: &dyn Fn(&mut [u8], u64) -> Result<usize>,
    live_size: u64,
    source: &dyn Fn(&mut [u8], u64) -> Result<usize>,
    size: u64,
    keyed: bool,
) -> Result<Option<Decided>> {
    // The common prefix, and its last line end.
    let both = live_size.min(size);
    let (mut shared, mut cut) = (0_u64, 0_u64);
    let mut other = vec![0_u8; PIECE];
    let mut differs = false;
    pieces(source, (0, both), &mut |at, piece| {
        if differs {
            return Ok(());
        }
        let theirs = other.get_mut(..piece.len()).unwrap_or_default();
        if live(theirs, at)? != piece.len() {
            return Err(BulkloadRefusal::HomesConsolidateRaced);
        }
        let same = piece
            .iter()
            .zip(theirs.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(piece.len());
        if let Some(nl) = piece
            .get(..same)
            .and_then(|head| head.iter().rposition(|byte| *byte == b'\n'))
        {
            cut = at + nl as u64 + 1;
        }
        shared = at + same as u64;
        differs = same < piece.len();
        Ok(())
    })?;
    if shared == size && live_size == size {
        return Ok(Some(Decided::Adopted));
    }
    if shared == size {
        return Ok(Some(Decided::Covered));
    }
    let ends_whole =
        |read: &dyn Fn(&mut [u8], u64) -> Result<usize>, length: u64| -> Result<bool> {
            if length == 0 {
                return Ok(true);
            }
            let mut byte = [0_u8; 1];
            Ok(read(&mut byte, length - 1)? == 1 && byte == [b'\n'])
        };
    if !ends_whole(live, live_size)? || !ends_whole(source, size)? {
        return Ok(None);
    }
    if shared == live_size {
        return Ok(Some(Decided::Extended));
    }
    // Every source record already held: covered.
    let mut held = Records::of(live, live_size, cut, keyed)?;
    let mut lacking = false;
    lines(source, (cut, size), &mut |_, line| {
        lacking = lacking || !held.holds(line, keyed);
        Ok(())
    })?;
    if !lacking {
        return Ok(Some(Decided::Covered));
    }
    drop(held);
    let mut offered = Records::of(source, size, cut, keyed)?;
    let mut keep = Vec::new();
    let mut kept: HashSet<[u8; 32]> = HashSet::new();
    lines(live, (cut, live_size), &mut |at, line| {
        let new = match id(line, keyed) {
            Id::Uuid(uuid) => !offered.uuids.contains(&uuid) && kept.insert(uuid),
            Id::Line(_) => !offered.holds(line, keyed),
        };
        if new {
            keep.push((at, line.len() as u64));
        }
        Ok(())
    })?;
    Ok(Some(Decided::Union {
        keep,
        source_records: offered.count,
    }))
}

/// The append-log [`super::Union`] over #297's in-memory [`Source`] (a
/// small file); the receive and the folds stream ([`union_stream`]).
///
/// # Errors
/// As [`union_stream`].
pub fn union(live: &Live<'_>, source: &Source<'_>, ledger: &Ledger) -> Result<Outcome> {
    union_stream(
        live,
        Feed::Bytes(source.bytes),
        source.bytes.len() as u64,
        ledger,
    )
}

/// Merge the source (`feed`, `size` bytes) into the live append log, by
/// streaming (see the module).
///
/// # Errors
/// - `HOMES_CONSOLIDATE_RACED`: a process holds the live file open (or this
///   platform cannot say), either side ends inside a line, or a writer
///   changed the file on every read.
/// - `DESTINATION_KIND_MISMATCH` for a live name that is no regular file (a
///   link is never followed); `PATH_ESCAPES_ROOT` for another user's file;
///   `DESTINATION_EXCHANGE_UNSUPPORTED` where two names cannot be traded.
/// - The I/O and ledger refusals.
pub fn union_stream(
    live: &Live<'_>,
    feed: Feed<'_>,
    size: u64,
    ledger: &Ledger,
) -> Result<Outcome> {
    let name = io::c_name(live.name).refuse_at(SITE)?;
    let keyed = by_uuid(live.rel);
    let source = |buffer: &mut [u8], at: u64| feed.read_at(buffer, at);
    for _attempt in 1..=config::ATTEMPTS {
        let file = match sys::open_read_at(live.dir, &name) {
            Ok(fd) => File::from(fd),
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                return create(live, &name, feed, size);
            }
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(BulkloadRefusal::DestinationKindMismatch);
            }
            Err(error) => return Err(error).refuse_at(SITE),
        };
        let stat = sys::fstat(&file).refuse_at(SITE)?;
        check_own(&stat)?;
        let read_live =
            |buffer: &mut [u8], at: u64| sys::pread_full(&file, buffer, at).refuse_at(SITE);
        let Some(decided) = decide(&read_live, stat.size, &source, size, keyed)? else {
            return Err(BulkloadRefusal::HomesConsolidateRaced);
        };
        let after = sys::fstat(&file).refuse_at(SITE)?;
        if after.size != stat.size || after.mtime_ns != stat.mtime_ns {
            continue;
        }
        let nothing = |action| Outcome {
            action,
            written: 0,
            recorded: 0,
        };
        match decided {
            Decided::Adopted => return Ok(nothing(Action::Adopted)),
            Decided::Covered => return Ok(nothing(Action::Covered)),
            Decided::Extended => {
                free(&file)?;
                if append_tail(live.dir, &name, &stat, (&source, stat.size, size))? {
                    // The live file now: the source's bytes, its own inode.
                    record(
                        ledger,
                        live.rel,
                        "extend",
                        [(&read_live, stat.size), (&source, size), (&read_live, size)],
                        (0, 0),
                    )?;
                    return Ok(Outcome {
                        action: Action::Extended,
                        written: size - stat.size,
                        recorded: 1,
                    });
                }
            }
            Decided::Union {
                keep,
                source_records,
            } => {
                free(&file)?;
                let extra: u64 = keep.iter().map(|(_, length)| length).sum();
                let output = Output::open(live.dir, live.name, feed, (size, &source))?;
                let wrote = output.append(&read_live, &keep, size);
                if let Err(refusal) = wrote {
                    output.discard(live.dir, size);
                    return Err(refusal);
                }
                if output.trade(live.dir, &name, &stat)? {
                    let made = |buffer: &mut [u8], at: u64| {
                        sys::pread_full(&output.file, buffer, at).refuse_at(SITE)
                    };
                    record(
                        ledger,
                        live.rel,
                        "consolidate",
                        [
                            (&read_live, stat.size),
                            (&source, size),
                            (&made, size + extra),
                        ],
                        (source_records, keep.len() as u64),
                    )?;
                    output.retire(live.dir)?;
                    return Ok(Outcome {
                        action: Action::Consolidated,
                        written: size + extra,
                        recorded: 1,
                    });
                }
                output.discard(live.dir, size);
            }
        }
    }
    Err(BulkloadRefusal::HomesConsolidateRaced)
}

/// This user's own regular file.
fn check_own(stat: &Stat) -> Result<()> {
    if !stat.is_file() {
        return Err(BulkloadRefusal::DestinationKindMismatch);
    }
    if stat.uid != sys::effective_uid() {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok(())
}

/// Land the source where the destination holds no such file, create-only:
/// the staged file itself, or a durable copy.
fn create(live: &Live<'_>, name: &CStr, feed: Feed<'_>, size: u64) -> Result<Outcome> {
    let output = Output::open(
        live.dir,
        live.name,
        feed,
        (size, &|buffer: &mut [u8], at| feed.read_at(buffer, at)),
    )?;
    output.sync()?;
    match io::publish_noreplace(live.dir, &output.name, name) {
        Ok(_) => {}
        Err(error) => {
            output.discard(live.dir, size);
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err(BulkloadRefusal::HomesConsolidateRaced);
            }
            return Err(error).refuse_at(SITE);
        }
    }
    live.dir.sync_all().refuse_at(SITE)?;
    Ok(Outcome {
        action: Action::Created,
        written: size,
        recorded: 0,
    })
}

/// A union's output: the staged file the source was streamed into, or a
/// private temporary holding a copy of the source.
struct Output {
    file: File,
    name: CString,
    /// Whether it is the receive's staged file (the caller's to discard).
    staged: bool,
}

impl Output {
    fn open(
        dir: &File,
        live_name: &[u8],
        feed: Feed<'_>,
        (size, source): (u64, ReadAt<'_>),
    ) -> Result<Self> {
        if let Feed::Staged { file, name } = feed {
            let file = file.try_clone().refuse_at(SITE)?;
            // The staged file holds the source's bytes and nothing past.
            file.set_len(size).refuse_at(SITE)?;
            return Ok(Self {
                file,
                name: name.to_owned(),
                staged: true,
            });
        }
        let mut temp = b".".to_vec();
        temp.extend_from_slice(live_name);
        temp.extend_from_slice(format!(".bulkload-union.{}", std::process::id()).as_bytes());
        let name = io::c_name(&temp).refuse_at(SITE)?;
        let file = File::from(sys::create_excl_at(dir.as_fd(), &name, 0o600).refuse_at(SITE)?);
        let output = Self {
            file,
            name,
            staged: false,
        };
        let copied = pieces(source, (0, size), &mut |at, piece| {
            sys::pwrite_all(&output.file, piece, at).refuse_at(SITE)
        });
        if let Err(refusal) = copied {
            output.discard(dir, size);
            return Err(refusal);
        }
        Ok(output)
    }

    /// Append the live file's `keep` records after the source's `size`
    /// bytes, then make it durable.
    fn append(
        &self,
        live: &dyn Fn(&mut [u8], u64) -> Result<usize>,
        keep: &[(u64, u64)],
        size: u64,
    ) -> Result<()> {
        let mut at = size;
        for (from, length) in keep {
            pieces(live, (*from, from + length), &mut |_, piece| {
                sys::pwrite_all(&self.file, piece, at).refuse_at(SITE)?;
                at += piece.len() as u64;
                Ok(())
            })?;
        }
        self.sync()
    }

    fn sync(&self) -> Result<()> {
        self.file.sync_all().refuse_at(SITE)
    }

    /// Trade it in for the live file if the live file is still `stat`;
    /// `false` (traded back, or never traded): a writer changed it.
    fn trade(&self, dir: &File, name: &CStr, stat: &Stat) -> Result<bool> {
        let unchanged = |now: &Stat| {
            now.node == stat.node && now.size == stat.size && now.mtime_ns == stat.mtime_ns
        };
        #[cfg(test)]
        super::tests::before_swap(dir, name);
        match sys::fstatat_nofollow(dir, name) {
            Ok(now) if unchanged(&now) && now.ctime_ns == stat.ctime_ns => {}
            Ok(_) | Err(_) => return Ok(false),
        }
        if let Err(error) = io::exchange(dir, &self.name, name) {
            if io::rename_unsupported(&error) {
                return Err(BulkloadRefusal::DestinationExchangeUnsupported);
            }
            return Err(error).refuse_at(SITE);
        }
        let out = sys::fstatat_nofollow(dir, &self.name);
        if !out.as_ref().is_ok_and(unchanged) {
            io::exchange(dir, &self.name, name).refuse_at(SITE)?;
            dir.sync_all().refuse_at(SITE)?;
            return Ok(false);
        }
        Ok(true)
    }

    /// After a trade: the old live file, now under the output's name, goes.
    fn retire(&self, dir: &File) -> Result<()> {
        sys::unlinkat(dir, &self.name, false).refuse_at(SITE)?;
        dir.sync_all().refuse_at(SITE)
    }

    /// Undo an output not traded in: a copy goes; the staged file is cut
    /// back to the source's bytes (the caller discards it).
    fn discard(&self, dir: &File, size: u64) {
        if self.staged {
            let _ = self.file.set_len(size);
        } else {
            let _ = sys::unlinkat(dir, &self.name, false);
        }
    }
}

/// Stream the source's `[from, to)` onto the live file in place, through
/// `O_APPEND` (a byte a writer appended first is never written over),
/// checked against the live file as read (`stat`) before and after, then
/// made durable. `false`: the file moved before the write, nothing written.
///
/// # Errors
/// `HOMES_CONSOLIDATE_RACED` when a writer appended between the check and
/// the write: both its bytes and the tail are in the file, and the next
/// pass merges again. The I/O refusals.
fn append_tail(
    dir: &File,
    name: &CStr,
    stat: &Stat,
    (source, from, to): (ReadAt<'_>, u64, u64),
) -> Result<bool> {
    let fd = match sys::open_append_at(dir, name) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(BulkloadRefusal::DestinationKindMismatch);
        }
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
        Err(error) => return Err(error).refuse_at(SITE),
    };
    let file = File::from(fd);
    let same = |now: &Stat| {
        now.node == stat.node && now.size == stat.size && now.mtime_ns == stat.mtime_ns
    };
    if !same(&sys::fstat(&file).refuse_at(SITE)?) {
        return Ok(false);
    }
    #[cfg(test)]
    tests::before_append(&file);
    let mut at = stat.size;
    pieces(source, (from, to), &mut |_, piece| {
        sys::pwrite_all(&file, piece, at).refuse_at(SITE)?;
        at += piece.len() as u64;
        Ok(())
    })?;
    file.sync_all().refuse_at(SITE)?;
    let after = sys::fstat(&file).refuse_at(SITE)?;
    if after.node != stat.node || after.size != stat.size + (to - from) {
        return Err(BulkloadRefusal::HomesConsolidateRaced);
    }
    Ok(true)
}

/// The union of the source's bytes into a live transcript that is a link to
/// an archived file, regularized at the live name.
///
/// SPEC; A-055, A-106, TIN-6587: `target`, the link's resolved target under
/// an archive root, is the live file. The union (streamed, as
/// [`union_stream`] decides it) is written as a regular file at the live
/// name in place of the link, so Claude Code's picker lists it; the archive
/// file stays exactly as it is. The target is held open by no process; the
/// output is written under a private name (the receive's staged file when
/// it holds the source), made durable, and traded for the link only while
/// the link (its identity and text) and the target are as read; what the
/// trade took out must be that link, else it goes back. Then the link is
/// removed. Ledgered once durable.
///
/// # Errors
/// - `HOMES_CONSOLIDATE_RACED`: the target ends inside a line, a process
///   holds it open, or the link or target moved under the trade.
/// - `DESTINATION_KIND_MISMATCH` when the name is no longer a link to
///   `target`; `PATH_ESCAPES_ROOT` for another user's file.
/// - The I/O and ledger refusals.
pub fn union_link(
    live: &Live<'_>,
    target: &Path,
    feed: Feed<'_>,
    size: u64,
    ledger: &Ledger,
) -> Result<Outcome> {
    let name = io::c_name(live.name).refuse_at(SITE)?;
    let link = sys::fstatat_nofollow(live.dir, &name).refuse_at(SITE)?;
    let text = std::fs::read_link(live.path).refuse_at(SITE)?;
    if link.mode & 0o170_000 != 0o120_000
        || std::fs::canonicalize(live.path).ok().as_deref() != Some(target)
    {
        return Err(BulkloadRefusal::DestinationKindMismatch);
    }
    let held = crate::hash::open_nofollow(target)?;
    let stat = sys::fstat(&held).refuse_at(SITE)?;
    check_own(&stat)?;
    let read_target =
        |buffer: &mut [u8], at: u64| sys::pread_full(&held, buffer, at).refuse_at(SITE);
    let source = |buffer: &mut [u8], at: u64| feed.read_at(buffer, at);
    let Some(decided) = decide(&read_target, stat.size, &source, size, by_uuid(live.rel))? else {
        return Err(BulkloadRefusal::HomesConsolidateRaced);
    };
    free(&held)?;
    // The regular file the live name is to hold: the target's own bytes
    // where it covers the source, else the source's, then (a union) the
    // target's extra records.
    let (output, base, keep, records, action) = match decided {
        Decided::Adopted | Decided::Covered => {
            let copy = Output::open(
                live.dir,
                live.name,
                Feed::File(&held),
                (stat.size, &read_target),
            )?;
            (copy, stat.size, Vec::new(), 0, Action::Consolidated)
        }
        Decided::Extended => (
            Output::open(live.dir, live.name, feed, (size, &source))?,
            size,
            Vec::new(),
            0,
            Action::Extended,
        ),
        Decided::Union {
            keep,
            source_records,
        } => (
            Output::open(live.dir, live.name, feed, (size, &source))?,
            size,
            keep,
            source_records,
            Action::Consolidated,
        ),
    };
    if let Err(refusal) = output.append(&read_target, &keep, base) {
        output.discard(live.dir, base);
        return Err(refusal);
    }
    let unmoved = |at: &Stat| at.node == link.node && at.ctime_ns == link.ctime_ns;
    let target_held = || {
        sys::fstat(&held).is_ok_and(|now| now.size == stat.size && now.mtime_ns == stat.mtime_ns)
            && std::fs::canonicalize(live.path).ok().as_deref() == Some(target)
    };
    let still = sys::fstatat_nofollow(live.dir, &name).is_ok_and(|now| unmoved(&now))
        && std::fs::read_link(live.path).is_ok_and(|now| now == text);
    if !still || !target_held() {
        output.discard(live.dir, base);
        return Err(BulkloadRefusal::HomesConsolidateRaced);
    }
    if let Err(error) = io::exchange(live.dir, &output.name, &name) {
        output.discard(live.dir, base);
        if io::rename_unsupported(&error) {
            return Err(BulkloadRefusal::DestinationExchangeUnsupported);
        }
        return Err(error).refuse_at(SITE);
    }
    let out = sys::fstatat_nofollow(live.dir, &output.name);
    if !out.as_ref().is_ok_and(unmoved)
        || !sys::fstat(&held)
            .is_ok_and(|now| now.size == stat.size && now.mtime_ns == stat.mtime_ns)
    {
        io::exchange(live.dir, &output.name, &name).refuse_at(SITE)?;
        live.dir.sync_all().refuse_at(SITE)?;
        output.discard(live.dir, base);
        return Err(BulkloadRefusal::HomesConsolidateRaced);
    }
    output.retire(live.dir)?;
    let extra: u64 = keep.iter().map(|(_, length)| length).sum();
    let made =
        |buffer: &mut [u8], at: u64| sys::pread_full(&output.file, buffer, at).refuse_at(SITE);
    record(
        ledger,
        live.rel,
        "regularize",
        [
            (&read_target, stat.size),
            (&source, size),
            (&made, base + extra),
        ],
        (records, keep.len() as u64),
    )?;
    Ok(Outcome {
        action,
        written: base + extra,
        recorded: 1,
    })
}

/// No process holds `file` open, apart from this very handle
/// ([`crate::procs::holders`]: by device and inode, the caller's own open
/// file description told apart exactly).
///
/// # Errors
/// `HOMES_CONSOLIDATE_RACED` when one does (retryable);
/// `HOMES_CONSOLIDATE_HOLDER_UNKNOWN` where this platform cannot say
/// (anything but Linux): nothing is written then.
fn free(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    match crate::procs::holders(&[file], &[], &[file.as_raw_fd()])? {
        Some(pids) if pids.is_empty() => Ok(()),
        Some(_) => Err(BulkloadRefusal::HomesConsolidateRaced),
        None => Err(BulkloadRefusal::HomesConsolidateHolderUnknown),
    }
}

/// The BLAKE3 of `[0, size)` of `read`, streamed.
fn digest(read: &dyn Fn(&mut [u8], u64) -> Result<usize>, size: u64) -> Result<blake3::Hasher> {
    let mut hasher = blake3::Hasher::new();
    pieces(read, (0, size), &mut |_, piece| {
        hasher.update(piece);
        Ok(())
    })?;
    Ok(hasher)
}

/// Ledger an append log written: both inputs' and the output's digests
/// (streamed) and the record counts. A union already recorded is not
/// recorded twice.
#[allow(clippy::type_complexity)] // Three readers, each with its size.
fn record(
    ledger: &Ledger,
    rel: &[u8],
    action: &str,
    sides: [(&dyn Fn(&mut [u8], u64) -> Result<usize>, u64); 3],
    (source_records, live_extra): (u64, u64),
) -> Result<()> {
    let wide = |count: u64| i64::try_from(count).unwrap_or(i64::MAX);
    let [(live, live_size), (source, size), (out, output_size)] = sides;
    let live_hash = digest(live, live_size)?;
    let source_hash = digest(source, size)?;
    let output = digest(out, output_size)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        });
    let conn = ledger.connection();
    conn.execute_batch(SCHEMA)
        .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    conn.execute(
        "INSERT OR IGNORE INTO append_log_union(file, action, live_blake3, source_blake3, output_blake3, source_records, live_extra, output_size, at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            rel,
            action,
            live_hash.finalize().as_bytes().as_slice(),
            source_hash.finalize().as_bytes().as_slice(),
            output.finalize().as_bytes().as_slice(),
            wide(source_records),
            wide(live_extra),
            wide(output_size),
            now,
        ],
    )
    .map_err(|_| BulkloadRefusal::SqliteStateChanged)?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests;
