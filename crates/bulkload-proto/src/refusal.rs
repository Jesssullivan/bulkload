//! The bulkload refusal taxonomy.
//!
//! Each variant has a stable machine-readable code, and variants are grouped
//! into families by prefix: `Snapshot*`, `Capture*`, `Digest*`, `Git*`,
//! `Sqlite*`, `Path*`, `Journal*`, `Protocol*` and `Budget*`.
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
    /// A path given as a repository is not that repository's root: Git would
    /// resolve it to an enclosing repository, or to none at all.
    GitRepositoryNotAtPath,
    /// A destination's refs do not prove it holds their history: it is a
    /// partial clone, or shallow at a frontier other than the source's (R-N75).
    GitHavesUnprovable,
    /// A Git destination's object store or common dir is on a filesystem
    /// the ingest does not support: a network filesystem (NFS, SMB, `WebDAV`
    /// and the like), where its `flock` locks cannot be trusted. Ingest
    /// destinations must be local filesystems (operator ruling OI-1001-Q17).
    GitDestinationFilesystemUnsupported,
    /// A Git source is a partial clone (a promisor remote, a partial-clone
    /// filter, `extensions.partialClone`, or a `.promisor` pack in its object
    /// store or an alternate). Reading it could fault in a lazy fetch, so v1
    /// carry refuses it before any other read (S2, OI-1003-Q16).
    GitSourcePartialClone,
    /// A Git child process exited non-zero (WP3). Carries its stderr's class
    /// from the closed [`StderrClass`] set; no byte of the stderr itself is
    /// carried or printed (R-N121).
    GitChildFailed(StderrClass),

    // ---- sqlite -----------------------------------------------------------
    /// `PRAGMA quick_check` or the foreign-key check failed.
    SqliteIntegrityCheckFailed,
    /// The database yielded a value type or magnitude the schema cannot carry.
    SqliteUnsupportedValue,
    /// Shared rows diverged between planning and apply.
    SqliteStateChanged,
    /// The `SQLite` online backup (open, step or finish) failed (WP3).
    /// Carries `SQLite`'s extended result code when `SQLite` reported one.
    SqliteBackupFailed(Option<i32>),
    /// A provider verb that reads a source database was run with effective
    /// uid 0 (S2, OI-1003-Q76). Opened as root, `SQLite` re-applies the
    /// database's ownership to its `-wal` (`fchown`), which moves the
    /// `-wal`'s ctime: a source metadata write no ruling admits. The verb
    /// refuses before it opens anything; run it as the database's owner.
    SqliteSourceAsRoot,

    // ---- journal -----------------------------------------------------------
    /// An existing journal belongs to a different transaction.
    JournalOwnershipConflict,

    // ---- budgets / transport ------------------------------------------------
    /// A capture, row, or spill budget was exceeded.
    BudgetExceeded,
    /// Writing the planned bytes would leave the destination filesystem with
    /// less free space than the configured floor (`--min-free-percent`,
    /// default 25%), or with no room at all (OI-1001-Q2).
    DestinationSpaceInsufficient,
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
            Self::GitIgnorePolicyConflict => "GIT_IGNORE_POLICY_CONFLICT",
            Self::GitDestinationParentMissing => "GIT_DESTINATION_PARENT_MISSING",
            Self::GitNestStashed => "GIT_NEST_STASHED",
            Self::GitNestDetachedUnreachable => "GIT_NEST_DETACHED_UNREACHABLE",
            Self::GitNestInnerRepository(_) => "GIT_NEST_INNER_REPOSITORY",
            Self::GitNestConversionAttribute(_) => "GIT_NEST_CONVERSION_ATTRIBUTE",
            Self::GitNestPopulatedSubmodule(_) => "GIT_NEST_POPULATED_SUBMODULE",
            Self::GitNestCarrierRefused(_) => "GIT_NEST_CARRIER_REFUSED",
            Self::GitRepositoryNotAtPath => "GIT_REPOSITORY_NOT_AT_PATH",
            Self::GitHavesUnprovable => "GIT_HAVES_UNPROVABLE",
            Self::GitDestinationFilesystemUnsupported => "GIT_DESTINATION_FILESYSTEM_UNSUPPORTED",
            Self::GitSourcePartialClone => "GIT_SOURCE_PARTIAL_CLONE",
            Self::GitChildFailed(_) => "GIT_CHILD_FAILED",
            Self::SqliteIntegrityCheckFailed => "SQLITE_INTEGRITY_CHECK_FAILED",
            Self::SqliteUnsupportedValue => "SQLITE_UNSUPPORTED_VALUE",
            Self::SqliteStateChanged => "SQLITE_STATE_CHANGED",
            Self::SqliteBackupFailed(_) => "SQLITE_BACKUP_FAILED",
            Self::SqliteSourceAsRoot => "SQLITE_SOURCE_AS_ROOT",
            Self::JournalOwnershipConflict => "JOURNAL_OWNERSHIP_CONFLICT",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::DestinationSpaceInsufficient => "DESTINATION_SPACE_INSUFFICIENT",
            Self::SalvageBoundExceeded => "SALVAGE_BOUND_EXCEEDED",
            Self::FrameCodec => "FRAME_CODEC",
            Self::ProtocolStateViolation => "PROTOCOL_STATE_VIOLATION",
            Self::WorkerLost => "WORKER_LOST",
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
        "GIT_IGNORE_POLICY_CONFLICT",
        "GIT_DESTINATION_PARENT_MISSING",
        "GIT_NEST_STASHED",
        "GIT_NEST_DETACHED_UNREACHABLE",
        "GIT_NEST_INNER_REPOSITORY",
        "GIT_NEST_CONVERSION_ATTRIBUTE",
        "GIT_NEST_POPULATED_SUBMODULE",
        "GIT_NEST_CARRIER_REFUSED",
        "GIT_REPOSITORY_NOT_AT_PATH",
        "GIT_HAVES_UNPROVABLE",
        "GIT_DESTINATION_FILESYSTEM_UNSUPPORTED",
        "GIT_SOURCE_PARTIAL_CLONE",
        "GIT_CHILD_FAILED",
        "SQLITE_INTEGRITY_CHECK_FAILED",
        "SQLITE_UNSUPPORTED_VALUE",
        "SQLITE_STATE_CHANGED",
        "SQLITE_BACKUP_FAILED",
        "SQLITE_SOURCE_AS_ROOT",
        "JOURNAL_OWNERSHIP_CONFLICT",
        "BUDGET_EXCEEDED",
        "DESTINATION_SPACE_INSUFFICIENT",
        "SALVAGE_BOUND_EXCEEDED",
        "FRAME_CODEC",
        "PROTOCOL_STATE_VIOLATION",
        "WORKER_LOST",
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
            | Self::GitNestCarrierRefused(ref path) => {
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
        const PATTERNS: [(StderrClass, &[&str]); 5] = [
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
    fn codes_are_unique_and_well_formed() {
        let all = [
            BulkloadRefusal::SnapshotRootsOverlap,
            BulkloadRefusal::SourceChangedAfterSnapshot,
            BulkloadRefusal::CaptureDrifted,
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
            BulkloadRefusal::GitIgnorePolicyConflict,
            BulkloadRefusal::GitDestinationParentMissing,
            BulkloadRefusal::GitNestStashed,
            BulkloadRefusal::GitNestDetachedUnreachable,
            BulkloadRefusal::GitNestInnerRepository(Vec::new()),
            BulkloadRefusal::GitNestConversionAttribute(Vec::new()),
            BulkloadRefusal::GitNestPopulatedSubmodule(Vec::new()),
            BulkloadRefusal::GitNestCarrierRefused(Vec::new()),
            BulkloadRefusal::GitRepositoryNotAtPath,
            BulkloadRefusal::GitHavesUnprovable,
            BulkloadRefusal::GitDestinationFilesystemUnsupported,
            BulkloadRefusal::GitSourcePartialClone,
            BulkloadRefusal::GitChildFailed(StderrClass::Other),
            BulkloadRefusal::SqliteIntegrityCheckFailed,
            BulkloadRefusal::SqliteUnsupportedValue,
            BulkloadRefusal::SqliteStateChanged,
            BulkloadRefusal::SqliteBackupFailed(None),
            BulkloadRefusal::SqliteSourceAsRoot,
            BulkloadRefusal::JournalOwnershipConflict,
            BulkloadRefusal::BudgetExceeded,
            BulkloadRefusal::DestinationSpaceInsufficient,
            BulkloadRefusal::SalvageBoundExceeded,
            BulkloadRefusal::FrameCodec,
            BulkloadRefusal::ProtocolStateViolation,
            BulkloadRefusal::WorkerLost,
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
    fn io_refusal_carries_errno() {
        let refusal = BulkloadRefusal::Io(Some(2));
        assert_eq!(refusal.code(), "IO");
        assert_eq!(refusal.to_string(), "IO (errno 2)");
    }
}
