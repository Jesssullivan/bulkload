---
name: bulkload
description: Move a live development estate (Git, worktrees, agent state, dots, SQLite) between hosts with the bulkload Rust engine while preserving both hosts' unique state.
---

# Bulkload

The engine is this repository's Rust workspace: `bulkload-agent` (verbs:
run it with no arguments for the list) and `bulkload-bench`. Read
`AGENTS.md` and `docs/SOURCES.md` (which file holds which fact) before
acting.

Keep writers running. Include Git, agents, credentials, dots, worktrees and
SQLite; preserve unique content on both hosts and destination machine
identity. Git needs union of refs, objects, stashes, indexes, dirt and
administration. SQLite needs backup-API capture and explicit composition,
never a raw WAL/SHM copy or replacement of unique rows. Preserve Home Manager
links and credential privacy.

Estate operations are not frozen: R-N56's freeze is superseded by
OI-1003-Q142 and OI-1003-Q194 C6 (`docs/slo.md`, amendment 2026-10-16).
Engine work is tracked in Linear project "Bulkload M2: SLO engine". Every
performance claim must come from `bulkload-bench` evidence.
