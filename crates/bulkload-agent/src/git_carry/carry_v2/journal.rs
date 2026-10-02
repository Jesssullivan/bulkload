//! The destination's ingest journal: `<state>/git-carry-v2/ingest/<pack_id>.journal`.
//!
//! An append-only text file of records, one per line, each ending in the first
//! 16 hex digits of its own BLAKE3 so a torn or altered line is recognized.
//! Every append is written in one `write` and sealed before the step it
//! records is taken as done (a record never precedes what it describes). The
//! first block, from `begin` to `planned`, is written and sealed at once;
//! `planned` carries the digest of the block, so a journal without it holds no
//! plan and is discarded. A final line without its newline is a torn append:
//! it is cut off on open, and the step it would have recorded is redone.
//!
//! The file is created `O_CREAT|O_EXCL|O_NOFOLLOW` at 0600 in a private
//! directory, reopened `O_NOFOLLOW` with the private-file checks, and held
//! under an exclusive `flock` for as long as its session is open, so two
//! sessions never interleave records.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::io::{AsRawFd as _, FromRawFd as _};
use std::path::Path;

use super::super::estimate::{
    create_private, cstring, private_file, private_subdirectory, PrivateState,
};
use super::is_oid;
use crate::durable::{seal_dir, seal_file};
use crate::BulkloadRefusal;

/// The private state directory ingest journals live in. Opening it creates
/// nothing.
#[derive(Debug)]
pub struct JournalStore {
    state: PrivateState,
}

impl JournalStore {
    /// Open `state_dir` (it must exist and be private).
    ///
    /// # Errors
    /// Refuses a state dir or an existing `git-carry-v2/ingest/` that is not
    /// private, or any I/O failure.
    pub fn open(state_dir: &Path) -> crate::Result<Self> {
        let state = PrivateState::open(state_dir)?;
        if let Some(carry) = private_subdirectory(state.directory(), "git-carry-v2", false)? {
            private_subdirectory(&carry, "ingest", false)?;
        }
        Ok(Self { state })
    }

    /// Whether the state dir lies at or under `path`, by directory identity.
    #[must_use]
    pub fn is_inside(&self, path: &Path) -> bool {
        self.state.is_inside(path)
    }

    /// A short name for this state dir: the first 16 hex digits of BLAKE3
    /// over its canonical spelling. It keys the quarantine, so two state dirs
    /// ingesting one plan never share (or adopt) a quarantine (#75 r2 N4).
    pub(super) fn key(&self) -> String {
        use std::os::unix::ffi::OsStrExt as _;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"bulkload git-carry-v2 quarantine key\0");
        hasher.update(self.state.root().as_os_str().as_bytes());
        hasher
            .finalize()
            .to_hex()
            .get(..16)
            .unwrap_or_default()
            .to_owned()
    }

    /// `git-carry-v2/ingest/`, created (0700, sealed) when `create` is set.
    fn directory(&self, create: bool) -> crate::Result<Option<File>> {
        let Some(carry) = private_subdirectory(self.state.directory(), "git-carry-v2", create)?
        else {
            return Ok(None);
        };
        let ingest = private_subdirectory(&carry, "ingest", create)?;
        if create {
            seal_dir(&carry)?;
            seal_dir(self.state.directory())?;
        }
        Ok(ingest)
    }
}

/// One journal record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Record {
    /// `begin <pack_id> <segments> <git-dir hex>`
    Begin {
        pack_id: String,
        segments: usize,
        git_dir: Vec<u8>,
    },
    /// `have <oid>`
    Have(String),
    /// `shallow <oid>`: one line of the negotiated shallow frontier (#75 r2
    /// N1).
    Shallow(String),
    /// `ref <name hex> <oid>`
    Ref(Vec<u8>, String),
    /// `planned <digest of the block so far>`
    Planned(String),
    /// `segment <k> <pack hash> <bytes> <blake3>`
    Segment {
        index: usize,
        pack: String,
        bytes: u64,
        blake3: String,
    },
    /// `connected`
    Connected,
    /// `migrated`
    Migrated,
    /// `publishing <carry refs before>`: the digest taken before the ref
    /// transaction, journaled before it runs (#75 r2 N3), so a resume
    /// compares against the state before any publication attempt.
    Publishing(String),
    /// `published <carry refs before> <carry refs after>`
    Published(String, String),
    /// `done`
    Done,
    /// `abandoned <reason>`
    Abandoned(String),
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            let pair = text.get(at..at + 2)?;
            if !pair.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return None;
            }
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

fn digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Record {
    fn body(&self) -> String {
        match self {
            Self::Begin {
                pack_id,
                segments,
                git_dir,
            } => format!("v1 begin {pack_id} {segments} {}", hex(git_dir)),
            Self::Have(oid) => format!("have {oid}"),
            Self::Shallow(oid) => format!("shallow {oid}"),
            Self::Ref(name, oid) => format!("ref {} {oid}", hex(name)),
            Self::Planned(value) => format!("planned {value}"),
            Self::Segment {
                index,
                pack,
                bytes,
                blake3,
            } => format!("segment {index} {pack} {bytes} {blake3}"),
            Self::Connected => "connected".to_owned(),
            Self::Migrated => "migrated".to_owned(),
            Self::Publishing(before) => format!("publishing {before}"),
            Self::Published(before, after) => format!("published {before} {after}"),
            Self::Done => "done".to_owned(),
            Self::Abandoned(reason) => format!("abandoned {reason}"),
        }
    }

    /// The line as written: body, a space, its 16-digit check, newline.
    pub(super) fn line(&self) -> String {
        let body = self.body();
        let check = blake3::hash(body.as_bytes()).to_hex();
        format!("{body} {}\n", check.get(..16).unwrap_or_default())
    }

    fn parse(line: &str) -> crate::Result<Self> {
        let bad = || BulkloadRefusal::SchemaMismatch;
        let (body, check) = line.rsplit_once(' ').ok_or_else(bad)?;
        let expected = blake3::hash(body.as_bytes()).to_hex();
        if Some(check) != expected.get(..16) {
            return Err(bad());
        }
        let fields: Vec<&str> = body.split(' ').collect();
        let record = match fields.as_slice() {
            ["v1", "begin", pack_id, segments, git_dir] if digest(pack_id) => Self::Begin {
                pack_id: (*pack_id).to_owned(),
                segments: segments.parse().map_err(|_| bad())?,
                git_dir: unhex(git_dir).ok_or_else(bad)?,
            },
            ["have", oid] if is_oid(oid.as_bytes()) => Self::Have((*oid).to_owned()),
            ["shallow", oid] if is_oid(oid.as_bytes()) => Self::Shallow((*oid).to_owned()),
            ["ref", name, oid] if is_oid(oid.as_bytes()) => {
                Self::Ref(unhex(name).ok_or_else(bad)?, (*oid).to_owned())
            }
            ["planned", value] if digest(value) => Self::Planned((*value).to_owned()),
            ["segment", index, pack, bytes, blake3]
                if is_oid(pack.as_bytes()) && digest(blake3) =>
            {
                Self::Segment {
                    index: index.parse().map_err(|_| bad())?,
                    pack: (*pack).to_owned(),
                    bytes: bytes.parse().map_err(|_| bad())?,
                    blake3: (*blake3).to_owned(),
                }
            }
            ["connected"] => Self::Connected,
            ["migrated"] => Self::Migrated,
            ["publishing", before] if digest(before) => Self::Publishing((*before).to_owned()),
            ["published", before, after] if digest(before) && digest(after) => {
                Self::Published((*before).to_owned(), (*after).to_owned())
            }
            ["done"] => Self::Done,
            ["abandoned", reason]
                if !reason.is_empty()
                    && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                Self::Abandoned((*reason).to_owned())
            }
            _ => return Err(bad()),
        };
        // Canonical form only: the line must be exactly what `line` writes.
        if record.body() != body {
            return Err(bad());
        }
        Ok(record)
    }
}

/// The digest `planned` carries: BLAKE3 of the block's lines before it.
pub(super) fn plan_digest(block: &[Record]) -> String {
    let mut hasher = blake3::Hasher::new();
    for record in block {
        hasher.update(record.line().as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Whether `line` is a well-formed record that only follows a sealed plan.
fn after_plan(line: &[u8]) -> bool {
    std::str::from_utf8(line)
        .ok()
        .and_then(|line| Record::parse(line).ok())
        .is_some_and(|record| {
            !matches!(
                record,
                Record::Begin { .. }
                    | Record::Have(_)
                    | Record::Shallow(_)
                    | Record::Ref(..)
                    | Record::Planned(_)
            )
        })
}

/// An open, locked journal.
#[derive(Debug)]
pub(super) struct Journal {
    file: File,
    directory: File,
    name: std::ffi::CString,
}

/// What an existing journal held when it was opened.
pub(super) enum Found {
    /// No journal existed.
    Fresh(Journal),
    /// A journal existed with no sealed plan, or abandoned; it was replaced
    /// by an empty one. Whatever quarantine its session made under this
    /// state dir's name is that session's, and is discarded (#75 r2 N2).
    Stale(Journal),
    /// A journal with a sealed plan, and every record after it.
    Existing(Journal, Vec<Record>),
}

impl Journal {
    /// Open (creating when absent) and lock `<pack_id>.journal`. A journal
    /// whose plan block never sealed, or that was abandoned, is removed and
    /// a fresh one created ([`Found::Stale`]); a torn final line is cut off.
    ///
    /// # Errors
    /// `JOURNAL_OWNERSHIP_CONFLICT` when another session holds the lock,
    /// `SCHEMA_MISMATCH` for a complete line that does not parse or check,
    /// the private-file checks, and any I/O failure.
    pub(super) fn open(store: &JournalStore, pack_id: &str) -> crate::Result<Found> {
        // #75 r1 B2: the name is built from `pack_id`, so only a plan id
        // (64 lowercase hex digits) may name a journal.
        if !super::ingest::pack_id_ok(pack_id) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        let directory = store.directory(true)?.ok_or(BulkloadRefusal::Io(None))?;
        let name = cstring(format!("{pack_id}.journal").as_bytes())?;
        let file = match open_rw(&directory, &name) {
            Ok(file) => file,
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => {
                return Self::create(directory, name).map(Found::Fresh);
            }
            Err(error) => return Err(error),
        };
        private_file(&file)?;
        let journal = Self {
            file,
            directory,
            name,
        };
        journal.lock()?;
        let records = journal.read()?;
        let sealed = records
            .iter()
            .any(|record| matches!(record, Record::Planned(_)));
        let abandoned = records
            .iter()
            .any(|record| matches!(record, Record::Abandoned(_)));
        if sealed && !abandoned {
            return Ok(Found::Existing(journal, records));
        }
        // No plan was ever sealed, or the session was abandoned: nothing it
        // recorded is still in force. Remove it and start again.
        journal.remove()?;
        let Self {
            directory, name, ..
        } = journal;
        Self::create(directory, name).map(Found::Stale)
    }

    fn create(directory: File, name: std::ffi::CString) -> crate::Result<Self> {
        let file = create_private(&directory, &name)?;
        seal_dir(&directory)?;
        let journal = Self {
            file,
            directory,
            name,
        };
        journal.lock()?;
        Ok(journal)
    }

    /// Take the exclusive lock, retrying briefly: a child that another
    /// thread of this process spawns shares every open file description
    /// until its `exec` closes the close-on-exec ones, so a lock just
    /// released by a closed session can look held for that instant. A
    /// session that really holds it for the whole window refuses
    /// `JOURNAL_OWNERSHIP_CONFLICT`.
    fn lock(&self) -> crate::Result<()> {
        const ATTEMPTS: u32 = 100;
        for attempt in 1..=ATTEMPTS {
            // SAFETY: the descriptor is open for the life of `self.file`.
            if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(error.into());
            }
            if attempt < ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        Err(BulkloadRefusal::JournalOwnershipConflict)
    }

    /// Every complete record. A torn final line is cut off and sealed: one
    /// without its newline (a prefix tear), or, #75 r1 D1(c), a complete
    /// final line that fails its check (power loss can keep a record's last
    /// page and lose its first); the step it would have recorded is redone.
    /// A bad line before a sealed plan means no plan was ever sealed, so it
    /// reads as no records. Any other bad line refuses `SCHEMA_MISMATCH`.
    fn read(&self) -> crate::Result<Vec<Record>> {
        let mut bytes = Vec::new();
        (&self.file).read_to_end(&mut bytes)?;
        let mut complete = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        let mut records = Vec::new();
        let mut start = 0;
        let body = bytes.get(..complete).unwrap_or_default();
        // `body` ends at a newline (or is empty): split off that last one.
        let lines: Vec<&[u8]> = body
            .strip_suffix(b"\n")
            .map(|body| body.split(|b| *b == b'\n').collect())
            .unwrap_or_default();
        for (index, line) in lines.iter().enumerate() {
            let parsed = std::str::from_utf8(line)
                .map_err(|_| BulkloadRefusal::SchemaMismatch)
                .and_then(Record::parse);
            match parsed {
                Ok(record) => records.push(record),
                Err(_) if index + 1 == lines.len() => {
                    complete = start;
                    break;
                }
                // A hole in a plan block that never sealed: nothing after it
                // was ever written (a block is sealed before anything else is
                // appended), so no plan exists.
                Err(_)
                    if !records.iter().any(|r| matches!(r, Record::Planned(_)))
                        && !lines.iter().skip(index + 1).any(|later| after_plan(later)) =>
                {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            }
            start += line.len() + 1;
        }
        if complete < bytes.len() {
            self.file
                .set_len(u64::try_from(complete).map_err(|_| BulkloadRefusal::BudgetExceeded)?)?;
            seal_file(&self.file)?;
        }
        Ok(records)
    }

    /// Append `records` in one write and seal it.
    pub(super) fn append(&self, records: &[Record]) -> crate::Result<()> {
        let text: String = records.iter().map(Record::line).collect();
        (&self.file).write_all(text.as_bytes())?;
        seal_file(&self.file)?;
        Ok(())
    }

    /// Remove the journal file (a session that never sealed a plan).
    pub(super) fn remove(&self) -> crate::Result<()> {
        // SAFETY: the name is NUL-terminated and relative to the open
        // journal directory.
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        seal_dir(&self.directory)?;
        Ok(())
    }
}

/// Open an existing journal for reading and appending, never following a
/// symlink at its name.
fn open_rw(directory: &File, name: &std::ffi::CString) -> crate::Result<File> {
    // SAFETY: `directory` is open and `name` NUL-terminated; no create flag,
    // so no mode argument. O_NONBLOCK keeps a planted FIFO from blocking.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_APPEND | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_a_changed_byte_is_refused() {
        let records = [
            Record::Begin {
                pack_id: "a".repeat(64),
                segments: 3,
                git_dir: b"/tmp/x y\n.git".to_vec(),
            },
            Record::Have("b".repeat(40)),
            Record::Shallow("9".repeat(40)),
            Record::Ref(b"refs/carry/v1/x".to_vec(), "c".repeat(64)),
            Record::Planned("d".repeat(64)),
            Record::Segment {
                index: 2,
                pack: "e".repeat(40),
                bytes: 99,
                blake3: "f".repeat(64),
            },
            Record::Connected,
            Record::Migrated,
            Record::Publishing("3".repeat(64)),
            Record::Published("1".repeat(64), "2".repeat(64)),
            Record::Done,
            Record::Abandoned("connectivity_missing".to_owned()),
        ];
        for record in &records {
            let line = record.line();
            let body = line.strip_suffix('\n').unwrap();
            assert_eq!(&Record::parse(body).unwrap(), record);
            let mut changed = body.as_bytes().to_vec();
            changed[3] ^= 1;
            let changed = String::from_utf8_lossy(&changed).into_owned();
            assert_eq!(
                Record::parse(&changed),
                Err(BulkloadRefusal::SchemaMismatch)
            );
        }
        assert_eq!(Record::parse("done"), Err(BulkloadRefusal::SchemaMismatch));
        assert_eq!(
            Record::parse("segment 01 x 1 y 0000000000000000"),
            Err(BulkloadRefusal::SchemaMismatch)
        );
    }
}
