//! The bulkload refusal taxonomy.
//!
//! Each variant has a stable machine-readable code, and variants are grouped
//! into families by prefix: `SnapshotCustody*`, `Capture*`, `Digest*`, `Git*`,
//! `Sqlite*`, `Path*`, `Rollback*` and `Budget*`.
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
    /// The live snapshot custody root could not be established or opened.
    SnapshotCustodyUnavailable,
    /// A payload resolved outside the custody root it claims to live under.
    SnapshotCustodyEscape,
    /// Declared snapshot roots overlap, alias, or are not unique.
    SnapshotRootsOverlap,
    /// The source tree changed after the immutable snapshot was taken.
    SourceChangedAfterSnapshot,
    /// A capture claimed ownership of a published snapshot it cannot prove.
    SnapshotOwnershipUnproven,
    /// A capture recorded drift under its pass, so it does not hold every
    /// seat's bytes; apply refuses it until a later pass extends it clean.
    CaptureDrifted,

    // ---- digest / sealing -------------------------------------------------
    /// A content digest did not match the digest the plan was sealed against.
    DigestMismatch,
    /// A sealed object required by this stage is missing.
    SealedObjectMissing,
    /// A sealed object changed between sealing and apply.
    SealedObjectChanged,
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
    /// The path map is empty, detached from source authority, or unmapped.
    PathMapDetached,
    /// A path left the tree it was resolved against (traversal or symlink).
    PathEscapesRoot,

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
    /// seat is absent from the captured worktree, or it lies in a nested
    /// repository, whose index is not carried (#106). Intent-to-add entries
    /// of a captured checkout are otherwise carried as index custody.
    GitInventoryIntentToAdd,
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

    // ---- sqlite -----------------------------------------------------------
    /// `PRAGMA quick_check` or the foreign-key check failed.
    SqliteIntegrityCheckFailed,
    /// The database yielded a value type or magnitude the schema cannot carry.
    SqliteUnsupportedValue,
    /// Shared rows diverged between planning and apply.
    SqliteStateChanged,

    // ---- rollback / journal ------------------------------------------------
    /// The rollback snapshot required to undo this journal is missing.
    RollbackSnapshotMissing,
    /// Rollback did not reach the exact recorded before-state.
    RollbackEndStateDiverged,
    /// A journal already rolled back cannot be applied again.
    JournalAlreadyRolledBack,
    /// An existing journal belongs to a different transaction.
    JournalOwnershipConflict,

    // ---- budgets / transport ------------------------------------------------
    /// A capture, row, or spill budget was exceeded.
    BudgetExceeded,
    /// Writing the planned bytes would leave the destination filesystem with
    /// less free space than the configured floor (`--min-free-percent`,
    /// default 25%), or with no room at all (OI-1001-Q2).
    DestinationSpaceInsufficient,
    /// The transport authority differs from the captured authority.
    TransportAuthorityMismatch,
    /// The frame could not be encoded or decoded.
    FrameCodec,

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
            Self::SnapshotCustodyUnavailable => "SNAPSHOT_CUSTODY_UNAVAILABLE",
            Self::SnapshotCustodyEscape => "SNAPSHOT_CUSTODY_ESCAPE",
            Self::SnapshotRootsOverlap => "SNAPSHOT_ROOTS_OVERLAP",
            Self::SourceChangedAfterSnapshot => "SOURCE_CHANGED_AFTER_SNAPSHOT",
            Self::SnapshotOwnershipUnproven => "SNAPSHOT_OWNERSHIP_UNPROVEN",
            Self::CaptureDrifted => "CAPTURE_DRIFTED",
            Self::DigestMismatch => "DIGEST_MISMATCH",
            Self::SealedObjectMissing => "SEALED_OBJECT_MISSING",
            Self::SealedObjectChanged => "SEALED_OBJECT_CHANGED",
            Self::ReceiptBindingInvalid => "RECEIPT_BINDING_INVALID",
            Self::SchemaMismatch => "SCHEMA_MISMATCH",
            Self::RequiredFieldMissing => "REQUIRED_FIELD_MISSING",
            Self::FieldDomainViolation => "FIELD_DOMAIN_VIOLATION",
            Self::ContractSelfInconsistent => "CONTRACT_SELF_INCONSISTENT",
            Self::ClosureUnaccounted => "CLOSURE_UNACCOUNTED",
            Self::PathNotAbsolute => "PATH_NOT_ABSOLUTE",
            Self::PathNotPortable => "PATH_NOT_PORTABLE",
            Self::PathMapDetached => "PATH_MAP_DETACHED",
            Self::PathEscapesRoot => "PATH_ESCAPES_ROOT",
            Self::GitUnavailable => "GIT_UNAVAILABLE",
            Self::GitAuthorityOutsideRoot => "GIT_AUTHORITY_OUTSIDE_ROOT",
            Self::GitAuthorityChanged => "GIT_AUTHORITY_CHANGED",
            Self::GitInventoryMalformed => "GIT_INVENTORY_MALFORMED",
            Self::GitInventoryMissingPrerequisite => "GIT_INVENTORY_MISSING_PREREQUISITE",
            Self::GitInventoryIntentToAdd => "GIT_INVENTORY_INTENT_TO_ADD",
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
            Self::SqliteIntegrityCheckFailed => "SQLITE_INTEGRITY_CHECK_FAILED",
            Self::SqliteUnsupportedValue => "SQLITE_UNSUPPORTED_VALUE",
            Self::SqliteStateChanged => "SQLITE_STATE_CHANGED",
            Self::RollbackSnapshotMissing => "ROLLBACK_SNAPSHOT_MISSING",
            Self::RollbackEndStateDiverged => "ROLLBACK_END_STATE_DIVERGED",
            Self::JournalAlreadyRolledBack => "JOURNAL_ALREADY_ROLLED_BACK",
            Self::JournalOwnershipConflict => "JOURNAL_OWNERSHIP_CONFLICT",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::DestinationSpaceInsufficient => "DESTINATION_SPACE_INSUFFICIENT",
            Self::TransportAuthorityMismatch => "TRANSPORT_AUTHORITY_MISMATCH",
            Self::FrameCodec => "FRAME_CODEC",
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
        "SNAPSHOT_CUSTODY_UNAVAILABLE",
        "SNAPSHOT_CUSTODY_ESCAPE",
        "SNAPSHOT_ROOTS_OVERLAP",
        "SOURCE_CHANGED_AFTER_SNAPSHOT",
        "SNAPSHOT_OWNERSHIP_UNPROVEN",
        "CAPTURE_DRIFTED",
        "DIGEST_MISMATCH",
        "SEALED_OBJECT_MISSING",
        "SEALED_OBJECT_CHANGED",
        "RECEIPT_BINDING_INVALID",
        "SCHEMA_MISMATCH",
        "REQUIRED_FIELD_MISSING",
        "FIELD_DOMAIN_VIOLATION",
        "CONTRACT_SELF_INCONSISTENT",
        "CLOSURE_UNACCOUNTED",
        "PATH_NOT_ABSOLUTE",
        "PATH_NOT_PORTABLE",
        "PATH_MAP_DETACHED",
        "PATH_ESCAPES_ROOT",
        "GIT_UNAVAILABLE",
        "GIT_AUTHORITY_OUTSIDE_ROOT",
        "GIT_AUTHORITY_CHANGED",
        "GIT_INVENTORY_MALFORMED",
        "GIT_INVENTORY_MISSING_PREREQUISITE",
        "GIT_INVENTORY_INTENT_TO_ADD",
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
        "SQLITE_INTEGRITY_CHECK_FAILED",
        "SQLITE_UNSUPPORTED_VALUE",
        "SQLITE_STATE_CHANGED",
        "ROLLBACK_SNAPSHOT_MISSING",
        "ROLLBACK_END_STATE_DIVERGED",
        "JOURNAL_ALREADY_ROLLED_BACK",
        "JOURNAL_OWNERSHIP_CONFLICT",
        "BUDGET_EXCEEDED",
        "DESTINATION_SPACE_INSUFFICIENT",
        "TRANSPORT_AUTHORITY_MISMATCH",
        "FRAME_CODEC",
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

impl From<std::io::Error> for BulkloadRefusal {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.raw_os_error())
    }
}

impl From<postcard::Error> for BulkloadRefusal {
    fn from(_: postcard::Error) -> Self {
        Self::FrameCodec
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::BulkloadRefusal;

    /// Codes are the wire identity of a refusal: they must be unique and
    /// `SCREAMING_SNAKE_CASE`.
    #[test]
    fn codes_are_unique_and_well_formed() {
        let all = [
            BulkloadRefusal::SnapshotCustodyUnavailable,
            BulkloadRefusal::SnapshotCustodyEscape,
            BulkloadRefusal::SnapshotRootsOverlap,
            BulkloadRefusal::SourceChangedAfterSnapshot,
            BulkloadRefusal::SnapshotOwnershipUnproven,
            BulkloadRefusal::CaptureDrifted,
            BulkloadRefusal::DigestMismatch,
            BulkloadRefusal::SealedObjectMissing,
            BulkloadRefusal::SealedObjectChanged,
            BulkloadRefusal::ReceiptBindingInvalid,
            BulkloadRefusal::SchemaMismatch,
            BulkloadRefusal::RequiredFieldMissing,
            BulkloadRefusal::FieldDomainViolation,
            BulkloadRefusal::ContractSelfInconsistent,
            BulkloadRefusal::ClosureUnaccounted,
            BulkloadRefusal::PathNotAbsolute,
            BulkloadRefusal::PathNotPortable,
            BulkloadRefusal::PathMapDetached,
            BulkloadRefusal::PathEscapesRoot,
            BulkloadRefusal::GitUnavailable,
            BulkloadRefusal::GitAuthorityOutsideRoot,
            BulkloadRefusal::GitAuthorityChanged,
            BulkloadRefusal::GitInventoryMalformed,
            BulkloadRefusal::GitInventoryMissingPrerequisite,
            BulkloadRefusal::GitInventoryIntentToAdd,
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
            BulkloadRefusal::SqliteIntegrityCheckFailed,
            BulkloadRefusal::SqliteUnsupportedValue,
            BulkloadRefusal::SqliteStateChanged,
            BulkloadRefusal::RollbackSnapshotMissing,
            BulkloadRefusal::RollbackEndStateDiverged,
            BulkloadRefusal::JournalAlreadyRolledBack,
            BulkloadRefusal::JournalOwnershipConflict,
            BulkloadRefusal::BudgetExceeded,
            BulkloadRefusal::DestinationSpaceInsufficient,
            BulkloadRefusal::TransportAuthorityMismatch,
            BulkloadRefusal::FrameCodec,
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
        let refusal = BulkloadRefusal::from(std::io::Error::from_raw_os_error(2));
        assert_eq!(refusal, BulkloadRefusal::Io(Some(2)));
        assert_eq!(refusal.code(), "IO");
        assert_eq!(refusal.to_string(), "IO (errno 2)");
    }
}
