# 2026-10-03 — whitepaper and bibliography (proof package item one)

**Lane:** `whitepaper`, a workflow subagent on sting. Branch
`docs/whitepaper-20261003` from `origin/main` `57030e1`, worktree
`bulkload.worktrees/whitepaper-20261003`. Not pushed. No PR yet.
**Rulings:** OI-1003-Q7 (proof package: whitepaper and bibliography),
OI-1003-Q23 (as the task gives it), R-N13 (receipts and this note).

## Done

- `docs/whitepaper/bulkload.md` covers the problem, the design, the
  invariants (R25, durability ordering, S2), how each SLO is proven, results,
  related work, future work, and limits. It cites code paths under
  `crates/` and evidence files with their dates.
  - Results quote only `docs/evidence/`.
  - Gate (a) is stated as having no passing verdict.
  - S1 gate (b), the S2 budget and the S3 ratio are marked "pending gate".
  - The TLA+ model is referenced as forthcoming, with no results described.
- `docs/whitepaper/bibliography.md` has 33 entries, each with a "verified
  via" note: Crossref DOI records, USENIX pages with ISBNs, the ANU handle,
  official documentation pages and repositories. The BLAKE3 specification
  PDF was downloaded to the session scratch directory and its text read.
  Every key cited in the paper is defined, and every defined key is cited
  (checked by script).
- `docs/slo.md`, proof-package section: one pointer line to both files.
- `AGENTS.md` is **not** changed. Its "Sources of truth" list names
  authorities to point at. The whitepaper is derived from `design.md`,
  `slo.md` and the evidence, and says it defers to them. Listing it there
  would present it as an authority. The `slo.md` pointer is enough to find
  it.

## Validation

`nice -n 10 nix develop .#default --command just check-fast`, with
`CARGO_TARGET_DIR=/srv/fast-local/jess/cargo-target/whitepaper`: exit 0.
- The lib tests passed 429, with 6 ignored.
- The fault harness, the power-loss proofs (5) and `resume-power-loss` (2)
  were green.
- The contract tests ran 22, OK. gitleaks found no leaks.

This lane changed documentation only.

## Open

- **The 2026-10-03 gate (a) abort is not in `docs/evidence/`.** The task
  brief reports that the run aborted on load. The paper records it as
  reported, not measured. The coordinator should commit the harness's
  ABORTED evidence file (named `r23-2026-10-03-<HHMM>Z`) so that the paper
  can cite it.
- **OI-1003-Q24..Q26 are not on `main`.** They are recorded on the
  coordinator's local branch (`9784467`). The paper describes Q25's
  priority scope with that provenance. Once the amendment merges into
  `docs/slo.md`, the sentence can point there instead.
- **OI-1003-Q23** is cited because the task requires it. Its text is not
  in any repository file this lane could read.
- **Bibliography fields not verified by tool:** the LNCS volume number for
  [TLC99] (omitted); the page range for [B3-18] (omitted); and the body of
  [AppleDiskWrites], which is rendered by script. The paper's Darwin flush
  semantics rest on `io/crash_check.rs`, not on a tool-read of that page.
- **Linear:** no comment was posted from this lane. The distilled fact for
  TIN-4543 is that the proof-package whitepaper draft exists on
  `docs/whitepaper-20261003`.
- Refresh the paper's results section once a gated gate (a) sample on
  corpus v1, the estate corpus (WP0(e)) or the TLA+ model lands.
