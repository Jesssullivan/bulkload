//! Typed per-item outcomes and refusals (WP3 PR 3, the S4 backbone).
//!
//! An estate verb records one `{item}.outcome` file per planned item. Until
//! WP3 PR 3 that record was the postcard tuple `(source, outcome, reason)`,
//! with the outcome a free string and a refusal only recoverable by parsing
//! the reason text. Closure matched on those strings.
//!
//! The record is now an [`OutcomeRecord`]: the source, a closed [`Outcome`]
//! enum, and the human-readable reason. A refusal is a typed [`Refusal`]
//! with a code, a site and an errno. The persisted form is [`MAGIC`] followed
//! by the postcard encoding, decoded strictly: the whole input must be
//! consumed ([`decode`]), so a record with trailing bytes proves nothing.
//!
//! Old ledgers stay readable. A record without the magic is decoded strictly
//! as the legacy tuple and mapped by [`OutcomeRecord::from_legacy`], which
//! maps every string the legacy writer produced. Anything it cannot map is an
//! [`Unreadable`] value, never a guess.
//!
//! Postcard encodes an enum by variant index, so the order of [`Outcome`]'s
//! variants is part of the on-disk format. `golden_encodings_are_stable`
//! pins it; append new variants, never reorder. Refusal codes are carried as
//! their stable strings ([`BulkloadRefusal::code`]), never as the refusal
//! enum's index, so deleting a refusal variant cannot shift a recorded code.
//!
//! Write time and read time validate a code differently, because the
//! taxonomy shrinks (WP3 PR 1 deleted nine codes; later lanes delete more):
//!
//! - a **writer** ([`Refusal::new`], [`Refusal::of`]) records only a code the
//!   taxonomy holds now ([`BulkloadRefusal::CODES`]);
//! - a **reader** accepts any well-formed code token ([`is_code_token`]). A
//!   record naming a code that has since left the taxonomy still decodes, in
//!   either format, as a refusal that [`Refusal::is_retired`]. Closure reads
//!   it as unaccounted (`refusal-code-retired`): it is not typed, no review
//!   or attestation closes it, and only a verb recording a current outcome
//!   does. It never makes the record unreadable.

use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::BulkloadRefusal;

/// The prefix of a typed outcome record (format 2). A legacy record starts
/// with the varint length of its absolute, hence non-empty, source path, so
/// it never starts with a zero byte.
pub const MAGIC: [u8; 8] = [0x00, b'b', b'l', b'o', b'u', b't', b'c', 0x02];

/// The site a record read from the legacy string form names: the legacy
/// writer recorded no site.
pub const LEGACY_SITE: &str = "outcome::legacy";

/// The largest outcome record read: 1 MiB. A real record is a path, a name
/// and a short reason.
const RECORD_LIMIT: u64 = 1024 * 1024;

/// One item's recorded outcome. The variant order is the on-disk format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// A capture exported and recorded the item clean.
    Captured,
    /// A capture recorded drift under its pass (R-N30).
    CapturedWithDrift,
    /// A capture extended an earlier drifted capture.
    CaptureExtendedFromDrift,
    /// A capture reused the retained capture after its census.
    CaptureReusedAfterCensus,
    /// The source's object store was rewritten under the pass (WP1 PR 4):
    /// drift custody, no capture recorded.
    DeferredWithDrift,
    /// Apply restored the planned workspace.
    WorkspaceRestored,
    /// A re-apply found the workspace journal and did not revalidate it.
    PreviousWorkspaceRestorationNotRevalidated,
    /// Apply imported the item's ref custody.
    RefsImported,
    /// A re-apply found the refs journal; no workspace parity is claimed.
    PreviousRefCustodyNotWorkspaceParity,
    /// `git-repair-missing-index` imported ref custody and created the
    /// missing index (#95).
    IndexRepaired,
    /// The verb refused the item.
    Refused(Refusal),
}

/// Every non-refusal outcome and its legacy string, in variant order.
pub const NAMED: [(Outcome, &str); 10] = [
    (Outcome::Captured, "captured"),
    (Outcome::CapturedWithDrift, "captured-with-drift"),
    (
        Outcome::CaptureExtendedFromDrift,
        "capture-extended-from-drift",
    ),
    (
        Outcome::CaptureReusedAfterCensus,
        "capture-reused-after-census",
    ),
    (Outcome::DeferredWithDrift, "deferred-with-drift"),
    (Outcome::WorkspaceRestored, "workspace-restored"),
    (
        Outcome::PreviousWorkspaceRestorationNotRevalidated,
        "previous-workspace-restoration-not-revalidated",
    ),
    (Outcome::RefsImported, "refs-imported"),
    (
        Outcome::PreviousRefCustodyNotWorkspaceParity,
        "previous-ref-custody-not-workspace-parity",
    ),
    (Outcome::IndexRepaired, "index-repaired"),
];

/// The legacy string of a refusal outcome.
pub const REFUSED: &str = "refused";

impl Outcome {
    /// The outcome's stable name, as receipts and the legacy record print it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Refused(_) => REFUSED,
            named => NAMED
                .iter()
                .find(|(outcome, _)| outcome == named)
                .map_or(REFUSED, |(_, name)| name),
        }
    }

    /// The non-refusal outcome named `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        NAMED
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(outcome, _)| outcome.clone())
    }

    /// The refusal, when this is one.
    #[must_use]
    pub const fn refusal(&self) -> Option<&Refusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            _ => None,
        }
    }
}

/// A typed refusal as an outcome record carries it.
///
/// `code` is a well-formed code token ([`is_code_token`]): one of
/// [`BulkloadRefusal::CODES`] when written, possibly a code retired since
/// when read ([`Refusal::is_retired`]). `site` is the `module::path` that
/// recorded it ([`crate::refuse::is_site`]); `errno` is the OS errno of a
/// bare `IO` refusal and nothing else. A decoded record that breaks any of
/// these refuses to decode.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RefusalFields")]
pub struct Refusal {
    code: String,
    site: String,
    errno: Option<i32>,
}

// The unchecked decode target: the same fields in the same order, so the
// encoding is exactly `Refusal`'s.
#[derive(Deserialize)]
struct RefusalFields {
    code: String,
    site: String,
    errno: Option<i32>,
}

impl TryFrom<RefusalFields> for Refusal {
    type Error = &'static str;

    fn try_from(fields: RefusalFields) -> Result<Self, Self::Error> {
        Self::decoded(&fields.code, &fields.site, fields.errno)
            .ok_or("refusal fields out of domain")
    }
}

impl Refusal {
    /// A refusal record to write: `code` is a code the taxonomy holds now,
    /// `site` is a `module::path`, and only an `IO` refusal names an errno.
    #[must_use]
    pub fn new(code: &str, site: &str, errno: Option<i32>) -> Option<Self> {
        BulkloadRefusal::is_code(code)
            .then(|| Self::decoded(code, site, errno))
            .flatten()
    }

    /// A refusal record as read: as [`Refusal::new`], but `code` is any
    /// well-formed code token, so a code that has left the taxonomy since
    /// the record was written still reads ([`Refusal::is_retired`]).
    #[must_use]
    pub(crate) fn decoded(code: &str, site: &str, errno: Option<i32>) -> Option<Self> {
        (is_code_token(code) && crate::refuse::is_site(site) && (errno.is_none() || code == "IO"))
            .then(|| Self {
                code: code.to_owned(),
                site: site.to_owned(),
                errno,
            })
    }

    /// The record of `refusal`, raised and recorded at `site`.
    #[must_use]
    pub fn of(refusal: &BulkloadRefusal, site: &'static str) -> Self {
        debug_assert!(crate::refuse::is_site(site), "site {site:?}");
        Self {
            code: refusal.code().to_owned(),
            site: site.to_owned(),
            errno: match refusal {
                BulkloadRefusal::Io(errno) => *errno,
                _ => None,
            },
        }
    }

    /// The refusal a legacy receipt reason names: the refusal's `Display`,
    /// its code first. `None` when the reason's first word is no code token
    /// ([`is_code_token`]), or is not a bare `IO`'s exact `Display` (`IO`,
    /// `IO (errno N)`). A code retired since the record was written maps
    /// like any other ([`Refusal::is_retired`]).
    #[must_use]
    pub fn from_display(reason: &str) -> Option<Self> {
        let code = reason.split_whitespace().next()?;
        if !is_code_token(code) {
            return None;
        }
        let errno = if code == "IO" {
            match reason {
                "IO" => None,
                _ => Some(
                    reason
                        .strip_prefix("IO (errno ")?
                        .strip_suffix(')')?
                        .parse::<i32>()
                        .ok()?,
                ),
            }
        } else {
            None
        };
        Self::decoded(code, LEGACY_SITE, errno)
    }

    /// The stable refusal code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The `module::path` that recorded the refusal.
    #[must_use]
    pub fn site(&self) -> &str {
        &self.site
    }

    /// The OS errno of a bare `IO` refusal.
    #[must_use]
    pub const fn errno(&self) -> Option<i32> {
        self.errno
    }

    /// Whether the code names a cause the taxonomy holds now. A bare `IO` or
    /// `FRAME_CODEC` does not, and neither does a retired code: none of them
    /// closes an item, and none can be dispositioned (S4).
    #[must_use]
    pub fn is_typed(&self) -> bool {
        is_typed_code(&self.code)
    }

    /// Whether the code has left the taxonomy since the record was written.
    #[must_use]
    pub fn is_retired(&self) -> bool {
        !BulkloadRefusal::is_code(&self.code)
    }
}

/// The longest code token read. The longest code today is 38 bytes.
const CODE_TOKEN_LIMIT: usize = 64;

/// Whether `code` has the shape of a taxonomy code.
///
/// That is `[A-Z][A-Z0-9_]*`, at most 64 bytes. Every code the taxonomy holds
/// or ever held has it; free text (`disk went away`) does not. Readers check
/// this shape, not membership in today's [`BulkloadRefusal::CODES`].
#[must_use]
pub fn is_code_token(code: &str) -> bool {
    let mut bytes = code.bytes();
    code.len() <= CODE_TOKEN_LIMIT
        && bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `code` is a code the taxonomy holds now that names a cause: not
/// `IO`, not `FRAME_CODEC`, not a retired code and not free text (S4: a bare
/// IO never counts).
#[must_use]
pub fn is_typed_code(code: &str) -> bool {
    BulkloadRefusal::is_code(code) && code != "IO" && code != "FRAME_CODEC"
}

/// One `{item}.outcome` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeRecord {
    /// The plan item's source, as the verb read it.
    pub source: PathBuf,
    pub outcome: Outcome,
    /// The human-readable detail: the refusal's `Display` (with any escaped
    /// path), or the receipt's counts (`drift=N nested=M`).
    pub reason: Option<String>,
}

/// Why an `{item}.outcome` record that exists proves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// It could not be read, or does not decode strictly in either format.
    Codec,
    /// A legacy record names an outcome no writer produced.
    OutcomeUnknown,
    /// A legacy `refused` record whose reason names no code token at all.
    RefusalUntyped,
}

impl Unreadable {
    /// The closure report's `unaccounted_reason`.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Codec => "outcome-record-unreadable",
            Self::OutcomeUnknown => "outcome-unknown",
            Self::RefusalUntyped => "refusal-untyped",
        }
    }
}

impl OutcomeRecord {
    /// The record of a receipt: `outcome` is the receipt's name, `refusal`
    /// the refusal it carries, recorded at `site`.
    ///
    /// # Errors
    /// `CONTRACT_SELF_INCONSISTENT` when the name is no outcome, or a
    /// `refused` receipt carries no refusal (or another name carries one).
    pub fn of_receipt(
        source: PathBuf,
        outcome: &str,
        refusal: Option<&BulkloadRefusal>,
        reason: Option<String>,
        site: &'static str,
    ) -> crate::Result<Self> {
        let outcome = match (outcome, refusal) {
            (REFUSED, Some(refusal)) => Outcome::Refused(Refusal::of(refusal, site)),
            (name, None) => {
                Outcome::named(name).ok_or(BulkloadRefusal::ContractSelfInconsistent)?
            }
            (_, Some(_)) => return Err(BulkloadRefusal::ContractSelfInconsistent),
        };
        Ok(Self {
            source,
            outcome,
            reason,
        })
    }

    /// Map a legacy `(source, outcome, reason)` record. Every outcome string
    /// the legacy writer produced maps to its [`Outcome`]; a `refused` record
    /// maps through its reason, the refusal's `Display`.
    ///
    /// # Errors
    /// [`Unreadable::OutcomeUnknown`] for a string no writer produced, and
    /// [`Unreadable::RefusalUntyped`] for a `refused` reason that names no
    /// refusal code.
    pub fn from_legacy(
        source: PathBuf,
        outcome: &str,
        reason: Option<String>,
    ) -> Result<Self, Unreadable> {
        let outcome = if outcome == REFUSED {
            Outcome::Refused(
                reason
                    .as_deref()
                    .and_then(Refusal::from_display)
                    .ok_or(Unreadable::RefusalUntyped)?,
            )
        } else {
            Outcome::named(outcome).ok_or(Unreadable::OutcomeUnknown)?
        };
        Ok(Self {
            source,
            outcome,
            reason,
        })
    }

    /// The persisted form: [`MAGIC`] then the record. Pass it to a postcard
    /// writer as is.
    #[must_use]
    pub const fn persisted(&self) -> ([u8; 8], &Self) {
        (MAGIC, self)
    }

    /// The persisted bytes.
    ///
    /// # Errors
    /// `FRAME_CODEC` for a record postcard cannot encode (a non-UTF-8 path).
    pub fn encode(&self) -> crate::Result<Vec<u8>> {
        postcard::to_allocvec(&self.persisted()).map_err(|_| BulkloadRefusal::FrameCodec)
    }
}

/// Decode `bytes` as exactly one `T`; anything left over refuses.
pub(crate) fn strict<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    match postcard::take_from_bytes::<T>(bytes) {
        Ok((value, [])) => Some(value),
        _ => None,
    }
}

/// Decode a persisted record in either format, strictly.
///
/// # Errors
/// [`Unreadable`]: the bytes decode as neither format exactly, or a legacy
/// record names an outcome or refusal [`OutcomeRecord::from_legacy`] cannot
/// map.
pub fn decode(bytes: &[u8]) -> Result<OutcomeRecord, Unreadable> {
    if let Some(body) = bytes.strip_prefix(&MAGIC) {
        return strict::<OutcomeRecord>(body).ok_or(Unreadable::Codec);
    }
    let (source, outcome, reason) =
        strict::<(PathBuf, String, Option<String>)>(bytes).ok_or(Unreadable::Codec)?;
    OutcomeRecord::from_legacy(source, &outcome, reason)
}

/// Read and decode an `{item}.outcome` record: no symlink is followed, and
/// a record over 1 MiB is unreadable.
///
/// # Errors
/// As [`decode`]; a read that fails is [`Unreadable::Codec`].
pub fn read_record(path: &Path) -> Result<OutcomeRecord, Unreadable> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Unreadable::Codec)?;
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unreadable::Codec)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(Unreadable::Codec);
    }
    decode(&bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::test_support::prop_config;
    use bulkload_proto::StderrClass;
    use proptest::prelude::*;

    // Every refusal a receipt can carry whose `Display` has a payload, with
    // that payload drawn: the legacy reason of each must map back to the
    // refusal's code (and errno).
    fn payload_refusal() -> impl Strategy<Value = BulkloadRefusal> {
        let path = proptest::collection::vec(any::<u8>(), 0..24);
        prop_oneof![
            proptest::option::of(any::<i32>()).prop_map(BulkloadRefusal::Io),
            proptest::option::of(any::<i32>()).prop_map(BulkloadRefusal::SqliteBackupFailed),
            path.clone()
                .prop_map(BulkloadRefusal::GitNestInnerRepository),
            path.clone()
                .prop_map(BulkloadRefusal::GitNestConversionAttribute),
            path.clone()
                .prop_map(BulkloadRefusal::GitNestPopulatedSubmodule),
            path.clone()
                .prop_map(BulkloadRefusal::GitNestCarrierRefused),
            path.clone().prop_map(BulkloadRefusal::GitSourceAlternates),
            proptest::option::of(path).prop_map(BulkloadRefusal::SpaceExhausted),
            prop_oneof![
                Just(StderrClass::NotARepository),
                Just(StderrClass::AuthFailed),
                Just(StderrClass::HostUnreachable),
                Just(StderrClass::Timeout),
                Just(StderrClass::BadObject),
                Just(StderrClass::NoSpace),
                Just(StderrClass::Other),
            ]
            .prop_map(BulkloadRefusal::GitChildFailed),
        ]
    }

    fn site() -> impl Strategy<Value = String> {
        proptest::collection::vec("[a-z][a-z0-9_]{0,10}", 1..4).prop_map(|parts| parts.join("::"))
    }

    // Any valid typed refusal record: every taxonomy code, any site, and an
    // errno only on IO.
    fn refusal() -> impl Strategy<Value = Refusal> {
        (
            0..BulkloadRefusal::CODES.len(),
            site(),
            proptest::option::of(any::<i32>()),
        )
            .prop_map(|(index, site, errno)| {
                let code = BulkloadRefusal::CODES[index];
                Refusal::new(code, &site, errno.filter(|_| code == "IO")).unwrap()
            })
    }

    fn outcome() -> impl Strategy<Value = Outcome> {
        prop_oneof![
            (0..NAMED.len()).prop_map(|index| NAMED[index].0.clone()),
            refusal().prop_map(Outcome::Refused),
        ]
    }

    fn record() -> impl Strategy<Value = OutcomeRecord> {
        (
            "/[a-z0-9/ ._-]{0,40}",
            outcome(),
            proptest::option::of("[ -~]{0,40}"),
        )
            .prop_map(|(source, outcome, reason)| OutcomeRecord {
                source: PathBuf::from(source),
                outcome,
                reason,
            })
    }

    fn legacy(source: &str, outcome: &str, reason: Option<&str>) -> Vec<u8> {
        postcard::to_allocvec(&(
            PathBuf::from(source),
            outcome.to_owned(),
            reason.map(str::to_owned),
        ))
        .unwrap()
    }

    proptest! {
        #![proptest_config(prop_config(256))]

        /// P72 OUTCOME-ROUNDTRIP (typed leg): every Outcome and Refusal
        /// variant round-trips through the persisted postcard form, and the
        /// decode is strict: one byte more or one byte less never decodes
        /// to a record.
        #[test]
        fn p72_typed_records_round_trip_strictly(record in record(), extra in any::<u8>()) {
            let bytes = record.encode().unwrap();
            prop_assert_eq!(decode(&bytes), Ok(record.clone()));
            let mut longer = bytes.clone();
            longer.push(extra);
            prop_assert_eq!(decode(&longer), Err(Unreadable::Codec));
            prop_assert!(decode(&bytes[..bytes.len() - 1]).is_err());
            // The legacy writer's form of the same record maps to the same
            // outcome; only the site of a refusal is lost (it was never
            // written).
            let reason = match &record.outcome {
                Outcome::Refused(refusal) => Some(refusal.errno().map_or_else(
                    || refusal.code().to_owned(),
                    |errno| format!("IO (errno {errno})"),
                )),
                _ => record.reason.clone(),
            };
            let old = decode(&legacy(
                &record.source.to_string_lossy(),
                record.outcome.name(),
                reason.as_deref(),
            ))
            .unwrap();
            match (&old.outcome, &record.outcome) {
                (Outcome::Refused(old), Outcome::Refused(new)) => {
                    prop_assert_eq!(old.code(), new.code());
                    prop_assert_eq!(old.errno(), new.errno());
                    prop_assert_eq!(old.site(), LEGACY_SITE);
                }
                (old, new) => prop_assert_eq!(old, new),
            }
        }

        /// P72 OUTCOME-ROUNDTRIP (legacy leg): the legacy reader maps every
        /// refusal `Display` the legacy writer recorded, payload and all, to
        /// the refusal's code and errno, and a receipt's typed record agrees.
        #[test]
        fn p72_legacy_refusal_reasons_map(refused in payload_refusal()) {
            let text = refused.to_string();
            let old = decode(&legacy("/src/a", REFUSED, Some(&text))).unwrap();
            let typed = OutcomeRecord::of_receipt(
                PathBuf::from("/src/a"),
                REFUSED,
                Some(&refused),
                Some(text.clone()),
                LEGACY_SITE,
            )
            .unwrap();
            prop_assert_eq!(&old, &typed);
            prop_assert_eq!(old.reason.as_deref(), Some(text.as_str()));
        }
    }

    /// P72 (legacy leg, exhaustive): every outcome string and every refusal
    /// code the legacy writer could record maps; strings it never wrote are
    /// unreadable, not guessed.
    #[test]
    fn p72_every_legacy_string_maps() {
        for (outcome, name) in &NAMED {
            let old = decode(&legacy("/src/a", name, Some("drift=2 nested=1"))).unwrap();
            assert_eq!(&old.outcome, outcome);
            assert_eq!(old.outcome.name(), *name);
            assert_eq!(old.reason.as_deref(), Some("drift=2 nested=1"));
        }
        for code in BulkloadRefusal::CODES {
            let old = decode(&legacy("/src/a", REFUSED, Some(code))).unwrap();
            let refusal = old.outcome.refusal().unwrap();
            assert_eq!((refusal.code(), refusal.errno()), (*code, None));
            assert_eq!(refusal.is_typed(), *code != "IO" && *code != "FRAME_CODEC");
        }
        assert_eq!(
            decode(&legacy("/src/a", REFUSED, Some("IO (errno 13)")))
                .unwrap()
                .outcome
                .refusal()
                .unwrap()
                .errno(),
            Some(13)
        );
        for (outcome, reason, why) in [
            ("finished", None, Unreadable::OutcomeUnknown),
            ("Refused", Some("IO"), Unreadable::OutcomeUnknown),
            (REFUSED, None, Unreadable::RefusalUntyped),
            (REFUSED, Some("disk went away"), Unreadable::RefusalUntyped),
            (REFUSED, Some("IO errno 2"), Unreadable::RefusalUntyped),
            (REFUSED, Some("IO (errno x)"), Unreadable::RefusalUntyped),
            (REFUSED, Some(""), Unreadable::RefusalUntyped),
        ] {
            assert_eq!(
                decode(&legacy("/src/a", outcome, reason)),
                Err(why),
                "{outcome} {reason:?}"
            );
        }
        // Trailing bytes after a legacy tuple are not a legacy record.
        let mut longer = legacy("/src/a", "captured", None);
        longer.push(0);
        assert_eq!(decode(&longer), Err(Unreadable::Codec));
        assert_eq!(decode(&[]), Err(Unreadable::Codec));
    }

    /// The checked-in legacy fixture (written by the pre-WP3-PR3 writer, one
    /// record per outcome string plus refusals) reads through the legacy
    /// reader to the expected typed records.
    #[test]
    fn checked_in_legacy_fixture_reads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/outcome-legacy");
        let expected: Vec<(&str, Outcome, Option<&str>)> = vec![
            ("captured", Outcome::Captured, None),
            (
                "captured-with-drift",
                Outcome::CapturedWithDrift,
                Some("drift=3"),
            ),
            (
                "capture-extended-from-drift",
                Outcome::CaptureExtendedFromDrift,
                None,
            ),
            (
                "capture-reused-after-census",
                Outcome::CaptureReusedAfterCensus,
                Some("nested=1"),
            ),
            (
                "deferred-with-drift",
                Outcome::DeferredWithDrift,
                Some("drift=1"),
            ),
            ("workspace-restored", Outcome::WorkspaceRestored, None),
            (
                "previous-workspace-restoration-not-revalidated",
                Outcome::PreviousWorkspaceRestorationNotRevalidated,
                None,
            ),
            ("refs-imported", Outcome::RefsImported, None),
            (
                "previous-ref-custody-not-workspace-parity",
                Outcome::PreviousRefCustodyNotWorkspaceParity,
                None,
            ),
            ("index-repaired", Outcome::IndexRepaired, None),
            (
                "refused-typed",
                Outcome::Refused(Refusal::new("GIT_NEST_STASHED", LEGACY_SITE, None).unwrap()),
                Some("GIT_NEST_STASHED"),
            ),
            (
                "refused-path",
                Outcome::Refused(
                    Refusal::new("GIT_NEST_INNER_REPOSITORY", LEGACY_SITE, None).unwrap(),
                ),
                Some("GIT_NEST_INNER_REPOSITORY path=\"a/b\\n\""),
            ),
            (
                "refused-io",
                Outcome::Refused(Refusal::new("IO", LEGACY_SITE, Some(2)).unwrap()),
                Some("IO (errno 2)"),
            ),
        ];
        for (name, outcome, reason) in expected {
            let path = root.join(format!("{name}.outcome"));
            // The fixture is byte for byte what the legacy writer wrote:
            // postcard of `(&source, outcome, &reason)`, no magic.
            assert_eq!(
                std::fs::read(&path).unwrap(),
                postcard::to_allocvec(&(
                    &PathBuf::from("/src/legacy"),
                    outcome.name(),
                    &reason.map(str::to_owned)
                ))
                .unwrap(),
                "{name}"
            );
            let record = read_record(&path).unwrap();
            assert_eq!(
                record,
                OutcomeRecord {
                    source: PathBuf::from("/src/legacy"),
                    outcome,
                    reason: reason.map(str::to_owned),
                },
                "{name}"
            );
        }
        assert_eq!(
            read_record(&root.join("refused-untyped.outcome")),
            Err(Unreadable::RefusalUntyped)
        );
        assert_eq!(
            read_record(&root.join("truncated.outcome")),
            Err(Unreadable::Codec)
        );
    }

    /// The on-disk format: postcard's variant index of each outcome, and one
    /// whole typed refusal record, byte for byte. Reordering a variant or a
    /// field fails here.
    #[test]
    fn golden_encodings_are_stable() {
        for (index, (outcome, _)) in NAMED.iter().enumerate() {
            assert_eq!(
                postcard::to_allocvec(outcome).unwrap(),
                vec![u8::try_from(index).unwrap()]
            );
        }
        let record = OutcomeRecord {
            source: PathBuf::from("/s"),
            outcome: Outcome::Refused(Refusal::new("IO", "estate::apply", Some(2)).unwrap()),
            reason: None,
        };
        let mut expected = MAGIC.to_vec();
        expected.extend_from_slice(&[2, b'/', b's', 10, 2, b'I', b'O', 13]);
        expected.extend_from_slice(b"estate::apply");
        expected.extend_from_slice(&[1, 4, 0]);
        assert_eq!(record.encode().unwrap(), expected);
    }

    /// Read time does not depend on today's taxonomy: a record naming a code
    /// absent from `CODES` (one a later lane deleted) decodes in both
    /// formats as a retired refusal. It is not typed, a writer cannot record
    /// it, and it is never `outcome-record-unreadable`.
    #[test]
    fn a_retired_code_still_decodes_and_is_not_typed() {
        let code = "JOURNAL_RETIRED_FOR_THIS_TEST";
        assert!(!BulkloadRefusal::is_code(code));
        assert!(Refusal::new(code, "estate::apply", None).is_none());
        let retired = Refusal::decoded(code, "estate::apply", None).unwrap();
        assert!(retired.is_retired() && !retired.is_typed());
        let record = OutcomeRecord {
            source: PathBuf::from("/s"),
            outcome: Outcome::Refused(retired),
            reason: Some(format!("{code} path=\"x\"")),
        };
        assert_eq!(decode(&record.encode().unwrap()), Ok(record.clone()));
        let old = decode(&legacy("/s", REFUSED, record.reason.as_deref())).unwrap();
        let refusal = old.outcome.refusal().unwrap();
        assert_eq!((refusal.code(), refusal.site()), (code, LEGACY_SITE));
        assert!(refusal.is_retired() && !refusal.is_typed());
        // A current code is not retired, typed or not.
        for current in ["IO", "FRAME_CODEC", "GIT_NEST_STASHED"] {
            let refusal = Refusal::new(current, "estate::apply", None).unwrap();
            assert!(!refusal.is_retired());
            assert_eq!(refusal.is_typed(), current == "GIT_NEST_STASHED");
        }
    }

    #[test]
    fn refusal_fields_are_closed() {
        assert!(Refusal::new("IO", "estate::apply", Some(5)).is_some());
        assert!(Refusal::new("GIT_NEST_STASHED", "estate::apply", None).is_some());
        // An errno only on IO; a known code; a module path site.
        assert!(Refusal::new("GIT_NEST_STASHED", "estate::apply", Some(5)).is_none());
        assert!(Refusal::new("NOT_A_CODE", "estate::apply", None).is_none());
        assert!(Refusal::new("IO", "Estate apply", None).is_none());
        // A decoded refusal is checked for shape: free text for a code, a
        // site that is no module path, or an errno off IO does not decode.
        for (code, site, errno) in [
            ("not a code", "estate::apply", None),
            ("", "estate::apply", None),
            ("lowercase", "estate::apply", None),
            ("9LIVES", "estate::apply", None),
            ("GIT_NEST_STASHED", "Estate apply", None),
            ("GIT_NEST_STASHED", "estate::apply", Some(5)),
            ("RETIRED_CODE", "estate::apply", Some(5)),
        ] {
            let forged = OutcomeRecord {
                source: PathBuf::from("/s"),
                outcome: Outcome::Refused(Refusal {
                    code: code.into(),
                    site: site.into(),
                    errno,
                }),
                reason: None,
            };
            assert_eq!(
                decode(&forged.encode().unwrap()),
                Err(Unreadable::Codec),
                "{code:?} {site:?} {errno:?}"
            );
        }
        let long = "A".repeat(CODE_TOKEN_LIMIT + 1);
        assert!(!is_code_token(&long) && is_code_token(&long[1..]));
        for code in BulkloadRefusal::CODES {
            assert!(is_code_token(code), "{code}");
        }
        // A receipt's name and refusal must agree.
        assert_eq!(
            OutcomeRecord::of_receipt(PathBuf::new(), REFUSED, None, None, "estate::apply"),
            Err(BulkloadRefusal::ContractSelfInconsistent)
        );
        assert_eq!(
            OutcomeRecord::of_receipt(
                PathBuf::new(),
                "captured",
                Some(&BulkloadRefusal::CaptureDrifted),
                None,
                "estate::apply"
            ),
            Err(BulkloadRefusal::ContractSelfInconsistent)
        );
        assert_eq!(
            OutcomeRecord::of_receipt(PathBuf::new(), "finished", None, None, "estate::apply"),
            Err(BulkloadRefusal::ContractSelfInconsistent)
        );
        assert!(crate::refuse::is_site(LEGACY_SITE));
    }
}
