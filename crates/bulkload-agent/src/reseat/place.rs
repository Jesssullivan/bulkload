//! One file's placement into the destination's live agent state (R0,
//! TIN-6587): the never-overwrite rules, the ledger and the crash points.
//!
//! For a destination path `D` and its landed file `L`:
//!
//! - **`D` absent**: copy `L` into a temporary beside `D`
//!   (`.<name>.bulkload-reseat`), give it `L`'s mode and modification time
//!   (Claude Code's `--continue` takes the newest file), seal it, publish it
//!   with a no-replace rename, seal the directory, then commit the row. A
//!   regular file, never a symlink (Claude Code lists only regular files)
//!   and never a hardlink (a destination resume would append to the
//!   landing).
//! - **`D` present, no row**: a regular file equal to `L` is adopted; a
//!   strict prefix of `L` is adopted and extended when the file is
//!   append-only (`.jsonl`, `.txt`); anything else is kept as it is and
//!   reported (`RESEAT_DEST_DIVERGED`, or `RESEAT_DEST_CONTINUED` when `L`
//!   is a prefix of `D`). A symlink or other node is never written
//!   (`DESTINATION_OCCUPIED`).
//! - **`D` holds this ledger's bytes**: unchanged identities read nothing
//!   (S3). A grown append-only `L` is proven to extend the placed bytes by
//!   the two window digests the row keeps (the first and last 64 KiB of
//!   them, R25), and only its new tail is read and appended
//!   (`O_APPEND`, so a concurrent destination writer is never overwritten),
//!   under an intent row a crash resumes from. A changed file that is not
//!   append-only (a plan, a memory note) supersedes the placement through an
//!   atomic exchange, checked afterwards: a displaced file that is not this
//!   ledger's placement is exchanged back.
//! - **`D` changed since bulkload's last write** (a session resumed there):
//!   the destination owns it from then on (`RESEAT_DEST_CONTINUED`); it is
//!   never extended again, and newer landed bytes stay in the landing.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use super::json::FieldScanner;
use super::ledger::{Identity, Intent, Ledger, Placement, State};
use super::Kind;
use crate::counters::{self, Counter};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// Bytes per read and write.
const CHUNK: usize = 1024 * 1024;
/// [`CHUNK`] as a file offset.
const CHUNK_BYTES: u64 = 1024 * 1024;
/// The window a row keeps a digest of at each end of the placed bytes.
pub const WINDOW: u64 = 64 * 1024;
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
    /// This ledger's placement, with nothing new: nothing read.
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
    /// The destination file system has no atomic exchange for a
    /// supersede.
    ExchangeUnsupported,
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
            Self::ExchangeUnsupported => "exchange-unsupported",
        }
    }

    /// Whether the destination file now holds the landed bytes.
    #[must_use]
    pub const fn landed(self) -> bool {
        matches!(
            self,
            Self::Placed | Self::Adopted | Self::Extended | Self::Superseded | Self::Unchanged
        )
    }

    /// The typed refusal a kept file reports, if any.
    #[must_use]
    pub const fn refusal(self) -> Option<BulkloadRefusal> {
        match self {
            Self::Placed | Self::Adopted | Self::Extended | Self::Superseded | Self::Unchanged => {
                None
            }
            Self::Diverged => Some(BulkloadRefusal::ReseatDestDiverged),
            Self::DestAhead | Self::Continued { .. } | Self::Removed => {
                Some(BulkloadRefusal::ReseatDestContinued)
            }
            Self::Occupied => Some(BulkloadRefusal::DestinationOccupied),
            Self::Rewritten => Some(BulkloadRefusal::ReseatSourceRewritten),
            Self::LandingNotRegular => Some(BulkloadRefusal::ReseatUnreadableForm),
            Self::ExchangeUnsupported => Some(BulkloadRefusal::DestinationExchangeUnsupported),
        }
    }
}

/// A placement's result: its outcome and, when asked, what the scanner saw
/// in the landed bytes this pass read.
pub struct Placed {
    pub outcome: Outcome,
    pub scanner: Option<FieldScanner>,
}

/// Whether a file at `path` is append-only: a transcript, a subagent
/// transcript, a debug log or a tool result.
#[must_use]
pub fn appendable(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl" || extension == "txt")
}

/// Places files under one canonical destination root.
pub struct Placer<'a> {
    pub ledger: &'a Ledger,
    pub root: &'a Path,
}

/// A landed file opened for this pass, with its identity.
struct Landing {
    file: File,
    meta: std::fs::Metadata,
    identity: Identity,
}

impl Landing {
    fn open(path: &Path) -> Result<Self> {
        let file = crate::hash::open_nofollow(path)?;
        let meta = file.metadata().refuse_at("reseat::place::landing")?;
        Ok(Self {
            identity: Identity::of(&meta),
            file,
            meta,
        })
    }

    fn read_at(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
        let mut bytes =
            vec![0_u8; usize::try_from(length).map_err(|_| BulkloadRefusal::BudgetExceeded)?];
        self.file
            .read_exact_at(&mut bytes, offset)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::UnexpectedEof => BulkloadRefusal::SourceChangedAfterSnapshot,
                _ => crate::refuse::io(&error, "reseat::place::read_at"),
            })?;
        counters::add_len(Counter::ReseatLandingRead, bytes.len());
        Ok(bytes)
    }
}

/// The digests of the first and last [`WINDOW`] bytes of `size` bytes,
/// and the last window's bytes.
fn windows(
    read: &dyn Fn(u64, u64) -> Result<Vec<u8>>,
    size: u64,
) -> Result<([u8; 32], [u8; 32], Vec<u8>)> {
    let span = size.min(WINDOW);
    let head = read(0, span)?;
    let tail = if size <= WINDOW {
        head.clone()
    } else {
        read(size - span, span)?
    };
    Ok((
        counters::hash(Counter::HashOther, &head),
        counters::hash(Counter::HashOther, &tail),
        tail,
    ))
}

/// The running digests of bytes streamed into a placement.
struct Stream {
    full: Option<blake3::Hasher>,
    head: Vec<u8>,
    tail: Vec<u8>,
    window: usize,
}

impl Stream {
    fn new(full: bool, head: Vec<u8>, tail: Vec<u8>) -> Self {
        Self {
            full: full.then(blake3::Hasher::new),
            head,
            tail,
            window: usize::try_from(WINDOW).unwrap_or(usize::MAX),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        if let Some(full) = self.full.as_mut() {
            counters::update(full, Counter::HashOther, bytes);
        }
        if self.head.len() < self.window {
            let take = (self.window - self.head.len()).min(bytes.len());
            self.head
                .extend_from_slice(bytes.get(..take).unwrap_or_default());
        }
        if bytes.len() >= self.window {
            self.tail.clear();
            self.tail
                .extend_from_slice(bytes.get(bytes.len() - self.window..).unwrap_or_default());
        } else {
            self.tail.extend_from_slice(bytes);
            let excess = self.tail.len().saturating_sub(self.window);
            self.tail.drain(..excess);
        }
    }

    fn finish(self) -> (Option<[u8; 32]>, [u8; 32], [u8; 32]) {
        (
            self.full.map(|full| *full.finalize().as_bytes()),
            counters::hash(Counter::HashOther, &self.head),
            counters::hash(Counter::HashOther, &self.tail),
        )
    }
}

fn lstat(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).refuse_at("reseat::place::lstat"),
    }
}

fn open_dir(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .refuse_at("reseat::place::open_dir")
}

fn c_name(name: &std::ffi::OsStr) -> Result<CString> {
    crate::io::c_name(name.as_bytes()).map_err(|_| BulkloadRefusal::PathNotPortable)
}

fn temp_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(TEMP_SUFFIX);
    temp
}

/// Copy `length` bytes of `from` (from its current offset) to `to`,
/// through `stream` and `scanner`.
fn copy(
    from: &File,
    to: &mut File,
    length: u64,
    stream: &mut Stream,
    scanner: Option<&mut FieldScanner>,
    mid: bool,
) -> Result<()> {
    let mut scanner = scanner;
    let mut reader = from;
    let mut left = length;
    let mut buffer = vec![0_u8; CHUNK];
    let mut first = true;
    while left > 0 {
        let want = usize::try_from(left.min(CHUNK_BYTES)).unwrap_or(CHUNK);
        let chunk = buffer
            .get_mut(..want)
            .ok_or(BulkloadRefusal::ContractSelfInconsistent)?;
        reader
            .read_exact(chunk)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::UnexpectedEof => BulkloadRefusal::SourceChangedAfterSnapshot,
                _ => crate::refuse::io(&error, "reseat::place::copy_read"),
            })?;
        counters::add_len(Counter::ReseatLandingRead, want);
        stream.feed(chunk);
        if let Some(scanner) = scanner.as_deref_mut() {
            if !scanner.complete() {
                scanner.feed(chunk);
            }
        }
        to.write_all(chunk).refuse_at("reseat::place::copy_write")?;
        counters::add_len(Counter::ReseatDestWrite, want);
        left = left.saturating_sub(u64::try_from(want).unwrap_or(CHUNK_BYTES));
        if mid && first {
            fault_point!(ReseatMidExtend);
        }
        first = false;
    }
    Ok(())
}

/// Whether the first `length` bytes of `a` and `b` are equal, read from
/// offset 0 (`a` landed, `b` destination), and the digest of them.
fn same_prefix(
    landing: &File,
    dest: &File,
    length: u64,
    mut scanner: Option<&mut FieldScanner>,
) -> Result<Option<Stream>> {
    let mut stream = Stream::new(true, Vec::new(), Vec::new());
    let mut a = vec![0_u8; CHUNK];
    let mut b = vec![0_u8; CHUNK];
    let mut at = 0_u64;
    while at < length {
        let want = usize::try_from((length - at).min(CHUNK_BYTES)).unwrap_or(CHUNK);
        let (left, right) = (
            a.get_mut(..want)
                .ok_or(BulkloadRefusal::ContractSelfInconsistent)?,
            b.get_mut(..want)
                .ok_or(BulkloadRefusal::ContractSelfInconsistent)?,
        );
        landing
            .read_exact_at(left, at)
            .refuse_at("reseat::place::same_prefix_landing")?;
        counters::add_len(Counter::ReseatLandingRead, want);
        dest.read_exact_at(right, at)
            .refuse_at("reseat::place::same_prefix_dest")?;
        counters::add_len(Counter::ReseatDestRead, want);
        if let Some(scanner) = scanner.as_deref_mut() {
            if !scanner.complete() {
                scanner.feed(left);
            }
        }
        if left != right {
            return Ok(None);
        }
        stream.feed(left);
        at = at.saturating_add(u64::try_from(want).unwrap_or(CHUNK_BYTES));
    }
    Ok(Some(stream))
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
    /// reads (a transcript's `slug`).
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
        match lstat(landing)? {
            Some(meta) if meta.is_file() => {}
            _ => return done(Outcome::LandingNotRegular),
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
        };
        let mut row = self.ledger.placement(&dest)?;
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
            head: None,
            tail: None,
            digest: None,
            intent: None,
        }
    }

    fn temp(&self) -> PathBuf {
        self.dest.with_file_name(temp_name(self.name))
    }

    /// Remove a temporary a crashed pass left before it published (no
    /// intent names it, so it is a staged copy of a landed file).
    fn clear_temp(&self) -> Result<()> {
        let temp = self.temp();
        if lstat(&temp)?.is_some_and(|meta| meta.is_file()) {
            crate::io::sys::unlinkat(
                self.directory,
                &c_name(temp_name(self.name).as_os_str())?,
                false,
            )
            .refuse_at("reseat::place::clear_temp")?;
        }
        Ok(())
    }

    /// Stage a full copy of the landed file in the temporary: written, its
    /// mode and modification time set, sealed.
    fn stage(&self, landing: &Landing) -> Result<(File, Stream, Option<FieldScanner>)> {
        self.clear_temp()?;
        let mut temp = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.temp())
            .map_err(|error| crate::refuse::io_in(&error, self.dest, "reseat::place::stage"))?;
        let mut stream = Stream::new(true, Vec::new(), Vec::new());
        let mut scanner = self.want.map(FieldScanner::new);
        copy(
            &landing.file,
            &mut temp,
            landing.identity.size,
            &mut stream,
            scanner.as_mut(),
            false,
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
        Ok((temp, stream, scanner))
    }

    /// `D` absent: stage, publish without replacing, commit.
    fn fresh(&self, landing: &Landing) -> Result<Placed> {
        let (temp, stream, scanner) = self.stage(landing)?;
        fault_point!(ReseatAfterTempSeal);
        match crate::io::publish_noreplace(
            self.directory,
            &c_name(temp_name(self.name).as_os_str())?,
            &c_name(self.name)?,
        ) {
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
        let placed = Identity::of(&temp.metadata().refuse_at("reseat::place::published")?);
        let (digest, head, tail) = stream.finish();
        let mut row = self.row(State::Ours, landing.identity, placed, landing.identity.size);
        row.digest = digest;
        row.head = Some(head);
        row.tail = Some(tail);
        self.placer.ledger.put(&row)?;
        Ok(Placed {
            outcome: Outcome::Placed,
            scanner,
        })
    }

    /// `D` present with no owned row: adopt, extend a prefix, or keep.
    fn existing(&self, landing: &Landing, meta: &std::fs::Metadata) -> Result<Placed> {
        let observed = Identity::of(meta);
        let mut scanner = self.want.map(FieldScanner::new);
        let keep = |state: State, outcome: Outcome, scanner| -> Result<Placed> {
            self.placer
                .ledger
                .put(&self.row(state, landing.identity, observed, 0))?;
            Ok(Placed { outcome, scanner })
        };
        if !meta.is_file() {
            return keep(State::Occupied, Outcome::Occupied, None);
        }
        let dest = crate::hash::open_nofollow(self.dest)?;
        if observed.size > landing.identity.size {
            return match same_prefix(
                &landing.file,
                &dest,
                landing.identity.size,
                scanner.as_mut(),
            )? {
                Some(_) => keep(State::Continued, Outcome::DestAhead, scanner),
                None => keep(State::Diverged, Outcome::Diverged, scanner),
            };
        }
        let Some(stream) = same_prefix(&landing.file, &dest, observed.size, scanner.as_mut())?
        else {
            return keep(State::Diverged, Outcome::Diverged, scanner);
        };
        if observed.size < landing.identity.size && !appendable(self.dest) {
            return keep(State::Diverged, Outcome::Diverged, scanner);
        }
        let (digest, head, tail) = stream.finish();
        let mut row = self.row(State::Adopted, landing.identity, observed, observed.size);
        row.head = Some(head);
        row.tail = Some(tail);
        if observed.size == landing.identity.size {
            row.digest = digest;
            self.placer.ledger.put(&row)?;
            return Ok(Placed {
                outcome: Outcome::Adopted,
                scanner,
            });
        }
        // A strict prefix: adopted at its size, then extended.
        row.landed = Identity::default();
        self.placer.ledger.put(&row)?;
        let read = |offset, length| landing.read_at(offset, length);
        let tail_bytes = windows(&read, observed.size)?.2;
        self.extend(row, landing, tail_bytes, scanner)
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
        if !appendable(self.dest) {
            return self.supersede(row, landing);
        }
        let size = landing.identity.size;
        if size < row.placed {
            return self.rewritten(row, landing);
        }
        let read = |offset, length| landing.read_at(offset, length);
        let (head, tail, tail_bytes) = windows(&read, row.placed)?;
        if row.placed > 0 && (Some(head) != row.head || Some(tail) != row.tail) {
            return self.rewritten(row, landing);
        }
        if size == row.placed {
            // Same length, new stamp: equal bytes keep the row, others are
            // a rewrite (never overwritten).
            let dest = crate::hash::open_nofollow(self.dest)?;
            if same_prefix(&landing.file, &dest, size, None)?.is_none() {
                return self.rewritten(row, landing);
            }
            self.placer.ledger.put(&Placement {
                landed: landing.identity,
                ..row
            })?;
            return done(Outcome::Unchanged);
        }
        self.extend(row, landing, tail_bytes, None)
    }

    fn rewritten(&self, row: Placement, landing: &Landing) -> Result<Placed> {
        self.placer.ledger.put(&Placement {
            state: State::Rewritten,
            landed: landing.identity,
            ..row
        })?;
        Ok(Placed {
            outcome: Outcome::Rewritten,
            scanner: None,
        })
    }

    /// Append the landed bytes past `row.placed` (an owned row whose
    /// destination identity was just checked), under an intent.
    /// `tail_bytes`: the last window of the placed bytes, already proven.
    fn extend(
        &self,
        row: Placement,
        landing: &Landing,
        tail_bytes: Vec<u8>,
        scanner: Option<FieldScanner>,
    ) -> Result<Placed> {
        let target = landing.identity.size;
        let mut intent = row.clone();
        intent.intent = Some(Intent::Extend(target));
        self.placer.ledger.put(&intent)?;
        fault_point!(ReseatAfterExtendIntent);
        let mut dest = OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.dest)
            .refuse_at("reseat::place::extend_open")?;
        if Identity::of(&dest.metadata().refuse_at("reseat::place::extend_stat")?)
            != row.placed_identity
        {
            return self.hand_over(row, landing);
        }
        let head = if row.placed < WINDOW {
            landing.read_at(0, row.placed)?
        } else {
            Vec::new()
        };
        let full_head = row.placed >= WINDOW;
        let mut stream = Stream::new(false, head, tail_bytes);
        let mut scanner = match (scanner, self.want, stream.tail.last()) {
            (Some(scanner), _, _) => Some(scanner),
            (None, Some(want), Some(b'\n') | None) => Some(FieldScanner::new(want)),
            (None, Some(want), Some(_)) => Some(FieldScanner::mid_line(want)),
            (None, None, _) => None,
        };
        std::io::Seek::seek(&mut &landing.file, std::io::SeekFrom::Start(row.placed))
            .refuse_at("reseat::place::extend_seek")?;
        copy(
            &landing.file,
            &mut dest,
            target - row.placed,
            &mut stream,
            scanner.as_mut(),
            true,
        )?;
        dest.set_modified(
            landing
                .meta
                .modified()
                .refuse_at("reseat::place::extend_mtime")?,
        )
        .refuse_at("reseat::place::extend_mtime")?;
        crate::io::durable::seal_file(&dest).refuse_at("reseat::place::extend_seal")?;
        fault_point!(ReseatAfterExtend);
        let placed = Identity::of(&dest.metadata().refuse_at("reseat::place::extended")?);
        if placed.size != target {
            // Another writer appended beside this pass: nothing of theirs
            // was overwritten (O_APPEND); the destination owns the file.
            return self.hand_over(row, landing);
        }
        let (_, head_digest, tail_digest) = stream.finish();
        let mut done = self.row(State::Ours, landing.identity, placed, target);
        done.head = if full_head {
            row.head
        } else {
            Some(head_digest)
        };
        done.tail = Some(tail_digest);
        self.placer.ledger.put(&done)?;
        Ok(Placed {
            outcome: Outcome::Extended,
            scanner,
        })
    }

    fn hand_over(&self, row: Placement, landing: &Landing) -> Result<Placed> {
        let observed = lstat(self.dest)?.map_or(row.placed_identity, |meta| Identity::of(&meta));
        let both = landing.identity != row.landed;
        self.placer.ledger.put(&Placement {
            state: State::Continued,
            placed_identity: observed,
            intent: None,
            ..row
        })?;
        Ok(Placed {
            outcome: Outcome::Continued { both },
            scanner: None,
        })
    }

    /// Exchange a changed non-append landed file in for this ledger's
    /// placement, under an intent, and check what it displaced.
    fn supersede(&self, row: Placement, landing: &Landing) -> Result<Placed> {
        let (temp, stream, scanner) = self.stage(landing)?;
        let staged = Identity::of(&temp.metadata().refuse_at("reseat::place::staged")?);
        let mut intent = row.clone();
        intent.intent = Some(Intent::Supersede(staged.ino));
        self.placer.ledger.put(&intent)?;
        let (temp_c, name_c) = (
            c_name(temp_name(self.name).as_os_str())?,
            c_name(self.name)?,
        );
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
        if !self.settle_displaced(&row, &temp_c, &name_c)? {
            return self.hand_over(row, landing);
        }
        let (digest, head, tail) = stream.finish();
        let mut done = self.row(State::Ours, landing.identity, staged, landing.identity.size);
        done.placed_identity =
            Identity::of(&lstat(self.dest)?.ok_or(BulkloadRefusal::SourceChangedAfterSnapshot)?);
        done.digest = digest;
        done.head = Some(head);
        done.tail = Some(tail);
        self.placer.ledger.put(&done)?;
        Ok(Placed {
            outcome: Outcome::Superseded,
            scanner,
        })
    }

    /// After an exchange: the file displaced to the temporary name is this
    /// ledger's placement (remove it and return `true`), or not (exchange
    /// it back, remove the staged copy, return `false`).
    fn settle_displaced(
        &self,
        row: &Placement,
        temp_c: &CString,
        name_c: &CString,
    ) -> Result<bool> {
        let displaced = lstat(&self.temp())?;
        let ours = displaced
            .as_ref()
            .is_some_and(|meta| Identity::of(meta) == row.placed_identity);
        if displaced.is_some() && !ours {
            crate::io::exchange(self.directory, temp_c, name_c)
                .refuse_at("reseat::place::exchange_back")?;
        }
        if displaced.is_some() {
            crate::io::sys::unlinkat(self.directory, temp_c, false)
                .refuse_at("reseat::place::displaced")?;
        }
        crate::io::durable::seal_dir(self.directory).refuse_at("reseat::place::supersede_seal")?;
        Ok(ours || displaced.is_none())
    }

    /// Finish or undo what a crash left under an intent; returns the row
    /// the pass continues from.
    fn settle_intent(&self, row: Placement) -> Result<Option<Placement>> {
        let observed = lstat(self.dest)?;
        match row.intent {
            None => Ok(Some(row)),
            Some(Intent::Supersede(staged)) => {
                let (temp_c, name_c) = (
                    c_name(temp_name(self.name).as_os_str())?,
                    c_name(self.name)?,
                );
                let exchanged = observed.as_ref().is_some_and(|meta| meta.ino() == staged);
                if exchanged {
                    if !self.settle_displaced(&row, &temp_c, &name_c)? {
                        let landing = Landing::open(self.landing_path)?;
                        self.hand_over(row, &landing)?;
                        return self.placer.ledger.placement(self.dest);
                    }
                    // The staged bytes are placed; the landing may have
                    // moved since, so the pass decides again from them.
                    let placed = Identity::of(
                        &lstat(self.dest)?.ok_or(BulkloadRefusal::SourceChangedAfterSnapshot)?,
                    );
                    let next = Placement {
                        landed: Identity::default(),
                        placed_identity: placed,
                        intent: None,
                        ..row
                    };
                    self.placer.ledger.put(&next)?;
                    return Ok(Some(next));
                }
                // Not exchanged: the temporary is the staged copy.
                self.clear_temp()?;
                let next = Placement {
                    intent: None,
                    ..row
                };
                self.placer.ledger.put(&next)?;
                Ok(Some(next))
            }
            Some(Intent::Extend(target)) => {
                let landing = Landing::open(self.landing_path)?;
                let Some(meta) = observed else {
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
                    && meta.dev() == row.placed_identity.dev;
                let read = |offset, length| landing.read_at(offset, length);
                let proven = same_file
                    && (row.placed..=target).contains(&size)
                    && size <= landing.identity.size
                    && (row.placed == 0 || {
                        let (head, tail, _) = windows(&read, row.placed)?;
                        Some(head) == row.head && Some(tail) == row.tail
                    })
                    && {
                        let dest = crate::hash::open_nofollow(self.dest)?;
                        let mut a = vec![0_u8; usize::try_from(size - row.placed).unwrap_or(0)];
                        dest.read_exact_at(&mut a, row.placed)
                            .refuse_at("reseat::place::settle_dest")?;
                        counters::add_len(Counter::ReseatDestRead, a.len());
                        a == landing.read_at(row.placed, size - row.placed)?
                    };
                if !proven {
                    self.hand_over(row, &landing)?;
                    return self.placer.ledger.placement(self.dest);
                }
                // The appended part is the landing's: continue from it.
                let (head, tail, _) = windows(&read, size)?;
                let next = Placement {
                    placed: size,
                    placed_identity: Identity::of(&meta),
                    landed: if size == landing.identity.size {
                        landing.identity
                    } else {
                        Identity::default()
                    },
                    head: Some(head),
                    tail: Some(tail),
                    digest: None,
                    intent: None,
                    state: State::Ours,
                    ..row
                };
                self.placer.ledger.put(&next)?;
                Ok(Some(next))
            }
        }
    }
}
