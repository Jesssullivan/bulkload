---
name: bulkload
description: Consolidate a live development estate (Git, worktrees, Claude and Codex homes, dots, SQLite) from neo into sting in place, with bulkload's own verbs, never besides or staging.
---

# Bulkload

Read `docs/SPEC.md` first, then `AGENTS.md`. SPEC.md is the requirement and
wins over every other file.

- Estate bytes move only through bulkload verbs (`bulkload-agent`; run it
  with no arguments for the list). No rsync, rclone, cp, scp, tar or lane
  scripts.
- Land in place at the live path, copied once and streamed. No staging copy,
  bundle, corpus, side root, landing area or beside copy on either machine.
- Where both sides differ, consolidate them into the one live path (git refs
  union and file-by-file merge, append-log union, structural config merge,
  SQLite row union). Never pick a winner. A case bulkload cannot merge yet is
  a typed hold that writes nothing.
- Nothing is excluded. Never halt, signal or slow a live writer on either
  machine.
- Only the operator's own `claude --resume` and `codex resume` on sting says
  a lane is done.
