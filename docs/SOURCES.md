# Where each fact lives

One canonical place per kind of fact. Every other place points to it, or
carries a label saying it is historical and what superseded it. Written
2026-10-16 under R-N13 (truthing pass, PR "docs: truthing — one source per
fact"). Changing a row of this table is a change to how the repo is governed:
it needs its own PR.

## The map

| Kind of fact | Canonical source | What every other place does |
|---|---|---|
| **Purpose and requirements**: what bulkload must achieve, and when it is done | [`docs/SPEC.md`](SPEC.md), once bulkload#236 merges. Its conflict readings C1–C20 are already standing rulings (OI-1003-Q194, Linear TIN-4543 comment `14484c4a`); until #236 merges, cite that record. | Point to SPEC. `docs/design.md` "Completion", `README.md`, `AGENTS.md`, the whitepaper and the Linear SSOT document do not restate purpose or the completion bar. `docs/slo.md`'s "Completion bar (OI-1003-Q4)" is the exception: SPEC does not supersede it and keeps it as Q4's record (SPEC "Not superseded, and what is corrected"; intent ledger SEAT-10, ONCE-08). It stays as written, with dated pointers to SPEC for what SPEC adds or corrects (C6, C20, the "not when a migration happens to succeed" wording). |
| **The operator's own words** (verbatim intent, with timestamps) | [`docs/intent-ledger.md`](intent-ledger.md), once #236 merges (append-only). | Quote by entry ID. GitHub #34 and `docs/evidence/rulings-ledger-r1-r43.md` R25 are paraphrases and say so. |
| **SLO definitions S1–S5, proof obligations, and the rulings that amend them** | [`docs/slo.md`](slo.md): the core table plus its dated amendments. | Point to slo.md; do not copy gate rules, orders, budgets or numbers (`AGENTS.md`, `docs/design.md`, the whitepaper, the Linear SSOT). |
| **Bulkload rulings as records** (each operator selection, its question and options) | Linear TIN-4543 comments (engine, SLOs, run scope); TIN-3692 (estate and cohort history). | slo.md carries the operative text of the ones that define or amend an SLO and cites the comment. A ruling quoted from workflow task text is not a record. |
| **Lab process rulings** (process control, hooks, receipts, notes) | Lab `AGENTS.md` "Ratified rulings": R-HOOK-CONVERGENCE-20261004 (TIN-3692 comment `98cf680c`). | `AGENTS.md` here keeps one pointer and repo-specific deltas only. |
| **Design**: how bulkload works, its contracts and refusal codes | [`docs/design.md`](design.md). | Crate docs and `USAGE` describe their own surface; the whitepaper explains and cites. |
| **Enforced engineering rules** | The guard that enforces each: the dependency wall in `crates/bulkload-agent/tests/dep_graph.rs`; the deny-panics wall in each crate's `[lints.clippy]` (`Cargo.toml`); the source command registry in `crates/bulkload-agent/tests/source_command_registry.rs`. | Name the guard; do not copy its list. |
| **Validation tiers and CI** | `justfile` recipe comments (`just --list`) and `.github/workflows/ci.yml`. | `AGENTS.md` states the rule ("run `just check-fast` before pushing") and points there. |
| **Formal model**: modules, run order, results of record | [`docs/formal/README.md`](formal/README.md); the row lists are `docs/formal/configs*.tsv`; the rendered file count is what `just tla-render --check` prints. | Link the module's results paragraph; do not restate row or file counts. |
| **Measured results** | [`docs/evidence/`](evidence/): one dated file per sample or run, never edited after the fact except for a dated label under its title. | slo.md records proof status and cites the file; the whitepaper and README link, not restate. |
| **Live workstreams and status** | The Linear document [Bulkload — product shape, SLOs, proofs and live workstreams (SSOT)](https://linear.app/tinyland/document/bulkload-product-shape-slos-proofs-and-live-workstreams-ssot-5e4bab288565) (P-TIN-177). Work items: GitHub issues (`gate-a`, `pre-migration`, `later`). | The document holds pointers, the priority order and the live board; it was rewritten 2026-10-16T17:34Z by the OI-1003-Q194 reconcile. Status, evidence and next for each stream live on its P-TIN-177 milestone and that milestone's live issue. Per the document: when a stream changes state, update its milestone and live issue; edit the document's table only when a live issue is added or retired. Agent notes and chat are not status. |
| **History**: what was done, when, and why | `docs/agent-notes/` (one dated record per session); `docs/plans/` (dated plans); `docs/evidence/rulings-ledger-r1-r43.md`; TIN-4543 and TIN-3692 comments; tag `carry-v2-final`. | Records are never rewritten. A record that is no longer true gets a dated label at its top naming what superseded it. |

## Precedence

- Purpose and done: SPEC (and, until #236 merges, the OI-1003-Q194 record).
- SLOs and their proofs: slo.md.
- How it works: design.md.
- A newer dated operator ruling beats an older document. When one does, the
  document is wrong and gets fixed or labelled; the ruling is not reread to
  fit the document.
- When two dated operator statements conflict and no ruling resolves them,
  no document decides it: both are stated with their dates and the conflict
  is listed below for the operator.

## Open for the operator

- **Builds and tests on neo.** FOOT-03 (2026-08-26): "we CANNOT EVER build
  on neo, EVER"; FOOT-10 (2026-09-01) restates it for tests, checks and
  lints; SPEC R4 cites both. R-N69 (about 2026-09-23, newer) allowed local
  cargo builds on neo behind an inline `TINYLAND_ALLOW_LOCAL_BUILD="<lane>
  (R-N69)"` prefix. Neither is recorded as superseding the other, and
  OI-1003-Q194 (C1–C20) does not address it. `AGENTS.md`, "Agent lanes",
  carries the same entry.
- **R-N81 and SPEC R3.** R-N81's load gate (load1 below 2.5 for a gated
  sample) against SPEC R3's "Samples run on live hosts" (`docs/slo.md`,
  amendment 2026-10-16, R-N91).

## Labels

- **Historical**: a dated record kept as written. Its top line names the date
  of the label and what superseded it.
- **Forward pointer**: inside slo.md, a superseded line keeps its text and
  gains a bracketed `[2026-10-16: …]` pointer to the line that replaced it.
