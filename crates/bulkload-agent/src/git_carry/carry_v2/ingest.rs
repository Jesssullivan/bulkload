//! Destination ingest: quarantine, connectivity, migration, one ref
//! transaction, all journaled so recovery is idempotent (W6 M1 PR 2).
//!
//! The sequence, each step journaled only once it is durable:
//!
//! 1. **Preflight** (R-N75, spike plan change 7): `rev-list --objects
//!    --missing=print` over the round's haves, which the sender left out of
//!    every segment. Any object their closure lacks refuses before anything
//!    is written: the connectivity check below trusts refs, so without this a
//!    ref with a broken closure could be published.
//! 2. **Begin**: the plan (pack id, segment count, haves, ref updates, the
//!    destination's git dir) is sealed into the journal, then the quarantine
//!    `objects/incoming-bulkload-<pack_id>/` is made (receive-pack's
//!    tmp-objdir pattern) and sealed.
//! 3. **Segments**, in order: `index-pack --stdin --fix-thin --keep` into the
//!    quarantine (`GIT_OBJECT_DIRECTORY` the quarantine, the main store its
//!    alternate, `GIT_QUARANTINE_PATH` set, so Git refuses any ref update).
//!    Each pack's files and the quarantine are sealed, then the segment is
//!    journaled: that is the ack, and `next_segment` is what `GitResume`
//!    carries. A crash in segment k leaves segments before k journaled, and
//!    the sender re-sends only segments from k.
//! 4. **Connectivity**, trusting only refs: `rev-list --objects --stdin --not
//!    --all --quiet` over the update oids, in the quarantine.
//! 5. **Migration**: each pack's `.keep`, `.pack`, `.rev` and then `.idx`
//!    (Git finds a pack by its `.idx`) are renamed into `objects/pack` without
//!    replacement; `objects/pack` is sealed, the quarantine removed, and
//!    `objects/` sealed.
//! 6. **Publish**: one `update-ref --stdin -z` transaction outside the
//!    quarantine. Each update is a `create` when the ref is absent and a
//!    `verify` when it already names the oid; any other value refuses
//!    `GIT_DESTINATION_OCCUPIED`. No existing ref is ever moved or deleted, so
//!    existing `refs/carry/*` refs keep their digests; the receipt proves it
//!    with a digest of every `refs/carry/*` ref outside the plan, taken
//!    before and after the transaction.
//! 7. **Keeps**: each migrated pack's `.keep` is dropped if it still holds
//!    this session's message, and `objects/pack` is sealed.
//!
//! Recovery re-runs from the last journaled step: stray `tmp_*` files and
//! unjournaled packs in the quarantine are swept; migration skips files
//! already moved; publication re-plans each `create` as a `verify` once the
//! ref holds its oid; dropping a keep that is gone is a no-op.
//!
//! Refusals: `held_tip_closure_incomplete` (preflight), `segment_invalid`
//! (index-pack), `connectivity_missing`; all three are `GIT_HAVES_UNPROVABLE`
//! and mean re-negotiate (spike Q3: a stale have fails at one of the last two).
//! After the first two the session can [`Ingest::abandon`]; a connectivity
//! failure abandons it itself. Nothing is published by a refused session.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::io::{AsRawFd as _, FromRawFd as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::super::estimate::{
    child_refusal, cstring, drain, hardened, has_control, local_probe, run_probe, Refused,
    Repository,
};
use super::journal::{plan_digest, Found, Journal, JournalStore, Record};
use super::{is_oid, lines, overlap, run_child, Offer, Outcome, PackPlan, StderrStore};
use crate::durable::{seal_dir, seal_file};
use crate::BulkloadRefusal;

/// The files a pack may have, in migration order: `.keep` first so the pack
/// is kept from the moment Git can see it, `.idx` last because Git finds a
/// pack by its `.idx`.
const PACK_FILES: [&str; 4] = ["keep", "pack", "rev", "idx"];

/// The destination repository, as the offer probe resolved it.
#[derive(Debug)]
pub struct Target {
    repository: Repository,
    root: PathBuf,
    common: PathBuf,
    objects: PathBuf,
    offer: Offer,
}

impl Target {
    /// Probe the repository rooted at `path` on this host (read-only).
    ///
    /// # Errors
    /// As [`super::Source::probe`].
    pub fn probe(path: &Path, store: Option<&StderrStore>) -> Outcome<Self> {
        if has_control(path.as_os_str()) {
            return Err(BulkloadRefusal::PathNotPortable.into());
        }
        let inside =
            |paths: &[&Path]| store.is_some_and(|store| paths.iter().any(|at| store.is_inside(at)));
        if inside(&[path]) {
            return Err(overlap());
        }
        let probe = run_probe(&mut local_probe(path), store)?;
        if inside(&[&probe.root, &probe.git_dir, &probe.common]) {
            return Err(overlap());
        }
        let repository = Repository {
            git_dir: probe.git_dir,
            ceiling: probe.ceiling,
        };
        let objects = run_child(
            hardened(&repository).args([
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ]),
            &[],
            store,
            "objects_path_failed",
            |stdout| {
                let mut text = Vec::new();
                stdout.read_to_end(&mut text)?;
                Ok(text)
            },
        )?;
        let objects = objects
            .strip_suffix(b"\n")
            .filter(|path| path.starts_with(b"/") && !path.iter().any(u8::is_ascii_control))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let objects = std::fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(objects)))?;
        Ok(Self {
            repository,
            root: probe.root,
            common: probe.common,
            objects,
            offer: Offer {
                tips: probe.tips,
                shallow: probe.shallow,
                partial: probe.partial,
            },
        })
    }

    /// What this destination offers the sender.
    #[must_use]
    pub const fn offer(&self) -> &Offer {
        &self.offer
    }

    /// The object directory packs are published into.
    #[must_use]
    pub fn objects(&self) -> &Path {
        &self.objects
    }

    fn contains_state(&self, inside: impl Fn(&Path) -> bool) -> bool {
        [
            self.root.as_path(),
            self.repository.git_dir.as_path(),
            self.common.as_path(),
            self.objects.as_path(),
        ]
        .into_iter()
        .any(inside)
    }

    /// The destination command: the estimate's hardened invocation (no
    /// hooks, no auto-gc or maintenance, `GIT_NO_LAZY_FETCH`,
    /// `GIT_NO_REPLACE_OBJECTS`, `LC_ALL=C`), with pack and ref writes fsynced
    /// by Git as well, and `GIT_QUARANTINE_PATH` stripped.
    ///
    /// #75 r1 B3: grafts are off (`GIT_GRAFT_FILE=/dev/null`), and neither
    /// `GIT_SHALLOW_FILE` nor `GIT_REPLACE_REF_BASE` is inherited. A graft
    /// that made a held tip parentless hid a deleted parent from both the
    /// preflight and the connectivity check; the history they walk is now
    /// the one the objects record (and the repository's real shallow file).
    fn git(&self) -> Command {
        let mut command = hardened(&self.repository);
        command
            .args([
                "-c",
                "core.fsync=committed,derived-metadata",
                "-c",
                "core.fsyncMethod=fsync",
            ])
            .env_remove("GIT_QUARANTINE_PATH")
            .env_remove("GIT_SHALLOW_FILE")
            .env_remove("GIT_REPLACE_REF_BASE")
            .env("GIT_GRAFT_FILE", "/dev/null");
        command
    }

    /// [`Target::git`] inside the quarantine `dir`: new objects go there,
    /// reads fall through to the main store, and Git refuses ref updates.
    fn quarantined(&self, dir: &Path) -> Command {
        let mut command = self.git();
        command
            .env("GIT_OBJECT_DIRECTORY", dir)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.objects)
            .env("GIT_QUARANTINE_PATH", dir);
        command
    }
}

/// One ref the ingest publishes: `name` must end up naming `oid`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RefUpdate {
    /// Full ref name under `refs/`.
    pub name: String,
    /// The object it names.
    pub oid: String,
}

/// What a destination is asked to ingest: the sender's plan (its `pack_id`,
/// segment count and haves) and the refs to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestPlan {
    pack_id: String,
    segments: usize,
    haves: Vec<String>,
    updates: Vec<RefUpdate>,
}

impl IngestPlan {
    /// The ingest of `plan` publishing `updates`.
    ///
    /// # Errors
    /// As [`IngestPlan::new`].
    pub fn of(plan: &PackPlan, updates: Vec<RefUpdate>) -> crate::Result<Self> {
        Self::new(
            plan.pack_id(),
            plan.segments(),
            plan.haves().to_vec(),
            updates,
        )
    }

    /// Build a plan from its parts, as the wire will carry them.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for a `pack_id` that is not 64 lowercase hex
    /// digits, a have or oid that is not an object name, a ref name outside
    /// the conservative set (`refs/carry/`, then `[A-Za-z0-9._/+@-]`
    /// components, no empty, dot-leading or `.lock` component, no `..`; #75
    /// r1 D7), or two names that clash: equal once ASCII case is folded, or
    /// one a directory of the other (#75 r1 B4, D3).
    pub fn new(
        pack_id: &str,
        segments: usize,
        haves: Vec<String>,
        mut updates: Vec<RefUpdate>,
    ) -> crate::Result<Self> {
        let bad = || BulkloadRefusal::FieldDomainViolation;
        if !pack_id_ok(pack_id) {
            return Err(bad());
        }
        if !haves.iter().all(|value| is_oid(value.as_bytes())) {
            return Err(bad());
        }
        updates.sort();
        for (index, update) in updates.iter().enumerate() {
            if !ref_name(&update.name) || !is_oid(update.oid.as_bytes()) {
                return Err(bad());
            }
            if updates
                .iter()
                .skip(index + 1)
                .any(|other| clashes(&update.name, &other.name))
            {
                return Err(bad());
            }
        }
        Ok(Self {
            pack_id: pack_id.to_owned(),
            segments,
            haves,
            updates,
        })
    }

    /// The sender's plan id.
    #[must_use]
    pub fn pack_id(&self) -> &str {
        &self.pack_id
    }

    /// The refs this ingest publishes, sorted by name.
    #[must_use]
    pub fn updates(&self) -> &[RefUpdate] {
        &self.updates
    }

    fn block(&self, git_dir: &Path) -> Vec<Record> {
        let mut block = vec![Record::Begin {
            pack_id: self.pack_id.clone(),
            segments: self.segments,
            git_dir: git_dir.as_os_str().as_bytes().to_vec(),
        }];
        block.extend(self.haves.iter().cloned().map(Record::Have));
        block.extend(
            self.updates
                .iter()
                .map(|update| Record::Ref(update.name.clone().into_bytes(), update.oid.clone())),
        );
        let sealed = plan_digest(&block);
        block.push(Record::Planned(sealed));
        block
    }

    /// Read the plan back from a journal's first block.
    fn from_records(records: &[Record]) -> crate::Result<(Self, Vec<u8>, usize)> {
        let bad = || BulkloadRefusal::SchemaMismatch;
        let end = records
            .iter()
            .position(|record| matches!(record, Record::Planned(_)))
            .ok_or_else(bad)?;
        let (block, rest) = records.split_at(end);
        let Some(Record::Planned(sealed)) = rest.first() else {
            return Err(bad());
        };
        if &plan_digest(block) != sealed {
            return Err(bad());
        }
        let Some((
            Record::Begin {
                pack_id,
                segments,
                git_dir,
            },
            body,
        )) = block.split_first()
        else {
            return Err(bad());
        };
        let mut haves = Vec::new();
        let mut updates = Vec::new();
        for record in body {
            match record {
                Record::Have(oid) if updates.is_empty() => haves.push(oid.clone()),
                Record::Ref(name, oid) => updates.push(RefUpdate {
                    name: String::from_utf8(name.clone()).map_err(|_| bad())?,
                    oid: oid.clone(),
                }),
                _ => return Err(bad()),
            }
        }
        let plan = Self::new(pack_id, *segments, haves, updates).map_err(|_| bad())?;
        Ok((plan, git_dir.clone(), end + 1))
    }
}

/// Whether `pack_id` is a plan id: exactly 64 lowercase hex digits (#75 r1
/// B2: it names a journal file, so nothing else may reach a path).
pub(super) fn pack_id_ok(pack_id: &str) -> bool {
    pack_id.len() == 64
        && pack_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether two ref names cannot both exist, or would alias, on some
/// destination: equal once ASCII case is folded (a case-insensitive file
/// system maps both to one loose file, #75 r1 B4), or one is a directory of
/// the other (a directory/file conflict, #75 r1 D3). Equal names clash too.
fn clashes(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    a == b
        || b.strip_prefix(a.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
        || a.strip_prefix(b.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Git's ref-name rules, narrowed to a conservative byte set and, for M1, to
/// `refs/carry/` (#75 r1 D7: no `refs/replace/`, `refs/heads/` or other
/// namespace an ordinary git command reads).
fn ref_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("refs/") else {
        return false;
    };
    if !name.starts_with("refs/carry/") {
        return false;
    }
    name.len() <= 1024
        && !name.contains("..")
        && !name.contains("@{")
        && rest.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part
                    .rsplit_once('.')
                    .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("lock"))
                && !part.ends_with('.')
                && part.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b'@')
                })
        })
}

/// A journaled segment: the destination's ack for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentAck {
    /// Segment index.
    pub index: usize,
    /// The pack's name hash, as `index-pack` printed it.
    pub pack: String,
    /// Bytes received.
    pub bytes: u64,
    /// BLAKE3 of the bytes received.
    pub blake3: String,
}

/// What a finished ingest did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReceipt {
    /// The sender's plan id.
    pub pack_id: String,
    /// Every segment, as journaled.
    pub segments: Vec<SegmentAck>,
    /// The refs published (created or verified), sorted by name.
    pub published: Vec<RefUpdate>,
    /// BLAKE3 of every `refs/carry/*` ref outside the plan, before the
    /// transaction.
    pub carry_refs_before: String,
    /// The same digest after it; equal when no existing carry ref moved.
    pub carry_refs_after: String,
}

impl IngestReceipt {
    /// `key=value` receipt lines.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("pack_id={}", self.pack_id),
            format!("segments={}", self.segments.len()),
            format!(
                "received_bytes={}",
                self.segments.iter().map(|ack| ack.bytes).sum::<u64>()
            ),
            format!("published_refs={}", self.published.len()),
            format!("carry_refs_before_blake3={}", self.carry_refs_before),
            format!("carry_refs_after_blake3={}", self.carry_refs_after),
            format!(
                "carry_refs_unchanged={}",
                u8::from(self.carry_refs_before == self.carry_refs_after)
            ),
        ];
        for ack in &self.segments {
            lines.push(format!(
                "segment={} pack={} bytes={} blake3={}",
                ack.index, ack.pack, ack.bytes, ack.blake3
            ));
        }
        lines
    }
}

/// How far a session got, from its journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Receiving,
    Connected,
    Migrated,
    Published,
    Done,
}

/// An open ingest session.
#[derive(Debug)]
pub struct Ingest<'a> {
    target: &'a Target,
    plan: IngestPlan,
    journal: Journal,
    acks: Vec<SegmentAck>,
    stage: Stage,
    carry: Option<(String, String)>,
    store: Option<&'a StderrStore>,
}

impl<'a> Ingest<'a> {
    /// Open the ingest of `plan` into `target`, journaled in `journals`: a
    /// new session (preflight, then the sealed plan and the quarantine), or
    /// the existing one for the same `pack_id`, which must hold the same
    /// plan for the same repository. An existing session still receiving
    /// runs the preflight again and sweeps its quarantine.
    ///
    /// # Errors
    /// `SNAPSHOT_ROOTS_OVERLAP` for a state dir inside the destination,
    /// `JOURNAL_OWNERSHIP_CONFLICT` for a journal held by another session or
    /// holding another plan, `GIT_DESTINATION_OCCUPIED` for a plan ref that
    /// already names another object, `GIT_HAVES_UNPROVABLE` /
    /// `held_tip_closure_incomplete` from the preflight, and I/O failures.
    pub fn open(
        target: &'a Target,
        journals: &JournalStore,
        plan: IngestPlan,
        store: Option<&'a StderrStore>,
    ) -> Outcome<Self> {
        Self::start(target, journals, Some(plan), None, store)
    }

    /// Resume the session for `pack_id` from its journal alone: the plan is
    /// read back from it, so recovery never needs the sender.
    ///
    /// # Errors
    /// `SEALED_OBJECT_MISSING` when no journal with a sealed plan exists,
    /// and as [`Ingest::open`].
    pub fn resume(
        target: &'a Target,
        journals: &JournalStore,
        pack_id: &str,
        store: Option<&'a StderrStore>,
    ) -> Outcome<Self> {
        Self::start(target, journals, None, Some(pack_id), store)
    }

    fn start(
        target: &'a Target,
        journals: &JournalStore,
        plan: Option<IngestPlan>,
        pack_id: Option<&str>,
        store: Option<&'a StderrStore>,
    ) -> Outcome<Self> {
        if target.contains_state(|path| journals.is_inside(path))
            || store.is_some_and(|store| target.contains_state(|path| store.is_inside(path)))
        {
            return Err(overlap());
        }
        let pack_id = plan
            .as_ref()
            .map(IngestPlan::pack_id)
            .or(pack_id)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?
            .to_owned();
        // #75 r1 B2: a resume's pack_id names a file; nothing but a plan id
        // may reach the journal store.
        if !pack_id_ok(&pack_id) {
            return Err(BulkloadRefusal::FieldDomainViolation.into());
        }
        let git_dir = target.repository.git_dir.as_path();
        match Journal::open(journals, &pack_id)? {
            Found::Fresh(journal) => {
                let Some(plan) = plan else {
                    // A resume found nothing sealed; leave no empty journal.
                    journal.remove()?;
                    return Err(BulkloadRefusal::SealedObjectMissing.into());
                };
                let session = Self {
                    target,
                    plan,
                    journal,
                    acks: Vec::new(),
                    stage: Stage::Receiving,
                    carry: None,
                    store,
                };
                // A refusal here leaves nothing: no plan, no quarantine, no
                // journal.
                // #75 r1 B1: a fresh session never adopts a quarantine it did
                // not make (a lost state dir's, or another state dir's).
                let fresh = session
                    .occupied()
                    .and_then(|()| session.preflight())
                    .and_then(|()| match open_dir(&session.quarantine_path()) {
                        Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => Ok(()),
                        Ok(_) => Err(Refused::because(
                            BulkloadRefusal::GitDestinationOccupied,
                            "quarantine_exists",
                        )),
                        Err(error) => Err(error.into()),
                    });
                if let Err(refused) = fresh {
                    session.journal.remove()?;
                    return Err(refused);
                }
                let block = session.plan.block(git_dir);
                session.journal.append(&block)?;
                session.make_quarantine()?;
                Ok(session)
            }
            Found::Existing(journal, records) => {
                let (journaled, recorded_dir, start) = IngestPlan::from_records(&records)?;
                if recorded_dir != git_dir.as_os_str().as_bytes()
                    || plan.as_ref().is_some_and(|plan| plan != &journaled)
                {
                    return Err(BulkloadRefusal::JournalOwnershipConflict.into());
                }
                let mut session = Self {
                    target,
                    plan: journaled,
                    journal,
                    acks: Vec::new(),
                    stage: Stage::Receiving,
                    carry: None,
                    store,
                };
                session.replay(records.get(start..).unwrap_or_default())?;
                if session.stage == Stage::Receiving {
                    session.preflight()?;
                    session.quarantine(true)?;
                    session.sweep()?;
                }
                Ok(session)
            }
        }
    }

    /// Apply the journaled steps after the plan block, in order.
    fn replay(&mut self, records: &[Record]) -> crate::Result<()> {
        let bad = || BulkloadRefusal::SchemaMismatch;
        for record in records {
            match (record, self.stage) {
                (
                    Record::Segment {
                        index,
                        pack,
                        bytes,
                        blake3,
                    },
                    Stage::Receiving,
                ) if *index == self.acks.len() && *index < self.plan.segments => {
                    self.acks.push(SegmentAck {
                        index: *index,
                        pack: pack.clone(),
                        bytes: *bytes,
                        blake3: blake3.clone(),
                    });
                }
                (Record::Connected, Stage::Receiving) if self.acks.len() == self.plan.segments => {
                    self.stage = Stage::Connected;
                }
                (Record::Migrated, Stage::Connected) => self.stage = Stage::Migrated,
                (Record::Published(before, after), Stage::Migrated) => {
                    self.carry = Some((before.clone(), after.clone()));
                    self.stage = Stage::Published;
                }
                (Record::Done, Stage::Published) => self.stage = Stage::Done,
                _ => return Err(bad()),
            }
        }
        Ok(())
    }

    /// The segment the destination wants next: what `GitResume` carries.
    #[must_use]
    pub const fn next_segment(&self) -> usize {
        self.acks.len()
    }

    /// Segments the plan has.
    #[must_use]
    pub const fn segments(&self) -> usize {
        self.plan.segments
    }

    /// The plan being ingested.
    #[must_use]
    pub const fn plan(&self) -> &IngestPlan {
        &self.plan
    }

    fn quarantine_path(&self) -> PathBuf {
        self.target
            .objects
            .join(format!("incoming-bulkload-{}", self.plan.pack_id))
    }

    /// Create the quarantine and its `pack/` for a new session, refusing
    /// `GIT_DESTINATION_OCCUPIED` if the name already exists (#75 r1 B1).
    fn make_quarantine(&self) -> Outcome<()> {
        let objects = open_dir(&self.target.objects)?;
        let name = cstring(format!("incoming-bulkload-{}", self.plan.pack_id).as_bytes())?;
        // SAFETY: `objects` is an open directory and `name` NUL-terminated.
        if unsafe { libc::mkdirat(objects.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err(Refused::because(
                    BulkloadRefusal::GitDestinationOccupied,
                    "quarantine_exists",
                ));
            }
            return Err(BulkloadRefusal::from(error).into());
        }
        self.quarantine(true)?;
        Ok(())
    }

    /// Open the quarantine directory and its `pack/`, creating and sealing
    /// them when `create` is set.
    fn quarantine(&self, create: bool) -> crate::Result<Option<(File, File)>> {
        let objects = open_dir(&self.target.objects)?;
        let name = cstring(format!("incoming-bulkload-{}", self.plan.pack_id).as_bytes())?;
        if create {
            make_dir(&objects, &name)?;
        }
        let quarantine = match open_dir_at(&objects, &name) {
            Ok(quarantine) => quarantine,
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) if !create => return Ok(None),
            Err(error) => return Err(error),
        };
        let pack = cstring(b"pack")?;
        if create {
            make_dir(&quarantine, &pack)?;
            seal_dir(&quarantine)?;
            seal_dir(&objects)?;
        }
        let pack = open_dir_at(&quarantine, &pack)?;
        Ok(Some((quarantine, pack)))
    }

    /// R-N75 held-tip closure preflight over the plan's haves.
    fn preflight(&self) -> Outcome<()> {
        if self.plan.haves.is_empty() {
            return Ok(());
        }
        let mut input = Vec::new();
        for have in &self.plan.haves {
            input.extend_from_slice(have.as_bytes());
            input.push(b'\n');
        }
        let mut absent = false;
        let refuse = |mut refused: Refused| {
            refused.refusal = BulkloadRefusal::GitHavesUnprovable;
            refused.reason = Some("held_tip_closure_incomplete");
            refused
        };
        run_child(
            self.target.git().args([
                "rev-list",
                "--objects",
                "--no-object-names",
                "--missing=print",
                "--stdin",
            ]),
            &input,
            self.store,
            "held_tip_closure_incomplete",
            |stdout| {
                lines(stdout, |line| {
                    absent |= line.starts_with(b"?");
                    Ok(())
                })
            },
        )
        .map_err(refuse)?;
        if absent {
            return Err(Refused::because(
                BulkloadRefusal::GitHavesUnprovable,
                "held_tip_closure_incomplete",
            ));
        }
        Ok(())
    }

    /// Current values of the plan's refs, by name, resolved byte-exactly.
    fn current(&self) -> Outcome<BTreeMap<String, String>> {
        let names: Vec<&str> = self.plan.updates.iter().map(|u| u.name.as_str()).collect();
        let values = self.resolve(&names)?;
        Ok(names
            .into_iter()
            .zip(values)
            .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value)))
            .collect())
    }

    /// Refuse `GIT_DESTINATION_OCCUPIED` / `carry_ref_occupied` when a plan
    /// ref already names another object, is a symbolic ref (#75 r1 D2), or
    /// clashes with an existing ref of another spelling: equal once ASCII
    /// case is folded (#75 r1 B4) or a directory of it, either way (#75 r1
    /// D3). Runs at open and again just before migration.
    fn occupied(&self) -> Outcome<()> {
        let occupied = || {
            Refused::because(
                BulkloadRefusal::GitDestinationOccupied,
                "carry_ref_occupied",
            )
        };
        let current = self.current()?;
        for update in &self.plan.updates {
            if current
                .get(&update.name)
                .is_some_and(|oid| oid != &update.oid)
            {
                return Err(occupied());
            }
        }
        for name in self.all_refs(None)? {
            if self
                .plan
                .updates
                .iter()
                .any(|update| update.name != name && clashes(&update.name, &name))
            {
                return Err(occupied());
            }
        }
        for update in &self.plan.updates {
            // `symbolic-ref -q` exits 0 for a symref (dangling or not) and 1
            // for anything else; a dangling one is invisible to for-each-ref.
            let status = self
                .target
                .git()
                .args(["symbolic-ref", "-q", &update.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            match status.code() {
                Some(1) => {}
                Some(0) => {
                    return Err(Refused::because(
                        BulkloadRefusal::GitDestinationOccupied,
                        "carry_ref_symbolic",
                    ))
                }
                _ => return Err(BulkloadRefusal::GitInventoryMalformed.into()),
            }
        }
        Ok(())
    }

    /// Bring the quarantine to exactly this session's journaled packs:
    /// remove `tmp_*` leftovers and packs no journal record names from its
    /// `pack/`, refuse anything else in it or beside `pack/`
    /// (`GIT_DESTINATION_OCCUPIED`), and refuse `SEALED_OBJECT_MISSING` when a
    /// journaled pack's `.pack` or `.idx` is gone (#75 r1 B1).
    fn sweep(&self) -> crate::Result<()> {
        let Some((quarantine, pack)) = self.quarantine(false)? else {
            return if self.acks.is_empty() {
                Ok(())
            } else {
                Err(BulkloadRefusal::SealedObjectMissing)
            };
        };
        if entries(&quarantine)? != [b"pack".to_vec()] {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        let kept: BTreeSet<&str> = self.acks.iter().map(|ack| ack.pack.as_str()).collect();
        for name in entries(&pack)? {
            let Ok(text) = std::str::from_utf8(&name) else {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            };
            let ours = text.starts_with("tmp_")
                || text
                    .strip_prefix("pack-")
                    .and_then(|rest| rest.split_once('.'))
                    .is_some_and(|(hash, extension)| {
                        is_oid(hash.as_bytes())
                            && PACK_FILES.contains(&extension)
                            && !kept.contains(hash)
                    });
            let journaled = text
                .strip_prefix("pack-")
                .and_then(|rest| rest.split_once('.'))
                .is_some_and(|(hash, extension)| {
                    kept.contains(hash) && PACK_FILES.contains(&extension)
                });
            if ours {
                unlink_at(&pack, &cstring(&name)?)?;
            } else if !journaled {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
        }
        seal_dir(&pack)?;
        for ack in &self.acks {
            for extension in ["pack", "idx"] {
                let name = cstring(format!("pack-{}.{extension}", ack.pack).as_bytes())?;
                if open_file_at(&pack, &name).is_err() {
                    return Err(BulkloadRefusal::SealedObjectMissing);
                }
            }
        }
        Ok(())
    }

    /// Receive segment `index` from `reader` into the quarantine. It must be
    /// [`Ingest::next_segment`]. Returns the ack once the segment is sealed
    /// and journaled.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for any other index or a session past
    /// receiving; `GIT_HAVES_UNPROVABLE` / `segment_invalid` when
    /// `index-pack` refuses the bytes (the quarantine is swept, and the
    /// segment can be sent again or the session abandoned); I/O failures.
    pub fn receive(&mut self, index: usize, reader: &mut (dyn Read + Send)) -> Outcome<SegmentAck> {
        if self.stage != Stage::Receiving || index != self.acks.len() || index >= self.plan.segments
        {
            return Err(BulkloadRefusal::FieldDomainViolation.into());
        }
        let (quarantine, pack_dir) = self.quarantine(false)?.ok_or(BulkloadRefusal::Io(None))?;
        drop(quarantine);
        let keep = format!("--keep=bulkload git-carry-v2 {}", self.plan.pack_id);
        let mut command = self.target.quarantined(&self.quarantine_path());
        command.args(["index-pack", "--stdin", "--fix-thin", keep.as_str()]);
        let indexed = index_pack(&mut command, reader, self.store);
        let (hash, bytes, blake3) = match indexed {
            Ok(indexed) => indexed,
            Err(refused) => {
                self.sweep()?;
                return Err(refused);
            }
        };
        fault_point!(GitIngestAfterIndexPack);
        for extension in PACK_FILES {
            match open_file_at(
                &pack_dir,
                &cstring(format!("pack-{hash}.{extension}").as_bytes())?,
            ) {
                Ok(file) => seal_file(&file)?,
                Err(BulkloadRefusal::Io(Some(libc::ENOENT))) if extension == "rev" => {}
                Err(error) => return Err(error.into()),
            }
        }
        seal_dir(&pack_dir)?;
        // #75 r1 D1(a): the ack is a record on the state dir's device; the
        // pack's device is fully flushed first, so the ack never outlives the
        // pack on power loss (device-wide on Darwin, R-N103).
        flush_device(&pack_dir)?;
        let ack = SegmentAck {
            index,
            pack: hash,
            bytes,
            blake3,
        };
        self.journal.append(&[Record::Segment {
            index,
            pack: ack.pack.clone(),
            bytes,
            blake3: ack.blake3.clone(),
        }])?;
        fault_point!(GitIngestAfterSegment);
        self.acks.push(ack.clone());
        Ok(ack)
    }

    /// Give up a session that has not migrated: remove its quarantine and
    /// journal it abandoned (the next open of the same `pack_id` starts
    /// fresh).
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` once packs have migrated (such a session
    /// can only finish); I/O failures.
    pub fn abandon(self, reason: &'static str) -> Outcome<()> {
        if self.stage >= Stage::Migrated {
            return Err(BulkloadRefusal::FieldDomainViolation.into());
        }
        self.discard_quarantine()?;
        self.journal
            .append(&[Record::Abandoned(reason.to_owned())])?;
        Ok(())
    }

    fn discard_quarantine(&self) -> crate::Result<()> {
        let objects = open_dir(&self.target.objects)?;
        let Some((quarantine, pack)) = self.quarantine(false)? else {
            return Ok(());
        };
        for name in entries(&pack)? {
            unlink_at(&pack, &cstring(&name)?)?;
        }
        drop(pack);
        remove_dir_at(&quarantine, &cstring(b"pack")?)?;
        drop(quarantine);
        remove_dir_at(
            &objects,
            &cstring(format!("incoming-bulkload-{}", self.plan.pack_id).as_bytes())?,
        )?;
        seal_dir(&objects)?;
        Ok(())
    }

    /// Finish: connectivity, migration, the ref transaction and the keeps,
    /// from wherever the journal left off.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` while segments are missing;
    /// `GIT_HAVES_UNPROVABLE` / `connectivity_missing` (the session is
    /// abandoned: nothing was published); `GIT_DESTINATION_OCCUPIED` when a
    /// plan ref names another object (packs stay kept; nothing published);
    /// `SEALED_OBJECT_MISSING` when a journaled pack is in neither the
    /// quarantine nor `objects/pack`; I/O failures.
    pub fn finish(mut self) -> Outcome<IngestReceipt> {
        if self.acks.len() != self.plan.segments {
            return Err(BulkloadRefusal::FieldDomainViolation.into());
        }
        if self.stage == Stage::Receiving {
            // #75 r1 B1: the check sees exactly the journaled packs, and
            // every one of them is there.
            self.sweep()?;
            if let Err(refused) = self.connected() {
                self.discard_quarantine()?;
                self.journal
                    .append(&[Record::Abandoned("connectivity_missing".to_owned())])?;
                return Err(refused);
            }
            self.journal.append(&[Record::Connected])?;
            fault_point!(GitIngestAfterConnected);
            self.stage = Stage::Connected;
        }
        if self.stage == Stage::Connected {
            // #75 r1 D3: refs can move between open and now. Refuse before
            // any pack migrates, while the caller can still abandon the
            // session (or finish it once the ref is cleared).
            self.occupied()?;
            self.migrate()?;
            fault_point!(GitIngestAfterMigrate);
            self.journal.append(&[Record::Migrated])?;
            self.stage = Stage::Migrated;
        }
        if self.stage == Stage::Migrated {
            let (before, after) = self.publish()?;
            fault_point!(GitIngestAfterPublish);
            self.journal
                .append(&[Record::Published(before.clone(), after.clone())])?;
            self.carry = Some((before, after));
            self.stage = Stage::Published;
        }
        if self.stage >= Stage::Published {
            // #75 r1 D1(b): a journaled publication is re-checked; a ref lost
            // to power loss is published again (its objects checked first).
            self.ensure_published()?;
        }
        if self.stage == Stage::Published {
            self.drop_keeps()?;
            fault_point!(GitIngestBeforeDone);
            self.journal.append(&[Record::Done])?;
            self.stage = Stage::Done;
        }
        let (carry_refs_before, carry_refs_after) =
            self.carry.take().ok_or(BulkloadRefusal::Io(None))?;
        Ok(IngestReceipt {
            pack_id: self.plan.pack_id,
            segments: self.acks,
            published: self.plan.updates,
            carry_refs_before,
            carry_refs_after,
        })
    }

    /// The refs-only connectivity check, in the quarantine.
    fn connected(&self) -> Outcome<()> {
        if self.plan.updates.is_empty() {
            return Ok(());
        }
        let mut input = Vec::new();
        for update in &self.plan.updates {
            input.extend_from_slice(update.oid.as_bytes());
            input.push(b'\n');
        }
        run_child(
            self.target.quarantined(&self.quarantine_path()).args([
                "rev-list",
                "--objects",
                "--stdin",
                "--not",
                "--all",
                "--quiet",
            ]),
            &input,
            self.store,
            "connectivity_missing",
            |stdout| {
                std::io::copy(stdout, &mut std::io::sink())?;
                Ok(())
            },
        )
        .map_err(|mut refused| {
            refused.refusal = BulkloadRefusal::GitHavesUnprovable;
            refused
        })
    }

    /// Move every journaled pack into `objects/pack`, `.keep` first and
    /// `.idx` last, without replacing anything; then seal `objects/pack`,
    /// remove the quarantine and seal `objects/`.
    fn migrate(&self) -> crate::Result<()> {
        let objects = open_dir(&self.target.objects)?;
        let target = open_dir_at(&objects, &cstring(b"pack")?)?;
        let from = self.quarantine(false)?;
        for ack in &self.acks {
            for extension in PACK_FILES {
                let name = cstring(format!("pack-{}.{extension}", ack.pack).as_bytes())?;
                let moved = match &from {
                    Some((_, pack)) => rename_noreplace(pack, &name, &target, &name),
                    None => Err(BulkloadRefusal::Io(Some(libc::ENOENT))),
                };
                match moved {
                    Ok(()) => fault_point!(GitIngestMidMigrate),
                    // Already there: an earlier attempt moved it, or Git
                    // holds the identical pack (names are content hashes).
                    Err(BulkloadRefusal::Io(Some(libc::EEXIST))) => {
                        if let Some((_, pack)) = &from {
                            unlink_at(pack, &name)?;
                        }
                    }
                    Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => {
                        let present = open_file_at(&target, &name).is_ok();
                        if !present && extension != "rev" {
                            return Err(BulkloadRefusal::SealedObjectMissing);
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        seal_dir(&target)?;
        drop(from);
        self.discard_quarantine()?;
        seal_dir(&objects)?;
        flush_device(&objects)?;
        Ok(())
    }

    /// One `update-ref --stdin -z` transaction: `create` for an absent ref,
    /// `verify` for one already naming its oid. Returns the digests of the
    /// `refs/carry/*` refs outside the plan before and after.
    fn publish(&self) -> Outcome<(String, String)> {
        let current = self.current()?;
        let mut transaction = Vec::new();
        for update in &self.plan.updates {
            let op = match current.get(&update.name) {
                None => "create",
                Some(oid) if oid == &update.oid => "verify",
                Some(_) => {
                    return Err(Refused::because(
                        BulkloadRefusal::GitDestinationOccupied,
                        "carry_ref_occupied",
                    ))
                }
            };
            transaction.extend_from_slice(op.as_bytes());
            transaction.push(b' ');
            transaction.extend_from_slice(update.name.as_bytes());
            transaction.push(0);
            transaction.extend_from_slice(update.oid.as_bytes());
            transaction.push(0);
        }
        let before = self.carry_digest()?;
        if !transaction.is_empty() {
            // #75 r1 D2: `--no-deref`, so a symref at a plan name is never
            // followed (and `occupied` refuses one before this).
            run_child(
                self.target
                    .git()
                    .args(["update-ref", "--no-deref", "--stdin", "-z"]),
                &transaction,
                self.store,
                "ref_transaction_refused",
                |stdout| {
                    std::io::copy(stdout, &mut std::io::sink())?;
                    Ok(())
                },
            )
            .map_err(|mut refused| {
                refused.refusal = BulkloadRefusal::GitDestinationOccupied;
                refused
            })?;
        }
        self.seal_refs()?;
        let after = self.carry_digest()?;
        if !self.all_published()? {
            return Err(Refused::because(
                BulkloadRefusal::GitDestinationOccupied,
                "published_ref_missing",
            ));
        }
        Ok((before, after))
    }

    /// Seal what the ref transaction wrote: every directory from `refs/` to
    /// each plan ref's parent, `packed-refs`, a `reftable/` directory and its
    /// files, and the common dir; then fully flush the device (#75 r1 D1(b):
    /// Git fsyncs ref contents, not the directories naming them).
    fn seal_refs(&self) -> crate::Result<()> {
        let common = open_dir(&self.target.common)?;
        for update in &self.plan.updates {
            let mut directory = open_dir_at(&common, &cstring(b"refs")?)?;
            let parts: Vec<&str> = update.name.split('/').collect();
            let parents = parts
                .get(1..parts.len().saturating_sub(1))
                .unwrap_or_default();
            seal_dir(&directory)?;
            for part in parents {
                directory = match open_dir_at(&directory, &cstring(part.as_bytes())?) {
                    Ok(next) => next,
                    // Packed or reftable storage has no loose directory.
                    Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => break,
                    Err(error) => return Err(error),
                };
                seal_dir(&directory)?;
            }
        }
        match open_file_at(&common, &cstring(b"packed-refs")?) {
            Ok(file) => seal_file(&file)?,
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => {}
            Err(error) => return Err(error),
        }
        match open_dir_at(&common, &cstring(b"reftable")?) {
            Ok(tables) => {
                for name in entries(&tables)? {
                    seal_file(&open_file_at(&tables, &cstring(&name)?)?)?;
                }
                seal_dir(&tables)?;
            }
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => {}
            Err(error) => return Err(error),
        }
        seal_dir(&common)?;
        flush_device(&common)?;
        Ok(())
    }

    /// Whether every plan ref resolves, byte-exactly, to its oid.
    fn all_published(&self) -> Outcome<bool> {
        let names: Vec<&str> = self.plan.updates.iter().map(|u| u.name.as_str()).collect();
        let values = self.resolve(&names)?;
        Ok(self
            .plan
            .updates
            .iter()
            .zip(values)
            .all(|(update, value)| value.as_deref() == Some(update.oid.as_str())))
    }

    /// A journaled publication whose refs no longer resolve (lost to power
    /// loss after the journal record) is published again, once the objects
    /// are shown complete without the quarantine (#75 r1 D1(b)).
    fn ensure_published(&self) -> Outcome<()> {
        if self.all_published()? {
            return Ok(());
        }
        self.occupied()?;
        self.connected_in_store()?;
        self.publish()?;
        Ok(())
    }

    /// The refs-only connectivity check against the main store.
    fn connected_in_store(&self) -> Outcome<()> {
        let mut input = Vec::new();
        for update in &self.plan.updates {
            input.extend_from_slice(update.oid.as_bytes());
            input.push(b'\n');
        }
        run_child(
            self.target.git().args([
                "rev-list",
                "--objects",
                "--stdin",
                "--not",
                "--all",
                "--quiet",
            ]),
            &input,
            self.store,
            "connectivity_missing",
            |stdout| {
                std::io::copy(stdout, &mut std::io::sink())?;
                Ok(())
            },
        )
        .map_err(|mut refused| {
            refused.refusal = BulkloadRefusal::GitHavesUnprovable;
            refused
        })
    }

    /// Resolve full ref names byte-exactly, as `cat-file --batch-check`
    /// does (loose file first, then packed): `None` for a name that names
    /// nothing. This sees a loose file a case-insensitive file system maps
    /// to a packed ref's name, which `for-each-ref` does not (#75 r1 B4).
    fn resolve(&self, names: &[&str]) -> Outcome<Vec<Option<String>>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::new();
        for name in names {
            input.extend_from_slice(name.as_bytes());
            input.push(b'\n');
        }
        let mut values = Vec::with_capacity(names.len());
        run_child(
            self.target
                .git()
                .args(["cat-file", "--batch-check=%(objectname)"]),
            &input,
            self.store,
            "ref_read_failed",
            |stdout| {
                lines(stdout, |line| {
                    values.push(if is_oid(line) {
                        Some(
                            String::from_utf8(line.to_vec())
                                .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?,
                        )
                    } else if line.ends_with(b" missing") {
                        None
                    } else {
                        return Err(BulkloadRefusal::GitInventoryMalformed);
                    });
                    Ok(())
                })
            },
        )?;
        if values.len() != names.len() {
            return Err(BulkloadRefusal::GitInventoryMalformed.into());
        }
        Ok(values)
    }

    /// BLAKE3 over `<oid> <refname>` lines of every `refs/carry/*` ref whose
    /// name the plan does not publish, in ref-name order, each oid resolved
    /// byte-exactly by name ([`Ingest::resolve`]), so a ref that a
    /// case-folded loose file now shadows changes the digest (#75 r1 B4).
    fn carry_digest(&self) -> Outcome<String> {
        let ours: BTreeSet<&str> = self
            .plan
            .updates
            .iter()
            .map(|update| update.name.as_str())
            .collect();
        let names: Vec<String> = self
            .all_refs(Some("refs/carry/"))?
            .into_iter()
            .filter(|name| !ours.contains(name.as_str()))
            .collect();
        let values = self.resolve(&names.iter().map(String::as_str).collect::<Vec<_>>())?;
        let mut hasher = blake3::Hasher::new();
        for (name, value) in names.iter().zip(values) {
            hasher.update(value.as_deref().unwrap_or("missing").as_bytes());
            hasher.update(b" ");
            hasher.update(name.as_bytes());
            hasher.update(b"\n");
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    /// Every ref name (under `prefix`, when given), as `for-each-ref` lists
    /// them, in name order.
    fn all_refs(&self, prefix: Option<&str>) -> Outcome<Vec<String>> {
        let mut command = self.target.git();
        command.args(["for-each-ref", "--format=%(refname)"]);
        if let Some(prefix) = prefix {
            command.arg(prefix);
        }
        let mut names = Vec::new();
        run_child(&mut command, &[], self.store, "ref_read_failed", |stdout| {
            lines(stdout, |line| {
                names.push(
                    String::from_utf8(line.to_vec())
                        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?,
                );
                Ok(())
            })
        })?;
        Ok(names)
    }

    /// Drop each migrated pack's `.keep` if it still holds this session's
    /// message; then seal `objects/pack`.
    fn drop_keeps(&self) -> crate::Result<()> {
        let objects = open_dir(&self.target.objects)?;
        let pack = open_dir_at(&objects, &cstring(b"pack")?)?;
        let message = format!("bulkload git-carry-v2 {}\n", self.plan.pack_id);
        for ack in &self.acks {
            let name = cstring(format!("pack-{}.keep", ack.pack).as_bytes())?;
            let mut file = match open_file_at(&pack, &name) {
                Ok(file) => file,
                Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => continue,
                Err(error) => return Err(error),
            };
            let mut held = Vec::new();
            (&mut file).take(4096).read_to_end(&mut held)?;
            if held == message.as_bytes() {
                unlink_at(&pack, &name)?;
            }
        }
        seal_dir(&pack)?;
        Ok(())
    }
}

/// `index-pack` fed from `reader`, whose bytes are counted and hashed on the
/// way in. Returns the pack hash `index-pack` printed, the bytes and their
/// BLAKE3. The child's stderr is classified, never echoed (R-N121).
fn index_pack(
    command: &mut Command,
    reader: &mut (dyn Read + Send),
    store: Option<&StderrStore>,
) -> Outcome<(String, u64, String)> {
    let capture = store.map(StderrStore::capture).transpose()?;
    let Ok(mut child) = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        if let (Some(store), Some(capture)) = (store, capture) {
            store.discard(capture);
        }
        return Err(Refused::because(
            BulkloadRefusal::GitUnavailable,
            "segment_invalid",
        ));
    };
    let (Some(mut stdin), Some(mut stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        child.wait()?;
        if let (Some(store), Some(capture)) = (store, capture) {
            store.discard(capture);
        }
        return Err(BulkloadRefusal::Io(None).into());
    };
    let (fed, drained, answer) = std::thread::scope(|scope| {
        let feeder = scope.spawn(move || -> std::io::Result<(u64, String, bool)> {
            let mut buffer = vec![0_u8; 256 * 1024];
            let mut hasher = blake3::Hasher::new();
            let mut bytes = 0_u64;
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                };
                let chunk = buffer.get(..read).unwrap_or_default();
                hasher.update(chunk);
                bytes += u64::try_from(read).unwrap_or(u64::MAX);
                // A child that stopped reading refused the pack; its status
                // says so, and nothing more is read.
                if stdin.write_all(chunk).is_err() {
                    return Ok((bytes, hasher.finalize().to_hex().to_string(), false));
                }
            }
            Ok((bytes, hasher.finalize().to_hex().to_string(), true))
        });
        let reader = scope.spawn(move || drain(stderr, capture));
        let mut answer = Vec::new();
        let read = stdout.read_to_end(&mut answer).map(|_| answer);
        (feeder.join(), reader.join(), read)
    });
    let status = child.wait()?;
    let drained = drained.map_err(|_| BulkloadRefusal::Io(None))?;
    let fed = fed.map_err(|_| BulkloadRefusal::Io(None))?;
    if !status.success() {
        return Err(child_refusal(
            BulkloadRefusal::GitHavesUnprovable,
            Some("segment_invalid"),
            store,
            drained,
        ));
    }
    if let (Some(store), (_, _, Some(capture), _)) = (store, drained) {
        store.discard(capture);
    }
    let (bytes, blake3, whole) = fed?;
    let answer = answer?;
    let hash = answer
        .strip_suffix(b"\n")
        .and_then(|line| line.strip_prefix(b"keep\t"))
        .filter(|hash| is_oid(hash))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    if !whole {
        return Err(BulkloadRefusal::GitInventoryMalformed.into());
    }
    let hash =
        String::from_utf8(hash.to_vec()).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    Ok((hash, bytes, blake3))
}

/// A full flush of `file`'s device, counted as one (`F_FULLFSYNC` on Darwin,
/// which drains the whole drive cache; `fsync` elsewhere).
fn flush_device(file: &File) -> crate::Result<()> {
    Ok(crate::counters::timed(
        crate::counters::Counter::FlushFull,
        crate::counters::Counter::FlushFullNs,
        || crate::io::sys::full_flush(file),
    )?)
}

fn open_dir(path: &Path) -> crate::Result<File> {
    let name = cstring(path.as_os_str().as_bytes())?;
    open_dir_raw(libc::AT_FDCWD, &name)
}

fn open_dir_at(parent: &File, name: &CString) -> crate::Result<File> {
    open_dir_raw(parent.as_raw_fd(), name)
}

fn open_dir_raw(at: libc::c_int, name: &CString) -> crate::Result<File> {
    // SAFETY: `at` is AT_FDCWD or an open directory; `name` is NUL-terminated;
    // no create flag, so no mode argument.
    let fd = unsafe {
        libc::openat(
            at,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_file_at(parent: &File, name: &CString) -> crate::Result<File> {
    // SAFETY: `parent` is an open directory and `name` NUL-terminated;
    // O_NONBLOCK keeps a planted FIFO from blocking the open.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn make_dir(parent: &File, name: &CString) -> crate::Result<()> {
    // SAFETY: `parent` is an open directory and `name` NUL-terminated.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EEXIST) {
            return Err(error.into());
        }
    }
    Ok(())
}

fn unlink_at(parent: &File, name: &CString) -> crate::Result<()> {
    // SAFETY: `parent` is an open directory and `name` NUL-terminated.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(error.into());
        }
    }
    Ok(())
}

fn remove_dir_at(parent: &File, name: &CString) -> crate::Result<()> {
    // SAFETY: `parent` is an open directory and `name` NUL-terminated.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(error.into());
        }
    }
    Ok(())
}

fn rename_noreplace(
    from_dir: &File,
    from: &CString,
    to_dir: &File,
    to: &CString,
) -> crate::Result<()> {
    crate::io::sys::rename_noreplace_at(from_dir, from, to_dir, to).map_err(Into::into)
}

/// Every entry name in `directory` except `.` and `..`, read through a fresh
/// descriptor of the same directory.
fn entries(directory: &File) -> crate::Result<Vec<Vec<u8>>> {
    let fresh = open_dir_at(directory, &cstring(b".")?)?;
    let fd = std::os::unix::io::IntoRawFd::into_raw_fd(fresh);
    // SAFETY: `fd` is an open directory descriptor this call owns; on
    // success `fdopendir` takes it over and `closedir` below releases it.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `fd` is still ours to close.
        unsafe { libc::close(fd) };
        return Err(error.into());
    }
    let mut names = Vec::new();
    let outcome = loop {
        // SAFETY: errno is thread-local; clearing it lets a NULL from
        // `readdir` tell the end of the stream from an error.
        unsafe { *errno() = 0 };
        // SAFETY: `stream` is a live DIR* until `closedir` below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            // SAFETY: as above.
            let code = unsafe { *errno() };
            break if code == 0 {
                Ok(())
            } else {
                Err(std::io::Error::from_raw_os_error(code))
            };
        }
        // SAFETY: `entry` points at a dirent valid until the next readdir,
        // and `d_name` is NUL-terminated.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    };
    // SAFETY: `stream` came from `fdopendir` and is closed exactly once.
    unsafe { libc::closedir(stream) };
    outcome?;
    Ok(names)
}

#[cfg(target_vendor = "apple")]
fn errno() -> *mut libc::c_int {
    // SAFETY: returns this thread's errno location; no preconditions.
    unsafe { libc::__error() }
}

#[cfg(not(target_vendor = "apple"))]
fn errno() -> *mut libc::c_int {
    // SAFETY: returns this thread's errno location; no preconditions.
    unsafe { libc::__errno_location() }
}
