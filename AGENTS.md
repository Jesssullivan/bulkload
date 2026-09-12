# bulkload Development Context

The Python migration engine is retired. Implementation belongs to the TCFS Rust
workspace in [tummycrypt PR #592](https://github.com/Jesssullivan/tummycrypt/pull/592).
M0/M1 work is not a completed M2 mover. Read docs/design.md for the product contract.
Historical rulings and interviews are evidence, not current operational status.

- Keep all operator and agent sessions running. Never signal a process.
- Include Git, agents, credentials, dots, SQLite and worktrees. Preserve both
  hosts’ unique state; an unresolved state class is not an implicit exclusion.
- Git-aware union preserves refs, objects, real stashes, indexes, working bytes
  and worktree administration without changing an active checkout.
- Carry account credentials privately; preserve destination machine identity
  and Home Manager authority. Never print credentials.
- Capture SQLite through its backup API; never copy live WAL/SHM bytes or replace
  a database with unique rows. Preserve conflicts for explicit resolution.
- Reclaim only proven redundant data after preserving unique content.
- Do not recreate the Python engine, stillness ceremony or installer.
- Use existing registered checks and apply_patch; stage explicit paths.
- CI remains on GloriousFlywheel tinyland-nix. Keep .bazelrc.flywheel endpoint-free.

just check validates repository and CI contracts. //:bulkload packages documents,
not an executable. Rust tests and benchmarks belong in the TCFS workspace.
