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
    /// A git inventory (index, refs, worktrees) is malformed.
    GitInventoryMalformed,
    /// The git destination already exists or is non-empty.
    GitDestinationOccupied,
    /// Captured and destination Git ignore policies differ.
    GitIgnorePolicyConflict,
    /// A path given as a repository is not that repository's root: Git would
    /// resolve it to an enclosing repository, or to none at all.
    GitRepositoryNotAtPath,
    /// A destination's refs do not prove it holds their history: it is a
    /// partial clone, or shallow at a frontier other than the source's (R-N75).
    GitHavesUnprovable,

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
            Self::PathNotAbsolute => "PATH_NOT_ABSOLUTE",
            Self::PathNotPortable => "PATH_NOT_PORTABLE",
            Self::PathMapDetached => "PATH_MAP_DETACHED",
            Self::PathEscapesRoot => "PATH_ESCAPES_ROOT",
            Self::GitUnavailable => "GIT_UNAVAILABLE",
            Self::GitAuthorityOutsideRoot => "GIT_AUTHORITY_OUTSIDE_ROOT",
            Self::GitAuthorityChanged => "GIT_AUTHORITY_CHANGED",
            Self::GitInventoryMalformed => "GIT_INVENTORY_MALFORMED",
            Self::GitDestinationOccupied => "GIT_DESTINATION_OCCUPIED",
            Self::GitIgnorePolicyConflict => "GIT_IGNORE_POLICY_CONFLICT",
            Self::GitRepositoryNotAtPath => "GIT_REPOSITORY_NOT_AT_PATH",
            Self::GitHavesUnprovable => "GIT_HAVES_UNPROVABLE",
            Self::SqliteIntegrityCheckFailed => "SQLITE_INTEGRITY_CHECK_FAILED",
            Self::SqliteUnsupportedValue => "SQLITE_UNSUPPORTED_VALUE",
            Self::SqliteStateChanged => "SQLITE_STATE_CHANGED",
            Self::RollbackSnapshotMissing => "ROLLBACK_SNAPSHOT_MISSING",
            Self::RollbackEndStateDiverged => "ROLLBACK_END_STATE_DIVERGED",
            Self::JournalAlreadyRolledBack => "JOURNAL_ALREADY_ROLLED_BACK",
            Self::JournalOwnershipConflict => "JOURNAL_OWNERSHIP_CONFLICT",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::TransportAuthorityMismatch => "TRANSPORT_AUTHORITY_MISMATCH",
            Self::FrameCodec => "FRAME_CODEC",
            Self::ProbeFailed => "PROBE_FAILED",
            Self::Io(_) => "IO",
        }
    }
}

impl fmt::Display for BulkloadRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io(Some(errno)) => write!(f, "IO (errno {errno})"),
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
            BulkloadRefusal::PathNotAbsolute,
            BulkloadRefusal::PathNotPortable,
            BulkloadRefusal::PathMapDetached,
            BulkloadRefusal::PathEscapesRoot,
            BulkloadRefusal::GitUnavailable,
            BulkloadRefusal::GitAuthorityOutsideRoot,
            BulkloadRefusal::GitAuthorityChanged,
            BulkloadRefusal::GitInventoryMalformed,
            BulkloadRefusal::GitDestinationOccupied,
            BulkloadRefusal::GitIgnorePolicyConflict,
            BulkloadRefusal::GitRepositoryNotAtPath,
            BulkloadRefusal::GitHavesUnprovable,
            BulkloadRefusal::SqliteIntegrityCheckFailed,
            BulkloadRefusal::SqliteUnsupportedValue,
            BulkloadRefusal::SqliteStateChanged,
            BulkloadRefusal::RollbackSnapshotMissing,
            BulkloadRefusal::RollbackEndStateDiverged,
            BulkloadRefusal::JournalAlreadyRolledBack,
            BulkloadRefusal::JournalOwnershipConflict,
            BulkloadRefusal::BudgetExceeded,
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
