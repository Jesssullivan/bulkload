//! Closure report (OI-1001-Q2): bulkload's own completion gate for an
//! estate apply, and the S4 proof surface (OI-1003-Q1, WP3 PR 3).
//!
//! Every planned item must end in exactly one of these dispositions, read
//! from the run's durable ledger (typed outcome records and apply journals,
//! see [`crate::estate::ledger`] and [`crate::outcome`]). Classification
//! matches on the typed [`Outcome`], never on strings:
//!
//! - `applied`: the item plans a workspace, its outcome is
//!   `WorkspaceRestored` (or a re-apply's
//!   `PreviousWorkspaceRestorationNotRevalidated`), and the exact journal for
//!   its current capture and SOURCE says `workspace-restored`.
//! - `refused-pending-review`: the outcome is a typed refusal (any taxonomy
//!   code but `IO` and `FRAME_CODEC`) that no review disposes yet. The item
//!   is accounted for natively, but the run is not complete (S4).
//! - `refused`: such a refusal, disposed by a row of the disposition ledger
//!   ([`crate::disposition`]): accept, re-carry or abandon.
//! - `referenced-only`: the item plans no workspace, its outcome is
//!   `RefsImported` (or `PreviousRefCustodyNotWorkspaceParity`, or
//!   `IndexRepaired` from `git-repair-missing-index` with a state directory,
//!   #95), and the exact current-capture journal says `refs-imported`. The
//!   refs are held; no working bytes were laid down, and none were planned.
//!
//! Anything else is `unaccounted`: no outcome record, an unreadable record,
//! a record naming another source, a bare `IO` or `FRAME_CODEC` refusal (it
//! names no cause), an outcome that does not match whether the item plans a
//! workspace, a missing or mismatched current-capture journal (a stale
//! journal from an earlier capture proves nothing), or an outcome that is not
//! an apply outcome at all. The native `verdict` passes only when
//! `unaccounted` is 0.
//!
//! An attestation ledger (`--attest`, #95, #133) may close natively
//! unaccounted items in its own `attested` block, never one whose own record
//! is an untyped refusal. It never changes the native `verdict` or totals,
//! and never overrides a native record.
//!
//! The top-level `gate` is S4: every planned item is accounted for natively
//! or by an accepted attestation row bound to the plan, the SOURCE label, the
//! item's source and its current capture digest, **and** every typed refusal
//! (native or attested) carries a disposition. A bare `IO` never counts and
//! can never be dispositioned or attested away.

use crate::refuse::RefuseAt as _;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::estate::{JournalState, Ledger, LedgerEntry};
use crate::outcome::Outcome;
use crate::{BulkloadRefusal, Result};

/// One planned item's closure disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Workspace restored, journal present.
    Applied,
    /// A typed refusal with this code that an operator review disposes (S4).
    Refused(String),
    /// A typed refusal with this code that no review disposes yet (S4).
    RefusedPendingReview(String),
    /// Ref custody imported, journal present; no working bytes.
    ReferencedOnly,
    /// Not provably closed, for this reason.
    Unaccounted(&'static str),
}

impl Disposition {
    /// The disposition's stable JSON name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Refused(_) => "refused",
            Self::RefusedPendingReview(_) => "refused-pending-review",
            Self::ReferencedOnly => "referenced-only",
            Self::Unaccounted(_) => "unaccounted",
        }
    }

    /// The typed refusal code, reviewed or not.
    #[must_use]
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Self::Refused(code) | Self::RefusedPendingReview(code) => Some(code),
            _ => None,
        }
    }
}

/// Native unaccounted reasons that mean the item's own record is an untyped
/// refusal. No attestation may close such an item (S4).
const UNTYPED_REFUSALS: &[&str] = &[
    "refusal-untyped",
    "refusal-untyped-io",
    "refusal-untyped-frame-codec",
];

/// Classify one ledger entry natively. A typed refusal is
/// [`Disposition::RefusedPendingReview`] here: only a disposition ledger
/// ([`Report::dispose`]) makes it [`Disposition::Refused`].
#[must_use]
pub fn classify(entry: &LedgerEntry) -> Disposition {
    let record = match &entry.record {
        None => return Disposition::Unaccounted("no-outcome-record"),
        Some(Err(why)) => return Disposition::Unaccounted(why.reason()),
        Some(Ok(record)) => record,
    };
    if record.source != entry.source {
        return Disposition::Unaccounted("record-source-mismatch");
    }
    // The exact current-capture journal must carry this body.
    let journal = |body: &str, closed: Disposition| match &entry.journal {
        JournalState::Present(found) if found == body => closed,
        JournalState::Present(_) => Disposition::Unaccounted("journal-outcome-mismatch"),
        JournalState::Absent => Disposition::Unaccounted("journal-missing"),
        JournalState::NoCapture => Disposition::Unaccounted("capture-record-missing"),
        JournalState::CaptureUnreadable => Disposition::Unaccounted("capture-record-unreadable"),
    };
    match &record.outcome {
        // A bare IO or FRAME_CODEC names no cause (S4).
        Outcome::Refused(refusal) => match refusal.code() {
            "IO" => Disposition::Unaccounted("refusal-untyped-io"),
            "FRAME_CODEC" => Disposition::Unaccounted("refusal-untyped-frame-codec"),
            code => Disposition::RefusedPendingReview(code.to_owned()),
        },
        Outcome::WorkspaceRestored | Outcome::PreviousWorkspaceRestorationNotRevalidated => {
            if entry.has_workspace {
                journal("workspace-restored", Disposition::Applied)
            } else {
                Disposition::Unaccounted("outcome-workspace-mismatch")
            }
        }
        // #95: git-repair-missing-index imported the same ref custody and
        // created the missing index into this state directory.
        Outcome::IndexRepaired => {
            if entry.has_workspace {
                Disposition::Unaccounted("index-repaired-for-workspace-item")
            } else {
                journal("refs-imported", Disposition::ReferencedOnly)
            }
        }
        Outcome::RefsImported | Outcome::PreviousRefCustodyNotWorkspaceParity => {
            if entry.has_workspace {
                // A planned workspace that only got refs is not closed.
                Disposition::Unaccounted("refs-only-for-workspace-item")
            } else {
                journal("refs-imported", Disposition::ReferencedOnly)
            }
        }
        Outcome::Captured
        | Outcome::CapturedWithDrift
        | Outcome::CaptureExtendedFromDrift
        | Outcome::CaptureReusedAfterCensus
        | Outcome::DeferredWithDrift => Disposition::Unaccounted("not-an-apply-outcome"),
    }
}

/// What the corpus holds for a planned item's current capture (#133).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureId {
    /// No `{item}.capture` record.
    None,
    /// A capture record that does not decode.
    Unreadable,
    /// The current capture's bundle digest, lowercase hex.
    Digest(String),
}

impl CaptureId {
    fn of(entry: &LedgerEntry) -> Self {
        match (&entry.journal, &entry.capture) {
            (JournalState::NoCapture, _) => Self::None,
            (_, Some(digest)) => Self::Digest(digest.clone()),
            (_, None) => Self::Unreadable,
        }
    }
}

/// One planned item in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub item: String,
    pub source: PathBuf,
    /// The typed outcome record's outcome, when one decoded.
    pub outcome: Option<Outcome>,
    pub disposition: Disposition,
    /// The item's current capture, which an attestation row must name.
    pub capture: CaptureId,
    /// The review that disposes this row's typed refusal (S4).
    pub review: Option<crate::disposition::Row>,
}

/// The per-item closure ledger and its totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Row>,
    pub applied: u64,
    /// Typed refusals a review disposes.
    pub refused: u64,
    /// Typed refusals no review disposes yet.
    pub refused_pending_review: u64,
    pub referenced_only: u64,
    pub unaccounted: u64,
    /// Typed refusal counts by code, reviewed or not.
    pub refusals: std::collections::BTreeMap<String, u64>,
    /// Outcome records for items the plan does not hold. Reported, not
    /// counted: they are not planned items.
    pub foreign: Vec<String>,
    /// Journals that are no planned item's current-capture journal (foreign
    /// or stale). Reported, not counted.
    pub unmatched_journals: Vec<String>,
    /// The attestation block, when an attestation ledger was given (#95).
    pub attested: Option<Attested>,
    /// The review block, when a disposition ledger was given (S4).
    pub reviewed: Option<Reviewed>,
}

impl Report {
    /// Build the report from a ledger.
    #[must_use]
    pub fn from_ledger(ledger: &Ledger) -> Self {
        let mut report = Self {
            foreign: ledger.foreign_records.clone(),
            unmatched_journals: ledger.unmatched_journals.clone(),
            ..Self::default()
        };
        for entry in &ledger.entries {
            report.rows.push(Row {
                item: entry.item.clone(),
                source: entry.source.clone(),
                outcome: entry
                    .record
                    .as_ref()
                    .and_then(|record| record.as_ref().ok())
                    .map(|record| record.outcome.clone()),
                disposition: classify(entry),
                capture: CaptureId::of(entry),
                review: None,
            });
        }
        report.recount();
        report
    }

    // Apply the reviews (when a disposition ledger was given) to every typed
    // refusal, native and attested, and recount the totals.
    fn recount(&mut self) {
        let ledger = self.reviewed.as_ref().map(|reviewed| &reviewed.ledger);
        let review =
            |item: &str, code: &str| ledger.and_then(|ledger| ledger.review(item, code).cloned());
        let mut used = std::collections::BTreeSet::new();
        let mut decisions = std::collections::BTreeMap::<&'static str, u64>::new();
        (
            self.applied,
            self.refused,
            self.refused_pending_review,
            self.referenced_only,
            self.unaccounted,
        ) = (0, 0, 0, 0, 0);
        self.refusals.clear();
        for row in &mut self.rows {
            if let Some(code) = row.disposition.refusal().map(str::to_owned) {
                row.review = review(&row.item, &code);
                row.disposition = match &row.review {
                    Some(found) => {
                        used.insert((row.item.clone(), code.clone()));
                        *decisions.entry(found.decision().name()).or_default() += 1;
                        Disposition::Refused(code.clone())
                    }
                    None => Disposition::RefusedPendingReview(code.clone()),
                };
                *self.refusals.entry(code).or_default() += 1;
            }
            match &row.disposition {
                Disposition::Applied => self.applied += 1,
                Disposition::Refused(_) => self.refused += 1,
                Disposition::RefusedPendingReview(_) => self.refused_pending_review += 1,
                Disposition::ReferencedOnly => self.referenced_only += 1,
                Disposition::Unaccounted(_) => self.unaccounted += 1,
            }
        }
        if let Some(attested) = &mut self.attested {
            attested.pending.clear();
            attested.reviews.clear();
            for row in &attested.items {
                let Some(code) = row
                    .refusal
                    .as_deref()
                    .filter(|_| row.disposition == "refused")
                else {
                    continue;
                };
                match review(&row.item, code) {
                    Some(found) => {
                        used.insert((row.item.clone(), code.to_owned()));
                        *decisions.entry(found.decision().name()).or_default() += 1;
                        attested.reviews.push((row.item.clone(), found));
                    }
                    None => attested.pending.push(row.item.clone()),
                }
            }
        }
        if let Some(reviewed) = &mut self.reviewed {
            reviewed.decisions = decisions;
            reviewed.unmatched = reviewed
                .ledger
                .rows
                .iter()
                .filter_map(|row| match row.scope() {
                    crate::disposition::Scope::Item(item) => Some((item.clone(), row.code())),
                    crate::disposition::Scope::Policy => None,
                })
                .filter(|(item, code)| !used.contains(&(item.clone(), (*code).to_owned())))
                .map(|(item, code)| (item, code.to_owned()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
        }
    }

    /// Planned items in the report.
    #[must_use]
    pub const fn planned(&self) -> u64 {
        self.rows.len() as u64
    }

    /// The native verdict (`verdict`): every planned item is accounted for
    /// by its own durable records (a typed refusal is, reviewed or not). An
    /// attestation or a review never changes it (#133).
    #[must_use]
    pub const fn native_passes(&self) -> bool {
        self.unaccounted == 0
    }

    /// The closure gate (`gate`, the exit status), S4: every planned item is
    /// accounted for natively or by an accepted, bound attestation row, and
    /// every typed refusal, native or attested, carries a disposition.
    #[must_use]
    pub fn passes(&self) -> bool {
        self.remaining_unaccounted() == 0 && self.pending_review() == 0
    }

    /// Items accounted for neither natively nor by an accepted attestation.
    #[must_use]
    pub const fn remaining_unaccounted(&self) -> u64 {
        match &self.attested {
            Some(attested) => self.unaccounted.saturating_sub(attested.items.len() as u64),
            None => self.unaccounted,
        }
    }

    /// Typed refusals, native or attested, that no review disposes (S4).
    #[must_use]
    pub fn pending_review(&self) -> u64 {
        self.refused_pending_review
            + self
                .attested
                .as_ref()
                .map_or(0, |attested| attested.pending.len() as u64)
    }

    /// Join an attestation ledger (`bulkload.closure-ledger.v1`, #95) to this
    /// native report. A row closes a planned item only when the native ledger
    /// leaves it unaccounted, its own record is not an untyped refusal, and
    /// the row names the item's source and current capture digest (#133); a
    /// native record is never overridden. Rows are reported in their own
    /// block, never in the native totals or verdict. An accepted row claiming
    /// `refused` is a typed refusal like any other: it needs a review (S4).
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for a ledger that lists an item twice. The
    /// ledger's schema, plan and SOURCE label bind when it is read.
    pub fn attest(&mut self, ledger: &AttestationLedger) -> Result<()> {
        let mut attested = Attested {
            ledger: ledger.path.clone(),
            ..Attested::default()
        };
        let mut seen = std::collections::BTreeSet::new();
        for row in &ledger.rows {
            if !seen.insert(row.item.clone()) {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
            let Some(native) = self.rows.iter().find(|native| native.item == row.item) else {
                attested.foreign.push(row.item.clone());
                continue;
            };
            if !matches!(native.disposition, Disposition::Unaccounted(_)) {
                // Never overrides a native record; a disagreement is listed.
                attested.superseded += 1;
                if row.disposition != native.disposition.name()
                    && !(row.disposition == "refused" && native.disposition.refusal().is_some())
                {
                    attested.disagreements.push(row.item.clone());
                }
                continue;
            }
            match row.verdict(&native.source, &native.capture, &native.disposition) {
                Ok(()) => {
                    *attested
                        .dispositions
                        .entry(row.disposition.clone())
                        .or_default() += 1;
                    attested.items.push(row.clone());
                }
                Err(why) => attested.rejected.push((row.item.clone(), why)),
            }
        }
        self.attested = Some(attested);
        self.recount();
        Ok(())
    }

    /// Join a disposition ledger (S4): every typed refusal, native or
    /// attested, that a review disposes becomes `refused` with that review;
    /// the rest stay pending. A bare `IO` is never touched: it is
    /// unaccounted, and no review can name it. The ledger's plan and SOURCE
    /// label bind when it is read.
    pub fn dispose(&mut self, ledger: &crate::disposition::Ledger) {
        self.reviewed = Some(Reviewed {
            ledger: ledger.clone(),
            ..Reviewed::default()
        });
        self.recount();
    }

    /// The gate as a value: `CLOSURE_UNACCOUNTED` when any item is
    /// unaccounted or any typed refusal is pending review.
    ///
    /// # Errors
    /// `CLOSURE_UNACCOUNTED` when the gate fails.
    pub fn gate(&self) -> Result<()> {
        if self.passes() {
            Ok(())
        } else {
            Err(BulkloadRefusal::ClosureUnaccounted)
        }
    }

    /// The report as one JSON document (`bulkload.closure.v1`).
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let _ = write!(
            out,
            "{{\"schema\":\"bulkload.closure.v1\",\"verdict\":\"{}\",\"gate\":\"{}\",\"totals\":{{\"planned\":{},\"applied\":{},\"refused\":{},\"refused_pending_review\":{},\"referenced_only\":{},\"unaccounted\":{},\"pending_review\":{}}},\"refusals\":{{",
            if self.native_passes() { "pass" } else { "fail" },
            if self.passes() { "pass" } else { "fail" },
            self.planned(),
            self.applied,
            self.refused,
            self.refused_pending_review,
            self.referenced_only,
            self.unaccounted,
            self.pending_review(),
        );
        for (index, (code, count)) in self.refusals.iter().enumerate() {
            let _ = write!(
                out,
                "{}{}:{count}",
                if index == 0 { "" } else { "," },
                json_string(code)
            );
        }
        out.push_str("},\"items\":[");
        for (index, row) in self.rows.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"item\":{},\"source\":{},\"capture\":{},\"outcome\":{},\"disposition\":\"{}\"",
                json_string(&row.item),
                json_string(&row.source.to_string_lossy()),
                match &row.capture {
                    CaptureId::Digest(digest) => json_string(digest),
                    CaptureId::None | CaptureId::Unreadable => "null".to_owned(),
                },
                row.outcome
                    .as_ref()
                    .map_or_else(|| "null".to_owned(), |outcome| json_string(outcome.name())),
                row.disposition.name(),
            );
            if let Some(refusal) = row.outcome.as_ref().and_then(Outcome::refusal) {
                let _ = write!(
                    out,
                    ",\"refusal\":{},\"site\":{},\"errno\":{}",
                    json_string(refusal.code()),
                    json_string(refusal.site()),
                    refusal
                        .errno()
                        .map_or_else(|| "null".to_owned(), |errno| errno.to_string()),
                );
            }
            if let Disposition::Unaccounted(why) = &row.disposition {
                let _ = write!(out, ",\"unaccounted_reason\":{}", json_string(why));
            }
            if let Some(review) = &row.review {
                out.push_str(",\"review\":");
                write_review(&mut out, review);
            }
            out.push('}');
        }
        out.push_str("],\"foreign_outcome_records\":[");
        for (index, item) in self.foreign.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(item));
        }
        out.push_str("],\"unmatched_journals\":[");
        for (index, name) in self.unmatched_journals.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(name));
        }
        out.push(']');
        if let Some(attested) = &self.attested {
            attested.write_json(&mut out, self);
        }
        if let Some(reviewed) = &self.reviewed {
            reviewed.write_json(&mut out, self);
        }
        out.push('}');
        out
    }
}

// One review as JSON: its decision, reviewer, date and basis.
fn write_review(out: &mut String, review: &crate::disposition::Row) {
    let _ = write!(
        out,
        "{{\"decision\":\"{}\",\"reviewer\":{},\"date\":{},\"basis\":\"{}\"}}",
        review.decision().name(),
        json_string(review.reviewer()),
        json_string(review.date()),
        review.basis(),
    );
}

/// The review block of a closure report (S4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reviewed {
    /// The disposition ledger joined.
    pub ledger: crate::disposition::Ledger,
    /// Disposed refusals (native and attested) by decision.
    pub decisions: std::collections::BTreeMap<&'static str, u64>,
    /// Item rows, as (item, code), that dispose no refusal in this report.
    pub unmatched: Vec<(String, String)>,
}

impl Reviewed {
    fn write_json(&self, out: &mut String, report: &Report) {
        let policies = self
            .ledger
            .rows
            .iter()
            .filter(|row| matches!(row.scope(), crate::disposition::Scope::Policy))
            .count();
        let _ = write!(
            out,
            ",\"reviewed\":{{\"schema\":\"{}\",\"ledger\":{},\"totals\":{{\"rows\":{},\"policies\":{},\"disposed\":{},\"pending_review\":{}}},\"decisions\":{{",
            crate::disposition::SCHEMA,
            json_string(&self.ledger.path.to_string_lossy()),
            self.ledger.rows.len(),
            policies,
            self.decisions.values().sum::<u64>(),
            report.pending_review(),
        );
        for (index, (name, count)) in self.decisions.iter().enumerate() {
            let _ = write!(
                out,
                "{}{}:{count}",
                if index == 0 { "" } else { "," },
                json_string(name)
            );
        }
        out.push_str("},\"policies\":[");
        let mut first = true;
        for row in &self.ledger.rows {
            if !matches!(row.scope(), crate::disposition::Scope::Policy) {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            let _ = write!(out, "{{\"refusal\":{},\"review\":", json_string(row.code()));
            write_review(out, row);
            out.push('}');
        }
        out.push_str("],\"unmatched\":[");
        for (index, (item, code)) in self.unmatched.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"item\":{},\"refusal\":{}}}",
                json_string(item),
                json_string(code)
            );
        }
        out.push_str("]}");
    }
}

/// Dispositions an attestation row may claim (#95). `present` and
/// `source-absent` are audit outcomes no apply verb produces: the content is
/// already at the destination, or the source no longer exists.
pub const ATTESTED_DISPOSITIONS: &[&str] = &[
    "applied",
    "refused",
    "referenced-only",
    "present",
    "source-absent",
];

/// The capture an attestation row names (#133).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestedCapture {
    /// No `capture` member: the row is bound to no capture.
    Missing,
    /// `"capture": null`: the row attests an item the corpus holds no
    /// capture record for.
    Null,
    /// A 64-hex capture digest, lowercased.
    Digest(String),
}

/// One attestation ledger row (`bulkload.closure-ledger.v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationRow {
    pub item: String,
    pub source: Option<String>,
    /// The item's current capture digest, as the row binds it.
    pub capture: AttestedCapture,
    pub disposition: String,
    pub basis: Option<String>,
    pub refusal: Option<String>,
    /// The row's `evidence`, as given; `None` when absent.
    pub evidence: Option<Json>,
}

impl AttestationRow {
    // Whether this row may close a natively unaccounted item whose plan
    // source is `source`, whose current capture is `capture` and whose native
    // disposition is `native`, and why not.
    fn verdict(
        &self,
        source: &std::path::Path,
        capture: &CaptureId,
        native: &Disposition,
    ) -> std::result::Result<(), &'static str> {
        // S4: a bare IO never counts. An item whose own record is an untyped
        // refusal is closed only by a verb that records a typed outcome.
        if matches!(native, Disposition::Unaccounted(why) if UNTYPED_REFUSALS.contains(why)) {
            return Err("native-refusal-untyped");
        }
        if self.source.as_deref().map(std::path::Path::new) != Some(source) {
            return Err("source-mismatch");
        }
        // #133: a row binds to the exact capture the corpus holds now; a
        // row written against an earlier capture proves nothing about it.
        match (&self.capture, capture) {
            (AttestedCapture::Missing, _) => return Err("capture-missing"),
            (_, CaptureId::Unreadable) => return Err("capture-record-unreadable"),
            (AttestedCapture::Null, CaptureId::None) => {}
            (AttestedCapture::Digest(named), CaptureId::Digest(current)) if named == current => {}
            _ => return Err("capture-mismatch"),
        }
        if !ATTESTED_DISPOSITIONS.contains(&self.disposition.as_str()) {
            return Err("disposition-unknown");
        }
        match self.basis.as_deref() {
            None | Some("") => return Err("basis-missing"),
            // A claim of native closure is not an attestation, and the native
            // ledger says otherwise.
            Some("native-closure-report") => return Err("basis-claims-native"),
            Some(_) => {}
        }
        if self.evidence.as_ref().is_none_or(Json::is_empty) {
            return Err("evidence-missing");
        }
        if self.disposition == "refused" {
            match self.refusal.as_deref() {
                Some("IO" | "FRAME_CODEC") => return Err("refusal-untyped"),
                Some(code) if BulkloadRefusal::is_code(code) => {}
                _ => return Err("refusal-untyped"),
            }
        }
        Ok(())
    }
}

/// A parsed attestation ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttestationLedger {
    /// Where it was read from, as given.
    pub path: PathBuf,
    pub rows: Vec<AttestationRow>,
}

impl AttestationLedger {
    /// Read and parse an attestation ledger, binding it to `plan` and the
    /// SOURCE label: the ledger must name both (`plan`, `source_label`, #133),
    /// and a ledger that names another plan or label refuses.
    ///
    /// # Errors
    /// `SCHEMA_MISMATCH` for another schema, `REQUIRED_FIELD_MISSING` without
    /// `plan`, `source_label` or `items`, `RECEIPT_BINDING_INVALID` for
    /// another plan or label,
    /// `FIELD_DOMAIN_VIOLATION` for a malformed row or document, and the read's
    /// refusal.
    pub fn read(path: &std::path::Path, plan: &std::path::Path, source: &str) -> Result<Self> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .refuse_at("closure::read")?;
        let mut bytes = Vec::new();
        file.take(LEDGER_LIMIT + 1)
            .read_to_end(&mut bytes)
            .refuse_at("closure::read")?;
        if bytes.len() as u64 > LEDGER_LIMIT {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
        let mut ledger = Self::parse(text, plan, source)?;
        path.clone_into(&mut ledger.path);
        Ok(ledger)
    }

    /// [`AttestationLedger::read`] on a document already in memory.
    ///
    /// # Errors
    /// As [`AttestationLedger::read`], less the file refusals.
    pub fn parse(text: &str, plan: &std::path::Path, source: &str) -> Result<Self> {
        let document = Json::parse(text)?;
        if document.get("schema").and_then(Json::as_str) != Some("bulkload.closure-ledger.v1") {
            return Err(BulkloadRefusal::SchemaMismatch);
        }
        // #133: both bindings are required; a ledger naming neither would
        // otherwise be accepted for any plan.
        let named = document
            .get("plan")
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let named = std::path::Path::new(
            named
                .as_str()
                .ok_or(BulkloadRefusal::FieldDomainViolation)?,
        );
        let same = match (std::fs::canonicalize(named), std::fs::canonicalize(plan)) {
            (Ok(named), Ok(plan)) => named == plan,
            _ => named == plan,
        };
        if !same {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        let label = document
            .get("source_label")
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        if label.as_str() != Some(source) {
            return Err(BulkloadRefusal::ReceiptBindingInvalid);
        }
        let Some(Json::Array(items)) = document.get("items") else {
            return Err(BulkloadRefusal::RequiredFieldMissing);
        };
        let text = |row: &Json, key: &str| -> Result<Option<String>> {
            match row.get(key) {
                None | Some(Json::Null) => Ok(None),
                Some(Json::String(value)) => Ok(Some(value.clone())),
                Some(_) => Err(BulkloadRefusal::FieldDomainViolation),
            }
        };
        let mut rows = Vec::with_capacity(items.len());
        for row in items {
            let item = text(row, "item")?
                .filter(|item| item.len() == 64 && item.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            let capture = match row.get("capture") {
                None => AttestedCapture::Missing,
                Some(Json::Null) => AttestedCapture::Null,
                Some(Json::String(digest))
                    if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    AttestedCapture::Digest(digest.to_ascii_lowercase())
                }
                Some(_) => return Err(BulkloadRefusal::FieldDomainViolation),
            };
            rows.push(AttestationRow {
                item,
                source: text(row, "source")?,
                capture,
                disposition: text(row, "disposition")?
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?,
                basis: text(row, "basis")?,
                refusal: text(row, "refusal")?,
                evidence: row.get("evidence").cloned(),
            });
        }
        Ok(Self {
            path: PathBuf::new(),
            rows,
        })
    }
}

/// The largest attestation ledger read: 16 MiB, as for a plan.
const LEDGER_LIMIT: u64 = 16 * 1024 * 1024;

/// The attestation block of a closure report (#95).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attested {
    pub ledger: PathBuf,
    /// Accepted rows: each closes one natively unaccounted planned item.
    pub items: Vec<AttestationRow>,
    /// Accepted rows by claimed disposition.
    pub dispositions: std::collections::BTreeMap<String, u64>,
    /// Rows for natively unaccounted items that close nothing, and why.
    pub rejected: Vec<(String, &'static str)>,
    /// Rows for items the native ledger already accounts for: ignored.
    pub superseded: u64,
    /// Superseded rows whose claimed disposition differs from the native one.
    pub disagreements: Vec<String>,
    /// Rows naming items the plan does not hold.
    pub foreign: Vec<String>,
    /// Accepted `refused` rows no review disposes (S4).
    pub pending: Vec<String>,
    /// Accepted `refused` rows and the review that disposes each (S4).
    pub reviews: Vec<(String, crate::disposition::Row)>,
}

impl Attested {
    fn write_json(&self, out: &mut String, report: &Report) {
        let _ = write!(
            out,
            ",\"attested\":{{\"schema\":\"bulkload.closure-ledger.v1\",\"ledger\":{},\"totals\":{{\"attested\":{},\"rejected\":{},\"superseded\":{},\"foreign\":{},\"unaccounted_after_attestation\":{},\"pending_review\":{}}},\"dispositions\":{{",
            json_string(&self.ledger.to_string_lossy()),
            self.items.len(),
            self.rejected.len(),
            self.superseded,
            self.foreign.len(),
            report.remaining_unaccounted(),
            self.pending.len(),
        );
        for (index, (name, count)) in self.dispositions.iter().enumerate() {
            let _ = write!(
                out,
                "{}{}:{count}",
                if index == 0 { "" } else { "," },
                json_string(name)
            );
        }
        out.push_str("},\"items\":[");
        for (index, row) in self.items.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"item\":{},\"source\":{},\"capture\":{},\"disposition\":{},\"basis\":{}",
                json_string(&row.item),
                row.source
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
                match &row.capture {
                    AttestedCapture::Digest(digest) => json_string(digest),
                    AttestedCapture::Null | AttestedCapture::Missing => "null".to_owned(),
                },
                json_string(&row.disposition),
                row.basis
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
            );
            if let Some(code) = &row.refusal {
                let _ = write!(out, ",\"refusal\":{}", json_string(code));
            }
            if let Some((_, review)) = self.reviews.iter().find(|(item, _)| *item == row.item) {
                out.push_str(",\"review\":");
                write_review(out, review);
            }
            if let Some(evidence) = &row.evidence {
                out.push_str(",\"evidence\":");
                evidence.write(out);
            }
            out.push('}');
        }
        out.push_str("],\"rejected\":[");
        for (index, (item, why)) in self.rejected.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"item\":{},\"reason\":{}}}",
                json_string(item),
                json_string(why)
            );
        }
        out.push_str("],\"disagreements\":[");
        for (index, item) in self.disagreements.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(item));
        }
        out.push_str("],\"foreign\":[");
        for (index, item) in self.foreign.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(item));
        }
        out.push_str("],\"pending_review\":[");
        for (index, item) in self.pending.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(item));
        }
        out.push_str("]}");
    }
}

/// A JSON value: just enough to read an attestation ledger and write its
/// evidence back verbatim. Hand-rolled, as the argument parser is: the agent
/// carries no JSON crate (R34 dependency wall).
///
/// WP3 PR 3 (2026-10-06) considered `serde_json` here and kept this parser.
/// The R34 wall is a closed list (`Cargo.toml`, `tests/dep_graph.rs`): adding
/// a crate to the agent's normal graph is a design change taken back to the
/// plan, not a bump, and the disposition ledger this PR adds is postcard, so
/// nothing new needs JSON. The attestation ledger is the only JSON input and
/// shrinks to the historical cohort rows under WP4, when this parser is
/// deleted with it rather than replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number's source text, validated as a JSON number.
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

/// Nesting deeper than this refuses rather than recurse without bound.
const JSON_DEPTH: usize = 64;

impl Json {
    /// Parse one JSON document; trailing non-whitespace refuses.
    ///
    /// # Errors
    /// `FIELD_DOMAIN_VIOLATION` for anything that is not one JSON value, or
    /// nests deeper than 64 levels.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parser = JsonParser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = parser.value(0)?;
        parser.space();
        if parser.at == parser.bytes.len() {
            Ok(value)
        } else {
            Err(BulkloadRefusal::FieldDomainViolation)
        }
    }

    /// An object member by key (the first, if repeated).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(members) => members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The string, when this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Null, an empty string, an empty array or an empty object.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Null => true,
            Self::String(value) => value.trim().is_empty(),
            Self::Array(values) => values.is_empty(),
            Self::Object(members) => members.is_empty(),
            Self::Bool(_) | Self::Number(_) => false,
        }
    }

    /// Write this value as compact JSON.
    pub fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Number(text) => out.push_str(text),
            Self::String(value) => out.push_str(&json_string(value)),
            Self::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    value.write(out);
                }
                out.push(']');
            }
            Self::Object(members) => {
                out.push('{');
                for (index, (name, value)) in members.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&json_string(name));
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl JsonParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn literal(&mut self, word: &[u8], value: Json) -> Result<Json> {
        if self.bytes.get(self.at..self.at + word.len()) == Some(word) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(BulkloadRefusal::FieldDomainViolation)
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json> {
        if depth > JSON_DEPTH {
            return Err(BulkloadRefusal::FieldDomainViolation);
        }
        self.space();
        match self.peek() {
            Some(b'n') => self.literal(b"null", Json::Null),
            Some(b't') => self.literal(b"true", Json::Bool(true)),
            Some(b'f') => self.literal(b"false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => {
                self.at += 1;
                let mut values = Vec::new();
                self.space();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Json::Array(values));
                }
                loop {
                    values.push(self.value(depth + 1)?);
                    self.space();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Array(values));
                        }
                        _ => return Err(BulkloadRefusal::FieldDomainViolation),
                    }
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut members = Vec::new();
                self.space();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Json::Object(members));
                }
                loop {
                    self.space();
                    if self.peek() != Some(b'"') {
                        return Err(BulkloadRefusal::FieldDomainViolation);
                    }
                    let name = self.string()?;
                    self.space();
                    if self.peek() != Some(b':') {
                        return Err(BulkloadRefusal::FieldDomainViolation);
                    }
                    self.at += 1;
                    members.push((name, self.value(depth + 1)?));
                    self.space();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Object(members));
                        }
                        _ => return Err(BulkloadRefusal::FieldDomainViolation),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(BulkloadRefusal::FieldDomainViolation),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(BulkloadRefusal::FieldDomainViolation),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(BulkloadRefusal::FieldDomainViolation);
            }
        }
        let text = self
            .bytes
            .get(start..self.at)
            .and_then(|text| std::str::from_utf8(text).ok())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
        Ok(Json::Number(text.to_owned()))
    }

    fn hex4(&mut self) -> Result<u32> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
        self.at += 4;
        Ok(digits)
    }

    fn string(&mut self) -> Result<String> {
        // The caller saw the opening quote.
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while matches!(self.peek(), Some(byte) if byte != b'"' && byte != b'\\' && byte >= 0x20)
            {
                self.at += 1;
            }
            out.push_str(
                self.bytes
                    .get(start..self.at)
                    .and_then(|run| std::str::from_utf8(run).ok())
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?,
            );
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escaped = self.peek().ok_or(BulkloadRefusal::FieldDomainViolation)?;
                    self.at += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&high) {
                                if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
                                    return Err(BulkloadRefusal::FieldDomainViolation);
                                }
                                self.at += 2;
                                let low = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&low) {
                                    return Err(BulkloadRefusal::FieldDomainViolation);
                                }
                                0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                            } else {
                                high
                            };
                            out.push(
                                char::from_u32(code)
                                    .ok_or(BulkloadRefusal::FieldDomainViolation)?,
                            );
                        }
                        _ => return Err(BulkloadRefusal::FieldDomainViolation),
                    }
                }
                _ => return Err(BulkloadRefusal::FieldDomainViolation),
            }
        }
    }
}

/// A JSON string literal: quotes, backslashes and control characters escaped.
#[must_use]
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if u32::from(control) < 0x20 || control == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::disposition::{Decision, Row as Review, Scope};
    use crate::outcome::{OutcomeRecord, Unreadable};

    fn present(body: &str) -> JournalState {
        JournalState::Present(body.to_owned())
    }

    fn entry(
        item: char,
        has_workspace: bool,
        record: Option<(&str, Option<&str>)>,
        journal: JournalState,
    ) -> LedgerEntry {
        let source = PathBuf::from(format!("/src/{item}"));
        let capture = match journal {
            JournalState::NoCapture | JournalState::CaptureUnreadable => None,
            JournalState::Absent | JournalState::Present(_) => Some(digest(item)),
        };
        // The test records are written in the legacy string form and read
        // through the legacy reader, as an old ledger is (WP3 PR 3).
        LedgerEntry {
            item: item.to_string().repeat(64),
            source: source.clone(),
            has_workspace,
            record: record.map(|(outcome, reason)| {
                OutcomeRecord::from_legacy(source, outcome, reason.map(str::to_owned))
            }),
            journal,
            capture,
        }
    }

    // A stand-in current capture digest for test item `item`.
    fn digest(item: char) -> String {
        blake3::hash(item.to_string().as_bytes())
            .to_hex()
            .as_str()
            .to_owned()
    }

    #[test]
    fn every_closed_disposition_is_classified() {
        let restored = entry(
            'a',
            true,
            Some(("workspace-restored", None)),
            present("workspace-restored"),
        );
        let previous = entry(
            'b',
            true,
            Some(("previous-workspace-restoration-not-revalidated", None)),
            present("workspace-restored"),
        );
        let refs = entry(
            'c',
            false,
            Some(("refs-imported", None)),
            present("refs-imported"),
        );
        let refused = entry(
            'd',
            true,
            Some(("refused", Some("GIT_INVENTORY_MALFORMED"))),
            JournalState::NoCapture,
        );
        let nest = entry(
            'f',
            true,
            Some(("refused", Some("GIT_NEST_STASHED path=\"x\""))),
            JournalState::Absent,
        );
        assert_eq!(classify(&restored), Disposition::Applied);
        assert_eq!(classify(&previous), Disposition::Applied);
        assert_eq!(classify(&refs), Disposition::ReferencedOnly);
        // S4: natively a typed refusal is pending review; only a review
        // makes it `refused`.
        assert_eq!(
            classify(&refused),
            Disposition::RefusedPendingReview("GIT_INVENTORY_MALFORMED".into())
        );
        assert_eq!(
            classify(&nest),
            Disposition::RefusedPendingReview("GIT_NEST_STASHED".into())
        );
    }

    // #80 review H2: a bare IO or FRAME_CODEC names no cause and does not
    // close an item.
    #[test]
    fn untyped_io_and_frame_codec_refusals_are_unaccounted() {
        for (reason, why) in [
            ("IO (errno 2)", "refusal-untyped-io"),
            ("IO", "refusal-untyped-io"),
            ("FRAME_CODEC", "refusal-untyped-frame-codec"),
        ] {
            let refused = entry(
                'e',
                true,
                Some(("refused", Some(reason))),
                JournalState::NoCapture,
            );
            assert_eq!(
                classify(&refused),
                Disposition::Unaccounted(why),
                "{reason}"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn unprovable_items_are_unaccounted() {
        let mut other_source = entry(
            'i',
            true,
            Some(("workspace-restored", None)),
            present("workspace-restored"),
        );
        if let Some(Ok(record)) = other_source.record.as_mut() {
            record.source = PathBuf::from("/src/elsewhere");
        }
        let cases = [
            (
                entry('a', true, None, JournalState::Absent),
                "no-outcome-record",
            ),
            (
                entry(
                    'b',
                    true,
                    Some(("workspace-restored", None)),
                    JournalState::Absent,
                ),
                "journal-missing",
            ),
            // A refs journal does not prove a workspace restore.
            (
                entry(
                    'c',
                    true,
                    Some(("workspace-restored", None)),
                    present("refs-imported"),
                ),
                "journal-outcome-mismatch",
            ),
            (
                entry(
                    'd',
                    true,
                    Some(("workspace-restored", None)),
                    JournalState::NoCapture,
                ),
                "capture-record-missing",
            ),
            (
                entry(
                    'e',
                    false,
                    Some(("refs-imported", None)),
                    JournalState::CaptureUnreadable,
                ),
                "capture-record-unreadable",
            ),
            // A planned workspace that only got refs is not closed.
            (
                entry(
                    'f',
                    true,
                    Some(("refs-imported", None)),
                    present("refs-imported"),
                ),
                "refs-only-for-workspace-item",
            ),
            (
                entry(
                    'g',
                    false,
                    Some(("workspace-restored", None)),
                    present("workspace-restored"),
                ),
                "outcome-workspace-mismatch",
            ),
            (
                entry('h', true, Some(("refused", None)), JournalState::Absent),
                "refusal-untyped",
            ),
            (
                entry(
                    'j',
                    true,
                    Some(("refused", Some("disk went away"))),
                    JournalState::Absent,
                ),
                "refusal-untyped",
            ),
            (
                entry(
                    'k',
                    true,
                    Some(("capture-reused-after-census", None)),
                    JournalState::Absent,
                ),
                "not-an-apply-outcome",
            ),
            // A legacy outcome string no writer produced (WP3 PR 3).
            (
                entry('m', true, Some(("finished", None)), JournalState::Absent),
                "outcome-unknown",
            ),
            (other_source, "record-source-mismatch"),
            (
                LedgerEntry {
                    record: Some(Err(Unreadable::Codec)),
                    ..entry('l', true, None, JournalState::Absent)
                },
                "outcome-record-unreadable",
            ),
        ];
        for (entry, why) in cases {
            assert_eq!(classify(&entry), Disposition::Unaccounted(why), "{entry:?}");
        }
    }

    #[test]
    fn report_fails_closed_when_any_item_is_unaccounted() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'a',
                    true,
                    Some(("workspace-restored", None)),
                    present("workspace-restored"),
                ),
                entry(
                    'b',
                    true,
                    Some(("refused", Some("CAPTURE_DRIFTED"))),
                    JournalState::Absent,
                ),
                entry(
                    'c',
                    false,
                    Some(("refs-imported", None)),
                    present("refs-imported"),
                ),
                entry('d', true, None, JournalState::Absent),
            ],
            foreign_records: vec!["f".repeat(64)],
            unmatched_journals: vec!["stale.done".to_owned()],
        };
        let report = Report::from_ledger(&ledger);
        assert_eq!(
            (
                report.planned(),
                report.applied,
                report.refused_pending_review,
                report.referenced_only,
                report.unaccounted
            ),
            (4, 1, 1, 1, 1)
        );
        assert!(!report.passes());
        assert_eq!(report.gate(), Err(BulkloadRefusal::ClosureUnaccounted));
        let json = report.to_json();
        assert!(json.starts_with(
            "{\"schema\":\"bulkload.closure.v1\",\"verdict\":\"fail\",\"gate\":\"fail\",\"totals\":{\"planned\":4,\"applied\":1,\"refused\":0,\"refused_pending_review\":1,\"referenced_only\":1,\"unaccounted\":1,\"pending_review\":1},\"refusals\":{\"CAPTURE_DRIFTED\":1}"
        ), "{json}");
        // A typed refusal row names its code, recording site and errno.
        assert!(json.contains(
            "\"disposition\":\"refused-pending-review\",\"refusal\":\"CAPTURE_DRIFTED\",\"site\":\"outcome::legacy\",\"errno\":null}"
        ), "{json}");
        // Each native row names its current capture, so an attestation
        // ledger can bind to it.
        assert!(json.contains(&format!(
            "\"source\":\"/src/a\",\"capture\":\"{}\",\"outcome\":\"workspace-restored\"",
            digest('a')
        )));
        assert!(json.contains(
            "\"source\":\"/src/d\",\"capture\":\"{}\",\"outcome\":null"
                .replace("{}", &digest('d'))
                .as_str()
        ));
        assert!(json.contains(
            "\"disposition\":\"unaccounted\",\"unaccounted_reason\":\"no-outcome-record\""
        ));
        assert!(json.contains("\"outcome\":null"));
        assert!(json.ends_with(&format!(
            "\"foreign_outcome_records\":[\"{}\"],\"unmatched_journals\":[\"stale.done\"]}}",
            "f".repeat(64)
        )));
    }

    #[test]
    fn report_passes_when_every_item_is_accounted() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'a',
                    true,
                    Some(("workspace-restored", None)),
                    present("workspace-restored"),
                ),
                entry(
                    'b',
                    true,
                    Some(("refused", Some("GIT_INVENTORY_MALFORMED"))),
                    JournalState::NoCapture,
                ),
            ],
            ..Ledger::default()
        };
        // Every item is accounted natively, but S4 (WP3 PR 3): the typed
        // refusal is pending review, so the gate fails until a review
        // disposes it. This test passed natively before S4 was provable.
        let mut report = Report::from_ledger(&ledger);
        assert!(report.native_passes());
        assert!(!report.passes());
        assert_eq!(report.pending_review(), 1);
        assert!(report
            .to_json()
            .contains("\"verdict\":\"pass\",\"gate\":\"fail\""));
        let mut reviews = crate::disposition::Ledger::new(std::path::Path::new("/plan"), "neo");
        reviews.rows.push(
            Review::new(
                Scope::Item("b".repeat(64)),
                "GIT_INVENTORY_MALFORMED",
                Decision::Accept,
                "jess",
                "2026-10-06",
            )
            .unwrap(),
        );
        report.dispose(&reviews);
        assert!(report.passes());
        assert!(report.native_passes());
        assert_eq!(report.gate(), Ok(()));
        assert_eq!((report.refused, report.refused_pending_review), (1, 0));
        let json = report.to_json();
        assert!(json.contains("\"verdict\":\"pass\",\"gate\":\"pass\""));
        assert!(json.contains(
            "\"disposition\":\"refused\",\"refusal\":\"GIT_INVENTORY_MALFORMED\",\"site\":\"outcome::legacy\",\"errno\":null,\"review\":{\"decision\":\"accept\",\"reviewer\":\"jess\",\"date\":\"2026-10-06\",\"basis\":\"item\"}}"
        ), "{json}");
        assert!(json.contains(
            "\"reviewed\":{\"schema\":\"bulkload.dispositions.v1\",\"ledger\":\"\",\"totals\":{\"rows\":1,\"policies\":0,\"disposed\":1,\"pending_review\":0},\"decisions\":{\"accept\":1},\"policies\":[],\"unmatched\":[]}"
        ), "{json}");
        // An empty plan is trivially closed.
        assert!(Report::from_ledger(&Ledger::default()).passes());
    }

    // #95: a repair recorded into a state directory reads as referenced-only
    // with its exact refs journal, and never closes a planned workspace.
    #[test]
    fn index_repaired_outcomes_are_referenced_only() {
        let repaired = |has_workspace, journal| {
            classify(&entry(
                'r',
                has_workspace,
                Some((crate::estate::INDEX_REPAIRED, None)),
                journal,
            ))
        };
        assert_eq!(
            repaired(false, present("refs-imported")),
            Disposition::ReferencedOnly
        );
        assert_eq!(
            repaired(true, present("refs-imported")),
            Disposition::Unaccounted("index-repaired-for-workspace-item")
        );
        assert_eq!(
            repaired(false, JournalState::Absent),
            Disposition::Unaccounted("journal-missing")
        );
        assert_eq!(
            repaired(false, present("workspace-restored")),
            Disposition::Unaccounted("journal-outcome-mismatch")
        );
    }

    // #95, #133: an attestation row closes only a natively unaccounted item,
    // in its own block; native records, totals and the native verdict are
    // never changed.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn attestation_closes_only_natively_unaccounted_items() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'a',
                    false,
                    Some(("refs-imported", None)),
                    present("refs-imported"),
                ),
                entry('b', false, None, JournalState::NoCapture),
                entry('c', true, None, JournalState::NoCapture),
                entry('d', true, None, JournalState::NoCapture),
                // No record (an untyped IO here would be rejected: see
                // `attestation_cannot_close_an_untyped_refusal`).
                entry('e', true, None, JournalState::Absent),
            ],
            ..Ledger::default()
        };
        let id = |c: char| c.to_string().repeat(64);
        let document = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[
              {{"item":"{a}","source":"/src/a","capture":"{ea}","disposition":"refused","basis":"native-closure-report","refusal":"CAPTURE_DRIFTED"}},
              {{"item":"{b}","source":"/src/b","capture":null,"disposition":"referenced-only","basis":"index-repaired","evidence":{{"receipt_dir":"/r/b","ruling":"R-N39"}}}},
              {{"item":"{c}","source":"/src/c","capture":null,"disposition":"refused","basis":"capture-refused","refusal":"GIT_INVENTORY_MALFORMED","evidence":"/r/c.log"}},
              {{"item":"{d}","source":"/src/d","capture":null,"disposition":"present","basis":"native-closure-report","evidence":"x"}},
              {{"item":"{e}","source":"/src/e","capture":"{ee}","disposition":"source-absent","basis":"audit","evidence":{{"neo":"absent é\n"}}}},
              {{"item":"{f}","source":"/src/f","capture":null,"disposition":"present","basis":"audit","evidence":"y"}}
            ]}}"#,
            a = id('a'),
            b = id('b'),
            c = id('c'),
            d = id('d'),
            e = id('e'),
            f = id('f'),
            ea = digest('a'),
            ee = digest('e').to_ascii_uppercase(),
        );
        let attestation =
            AttestationLedger::parse(&document, std::path::Path::new("/plan"), "neo").unwrap();
        let mut report = Report::from_ledger(&ledger);
        assert_eq!(report.unaccounted, 4);
        report.attest(&attestation).unwrap();
        let attested = report.attested.clone().unwrap();
        // Native totals are untouched.
        assert_eq!((report.referenced_only, report.unaccounted), (1, 4));
        assert_eq!(
            attested
                .items
                .iter()
                .map(|row| row.item.clone())
                .collect::<Vec<_>>(),
            vec![id('b'), id('c'), id('e')]
        );
        // A row claiming native closure for an unaccounted item closes nothing.
        assert_eq!(attested.rejected, vec![(id('d'), "basis-claims-native")]);
        // A natively closed item is superseded; its disagreeing claim is listed.
        assert_eq!(attested.superseded, 1);
        assert_eq!(attested.disagreements, vec![id('a')]);
        assert_eq!(attested.foreign, vec![id('f')]);
        assert_eq!(report.remaining_unaccounted(), 1);
        assert_eq!(report.gate(), Err(BulkloadRefusal::ClosureUnaccounted));
        let json = report.to_json();
        assert!(json.starts_with(
            "{\"schema\":\"bulkload.closure.v1\",\"verdict\":\"fail\",\"gate\":\"fail\",\"totals\":{\"planned\":5,\"applied\":0,\"refused\":0,\"refused_pending_review\":0,\"referenced_only\":1,\"unaccounted\":4,\"pending_review\":1}"
        ), "{json}");
        // The attested refusal of c is a typed refusal: it needs a review.
        assert!(json.contains(
            "\"totals\":{\"attested\":3,\"rejected\":1,\"superseded\":1,\"foreign\":1,\"unaccounted_after_attestation\":1,\"pending_review\":1},\"dispositions\":{\"referenced-only\":1,\"refused\":1,\"source-absent\":1}"
        ), "{json}");
        assert!(
            json.ends_with(&format!("\"pending_review\":[\"{}\"]}}}}", id('c'))),
            "{json}"
        );
        // The attested block carries each row's bound capture.
        assert!(json.contains(&format!(
            "\"source\":\"/src/e\",\"capture\":\"{}\",\"disposition\":\"source-absent\"",
            digest('e')
        )));
        // Evidence is carried back verbatim, re-escaped.
        assert!(
            json.contains("\"evidence\":{\"neo\":\"absent \u{e9}\\n\"}"),
            "{json}"
        );
        assert!(json.contains("\"evidence\":{\"receipt_dir\":\"/r/b\",\"ruling\":\"R-N39\"}"));

        // With the last unaccounted item attested, the gate passes, while
        // the top-level verdict stays the native one (#133).
        let closing = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[
              {{"item":"{b}","source":"/src/b","capture":null,"disposition":"referenced-only","basis":"index-repaired","evidence":"/r/b"}},
              {{"item":"{c}","source":"/src/c","capture":null,"disposition":"refused","basis":"capture-refused","refusal":"GIT_INVENTORY_INTENT_TO_ADD","evidence":"/r/c"}},
              {{"item":"{d}","source":"/src/d","capture":null,"disposition":"present","basis":"parity-audit","evidence":["/r/d"]}},
              {{"item":"{e}","source":"/src/e","capture":"{ee}","disposition":"source-absent","basis":"audit","evidence":"/r/e"}}
            ]}}"#,
            b = id('b'),
            c = id('c'),
            d = id('d'),
            e = id('e'),
            ee = digest('e'),
        );
        let mut report = Report::from_ledger(&ledger);
        report
            .attest(
                &AttestationLedger::parse(&closing, std::path::Path::new("/plan"), "neo").unwrap(),
            )
            .unwrap();
        // Every item is accounted, but the attested refusal of c is pending
        // review (S4, WP3 PR 3); a standing policy for its code disposes it.
        assert_eq!(report.remaining_unaccounted(), 0);
        assert_eq!(report.pending_review(), 1);
        assert!(!report.passes());
        let mut reviews = crate::disposition::Ledger::new(std::path::Path::new("/plan"), "neo");
        reviews.rows.push(
            Review::new(
                Scope::Policy,
                "GIT_INVENTORY_INTENT_TO_ADD",
                Decision::ReCarry,
                "jess",
                "2026-10-06",
            )
            .unwrap(),
        );
        report.dispose(&reviews);
        assert!(report.passes());
        assert!(!report.native_passes());
        assert_eq!(report.gate(), Ok(()));
        let json = report.to_json();
        assert!(
            json.contains("\"verdict\":\"fail\",\"gate\":\"pass\",\"totals\":{\"planned\":5,\"applied\":0,\"refused\":0,\"refused_pending_review\":0,\"referenced_only\":1,\"unaccounted\":4,\"pending_review\":0}"),
            "{json}"
        );
        assert!(json.contains(
            "\"refusal\":\"GIT_INVENTORY_INTENT_TO_ADD\",\"review\":{\"decision\":\"re-carry\",\"reviewer\":\"jess\",\"date\":\"2026-10-06\",\"basis\":\"policy\"}"
        ), "{json}");
    }

    // #133: an attestation never overrides a native applied, refused or
    // referenced-only record, whatever it claims; such rows close nothing
    // and the native disposition stands.
    #[test]
    fn attestation_never_overrides_a_native_record() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'a',
                    true,
                    Some(("workspace-restored", None)),
                    present("workspace-restored"),
                ),
                entry(
                    'b',
                    true,
                    Some(("refused", Some("GIT_DESTINATION_OCCUPIED"))),
                    JournalState::Absent,
                ),
                entry(
                    'c',
                    false,
                    Some(("refs-imported", None)),
                    present("refs-imported"),
                ),
                entry('d', false, None, JournalState::Absent),
            ],
            ..Ledger::default()
        };
        let id = |c: char| c.to_string().repeat(64);
        let row = |item: char, disposition: &str| {
            format!(
                r#"{{"item":"{}","source":"/src/{item}","capture":"{}","disposition":"{disposition}","basis":"audit","evidence":"e","refusal":"GIT_INVENTORY_MALFORMED"}}"#,
                id(item),
                digest(item)
            )
        };
        let document = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[{},{},{}]}}"#,
            row('a', "refused"),
            row('b', "present"),
            row('c', "applied"),
        );
        let mut report = Report::from_ledger(&ledger);
        report
            .attest(
                &AttestationLedger::parse(&document, std::path::Path::new("/plan"), "neo").unwrap(),
            )
            .unwrap();
        let attested = report.attested.clone().unwrap();
        assert!(attested.items.is_empty());
        assert_eq!(attested.superseded, 3);
        assert_eq!(attested.disagreements, vec![id('a'), id('b'), id('c')]);
        // The native dispositions and totals stand.
        assert_eq!(report.rows[0].disposition, Disposition::Applied);
        assert_eq!(
            report.rows[1].disposition,
            Disposition::RefusedPendingReview("GIT_DESTINATION_OCCUPIED".into())
        );
        assert_eq!(report.rows[2].disposition, Disposition::ReferencedOnly);
        assert_eq!(
            (
                report.applied,
                report.refused_pending_review,
                report.referenced_only,
                report.unaccounted
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(report.refusals.get("GIT_INVENTORY_MALFORMED"), None);
        // The one natively unaccounted item is still open: both verdicts fail.
        assert_eq!(report.remaining_unaccounted(), 1);
        assert!(!report.passes());
        assert!(report
            .to_json()
            .contains("\"verdict\":\"fail\",\"gate\":\"fail\""));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn attestation_rows_that_prove_nothing_are_rejected() {
        let ledger = Ledger {
            entries: vec![
                entry('b', false, None, JournalState::NoCapture),
                entry('c', false, None, JournalState::Absent),
                entry('a', false, None, JournalState::CaptureUnreadable),
            ],
            ..Ledger::default()
        };
        let row = |item: char, fields: &str| {
            format!(
                r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/p","source_label":"neo","items":[{{"item":"{}",{fields}}}]}}"#,
                item.to_string().repeat(64)
            )
        };
        let current = format!(
            r#""source":"/src/c","capture":"{}","disposition":"present","basis":"audit","evidence":"e""#,
            digest('c')
        );
        let stale = format!(
            r#""source":"/src/c","capture":"{}","disposition":"present","basis":"audit","evidence":"e""#,
            digest('z')
        );
        let named_for_none = format!(
            r#""source":"/src/b","capture":"{}","disposition":"present","basis":"audit","evidence":"e""#,
            digest('b')
        );
        let cases: Vec<(char, String, &str)> = vec![
            (
                'b',
                r#""source":"/src/other","capture":null,"disposition":"present","basis":"audit","evidence":"e""#.into(),
                "source-mismatch",
            ),
            (
                'b',
                r#""capture":null,"disposition":"present","basis":"audit","evidence":"e""#.into(),
                "source-mismatch",
            ),
            // #133: a row must name the item's current capture.
            (
                'b',
                r#""source":"/src/b","disposition":"present","basis":"audit","evidence":"e""#.into(),
                "capture-missing",
            ),
            ('b', named_for_none, "capture-mismatch"),
            (
                'c',
                r#""source":"/src/c","capture":null,"disposition":"present","basis":"audit","evidence":"e""#.into(),
                "capture-mismatch",
            ),
            ('c', stale, "capture-mismatch"),
            (
                'a',
                r#""source":"/src/a","capture":null,"disposition":"present","basis":"audit","evidence":"e""#.into(),
                "capture-record-unreadable",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"fine","basis":"audit","evidence":"e""#.into(),
                "disposition-unknown",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"present","evidence":"e""#.into(),
                "basis-missing",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"present","basis":"audit""#.into(),
                "evidence-missing",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"present","basis":"audit","evidence":{}"#.into(),
                "evidence-missing",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"refused","basis":"audit","evidence":"e","refusal":"IO""#.into(),
                "refusal-untyped",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"refused","basis":"audit","evidence":"e","refusal":"disk gone""#.into(),
                "refusal-untyped",
            ),
            (
                'b',
                r#""source":"/src/b","capture":null,"disposition":"refused","basis":"audit","evidence":"e""#.into(),
                "refusal-untyped",
            ),
        ];
        for (item, fields, why) in cases {
            let mut report = Report::from_ledger(&ledger);
            report
                .attest(
                    &AttestationLedger::parse(
                        &row(item, &fields),
                        std::path::Path::new("/p"),
                        "neo",
                    )
                    .unwrap(),
                )
                .unwrap();
            let attested = report.attested.clone().unwrap();
            assert_eq!(
                attested.rejected,
                vec![(item.to_string().repeat(64), why)],
                "{fields}"
            );
            assert!(attested.items.is_empty(), "{fields}");
            assert!(!report.passes());
        }
        // The row bound to the current capture closes its item.
        let mut report = Report::from_ledger(&ledger);
        report
            .attest(
                &AttestationLedger::parse(&row('c', &current), std::path::Path::new("/p"), "neo")
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(report.attested.unwrap().items.len(), 1);
        // A capture member that is not a 64-hex digest or null refuses the
        // whole ledger.
        for bad in [r#""capture":"abc""#, r#""capture":7"#] {
            assert_eq!(
                AttestationLedger::parse(
                    &row(
                        'b',
                        &format!(r#""source":"/src/b",{bad},"disposition":"present""#)
                    ),
                    std::path::Path::new("/p"),
                    "neo"
                ),
                Err(BulkloadRefusal::FieldDomainViolation),
                "{bad}"
            );
        }
    }

    #[test]
    fn attestation_ledgers_bind_to_their_plan_label_and_schema() {
        let plan = std::path::Path::new("/plan");
        let parse = |text: &str| AttestationLedger::parse(text, plan, "neo");
        assert_eq!(
            parse(
                r#"{"schema":"bulkload.closure.v1","plan":"/plan","source_label":"neo","items":[]}"#
            ),
            Err(BulkloadRefusal::SchemaMismatch)
        );
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo"}"#),
            Err(BulkloadRefusal::RequiredFieldMissing)
        );
        // #133: both the plan and the SOURCE label are required.
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","items":[]}"#),
            Err(BulkloadRefusal::RequiredFieldMissing)
        );
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","items":[]}"#),
            Err(BulkloadRefusal::RequiredFieldMissing)
        );
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","source_label":"neo","items":[]}"#),
            Err(BulkloadRefusal::RequiredFieldMissing)
        );
        assert_eq!(
            parse(
                r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"sting","items":[]}"#
            ),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            parse(
                r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":null,"items":[]}"#
            ),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            parse(
                r#"{"schema":"bulkload.closure-ledger.v1","plan":"/other","source_label":"neo","items":[]}"#
            ),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert!(parse(
            r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[]}"#
        )
        .is_ok());
        assert_eq!(
            parse(
                r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[{"item":"short","disposition":"present"}]}"#
            ),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        // An item listed twice is ambiguous: the whole ledger refuses.
        let twice = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[{{"item":"{b}","disposition":"present"}},{{"item":"{b}","disposition":"present"}}]}}"#,
            b = "b".repeat(64)
        );
        let mut report = Report::from_ledger(&Ledger {
            entries: vec![entry('b', false, None, JournalState::NoCapture)],
            ..Ledger::default()
        });
        assert_eq!(
            report.attest(&parse(&twice).unwrap()),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        // Without an attestation ledger the report is exactly the native one,
        // and its gate is the native verdict.
        let native = Report::from_ledger(&Ledger::default()).to_json();
        assert!(!native.contains("attested"));
        assert!(native.contains("\"verdict\":\"pass\",\"gate\":\"pass\""));
    }

    #[test]
    fn json_parser_is_closed_and_round_trips() {
        let text =
            r#" {"a":[1,-2.5e3,true,false,null,"x\"\\\/\b\f\n\r\t\u0041\ud83d\ude00"],"b":{}} "#;
        let value = Json::parse(text).unwrap();
        let mut out = String::new();
        value.write(&mut out);
        assert_eq!(
            out,
            "{\"a\":[1,-2.5e3,true,false,null,\"x\\\"\\\\/\\u0008\\u000c\\n\\r\\tA\u{1f600}\"],\"b\":{}}"
        );
        assert_eq!(Json::parse(&out).unwrap(), value);
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "01",
            "1.",
            "-",
            "\"\\x\"",
            "\"\\ud83d\"",
            "\"a\nb\"",
            "nul",
            "[] []",
            "{\"a\":1,}",
        ] {
            assert_eq!(
                Json::parse(bad),
                Err(BulkloadRefusal::FieldDomainViolation),
                "{bad:?}"
            );
        }
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert_eq!(
            Json::parse(&deep),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        let shallow = format!("{}{}", "[".repeat(60), "]".repeat(60));
        assert!(Json::parse(&shallow).is_ok());
    }

    #[test]
    fn json_strings_cannot_break_out() {
        assert_eq!(json_string("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
    }

    // S4 (WP3 PR 3): an item whose own record is an untyped refusal is never
    // closed by an attestation, however well bound the row is, and no review
    // can name its code.
    #[test]
    fn attestation_cannot_close_an_untyped_refusal() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'e',
                    true,
                    Some(("refused", Some("IO (errno 2)"))),
                    JournalState::Absent,
                ),
                entry(
                    'f',
                    true,
                    Some(("refused", Some("FRAME_CODEC"))),
                    JournalState::Absent,
                ),
                entry(
                    'c',
                    true,
                    Some(("refused", Some("disk went away"))),
                    JournalState::Absent,
                ),
            ],
            ..Ledger::default()
        };
        let row = |c: char| {
            format!(
                r#"{{"item":"{}","source":"/src/{c}","capture":"{}","disposition":"source-absent","basis":"audit","evidence":"e"}}"#,
                c.to_string().repeat(64),
                digest(c)
            )
        };
        let document = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[{},{},{}]}}"#,
            row('e'),
            row('f'),
            row('c')
        );
        let mut report = Report::from_ledger(&ledger);
        report
            .attest(
                &AttestationLedger::parse(&document, std::path::Path::new("/plan"), "neo").unwrap(),
            )
            .unwrap();
        let attested = report.attested.clone().unwrap();
        assert!(attested.items.is_empty());
        assert_eq!(
            attested.rejected,
            ['e', 'f', 'c']
                .map(|c| (c.to_string().repeat(64), "native-refusal-untyped"))
                .to_vec()
        );
        assert_eq!(report.remaining_unaccounted(), 3);
        // Nor can a review: no row may name IO or FRAME_CODEC.
        for code in ["IO", "FRAME_CODEC"] {
            assert!(
                Review::new(Scope::Policy, code, Decision::Accept, "jess", "2026-10-06").is_err()
            );
        }
        assert!(!report.passes());
        // The bare IO row still names its errno and the site that recorded it.
        assert!(report.to_json().contains(
            "\"disposition\":\"unaccounted\",\"refusal\":\"IO\",\"site\":\"outcome::legacy\",\"errno\":2,\"unaccounted_reason\":\"refusal-untyped-io\""
        ));
    }

    // S4: item rows dispose exactly their (item, code); a standing policy
    // disposes its code for every item; rows that dispose nothing are listed.
    #[test]
    fn reviews_dispose_typed_refusals_by_item_then_policy() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'a',
                    true,
                    Some(("refused", Some("CAPTURE_DRIFTED"))),
                    JournalState::Absent,
                ),
                entry(
                    'b',
                    true,
                    Some(("refused", Some("CAPTURE_DRIFTED"))),
                    JournalState::Absent,
                ),
                entry(
                    'c',
                    true,
                    Some(("refused", Some("GIT_NEST_STASHED"))),
                    JournalState::Absent,
                ),
                entry(
                    'd',
                    true,
                    Some(("refused", Some("IO"))),
                    JournalState::Absent,
                ),
            ],
            ..Ledger::default()
        };
        let mut reviews = crate::disposition::Ledger::new(std::path::Path::new("/plan"), "neo");
        let review = |scope, code: &str, decision| {
            Review::new(scope, code, decision, "jess", "2026-10-06").unwrap()
        };
        reviews.rows = vec![
            review(
                Scope::Item("a".repeat(64)),
                "CAPTURE_DRIFTED",
                Decision::Abandon,
            ),
            // Names the right item with the wrong code: disposes nothing.
            review(
                Scope::Item("c".repeat(64)),
                "CAPTURE_DRIFTED",
                Decision::Accept,
            ),
            review(
                Scope::Item("d".repeat(64)),
                "CAPTURE_DRIFTED",
                Decision::Accept,
            ),
        ];
        let mut report = Report::from_ledger(&ledger);
        report.dispose(&reviews);
        assert_eq!(
            report
                .rows
                .iter()
                .map(|row| row.disposition.clone())
                .collect::<Vec<_>>(),
            vec![
                Disposition::Refused("CAPTURE_DRIFTED".into()),
                Disposition::RefusedPendingReview("CAPTURE_DRIFTED".into()),
                Disposition::RefusedPendingReview("GIT_NEST_STASHED".into()),
                Disposition::Unaccounted("refusal-untyped-io"),
            ]
        );
        assert_eq!(
            report.reviewed.as_ref().unwrap().unmatched,
            vec![
                ("c".repeat(64), "CAPTURE_DRIFTED".to_owned()),
                ("d".repeat(64), "CAPTURE_DRIFTED".to_owned()),
            ]
        );
        assert_eq!(report.pending_review(), 2);
        reviews
            .rows
            .push(review(Scope::Policy, "CAPTURE_DRIFTED", Decision::ReCarry));
        reviews
            .rows
            .push(review(Scope::Policy, "GIT_NEST_STASHED", Decision::Accept));
        report.dispose(&reviews);
        assert_eq!(report.pending_review(), 0);
        // The item row still wins over the policy for its own item.
        assert_eq!(
            report.rows[0].review.as_ref().map(Review::decision),
            Some(Decision::Abandon)
        );
        assert_eq!(
            report.rows[1].review.as_ref().map(Review::basis),
            Some("policy")
        );
        assert_eq!(report.refusals.get("CAPTURE_DRIFTED"), Some(&2));
        // The bare IO is still unaccounted: the gate stays red.
        assert_eq!((report.refused, report.unaccounted), (3, 1));
        assert!(!report.passes());
        assert!(report.native_passes() == (report.unaccounted == 0));
        let json = report.to_json();
        assert!(
            json.contains("\"decisions\":{\"abandon\":1,\"accept\":1,\"re-carry\":1}"),
            "{json}"
        );
    }

    mod p73 {
        //! P73 CLOSURE-DISPOSITION (WP3 PR 3, S4): closure is green iff every
        //! typed refusal carries a review and no untyped refusal exists. Red
        //! on a generated ledger with one undispositioned refusal or one bare
        //! IO, even when every typed code has a standing policy and the bare
        //! IO is attested.

        use super::*;
        use crate::outcome::Refusal;
        use crate::test_support::prop_config;
        use proptest::prelude::*;

        #[derive(Debug, Clone)]
        enum Kind {
            Applied,
            Referenced,
            /// A typed refusal (index into the typed codes) and how it is
            /// reviewed: 0 none, 1 an item row, 2 a standing policy.
            Typed(usize, u8),
            BareIo(Option<i32>),
            FrameCodec,
            Untyped,
        }

        fn typed_codes() -> Vec<&'static str> {
            BulkloadRefusal::CODES
                .iter()
                .copied()
                .filter(|code| crate::outcome::is_typed_code(code))
                .collect()
        }

        fn kind(untyped: bool) -> BoxedStrategy<Kind> {
            let typed = (0..typed_codes().len(), 0..3_u8).prop_map(|(c, r)| Kind::Typed(c, r));
            if untyped {
                prop_oneof![
                    3 => Just(Kind::Applied),
                    3 => Just(Kind::Referenced),
                    6 => typed,
                    1 => proptest::option::of(any::<i32>()).prop_map(Kind::BareIo),
                    1 => Just(Kind::FrameCodec),
                    1 => Just(Kind::Untyped),
                ]
                .boxed()
            } else {
                prop_oneof![Just(Kind::Applied), Just(Kind::Referenced), typed].boxed()
            }
        }

        fn item(index: usize) -> String {
            format!("{index:064x}")
        }

        fn refused(refusal: Refusal) -> OutcomeRecord {
            OutcomeRecord {
                source: PathBuf::new(),
                outcome: Outcome::Refused(refusal),
                reason: None,
            }
        }

        // The ledger, the review ledger and the model's verdict.
        #[allow(clippy::too_many_lines)]
        fn build(kinds: &[Kind]) -> (Ledger, crate::disposition::Ledger, bool) {
            let codes = typed_codes();
            let mut reviews = crate::disposition::Ledger::new(std::path::Path::new("/plan"), "neo");
            let mut entries = Vec::new();
            for (index, kind) in kinds.iter().enumerate() {
                let (has_workspace, outcome, journal) = match kind {
                    Kind::Applied => (
                        true,
                        Some(Ok(OutcomeRecord {
                            source: PathBuf::new(),
                            outcome: Outcome::WorkspaceRestored,
                            reason: None,
                        })),
                        JournalState::Present("workspace-restored".into()),
                    ),
                    Kind::Referenced => (
                        false,
                        Some(Ok(OutcomeRecord {
                            source: PathBuf::new(),
                            outcome: Outcome::RefsImported,
                            reason: None,
                        })),
                        JournalState::Present("refs-imported".into()),
                    ),
                    Kind::Typed(code, how) => {
                        let code = codes[*code];
                        match how {
                            1 => reviews.rows.push(
                                Review::new(
                                    Scope::Item(item(index)),
                                    code,
                                    Decision::Accept,
                                    "p73",
                                    "2026-10-06",
                                )
                                .unwrap(),
                            ),
                            2 => reviews.rows.push(
                                Review::new(
                                    Scope::Policy,
                                    code,
                                    Decision::Abandon,
                                    "p73",
                                    "2026-10-06",
                                )
                                .unwrap(),
                            ),
                            _ => {}
                        }
                        (
                            true,
                            Some(Ok(refused(
                                Refusal::new(code, "estate::apply", None).unwrap(),
                            ))),
                            JournalState::Absent,
                        )
                    }
                    Kind::BareIo(errno) => {
                        // No review can name it.
                        assert!(Review::new(
                            Scope::Item(item(index)),
                            "IO",
                            Decision::Accept,
                            "p73",
                            "2026-10-06"
                        )
                        .is_err());
                        (
                            true,
                            Some(Ok(refused(
                                Refusal::new("IO", "estate::apply", *errno).unwrap(),
                            ))),
                            JournalState::Absent,
                        )
                    }
                    Kind::FrameCodec => (
                        true,
                        Some(Ok(refused(
                            Refusal::new("FRAME_CODEC", "estate::apply", None).unwrap(),
                        ))),
                        JournalState::Absent,
                    ),
                    Kind::Untyped => (
                        true,
                        Some(Err(Unreadable::RefusalUntyped)),
                        JournalState::Absent,
                    ),
                };
                entries.push(LedgerEntry {
                    item: item(index),
                    source: PathBuf::new(),
                    has_workspace,
                    record: outcome,
                    journal,
                    capture: Some(item(index)),
                });
            }
            // The model: every typed refusal has an item row for itself or a
            // policy row for its code, and nothing is untyped.
            let green = kinds.iter().enumerate().all(|(index, kind)| match kind {
                Kind::Applied | Kind::Referenced => true,
                Kind::Typed(code, _) => reviews.rows.iter().any(|row| {
                    row.code() == codes[*code]
                        && (row.scope() == &Scope::Policy
                            || row.scope() == &Scope::Item(item(index)))
                }),
                Kind::BareIo(_) | Kind::FrameCodec | Kind::Untyped => false,
            });
            (
                Ledger {
                    entries,
                    ..Ledger::default()
                },
                reviews,
                green,
            )
        }

        // An attestation that tries to close every natively unaccounted item.
        fn attest_everything(report: &mut Report) {
            let rows: Vec<String> = report
                .rows
                .iter()
                .filter(|row| matches!(row.disposition, Disposition::Unaccounted(_)))
                .map(|row| {
                    format!(
                        r#"{{"item":"{}","source":"","capture":"{}","disposition":"present","basis":"audit","evidence":"p73"}}"#,
                        row.item, row.item
                    )
                })
                .collect();
            let document = format!(
                r#"{{"schema":"bulkload.closure-ledger.v1","plan":"/plan","source_label":"neo","items":[{}]}}"#,
                rows.join(",")
            );
            report
                .attest(
                    &AttestationLedger::parse(&document, std::path::Path::new("/plan"), "neo")
                        .unwrap(),
                )
                .unwrap();
        }

        proptest! {
            #![proptest_config(prop_config(256))]

            /// Green iff every typed refusal is dispositioned and nothing is
            /// untyped, whatever an attestation claims for the untyped ones.
            #[test]
            fn p73_closure_is_green_iff_every_refusal_is_dispositioned(
                kinds in proptest::collection::vec(kind(true), 0..12),
                attest in any::<bool>(),
            ) {
                let (ledger, reviews, green) = build(&kinds);
                let mut report = Report::from_ledger(&ledger);
                report.dispose(&reviews);
                if attest {
                    attest_everything(&mut report);
                    let attested = report.attested.as_ref().unwrap();
                    prop_assert!(attested.items.is_empty());
                }
                prop_assert_eq!(report.passes(), green, "{:?}", kinds);
                prop_assert_eq!(report.gate().is_ok(), green);
                // Typed refusals are never unaccounted and untyped ones never
                // pending review: the two sets do not mix.
                let typed = kinds.iter().filter(|kind| matches!(kind, Kind::Typed(..))).count() as u64;
                prop_assert_eq!(report.refused + report.refused_pending_review, typed);
                prop_assert_eq!(
                    report.unaccounted,
                    kinds.len() as u64 - typed
                        - kinds.iter().filter(|kind| matches!(kind, Kind::Applied | Kind::Referenced)).count() as u64
                );
            }

            /// A green ledger turns red with one more undispositioned typed
            /// refusal, or one more bare IO, even when every typed code has a
            /// standing policy and the IO item is attested.
            #[test]
            fn p73_one_undispositioned_refusal_or_bare_io_turns_it_red(
                kinds in proptest::collection::vec(kind(false), 0..10),
                code in 0..typed_codes().len(),
                errno in proptest::option::of(any::<i32>()),
            ) {
                let reviewed: Vec<Kind> = kinds
                    .into_iter()
                    .map(|kind| match kind {
                        // Item rows only, so the new refusal's code has no
                        // policy to fall back on.
                        Kind::Typed(code, _) => Kind::Typed(code, 1),
                        other => other,
                    })
                    .collect();
                let (ledger, reviews, green) = build(&reviewed);
                prop_assert!(green);
                let mut report = Report::from_ledger(&ledger);
                report.dispose(&reviews);
                prop_assert!(report.passes());

                let mut pending = reviewed.clone();
                pending.push(Kind::Typed(code, 0));
                let (ledger, reviews, green) = build(&pending);
                prop_assert!(!green);
                prop_assert!(reviews.rows.iter().all(|row| row.scope() != &Scope::Policy));
                let mut report = Report::from_ledger(&ledger);
                report.dispose(&reviews);
                prop_assert!(!report.passes());
                prop_assert_eq!(report.pending_review(), 1);

                let mut bare = reviewed;
                bare.push(Kind::BareIo(errno));
                let (ledger, mut reviews, _) = build(&bare);
                for typed in typed_codes() {
                    reviews.rows.push(Review::new(Scope::Policy, typed, Decision::Accept, "p73", "2026-10-06").unwrap());
                }
                let mut report = Report::from_ledger(&ledger);
                report.dispose(&reviews);
                attest_everything(&mut report);
                prop_assert!(!report.passes());
                prop_assert_eq!(report.pending_review(), 0);
                prop_assert_eq!(report.remaining_unaccounted(), 1);
                prop_assert_eq!(
                    &report.attested.as_ref().unwrap().rejected,
                    &vec![(item(bare.len() - 1), "native-refusal-untyped")]
                );
            }
        }
    }
}
