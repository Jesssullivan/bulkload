//! Closure report (OI-1001-Q2): bulkload's own completion gate for an
//! estate apply.
//!
//! Every planned item must end in exactly one of three dispositions, read
//! from the run's durable ledger (outcome records and apply journals, see
//! [`crate::estate::ledger`]):
//!
//! - `applied`: the item plans a workspace, its outcome is
//!   `workspace-restored` (or a re-apply's
//!   `previous-workspace-restoration-not-revalidated`), and the exact
//!   journal for its current capture and SOURCE says `workspace-restored`.
//! - `refused`: the outcome is `refused` and its reason begins with a typed
//!   refusal code. A bare `IO` or `FRAME_CODEC` names no cause, so it is
//!   not typed enough to close an item.
//! - `referenced-only`: the item plans no workspace, its outcome is
//!   `refs-imported` (or `previous-ref-custody-not-workspace-parity`), and
//!   the exact current-capture journal says `refs-imported`. The refs are
//!   held; no working bytes were laid down, and none were planned.
//!
//! Anything else is `unaccounted`: no outcome record, an unreadable record,
//! a record naming another source, an untyped refusal, an outcome that does
//! not match whether the item plans a workspace, a missing or mismatched
//! current-capture journal (a stale journal from an earlier capture proves
//! nothing), or an outcome that is not an apply outcome at all. The report
//! passes only when `unaccounted` is 0.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::estate::{JournalState, Ledger, LedgerEntry};
use crate::{BulkloadRefusal, Result};

/// One planned item's closure disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Workspace restored, journal present.
    Applied,
    /// Refused with this typed refusal code.
    Refused(String),
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
            Self::ReferencedOnly => "referenced-only",
            Self::Unaccounted(_) => "unaccounted",
        }
    }
}

/// Classify one ledger entry.
#[must_use]
pub fn classify(entry: &LedgerEntry) -> Disposition {
    if entry.record_unreadable {
        return Disposition::Unaccounted("outcome-record-unreadable");
    }
    let Some((source, outcome, reason)) = &entry.record else {
        return Disposition::Unaccounted("no-outcome-record");
    };
    if *source != entry.source {
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
    match outcome.as_str() {
        "refused" => {
            // A receipt reason is the refusal's Display: its code, then any
            // escaped detail (`GIT_NEST_... path="..."`, `IO (errno 2)`).
            match reason
                .as_deref()
                .and_then(|reason| reason.split_whitespace().next())
                .filter(|code| BulkloadRefusal::is_code(code))
            {
                None => Disposition::Unaccounted("refusal-untyped"),
                Some("IO") => Disposition::Unaccounted("refusal-untyped-io"),
                Some("FRAME_CODEC") => Disposition::Unaccounted("refusal-untyped-frame-codec"),
                Some(code) => Disposition::Refused(code.to_owned()),
            }
        }
        "workspace-restored" | "previous-workspace-restoration-not-revalidated" => {
            if entry.has_workspace {
                journal("workspace-restored", Disposition::Applied)
            } else {
                Disposition::Unaccounted("outcome-workspace-mismatch")
            }
        }
        "refs-imported" | "previous-ref-custody-not-workspace-parity" => {
            if entry.has_workspace {
                // A planned workspace that only got refs is not closed.
                Disposition::Unaccounted("refs-only-for-workspace-item")
            } else {
                journal("refs-imported", Disposition::ReferencedOnly)
            }
        }
        _ => Disposition::Unaccounted("not-an-apply-outcome"),
    }
}

/// One planned item in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub item: String,
    pub source: PathBuf,
    pub outcome: Option<String>,
    pub disposition: Disposition,
}

/// The per-item closure ledger and its totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Row>,
    pub applied: u64,
    pub refused: u64,
    pub referenced_only: u64,
    pub unaccounted: u64,
    /// Typed refusal counts by code.
    pub refusals: std::collections::BTreeMap<String, u64>,
    /// Outcome records for items the plan does not hold. Reported, not
    /// counted: they are not planned items.
    pub foreign: Vec<String>,
    /// Journals that are no planned item's current-capture journal (foreign
    /// or stale). Reported, not counted.
    pub unmatched_journals: Vec<String>,
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
            let disposition = classify(entry);
            match &disposition {
                Disposition::Applied => report.applied += 1,
                Disposition::ReferencedOnly => report.referenced_only += 1,
                Disposition::Unaccounted(_) => report.unaccounted += 1,
                Disposition::Refused(code) => {
                    report.refused += 1;
                    *report.refusals.entry(code.clone()).or_default() += 1;
                }
            }
            report.rows.push(Row {
                item: entry.item.clone(),
                source: entry.source.clone(),
                outcome: entry.record.as_ref().map(|(_, outcome, _)| outcome.clone()),
                disposition,
            });
        }
        report
    }

    /// Planned items in the report.
    #[must_use]
    pub const fn planned(&self) -> u64 {
        self.rows.len() as u64
    }

    /// The closure gate: every planned item is accounted for.
    #[must_use]
    pub const fn passes(&self) -> bool {
        self.unaccounted == 0
    }

    /// The gate as a value: `CLOSURE_UNACCOUNTED` when any item is.
    ///
    /// # Errors
    /// `CLOSURE_UNACCOUNTED` when `unaccounted > 0`.
    pub const fn gate(&self) -> Result<()> {
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
            "{{\"schema\":\"bulkload.closure.v1\",\"verdict\":\"{}\",\"totals\":{{\"planned\":{},\"applied\":{},\"refused\":{},\"referenced_only\":{},\"unaccounted\":{}}},\"refusals\":{{",
            if self.passes() { "pass" } else { "fail" },
            self.planned(),
            self.applied,
            self.refused,
            self.referenced_only,
            self.unaccounted,
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
                "{{\"item\":{},\"source\":{},\"outcome\":{},\"disposition\":\"{}\"",
                json_string(&row.item),
                json_string(&row.source.to_string_lossy()),
                row.outcome
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
                row.disposition.name(),
            );
            match &row.disposition {
                Disposition::Refused(code) => {
                    let _ = write!(out, ",\"refusal\":{}", json_string(code));
                }
                Disposition::Unaccounted(why) => {
                    let _ = write!(out, ",\"unaccounted_reason\":{}", json_string(why));
                }
                Disposition::Applied | Disposition::ReferencedOnly => {}
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
        out.push_str("]}");
        out
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
        LedgerEntry {
            item: item.to_string().repeat(64),
            source: source.clone(),
            has_workspace,
            record: record
                .map(|(outcome, reason)| (source, outcome.to_owned(), reason.map(str::to_owned))),
            record_unreadable: false,
            journal,
        }
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
        assert_eq!(
            classify(&refused),
            Disposition::Refused("GIT_INVENTORY_MALFORMED".into())
        );
        assert_eq!(
            classify(&nest),
            Disposition::Refused("GIT_NEST_STASHED".into())
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
        if let Some(record) = other_source.record.as_mut() {
            record.0 = PathBuf::from("/src/elsewhere");
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
            (other_source, "record-source-mismatch"),
            (
                LedgerEntry {
                    record_unreadable: true,
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
                report.refused,
                report.referenced_only,
                report.unaccounted
            ),
            (4, 1, 1, 1, 1)
        );
        assert!(!report.passes());
        assert_eq!(report.gate(), Err(BulkloadRefusal::ClosureUnaccounted));
        let json = report.to_json();
        assert!(json.starts_with(
            "{\"schema\":\"bulkload.closure.v1\",\"verdict\":\"fail\",\"totals\":{\"planned\":4,\"applied\":1,\"refused\":1,\"referenced_only\":1,\"unaccounted\":1},\"refusals\":{\"CAPTURE_DRIFTED\":1}"
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
        let report = Report::from_ledger(&ledger);
        assert!(report.passes());
        assert_eq!(report.gate(), Ok(()));
        assert!(report.to_json().contains("\"verdict\":\"pass\""));
        // An empty plan is trivially closed.
        assert!(Report::from_ledger(&Ledger::default()).passes());
    }

    #[test]
    fn json_strings_cannot_break_out() {
        assert_eq!(json_string("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
    }
}
