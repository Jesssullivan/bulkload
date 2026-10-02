# 2026-10-01: Closure report and destination space preflight

**Lane:** closure-preflight. Ratified by operator interview 2026-10-01 Q2
(OI-1001-Q2). Receipts cite R-N13. Issue bulkload#79 (product bar #34).
Worktree `bulkload.worktrees/closure-preflight-20261001`, branch
`feat/closure-report-space-preflight`, from origin/main 9766971. Built with
its own `CARGO_TARGET_DIR=/srv/fast-local/jess/cargo-target/bulkload-closure-preflight`
on sting.

## Done

- `bulkload-proto`: added `CLOSURE_UNACCOUNTED` and
  `DESTINATION_SPACE_INSUFFICIENT`, plus `BulkloadRefusal::CODES` and
  `is_code`. A test keeps `CODES` equal to the variant codes.
- `space.rs`: `statvfs` probe (with `// SAFETY:`), a pure `check(planned,
  space, percent)` and a process-wide floor. The binary defaults the floor
  to 25 (`--min-free-percent=N`). A library caller gets 0, which still
  refuses a write larger than the space available.
- `transfer::receive`: checks each batch before `WantFiles`. `copy` reports
  the receiver's space refusal rather than the source's broken-pipe error.
- `estate::apply`: `space_plan` sums pending bundle sizes per destination
  filesystem, plus the `jobs` largest bundles on the corpus filesystem for
  staging. It refuses before any item runs.
- `estate::ledger` and `closure.rs`: the `closure-report` verb, with
  `bulkload.closure.v1` JSON.
- `docs/design.md`: added the Closure paragraph under Completion and the
  Space preflight paragraph under Durability.
- Read-only check against sting's 2026-09-22 cohorts, with no estate
  operation (R-N56): cohort1 passes (22 referenced-only, 4 refused IO).
  cohort2b passes (3 applied, 2 referenced-only). **cohort2a fails:**
  169 planned, 146 applied, 22 refused, 1 unaccounted (`no-outcome-record`).

## Open

- No whole-cohort preflight for `pull` until wire v5 carries a plan total.
  Today the check runs per batch (W4 owns the wire).
- `estate-apply` charges bundle size, which is a lower bound on a checkout.
  `estate-capture` (which writes the corpus) has no preflight yet.
- Is the `referenced-only` definition right? It is ref custody without
  working bytes.
- The cohort2a unaccounted item is `7955f55f9872…`
  (`/Users/jess/git/lab-wt/tin-4287-adapter`). None of the three state
  directories holds an outcome record for it. It needs to be resolved.
