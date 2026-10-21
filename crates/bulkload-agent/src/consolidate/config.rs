//! Structured config (operator ruling 2026-10-20, item 2).
//!
//! The class is `settings.json`, `settings.local.json`, `~/.claude.json`,
//! `.mcp.json`, `plugins/*.json`, Codex `config.toml`, and the other JSON
//! and TOML of the homes.
//!
//! - **The union.** It is structural ([`super::value::union`]). The source's
//!   host paths are first written as the destination's through the `--map`
//!   set. A key both sides hold with different values takes the value of
//!   the side modified last. The other value goes to the ledger with its
//!   key path: nothing is lost, and no second file is written.
//! - **Formatting.** A JSON file is written in the live file's style: its
//!   key order, then the new keys. A TOML file is edited in place, keeping
//!   its comments.
//! - **The write.** The merged bytes are written beside the live file under
//!   a private name and made durable, then traded in with one exchange.
//!   - The live file must be this user's own regular file, never followed
//!     through a link.
//!   - A writer that changed it after it was read puts the trade back, and
//!     the file is read and merged again, up to [`ATTEMPTS`] times; past
//!     that the merge refuses `HOMES_CONSOLIDATE_RACED`, retryable, nothing
//!     changed.
//!   - The ledger records a value only after the bytes holding the other
//!     value are durable.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::fd::AsFd;

use super::value::{union as value_union, Conflict, Maps, Side, Value};
use super::{json, toml, Action, Ledger, Live, Source, Unsupported};
use crate::io::{self, sys, Stat};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// Reads and merges of one file before `HOMES_CONSOLIDATE_RACED`.
pub const ATTEMPTS: usize = 4;

/// The largest config file merged; a larger one is held.
pub const SIZE_LIMIT: u64 = 64 << 20;

/// The two structured formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Toml,
}

impl Format {
    /// The format of a file by its name: `*.json`, `*.toml`.
    #[must_use]
    pub fn of(name: &[u8]) -> Option<Self> {
        if name.ends_with(b".json") {
            Some(Self::Json)
        } else if name.ends_with(b".toml") {
            Some(Self::Toml)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Toml => "toml",
        }
    }

    /// `value` on one line, as the ledger records it.
    #[must_use]
    pub fn one_line(self, value: &Value) -> String {
        match self {
            Self::Json => json::compact(value),
            Self::Toml => {
                let mut out = String::new();
                if toml::inline(&mut out, value).is_err() {
                    return json::compact(value);
                }
                out
            }
        }
    }
}

/// A merge of two documents, before anything is written.
#[derive(Debug)]
pub struct Merged {
    pub action: Action,
    /// The bytes to write, for [`Action::Consolidated`].
    pub bytes: Option<Vec<u8>>,
    pub conflicts: Vec<Conflict>,
    pub keys_added: usize,
    pub elements_added: usize,
}

impl Merged {
    const fn held(reason: &'static str) -> Self {
        Self {
            action: Action::Held(reason),
            bytes: None,
            conflicts: Vec::new(),
            keys_added: 0,
            elements_added: 0,
        }
    }
}

/// One side of a merge: its bytes and modification time.
#[derive(Debug, Clone, Copy)]
pub struct Contents<'a> {
    pub bytes: &'a [u8],
    pub mtime_ns: i128,
}

/// The merge of the source's document into the live one, with nothing
/// read or written: what `--plan-only` reports and [`consolidate`] writes.
#[must_use]
pub fn merge(format: Format, live: Contents<'_>, source: Contents<'_>, maps: &Maps) -> Merged {
    if live.bytes == source.bytes {
        return Merged {
            action: Action::Adopted,
            bytes: None,
            conflicts: Vec::new(),
            keys_added: 0,
            elements_added: 0,
        };
    }
    match merge_documents(format, live, source, maps) {
        Ok(merged) => merged,
        Err(Unsupported(reason)) => Merged::held(reason),
    }
}

fn merge_documents(
    format: Format,
    live: Contents<'_>,
    source: Contents<'_>,
    maps: &Maps,
) -> std::result::Result<Merged, Unsupported> {
    let live_text = std::str::from_utf8(live.bytes).map_err(|_| Unsupported("not UTF-8"))?;
    let source_text = std::str::from_utf8(source.bytes).map_err(|_| Unsupported("not UTF-8"))?;
    let later = if source.mtime_ns > live.mtime_ns {
        Side::Source
    } else {
        Side::Live
    };
    let translate = |value: &Value| {
        maps.value(value)
            .ok_or(Unsupported("two source keys translate to one"))
    };
    let (merged, bytes) = match format {
        Format::Json => {
            let held = json::parse(live_text)?;
            let offered = translate(&json::parse(source_text)?)?;
            let merged = value_union(&held, &offered, later);
            let bytes = if merged.covered() {
                None
            } else {
                let text = json::write(&merged.value, &json::Style::of(live_text));
                if !json::parse(&text)?.same(&merged.value) {
                    return Err(Unsupported(
                        "json: the write does not read back as the union",
                    ));
                }
                Some(text.into_bytes())
            };
            (merged, bytes)
        }
        Format::Toml => {
            let doc = toml::parse(live_text)?;
            let offered = translate(&toml::parse(source_text)?.value)?;
            let merged = value_union(&doc.value, &offered, later);
            let bytes = if merged.covered() {
                None
            } else {
                Some(doc.apply(&merged.value)?.into_bytes())
            };
            (merged, bytes)
        }
    };
    Ok(Merged {
        action: if bytes.is_some() {
            Action::Consolidated
        } else {
            Action::Covered
        },
        bytes,
        conflicts: merged.conflicts,
        keys_added: merged.keys_added,
        elements_added: merged.elements_added,
    })
}

/// The file a consolidation writes: `name` in the open, owner-checked
/// directory `dir`, reported as `rel` (relative to the homes).
pub struct Target<'a> {
    pub dir: &'a File,
    pub name: &'a [u8],
    pub rel: &'a [u8],
}

/// What [`consolidate`] did.
#[derive(Debug)]
pub struct Outcome {
    pub action: Action,
    pub conflicts: Vec<Conflict>,
    pub keys_added: usize,
    pub elements_added: usize,
    /// The bytes written (0 unless consolidated or created).
    pub written: u64,
    /// Reads of the live file (more than 1: a writer changed it).
    pub attempts: usize,
}

/// Merge `source` into the live file `target` and write the union over it,
/// crash-safely; record the conflicts in `ledger`.
///
/// # Errors
/// - `DESTINATION_KIND_MISMATCH` for a live name that is no regular file
///   (a link is never followed).
/// - `PATH_ESCAPES_ROOT` for one another user owns.
/// - `DESTINATION_EXCHANGE_UNSUPPORTED` where the file system cannot trade
///   two names.
/// - `HOMES_CONSOLIDATE_RACED` after [`ATTEMPTS`] reads a writer changed.
/// - The I/O and ledger refusals.
pub fn consolidate(
    target: &Target<'_>,
    format: Format,
    source: Contents<'_>,
    maps: &Maps,
    ledger: &Ledger,
) -> Result<Outcome> {
    let name = io::c_name(target.name).refuse_at("consolidate::config::name")?;
    for attempt in 1..=ATTEMPTS {
        let (stat, live) = match read_live(target.dir, &name)? {
            Read::Absent => return create(target, &name, source.bytes, attempt),
            Read::Changed => continue,
            Read::Live(stat, bytes) => (stat, bytes),
        };
        let merged = merge(
            format,
            Contents {
                bytes: &live,
                mtime_ns: stat.mtime_ns,
            },
            source,
            maps,
        );
        let Some(bytes) = merged.bytes else {
            // Nothing to write: the live file's own values stand, and a
            // conflict's other value is the source's, recorded.
            ledger.record(target.rel, format, &merged.conflicts)?;
            return Ok(Outcome {
                action: merged.action,
                conflicts: merged.conflicts,
                keys_added: 0,
                elements_added: 0,
                written: 0,
                attempts: attempt,
            });
        };
        if swap_in(target.dir, &name, &stat, &bytes, attempt)? {
            ledger.record(target.rel, format, &merged.conflicts)?;
            return Ok(Outcome {
                action: Action::Consolidated,
                conflicts: merged.conflicts,
                keys_added: merged.keys_added,
                elements_added: merged.elements_added,
                written: bytes.len() as u64,
                attempts: attempt,
            });
        }
    }
    Err(BulkloadRefusal::HomesConsolidateRaced)
}

/// The structured-config [`super::Union`]: [`consolidate`] with the
/// source's bytes, the format by the live file's name.
///
/// # Errors
/// As [`consolidate`].
pub fn union(live: &Live<'_>, source: &Source<'_>, ledger: &Ledger) -> Result<super::Outcome> {
    let Some(format) = Format::of(live.name) else {
        return Ok(super::Outcome {
            action: Action::Held("config: not JSON or TOML"),
            written: 0,
            recorded: 0,
        });
    };
    if source.bytes.len() as u64 > SIZE_LIMIT {
        return Ok(super::Outcome {
            action: Action::Held("config: over the size limit"),
            written: 0,
            recorded: 0,
        });
    }
    let outcome = consolidate(
        &Target {
            dir: live.dir,
            name: live.name,
            rel: live.rel,
        },
        format,
        Contents {
            bytes: source.bytes,
            mtime_ns: source.mtime_ns,
        },
        source.maps,
        ledger,
    )?;
    Ok(super::Outcome {
        action: outcome.action,
        written: outcome.written,
        recorded: outcome.conflicts.len(),
    })
}

/// A read of the live file.
pub(crate) enum Read {
    Absent,
    /// A writer changed it while it was read.
    Changed,
    Live(Stat, Vec<u8>),
}

/// The live file's identity and bytes.
pub(crate) fn read_live(dir: &File, name: &CStr) -> Result<Read> {
    let fd = match sys::open_read_at(dir, name) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(Read::Absent),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(BulkloadRefusal::DestinationKindMismatch);
        }
        Err(error) => return Err(error).refuse_at("consolidate::config::open"),
    };
    let stat = sys::fstat(&fd).refuse_at("consolidate::config::stat")?;
    check_own(&stat)?;
    if stat.size > SIZE_LIMIT {
        return Err(BulkloadRefusal::HomesConsolidateUnsupported);
    }
    let mut bytes = vec![0_u8; usize::try_from(stat.size).unwrap_or(0)];
    let read = sys::pread_full(&fd, &mut bytes, 0).refuse_at("consolidate::config::read")?;
    let after = sys::fstat(&fd).refuse_at("consolidate::config::stat")?;
    if read as u64 != stat.size || after.size != stat.size || after.mtime_ns != stat.mtime_ns {
        return Ok(Read::Changed);
    }
    Ok(Read::Live(stat, bytes))
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

/// The private name the merged bytes are written under.
fn temp_name(name: &CStr, attempt: usize) -> Result<CString> {
    let mut temp = b".".to_vec();
    temp.extend_from_slice(name.to_bytes());
    temp.extend_from_slice(
        format!(".bulkload-consolidate.{}.{attempt}", std::process::id()).as_bytes(),
    );
    io::c_name(&temp).refuse_at("consolidate::config::temp")
}

/// Write `bytes` under a private name, durable, then trade it for the live
/// file if the live file is still `stat`. `false`: a writer changed it, the
/// trade is undone (or never made) and nothing of this attempt remains.
pub(crate) fn swap_in(
    dir: &File,
    name: &CStr,
    stat: &Stat,
    bytes: &[u8],
    attempt: usize,
) -> Result<bool> {
    let temp = temp_name(name, attempt)?;
    let file = File::from(
        sys::create_excl_at(dir.as_fd(), &temp, stat.mode & 0o7777)
            .refuse_at("consolidate::config::create")?,
    );
    let written = sys::pwrite_all(&file, bytes, 0)
        .and_then(|()| sys::fchmod(&file, stat.mode & 0o7777))
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = sys::unlinkat(dir, &temp, false);
        return Err(error).refuse_at("consolidate::config::write");
    }
    drop(file);
    #[cfg(test)]
    super::tests::before_swap(dir, name);
    let unchanged = |now: &Stat| {
        now.node == stat.node && now.size == stat.size && now.mtime_ns == stat.mtime_ns
    };
    match sys::fstatat_nofollow(dir, name) {
        Ok(now) if unchanged(&now) && now.ctime_ns == stat.ctime_ns => {}
        Ok(_) | Err(_) => {
            let _ = sys::unlinkat(dir, &temp, false);
            return Ok(false);
        }
    }
    if let Err(error) = io::exchange(dir, &temp, name) {
        let _ = sys::unlinkat(dir, &temp, false);
        if io::rename_unsupported(&error) {
            return Err(BulkloadRefusal::DestinationExchangeUnsupported);
        }
        return Err(error).refuse_at("consolidate::config::exchange");
    }
    // What the trade took out must be the file that was read; else a writer
    // replaced or wrote it in between, and it goes back.
    let out = sys::fstatat_nofollow(dir, &temp);
    if !out.as_ref().is_ok_and(unchanged) {
        io::exchange(dir, &temp, name).refuse_at("consolidate::config::exchange_back")?;
        let _ = sys::unlinkat(dir, &temp, false);
        dir.sync_all().refuse_at("consolidate::config::sync_dir")?;
        return Ok(false);
    }
    sys::unlinkat(dir, &temp, false).refuse_at("consolidate::config::unlink")?;
    dir.sync_all().refuse_at("consolidate::config::sync_dir")?;
    Ok(true)
}

/// Land the source's bytes where the destination holds no such file,
/// create-only.
fn create(target: &Target<'_>, name: &CStr, bytes: &[u8], attempt: usize) -> Result<Outcome> {
    let temp = temp_name(name, attempt)?;
    let file = File::from(
        sys::create_excl_at(target.dir.as_fd(), &temp, 0o600)
            .refuse_at("consolidate::config::create")?,
    );
    let written = sys::pwrite_all(&file, bytes, 0).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = sys::unlinkat(target.dir, &temp, false);
        return Err(error).refuse_at("consolidate::config::write");
    }
    drop(file);
    match io::publish_noreplace(target.dir, &temp, name) {
        Ok(_) => {}
        Err(error) => {
            let _ = sys::unlinkat(target.dir, &temp, false);
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err(BulkloadRefusal::HomesConsolidateRaced);
            }
            return Err(error).refuse_at("consolidate::config::publish");
        }
    }
    target
        .dir
        .sync_all()
        .refuse_at("consolidate::config::sync_dir")?;
    Ok(Outcome {
        action: Action::Created,
        conflicts: Vec::new(),
        keys_added: 0,
        elements_added: 0,
        written: bytes.len() as u64,
        attempts: attempt,
    })
}
