//! The disposition ledger (WP3 PR 3, S4, OI-1003-Q1).
//!
//! S4 counts a run complete only when every typed refusal carries an
//! operator-reviewed disposition: accept, re-carry or abandon. This ledger
//! holds those reviews. It is bound to one plan, by path **and by a digest
//! of the plan's bytes**, and to one SOURCE label. A ledger written for one
//! plan refuses for any other plan later placed at the same path. It holds
//! two kinds of row:
//!
//! - an **item** row: one planned item's refusal with one code, bound to
//!   the **refusal instance** it reviews ([`crate::closure::instance`]: the
//!   item's current capture and its outcome record). A later refusal of the
//!   same code for the same item, against another capture or with another
//!   record, is a different instance: the old row no longer disposes it and
//!   the report lists the row as stale.
//! - a **standing-policy** row: every refusal with one code, for any item
//!   of the bound plan, now or later. It is open-ended by design and names
//!   no instance; the plan digest and the SOURCE label are its only bounds.
//!
//! Each row names its reviewer and date. Rows are only appended; for an
//! (item, instance, code) the latest item row wins, else the latest policy
//! row for the code. A bare `IO` or `FRAME_CODEC` names no cause and can
//! never be dispositioned: no row may name either, the writer refuses one,
//! and a ledger holding one does not decode.
//!
//! A row is written only for a code the taxonomy holds now. A row whose code
//! has left the taxonomy since still decodes ([`Row::is_retired`]): it
//! disposes nothing and the report lists it, and every other row of the
//! ledger keeps working.
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
use crate::outcome::{is_code_token, is_typed_code};
use crate::refuse::RefuseAt as _;
use crate::{BulkloadRefusal, Result};

/// The prefix of a disposition ledger (`bulkload.dispositions.v1`).
pub const MAGIC: [u8; 8] = [0x00, b'b', b'l', b'd', b'i', b's', b'p', 0x01];

/// The ledger's schema name, as the closure report prints it.
pub const SCHEMA: &str = "bulkload.dispositions.v1";

/// The largest disposition ledger read: 16 MiB, as for a plan.
const LEDGER_LIMIT: u64 = 16 * 1024 * 1024;

/// The largest plan read for its digest: 16 MiB, the plan reader's own limit.
const PLAN_LIMIT: u64 = 16 * 1024 * 1024;

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
    /// One planned item's refusal: the item identity and the refusal
    /// instance the review was made against ([`crate::closure::instance`]),
    /// each 64 lowercase hex.
    Item { item: String, instance: String },
    /// A standing policy: every refusal with the row's code, for any item of
    /// the bound plan, now or later.
    Policy,
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
        // Read time: any well-formed code token but the two that never name
        // a cause. A code retired since the row was written still decodes.
        let code_ok =
            is_code_token(&fields.code) && fields.code != "IO" && fields.code != "FRAME_CODEC";
        Self::checked(
            fields.scope,
            &fields.code,
            fields.decision,
            &fields.reviewer,
            &fields.date,
            code_ok,
        )
        .map_err(|_| "disposition row out of domain")
    }
}

impl Row {
    /// A review row to write.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` when the code is not a typed refusal code the
    /// taxonomy holds now (a bare `IO` or `FRAME_CODEC` can never be
    /// dispositioned), the item or its instance is not 64 lowercase hex, the
    /// reviewer is empty, over 128 bytes or holds a control character, or
    /// the date is not a calendar `YYYY-MM-DD`.
    pub fn new(
        scope: Scope,
        code: &str,
        decision: Decision,
        reviewer: &str,
        date: &str,
    ) -> Result<Self> {
        Self::checked(scope, code, decision, reviewer, date, is_typed_code(code))
    }

    fn checked(
        scope: Scope,
        code: &str,
        decision: Decision,
        reviewer: &str,
        date: &str,
        code_ok: bool,
    ) -> Result<Self> {
        let item_ok = match &scope {
            Scope::Item { item, instance } => is_hex64(item) && is_hex64(instance),
            Scope::Policy => true,
        };
        let reviewer_ok = !reviewer.trim().is_empty()
            && reviewer.len() <= REVIEWER_LIMIT
            && !reviewer.chars().any(char::is_control);
        if !(code_ok && item_ok && reviewer_ok && is_date(date)) {
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

    /// Whether the row's code has left the taxonomy since it was written.
    /// Such a row disposes nothing.
    #[must_use]
    pub fn is_retired(&self) -> bool {
        !BulkloadRefusal::is_code(&self.code)
    }

    /// The item an item row names.
    #[must_use]
    pub fn item(&self) -> Option<&str> {
        match &self.scope {
            Scope::Item { item, .. } => Some(item),
            Scope::Policy => None,
        }
    }

    /// The refusal instance an item row was made against.
    #[must_use]
    pub fn instance(&self) -> Option<&str> {
        match &self.scope {
            Scope::Item { instance, .. } => Some(instance),
            Scope::Policy => None,
        }
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
            Scope::Item { .. } => "item",
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
    plan_digest: String,
    source_label: String,
    rows: Vec<Row>,
}

/// The digest a ledger binds its plan by: blake3 of the plan file's bytes,
/// lowercase hex. The file must be a plan the estate reader accepts.
///
/// # Errors
/// The plan's read refusals (`IO` for a missing or symlinked file,
/// `FIELD_DOMAIN_VIOLATION` over 16 MiB, `FRAME_CODEC` for bytes that are
/// not a plan).
pub fn plan_digest(plan: &Path) -> Result<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(plan)
        .refuse_at("disposition::plan_digest")?;
    let mut bytes = Vec::new();
    file.take(PLAN_LIMIT + 1)
        .read_to_end(&mut bytes)
        .refuse_at("disposition::plan_digest")?;
    if bytes.len() as u64 > PLAN_LIMIT {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    // The bytes must be a plan: a ledger is never bound to anything else.
    crate::estate::inspect(plan)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// A disposition ledger bound to a plan and a SOURCE label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    /// Where it was read from, as given.
    pub path: PathBuf,
    /// The plan it binds to, canonical when it was created.
    pub plan: PathBuf,
    /// The digest of that plan's bytes ([`plan_digest`]).
    pub plan_digest: String,
    /// The SOURCE label it binds to.
    pub source_label: String,
    /// Every review, in the order it was recorded.
    pub rows: Vec<Row>,
}

impl Ledger {
    /// An empty ledger bound to `plan` (its path made canonical when it
    /// resolves), the plan content `plan_digest` names, and `source_label`.
    #[must_use]
    pub fn bound(plan: &Path, plan_digest: &str, source_label: &str) -> Self {
        Self {
            path: PathBuf::new(),
            plan: std::fs::canonicalize(plan).unwrap_or_else(|_| plan.to_owned()),
            plan_digest: plan_digest.to_owned(),
            source_label: source_label.to_owned(),
            rows: Vec::new(),
        }
    }

    /// The review that disposes `item`'s refusal `instance` with `code`: the
    /// latest item row for exactly that item, instance and code, else the
    /// latest standing policy row for the code. `None` for a bare `IO` or
    /// `FRAME_CODEC` and for a code the taxonomy no longer holds, whatever
    /// the ledger holds.
    #[must_use]
    pub fn review(&self, item: &str, instance: &str, code: &str) -> Option<&Row> {
        if !is_typed_code(code) {
            return None;
        }
        let latest = |matches: &dyn Fn(&Row) -> bool| {
            self.rows
                .iter()
                .rev()
                .find(|row| row.code == code && matches(row))
        };
        latest(&|row| row.item() == Some(item) && row.instance() == Some(instance))
            .or_else(|| latest(&|row| row.scope == Scope::Policy))
    }

    /// The persisted bytes.
    ///
    /// # Errors
    /// `FRAME_CODEC` for a ledger postcard cannot encode (a non-UTF-8 path).
    pub fn encode(&self) -> Result<Vec<u8>> {
        let body = postcard::to_allocvec(&Body {
            plan: self.plan.clone(),
            plan_digest: self.plan_digest.clone(),
            source_label: self.source_label.clone(),
            rows: self.rows.clone(),
        })
        .map_err(|_| BulkloadRefusal::FrameCodec)?;
        Ok([MAGIC.as_slice(), &body].concat())
    }

    /// Decode a ledger strictly and bind it to `plan`, the plan content
    /// `plan_digest` names, and `source_label`.
    ///
    /// # Errors
    /// `SCHEMA_MISMATCH` without the magic, `FIELD_DOMAIN_VIOLATION` when the
    /// body is not exactly one ledger or any row is out of domain (a row
    /// naming `IO` among them; a row naming a retired code is in domain),
    /// and `RECEIPT_BINDING_INVALID` for a ledger bound to another plan path,
    /// other plan bytes or another label.
    pub fn decode(
        bytes: &[u8],
        plan: &Path,
        plan_digest: &str,
        source_label: &str,
    ) -> Result<Self> {
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
        if !same
            || !is_hex64(&body.plan_digest)
            || body.plan_digest != plan_digest
            || body.source_label != source_label
        {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        Ok(Self {
            path: PathBuf::new(),
            plan: body.plan,
            plan_digest: body.plan_digest,
            source_label: body.source_label,
            rows: body.rows,
        })
    }

    /// Read a ledger and bind it to the plan file at `plan` as it is now (no
    /// symlink is followed).
    ///
    /// # Errors
    /// As [`Ledger::decode`] and [`plan_digest`], `BUDGET_EXCEEDED` over
    /// 16 MiB, and the read's refusal.
    pub fn read(path: &Path, plan: &Path, source_label: &str) -> Result<Self> {
        Self::read_bound(path, plan, &plan_digest(plan)?, source_label)
    }

    fn read_bound(path: &Path, plan: &Path, digest: &str, source_label: &str) -> Result<Self> {
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
        let mut ledger = Self::decode(&bytes, plan, digest, source_label)?;
        path.clone_into(&mut ledger.path);
        Ok(ledger)
    }
}

/// Append one review to the ledger at `path` (`closure-dispose`).
///
/// The ledger is created bound to `plan` and `source_label` when absent. The
/// plan is read for every row, item or policy: it must exist and be a plan,
/// and its bytes are what the ledger binds to. An item row must name an item
/// `plan` holds.
///
/// The caller proves an item row's refusal exists: `closure-dispose` builds
/// the closure report and takes the row's instance from it
/// ([`crate::closure::Report::reviewable`]).
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
    let digest = plan_digest(plan)?;
    if let Some(item) = row.item() {
        let mut planned = false;
        for candidate in crate::estate::inspect(plan)? {
            planned |= crate::estate::id(&candidate)? == item;
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
        Ledger::read_bound(path, plan, &digest, source_label)?
    } else {
        Ledger::bound(plan, &digest, source_label)
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

    fn hex(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn item(c: char) -> Scope {
        Scope::Item {
            item: hex(c),
            instance: hex('1'),
        }
    }

    #[test]
    fn rows_are_closed_and_never_name_untyped_io() {
        let ok = |scope, code: &str, reviewer: &str, date: &str| {
            Row::new(scope, code, Decision::Accept, reviewer, date)
        };
        assert!(ok(item('a'), "GIT_NEST_STASHED", "jess", "2026-10-06").is_ok());
        assert!(ok(Scope::Policy, "CAPTURE_DRIFTED", "jess", "2028-02-29").is_ok());
        let upper = Scope::Item {
            item: "A".repeat(64),
            instance: hex('1'),
        };
        let unbound = Scope::Item {
            item: hex('a'),
            instance: String::new(),
        };
        for (scope, code, reviewer, date) in [
            (Scope::Policy, "IO", "jess", "2026-10-06"),
            (item('a'), "FRAME_CODEC", "jess", "2026-10-06"),
            (Scope::Policy, "NOT_A_CODE", "jess", "2026-10-06"),
            (upper, "GIT_NEST_STASHED", "jess", "2026-10-06"),
            // An item row names the refusal instance it reviews.
            (unbound, "GIT_NEST_STASHED", "jess", "2026-10-06"),
            (item('a'), "GIT_NEST_STASHED", "", "2026-10-06"),
            (item('a'), "GIT_NEST_STASHED", "a\nb", "2026-10-06"),
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
    fn latest_item_row_for_the_instance_then_policy_decides() {
        let mut ledger = Ledger::bound(Path::new("/plan"), &hex('d'), "neo");
        let row = |scope, code: &str, decision| {
            Row::new(scope, code, decision, "jess", "2026-10-06").unwrap()
        };
        let other = Scope::Item {
            item: hex('a'),
            instance: hex('2'),
        };
        ledger.rows = vec![
            row(Scope::Policy, "CAPTURE_DRIFTED", Decision::ReCarry),
            row(item('a'), "CAPTURE_DRIFTED", Decision::Accept),
            row(item('a'), "CAPTURE_DRIFTED", Decision::Abandon),
            row(other, "GIT_NEST_STASHED", Decision::Accept),
        ];
        let decide = |item: char, instance: char, code| {
            ledger
                .review(&hex(item), &hex(instance), code)
                .map(|row| (row.decision(), row.basis()))
        };
        assert_eq!(
            decide('a', '1', "CAPTURE_DRIFTED"),
            Some((Decision::Abandon, "item"))
        );
        // The same item and code, another refusal instance: the item rows
        // do not reach it; only the standing policy does.
        assert_eq!(
            decide('a', '2', "CAPTURE_DRIFTED"),
            Some((Decision::ReCarry, "policy"))
        );
        assert_eq!(
            decide('b', '1', "CAPTURE_DRIFTED"),
            Some((Decision::ReCarry, "policy"))
        );
        // No policy for this code: only the exact instance is disposed.
        assert_eq!(
            decide('a', '2', "GIT_NEST_STASHED"),
            Some((Decision::Accept, "item"))
        );
        assert_eq!(decide('a', '1', "GIT_NEST_STASHED"), None);
        assert_eq!(decide('a', '1', "IO"), None);
    }

    #[test]
    fn ledgers_decode_strictly_and_bind() {
        let plan = Path::new("/plan-that-does-not-exist");
        let digest = hex('d');
        let mut ledger = Ledger::bound(plan, &digest, "neo");
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
        ledger.rows.push(
            Row::new(
                item('a'),
                "GIT_NEST_STASHED",
                Decision::ReCarry,
                "jess",
                "2026-10-06",
            )
            .unwrap(),
        );
        let bytes = ledger.encode().unwrap();
        assert_eq!(
            Ledger::decode(&bytes, plan, &digest, "neo").unwrap(),
            ledger
        );
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(
            Ledger::decode(&longer, plan, &digest, "neo"),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        assert_eq!(
            Ledger::decode(&bytes[1..], plan, &digest, "neo"),
            Err(BulkloadRefusal::SchemaMismatch)
        );
        assert_eq!(
            Ledger::decode(&bytes, Path::new("/other"), &digest, "neo"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        // The same path holding other plan bytes binds nothing.
        assert_eq!(
            Ledger::decode(&bytes, plan, &hex('e'), "neo"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            Ledger::decode(&bytes, plan, &digest, "sting"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        // A ledger naming no plan digest binds nothing, whatever is asked.
        let unbound = Ledger::bound(plan, "", "neo").encode().unwrap();
        assert_eq!(
            Ledger::decode(&unbound, plan, "", "neo"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        // A forged IO row (bypassing `Row::new`) refuses the whole ledger.
        let mut forged = ledger.clone();
        forged.rows.push(Row {
            scope: Scope::Policy,
            code: "IO".into(),
            decision: Decision::Accept,
            reviewer: "jess".into(),
            date: "2026-10-06".into(),
        });
        assert_eq!(
            Ledger::decode(&forged.encode().unwrap(), plan, &digest, "neo"),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        // So does a row whose code is free text.
        let mut forged = ledger.clone();
        forged.rows.push(Row {
            scope: Scope::Policy,
            code: "disk went away".into(),
            decision: Decision::Accept,
            reviewer: "jess".into(),
            date: "2026-10-06".into(),
        });
        assert_eq!(
            Ledger::decode(&forged.encode().unwrap(), plan, &digest, "neo"),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
    }

    // A code that leaves the taxonomy after a row was written (a later lane
    // deletes the variant) fails closed for that row only: the ledger still
    // decodes, every other review still works, and a writer cannot add one.
    #[test]
    fn a_retired_code_fails_closed_per_row_not_per_ledger() {
        let plan = Path::new("/plan-that-does-not-exist");
        let digest = hex('d');
        let retired = "JOURNAL_RETIRED_FOR_THIS_TEST";
        assert!(!BulkloadRefusal::is_code(retired));
        assert_eq!(
            Row::new(
                Scope::Policy,
                retired,
                Decision::Accept,
                "jess",
                "2026-10-06"
            ),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        let mut ledger = Ledger::bound(plan, &digest, "neo");
        for (scope, code) in [
            (Scope::Policy, retired),
            (item('a'), retired),
            (item('a'), "GIT_NEST_STASHED"),
        ] {
            // As a binary that still held the code wrote it.
            ledger.rows.push(Row {
                scope,
                code: code.into(),
                decision: Decision::Accept,
                reviewer: "jess".into(),
                date: "2026-10-06".into(),
            });
        }
        let read = Ledger::decode(&ledger.encode().unwrap(), plan, &digest, "neo").unwrap();
        assert_eq!(read, ledger);
        assert_eq!(
            read.rows.iter().map(Row::is_retired).collect::<Vec<_>>(),
            vec![true, true, false]
        );
        // The retired rows dispose nothing; the current one still does.
        assert_eq!(read.review(&hex('a'), &hex('1'), retired), None);
        assert!(read
            .review(&hex('a'), &hex('1'), "GIT_NEST_STASHED")
            .is_some());
    }

    fn plan_with(root: &Path, name: &str, source: &str) -> (PathBuf, String) {
        let plan = root.join(name);
        let source = root.join(source);
        std::fs::create_dir_all(&source).unwrap();
        crate::estate::add(&plan, &source, &root.join("repository"), None).unwrap();
        let items = crate::estate::inspect(&plan).unwrap();
        (plan, crate::estate::id(&items[0]).unwrap())
    }

    // The ledger binds the plan's bytes, and `record` reads the plan for
    // every row: a policy row cannot be recorded against a path that holds
    // no plan, and a ledger does not follow a path to another plan.
    #[test]
    fn a_ledger_is_bound_to_the_plan_bytes_not_only_its_path() {
        let root = tempfile::tempdir().unwrap();
        let (plan, first) = plan_with(root.path(), "plan", "s1");
        let ledger = root.path().join("reviews");
        let policy = || {
            Row::new(
                Scope::Policy,
                "GIT_NEST_STASHED",
                Decision::Accept,
                "jess",
                "2026-10-06",
            )
            .unwrap()
        };
        // No plan, or a file that is not a plan: nothing is created.
        let missing = root.path().join("no-such-plan");
        assert!(record(&root.path().join("l2"), &missing, "neo", policy()).is_err());
        let junk = root.path().join("junk");
        std::fs::write(&junk, b"not a plan").unwrap();
        assert!(record(&root.path().join("l2"), &junk, "neo", policy()).is_err());
        assert!(!root.path().join("l2").exists());

        let recorded = record(&ledger, &plan, "neo", policy()).unwrap();
        assert_eq!(recorded.plan_digest, plan_digest(&plan).unwrap());
        assert_eq!(Ledger::read(&ledger, &plan, "neo").unwrap().rows.len(), 1);

        // Another plan placed at the same path: the ledger binds nothing,
        // for reading and for appending, and is left as it was.
        let before = std::fs::read(&ledger).unwrap();
        let (other, second) = plan_with(root.path(), "other-plan", "s2");
        assert_ne!(first, second);
        std::fs::rename(&other, &plan).unwrap();
        assert_eq!(
            Ledger::read(&ledger, &plan, "neo"),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            record(&ledger, &plan, "neo", policy()),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(std::fs::read(&ledger).unwrap(), before);
        // An item row for an item the plan does not hold binds nothing.
        let foreign = Row::new(
            Scope::Item {
                item: first,
                instance: hex('1'),
            },
            "GIT_NEST_STASHED",
            Decision::Accept,
            "jess",
            "2026-10-06",
        )
        .unwrap();
        assert_eq!(
            record(&root.path().join("l3"), &plan, "neo", foreign),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
    }
}
