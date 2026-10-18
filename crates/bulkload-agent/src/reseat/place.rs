//! One file's placement into the destination's live agent state (R0,
//! TIN-6587): the never-overwrite rules, the ledger and the crash points.
//!
//! For a destination path `D` and its landed file `L` (a `.jsonl.zst`
//! rollout is read as its decompressed stream, and `D` is then the plain
//! `.jsonl`):
//!
//! - **One landed file per destination path.** A row names the landed
//!   file it belongs to; another landed file for the same `D` (two source
//!   keys mapped onto one destination key) is a collision
//!   (`RESEAT_LANE_COLLISION`), never a supersede. A rollout whose landed
//!   form changed (`.jsonl` ↔ `.jsonl.zst`) is the same stream.
//! - **Whole lines.** A `.jsonl` file is placed and extended only up to the
//!   last newline of the landed bytes: a line the source was still writing
//!   is held back until it is whole, so the destination never ends inside
//!   a record that a destination resume would append to. Every write of an
//!   extension is whole lines.
//! - **`D` absent**: copy `L` into a temporary beside `D`
//!   (`.<name>.<token>.bulkload-reseat`, the token this ledger's own, so two
//!   ledgers never share a temporary), give it `L`'s mode and modification time
//!   (Claude Code's `--continue` takes the newest file), seal it, publish it
//!   with a no-replace rename, seal the directory, then commit the row. A
//!   regular file, never a symlink (Claude Code lists only regular files)
//!   and never a hardlink (a destination resume would append to the
//!   landing).
//! - **`D` present, no row**: a regular file equal to `L` is adopted; a
//!   strict prefix of `L` is adopted and extended when the file is
//!   append-only (`.jsonl`, `.txt`); anything else is kept as it is and
//!   reported (`RESEAT_DEST_DIVERGED`, or `RESEAT_DEST_CONTINUED` when `L`
//!   is a prefix of `D`). A symlink or other node, or a regular file with
//!   more than one link (an archive copy, or the landing itself: extending
//!   it would change the other name), is never written
//!   (`DESTINATION_OCCUPIED`).
//! - **`D` holds this ledger's bytes**: unchanged identities read nothing
//!   (S3). A grown append-only `L` is proven to extend the placed bytes by
//!   the BLAKE3 of all of them (a change anywhere in them is
//!   `RESEAT_SOURCE_REWRITTEN`, never a hybrid), and its new tail is
//!   appended (`O_APPEND`) under an intent row a crash resumes from. Before
//!   every write the destination's size is checked against what this pass
//!   wrote: a destination writer appending beside it (a session resumed
//!   there) stops the extension (`RESEAT_EXTEND_RACED`), its lines whole,
//!   the file the destination's from then on, its modification time not
//!   reset. One window stays: a destination append landing between the size
//!   check and the `write` of the same chunk. POSIX has no conditional
//!   append, so it is detected (the size after the write is not this pass's)
//!   rather than prevented: the lines stay whole, the file is handed over
//!   (`RESEAT_EXTEND_RACED`) and `--check` fails it (`hash-differs`), with
//!   nothing lost. The window is two system calls wide, once per 1 MiB
//!   chunk. A changed file that is not append-only (a plan, a memory note)
//!   supersedes the placement through an atomic exchange, checked
//!   afterwards against the staged file's whole identity: a displaced file
//!   that is not this ledger's placement is exchanged back, and a staged
//!   file edited in place since is the destination's.
//! - **`D` changed since bulkload's last write** (a session resumed there):
//!   the destination owns it from then on (`RESEAT_DEST_CONTINUED`); it is
//!   never extended again, and newer landed bytes stay in the landing.
//! - **An unreadable landed form** (a truncated or corrupt `.jsonl.zst`) is
//!   that file's `unreadable-form` (`RESEAT_UNREADABLE_FORM`), never the
//!   run's refusal; its temporary is removed, and an extension it cut short
//!   keeps the whole lines it appended, proven, as this ledger's.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use super::json::FieldScanner;
use super::ledger::{Identity, Intent, Ledger, Placement, Regular, RegularState, State};
use super::Kind;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// Bytes per read.
const CHUNK: usize = 1024 * 1024;
/// The longest line held in memory to be written whole.
const LINE_LIMIT: usize = 512 * 1024 * 1024;
/// The temporary's suffix; nothing else names a file this way.
const TEMP_SUFFIX: &str = ".bulkload-reseat";

/// What one placement did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Absent at the destination: written.
    Placed,
    /// Already there, equal to the landed file: adopted.
    Adopted,
    /// Grown at the landing: its new tail appended.
    Extended,
    /// A changed file that is not append-only: exchanged in.
    Superseded,
    /// This ledger's placement, with nothing new: nothing written.
    Unchanged,
    /// The destination holds different bytes: kept.
    Diverged,
    /// The destination holds the landed bytes and more: kept.
    DestAhead,
    /// This ledger's placement changed at the destination; `both` when the
    /// landing grew too (both hosts continued the session).
    Continued { both: bool },
    /// This ledger's placement was removed at the destination: not put
    /// back.
    Removed,
    /// A symlink or other node at the path or above it: never written.
    Occupied,
    /// The landed file stopped extending the placed bytes.
    Rewritten,
    /// The landed file is not a regular file.
    LandingNotRegular,
    /// The landed file cannot be read as its form (a corrupt `.zst`).
    Unreadable,
    /// The destination file system has no atomic exchange for a
    /// supersede.
    ExchangeUnsupported,
    /// Another landed file already owns this destination path.
    Collision,
    /// A destination writer appended while this pass extended the file:
    /// stopped, lines whole, the destination's from then on.
    Raced,
    /// A `.jsonl` with no complete line yet: nothing placed.
    Held,
    /// The destination's symlink into a `--replace-archived-dest` root,
    /// replaced by the differing landed file (A-106).
    ReplacedArchived,
}

impl Outcome {
    /// The report word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Placed => "placed",
            Self::Adopted => "adopted",
            Self::Extended => "extended",
            Self::Superseded => "superseded",
            Self::Unchanged => "unchanged",
            Self::Diverged => "diverged",
            Self::DestAhead => "dest-ahead",
            Self::Continued { both: false } => "dest-continued",
            Self::Continued { both: true } => "both-continued",
            Self::Removed => "dest-removed",
            Self::Occupied => "occupied",
            Self::Rewritten => "source-rewritten",
            Self::LandingNotRegular => "landing-not-regular",
            Self::Unreadable => "unreadable-form",
            Self::ExchangeUnsupported => "exchange-unsupported",
            Self::Collision => "collision",
            Self::Raced => "extend-raced",
            Self::Held => "held",
            Self::ReplacedArchived => "replaced-archived",
        }
    }

    /// Whether the destination file now holds the landed bytes.
    #[must_use]
    pub const fn landed(self) -> bool {
        matches!(
            self,
            Self::Placed
                | Self::Adopted
                | Self::Extended
                | Self::Superseded
                | Self::Unchanged
                | Self::ReplacedArchived
        )
    }

    /// The typed refusal a kept file reports, if any.
    #[must_use]
    pub const fn refusal(self) -> Option<BulkloadRefusal> {
        match self {
            Self::Placed
            | Self::Adopted
            | Self::Extended
            | Self::Superseded
            | Self::Unchanged
            | Self::Held
            | Self::ReplacedArchived => None,
            Self::Diverged => Some(BulkloadRefusal::ReseatDestDiverged),
            Self::DestAhead | Self::Continued { .. } | Self::Removed => {
                Some(BulkloadRefusal::ReseatDestContinued)
            }
            Self::Occupied => Some(BulkloadRefusal::DestinationOccupied),
            Self::Rewritten => Some(BulkloadRefusal::ReseatSourceRewritten),
            Self::LandingNotRegular | Self::Unreadable => {
                Some(BulkloadRefusal::ReseatUnreadableForm)
            }
            Self::ExchangeUnsupported => Some(BulkloadRefusal::DestinationExchangeUnsupported),
            Self::Collision => Some(BulkloadRefusal::ReseatLaneCollision),
            Self::Raced => Some(BulkloadRefusal::ReseatExtendRaced),
        }
    }
}

/// A placement's result: its outcome and, when asked, what the scanner saw
/// in the landed bytes this pass placed or proved.
pub struct Placed {
    pub outcome: Outcome,
    pub scanner: Option<FieldScanner>,
}

/// Whether a file at `path` is append-only: a transcript, a subagent
/// transcript, a rollout, a debug log or a tool result.
#[must_use]
pub fn appendable(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl" || extension == "txt")
}

/// Whether a file at `path` is JSON Lines: placed and extended in whole
/// lines only.
#[must_use]
pub fn line_ruled(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
}

/// Whether a landed path is a zstd-compressed stream.
#[must_use]
pub fn compressed(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "zst")
}

/// `path` without a `.zst` suffix.
#[must_use]
pub fn plain(path: &Path) -> PathBuf {
    if compressed(path) {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

/// Whether two landed paths name one stream (equal, or one form of the
/// other).
fn same_stream(a: &Path, b: &Path) -> bool {
    a == b || plain(a) == plain(b)
}

/// The bytes a placement of `landed` at `dest` would write at most, for
/// the free-space check.
///
/// Nothing for a landed file the ledger already holds
/// unchanged or a destination file the destination owns, the new tail of
/// this ledger's own placement, else the whole content (decompressed for a
/// `.zst`: its frame's declared size, or the stream counted).
///
/// # Errors
/// Ledger and I/O refusals.
pub fn planned(ledger: Option<&Ledger>, dest: &Path, landed: &Path) -> Result<u64> {
    let Some(meta) = lstat(landed)? else {
        return Ok(0);
    };
    if !meta.is_file() {
        return Ok(0);
    }
    let row = match ledger {
        Some(ledger) => ledger.placement(dest)?,
        None => None,
    };
    if row
        .as_ref()
        .is_some_and(|row| row.landed == Identity::of(&meta) || row.state == State::Continued)
    {
        return Ok(0);
    }
    let size = content_size(landed, &meta)?;
    Ok(match row {
        Some(row) if row.state.owned() => size.saturating_sub(row.placed),
        _ => size,
    })
}

/// The content size of a landed file: its size, or a `.zst`'s
/// decompressed size.
fn content_size(landed: &Path, meta: &std::fs::Metadata) -> Result<u64> {
    if !compressed(landed) {
        return Ok(meta.len());
    }
    let file = crate::hash::open_nofollow(landed)?;
    let mut head = [0_u8; 18];
    let read = file
        .read_at(&mut head, 0)
        .refuse_at("reseat::place::content_size")?;
    counters::add_len(Counter::ReseatLandingRead, read);
    if let Ok(Some(size)) =
        zstd::zstd_safe::get_frame_content_size(head.get(..read).unwrap_or_default())
    {
        return Ok(size);
    }
    // No declared size: count the stream (an unreadable one places
    // nothing).
    let landing = Landing::open(landed)?;
    let Ok(mut reader) = landing.reader() else {
        return Ok(0);
    };
    let mut buffer = vec![0_u8; CHUNK];
    let mut total = 0_u64;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => total += read as u64,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Ok(0),
        }
    }
}

/// Places files under one canonical destination root.
pub struct Placer<'a> {
    pub ledger: &'a Ledger,
    pub root: &'a Path,
    /// `--replace-archived-dest` roots (canonical): a transcript, sidecar
    /// or rollout at the destination that is a symlink resolving to a
    /// regular file under one is replaced by a differing landed file
    /// (A-106).
    pub archived: &'a [PathBuf],
}

impl<'a> Placer<'a> {
    /// A placer for `root` ledgered in `ledger`.
    #[must_use]
    pub const fn new(ledger: &'a Ledger, root: &'a Path) -> Self {
        Self {
            ledger,
            root,
            archived: &[],
        }
    }

    /// The same placer, replacing archive links under `archived`.
    #[must_use]
    pub const fn with_archived(self, archived: &'a [PathBuf]) -> Self {
        Self { archived, ..self }
    }

    /// The destination's link at `dest` that a differing landed file of
    /// `kind` replaces: a symlink (never regularized, replaced or undone
    /// by this ledger) whose fully resolved target is a regular file under
    /// an archive root. Every hop is followed by the kernel; only the
    /// final target's real path is judged, against canonical roots, so no
    /// `..` or intermediate link can place it under a root it is not
    /// under. `(link text, resolved target, link metadata)`.
    fn archived_link(
        &self,
        dest: &Path,
        kind: Kind,
    ) -> Result<Option<(PathBuf, PathBuf, std::fs::Metadata)>> {
        if self.archived.is_empty()
            || !matches!(kind, Kind::Transcript | Kind::Sidecar | Kind::Rollout)
        {
            return Ok(None);
        }
        if self.ledger.regular(dest)?.is_some() {
            return Ok(None);
        }
        archive_link(dest, self.archived)
    }
}

/// The symlink at `dest` when its fully resolved target is a regular file
/// under one of the canonical `roots`.
///
/// Returns `(link text, resolved target, link metadata)`. Every hop is
/// followed by the kernel; only the final target's real path is judged,
/// so no `..` or intermediate link can place it under a root it is not
/// under. A link whose own directory resolves under a root is never one:
/// the exchange would happen inside the archive (review A1).
///
/// # Errors
/// `lstat` refusals other than absence.
pub fn archive_link(
    dest: &Path,
    roots: &[PathBuf],
) -> Result<Option<(PathBuf, PathBuf, std::fs::Metadata)>> {
    if roots.is_empty() {
        return Ok(None);
    }
    let Some(meta) = lstat(dest)? else {
        return Ok(None);
    };
    if !meta.is_symlink() {
        return Ok(None);
    }
    let Ok(text) = std::fs::read_link(dest) else {
        return Ok(None);
    };
    let Ok(resolved) = std::fs::canonicalize(dest) else {
        return Ok(None);
    };
    if !lstat(&resolved)?.is_some_and(|target| target.is_file())
        || !roots.iter().any(|root| resolved.starts_with(root))
    {
        return Ok(None);
    }
    let Some(parent) = dest
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
    else {
        return Ok(None);
    };
    if roots.iter().any(|root| parent.starts_with(root)) {
        return Ok(None);
    }
    Ok(Some((text, resolved, meta)))
}

/// Whether a process this user can see holds `path` open.
///
/// Every `/proc/<pid>/fd/*` link is read (observation only, no process is
/// signalled, R-N11); a live writer would keep appending to the archive
/// target after the swap (review A2). `false` where `/proc` is absent.
#[must_use]
pub fn open_by_a_process(path: &Path) -> bool {
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return false;
    };
    for process in processes.flatten() {
        let name = process.file_name();
        if !name.as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        if fds
            .flatten()
            .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|open| open == path))
        {
            return true;
        }
    }
    false
}

/// `(identity, BLAKE3, BLAKE3 of the first bytes when longer)`.
type TargetDigests = (Identity, [u8; 32], Option<[u8; 32]>);

/// The archive target's identity and BLAKE3, and the BLAKE3 of its first
/// `prefix` bytes when it is longer (whether the landing is a prefix of
/// it); `None` when it is not a regular file or changed while read.
fn target_digests(path: &Path, prefix: u64) -> Result<Option<TargetDigests>> {
    let Ok(mut file) = crate::hash::open_nofollow(path) else {
        return Ok(None);
    };
    let meta = file.metadata().refuse_at("reseat::place::target")?;
    if !meta.is_file() {
        return Ok(None);
    }
    let identity = Identity::of(&meta);
    let mut hasher = blake3::Hasher::new();
    let mut at_prefix = None;
    let mut buffer = vec![0_u8; CHUNK];
    let mut total = 0_u64;
    loop {
        if total == prefix && at_prefix.is_none() && identity.size > prefix {
            at_prefix = Some(*hasher.finalize().as_bytes());
        }
        let want = if total < prefix && identity.size > prefix {
            usize::try_from((prefix - total).min(CHUNK as u64)).unwrap_or(CHUNK)
        } else {
            CHUNK
        };
        let read = file
            .read(buffer.get_mut(..want).unwrap_or_default())
            .refuse_at("reseat::place::target_read")?;
        if read == 0 {
            break;
        }
        counters::add_len(Counter::ReseatDestRead, read);
        counters::update(
            &mut hasher,
            Counter::HashOther,
            buffer.get(..read).unwrap_or_default(),
        );
        total = total.saturating_add(read as u64);
    }
    let after = Identity::of(&file.metadata().refuse_at("reseat::place::target")?);
    if after != identity || total != identity.size {
        return Ok(None);
    }
    Ok(Some((identity, *hasher.finalize().as_bytes(), at_prefix)))
}

/// The `placed` value of an `occupied` archive-link row: the landing is
/// the target (equal), or a strict prefix of it (the archive ahead).
const LINK_EQUAL: u64 = 0;
const LINK_AHEAD: u64 = 1;

/// Run `hook` after a replacement's exchange (unit tests only: a live
/// writer between the swap and its checks).
#[cfg(test)]
type ReplaceHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
std::thread_local! {
    static REPLACE_HOOK: std::cell::RefCell<Option<ReplaceHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Set (or clear) the after-exchange hook on this thread.
#[cfg(test)]
pub fn set_replace_hook(hook: Option<ReplaceHook>) {
    REPLACE_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(test)]
fn replace_hook(dest: &Path) {
    REPLACE_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(dest);
        }
    });
}

#[cfg(not(test))]
const fn replace_hook(_dest: &Path) {}

/// The landed file's bytes, from offset 0 to its identity's size, through
/// `pread` (so the landing is never read past what it was when opened).
struct Raw<'a> {
    file: &'a File,
    at: u64,
    end: u64,
}

impl Read for Raw<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self.end.saturating_sub(self.at);
        let want = usize::try_from(left.min(buffer.len() as u64)).unwrap_or(0);
        let Some(target) = buffer.get_mut(..want) else {
            return Ok(0);
        };
        if target.is_empty() {
            return Ok(0);
        }
        let read = self.file.read_at(target, self.at)?;
        if read == 0 {
            // The landed file shrank since it was opened.
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        counters::add_len(Counter::ReseatLandingRead, read);
        self.at += read as u64;
        Ok(read)
    }
}

/// A landed file opened for this pass, with its identity.
pub struct Landing {
    file: File,
    meta: std::fs::Metadata,
    identity: Identity,
    compressed: bool,
}

impl Landing {
    /// Open the landed file at `path`.
    ///
    /// # Errors
    /// The I/O refusal of opening it.
    pub fn open(path: &Path) -> Result<Self> {
        let file = crate::hash::open_nofollow(path)?;
        let meta = file.metadata().refuse_at("reseat::place::landing")?;
        Ok(Self {
            identity: Identity::of(&meta),
            file,
            meta,
            compressed: compressed(path),
        })
    }

    /// Its identity.
    #[must_use]
    pub const fn identity(&self) -> Identity {
        self.identity
    }

    /// A sequential reader of its content (decompressed for a `.zst`),
    /// from the start.
    ///
    /// # Errors
    /// The refusal of starting the decoder.
    pub fn reader(&self) -> Result<Box<dyn Read + '_>> {
        let raw = Raw {
            file: &self.file,
            at: 0,
            end: self.identity.size,
        };
        if self.compressed {
            let decoder = zstd::stream::read::Decoder::new(raw)
                .map_err(|_| BulkloadRefusal::ReseatUnreadableForm)?;
            Ok(Box::new(decoder))
        } else {
            Ok(Box::new(raw))
        }
    }

    fn read_error(&self, error: &std::io::Error, at: &'static str) -> BulkloadRefusal {
        if self.compressed {
            // A corrupt frame, or a truncated one (the decoder's
            // `UnexpectedEof`): this file cannot be read as its form.
            return BulkloadRefusal::ReseatUnreadableForm;
        }
        match error.kind() {
            std::io::ErrorKind::UnexpectedEof => BulkloadRefusal::SourceChangedAfterSnapshot,
            _ => crate::refuse::io(error, at),
        }
    }
}

/// Read up to `buffer.len()` bytes, retrying interruptions.
fn fill(
    landing: &Landing,
    reader: &mut dyn Read,
    buffer: &mut [u8],
    at: &'static str,
) -> Result<usize> {
    loop {
        match reader.read(buffer) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(landing.read_error(&error, at)),
        }
    }
}

/// What [`read_exact_hash`] found.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prefixed {
    /// The content has the bytes; the last of them (none for 0 bytes).
    Read(Option<u8>),
    /// The content is shorter.
    Short,
}

/// Read exactly `length` content bytes into `hasher` (and `scanner`).
fn read_exact_hash(
    landing: &Landing,
    reader: &mut dyn Read,
    length: u64,
    hasher: &mut blake3::Hasher,
    mut scanner: Option<&mut FieldScanner>,
) -> Result<Prefixed> {
    let mut buffer = vec![0_u8; CHUNK];
    let mut left = length;
    let mut last = None;
    while left > 0 {
        let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
        let chunk = buffer
            .get_mut(..want)
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
        let read = fill(landing, reader, chunk, "reseat::place::prove")?;
        if read == 0 {
            return Ok(Prefixed::Short);
        }
        let bytes = chunk.get(..read).unwrap_or_default();
        counters::update(hasher, Counter::HashOther, bytes);
        if let Some(scanner) = scanner.as_deref_mut() {
            if !scanner.complete() {
                scanner.feed(bytes);
            }
        }
        last = bytes.last().copied();
        left -= read as u64;
    }
    Ok(Prefixed::Read(last))
}

/// Whether the reader has another byte; it is handed back as the first of
/// the rest.
fn peek(landing: &Landing, reader: &mut dyn Read) -> Result<Option<u8>> {
    let mut one = [0_u8; 1];
    Ok((fill(landing, reader, &mut one, "reseat::place::peek")? == 1).then_some(one[0]))
}

/// What [`pump`] did.
struct Pumped {
    /// Bytes handed to the writer.
    written: u64,
    /// Bytes read and held back: a line not yet whole.
    held: u64,
    /// The writer asked to stop.
    stopped: bool,
}

/// Read `reader` to its end, handing `write` whole lines (every byte when
/// not `lines`), hashing and scanning what it hands. `write` returns
/// `false` to stop.
fn pump(
    landing: &Landing,
    reader: &mut dyn Read,
    lines: bool,
    hasher: &mut blake3::Hasher,
    mut scanner: Option<&mut FieldScanner>,
    write: &mut dyn FnMut(&[u8]) -> Result<bool>,
) -> Result<Pumped> {
    let mut buffer = vec![0_u8; CHUNK];
    let mut carry: Vec<u8> = Vec::new();
    let mut pumped = Pumped {
        written: 0,
        held: 0,
        stopped: false,
    };
    loop {
        let read = fill(landing, reader, &mut buffer, "reseat::place::pump")?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).unwrap_or_default();
        let ready: &[u8] = if lines {
            carry.extend_from_slice(chunk);
            let Some(end) = carry.iter().rposition(|byte| *byte == b'\n') else {
                if carry.len() > LINE_LIMIT {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                continue;
            };
            carry.get(..=end).unwrap_or_default()
        } else {
            chunk
        };
        if !write(ready)? {
            pumped.stopped = true;
            break;
        }
        counters::update(hasher, Counter::HashOther, ready);
        if let Some(scanner) = scanner.as_deref_mut() {
            if !scanner.complete() {
                scanner.feed(ready);
            }
        }
        pumped.written += ready.len() as u64;
        if lines {
            let done = ready.len();
            carry.drain(..done);
        }
    }
    pumped.held = carry.len() as u64;
    Ok(pumped)
}

pub(super) fn lstat(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).refuse_at("reseat::place::lstat"),
    }
}

pub(super) fn identity_at(path: &Path) -> Result<Option<Identity>> {
    Ok(lstat(path)?.map(|meta| Identity::of(&meta)))
}

pub(super) fn open_dir(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .refuse_at("reseat::place::open_dir")
}

pub(super) fn c_name(name: &std::ffi::OsStr) -> Result<CString> {
    crate::io::c_name(name.as_bytes()).map_err(|_| BulkloadRefusal::PathNotPortable)
}

/// The temporary beside `name`: `.<name>.<token>.bulkload-reseat`, or the
/// shared `.<name>.bulkload-reseat` of a version 2 ledger (empty token).
#[must_use]
pub fn temp_name(name: &std::ffi::OsStr, token: &str) -> std::ffi::OsString {
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    if !token.is_empty() {
        temp.push(".");
        temp.push(token);
    }
    temp.push(TEMP_SUFFIX);
    temp
}

/// Compare the next `length` content bytes with `dest`'s bytes from
/// offset 0, hashing (and scanning) the landed ones.
enum Prefix {
    /// Equal for all `length` bytes.
    Equal,
    /// Different within them.
    Diverged,
    /// The landed content ended first, equal so far: the destination holds
    /// it and more.
    LandingShorter,
}

fn compare_prefix(
    landing: &Landing,
    reader: &mut dyn Read,
    dest: &File,
    length: u64,
    hasher: &mut blake3::Hasher,
    mut scanner: Option<&mut FieldScanner>,
) -> Result<Prefix> {
    let mut a = vec![0_u8; CHUNK];
    let mut b = vec![0_u8; CHUNK];
    let mut at = 0_u64;
    while at < length {
        let want = usize::try_from((length - at).min(CHUNK as u64)).unwrap_or(CHUNK);
        let left = a
            .get_mut(..want)
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
        let read = fill(landing, reader, left, "reseat::place::compare")?;
        if read == 0 {
            return Ok(Prefix::LandingShorter);
        }
        let (left, right) = (
            left.get(..read).unwrap_or_default(),
            b.get_mut(..read)
                .ok_or(BulkloadRefusal::ContractSelfInconsistent)?,
        );
        dest.read_exact_at(right, at)
            .refuse_at("reseat::place::compare_dest")?;
        counters::add_len(Counter::ReseatDestRead, read);
        if let Some(scanner) = scanner.as_deref_mut() {
            if !scanner.complete() {
                scanner.feed(left);
            }
        }
        if left != right {
            return Ok(Prefix::Diverged);
        }
        counters::update(hasher, Counter::HashOther, left);
        at += read as u64;
    }
    Ok(Prefix::Equal)
}

impl Placer<'_> {
    /// Open the parent directory of `rel` under the root, creating missing
    /// directories, or `None` when a component is a symlink or not a
    /// directory (`DESTINATION_OCCUPIED`: nothing is written through it).
    fn parent(&self, rel: &Path) -> Result<Option<File>> {
        let mut at = self.root.to_path_buf();
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err(BulkloadRefusal::PathEscapesRoot);
            };
            let next = at.join(name);
            match lstat(&next)? {
                Some(meta) if meta.is_dir() => {}
                Some(_) => return Ok(None),
                None => {
                    match std::fs::create_dir(&next) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => {
                            return Err(crate::refuse::io_in(&error, &at, "reseat::place::mkdir"))
                        }
                    }
                    if !lstat(&next)?.is_some_and(|meta| meta.is_dir()) {
                        return Ok(None);
                    }
                    crate::io::durable::seal_dir(&open_dir(&at)?)
                        .refuse_at("reseat::place::seal_parent")?;
                }
            }
            at = next;
        }
        open_dir(&at).map(Some)
    }

    /// Place the landed file `landing` at `rel` under the root.
    ///
    /// `want`: top-level fields to scan for in the landed bytes this pass
    /// places or proves (a transcript's `slug`).
    ///
    /// # Errors
    /// I/O and ledger refusals; every kept or refused file is an
    /// [`Outcome`], never an error.
    pub fn place(
        &self,
        landing: &Path,
        rel: &Path,
        kind: Kind,
        session: &str,
        want: Option<&'static [&'static str]>,
    ) -> Result<Placed> {
        let placed = match self.place_inner(landing, rel, kind, session, want) {
            Err(BulkloadRefusal::ReseatUnreadableForm) => Placed {
                outcome: Outcome::Unreadable,
                scanner: None,
            },
            other => other?,
        };
        if placed.outcome == Outcome::Unreadable {
            self.remember_unreadable(landing, rel, kind, session)?;
        }
        Ok(placed)
    }

    /// Ledger an unreadable landed file that holds no placement, so a pass
    /// over the same landed identity does not read it again (S3).
    fn remember_unreadable(
        &self,
        landing: &Path,
        rel: &Path,
        kind: Kind,
        session: &str,
    ) -> Result<()> {
        let dest = self.root.join(rel);
        if self
            .ledger
            .placement(&dest)?
            .is_some_and(|row| row.state.owned() || row.state == State::Continued)
        {
            return Ok(());
        }
        let Some(landed) = identity_at(landing)? else {
            return Ok(());
        };
        self.ledger.put(&Placement {
            dest: dest.clone(),
            landing: landing.to_path_buf(),
            kind: kind.word(),
            session: session.to_owned(),
            state: State::Unreadable,
            landed,
            placed_identity: identity_at(&dest)?.unwrap_or_default(),
            placed: 0,
            digest: None,
            intent: None,
        })
    }

    fn place_inner(
        &self,
        landing: &Path,
        rel: &Path,
        kind: Kind,
        session: &str,
        want: Option<&'static [&'static str]>,
    ) -> Result<Placed> {
        let done = |outcome| {
            Ok(Placed {
                outcome,
                scanner: None,
            })
        };
        if rel
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
        let dest = self.root.join(rel);
        let Some(name) = rel.file_name() else {
            return Err(BulkloadRefusal::PathEscapesRoot);
        };
        let landed_now = match lstat(landing)? {
            Some(meta) if meta.is_file() => Identity::of(&meta),
            _ => return done(Outcome::LandingNotRegular),
        };
        let mut row = self.ledger.placement(&dest)?;
        if row
            .as_ref()
            .is_some_and(|row| row.state == State::Unreadable && row.landed == landed_now)
        {
            // Unreadable as it was last pass: not read again.
            return done(Outcome::Unreadable);
        }
        let archived = self.archived_link(&dest, kind)?;
        if archived.is_some()
            && row.as_ref().is_some_and(|row| {
                row.state == State::Occupied
                    && !(same_stream(&row.landing, landing)
                        && row.landed == landed_now
                        && row.digest.is_some())
            })
        {
            // The destination's archive link, kept (`occupied`) by a pass
            // that did not replace (another landing, or without the
            // flag): nothing is placed there, so it is decided again
            // (A-106). A row that proved the landing equal to the archive
            // target (its digest set) stands while both are unchanged.
            row = None;
        }
        if row
            .as_ref()
            .is_some_and(|row| !same_stream(&row.landing, landing))
        {
            // Another landed file owns this destination path.
            return done(Outcome::Collision);
        }
        let Some(directory) = self.parent(rel)? else {
            return done(Outcome::Occupied);
        };
        let site = Site {
            placer: self,
            dest: &dest,
            name,
            directory: &directory,
            landing_path: landing,
            kind,
            session,
            want,
            lines: line_ruled(&dest),
        };
        if let Some(current) = row.as_ref().filter(|row| row.intent.is_some()) {
            row = site.settle_intent(current.clone())?;
        }
        let landing = Landing::open(landing)?;
        let observed = lstat(&dest)?;
        match row {
            Some(row) if row.state.owned() => site.owned(row, &landing, observed.as_ref()),
            Some(row) if row.state == State::Continued => {
                let both = landing.identity != row.landed;
                done(Outcome::Continued { both })
            }
            Some(row)
                if row.landed == landing.identity
                    && observed.as_ref().map(Identity::of) == Some(row.placed_identity) =>
            {
                done(match row.state {
                    State::Diverged => Outcome::Diverged,
                    State::Occupied => Outcome::Occupied,
                    State::Rewritten => Outcome::Rewritten,
                    State::Unreadable => Outcome::Unreadable,
                    // Owned and continued rows are matched above.
                    State::Ours | State::Adopted | State::Continued => Outcome::Unchanged,
                })
            }
            // A kept file or the landing changed: decide again as if new.
            Some(_) | None => observed.map_or_else(
                || site.fresh(&landing),
                |meta| site.existing(&landing, &meta),
            ),
        }
    }
}

/// One placement in progress.
struct Site<'a> {
    placer: &'a Placer<'a>,
    dest: &'a Path,
    name: &'a std::ffi::OsStr,
    directory: &'a File,
    landing_path: &'a Path,
    kind: Kind,
    session: &'a str,
    want: Option<&'static [&'static str]>,
    /// JSON Lines: whole lines only.
    lines: bool,
}

/// A staged full copy of the landed content.
struct Staged {
    file: File,
    digest: [u8; 32],
    written: u64,
    held: u64,
    scanner: Option<FieldScanner>,
}

/// How an exchange settled.
enum Exchanged {
    /// The staged file is at the destination path and the displaced one
    /// was this ledger's placement (removed).
    Ours,
    /// The destination's file is at the path: handed over.
    HandedOver,
}

impl Site<'_> {
    fn row(
        &self,
        state: State,
        landed: Identity,
        placed_identity: Identity,
        placed: u64,
    ) -> Placement {
        Placement {
            dest: self.dest.to_path_buf(),
            landing: self.landing_path.to_path_buf(),
            kind: self.kind.word(),
            session: self.session.to_owned(),
            state,
            landed,
            placed_identity,
            placed,
            digest: None,
            intent: None,
        }
    }

    fn temp(&self) -> PathBuf {
        self.dest
            .with_file_name(temp_name(self.name, self.placer.ledger.temp_token()))
    }

    fn temp_c(&self) -> Result<CString> {
        c_name(temp_name(self.name, self.placer.ledger.temp_token()).as_os_str())
    }

    /// Remove a temporary a crashed pass left before it published (no
    /// intent names it, so it is a staged copy of a landed file).
    fn clear_temp(&self) -> Result<()> {
        if lstat(&self.temp())?.is_some_and(|meta| meta.is_file()) {
            crate::io::sys::unlinkat(self.directory, &self.temp_c()?, false)
                .refuse_at("reseat::place::clear_temp")?;
        }
        Ok(())
    }

    /// Stage a full copy of the landed content (whole lines for JSON
    /// Lines) in the temporary: written, its mode and modification time
    /// set, sealed.
    fn stage(&self, landing: &Landing) -> Result<Staged> {
        self.clear_temp()?;
        let mut temp = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.temp())
            .map_err(|error| crate::refuse::io_in(&error, self.dest, "reseat::place::stage"))?;
        let mut hasher = blake3::Hasher::new();
        let mut scanner = self.want.map(FieldScanner::new);
        let mut reader = landing.reader()?;
        let pumped = pump(
            landing,
            &mut reader,
            self.lines,
            &mut hasher,
            scanner.as_mut(),
            &mut |bytes| {
                temp.write_all(bytes)
                    .refuse_at("reseat::place::stage_write")?;
                counters::add_len(Counter::ReseatDestWrite, bytes.len());
                Ok(true)
            },
        )?;
        temp.set_permissions(std::fs::Permissions::from_mode(
            landing.meta.permissions().mode() & 0o777,
        ))
        .refuse_at("reseat::place::stage_mode")?;
        temp.set_modified(
            landing
                .meta
                .modified()
                .refuse_at("reseat::place::stage_mtime")?,
        )
        .refuse_at("reseat::place::stage_mtime")?;
        crate::io::durable::seal_file(&temp).refuse_at("reseat::place::stage_seal")?;
        Ok(Staged {
            file: temp,
            digest: *hasher.finalize().as_bytes(),
            written: pumped.written,
            held: pumped.held,
            scanner,
        })
    }

    /// [`Self::stage`], with the temporary removed when staging fails: an
    /// unreadable landing is `None`; any other refusal (a full file system)
    /// is the run's, and no staged temporary is left in the live tree.
    fn stage_or_clear(&self, landing: &Landing) -> Result<Option<Staged>> {
        match self.stage(landing) {
            Ok(staged) => Ok(Some(staged)),
            Err(BulkloadRefusal::ReseatUnreadableForm) => {
                self.clear_temp()?;
                Ok(None)
            }
            Err(refusal) => {
                let _ = self.clear_temp();
                Err(refusal)
            }
        }
    }

    /// `D` absent: stage, publish without replacing, commit.
    fn fresh(&self, landing: &Landing) -> Result<Placed> {
        let Some(staged) = self.stage_or_clear(landing)? else {
            return Ok(Placed {
                outcome: Outcome::Unreadable,
                scanner: None,
            });
        };
        if self.lines && staged.written == 0 && staged.held > 0 {
            // Not one whole line yet: nothing to place.
            self.clear_temp()?;
            return Ok(Placed {
                outcome: Outcome::Held,
                scanner: None,
            });
        }
        fault_point!(ReseatAfterTempSeal);
        match crate::io::publish_noreplace(self.directory, &self.temp_c()?, &c_name(self.name)?) {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                // Something appeared at the path: leave it, decide again.
                self.clear_temp()?;
                let meta = lstat(self.dest)?.ok_or(BulkloadRefusal::SourceChangedAfterSnapshot)?;
                return self.existing(landing, &meta);
            }
            Err(error) => return Err(error).refuse_at("reseat::place::publish"),
        }
        crate::io::durable::seal_dir(self.directory).refuse_at("reseat::place::publish_seal")?;
        fault_point!(ReseatAfterPublish);
        let placed = Identity::of(
            &staged
                .file
                .metadata()
                .refuse_at("reseat::place::published")?,
        );
        let mut row = self.row(State::Ours, landing.identity, placed, staged.written);
        row.digest = Some(staged.digest);
        self.placer.ledger.put(&row)?;
        Ok(Placed {
            outcome: Outcome::Placed,
            scanner: staged.scanner,
        })
    }

    /// `D` present with no owned row: adopt, extend a prefix, or keep.
    fn existing(&self, landing: &Landing, meta: &std::fs::Metadata) -> Result<Placed> {
        let observed = Identity::of(meta);
        let keep = |state: State, outcome: Outcome, scanner| -> Result<Placed> {
            self.placer
                .ledger
                .put(&self.row(state, landing.identity, observed, 0))?;
            Ok(Placed { outcome, scanner })
        };
        if meta.is_symlink() {
            if let Some((text, resolved, link)) = self.placer.archived_link(self.dest, self.kind)? {
                return self.replace_archived(landing, (&text, &resolved, &link));
            }
        }
        if !meta.is_file() || meta.nlink() > 1 {
            // Another name for these bytes (an archive copy, the landing
            // itself) would change with them.
            return keep(State::Occupied, Outcome::Occupied, None);
        }
        let dest = crate::hash::open_nofollow(self.dest)?;
        let mut scanner = self.want.map(FieldScanner::new);
        let mut hasher = blake3::Hasher::new();
        let mut reader = landing.reader()?;
        match compare_prefix(
            landing,
            &mut reader,
            &dest,
            observed.size,
            &mut hasher,
            scanner.as_mut(),
        )? {
            Prefix::Diverged => return keep(State::Diverged, Outcome::Diverged, scanner),
            Prefix::LandingShorter => return keep(State::Continued, Outcome::DestAhead, scanner),
            Prefix::Equal => {}
        }
        let mut row = self.row(State::Adopted, landing.identity, observed, observed.size);
        row.digest = Some(*hasher.finalize().as_bytes());
        let Some(first) = peek(landing, &mut reader)? else {
            // Equal.
            self.placer.ledger.put(&row)?;
            return Ok(Placed {
                outcome: Outcome::Adopted,
                scanner,
            });
        };
        if !appendable(self.dest) {
            return keep(State::Diverged, Outcome::Diverged, scanner);
        }
        // A strict prefix: adopted at its size, then extended.
        row.landed = Identity::default();
        self.placer.ledger.put(&row)?;
        let mut rest = std::io::Cursor::new([first]).chain(reader);
        let mid_line =
            observed.size > 0 && last_byte(&dest, observed.size)? != Some(b'\n') && self.lines;
        let placed = self.extend(row, landing, &mut rest, hasher, scanner, mid_line)?;
        Ok(match placed.outcome {
            // Nothing whole to append: adopted as it is.
            Outcome::Unchanged => Placed {
                outcome: Outcome::Adopted,
                scanner: placed.scanner,
            },
            _ => placed,
        })
    }

    /// `D` is the destination's symlink into a `--replace-archived-dest`
    /// root (A-106: the operator's ruling that the landed version wins and
    /// the fork stays in the archive).
    ///
    /// The landed content is staged beside it (whole lines, its mode and
    /// mtime, sealed) and compared with the target by BLAKE3: equal, the
    /// link stays (`occupied`); the landing a strict prefix of the target
    /// (the archive ahead, not a fork), the link stays (`dest-ahead`).
    /// Both are recorded with the target's identity and not read again
    /// while it, the link and the landing are unchanged. A target some
    /// process holds open stays (`occupied`). Otherwise the link text, the
    /// target's real path, identity and digest are ledgered (`intent`);
    /// the target and link are verified unchanged; the copy and the link
    /// are exchanged atomically; the link and the target are verified
    /// again (else everything is put back); the link (now under the
    /// temporary name) is removed and the directory sealed; then the row
    /// is `done` and the placement row names the landed file, in one
    /// commit. The archive target is never written or removed; `--undo`
    /// puts the exact link text back over an unchanged copy and a verified
    /// target.
    #[allow(clippy::too_many_lines)]
    fn replace_archived(
        &self,
        landing: &Landing,
        (text, resolved, link): (&Path, &Path, &std::fs::Metadata),
    ) -> Result<Placed> {
        let ledger = self.placer.ledger;
        let link_identity = Identity::of(link);
        let kept = |outcome: Outcome, proof: Option<([u8; 32], Identity, u64)>| -> Result<Placed> {
            // A proved row records the target's identity (in place of the
            // link's) and whether the landing equals it or is a prefix of
            // it; an unproved one the link's, decided again next pass.
            let row = match proof {
                Some((digest, target, relation)) => {
                    let mut row = self.row(State::Occupied, landing.identity, target, relation);
                    row.digest = Some(digest);
                    row
                }
                None => self.row(State::Occupied, landing.identity, link_identity, 0),
            };
            ledger.put(&row)?;
            Ok(Placed {
                outcome,
                scanner: None,
            })
        };
        let occupied = || kept(Outcome::Occupied, None);
        let now_target = lstat(resolved)?.map(|meta| Identity::of(&meta));
        if let Some(row) = ledger.placement(self.dest)? {
            if row.state == State::Occupied
                && same_stream(&row.landing, self.landing_path)
                && row.landed == landing.identity
                && row.digest.is_some()
                && now_target == Some(row.placed_identity)
            {
                // Proved before; the landing and the target unchanged.
                return Ok(Placed {
                    outcome: if row.placed == LINK_AHEAD {
                        Outcome::DestAhead
                    } else {
                        Outcome::Occupied
                    },
                    scanner: None,
                });
            }
        }
        if open_by_a_process(resolved) {
            // A live session holds the archive target open: its writes
            // would go on into the archive after the swap.
            return occupied();
        }
        let Some(staged) = self.stage_or_clear(landing)? else {
            return Ok(Placed {
                outcome: Outcome::Unreadable,
                scanner: None,
            });
        };
        let Some(target) = target_digests(resolved, staged.written)? else {
            // Not a regular file, or written while it was read.
            self.clear_temp()?;
            return occupied();
        };
        if staged.written == target.0.size && staged.digest == target.1 {
            // The landed file is the archive's: nothing to replace.
            self.clear_temp()?;
            return kept(Outcome::Occupied, Some((target.1, target.0, LINK_EQUAL)));
        }
        if self.lines && staged.written == 0 && staged.held > 0 {
            self.clear_temp()?;
            return Ok(Placed {
                outcome: Outcome::Held,
                scanner: None,
            });
        }
        if target.2 == Some(staged.digest) {
            // The archive holds the landed lines and more: ahead, not a
            // fork; kept like a destination that continued (review A3).
            self.clear_temp()?;
            return kept(Outcome::DestAhead, Some((target.1, target.0, LINK_AHEAD)));
        }
        let copy = Identity::of(
            &staged
                .file
                .metadata()
                .refuse_at("reseat::place::replace_staged")?,
        );
        let regular = Regular {
            link: self.dest.to_path_buf(),
            target: text.to_path_buf(),
            resolved: resolved.to_path_buf(),
            link_identity,
            target_identity: target.0,
            regular: copy,
            digest: Some(target.1),
            state: RegularState::Intent,
            replace: true,
        };
        let mut placement = self.row(State::Ours, landing.identity, copy, staged.written);
        placement.digest = Some(staged.digest);
        let unchanged = || {
            lstat(resolved)
                .ok()
                .flatten()
                .map(|meta| Identity::of(&meta))
                == Some(target.0)
                && lstat(self.dest)
                    .ok()
                    .flatten()
                    .map(|meta| Identity::of(&meta))
                    == Some(link_identity)
                && std::fs::read_link(self.dest).is_ok_and(|now| now == text)
        };
        if !unchanged() {
            // The link or the archive moved since they were read: kept, the
            // staged copy removed (the landing holds its bytes).
            self.clear_temp()?;
            return occupied();
        }
        ledger.put_regular(&regular)?;
        fault_point!(ReseatAfterReplaceIntent);
        let name = c_name(self.name)?;
        let temp = self.temp_c()?;
        let undo_intent = || ledger.delete_regular(self.dest);
        if !unchanged() || open_by_a_process(resolved) {
            self.clear_temp()?;
            undo_intent()?;
            return occupied();
        }
        if let Err(error) = crate::io::exchange(self.directory, &temp, &name) {
            self.clear_temp()?;
            undo_intent()?;
            if crate::io::rename_unsupported(&error) {
                return Ok(Placed {
                    outcome: Outcome::ExchangeUnsupported,
                    scanner: None,
                });
            }
            return Err(error).refuse_at("reseat::place::replace_exchange");
        }
        fault_point!(ReseatAfterReplaceExchange);
        replace_hook(self.dest);
        let link_at_temp = lstat(&self.temp())?
            .is_some_and(|meta| meta.is_symlink() && Identity::of(&meta) == link_identity)
            && std::fs::read_link(self.temp()).is_ok_and(|now| now == text);
        let target_held = lstat(resolved)?.map(|meta| Identity::of(&meta)) == Some(target.0);
        if !link_at_temp || !target_held {
            // Something other than the link was displaced, or the archive
            // was written under the swap: put it back as it was; the
            // staged copy (the landing holds its bytes) removed.
            crate::io::exchange(self.directory, &temp, &name)
                .refuse_at("reseat::place::replace_exchange_back")?;
            self.clear_temp()?;
            crate::io::durable::seal_dir(self.directory)
                .refuse_at("reseat::place::replace_seal")?;
            undo_intent()?;
            return occupied();
        }
        crate::io::sys::unlinkat(self.directory, &temp, false)
            .refuse_at("reseat::place::replace_unlink_link")?;
        crate::io::durable::seal_dir(self.directory).refuse_at("reseat::place::replace_seal")?;
        fault_point!(ReseatBeforeReplaceDone);
        ledger.put_replace_done(
            &Regular {
                state: RegularState::Done,
                ..regular
            },
            &placement,
        )?;
        counters::bump(Counter::ReseatArchivedReplaced);
        Ok(Placed {
            outcome: Outcome::ReplacedArchived,
            scanner: staged.scanner,
        })
    }

    /// `D` is this ledger's: unchanged, extend, supersede, or hand over.
    fn owned(
        &self,
        row: Placement,
        landing: &Landing,
        observed: Option<&std::fs::Metadata>,
    ) -> Result<Placed> {
        let done = |outcome| {
            Ok(Placed {
                outcome,
                scanner: None,
            })
        };
        let Some(meta) = observed else {
            self.placer.ledger.put(&Placement {
                state: State::Continued,
                ..row
            })?;
            return done(Outcome::Removed);
        };
        if Identity::of(meta) != row.placed_identity {
            let both = landing.identity != row.landed;
            self.placer.ledger.put(&Placement {
                state: State::Continued,
                placed_identity: Identity::of(meta),
                ..row
            })?;
            return done(Outcome::Continued { both });
        }
        if landing.identity == row.landed {
            return done(Outcome::Unchanged);
        }
        if meta.nlink() > 1 {
            // Linked elsewhere since it was placed: never written again.
            self.placer.ledger.put(&Placement {
                state: State::Occupied,
                landed: landing.identity,
                ..row
            })?;
            return done(Outcome::Occupied);
        }
        let unchanged = |row: Placement| -> Result<Placed> {
            self.placer.ledger.put(&Placement {
                landed: landing.identity,
                landing: self.landing_path.to_path_buf(),
                ..row
            })?;
            done(Outcome::Unchanged)
        };
        let mut reader = landing.reader()?;
        let mut hasher = blake3::Hasher::new();
        if !appendable(self.dest) {
            // A plan or a memory note: equal bytes keep the row; any change
            // supersedes it.
            let whole = read_exact_hash(landing, &mut reader, row.placed, &mut hasher, None)?;
            if whole != Prefixed::Short
                && peek(landing, &mut reader)?.is_none()
                && Some(*hasher.finalize().as_bytes()) == row.digest
            {
                return unchanged(row);
            }
            return self.supersede(row, landing);
        }
        // Every placed byte must still be the landing's, before anything
        // is appended after them.
        let Prefixed::Read(last) =
            read_exact_hash(landing, &mut reader, row.placed, &mut hasher, None)?
        else {
            return self.rewritten(row, landing);
        };
        if row.digest.is_none() || Some(*hasher.finalize().as_bytes()) != row.digest {
            return self.rewritten(row, landing);
        }
        let Some(first) = peek(landing, &mut reader)? else {
            return unchanged(row);
        };
        let mut rest = std::io::Cursor::new([first]).chain(reader);
        let mid_line = self.lines && row.placed > 0 && last != Some(b'\n');
        let scanner = None;
        self.extend(row, landing, &mut rest, hasher, scanner, mid_line)
    }

    fn rewritten(&self, row: Placement, landing: &Landing) -> Result<Placed> {
        self.placer.ledger.put(&Placement {
            state: State::Rewritten,
            landed: landing.identity,
            landing: self.landing_path.to_path_buf(),
            ..row
        })?;
        Ok(Placed {
            outcome: Outcome::Rewritten,
            scanner: None,
        })
    }

    /// Append the rest of `reader` (the landed content after `row.placed`,
    /// proven by `hasher`) in whole lines, under an intent, checking the
    /// destination's size before every write.
    #[allow(clippy::too_many_lines)]
    fn extend(
        &self,
        row: Placement,
        landing: &Landing,
        reader: &mut dyn Read,
        mut hasher: blake3::Hasher,
        scanner: Option<FieldScanner>,
        mid_line: bool,
    ) -> Result<Placed> {
        let mut intent = row.clone();
        intent.intent = Some(Intent::Extend);
        intent.landing = self.landing_path.to_path_buf();
        self.placer.ledger.put(&intent)?;
        fault_point!(ReseatAfterExtendIntent);
        let mut dest = OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.dest)
            .refuse_at("reseat::place::extend_open")?;
        let size_now = |dest: &File| -> Result<Identity> {
            Ok(Identity::of(
                &dest.metadata().refuse_at("reseat::place::extend_stat")?,
            ))
        };
        if size_now(&dest)? != row.placed_identity {
            return self.hand_over(row, landing, Outcome::Continued { both: true });
        }
        let mut scanner = match (scanner, self.want) {
            (Some(scanner), _) => Some(scanner),
            (None, Some(want)) if mid_line => Some(FieldScanner::mid_line(want)),
            (None, Some(want)) => Some(FieldScanner::new(want)),
            (None, None) => None,
        };
        let mut expected = row.placed;
        let mut raced = false;
        let mut first = true;
        let pumped = pump(
            landing,
            reader,
            self.lines,
            &mut hasher,
            scanner.as_mut(),
            &mut |bytes| {
                // A destination writer appended since this pass's last
                // write: stop, with every line whole. (The window between
                // this check and the write is detected after it, below.)
                if size_now(&dest)?.size != expected {
                    raced = true;
                    return Ok(false);
                }
                dest.write_all(bytes)
                    .refuse_at("reseat::place::extend_write")?;
                counters::add_len(Counter::ReseatDestWrite, bytes.len());
                expected += bytes.len() as u64;
                if first {
                    fault_point!(ReseatMidExtend);
                }
                first = false;
                Ok(true)
            },
        );
        let pumped = match pumped {
            Ok(pumped) => pumped,
            Err(BulkloadRefusal::ReseatUnreadableForm) => {
                // The landed form broke off mid-stream (a truncated
                // `.zst`): what was appended is whole lines of it, proven by
                // the hash of every byte placed. Keep them as this
                // ledger's; the landing is decided again next pass.
                let placed = size_now(&dest)?;
                if placed.size != expected {
                    return self.hand_over(row, landing, Outcome::Raced);
                }
                crate::io::durable::seal_file(&dest).refuse_at("reseat::place::extend_seal")?;
                let mut kept = self.row(State::Ours, Identity::default(), placed, expected);
                kept.digest = Some(*hasher.finalize().as_bytes());
                self.placer.ledger.put(&kept)?;
                return Ok(Placed {
                    outcome: Outcome::Unreadable,
                    scanner: None,
                });
            }
            Err(refusal) => return Err(refusal),
        };
        if raced || size_now(&dest)?.size != expected {
            return self.hand_over(row, landing, Outcome::Raced);
        }
        if pumped.written == 0 {
            // Only a line not yet whole: nothing appended.
            self.placer.ledger.put(&Placement {
                landed: landing.identity,
                landing: self.landing_path.to_path_buf(),
                intent: None,
                ..row
            })?;
            return Ok(Placed {
                outcome: Outcome::Unchanged,
                scanner,
            });
        }
        dest.set_modified(
            landing
                .meta
                .modified()
                .refuse_at("reseat::place::extend_mtime")?,
        )
        .refuse_at("reseat::place::extend_mtime")?;
        crate::io::durable::seal_file(&dest).refuse_at("reseat::place::extend_seal")?;
        fault_point!(ReseatAfterExtend);
        let placed = size_now(&dest)?;
        if placed.size != expected {
            return self.hand_over(row, landing, Outcome::Raced);
        }
        let mut done = self.row(State::Ours, landing.identity, placed, expected);
        done.digest = Some(*hasher.finalize().as_bytes());
        self.placer.ledger.put(&done)?;
        Ok(Placed {
            outcome: Outcome::Extended,
            scanner,
        })
    }

    /// The destination owns the file from now on.
    fn hand_over(&self, row: Placement, landing: &Landing, outcome: Outcome) -> Result<Placed> {
        let observed = identity_at(self.dest)?.unwrap_or(row.placed_identity);
        let both = landing.identity != row.landed;
        self.placer.ledger.put(&Placement {
            state: State::Continued,
            placed_identity: observed,
            intent: None,
            ..row
        })?;
        Ok(Placed {
            outcome: match outcome {
                Outcome::Continued { .. } => Outcome::Continued { both },
                other => other,
            },
            scanner: None,
        })
    }

    /// Exchange a changed non-append landed file in for this ledger's
    /// placement, under an intent, and check what it displaced.
    fn supersede(&self, row: Placement, landing: &Landing) -> Result<Placed> {
        let Some(staged) = self.stage_or_clear(landing)? else {
            return Ok(Placed {
                outcome: Outcome::Unreadable,
                scanner: None,
            });
        };
        let staged_identity =
            Identity::of(&staged.file.metadata().refuse_at("reseat::place::staged")?);
        let mut intent = row.clone();
        intent.intent = Some(Intent::Supersede(staged_identity));
        self.placer.ledger.put(&intent)?;
        let (temp_c, name_c) = (self.temp_c()?, c_name(self.name)?);
        if let Err(error) = crate::io::exchange(self.directory, &temp_c, &name_c) {
            if !crate::io::rename_unsupported(&error) {
                return Err(error).refuse_at("reseat::place::exchange");
            }
            crate::io::sys::unlinkat(self.directory, &temp_c, false)
                .refuse_at("reseat::place::exchange_unsupported")?;
            self.placer.ledger.put(&row)?;
            return Ok(Placed {
                outcome: Outcome::ExchangeUnsupported,
                scanner: None,
            });
        }
        fault_point!(ReseatAfterSupersedeExchange);
        match self.settle_exchange(&row, staged_identity)? {
            Exchanged::HandedOver => {
                self.hand_over(row, landing, Outcome::Continued { both: true })
            }
            Exchanged::Ours => {
                let mut done = self.row(
                    State::Ours,
                    landing.identity,
                    staged_identity,
                    staged.written,
                );
                done.digest = Some(staged.digest);
                self.placer.ledger.put(&done)?;
                Ok(Placed {
                    outcome: Outcome::Superseded,
                    scanner: staged.scanner,
                })
            }
        }
    }

    /// After an exchange: the staged file (identity `staged`) should be at
    /// the path and this ledger's placement under the temporary name.
    ///
    /// - Both so: the placement is removed; ours.
    /// - The staged file at the path, something else displaced (the
    ///   destination changed it before the exchange): exchanged back, the
    ///   staged copy removed; handed over.
    /// - The staged file at the path changed since (edited in place after
    ///   a crash): the destination's; this ledger's displaced placement is
    ///   removed, anything else displaced is left; handed over.
    fn settle_exchange(&self, row: &Placement, staged: Identity) -> Result<Exchanged> {
        let (temp_c, name_c) = (self.temp_c()?, c_name(self.name)?);
        let at_path = identity_at(self.dest)?;
        let displaced = identity_at(&self.temp())?;
        let displaced_ours = displaced == Some(row.placed_identity);
        let settled = if at_path == Some(staged) {
            match displaced {
                None => Exchanged::Ours,
                Some(_) if displaced_ours => {
                    crate::io::sys::unlinkat(self.directory, &temp_c, false)
                        .refuse_at("reseat::place::displaced")?;
                    Exchanged::Ours
                }
                Some(_) => {
                    crate::io::exchange(self.directory, &temp_c, &name_c)
                        .refuse_at("reseat::place::exchange_back")?;
                    if identity_at(&self.temp())? == Some(staged) {
                        crate::io::sys::unlinkat(self.directory, &temp_c, false)
                            .refuse_at("reseat::place::exchange_back_staged")?;
                    }
                    Exchanged::HandedOver
                }
            }
        } else {
            if displaced_ours {
                crate::io::sys::unlinkat(self.directory, &temp_c, false)
                    .refuse_at("reseat::place::displaced")?;
            }
            Exchanged::HandedOver
        };
        crate::io::durable::seal_dir(self.directory).refuse_at("reseat::place::supersede_seal")?;
        Ok(settled)
    }

    /// Finish or undo what a crash left under an intent; returns the row
    /// the pass continues from.
    fn settle_intent(&self, row: Placement) -> Result<Option<Placement>> {
        match row.intent {
            None => Ok(Some(row)),
            Some(Intent::Supersede(staged)) => self.settle_supersede(row, staged),
            Some(Intent::Extend) => self.settle_extend(row),
        }
    }

    fn settle_supersede(&self, row: Placement, staged: Identity) -> Result<Option<Placement>> {
        let cleared = Placement {
            intent: None,
            ..row.clone()
        };
        if identity_at(&self.temp())? == Some(staged) {
            // Not exchanged: the temporary is the staged copy.
            self.clear_temp()?;
            self.placer.ledger.put(&cleared)?;
            return Ok(Some(cleared));
        }
        let exchanged = identity_at(self.dest)?
            .is_some_and(|now| now.dev == staged.dev && now.ino == staged.ino);
        if !exchanged {
            // The staged file is at neither name: decide from the
            // destination's file as it is.
            self.placer.ledger.put(&cleared)?;
            return Ok(Some(cleared));
        }
        match self.settle_exchange(&row, staged)? {
            Exchanged::HandedOver => {
                let landing = Landing::open(self.landing_path)?;
                self.hand_over(row, &landing, Outcome::Continued { both: true })?;
                self.placer.ledger.placement(self.dest)
            }
            Exchanged::Ours => {
                // The staged bytes are placed; the landing may have moved
                // since, so the pass decides again from them.
                let mut hasher = blake3::Hasher::new();
                let mut file = crate::hash::open_nofollow(self.dest)?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .refuse_at("reseat::place::settle_supersede")?;
                counters::add_len(Counter::ReseatDestRead, bytes.len());
                counters::update(&mut hasher, Counter::HashOther, &bytes);
                let next = Placement {
                    state: State::Ours,
                    landed: Identity::default(),
                    placed_identity: staged,
                    placed: bytes.len() as u64,
                    digest: Some(*hasher.finalize().as_bytes()),
                    intent: None,
                    ..row
                };
                self.placer.ledger.put(&next)?;
                Ok(Some(next))
            }
        }
    }

    /// After a crash inside an extension: the destination's bytes past
    /// `placed` are adopted when they are exactly the landing's next bytes
    /// (this pass's own writes); otherwise the file is handed over.
    fn settle_extend(&self, row: Placement) -> Result<Option<Placement>> {
        let landing = Landing::open(self.landing_path)?;
        let Some(meta) = lstat(self.dest)? else {
            let next = Placement {
                intent: None,
                ..row
            };
            self.placer.ledger.put(&next)?;
            return Ok(Some(next));
        };
        let size = meta.size();
        let same_file = meta.is_file()
            && meta.ino() == row.placed_identity.ino
            && meta.dev() == row.placed_identity.dev
            && size >= row.placed;
        let mut hasher = blake3::Hasher::new();
        let proven = same_file && {
            let mut reader = landing.reader()?;
            read_exact_hash(&landing, &mut reader, row.placed, &mut hasher, None)?
                != Prefixed::Short
                && Some(*hasher.clone().finalize().as_bytes()) == row.digest
                && {
                    let dest = crate::hash::open_nofollow(self.dest)?;
                    let mut appended = vec![0_u8; usize::try_from(size - row.placed).unwrap_or(0)];
                    dest.read_exact_at(&mut appended, row.placed)
                        .refuse_at("reseat::place::settle_dest")?;
                    counters::add_len(Counter::ReseatDestRead, appended.len());
                    let mut landed = vec![0_u8; appended.len()];
                    let mut at = 0;
                    while at < landed.len() {
                        let read = fill(
                            &landing,
                            &mut reader,
                            landed.get_mut(at..).unwrap_or_default(),
                            "reseat::place::settle_landing",
                        )?;
                        if read == 0 {
                            break;
                        }
                        at += read;
                    }
                    counters::update(&mut hasher, Counter::HashOther, &appended);
                    at == landed.len() && landed == appended
                }
        };
        if !proven {
            self.hand_over(row, &landing, Outcome::Continued { both: true })?;
            return self.placer.ledger.placement(self.dest);
        }
        // The appended part is the landing's: continue from it.
        let next = Placement {
            placed: size,
            placed_identity: Identity::of(&meta),
            landed: Identity::default(),
            digest: Some(*hasher.finalize().as_bytes()),
            intent: None,
            state: State::Ours,
            ..row
        };
        self.placer.ledger.put(&next)?;
        Ok(Some(next))
    }
}

/// The byte before offset `size` of `file`.
fn last_byte(file: &File, size: u64) -> Result<Option<u8>> {
    if size == 0 {
        return Ok(None);
    }
    let mut one = [0_u8; 1];
    file.read_exact_at(&mut one, size - 1)
        .refuse_at("reseat::place::last_byte")?;
    counters::add_len(Counter::ReseatDestRead, 1);
    Ok(Some(one[0]))
}
