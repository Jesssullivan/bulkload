# 2026-10-22: orphaned hand-off blocks, every open row listed

Lane `fix/handoff-orphan-open-rows-20261022` (from main ff2c921). Two gaps
a GloriousFlywheel agent on sting found (read-only evidence:
`sting:~/build/g55-lane0/handoff-check-after.txt`, `open-rows-after.tsv`,
`estate-diff-after.txt`). SPEC rules served: rule 8 (a rerun converges)
and Done 2 (the closure shows nothing unaccounted).

1. **Orphaned blocks.** 27-30 GloriousFlywheel worktrees carry a block of
   source `neo-inplace-20261016` (agent `9854236c1420`) in `AGENTS.md` and
   `CLAUDE.md`, and the main checkout an `i04` block (agent
   `37e7f3866b4e`) for the path it had before it moved
   (`GloriousFlywheel.neo-20261016`, with no record in the main's ledger).
   Their move state (`/srv/fast-local/jess/estate/inplace-stage/move/r02/
   {carry,private}`) was deleted under an operator ruling, so `--check`
   refuses `IO` errno 2 and `--clear` (plain or `--legacy-only`), which
   iterates the plan's items, cannot retire them.
   - `estate-handoff --clear --orphaned --workspace PATH [--workspace
     PATH]... [--keep-going] SOURCE` (`--orphaned` is a flag of `--clear`,
     as `--clear-carried` and `--legacy-only` are; SOURCE stays
     positional, as with `--move-state`). It reads no plan, corpus or
     state.
   - Each target's head chain is walked block by block with
     `render::marker`; a block naming `source \`SOURCE\`` is removed when
     it is the record's (length and blake3) or sealed with a `for=` that
     is this host's identity for SOURCE and the landing the block names
     ("This landing" row), the given path or its canonical form. The two
     sting identities are checked in a unit row
     (`5424308c007efc12`, `919c28cda68e71b4`).
   - The block's bytes include its trailing empty line, which is what the
     render inserted above the file's bytes, so removing them returns the
     file byte for byte. A created file left empty is removed only when
     the record says `created`; otherwise it stays, empty
     (`cleared-empty-kept`).
   - Recorded as `--clear` records it (lock, `handoff.intent` first,
     exchange with the displaced file checked, record and intent
     removed). Refusals: `HANDOFF_BLOCK_MODIFIED` (malformed, nested or
     edited block of SOURCE), `HANDOFF_TARGET_UNSUPPORTED` (symlinked
     target or workspace), `PATH_NOT_ABSOLUTE`; nothing written.
2. **Open rows.** `--check` counted 30 open rows while the block listed 30
   names "and 6 more", and 3 rows had no listed path. `Inputs::open_rows`
   lists every row `open` counts (`ref`, `dest-only`, `sibling`,
   `refused`, `behind`, each with its reason); `open` is its length.
   `--check` and `--dry-run` print one `"kind":"open-row"` line per row
   after the workspace line, uncapped. The block keeps its cap.

Not done here: running the orphaned clear on sting's GloriousFlywheel
workspaces. That is the operator's or the GF lane's step, with this
build:

```
bulkload-agent estate-handoff --clear --orphaned \
  --workspace /srv/fast-local/jess/git/GloriousFlywheel \
  --workspace /srv/fast-local/jess/git/GloriousFlywheel/.worktrees/<name> ... \
  --keep-going neo-inplace-20261016
```

Tests:
- `render::tests::open_rows_are_uncapped_and_their_count_is_open`
- `render::tests::an_orphaned_blocks_identity_is_derivable_from_its_text`
- `handoff::tests::source_blocks_finds_the_sources_blocks_in_the_head_chain_only`
- `handoff::tests::an_orphaned_clear_removes_this_hosts_blocks_byte_for_byte`
- `handoff::tests::an_orphaned_clear_follows_the_record_for_a_created_file`
- `handoff::tests::an_orphaned_clear_refuses_typed_and_writes_nothing`
- `estate_handoff::h58_an_orphaned_block_is_cleared_with_no_plan`
- `estate_handoff::h59_an_orphaned_clear_refuses_malformed_markers_and_links`
- `estate_handoff::h60_every_open_row_is_listed_uncapped`
