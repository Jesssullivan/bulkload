# 2026-10-16: agent-tooling contract correction

Lane: agent-tooling fix subagent, run by the coordinator's workflow in
`bulkload.worktrees/agent-tooling-20261016` (branch
`docs/agent-tooling-20261016`, off `origin/main` `6565621`).

Rulings: R-HOOK-CONVERGENCE-20261004 (Linear TIN-3692 comment `98cf680c`,
amended R-N11 and R-N12), R-N13, and the operator's 2026-10-16 statement
"force push is totally allowe[d]". SPEC C6 and C18 (PR #236, OI-1003-Q194)
for the R-N56 and R-N91 supersessions.

## Done

- `AGENTS.md`: the superseded R-N11 blanket process ban and the R-N12
  "guard-hook refusal is a stop" block are replaced by the
  R-HOOK-CONVERGENCE-20261004 block, plus the operator's force-push
  statement. The "outrank everything else" sentence is gone: later ratified
  operator rulings outrank this file.
- Estate rules: the session bullet uses amended R-N11 (cross-session actions
  need explicit user authorization). The R-N56 freeze line now says it is
  superseded by SPEC C6.
- Agent lanes: the hook-stop bullet (R-N12, R-N80), the R-N92 wording check
  (no such pre-commit exists) and the R-N98 hook-escape ban are removed;
  hook output is advisory. `TINYLAND_ALLOW_LOCAL_BUILD` is an optional
  annotation. R-N91 quiet holds are superseded by never-halt (SPEC C18, R3).
- Durable notes: "delete entries that are no longer true (R-N55)" becomes
  "supersede with a dated entry; never delete history".
- `.agents/skills/bulkload/SKILL.md` and `docs/design.md`: the same R-N56 and
  R-N91 corrections.

## Open

- The bench scripts (`gate_b.py`, `r23_ab.py`) still require
  `--coordinator-quiet` and the R-N81 load gate. Retiring them is a code
  change for its own lane.
- The local root checkout is parked on
  `docs/wayfinding-20260829`, not `main`, so it was not fast-forwarded.
- Lab-owned fixes (gstack force-push text, the Home Manager planner line, the
  global auto-mode block, attribution keys) are routed to Linear TIN-3692.
