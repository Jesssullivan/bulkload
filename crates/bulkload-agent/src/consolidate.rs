//! Consolidating the agent homes (operator ruling 2026-10-20).
//!
//! The source's `~/.claude`, `~/.claude.json` and `~/.codex` become one
//! with the destination's live homes, never a second copy beside them. "No
//! dupes, no duplicated besides, no stage dupes": SPEC RUL-10, a lossless
//! union into DEST (C10; LIVE-15, LIVE-18, CONF-05, KEEP-36).
//!
//! Each file [`Class`] has its own [`Union`]:
//! - append logs: the #294 lane's JSONL union;
//! - structured config: [`config`], JSON and TOML merged key by key;
//! - Codex `SQLite`: the #278 lane's row union;
//! - anything else: [`opaque`], equal bytes adopted and differing bytes
//!   held.
//!
//! A class whose union has not landed is held typed
//! (`HOMES_CONSOLIDATE_UNSUPPORTED`), listed, and never landed beside.
//! [`value`] holds the structured values, [`json`] and [`toml`] their
//! parsers and writers, and [`ledger`] the record of every value a union
//! did not keep.

use std::fs::File;
use std::path::Path;

pub mod config;
pub mod json;
pub mod ledger;
pub mod toml;
pub mod value;

pub use ledger::Ledger;
pub use value::Maps;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines
)]
mod tests;

/// Why a file is not consolidated: it is held typed
/// (`HOMES_CONSOLIDATE_UNSUPPORTED`), nothing written, and listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

/// A file class of the homes, by the union it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Append-only logs, `*.jsonl`: Claude transcripts, Codex rollouts,
    /// `history.jsonl`. The live file grows by appends only.
    AppendLog,
    /// JSON and TOML config ([`config`]).
    Config(config::Format),
    /// A `SQLite` database: Codex `state_5.sqlite` and its kin.
    Sqlite,
    /// Anything else: equal bytes adopted, differing bytes held.
    Opaque,
}

impl Class {
    /// The class of the file at `rel_path`. `sqlite`: the session carries
    /// it as a `SQLite` snapshot (its header said so).
    #[must_use]
    pub fn of(rel_path: &[u8], sqlite: bool) -> Self {
        let name = rel_path
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or(rel_path);
        // A store's `-wal`, `-shm` and `-journal` go with it.
        let base = [b"-wal".as_slice(), b"-shm", b"-journal"]
            .iter()
            .find_map(|sidecar| name.strip_suffix(*sidecar))
            .unwrap_or(name);
        if sqlite || base.ends_with(b".sqlite") || base.ends_with(b".sqlite3") {
            Self::Sqlite
        } else if name.ends_with(b".jsonl") {
            Self::AppendLog
        } else if let Some(format) = config::Format::of(name) {
            Self::Config(format)
        } else {
            Self::Opaque
        }
    }

    /// Its report word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::AppendLog => "append-log",
            Self::Config(config::Format::Json) => "config-json",
            Self::Config(config::Format::Toml) => "config-toml",
            Self::Sqlite => "sqlite",
            Self::Opaque => "opaque",
        }
    }
}

/// What a union did to the live file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The bytes are equal: nothing written.
    Adopted,
    /// The live file was a prefix of the source's: its tail was appended.
    Extended,
    /// The live file already holds everything the source's does: nothing
    /// written.
    Covered,
    /// The union was written over the live file.
    Consolidated,
    /// The destination held no such file; the source's was landed.
    Created,
    /// Not merged (`HOMES_CONSOLIDATE_UNSUPPORTED`): nothing written.
    Held(&'static str),
}

impl Action {
    /// Its report word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Adopted => "adopt",
            Self::Extended => "extend",
            Self::Covered => "covered",
            Self::Consolidated => "consolidate",
            Self::Created => "created",
            Self::Held(_) => "hold",
        }
    }

    /// The typed refusal a held file is listed with.
    #[must_use]
    pub const fn refusal(&self) -> Option<crate::BulkloadRefusal> {
        match self {
            Self::Held(_) => Some(crate::BulkloadRefusal::HomesConsolidateUnsupported),
            _ => None,
        }
    }
}

/// The live destination file a union writes.
#[derive(Clone, Copy)]
pub struct Live<'a> {
    /// Its directory, open beneath the owner-checked home root: a union
    /// reads and writes the file relative to it (`*at` calls, no-follow),
    /// never through a path.
    pub dir: &'a File,
    /// Its name in `dir`.
    pub name: &'a [u8],
    /// Its full path, for reports and for a union that must open it by
    /// path (`SQLite`); such a union checks that what it opened is the
    /// file at `dir`/`name`.
    pub path: &'a Path,
    /// Its path relative to the destination root: the ledger's key.
    pub rel: &'a [u8],
}

/// The source's version of the file, as the receive gathered it: its bytes
/// in memory, never staged on disk on either machine (SPEC rule 2), and
/// dropped once the union returns. A file over [`IN_MEMORY_LIMIT`] is
/// never gathered: it is held.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub bytes: &'a [u8],
    /// The source's modification time: a value both sides hold differently
    /// takes the side modified last.
    pub mtime_ns: i128,
    /// The `--map` set: a source host path is the destination's.
    pub maps: &'a Maps,
}

/// The largest file the receive gathers in memory for a union (64 MiB); a
/// larger one is held typed, so a big file never balloons memory.
pub const IN_MEMORY_LIMIT: u64 = 64 << 20;

/// What a [`Union`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub action: Action,
    /// Bytes written to the live file (0 when nothing was).
    pub written: u64,
    /// Values recorded in the ledger (a union's conflicts).
    pub recorded: usize,
}

/// A class's union: merge `source` into `live` crash-safely and record in
/// `ledger` what the merged file does not keep.
///
/// - **Contract.** The live file must be the user's own regular file. On
///   success it holds everything either side held, and nothing is ever
///   written beside it.
/// - **Holds.** What the union cannot carry losslessly returns
///   [`Action::Held`] with nothing written.
/// - **Retries.** A live writer that keeps changing the file is
///   `HOMES_CONSOLIDATE_RACED`, retryable.
/// - **Ledger order.** The ledger is written only after the merged bytes
///   are durable.
pub type Union = fn(&Live<'_>, &Source<'_>, &Ledger) -> crate::Result<Outcome>;

/// The union of `class`; `None` while its lane has not landed, and such a
/// file is held typed (`HOMES_CONSOLIDATE_UNSUPPORTED`).
#[must_use]
pub fn union_of(class: Class) -> Option<Union> {
    match class {
        Class::Config(_) => Some(config::union),
        Class::Opaque => Some(opaque),
        // The #294 lane's JSONL union and the #278 lane's row union.
        Class::AppendLog | Class::Sqlite => None,
    }
}

/// An opaque file: equal bytes adopted, differing bytes held.
///
/// # Errors
/// The read refusals.
pub fn opaque(live: &Live<'_>, source: &Source<'_>, _ledger: &Ledger) -> crate::Result<Outcome> {
    use crate::refuse::RefuseAt as _;
    let name = crate::io::c_name(live.name).refuse_at("consolidate::opaque")?;
    let fd = crate::io::sys::open_read_at(live.dir, &name).refuse_at("consolidate::opaque")?;
    let held = File::from(fd);
    let stat = crate::io::sys::fstat(&held).refuse_at("consolidate::opaque")?;
    let mut live = vec![0_u8; usize::try_from(stat.size).unwrap_or(0)];
    let read = crate::io::sys::pread_full(&held, &mut live, 0).refuse_at("consolidate::opaque")?;
    let same = stat.is_file() && read as u64 == stat.size && live.as_slice() == source.bytes;
    Ok(Outcome {
        action: if same {
            Action::Adopted
        } else {
            Action::Held("opaque: the bytes differ")
        },
        written: 0,
        recorded: 0,
    })
}
