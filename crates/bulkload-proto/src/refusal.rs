//! The bulkload refusal taxonomy.
//!
//! Each variant has a stable machine-readable code, and variants are grouped
//! into families by prefix: `Snapshot*`, `Capture*`, `Digest*`, `Git*`,
//! `Sqlite*`, `Path*`, `Protocol*`, `Budget*` and `Reseat*`.
//!
//! Every variant has at least one constructor outside test code
//! (`crates/bulkload-agent/tests/refusal_taxonomy.rs`, WP3): a code nothing
//! raises is deleted, not kept as vocabulary.
//!
//! Every variant is a *refusal*: the operation did not complete. Earlier durable
//! progress or prepared state can remain; inspect its receipts before retrying.
//! Refusals are values, never panics (R33).

use core::fmt;

/// A bulkload refusal.
///
/// Variants are grouped into families (see the module docs) and are
/// non-exhaustive on purpose: new refusal classes are added as values, never
/// as panics.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkloadRefusal {
    // ---- custody / snapshot integrity ------------------------------------
    /// Declared snapshot roots overlap, alias, or are not unique.
    SnapshotRootsOverlap,
    /// The source tree changed after the immutable snapshot was taken.
    SourceChangedAfterSnapshot,
    /// A capture recorded drift under its pass, so it does not hold every
    /// seat's bytes; apply refuses it until a later pass extends it clean.
    CaptureDrifted,
    /// The corpus holds no capture record for a planned item (S4, #219): its
    /// capture refused, and the capture's own outcome record names why, or
    /// no capture ran. Apply refuses it before reading anything else; a
    /// capture pass that records the item is the recovery. Distinct from
    /// `SEALED_OBJECT_MISSING`, which names a recorded capture whose bundle,
    /// link or base the corpus has lost.
    CaptureAbsent,

    // ---- digest / sealing -------------------------------------------------
    /// A content digest did not match the digest the plan was sealed against.
    DigestMismatch,
    /// A sealed object required by this stage is missing.
    SealedObjectMissing,
    /// A receipt is not bound to the plan or push it claims.
    ReceiptBindingInvalid,

    // ---- schema / contract ------------------------------------------------
    /// The input is not the record type this stage accepts.
    SchemaMismatch,
    /// A required field is absent from an otherwise well-formed record.
    RequiredFieldMissing,
    /// A declared enum-like field carries a value outside its domain.
    FieldDomainViolation,
    /// Readiness or completeness disagrees with the recorded blockers.
    ContractSelfInconsistent,
    /// A closure report found planned items whose durable ledger does not end
    /// as applied, a typed refusal or referenced-only (OI-1001-Q2).
    ClosureUnaccounted,

    // ---- path handling ----------------------------------------------------
    /// A path that must be absolute is not.
    PathNotAbsolute,
    /// A path carries control or format characters, or an interior NUL.
    PathNotPortable,
    /// A path left the tree it was resolved against (traversal or symlink).
    PathEscapesRoot,
    /// A directory lies deeper beneath the walk root than the walk descends
    /// (`walk::MAX_WALK_DEPTH`); its contents are refused as one subtree.
    PathDepthExceeded,
    /// A path relative to the walk root is longer than the walk carries
    /// (`walk::MAX_REL_PATH_BYTES`); the seat, and anything beneath it, is
    /// refused.
    PathTooLong,

    // ---- git custody ------------------------------------------------------
    /// Git is not available on this host.
    GitUnavailable,
    /// A git authority path resolves outside the live snapshot root.
    GitAuthorityOutsideRoot,
    /// Git bytes or refs changed while the live snapshot was being taken.
    GitAuthorityChanged,
    /// A git inventory (index, refs, worktrees) is malformed. The cause is
    /// none of the more specific `GIT_INVENTORY_*` sub-codes below (#106).
    GitInventoryMalformed,
    /// A bundle names prerequisite commits the receiving repository does not
    /// hold, so it cannot be verified or imported there (#106). Fetching the
    /// named commits as objects and retrying is the documented recovery.
    GitInventoryMissingPrerequisite,
    /// An intent-to-add (`git add -N`) index entry cannot be carried: its
    /// seat is absent from the captured worktree, its seat's mode is not the
    /// one the entry records (a restore re-marks the path from the seat, so
    /// `git add -N f; chmod +x f` cannot be restored as captured, #131), or
    /// it lies in a nested repository, whose index is not carried (#106).
    /// Capture refuses it, and a restore checks it again before any worktree
    /// byte is laid down. Intent-to-add entries of a captured checkout are
    /// otherwise carried as index custody.
    GitInventoryIntentToAdd,
    /// A non-bare repository has no index file (S4, #162): a `--no-checkout`
    /// clone or worktree. Git reads the absent file as an unborn index, which
    /// a plain `git checkout` populates as an initial checkout; an empty index
    /// file does not, so no carried index can restore this state. Capture
    /// refuses rather than lay down a checkout whose every HEAD path is a
    /// staged deletion.
    GitInventoryIndexAbsent,
    /// A Git inventory is well formed but larger than a carry size bound
    /// (OI-1003-Q54, #178): a v1 bundle header over its 16 MiB cap, a ref
    /// table over its bound, or a shallow envelope's manifest over its cap.
    /// Writers check before they write, so no capture is recorded that a
    /// later reader refuses for size; a reader meeting one refuses here, not
    /// `GIT_INVENTORY_MALFORMED`. The disposition is the inventory's size
    /// (most often the distinct objects its refs name), not corruption.
    GitInventoryOverCap,
    /// A capture of a bare repository (ref custody only, with no worktree)
    /// was asked to lay down a workspace: a restore, a linked worktree, an
    /// attachment or an index repair (S4, #162). Refused before anything is
    /// written; such an item imports its refs, planned without a workspace.
    GitBareCaptureWorkspace,
    /// The git destination already exists or is non-empty.
    GitDestinationOccupied,
    /// A checkout or ref bulkload landed was changed at the destination
    /// since it landed (estate converge): a seat, the index, HEAD, a
    /// standalone item's `info/exclude`, or a seat added where the source
    /// added one. Or a checkout this item landed before was removed. The
    /// item's proof failed before anything but an additive object import
    /// was written; the operator's change is never overwritten.
    GitDestinationModified,
    /// `estate-handoff` (TIN-6569, OI-1003-Q183): the workspace has no
    /// workspace ledger for the SOURCE label (never applied, refused, or
    /// its journal missing), so there is no landing to hand off. Nothing is
    /// written.
    HandoffNotLanded,
    /// `estate-handoff`: a target (`AGENTS.md`, `CLAUDE.md`, an opted-in
    /// `README.md`) is a directory, a FIFO or another file that is not
    /// regular, or a symbolic link that is not `CLAUDE.md -> AGENTS.md`
    /// (or the other way round) inside the worktree. Nothing is written for
    /// the whole workspace.
    HandoffTargetUnsupported,
    /// `estate-handoff`: a hand-off marker at byte 0 that is not the block
    /// bulkload recorded (an edit inside the block, a block with no record,
    /// a BEGIN with no END), or a writer that changed the file while the
    /// hand-off exchanged it. The file is the user's: nothing is written,
    /// an exchange is undone.
    HandoffBlockModified,
    /// `estate-handoff --check`: the hand-off block's do-not-commit token
    /// is in a target's index blob, its HEAD blob or a destination-ahead
    /// tip's blob. Read-only; nothing is written.
    HandoffMarkerCommitted,
    /// `estate-apply --adopt` (#226, OI-1003-Q171/Q172): a destination
    /// bulkload never landed holds the item's path, its repository is the
    /// capture's (related), and it differs from the current capture in a
    /// component the adopt proof covers: HEAD, the index entries, a
    /// standalone item's `info/exclude`, or a carried seat's kind, mode or
    /// content. Nothing is imported or written; the destination's work is
    /// never claimed (accept keeps it, re-carry lands it elsewhere).
    GitDestinationDiverged,
    /// Between the capture a checkout holds and the current one, the source
    /// changed what converge does not carry: the shallow frontier, nested
    /// repository custody, the omitted rebuildable roots or bareness.
    /// Nothing is written but an additive object import.
    GitConvergeShapeChanged,
    /// The capture is not newer than the one the destination checkout
    /// already holds (an older corpus applied over a newer landing).
    /// Nothing is written.
    GitConvergeStaleCapture,
    /// A converge was interrupted, by a crash or by a third party's write
    /// to a component it was replacing (which is kept, or put back). Its
    /// intent is kept: the checkout is part old, part new. Closure counts
    /// the item unaccounted whatever its disposition, until a rerun settles
    /// it forward.
    GitConvergeInterrupted,
    /// Captured and destination Git ignore policies differ.
    GitIgnorePolicyConflict,
    /// A restore destination's parent directory does not exist (R-N114):
    /// typically an enclosing item that should have created it did not.
    GitDestinationParentMissing,
    /// A nested repository holds a stash (`refs/stash` or a non-empty stash
    /// reflog): work its enclosing capture would not carry (R-N83).
    GitNestStashed,
    /// A nested repository's detached HEAD holds commits no local branch,
    /// tag or remote-tracking ref reaches (R-N83).
    GitNestDetachedUnreachable,
    /// A nested repository holds an ignored repository of its own, outside
    /// any rebuildable root, which no capture carries (R-N111). Carries the
    /// inner repository's path relative to the captured checkout, raw bytes;
    /// [`fmt::Display`] prints it escaped.
    GitNestInnerRepository(Vec<u8>),
    /// A tracked path in a nested repository has a built-in conversion
    /// attribute (`ident`, `working-tree-encoding`, `text`, `eol`, `crlf`)
    /// that can hide an edit from its status (R-N73). Carries the path
    /// relative to the captured checkout; [`fmt::Display`] prints it escaped.
    GitNestConversionAttribute(Vec<u8>),
    /// A nested repository holds a populated submodule of its own, or anything
    /// but an empty directory at one of its gitlink paths (R-N115). Carries
    /// the gitlink's path relative to the captured checkout; [`fmt::Display`]
    /// prints it escaped.
    GitNestPopulatedSubmodule(Vec<u8>),
    /// A nested repository planned as its own estate item refused its own
    /// capture in the same pass, so it carries none of its seats: the
    /// enclosing capture refuses rather than name it as the carrier (R-N114).
    /// Carries the nest's path relative to the enclosing checkout;
    /// [`fmt::Display`] prints it escaped.
    GitNestCarrierRefused(Vec<u8>),
    /// `estate-carry-files` (M2r2 b) landed a `.git` pointer file or symlink
    /// that resolves, where it landed, to an existing directory outside the
    /// carried tree: Git would act on that repository through the carried
    /// copy. The item is not recorded as carried; the landed bytes are left
    /// for the operator. Carries the entry's path relative to the
    /// destination; [`fmt::Display`] prints it escaped.
    GitCarryPointerAttached(Vec<u8>),
    /// A path given as a repository is not that repository's root: Git would
    /// resolve it to an enclosing repository, or to none at all.
    GitRepositoryNotAtPath,
    /// A destination's refs do not prove it holds their history: it is a
    /// partial clone, or shallow at a frontier other than the source's (R-N75).
    GitHavesUnprovable,
    /// A Git source is a partial clone (a promisor remote, a partial-clone
    /// filter, `extensions.partialClone`, or a `.promisor` pack in its object
    /// store or an alternate). Reading it could fault in a lazy fetch, so v1
    /// carry refuses it before any other read (S2, OI-1003-Q16).
    GitSourcePartialClone,
    /// A Git source borrows objects through `objects/info/alternates` in a
    /// way a capture cannot follow (S4, #219): a chain of alternates deeper
    /// than Git reads through the capture's private repository (which adds
    /// one level), or an entry the capture does not resolve (a quoted path).
    /// Decided at capture, before any export, so no landing ever depends on
    /// a store it does not carry. Alternates a capture can follow are
    /// followed: the bundle packs every reachable object wherever it is
    /// stored, and the landing has no alternates. Carries the alternates
    /// file that names the unfollowable entry, raw bytes; [`fmt::Display`]
    /// prints it escaped.
    GitSourceAlternates(Vec<u8>),
    /// A Git child process exited non-zero (WP3). Carries its stderr's class
    /// from the closed [`StderrClass`] set; no byte of the stderr itself is
    /// carried or printed (R-N121).
    GitChildFailed(StderrClass),

    // ---- sqlite -----------------------------------------------------------
    /// `PRAGMA quick_check` or the foreign-key check failed; for a `SQLite`
    /// snapshot seat (#218), the snapshot's `integrity_check` at the source
    /// or the destination. Its default S4 disposition is `abandon`
    /// (OI-1003-Q148).
    SqliteIntegrityCheckFailed,
    /// The database yielded a value type or magnitude the schema cannot carry.
    SqliteUnsupportedValue,
    /// `SQLite` state that may not be read or carried as it is: shared rows
    /// diverged between planning and apply (the provider verbs); a seat
    /// whose first bytes are a `SQLite` database or WAL magic, or a `-wal`,
    /// `-shm` or `-journal` by name (the transfer, #186; in `--sqlite=snapshot`
    /// mode only WAL magic, an orphan sidecar and a sidecar whose base was not
    /// carried as a snapshot, #218); or a backup step that met a writer's
    /// lock (busy timeout 0).
    SqliteStateChanged,
    /// The `SQLite` online backup (open, step or finish) failed (WP3).
    /// Carries `SQLite`'s extended result code when `SQLite` reported one,
    /// which crosses the wire beside the code ([`Self::sqlite_code`]):
    /// corruption found by the backup step (primary code 11
    /// `SQLITE_CORRUPT` or 26 `SQLITE_NOTADB`) is an integrity failure and
    /// takes [`Self::SqliteIntegrityCheckFailed`]'s default S4 disposition,
    /// `abandon` (OI-1003-Q148).
    SqliteBackupFailed(Option<i32>),
    /// A provider verb that reads a source database was run with effective
    /// uid 0 (S2, OI-1003-Q76). Opened as root, `SQLite` re-applies the
    /// database's ownership to its `-wal` (`fchown`), which moves the
    /// `-wal`'s ctime: a source metadata write no ruling admits. The verb
    /// refuses before it opens anything; run it as the database's owner.
    SqliteSourceAsRoot,
    /// A database seat a `--sqlite=snapshot` transfer would open with
    /// `SQLite` is owned by another user than the reader (S2, #218). `SQLite`
    /// creates the database's `-shm` (and, under OI-1003-Q72, an empty
    /// `-wal`) owned by the reading user, and the owner's own writer may then
    /// fail to open them read-write. Refused per seat, from the `fstat` of
    /// the descriptor the sniff already holds, before any `SQLite` open; run
    /// the transfer's source half as the database's owner.
    SqliteSourceNotOwner,

    // ---- budgets / transport ------------------------------------------------
    /// A capture, row, or spill budget was exceeded.
    BudgetExceeded,
    /// Writing the planned bytes would leave the destination filesystem with
    /// less free space than the configured floor (`--min-free-percent`,
    /// default 25%), or with no room at all (OI-1001-Q2). Inside a
    /// project-quota tree the quota is a second limit, its floor that
    /// percent of the quota's own limit; the binary names the limit that
    /// bound and both limits' numbers (`space_refusal`).
    DestinationSpaceInsufficient,
    /// A write met `ENOSPC` or `EDQUOT` (S4, #220, #126), named by the site
    /// that knows where it wrote: one of bulkload's own temporaries (under
    /// `TMPDIR`), a bundle stage, a store's state directory, or a capture's
    /// Git children (under `PRIVATE_STATE`, which no preflight charges).
    /// Carries the directory whose filesystem ran out, raw bytes
    /// ([`fmt::Display`] prints it escaped); `None` when no site can name
    /// it: a Git child's stderr is classified and never carried (R-N121). A
    /// write a space preflight charged that then runs out (an estate-apply
    /// Git child, a destination group commit) refuses
    /// `DESTINATION_SPACE_INSUFFICIENT` instead.
    SpaceExhausted(Option<Vec<u8>>),
    /// A transfer's regular-file destination path holds something this
    /// store cannot replace: a file it has no row for, or one changed since
    /// its row was written; a node of another kind; a file that appeared
    /// while the output was staged. Nothing is replaced: a rerun supersedes
    /// only an output whose identity is this store's own row (no-clobber,
    /// WP0(d), OI-1003-Q18, #187). The Git carry's occupied destinations
    /// keep `GIT_DESTINATION_OCCUPIED`.
    DestinationOccupied,
    /// A changed seat's output is this store's own, but its file system has
    /// no atomic exchange of two names (`renameat2(RENAME_EXCHANGE)`,
    /// `renameatx_np(RENAME_SWAP)`), the one call a superseding publish
    /// replaces an output with (WP0(d), #187). The old output and its row
    /// are kept; nothing is staged or asked of the source for it.
    DestinationExchangeUnsupported,
    /// A salvaged destination temporary that a refused entry staged chunks
    /// from could not be kept for the next run: the session's salvage bound
    /// (by count and bytes) was already reached, so it was removed and its
    /// chunks are sent again (#124, OI-1002-Q33).
    SalvageBoundExceeded,
    /// The frame could not be encoded or decoded.
    FrameCodec,
    /// A well-formed frame arrived that the session's state does not allow
    /// (out of order, for an unknown or settled entry, or contradicting what
    /// the peer already said), or a session's own bookkeeping disagreed with
    /// itself (WP3). Never re-synchronised: the session ends.
    ProtocolStateViolation,
    /// A worker thread, or the peer of one of its channels, ended before the
    /// work it owned was done (WP3).
    WorkerLost,

    // ---- agent session re-seat (R0, TIN-6587) -------------------------------
    /// `estate-reseat`: the destination already holds a file at a session's
    /// place that is neither the landed copy nor a byte prefix of it. It is
    /// kept as it is; the landed copy stays beside it in the landing, and
    /// both are reported for explicit resolution (never overwritten).
    ReseatDestDiverged,
    /// `estate-reseat`: a file this ledger placed changed at the destination
    /// since its last pass (a session resumed there appends to its
    /// transcript). The destination owns it from now on; it is never
    /// extended again, and newer landed bytes stay in the landing.
    ReseatDestContinued,
    /// `estate-reseat`: the landed file no longer extends the bytes placed
    /// from it (it shrank, or the BLAKE3 of the placed bytes no longer
    /// matches its prefix). Nothing is written; the placed file is kept.
    ReseatSourceRewritten,
    /// `estate-reseat`: a session's recorded working directory has no lane
    /// on the destination (no `--map`, not under the source home's `git`).
    /// The session stays in the landing and is reported.
    ReseatLaneUnmapped,
    /// `estate-reseat`: a landed transcript records no working directory, or
    /// one whose project key is not the directory it was landed under, so
    /// its lane cannot be derived. Nothing is placed for it.
    ReseatLaneUnresolved,
    /// `estate-reseat`: the destination's Claude Code `cleanupPeriodDays`
    /// (default 30) would delete a re-seated transcript at its next start,
    /// because the transcript keeps its source modification time. Nothing
    /// of Claude's is placed.
    ReseatRetentionUnsafe,
    /// `estate-reseat`: the destination's Codex thread index already holds
    /// a row for a thread under another working directory or rollout path.
    /// The row is never updated (insert only); the thread is reported.
    ReseatIndexConflict,
    /// `estate-reseat`: a landed rollout is in a form the destination's
    /// Codex cannot read (`.jsonl.gz`), or a landed file cannot be read as
    /// its form (a corrupt `.jsonl.zst`). It stays in the landing as a file
    /// and is not registered.
    ReseatUnreadableForm,
    /// `estate-reseat`: a landed agent tree the run was asked to re-seat
    /// (`.claude` or `.codex` under the landed home) is absent or not a
    /// directory (a symlink to a source-only volume). Nothing is written;
    /// `--no-claude` / `--no-codex` leave a tool out explicitly.
    ReseatLandingAbsent,
    /// `estate-reseat`: the destination has no Codex thread index
    /// (`state_5.sqlite`) to register landed threads in. Placing rollouts
    /// without rows would let the destination's first Codex start backfill
    /// them under the source's working directory, out of every lane's
    /// picker for good, so nothing is written until Codex has run there
    /// once (or `--no-codex`).
    ReseatIndexAbsent,
    /// `estate-reseat`: a landed rollout has no row in the landed Codex
    /// snapshot. Its file is placed (resumable by ID) but no row is made up
    /// for it, so no picker lists it; it is reported for explicit
    /// resolution.
    ReseatIndexRowAbsent,
    /// `estate-reseat`: two landed files map to one destination path (two
    /// source project keys mapped onto one destination key). The first
    /// keeps the path; the other stays in the landing and is reported,
    /// never superseding it.
    ReseatLaneCollision,
    /// `estate-reseat`: a destination writer (a session resumed there)
    /// appended to a transcript while a pass was extending it. The pass
    /// stopped between whole lines; the file is the destination's from
    /// then on and the rest of the landed bytes stay in the landing.
    ReseatExtendRaced,
    /// `estate-reseat`: another `estate-reseat` holds this run's private
    /// state or a destination agent root (an exclusive `flock` on each):
    /// two runs never write one destination at once (OI-1003-Q198 review).
    ReseatLocked,
    /// `estate-reseat`: a symlinked destination transcript's target changed
    /// while it was being regularized into a regular file (OI-1003-Q198
    /// and its re-review): nothing that holds a session's bytes is removed
    /// or replaced; the archive and the copy are both kept (the link left
    /// or put back and the copy kept aside, or the copy at the path when
    /// both were written).
    ReseatRegularizeRaced,
    /// `estate-reseat --undo`: the archive target a regularization row's
    /// link names is absent, not a regular file, or no longer holds the
    /// bytes the regular copy was made from (OI-1003-Q198 re-review): the
    /// link is not put back, the regular file is kept, nothing changes.
    ReseatUndoTargetChanged,
    /// `estate-reseat --check`: at least one lane failed the R0 acceptance
    /// check (its sessions are not listable and byte-identical in its
    /// destination working directory, its Git state is not equal, or its
    /// hand-off note is missing), or a check component was not given.
    ReseatR0Failed,

    // ---- handoff proof ------------------------------------------------------
    /// A credential-class probe did not prove what it set out to prove.
    ///
    /// Raised by `handoff-verify` when the receipt's verdict is not a pass:
    /// some probe failed, or a class produced no passing probe at all. The
    /// receipt is still written -- this refusal carries the nonzero exit, not
    /// the evidence.
    ProbeFailed,
    /// An underlying I/O operation refused; carries the OS errno when known.
    Io(Option<i32>),
}

impl BulkloadRefusal {
    /// `SQLite`'s extended result code, which only
    /// [`Self::SqliteBackupFailed`] carries; sent beside the code in a
    /// refusal frame, so the receiver can tell the backup's corruption
    /// (OI-1003-Q148) from its other failures.
    #[must_use]
    pub const fn sqlite_code(&self) -> Option<i32> {
        match *self {
            Self::SqliteBackupFailed(code) => code,
            _ => None,
        }
    }

    /// The stable machine-readable code for this refusal.
    ///
    /// These strings are the wire/log identity of a refusal and must not change
    /// once a milestone ships.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match *self {
            Self::SnapshotRootsOverlap => "SNAPSHOT_ROOTS_OVERLAP",
            Self::SourceChangedAfterSnapshot => "SOURCE_CHANGED_AFTER_SNAPSHOT",
            Self::CaptureDrifted => "CAPTURE_DRIFTED",
            Self::CaptureAbsent => "CAPTURE_ABSENT",
            Self::DigestMismatch => "DIGEST_MISMATCH",
            Self::SealedObjectMissing => "SEALED_OBJECT_MISSING",
            Self::ReceiptBindingInvalid => "RECEIPT_BINDING_INVALID",
            Self::SchemaMismatch => "SCHEMA_MISMATCH",
            Self::RequiredFieldMissing => "REQUIRED_FIELD_MISSING",
            Self::FieldDomainViolation => "FIELD_DOMAIN_VIOLATION",
            Self::ContractSelfInconsistent => "CONTRACT_SELF_INCONSISTENT",
            Self::ClosureUnaccounted => "CLOSURE_UNACCOUNTED",
            Self::PathNotAbsolute => "PATH_NOT_ABSOLUTE",
            Self::PathNotPortable => "PATH_NOT_PORTABLE",
            Self::PathEscapesRoot => "PATH_ESCAPES_ROOT",
            Self::PathDepthExceeded => "PATH_DEPTH_EXCEEDED",
            Self::PathTooLong => "PATH_TOO_LONG",
            Self::GitUnavailable => "GIT_UNAVAILABLE",
            Self::GitAuthorityOutsideRoot => "GIT_AUTHORITY_OUTSIDE_ROOT",
            Self::GitAuthorityChanged => "GIT_AUTHORITY_CHANGED",
            Self::GitInventoryMalformed => "GIT_INVENTORY_MALFORMED",
            Self::GitInventoryMissingPrerequisite => "GIT_INVENTORY_MISSING_PREREQUISITE",
            Self::GitInventoryIntentToAdd => "GIT_INVENTORY_INTENT_TO_ADD",
            Self::GitInventoryIndexAbsent => "GIT_INVENTORY_INDEX_ABSENT",
            Self::GitInventoryOverCap => "GIT_INVENTORY_OVER_CAP",
            Self::GitBareCaptureWorkspace => "GIT_BARE_CAPTURE_WORKSPACE",
            Self::GitDestinationOccupied => "GIT_DESTINATION_OCCUPIED",
            Self::GitDestinationModified => "GIT_DESTINATION_MODIFIED",
            Self::GitDestinationDiverged => "GIT_DESTINATION_DIVERGED",
            Self::HandoffNotLanded => "HANDOFF_NOT_LANDED",
            Self::HandoffTargetUnsupported => "HANDOFF_TARGET_UNSUPPORTED",
            Self::HandoffBlockModified => "HANDOFF_BLOCK_MODIFIED",
            Self::HandoffMarkerCommitted => "HANDOFF_MARKER_COMMITTED",
            Self::GitConvergeShapeChanged => "GIT_CONVERGE_SHAPE_CHANGED",
            Self::GitConvergeStaleCapture => "GIT_CONVERGE_STALE_CAPTURE",
            Self::GitConvergeInterrupted => "GIT_CONVERGE_INTERRUPTED",
            Self::GitIgnorePolicyConflict => "GIT_IGNORE_POLICY_CONFLICT",
            Self::GitDestinationParentMissing => "GIT_DESTINATION_PARENT_MISSING",
            Self::GitNestStashed => "GIT_NEST_STASHED",
            Self::GitNestDetachedUnreachable => "GIT_NEST_DETACHED_UNREACHABLE",
            Self::GitNestInnerRepository(_) => "GIT_NEST_INNER_REPOSITORY",
            Self::GitNestConversionAttribute(_) => "GIT_NEST_CONVERSION_ATTRIBUTE",
            Self::GitNestPopulatedSubmodule(_) => "GIT_NEST_POPULATED_SUBMODULE",
            Self::GitNestCarrierRefused(_) => "GIT_NEST_CARRIER_REFUSED",
            Self::GitCarryPointerAttached(_) => "GIT_CARRY_POINTER_ATTACHED",
            Self::GitRepositoryNotAtPath => "GIT_REPOSITORY_NOT_AT_PATH",
            Self::GitHavesUnprovable => "GIT_HAVES_UNPROVABLE",
            Self::GitSourcePartialClone => "GIT_SOURCE_PARTIAL_CLONE",
            Self::GitSourceAlternates(_) => "GIT_SOURCE_ALTERNATES",
            Self::GitChildFailed(_) => "GIT_CHILD_FAILED",
            Self::SqliteIntegrityCheckFailed => "SQLITE_INTEGRITY_CHECK_FAILED",
            Self::SqliteUnsupportedValue => "SQLITE_UNSUPPORTED_VALUE",
            Self::SqliteStateChanged => "SQLITE_STATE_CHANGED",
            Self::SqliteBackupFailed(_) => "SQLITE_BACKUP_FAILED",
            Self::SqliteSourceAsRoot => "SQLITE_SOURCE_AS_ROOT",
            Self::SqliteSourceNotOwner => "SQLITE_SOURCE_NOT_OWNER",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::DestinationSpaceInsufficient => "DESTINATION_SPACE_INSUFFICIENT",
            Self::SpaceExhausted(_) => "SPACE_EXHAUSTED",
            Self::DestinationOccupied => "DESTINATION_OCCUPIED",
            Self::DestinationExchangeUnsupported => "DESTINATION_EXCHANGE_UNSUPPORTED",
            Self::SalvageBoundExceeded => "SALVAGE_BOUND_EXCEEDED",
            Self::FrameCodec => "FRAME_CODEC",
            Self::ProtocolStateViolation => "PROTOCOL_STATE_VIOLATION",
            Self::WorkerLost => "WORKER_LOST",
            Self::ReseatDestDiverged => "RESEAT_DEST_DIVERGED",
            Self::ReseatDestContinued => "RESEAT_DEST_CONTINUED",
            Self::ReseatSourceRewritten => "RESEAT_SOURCE_REWRITTEN",
            Self::ReseatLaneUnmapped => "RESEAT_LANE_UNMAPPED",
            Self::ReseatLaneUnresolved => "RESEAT_LANE_UNRESOLVED",
            Self::ReseatRetentionUnsafe => "RESEAT_RETENTION_UNSAFE",
            Self::ReseatIndexConflict => "RESEAT_INDEX_CONFLICT",
            Self::ReseatUnreadableForm => "RESEAT_UNREADABLE_FORM",
            Self::ReseatLandingAbsent => "RESEAT_LANDING_ABSENT",
            Self::ReseatIndexAbsent => "RESEAT_INDEX_ABSENT",
            Self::ReseatIndexRowAbsent => "RESEAT_INDEX_ROW_ABSENT",
            Self::ReseatLaneCollision => "RESEAT_LANE_COLLISION",
            Self::ReseatExtendRaced => "RESEAT_EXTEND_RACED",
            Self::ReseatLocked => "RESEAT_LOCKED",
            Self::ReseatRegularizeRaced => "RESEAT_REGULARIZE_RACED",
            Self::ReseatUndoTargetChanged => "RESEAT_UNDO_TARGET_CHANGED",
            Self::ReseatR0Failed => "RESEAT_R0_FAILED",
            Self::ProbeFailed => "PROBE_FAILED",
            Self::Io(_) => "IO",
        }
    }
}

impl BulkloadRefusal {
    /// Every stable refusal code, in taxonomy order. A closure report uses it
    /// to tell a typed refusal from free text; the unit tests hold it equal to
    /// the set of [`BulkloadRefusal::code`] values.
    pub const CODES: &'static [&'static str] = &[
        "SNAPSHOT_ROOTS_OVERLAP",
        "SOURCE_CHANGED_AFTER_SNAPSHOT",
        "CAPTURE_DRIFTED",
        "CAPTURE_ABSENT",
        "DIGEST_MISMATCH",
        "SEALED_OBJECT_MISSING",
        "RECEIPT_BINDING_INVALID",
        "SCHEMA_MISMATCH",
        "REQUIRED_FIELD_MISSING",
        "FIELD_DOMAIN_VIOLATION",
        "CONTRACT_SELF_INCONSISTENT",
        "CLOSURE_UNACCOUNTED",
        "PATH_NOT_ABSOLUTE",
        "PATH_NOT_PORTABLE",
        "PATH_ESCAPES_ROOT",
        "PATH_DEPTH_EXCEEDED",
        "PATH_TOO_LONG",
        "GIT_UNAVAILABLE",
        "GIT_AUTHORITY_OUTSIDE_ROOT",
        "GIT_AUTHORITY_CHANGED",
        "GIT_INVENTORY_MALFORMED",
        "GIT_INVENTORY_MISSING_PREREQUISITE",
        "GIT_INVENTORY_INTENT_TO_ADD",
        "GIT_INVENTORY_INDEX_ABSENT",
        "GIT_INVENTORY_OVER_CAP",
        "GIT_BARE_CAPTURE_WORKSPACE",
        "GIT_DESTINATION_OCCUPIED",
        "GIT_DESTINATION_MODIFIED",
        "GIT_DESTINATION_DIVERGED",
        "HANDOFF_NOT_LANDED",
        "HANDOFF_TARGET_UNSUPPORTED",
        "HANDOFF_BLOCK_MODIFIED",
        "HANDOFF_MARKER_COMMITTED",
        "GIT_CONVERGE_SHAPE_CHANGED",
        "GIT_CONVERGE_STALE_CAPTURE",
        "GIT_CONVERGE_INTERRUPTED",
        "GIT_IGNORE_POLICY_CONFLICT",
        "GIT_DESTINATION_PARENT_MISSING",
        "GIT_NEST_STASHED",
        "GIT_NEST_DETACHED_UNREACHABLE",
        "GIT_NEST_INNER_REPOSITORY",
        "GIT_NEST_CONVERSION_ATTRIBUTE",
        "GIT_NEST_POPULATED_SUBMODULE",
        "GIT_NEST_CARRIER_REFUSED",
        "GIT_CARRY_POINTER_ATTACHED",
        "GIT_REPOSITORY_NOT_AT_PATH",
        "GIT_HAVES_UNPROVABLE",
        "GIT_SOURCE_PARTIAL_CLONE",
        "GIT_SOURCE_ALTERNATES",
        "GIT_CHILD_FAILED",
        "SQLITE_INTEGRITY_CHECK_FAILED",
        "SQLITE_UNSUPPORTED_VALUE",
        "SQLITE_STATE_CHANGED",
        "SQLITE_BACKUP_FAILED",
        "SQLITE_SOURCE_AS_ROOT",
        "SQLITE_SOURCE_NOT_OWNER",
        "BUDGET_EXCEEDED",
        "DESTINATION_SPACE_INSUFFICIENT",
        "SPACE_EXHAUSTED",
        "DESTINATION_OCCUPIED",
        "DESTINATION_EXCHANGE_UNSUPPORTED",
        "SALVAGE_BOUND_EXCEEDED",
        "FRAME_CODEC",
        "PROTOCOL_STATE_VIOLATION",
        "WORKER_LOST",
        "RESEAT_DEST_DIVERGED",
        "RESEAT_DEST_CONTINUED",
        "RESEAT_SOURCE_REWRITTEN",
        "RESEAT_LANE_UNMAPPED",
        "RESEAT_LANE_UNRESOLVED",
        "RESEAT_RETENTION_UNSAFE",
        "RESEAT_INDEX_CONFLICT",
        "RESEAT_UNREADABLE_FORM",
        "RESEAT_LANDING_ABSENT",
        "RESEAT_INDEX_ABSENT",
        "RESEAT_INDEX_ROW_ABSENT",
        "RESEAT_LANE_COLLISION",
        "RESEAT_EXTEND_RACED",
        "RESEAT_LOCKED",
        "RESEAT_REGULARIZE_RACED",
        "RESEAT_UNDO_TARGET_CHANGED",
        "RESEAT_R0_FAILED",
        "PROBE_FAILED",
        "IO",
    ];

    /// Whether `code` is one of [`BulkloadRefusal::CODES`].
    #[must_use]
    pub fn is_code(code: &str) -> bool {
        Self::CODES.contains(&code)
    }
}

impl fmt::Display for BulkloadRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io(Some(errno)) => write!(f, "IO (errno {errno})"),
            Self::SqliteBackupFailed(Some(code)) => {
                write!(f, "{} (sqlite code {code})", self.code())
            }
            Self::GitChildFailed(class) => {
                write!(f, "{} stderr_class={}", self.code(), class.code())
            }
            // Escaped inside quotes: a path cannot forge a second line.
            Self::GitNestInnerRepository(ref path)
            | Self::GitNestConversionAttribute(ref path)
            | Self::GitNestPopulatedSubmodule(ref path)
            | Self::GitNestCarrierRefused(ref path)
            | Self::GitCarryPointerAttached(ref path)
            | Self::GitSourceAlternates(ref path)
            | Self::SpaceExhausted(Some(ref path)) => {
                write!(f, "{} path=\"{}\"", self.code(), path.escape_ascii())
            }
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for BulkloadRefusal {}

/// What a Git child's stderr says, from a closed set (R-N121). The class is
/// all a refusal, receipt or log line ever carries of a child's stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrClass {
    /// The path is not a repository, or the remote repository is absent.
    NotARepository,
    /// ssh or HTTP authentication, or host-key verification, failed.
    AuthFailed,
    /// The host could not be resolved or reached.
    HostUnreachable,
    /// A connection or operation timed out.
    Timeout,
    /// Git reported a missing, bad or corrupt object.
    BadObject,
    /// A write failed for lack of space or quota (`ENOSPC`, `EDQUOT`; S4,
    /// #220). A v1 child of this class refuses a space code, never
    /// `GIT_CHILD_FAILED`: `SPACE_EXHAUSTED`, which its verb names, or
    /// `DESTINATION_SPACE_INSUFFICIENT` where its verb's preflight charged
    /// the write.
    NoSpace,
    /// Anything else.
    Other,
}

impl StderrClass {
    /// The stable code printed as `stderr_class=`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotARepository => "not_a_repository",
            Self::AuthFailed => "auth_failed",
            Self::HostUnreachable => "host_unreachable",
            Self::Timeout => "timeout",
            Self::BadObject => "bad_object",
            Self::NoSpace => "no_space",
            Self::Other => "other",
        }
    }

    /// Classify raw stderr by the phrases real git and OpenSSH print in the
    /// C locale, which every child runs under (`LC_ALL=C`). The first class
    /// whose pattern matches wins; the order puts timeouts ahead of the
    /// unreachable-host phrases they share a line with. Lines from a shell's
    /// `setlocale` warning are ignored, and "No such file or directory" only
    /// counts after git's or the shell's change-directory failure.
    #[must_use]
    pub fn of(raw: &[u8]) -> Self {
        const PATTERNS: [(StderrClass, &[&str]); 6] = [
            // First: a full disk under any other failure is the cause.
            (
                StderrClass::NoSpace,
                &["no space left on device", "disk quota exceeded"],
            ),
            (
                StderrClass::Timeout,
                &["timed out", "timeout, server", "connection timeout"],
            ),
            (
                StderrClass::HostUnreachable,
                &[
                    "could not resolve hostname",
                    "could not resolve host",
                    "name or service not known",
                    "nodename nor servname provided",
                    "temporary failure in name resolution",
                    "no route to host",
                    "network is unreachable",
                    "connection refused",
                    "connection closed by remote host",
                    "connection reset by peer",
                ],
            ),
            (
                StderrClass::AuthFailed,
                &[
                    "permission denied (publickey",
                    "permission denied, please try again",
                    "authentication failed",
                    "host key verification failed",
                    "could not read username",
                    "could not read password",
                    "too many authentication failures",
                    "no supported authentication methods",
                ],
            ),
            (
                StderrClass::NotARepository,
                &[
                    "not a git repository",
                    "does not appear to be a git repository",
                    "repository not found",
                    "fatal: cannot change to '",
                    ": cd: ",
                ],
            ),
            (
                StderrClass::BadObject,
                &[
                    "bad object",
                    "bad revision",
                    "missing object",
                    "object not found",
                    "is corrupt",
                    "unable to read",
                    "invalid object",
                    "did not receive expected object",
                ],
            ),
        ];
        let text: String = String::from_utf8_lossy(raw)
            .to_lowercase()
            .lines()
            .filter(|line| !line.contains("setlocale"))
            .collect::<Vec<_>>()
            .join("\n");
        PATTERNS
            .iter()
            .find(|(_, phrases)| phrases.iter().any(|phrase| text.contains(phrase)))
            .map_or(Self::Other, |(class, _)| *class)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{BulkloadRefusal, StderrClass};

    /// Codes are the wire identity of a refusal: they must be unique and
    /// `SCREAMING_SNAKE_CASE`.
    #[test]
    // One row per refusal: the list grows with the taxonomy.
    #[allow(clippy::too_many_lines)]
    fn codes_are_unique_and_well_formed() {
        let all = [
            BulkloadRefusal::SnapshotRootsOverlap,
            BulkloadRefusal::SourceChangedAfterSnapshot,
            BulkloadRefusal::CaptureDrifted,
            BulkloadRefusal::CaptureAbsent,
            BulkloadRefusal::DigestMismatch,
            BulkloadRefusal::SealedObjectMissing,
            BulkloadRefusal::ReceiptBindingInvalid,
            BulkloadRefusal::SchemaMismatch,
            BulkloadRefusal::RequiredFieldMissing,
            BulkloadRefusal::FieldDomainViolation,
            BulkloadRefusal::ContractSelfInconsistent,
            BulkloadRefusal::ClosureUnaccounted,
            BulkloadRefusal::PathNotAbsolute,
            BulkloadRefusal::PathNotPortable,
            BulkloadRefusal::PathEscapesRoot,
            BulkloadRefusal::PathDepthExceeded,
            BulkloadRefusal::PathTooLong,
            BulkloadRefusal::GitUnavailable,
            BulkloadRefusal::GitAuthorityOutsideRoot,
            BulkloadRefusal::GitAuthorityChanged,
            BulkloadRefusal::GitInventoryMalformed,
            BulkloadRefusal::GitInventoryMissingPrerequisite,
            BulkloadRefusal::GitInventoryIntentToAdd,
            BulkloadRefusal::GitInventoryIndexAbsent,
            BulkloadRefusal::GitInventoryOverCap,
            BulkloadRefusal::GitBareCaptureWorkspace,
            BulkloadRefusal::GitDestinationOccupied,
            BulkloadRefusal::GitDestinationModified,
            BulkloadRefusal::GitDestinationDiverged,
            BulkloadRefusal::HandoffNotLanded,
            BulkloadRefusal::HandoffTargetUnsupported,
            BulkloadRefusal::HandoffBlockModified,
            BulkloadRefusal::HandoffMarkerCommitted,
            BulkloadRefusal::GitConvergeShapeChanged,
            BulkloadRefusal::GitConvergeStaleCapture,
            BulkloadRefusal::GitConvergeInterrupted,
            BulkloadRefusal::GitIgnorePolicyConflict,
            BulkloadRefusal::GitDestinationParentMissing,
            BulkloadRefusal::GitNestStashed,
            BulkloadRefusal::GitNestDetachedUnreachable,
            BulkloadRefusal::GitNestInnerRepository(Vec::new()),
            BulkloadRefusal::GitNestConversionAttribute(Vec::new()),
            BulkloadRefusal::GitNestPopulatedSubmodule(Vec::new()),
            BulkloadRefusal::GitNestCarrierRefused(Vec::new()),
            BulkloadRefusal::GitCarryPointerAttached(Vec::new()),
            BulkloadRefusal::GitRepositoryNotAtPath,
            BulkloadRefusal::GitHavesUnprovable,
            BulkloadRefusal::GitSourcePartialClone,
            BulkloadRefusal::GitSourceAlternates(Vec::new()),
            BulkloadRefusal::GitChildFailed(StderrClass::Other),
            BulkloadRefusal::SqliteIntegrityCheckFailed,
            BulkloadRefusal::SqliteUnsupportedValue,
            BulkloadRefusal::SqliteStateChanged,
            BulkloadRefusal::SqliteBackupFailed(None),
            BulkloadRefusal::SqliteSourceAsRoot,
            BulkloadRefusal::SqliteSourceNotOwner,
            BulkloadRefusal::BudgetExceeded,
            BulkloadRefusal::DestinationSpaceInsufficient,
            BulkloadRefusal::SpaceExhausted(None),
            BulkloadRefusal::DestinationOccupied,
            BulkloadRefusal::DestinationExchangeUnsupported,
            BulkloadRefusal::SalvageBoundExceeded,
            BulkloadRefusal::FrameCodec,
            BulkloadRefusal::ProtocolStateViolation,
            BulkloadRefusal::WorkerLost,
            BulkloadRefusal::ReseatDestDiverged,
            BulkloadRefusal::ReseatDestContinued,
            BulkloadRefusal::ReseatSourceRewritten,
            BulkloadRefusal::ReseatLaneUnmapped,
            BulkloadRefusal::ReseatLaneUnresolved,
            BulkloadRefusal::ReseatRetentionUnsafe,
            BulkloadRefusal::ReseatIndexConflict,
            BulkloadRefusal::ReseatUnreadableForm,
            BulkloadRefusal::ReseatLandingAbsent,
            BulkloadRefusal::ReseatIndexAbsent,
            BulkloadRefusal::ReseatIndexRowAbsent,
            BulkloadRefusal::ReseatLaneCollision,
            BulkloadRefusal::ReseatExtendRaced,
            BulkloadRefusal::ReseatLocked,
            BulkloadRefusal::ReseatRegularizeRaced,
            BulkloadRefusal::ReseatUndoTargetChanged,
            BulkloadRefusal::ReseatR0Failed,
            BulkloadRefusal::ProbeFailed,
            BulkloadRefusal::Io(None),
        ];

        let mut codes: Vec<&'static str> = all.iter().map(BulkloadRefusal::code).collect();
        let total = codes.len();
        assert!(total >= 15, "taxonomy should carry at least 15 refusals");
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "refusal codes must be unique");

        let mut listed: Vec<&'static str> = BulkloadRefusal::CODES.to_vec();
        listed.sort_unstable();
        assert_eq!(listed, codes, "CODES must list exactly the variant codes");
        assert!(BulkloadRefusal::is_code("DESTINATION_SPACE_INSUFFICIENT"));
        assert!(!BulkloadRefusal::is_code("IO (errno 2)"));

        for code in codes {
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "code {code} is not SCREAMING_SNAKE_CASE"
            );
        }
    }

    #[test]
    fn path_refusals_print_their_path_escaped() {
        assert_eq!(
            BulkloadRefusal::SpaceExhausted(Some(b"/tmp/a\nb".to_vec())).to_string(),
            "SPACE_EXHAUSTED path=\"/tmp/a\\nb\""
        );
        assert_eq!(
            BulkloadRefusal::SpaceExhausted(None).to_string(),
            "SPACE_EXHAUSTED"
        );
        assert_eq!(
            BulkloadRefusal::GitSourceAlternates(b"/s/objects/info/alternates".to_vec())
                .to_string(),
            "GIT_SOURCE_ALTERNATES path=\"/s/objects/info/alternates\""
        );
    }

    #[test]
    fn a_full_disk_classifies_ahead_of_every_other_phrase() {
        assert_eq!(
            StderrClass::of(b"fatal: unable to write: No space left on device"),
            StderrClass::NoSpace
        );
        assert_eq!(
            StderrClass::of(b"error: cannot create file: Disk quota exceeded\nfatal: bad object"),
            StderrClass::NoSpace
        );
        assert_eq!(
            StderrClass::of(b"fatal: bad object HEAD"),
            StderrClass::BadObject
        );
    }

    #[test]
    fn io_refusal_carries_errno() {
        let refusal = BulkloadRefusal::Io(Some(2));
        assert_eq!(refusal.code(), "IO");
        assert_eq!(refusal.to_string(), "IO (errno 2)");
    }
}
