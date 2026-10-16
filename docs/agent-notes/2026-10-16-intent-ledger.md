# 2026-10-16 intent ledger and SPEC revision

Lane: intent mining for `docs/SPEC.md` (PR #236, branch `docs/spec-20261016`).
Rulings cited: R-N13; the operator's 2026-10-16 statements PUR-12, PUR-13
and PROC-15 in `docs/intent-ledger.md`.

## Done

- `docs/intent-ledger.md`: 126 verbatim entries in 11 themes, plus "Where
  intent was lost" (L1–L9) and seven conflicts that need an operator ruling.
  Every operator quote was string-matched against its transcript or Codex
  history source; three neo-only entries were matched read-only on neo.
- `docs/SPEC.md`: every requirement now cites ledger IDs. Added Pi, path
  translation, DEST never clobbered (union), the operator's seat check, R4
  (both hosts stay usable), and the open-conflicts list. Gate ranking no
  longer overrides OI-1003-Q97 until the operator rules.
- `AGENTS.md` top block: plans and audits quote the SPEC requirement they
  serve; standing rulings are never re-asked; the ledger is append-only.

## Open

- Operator ratification of SPEC.md and the seven ledger conflicts.
- AGENTS.md R-N11/R-N12 text predates R-HOOK-CONVERGENCE-20261004
  (TIN-3692 comment 98cf680c); not changed in this lane.
