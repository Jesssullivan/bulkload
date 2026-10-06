//! The disposition ledger (WP3 PR 3, S4, OI-1003-Q1).
//!
//! S4 counts a run complete only when every typed refusal carries an
//! operator-reviewed disposition: accept, re-carry or abandon. This ledger
//! holds those reviews. It is bound to one plan and one SOURCE label, like
//! the attestation ledger (#133), and holds two kinds of row:
//!
//! - an **item** row: one planned item's refusal with one code;
//! - a **standing-policy** row: every refusal with one code, for any item.
//!
//! Each row names its reviewer and date. Rows are only appended; for an
//! (item, code) the latest item row wins, else the latest policy row for the
//! code. A bare `IO` or `FRAME_CODEC` names no cause and can never be
//! dispositioned: no row may name either, the writer refuses one, and a
//! ledger holding one does not decode.
//!
//! The ledger is postcard after [`MAGIC`], decoded strictly. `closure-dispose`
//! appends a row under an exclusive lock with the same durability order as
//! the estate records (write, flush, rename, flush the directory).

use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::counters::CountedSync as _;
use crate::outcome::is_typed_code;
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The prefix of a disposition ledger (`bulkload.dispositions.v1`).
pub const MAGIC: [u8; 8] = [0x00, b'b', b'l', b'd', b'i', b's', b'p', 0x01];

/// The ledger's schema name, as the closure report prints it.
pub const SCHEMA: &str = "bulkload.dispositions.v1";

/// The largest disposition ledger read: 16 MiB, as for a plan.
const LEDGER_LIMIT: u64 = 16 * 1024 * 1024;

/// The longest reviewer name.
const REVIEWER_LIMIT: usize = 128;

/// An operator's decision on a typed refusal (S4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Decision {
    /// The refusal is the right end state for the item.
    Accept,
    /// The item is to be carried again once its cause is fixed.
    ReCarry,
    /// The item is deliberately not carried.
    Abandon,
}

impl Decision {
    /// The decision's stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::ReCarry => "re-carry",
            Self::Abandon => "abandon",
        }
    }

    /// The decision named `name`.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for any other word.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "accept" => Ok(Self::Accept),
            "re-carry" => Ok(Self::ReCarry),
            "abandon" => Ok(Self::Abandon),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        }
    }
}

/// What a row disposes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    /// One planned item's refusal, by item identity (64 lowercase hex).
    Item(String),
    /// A standing policy: every refusal with the row's code.
    Policy,
}

/// One review.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RowFields")]
pub struct Row {
    scope: Scope,
    code: String,
    decision: Decision,
    reviewer: String,
    date: String,
}

// The unchecked decode target, field for field.
#[derive(Deserialize)]
struct RowFields {
    scope: Scope,
    code: String,
    decision: Decision,
    reviewer: String,
    date: String,
}

impl TryFrom<RowFields> for Row {
    type Error = &'static str;

    fn try_from(fields: RowFields) -> std::result::Result<Self, Self::Error> {
        Self::new(
            fields.scope,
            &fields.code,
            fields.decision,
            &fields.reviewer,
            &fields.date,
        )
        .map_err(|_| "disposition row out of domain")
    }
}

impl Row {
    /// A review row.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` when the code is not a typed refusal code
    /// (a bare `IO` or `FRAME_CODEC` can never be dispositioned), the item is
    /// not 64 lowercase hex, the reviewer is empty, over 128 bytes or holds a
    /// control character, or the date is not a calendar `YYYY-MM-DD`.
    pub fn new(
        scope: Scope,
        code: &str,
        decision: Decision,
        reviewer: &str,
        date: &str,
    ) -> Result<Self> {
        let item_ok = match &scope {
            Scope::Item(item) => {
                item.len() == 64
                    && item
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }
            Scope::Policy => true,
        };
        let reviewer_ok = !reviewer.trim().is_empty()
            && reviewer.len() <= REVIEWER_LIMIT
            && !reviewer.chars().any(char::is_control);
        if !(is_typed_code(code) && item_ok && reviewer_ok && is_date(date)) {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        Ok(Self {
            scope,
            code: code.to_owned(),
            decision,
            reviewer: reviewer.to_owned(),
            date: date.to_owned(),
        })
    }

    #[must_use]
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub const fn decision(&self) -> Decision {
        self.decision
    }

    #[must_use]
    pub fn reviewer(&self) -> &str {
        &self.reviewer
    }

    #[must_use]
    pub fn date(&self) -> &str {
        &self.date
    }

    /// `item` or `policy`, as the closure report prints a review's basis.
    #[must_use]
    pub const fn basis(&self) -> &'static str {
        match self.scope {
            Scope::Item(_) => "item",
            Scope::Policy => "policy",
        }
    }
}

// A calendar date `YYYY-MM-DD` (proleptic Gregorian, years 1970-9999).
fn is_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = bytes.get(range)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year >= 1970 && (1..=days).contains(&day)
}

// The persisted ledger body after the magic.
#[derive(Serialize, Deserialize)]
struct Body {
    plan: PathBuf,
    source_label: String,
    rows: Vec<Row>,
}

/// A disposition ledger bound to a plan and a SOURCE label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    /// Where it was read from, as given.
    pub path: PathBuf,
    /// The plan it binds to, canonical when it was created.
    pub plan: PathBuf,
    /// The SOURCE label it binds to.
    pub source_label: String,
    /// Every review, in the order it was recorded.
    pub rows: Vec<Row>,
}

impl Ledger {
    /// An empty ledger bound to `plan` and `source_label`.
    #[must_use]
    pub fn new(plan: &Path, source_label: &str) -> Self {
        Self {
            path: PathBuf::new(),
            plan: std::fs::canonicalize(plan).unwrap_or_else(|_| plan.to_owned()),
            source_label: source_label.to_owned(),
            rows: Vec::new(),
        }
    }

    /// The review that disposes `item`'s refusal with `code`: the latest
    /// item row for exactly that item and code, else the latest standing
    /// policy row for the code. `None` for a bare `IO` or `FRAME_CODEC`,
    /// whatever the ledger holds.
    #[must_use]
    pub fn review(&self, item: &str, code: &str) -> Option<&Row> {
        if !is_typed_code(code) {
            return None;
        }
        let latest = |scope: &Scope| {
            self.rows
                .iter()
                .rev()
                .find(|row| row.code == code && row.scope == *scope)
        };
        latest(&Scope::Item(item.to_owned())).or_else(|| latest(&Scope::Policy))
    }

    /// The persisted bytes.
    ///
    /// # Errors
    /// `FRAME_CODEC` for a ledger postcard cannot encode (a non-UTF-8 path).
    pub fn encode(&self) -> Result<Vec<u8>> {
        let body = postcard::to_allocvec(&Body {
            plan: self.plan.clone(),
            source_label: self.source_label.clone(),
            rows: self.rows.clone(),
        })
        .map_err(|_| BulkloadRefusal::FrameCodec)?;
        Ok([MAGIC.as_slice(), &body].concat())
    }

    /// Decode a ledger strictly and bind it to `plan` and `source_label`.
    ///
    /// # Errors
    /// `SCHEMA_MISMATCH` without the magic, `FIELD_DOMAIN_VIOLATION` when the
    /// body is not exactly one ledger or any row is out of domain (a row
    /// naming `IO` among them), and `RECEIPT_BINDING_INVALID` for a ledger
    /// bound to another plan or label.
    pub fn decode(bytes: &[u8], plan: &Path, source_label: &str) -> Result<Self> {
        let body = bytes
            .strip_prefix(&MAGIC)
            .ok_or(BulkloadRefusal::SchemaMismatch)?;
        let body: Body =
            crate::outcome::strict(body).ok_or(BulkloadRefusal::FieldDomainViolation)?;
        let same = match (
            std::fs::canonicalize(&body.plan),
            std::fs::canonicalize(plan),
        ) {
            (Ok(named), Ok(plan)) => named == plan,
            _ => body.plan == plan,
        };
        if !same || body.source_label != source_label {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        Ok(Self {
            path: PathBuf::new(),
            plan: body.plan,
            source_label: body.source_label,
            rows: body.rows,
        })
    }

    /// Read a ledger and bind it (no symlink is followed).
    ///
    /// # Errors
    /// As [`Ledger::decode`], `BUDGET_EXCEEDED` over 16 MiB, and the read's
    /// refusal.
    pub fn read(path: &Path, plan: &Path, source_label: &str) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .refuse_at("disposition::read")?;
        let mut bytes = Vec::new();
        file.take(LEDGER_LIMIT + 1)
            .read_to_end(&mut bytes)
            .refuse_at("disposition::read")?;
        if bytes.len() as u64 > LEDGER_LIMIT {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let mut ledger = Self::decode(&bytes, plan, source_label)?;
        path.clone_into(&mut ledger.path);
        Ok(ledger)
    }
}

/// Append one review to the ledger at `path` (`closure-dispose`), creating
/// it bound to `plan` and `source_label` when absent. An item row must name
/// an item `plan` holds.
///
/// Durability: under an exclusive lock on `{path}.lock`, the whole ledger is
/// written to `{path}.pending`, flushed, renamed over `path`, and the
/// directory flushed. A crash leaves the old ledger or the new one.
///
/// # Errors
/// `RECEIPT_BINDING_INVALID` for an item the plan does not hold or a ledger
/// bound elsewhere; the plan's and the ledger's read refusals; `IO` when
/// another writer holds the lock or a write fails.
pub fn record(path: &Path, plan: &Path, source_label: &str, row: Row) -> Result<Ledger> {
    if let Scope::Item(item) = &row.scope {
        let mut planned = false;
        for candidate in crate::estate::inspect(plan)? {
            planned |= crate::estate::id(&candidate)? == *item;
        }
        if !planned {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
    }
    let sibling = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(sibling(".lock"))
        .refuse_at("disposition::record")?;
    // SAFETY: `lock` owns its descriptor for the whole call, so the lock is
    // held until it drops at return; flock has no memory effects.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(crate::refuse::io(
            &std::io::Error::last_os_error(),
            "disposition::record",
        ));
    }
    let mut ledger = if path.try_exists().refuse_at("disposition::record")? {
        Ledger::read(path, plan, source_label)?
    } else {
        Ledger::new(plan, source_label)
    };
    ledger.rows.push(row);
    let bytes = ledger.encode()?;
    // Under the lock a `.pending` file can only be a crashed writer's.
    let pending = sibling(".pending");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&pending)
        .refuse_at("disposition::record")?;
    file.write_all(&bytes).refuse_at("disposition::record")?;
    file.sync_file_counted().refuse_at("disposition::record")?;
    std::fs::rename(&pending, path).refuse_at("disposition::record")?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::File::open(parent)
        .refuse_at("disposition::record")?
        .sync_dir_counted()
        .refuse_at("disposition::record")?;
    drop(lock);
    path.clone_into(&mut ledger.path);
    Ok(ledger)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn item(c: char) -> String {
        c.to_string().repeat(64)
    }

    #[test]
    fn rows_are_closed_and_never_name_untyped_io() {
        let ok = |scope, code: &str, reviewer: &str, date: &str| {
            Row::new(scope, code, Decision::Accept, reviewer, date)
        };
        assert!(ok(
            Scope::Item(item('a')),
            "GIT_NEST_STASHED",
            "jess",
            "2026-10-06"
        )
        .is_ok());
        assert!(ok(Scope::Policy, "CAPTURE_DRIFTED", "jess", "2028-02-29").is_ok());
        for (scope, code, reviewer, date) in [
            (Scope::Policy, "IO", "jess", "2026-10-06"),
            (Scope::Item(item('a')), "FRAME_CODEC", "jess", "2026-10-06"),
            (Scope::Policy, "NOT_A_CODE", "jess", "2026-10-06"),
            (
                Scope::Item("A".repeat(64)),
                "GIT_NEST_STASHED",
                "jess",
                "2026-10-06",
            ),
            (Scope::Item(item('a')), "GIT_NEST_STASHED", "", "2026-10-06"),
            (
                Scope::Item(item('a')),
                "GIT_NEST_STASHED",
                "a\nb",
                "2026-10-06",
            ),
            (Scope::Policy, "GIT_NEST_STASHED", "jess", "2026-02-29"),
            (Scope::Policy, "GIT_NEST_STASHED", "jess", "2026-13-01"),
            (Scope::Policy, "GIT_NEST_STASHED", "jess", "2026-1-01"),
            (Scope::Policy, "GIT_NEST_STASHED", "jess", "yesterday"),
        ] {
            assert_eq!(
                ok(scope.clone(), code, reviewer, date),
                Err(BulkloadRefusal::FieldDomainViolation),
                "{scope:?} {code} {reviewer:?} {date}"
            );
        }
        for name in ["accept", "re-carry", "abandon"] {
            assert_eq!(Decision::parse(name).unwrap().name(), name);
        }
        assert!(Decision::parse("ignore").is_err());
    }

    #[test]
    fn latest_item_row_then_policy_decides() {
        let mut ledger = Ledger::new(Path::new("/plan"), "neo");
        let row = |scope, code: &str, decision| {
            Row::new(scope, code, decision, "jess", "2026-10-06").unwrap()
        };
        ledger.rows = vec![
            row(Scope::Policy, "CAPTURE_DRIFTED", Decision::ReCarry),
            row(Scope::Item(item('a')), "CAPTURE_DRIFTED", Decision::Accept),
            row(Scope::Item(item('a')), "CAPTURE_DRIFTED", Decision::Abandon),
        ];
        let decide = |item: &str, code| ledger.review(item, code).map(Row::decision);
        assert_eq!(
            decide(&item('a'), "CAPTURE_DRIFTED"),
            Some(Decision::Abandon)
        );
        assert_eq!(
            decide(&item('b'), "CAPTURE_DRIFTED"),
            Some(Decision::ReCarry)
        );
        assert_eq!(decide(&item('a'), "GIT_NEST_STASHED"), None);
        assert_eq!(decide(&item('a'), "IO"), None);
    }

    #[test]
    fn ledgers_decode_strictly_and_bind() {
        let plan = Path::new("/plan-that-does-not-exist");
        let mut ledger = Ledger::new(plan, "neo");
        ledger.rows.push(
            Row::new(
                Scope::Policy,
                "CAPTURE_DRIFTED",
                Decision::Accept,
                "jess",
                "2026-10-06",
            )
            .unwrap(),
        );
        let bytes = ledger.encode().unwrap();
        assert_eq!(Ledger::decode(&bytes, plan, "neo").unwrap(), ledger);
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(
            Ledger::decode(&longer, plan, "neo"),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        assert_eq!(
            Ledger::decode(&bytes[1..], plan, "neo"),
            Err(BulkloadRefusal::SchemaMismatch)
        );
        assert_eq!(
            Ledger::decode(&bytes, Path::new("/other"), "neo"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            Ledger::decode(&bytes, plan, "sting"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        // A forged IO row (bypassing `Row::new`) refuses the whole ledger.
        ledger.rows.push(Row {
            scope: Scope::Policy,
            code: "IO".into(),
            decision: Decision::Accept,
            reviewer: "jess".into(),
            date: "2026-10-06".into(),
        });
        assert_eq!(
            Ledger::decode(&ledger.encode().unwrap(), plan, "neo"),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
    }
}
