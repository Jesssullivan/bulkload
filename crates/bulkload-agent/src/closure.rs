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
//!   `refs-imported` (or `previous-ref-custody-not-workspace-parity`, or
//!   `index-repaired` from `git-repair-missing-index` with a state directory,
//!   #95), and the exact current-capture journal says `refs-imported`. The
//!   refs are held; no working bytes were laid down, and none were planned.
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
        // #95: git-repair-missing-index imported the same ref custody and
        // created the missing index into this state directory.
        crate::estate::INDEX_REPAIRED => {
            if entry.has_workspace {
                Disposition::Unaccounted("index-repaired-for-workspace-item")
            } else {
                journal("refs-imported", Disposition::ReferencedOnly)
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
    /// The attestation block, when an attestation ledger was given (#95).
    pub attested: Option<Attested>,
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

    /// The closure gate: every planned item is accounted for natively, or,
    /// when an attestation ledger was given, natively or by attestation.
    #[must_use]
    pub const fn passes(&self) -> bool {
        self.remaining_unaccounted() == 0
    }

    /// Items accounted for neither natively nor by an accepted attestation.
    #[must_use]
    pub const fn remaining_unaccounted(&self) -> u64 {
        match &self.attested {
            Some(attested) => self.unaccounted.saturating_sub(attested.items.len() as u64),
            None => self.unaccounted,
        }
    }

    /// Join an attestation ledger (`bulkload.closure-ledger.v1`, #95) to this
    /// native report. A row closes a planned item only when the native ledger
    /// leaves it unaccounted; a native record is never overridden. Rows are
    /// reported in their own block, never in the native totals.
    ///
    /// # Errors
    /// Refuses a ledger that is not `bulkload.closure-ledger.v1`, names
    /// another plan or SOURCE label, lists an item twice, or does not parse.
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
                if row.disposition != native.disposition.name() {
                    attested.disagreements.push(row.item.clone());
                }
                continue;
            }
            match row.verdict(&native.source) {
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
        Ok(())
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
        out.push(']');
        if let Some(attested) = &self.attested {
            attested.write_json(&mut out, self);
        }
        out.push('}');
        out
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

/// One attestation ledger row (`bulkload.closure-ledger.v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationRow {
    pub item: String,
    pub source: Option<String>,
    pub disposition: String,
    pub basis: Option<String>,
    pub refusal: Option<String>,
    /// The row's `evidence`, as given; `None` when absent.
    pub evidence: Option<Json>,
}

impl AttestationRow {
    // Whether this row may close a natively unaccounted item whose plan
    // source is `source`, and why not.
    fn verdict(&self, source: &std::path::Path) -> std::result::Result<(), &'static str> {
        if self.source.as_deref().map(std::path::Path::new) != Some(source) {
            return Err("source-mismatch");
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
    /// SOURCE label: a ledger that names another plan (`plan`) or another
    /// label (`source_label`) refuses. Either field may be absent.
    ///
    /// # Errors
    /// `SCHEMA_MISMATCH` for another schema, `REQUIRED_FIELD_MISSING` without
    /// `items`, `RECEIPT_BINDING_INVALID` for another plan or label,
    /// `FIELD_DOMAIN_VIOLATION` for a malformed row or document, and the read's
    /// refusal.
    pub fn read(path: &std::path::Path, plan: &std::path::Path, source: &str) -> Result<Self> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut bytes = Vec::new();
        file.take(LEDGER_LIMIT + 1).read_to_end(&mut bytes)?;
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
        if let Some(named) = document.get("plan") {
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
        }
        if let Some(label) = document.get("source_label") {
            if label.as_str() != Some(source) {
                return Err(BulkloadRefusal::ReceiptBindingInvalid);
            }
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
            rows.push(AttestationRow {
                item,
                source: text(row, "source")?,
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
}

impl Attested {
    fn write_json(&self, out: &mut String, report: &Report) {
        let _ = write!(
            out,
            ",\"attested\":{{\"schema\":\"bulkload.closure-ledger.v1\",\"ledger\":{},\"native_verdict\":\"{}\",\"totals\":{{\"attested\":{},\"rejected\":{},\"superseded\":{},\"foreign\":{},\"unaccounted_after_attestation\":{}}},\"dispositions\":{{",
            json_string(&self.ledger.to_string_lossy()),
            if report.unaccounted == 0 { "pass" } else { "fail" },
            self.items.len(),
            self.rejected.len(),
            self.superseded,
            self.foreign.len(),
            report.remaining_unaccounted(),
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
                "{{\"item\":{},\"source\":{},\"disposition\":{},\"basis\":{}",
                json_string(&row.item),
                row.source
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
                json_string(&row.disposition),
                row.basis
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
            );
            if let Some(code) = &row.refusal {
                let _ = write!(out, ",\"refusal\":{}", json_string(code));
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
        out.push_str("]}");
    }
}

/// A JSON value: just enough to read an attestation ledger and write its
/// evidence back verbatim. Hand-rolled, as the argument parser is: the agent
/// carries no JSON crate (R34 dependency wall).
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

    // #95: an attestation row closes only a natively unaccounted item, in
    // its own block; native records are never overridden.
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
                entry(
                    'e',
                    true,
                    Some(("refused", Some("IO (errno 2)"))),
                    JournalState::NoCapture,
                ),
            ],
            ..Ledger::default()
        };
        let id = |c: char| c.to_string().repeat(64);
        let document = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","source_label":"neo","items":[
              {{"item":"{a}","source":"/src/a","disposition":"refused","basis":"native-closure-report","refusal":"CAPTURE_DRIFTED"}},
              {{"item":"{b}","source":"/src/b","disposition":"referenced-only","basis":"index-repaired","evidence":{{"receipt_dir":"/r/b","ruling":"R-N39"}}}},
              {{"item":"{c}","source":"/src/c","disposition":"refused","basis":"capture-refused","refusal":"GIT_INVENTORY_MALFORMED","evidence":"/r/c.log"}},
              {{"item":"{d}","source":"/src/d","disposition":"present","basis":"native-closure-report","evidence":"x"}},
              {{"item":"{e}","source":"/src/e","disposition":"source-absent","basis":"audit","evidence":{{"neo":"absent \u00e9\n"}}}},
              {{"item":"{f}","source":"/src/f","disposition":"present","basis":"audit","evidence":"y"}}
            ]}}"#,
            a = id('a'),
            b = id('b'),
            c = id('c'),
            d = id('d'),
            e = id('e'),
            f = id('f'),
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
            "{\"schema\":\"bulkload.closure.v1\",\"verdict\":\"fail\",\"totals\":{\"planned\":5,\"applied\":0,\"refused\":0,\"referenced_only\":1,\"unaccounted\":4}"
        ));
        assert!(json.contains(
            "\"native_verdict\":\"fail\",\"totals\":{\"attested\":3,\"rejected\":1,\"superseded\":1,\"foreign\":1,\"unaccounted_after_attestation\":1},\"dispositions\":{\"referenced-only\":1,\"refused\":1,\"source-absent\":1}"
        ), "{json}");
        // Evidence is carried back verbatim, re-escaped.
        assert!(
            json.contains("\"evidence\":{\"neo\":\"absent \u{e9}\\n\"}"),
            "{json}"
        );
        assert!(json.contains("\"evidence\":{\"receipt_dir\":\"/r/b\",\"ruling\":\"R-N39\"}"));

        // With the last unaccounted item attested, the gate passes.
        let closing = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","items":[
              {{"item":"{b}","source":"/src/b","disposition":"referenced-only","basis":"index-repaired","evidence":"/r/b"}},
              {{"item":"{c}","source":"/src/c","disposition":"refused","basis":"capture-refused","refusal":"GIT_INVENTORY_INTENT_TO_ADD","evidence":"/r/c"}},
              {{"item":"{d}","source":"/src/d","disposition":"present","basis":"parity-audit","evidence":["/r/d"]}},
              {{"item":"{e}","source":"/src/e","disposition":"source-absent","basis":"audit","evidence":"/r/e"}}
            ]}}"#,
            b = id('b'),
            c = id('c'),
            d = id('d'),
            e = id('e'),
        );
        let mut report = Report::from_ledger(&ledger);
        report
            .attest(
                &AttestationLedger::parse(&closing, std::path::Path::new("/plan"), "neo").unwrap(),
            )
            .unwrap();
        assert!(report.passes());
        assert!(report.to_json().contains("\"verdict\":\"pass\""));
    }

    #[test]
    fn attestation_rows_that_prove_nothing_are_rejected() {
        let ledger = Ledger {
            entries: vec![entry('b', false, None, JournalState::NoCapture)],
            ..Ledger::default()
        };
        let row = |fields: &str| {
            format!(
                r#"{{"schema":"bulkload.closure-ledger.v1","items":[{{"item":"{}",{fields}}}]}}"#,
                "b".repeat(64)
            )
        };
        for (fields, why) in [
            (
                r#""source":"/src/other","disposition":"present","basis":"audit","evidence":"e""#,
                "source-mismatch",
            ),
            (
                r#""disposition":"present","basis":"audit","evidence":"e""#,
                "source-mismatch",
            ),
            (
                r#""source":"/src/b","disposition":"fine","basis":"audit","evidence":"e""#,
                "disposition-unknown",
            ),
            (
                r#""source":"/src/b","disposition":"present","evidence":"e""#,
                "basis-missing",
            ),
            (
                r#""source":"/src/b","disposition":"present","basis":"audit""#,
                "evidence-missing",
            ),
            (
                r#""source":"/src/b","disposition":"present","basis":"audit","evidence":{}"#,
                "evidence-missing",
            ),
            (
                r#""source":"/src/b","disposition":"refused","basis":"audit","evidence":"e","refusal":"IO"}"#
                    .trim_end_matches('}'),
                "refusal-untyped",
            ),
            (
                r#""source":"/src/b","disposition":"refused","basis":"audit","evidence":"e","refusal":"disk gone""#,
                "refusal-untyped",
            ),
            (
                r#""source":"/src/b","disposition":"refused","basis":"audit","evidence":"e""#,
                "refusal-untyped",
            ),
        ] {
            let mut report = Report::from_ledger(&ledger);
            report
                .attest(
                    &AttestationLedger::parse(&row(fields), std::path::Path::new("/p"), "neo")
                        .unwrap(),
                )
                .unwrap();
            let attested = report.attested.clone().unwrap();
            assert_eq!(attested.rejected, vec![("b".repeat(64), why)], "{fields}");
            assert!(!report.passes());
        }
    }

    #[test]
    fn attestation_ledgers_bind_to_their_plan_label_and_schema() {
        let plan = std::path::Path::new("/plan");
        let parse = |text: &str| AttestationLedger::parse(text, plan, "neo");
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure.v1","items":[]}"#),
            Err(BulkloadRefusal::SchemaMismatch)
        );
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1"}"#),
            Err(BulkloadRefusal::RequiredFieldMissing)
        );
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","source_label":"sting","items":[]}"#),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert_eq!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","plan":"/other","items":[]}"#),
            Err(BulkloadRefusal::ReceiptBindingInvalid)
        );
        assert!(
            parse(r#"{"schema":"bulkload.closure-ledger.v1","plan":"/plan","items":[]}"#).is_ok()
        );
        assert_eq!(
            parse(
                r#"{"schema":"bulkload.closure-ledger.v1","items":[{"item":"short","disposition":"present"}]}"#
            ),
            Err(BulkloadRefusal::FieldDomainViolation)
        );
        // An item listed twice is ambiguous: the whole ledger refuses.
        let twice = format!(
            r#"{{"schema":"bulkload.closure-ledger.v1","items":[{{"item":"{b}","disposition":"present"}},{{"item":"{b}","disposition":"present"}}]}}"#,
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
        // Without an attestation ledger the report is exactly the native one.
        assert!(!Report::from_ledger(&Ledger::default())
            .to_json()
            .contains("attested"));
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
}
