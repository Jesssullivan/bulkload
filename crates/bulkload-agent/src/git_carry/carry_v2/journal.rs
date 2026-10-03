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

    /// Short names for this state dir: the first 16 hex digits of BLAKE3
    /// over its identity token. The current one keys the quarantine, so two
    /// state dirs ingesting one plan never share (or adopt) a quarantine
    /// (#75 r2 N4).
    ///
    /// #82 (R25 / R-N58): the token is 32 random bytes created once in
    /// `git-carry-v2/quarantine-key` ([`state_binding`]), never the state
    /// dir's path, so a state dir renamed or remounted elsewhere keeps its
    /// key and resumes its own quarantine instead of re-sending every
    /// segment. A state dir lost and recreated gets a new token, so it never
    /// adopts the lost one's quarantine.
    ///
    /// #120: the token is bound to the inode numbers of the state dir and of
    /// the token file. A copy of the state dir (token included) has other
    /// inodes, so its first open mints a token of its own and records the
    /// one it was copied with as its predecessor: the copy and the original
    /// never name (or discard) each other's quarantine, however their
    /// sessions are ordered. The predecessor's key lets a copied in-flight
    /// journal refuse instead of re-sending what the original's quarantine
    /// durably holds ([`StateKeys::predecessor`]).
    ///
    /// # Errors
    /// As [`state_binding`].
    pub(super) fn keys(&self) -> crate::Result<StateKeys> {
        let carry = private_subdirectory(self.state.directory(), "git-carry-v2", true)?
            .ok_or(BulkloadRefusal::Io(None))?;
        seal_dir(self.state.directory())?;
        let binding = state_binding(self.state.directory(), &carry)?;
        Ok(StateKeys {
            current: quarantine_key(&binding.token),
            predecessor: binding.predecessor.as_ref().map(quarantine_key),
        })
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

/// The quarantine keys [`JournalStore::keys`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StateKeys {
    /// This state dir's key: it names the quarantine.
    pub(super) current: String,
    /// #120: the key of the state dir this one was copied from (or moved
    /// from across file systems, which is a copy), when its binding records
    /// one. That quarantine is never this state dir's to adopt, sweep or
    /// discard.
    pub(super) predecessor: Option<String>,
}

/// The first 16 hex digits of BLAKE3 over a domain tag and `token`.
fn quarantine_key(token: &[u8; TOKEN_BYTES]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload git-carry-v2 quarantine key\0");
    hasher.update(token);
    hasher
        .finalize()
        .to_hex()
        .get(..16)
        .unwrap_or_default()
        .to_owned()
}

/// Bytes in the state dir's identity token.
const TOKEN_BYTES: usize = 32;

/// Bytes in a binding record (#120): the token, the predecessor's token
/// (zeros for none), the state dir's inode number, the token file's inode
/// number (both little-endian `u64`), and the first 8 bytes of a BLAKE3
/// check over the rest.
const BINDING_BYTES: usize = 2 * TOKEN_BYTES + 3 * 8;

/// A state dir's identity token and the token it was copied with, if any.
struct Binding {
    token: [u8; TOKEN_BYTES],
    predecessor: Option<[u8; TOKEN_BYTES]>,
}

/// The inode numbers a binding records: the state dir's and the token
/// file's.
///
/// #120: these, and not the device number, decide whether a token is the
/// state dir's own. Both survive a rename within a file system, a remount
/// and a reboot on Linux and Darwin. The device number does not: Darwin's
/// `st_dev` is assigned at mount and can change across reboots, and on
/// Linux it can follow device-mapper or probe order, so a binding that
/// checked it would read a reboot as a copy. A file-level copy (`cp -a`,
/// rsync, tar, Finder, a clonefile, a restore from a file backup, a move
/// across file systems) makes new inodes for both and so is caught.
/// A block-level clone (a disk image, a file system snapshot) keeps inode
/// numbers and is not distinguished; it clones a destination on the same
/// volume with it, so its quarantines live in another repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bound {
    state: u64,
    file: u64,
}

impl Bound {
    fn of(state: &File, file: &File) -> crate::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        Ok(Self {
            state: state.metadata()?.ino(),
            file: file.metadata()?.ino(),
        })
    }
}

fn binding_check(body: &[u8]) -> [u8; 8] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bulkload git-carry-v2 quarantine binding\0");
    hasher.update(body);
    let mut check = [0_u8; 8];
    check.copy_from_slice(hasher.finalize().as_bytes().get(..8).unwrap_or(&[0; 8]));
    check
}

fn encode_binding(binding: &Binding, bound: Bound) -> [u8; BINDING_BYTES] {
    let mut record = [0_u8; BINDING_BYTES];
    let (body, check) = record.split_at_mut(BINDING_BYTES - 8);
    let (token, rest) = body.split_at_mut(TOKEN_BYTES);
    token.copy_from_slice(&binding.token);
    let (predecessor, rest) = rest.split_at_mut(TOKEN_BYTES);
    predecessor.copy_from_slice(&binding.predecessor.unwrap_or([0; TOKEN_BYTES]));
    let (state, file) = rest.split_at_mut(8);
    state.copy_from_slice(&bound.state.to_le_bytes());
    file.copy_from_slice(&bound.file.to_le_bytes());
    check.copy_from_slice(&binding_check(body));
    record
}

/// A binding record and the inodes it is bound to; `None` when its check
/// fails.
fn decode_binding(record: &[u8]) -> Option<(Binding, Bound)> {
    if record.len() != BINDING_BYTES {
        return None;
    }
    let (body, check) = record.split_at(BINDING_BYTES - 8);
    if binding_check(body) != check {
        return None;
    }
    let token: [u8; TOKEN_BYTES] = body.get(..TOKEN_BYTES)?.try_into().ok()?;
    let predecessor: [u8; TOKEN_BYTES] = body.get(TOKEN_BYTES..2 * TOKEN_BYTES)?.try_into().ok()?;
    let state = u64::from_le_bytes(
        body.get(2 * TOKEN_BYTES..2 * TOKEN_BYTES + 8)?
            .try_into()
            .ok()?,
    );
    let file = u64::from_le_bytes(body.get(2 * TOKEN_BYTES + 8..)?.try_into().ok()?);
    Some((
        Binding {
            token,
            predecessor: (predecessor != [0; TOKEN_BYTES]).then_some(predecessor),
        },
        Bound { state, file },
    ))
}

fn random_token() -> crate::Result<[u8; TOKEN_BYTES]> {
    let mut token = [0_u8; TOKEN_BYTES];
    File::open("/dev/urandom")?.read_exact(&mut token)?;
    Ok(token)
}

/// The state dir's identity token (#82) and its binding (#120):
/// `quarantine-key` in `carry` (`git-carry-v2/`), created 0600 on first use
/// and read back after the private-file checks every time after. It holds
/// one [`BINDING_BYTES`] record.
///
/// - Shorter than a token: its creator died between the create and the
///   sealed write, so it was never handed to anyone; a fresh token is bound.
/// - Exactly a token: a #82 token from before #120 (none is deployed:
///   R-N56), bound in place to this state dir, keeping its token.
/// - A record bound to this state dir's inodes: its token.
/// - A record bound to other inodes: this state dir is a copy. A fresh token
///   is bound, with the record's token as its predecessor; the original
///   keeps its token, so neither ever discards the other's quarantine.
/// - Anything else (another length, or a record failing its check):
///   `PATH_ESCAPES_ROOT`; no crash leaves it (see below).
///
/// A record is written whole with one `pwrite` at offset 0 and never
/// truncated first, so a crash leaves the old record or the new one: it is
/// far smaller than a sector, and the file only grows (0 or 32 bytes to a
/// record, or a record over a record). It is only ever read or written
/// under an exclusive `flock` on the file, and both the file and `carry`
/// are sealed before the lock is released or the token returned, so no
/// quarantine is ever named by a token that a power loss could take back.
///
/// # Errors
/// `JOURNAL_OWNERSHIP_CONFLICT` when the lock stays held, `PATH_ESCAPES_ROOT`
/// for a file that fails the private-file checks or holds no valid record,
/// and I/O failures.
fn state_binding(state: &File, carry: &File) -> crate::Result<Binding> {
    use std::os::unix::fs::FileExt as _;
    const ATTEMPTS: u32 = 50;
    let name = cstring(b"quarantine-key")?;
    for _ in 0..ATTEMPTS {
        // Never `O_APPEND`: a record is written at offset 0, and Linux
        // appends a `pwrite` to a file opened for appending.
        let file = match open_rw_flags(carry, &name, 0) {
            Ok(file) => file,
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => match create_private(carry, &name) {
                Ok(file) => file,
                // Another opener created it first: open theirs.
                Err(BulkloadRefusal::Io(Some(libc::EEXIST))) => continue,
                Err(error) => return Err(error),
            },
            Err(error) => return Err(error),
        };
        private_file(&file)?;
        if !lock_exclusive(&file)? {
            return Err(BulkloadRefusal::JournalOwnershipConflict);
        }
        let mut held = Vec::with_capacity(BINDING_BYTES + 1);
        (&file)
            .take(u64::try_from(BINDING_BYTES + 1).map_err(|_| BulkloadRefusal::BudgetExceeded)?)
            .read_to_end(&mut held)?;
        let bound = Bound::of(state, &file)?;
        let (binding, write) = match held.len() {
            length if length < TOKEN_BYTES => (
                Binding {
                    token: random_token()?,
                    predecessor: None,
                },
                true,
            ),
            TOKEN_BYTES => {
                let mut token = [0_u8; TOKEN_BYTES];
                token.copy_from_slice(&held);
                (
                    Binding {
                        token,
                        predecessor: None,
                    },
                    true,
                )
            }
            BINDING_BYTES => {
                let (binding, recorded) =
                    decode_binding(&held).ok_or(BulkloadRefusal::PathEscapesRoot)?;
                if recorded == bound {
                    (binding, false)
                } else {
                    (
                        Binding {
                            token: random_token()?,
                            predecessor: Some(binding.token),
                        },
                        true,
                    )
                }
            }
            _ => return Err(BulkloadRefusal::PathEscapesRoot),
        };
        if write {
            file.write_all_at(&encode_binding(&binding, bound), 0)?;
        }
        // Sealed by whoever reads it, too: a creator that died after its
        // write and before its seal left the bytes only in the page cache.
        seal_file(&file)?;
        seal_dir(carry)?;
        return Ok(binding);
    }
    Err(BulkloadRefusal::JournalOwnershipConflict)
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
    /// No journal, or one with no sealed plan or abandoned (replaced by an
    /// empty one). Either way any quarantine under this state dir's name is
    /// garbage, and the session discards it (#75 r2 N2, r3 M2).
    Fresh(Journal),
    /// A journal with a sealed plan, and every record after it.
    Existing(Journal, Vec<Record>),
}

impl Journal {
    /// Open (creating when absent) and lock `<pack_id>.journal`. A journal
    /// whose plan block never sealed, or that was abandoned, is truncated in
    /// place (never unlinked and recreated: #75 r4 N1) and returned as
    /// [`Found::Fresh`]; a torn final line is cut off.
    ///
    /// #75 r4 N1: the lock is proven exclusive before anything is read. After
    /// `flock`, the locked descriptor must still be the file at the name
    /// (same device and inode); one whose name was removed or now names
    /// another file is dropped and the open retried. So two openers never
    /// both believe they own a journal, and a loser refuses
    /// `JOURNAL_OWNERSHIP_CONFLICT`, never a raw I/O error.
    ///
    /// # Errors
    /// `JOURNAL_OWNERSHIP_CONFLICT` when another session holds the lock (or
    /// the name keeps changing under the open), `SCHEMA_MISMATCH` for a
    /// complete line that does not parse or check, the private-file checks,
    /// and any I/O failure.
    pub(super) fn open(store: &JournalStore, pack_id: &str) -> crate::Result<Found> {
        // #75 r1 B2: the name is built from `pack_id`, so only a plan id
        // (64 lowercase hex digits) may name a journal.
        if !super::ingest::pack_id_ok(pack_id) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        let directory = store.directory(true)?.ok_or(BulkloadRefusal::Io(None))?;
        let name = cstring(format!("{pack_id}.journal").as_bytes())?;
        let journal = Self::claim(directory, name)?;
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
        // recorded is still in force. Empty it in place, under the lock.
        journal.file.set_len(0)?;
        seal_file(&journal.file)?;
        Ok(Found::Fresh(journal))
    }

    /// Open or create the file at `name` and lock it, until the locked
    /// descriptor is the file the name holds.
    fn claim(directory: File, name: std::ffi::CString) -> crate::Result<Self> {
        const ATTEMPTS: u32 = 50;
        for _ in 0..ATTEMPTS {
            let file = match open_rw(&directory, &name) {
                Ok(file) => file,
                Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => {
                    match create_private(&directory, &name) {
                        Ok(file) => {
                            seal_dir(&directory)?;
                            file
                        }
                        // Another opener created it first: open theirs.
                        Err(BulkloadRefusal::Io(Some(libc::EEXIST))) => continue,
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            };
            private_file(&file)?;
            if !lock_exclusive(&file)? {
                return Err(BulkloadRefusal::JournalOwnershipConflict);
            }
            if names(&directory, &name, &file)? {
                return Ok(Self {
                    file,
                    directory,
                    name,
                });
            }
            // The name was removed or replaced while this open waited: the
            // lock is on a file no session will use. Drop it and retry.
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
    ///
    /// #75 r4 N1: only while the name still holds this session's file; one
    /// that names another file is left alone.
    pub(super) fn remove(&self) -> crate::Result<()> {
        if !self.owned()? {
            return Ok(());
        }
        // SAFETY: the name is NUL-terminated and relative to the open
        // journal directory.
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        seal_dir(&self.directory)?;
        Ok(())
    }

    /// Whether the name still holds this session's file (#75 r4 N1).
    pub(super) fn owned(&self) -> crate::Result<bool> {
        names(&self.directory, &self.name, &self.file)
    }
}

/// Take an exclusive `flock` on `file`, retrying for about two seconds: a
/// child that another thread of this process spawns shares every open file
/// description until its `exec` closes the close-on-exec ones, so a lock
/// just released can look held for that instant. `false` when it stayed
/// held for the whole window.
pub(super) fn lock_exclusive(file: &File) -> crate::Result<bool> {
    const ATTEMPTS: u32 = 100;
    for attempt in 1..=ATTEMPTS {
        // SAFETY: `file` is open for the duration of the call; flock takes
        // no pointers.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EWOULDBLOCK | libc::EINTR) => {}
            _ => return Err(error.into()),
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Ok(false)
}

/// Whether `name` in `directory` (not followed) is `file`: same device and
/// inode. `false` when the name is gone.
fn names(directory: &File, name: &std::ffi::CString, file: &File) -> crate::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let held = file.metadata()?;
    let mut at = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `directory` is open, `name` NUL-terminated, and `at` is valid
    // writable storage for one `stat`.
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            at.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(false);
        }
        return Err(error.into());
    }
    // SAFETY: `fstatat` succeeded, so it filled `at`.
    let at = unsafe { at.assume_init() };
    // `dev_t` is `i32` on Darwin and `u64` on Linux; a device number is
    // never negative, so the cast loses nothing.
    #[allow(
        clippy::useless_conversion,
        clippy::unnecessary_cast,
        clippy::cast_sign_loss
    )]
    let same = at.st_dev as u64 == held.dev() && at.st_ino as u64 == held.ino();
    Ok(same)
}

/// Open an existing journal for reading and appending, never following a
/// symlink at its name.
fn open_rw(directory: &File, name: &std::ffi::CString) -> crate::Result<File> {
    open_rw_flags(directory, name, libc::O_APPEND)
}

/// Open an existing file for reading and writing with `extra` flags, never
/// following a symlink at its name.
fn open_rw_flags(
    directory: &File,
    name: &std::ffi::CString,
    extra: libc::c_int,
) -> crate::Result<File> {
    // SAFETY: `directory` is open and `name` NUL-terminated; no create flag,
    // so no mode argument. O_NONBLOCK keeps a planted FIFO from blocking.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | extra | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
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

    fn private_dir(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn key_of(state: &Path) -> String {
        JournalStore::open(state).unwrap().keys().unwrap().current
    }

    /// #82: the quarantine key follows the state dir's identity token, not
    /// its path. It is stable across opens, survives a rename, differs
    /// between state dirs (and for a state dir recreated at the same path),
    /// and a token file its creator never finished is written afresh; one
    /// that is neither a token nor a binding record refuses.
    #[test]
    fn the_quarantine_key_follows_the_state_dirs_token_not_its_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tempfile::tempdir().unwrap();
        let state = scratch.path().join("state");
        private_dir(&state);
        let keys = JournalStore::open(&state).unwrap().keys().unwrap();
        let key = keys.current.clone();
        assert_eq!(key.len(), 16);
        assert_eq!(keys.predecessor, None);
        assert_eq!(key_of(&state), key);
        let token = state.join("git-carry-v2/quarantine-key");
        let metadata = std::fs::metadata(&token).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(metadata.len(), 88);

        let moved = scratch.path().join("moved");
        std::fs::rename(&state, &moved).unwrap();
        let after = JournalStore::open(&moved).unwrap().keys().unwrap();
        assert_eq!(after.current, key, "a rename keeps the key");
        assert_eq!(after.predecessor, None, "a rename is not a copy");

        private_dir(&state);
        let recreated = key_of(&state);
        assert_ne!(recreated, key, "a recreated state dir is a new one");

        // A creator that died between the create and the write.
        let torn = state.join("git-carry-v2/quarantine-key");
        std::fs::write(&torn, b"short").unwrap();
        let rewritten = key_of(&state);
        assert_eq!(std::fs::metadata(&torn).unwrap().len(), 88);
        assert_eq!(key_of(&state), rewritten);

        for length in [33, 87, 89] {
            std::fs::write(&torn, vec![7_u8; length]).unwrap();
            assert_eq!(
                JournalStore::open(&state).unwrap().keys(),
                Err(BulkloadRefusal::PathEscapesRoot),
                "{length}"
            );
        }
    }

    /// #120: a byte-for-byte copy of a state dir (its token included) gets a
    /// token of its own on its first open and names the original's as its
    /// predecessor; the original keeps its key, and both are stable after.
    /// A #82 token from before the binding is bound in place; a record that
    /// fails its check refuses.
    #[test]
    fn issue120_a_copied_state_dir_mints_its_own_key() {
        let scratch = tempfile::tempdir().unwrap();
        let state = scratch.path().join("state");
        private_dir(&state);
        let original = key_of(&state);
        let copy = scratch.path().join("copy");
        let status = std::process::Command::new("cp")
            .arg("-a")
            .arg(&state)
            .arg(&copy)
            .status()
            .unwrap();
        assert!(status.success());
        let token = |dir: &Path| std::fs::read(dir.join("git-carry-v2/quarantine-key")).unwrap();
        assert_eq!(token(&copy), token(&state), "a byte-for-byte copy");

        let copied = JournalStore::open(&copy).unwrap().keys().unwrap();
        assert_ne!(copied.current, original, "the copy mints its own key");
        assert_eq!(copied.predecessor.as_deref(), Some(original.as_str()));
        assert_eq!(
            JournalStore::open(&copy).unwrap().keys().unwrap(),
            copied,
            "stable once bound"
        );
        let kept = JournalStore::open(&state).unwrap().keys().unwrap();
        assert_eq!(kept.current, original, "the original keeps its key");
        assert_eq!(kept.predecessor, None);

        // A #82 token (32 bytes, unbound) is bound in place.
        let legacy = scratch.path().join("legacy");
        private_dir(&legacy);
        let _ = key_of(&legacy);
        let path = legacy.join("git-carry-v2/quarantine-key");
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..32]).unwrap();
        let mut token32 = [0_u8; TOKEN_BYTES];
        token32.copy_from_slice(&bytes[..32]);
        assert_eq!(key_of(&legacy), quarantine_key(&token32));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 88);

        let mut bad = std::fs::read(&path).unwrap();
        bad[40] ^= 1;
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(
            JournalStore::open(&legacy).unwrap().keys(),
            Err(BulkloadRefusal::PathEscapesRoot)
        );
    }

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
