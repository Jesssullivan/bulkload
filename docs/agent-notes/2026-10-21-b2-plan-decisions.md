# 2026-10-21: B2 plan decisions, no carry-beside, commit-graph retry, home map

Lane `feat/b2-plan-decisions-20261021` (from main 2273723). The coordinator
asked for these four fixes in one PR after the git-20261021 plan-only on the
named lanes. That plan reported only the retired corpus apply's words, would
have carried GloriousFlywheel beside its live path, and landed two items at
raw neo paths.

SPEC rules served: rule 1 (the lane's state is what bulkload's plan says, so
the plan has to say it), rule 3 (no duplicates), rule 4 (a case not merged
yet is a typed hold) and rule 7 (never clobber).

1. **The plan prints the B2 decisions.** `estate-move --plan-only` reads
   each item's B1 snapshot, the live path, and one read-only `negotiate`
   session with the source (`git_carry::preview`; a new verb through
   `pull_command`'s one ssh builder). Each item gets an `estate-move item=`
   line with:
   - the action (create, consolidate, switch, or held with its code);
   - `refs_created`, `refs_forwarded`, `refs_switched`, `refs_tracked` (at
     `refs/remotes/SOURCE`) and `refs_kept`;
   - the source's changed, untracked and ignored files;
   - `land`, `merges` (`git merge-file` against the merge base of the two
     HEADs), `conflicts`, `unknown`;
   - `worktree=main|new|registered`, `asides` and `parents`.

   Each repository gets an `estate-move repository=` line with
   `fetch_bytes` (the source's `rev-list --objects --disk-usage` of the
   wants the live store lacks, not reached from the live refs it holds). A
   tracked ref gets a `ref=… union=tracked at=…` line, and each `.neo-*`,
   `.sting-*` or `.LABEL` sibling an `aside=… action=retire-beside` line.
   `diff.jsonl` now holds the per-file rows from these, not `estate-diff`
   over the retired corpus. The legacy action words are renamed: land is
   create, adopt is consolidate, refs-only is refs-consolidate, refuse is
   held.
2. **No carry-beside.** A refused capture is `held` (reason
   `capture-refused`) wherever its live path is. The carry phase and its
   `--beside` call are deleted. The apply records a typed hold
   (`CAPTURE_ABSENT`, with the source's code as `capture_code`) and writes
   nothing. Old ledgers' carry variants decode and read as holds.
3. **Stale commit-graph.** `run_git` retries a failed read-only Git child
   once with `core.commitGraph=false` (set through `GIT_CONFIG_*`, never
   written to the repository). Test: a graph naming a pruned commit fails
   plain `fsck`, and the snapshot succeeds with the graph bytes unchanged.
4. **Default home map.** The source half records its `$HOME` in
   `carry/move-source.home`. The destination places an item the maps left
   at its own path, under that home, under `--dest-home`/`$HOME`, resolved
   and owner-checked. Absent parents are made 0700 at landing
   (`parents::make_home_parents`). From another host, a path outside both
   is held `DESTINATION_UNMAPPED` (new refusal). `--dest-home` no longer
   needs `--sessions`.

Not done here: the apply does not yet retire the `aside=` duplicates. The
plan lists them, and retiring them (consolidate into the live path, then
remove) is its own lane under rule 3.
