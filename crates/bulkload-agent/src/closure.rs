//! Closure report (OI-1001-Q2): bulkload's own completion gate for an
//! estate apply.
//!
//! Every planned item must end in exactly one of three dispositions, read
//! from the run's durable ledger (outcome records and apply journals, see
//! [`crate::estate::ledger`]):
//!
//! - `applied`: the workspace was restored (`workspace-restored`, or a
//!   re-apply's `previous-workspace-restoration-not-revalidated`) and a
//!   `workspace-restored` journal proves it.
//! - `refused`: the outcome is `refused` and its reason begins with a typed
//!   refusal code from the taxonomy.
//! - `referenced-only`: only ref custody was imported (`refs-imported`, or a
//!   re-apply's `previous-ref-custody-not-workspace-parity`) and a
//!   `refs-imported` journal proves it. The refs are held; no working bytes
//!   were laid down.
//!
//! Anything else is `unaccounted`: no outcome record, an unreadable record, a
//! refusal without a typed code, an applied outcome with no matching journal,
//! or an outcome that is not an apply outcome at all (a capture-stage record,
//! say). The report passes only when `unaccounted` is 0.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::estate::{Ledger, LedgerEntry};
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
    let Some((outcome, reason)) = &entry.record else {
        return Disposition::Unaccounted("no-outcome-record");
    };
    let journaled = |body: &str| entry.journals.iter().any(|journal| journal == body);
    match outcome.as_str() {
        "refused" => {
            // A receipt reason is the refusal's Display: its code, then any
            // escaped detail (`GIT_NEST_... path="..."`, `IO (errno 2)`).
            let code = reason
                .as_deref()
                .and_then(|reason| reason.split_whitespace().next())
                .filter(|code| BulkloadRefusal::is_code(code));
            code.map_or(Disposition::Unaccounted("refusal-untyped"), |code| {
                Disposition::Refused(code.to_owned())
            })
        }
        "workspace-restored" | "previous-workspace-restoration-not-revalidated" => {
            if journaled("workspace-restored") {
                Disposition::Applied
            } else {
                Disposition::Unaccounted("journal-missing")
            }
        }
        "refs-imported" | "previous-ref-custody-not-workspace-parity" => {
            if journaled("refs-imported") {
                Disposition::ReferencedOnly
            } else {
                Disposition::Unaccounted("journal-missing")
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
}

impl Report {
    /// Build the report from a ledger.
    #[must_use]
    pub fn from_ledger(ledger: &Ledger) -> Self {
        let mut report = Self {
            foreign: ledger.foreign.clone(),
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
                outcome: entry.record.as_ref().map(|(outcome, _)| outcome.clone()),
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

    fn entry(item: char, record: Option<(&str, Option<&str>)>, journals: &[&str]) -> LedgerEntry {
        LedgerEntry {
            item: item.to_string().repeat(64),
            source: PathBuf::from(format!("/src/{item}")),
            record: record.map(|(outcome, reason)| (outcome.to_owned(), reason.map(str::to_owned))),
            record_unreadable: false,
            journals: journals.iter().map(|body| (*body).to_owned()).collect(),
        }
    }

    #[test]
    fn every_disposition_is_classified() {
        let restored = entry(
            'a',
            Some(("workspace-restored", None)),
            &["workspace-restored"],
        );
        let previous = entry(
            'b',
            Some(("previous-workspace-restoration-not-revalidated", None)),
            &["workspace-restored"],
        );
        let refs = entry('c', Some(("refs-imported", None)), &["refs-imported"]);
        let refused = entry('d', Some(("refused", Some("GIT_INVENTORY_MALFORMED"))), &[]);
        let errno = entry('e', Some(("refused", Some("IO (errno 2)"))), &[]);
        let nest = entry(
            'f',
            Some(("refused", Some("GIT_NEST_STASHED path=\"x\""))),
            &[],
        );
        assert_eq!(classify(&restored), Disposition::Applied);
        assert_eq!(classify(&previous), Disposition::Applied);
        assert_eq!(classify(&refs), Disposition::ReferencedOnly);
        assert_eq!(
            classify(&refused),
            Disposition::Refused("GIT_INVENTORY_MALFORMED".into())
        );
        assert_eq!(classify(&errno), Disposition::Refused("IO".into()));
        assert_eq!(
            classify(&nest),
            Disposition::Refused("GIT_NEST_STASHED".into())
        );
    }

    #[test]
    fn unprovable_items_are_unaccounted() {
        let cases = [
            (entry('a', None, &[]), "no-outcome-record"),
            (
                entry('b', Some(("workspace-restored", None)), &[]),
                "journal-missing",
            ),
            // A refs journal does not prove a workspace restore.
            (
                entry('c', Some(("workspace-restored", None)), &["refs-imported"]),
                "journal-missing",
            ),
            (
                entry('d', Some(("refs-imported", None)), &[]),
                "journal-missing",
            ),
            (entry('e', Some(("refused", None)), &[]), "refusal-untyped"),
            (
                entry('f', Some(("refused", Some("disk went away"))), &[]),
                "refusal-untyped",
            ),
            (
                entry('g', Some(("capture-reused-after-census", None)), &[]),
                "not-an-apply-outcome",
            ),
            (
                LedgerEntry {
                    record_unreadable: true,
                    ..entry('h', None, &[])
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
                    Some(("workspace-restored", None)),
                    &["workspace-restored"],
                ),
                entry('b', Some(("refused", Some("CAPTURE_DRIFTED"))), &[]),
                entry('c', Some(("refs-imported", None)), &["refs-imported"]),
                entry('d', None, &[]),
            ],
            foreign: vec!["f".repeat(64)],
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
            "\"foreign_outcome_records\":[\"{}\"]}}",
            "f".repeat(64)
        )));
    }

    #[test]
    fn report_passes_when_every_item_is_accounted() {
        let ledger = Ledger {
            entries: vec![
                entry(
                    'a',
                    Some(("workspace-restored", None)),
                    &["workspace-restored"],
                ),
                entry('b', Some(("refused", Some("GIT_INVENTORY_MALFORMED"))), &[]),
            ],
            foreign: Vec::new(),
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
