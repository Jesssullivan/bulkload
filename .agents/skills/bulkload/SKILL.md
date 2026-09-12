---
name: bulkload
description: Continue a live dev-seat migration through TCFS while preserving both hosts’ unique Git and agent state.
---

# Bulkload

The Python engine and its installer are retired. Do not invoke the former
capture/plan/stage/apply/verify/rollback ceremony.

Implementation is in [tummycrypt PR #592](https://github.com/Jesssullivan/tummycrypt/pull/592).
Inspect its current Rust commands and readiness before acting; M0/M1 code is
not a completed mover.

Keep writers running. Include Git, agents, credentials, dots, worktrees and
SQLite; preserve unique content on both hosts and destination machine identity.
Git needs union of refs, objects, stashes, indexes, dirt and administration.
SQLite needs backup-API capture and explicit composition, never raw WAL/SHM copy
or replacement of unique rows. Preserve Home Manager links and credential privacy.

Verify working continuity on Sting and keep unresolved coverage visible.
Reclaim only proven redundant content. Do not add ceremony scripts or harnesses.
The [R25 product bar](https://github.com/Jesssullivan/bulkload/issues/34) requires
incremental resume and measured improvement over rclone on the real corpus.
