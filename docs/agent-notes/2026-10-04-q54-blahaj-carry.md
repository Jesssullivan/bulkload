# 2026-10-05 — Q54 lane: blahaj carries under v1's ref table (sting)

Lane: q54-blahaj-carry, host sting. Worktree
`bulkload.worktrees/q54-blahaj-carry-20261005`, branch
`docs/q54-blahaj-carry-20261005`, cut from origin/main `40adca8` (#182
merged). This lane proves the Q54 precondition on the real blahaj before L5
deletes carry_v2. Rounds 1 and 2 pushed the branch for the coordinator
without a PR; round 3, the recheck-and-ship stage, opened
[PR #184](https://github.com/Jesssullivan/bulkload/pull/184) (not merged).
The file name keeps the dispatch's date; the work ran on 2026-10-05, from
14:25Z to about 15:35Z.

**Result: the Q54 precondition is MET, on the shallow-envelope path, from
an empty corpus.** See
[the evidence page](../evidence/2026-10-05-blahaj-v1-carry.md). Round 2
(below) corrected two claims: blahaj's old-format outcome is not observed,
and pass 2's zero read is a constant, not a measurement.

Rulings cited:

- **OI-1003-Q54, OI-1003-Q55.** v1 must carry refs-heavy repositories
  before carry_v2 is deleted, proved on the real blahaj. This lane is that
  proof. Their text is cited as the dispatch gave it.
- **OI-1003-Q35.** Byte counters, `census_walks`, pack bytes and the
  rusage CPU are admissible; wall time is informational.
- **R-N13.** Every step below cites its ruling, and this note is the
  session record.
- **R-N11, R-N104, R-N92.** No process was signalled, and no PID was
  checked. Waits were on this lane's own background tasks and on files.
- **R-N98.** No hook was bypassed.
- **R-N12, R-N101.** No guard hook refused anything this session.
- **R-N56.** The capture read the real blahaj into private scratch, under
  Q54/Q55's ruling, as test evidence. Nothing was applied to any estate
  destination.

## What was done

1. **Worktree** (OI-1003-Q54, R-N13). Created from origin/main `40adca8`
   as the dispatch specified.
2. **Build** (OI-1003-Q54, Q55). `s3_estate.py build --rev 40adca8 --jobs
   4` under the shared `.check-fast.lock`, at nice 19, with its own target
   dir in private scratch. The tree came from `git archive`. Queued
   14:27:06Z; cargo took 3 min 31 s. Binary sha256 `74a6e0ce…71493ad8`.
3. **Harness** (OI-1003-Q54, Q35):
   `docs/evidence/2026-10-05-blahaj-v1-carry-harness.py`. It is stdlib
   plus `s3_estate`'s Runner, parsers and `git_env`. In blahaj it runs
   only reads, with `GIT_OPTIONAL_LOCKS=0`, plus an lstat census of
   `.git`.
4. **Run** (OI-1003-Q54, Q55, Q35), 14:31:10Z to 14:36:10Z. The steps
   were: census `t0`, baseline `before`, census `t1`, `estate-add-batch`
   (one item, no workspace), `estate-capture` pass 1, census, pass 2,
   census, baseline `after`, census, `git init --bare` destination,
   `estate-apply` (`blahaj-q54`, refs only), compare, destination fsck,
   census `final`.
5. **Evidence** (OI-1003-Q54, R-N13). The page
   `docs/evidence/2026-10-05-blahaj-v1-carry.md` and the record
   `docs/evidence/2026-10-05-blahaj-v1-carry.json`, which holds every
   step, the raw logs and the build record.
6. **Scratch cleanup** (R-N13). Deleted by literal paths: the private
   state, corpus, destination, apply state, plan, baseline listings and the
   build tree and binary. The kept files are `run/q54-blahaj.json`,
   `run/logs/`, `run/census/` and the build log and JSON, under
   `/srv/scratch/jess/tmp/q54-blahaj-carry-20261005/` (scratch, not
   durable; the committed JSON carries them). A census `post-cleanup`
   followed.
7. **Validation** (OI-1001-Q2). `just check-fast` in `nix develop
   .#default`, under the shared flock at nice 10, 14:38:37Z to 14:49:20Z,
   on the tree this note is committed with (the docs below, before this
   line was added). **Green, exit 0:**
   - 29 cargo test results, all ok (747 passed, 0 failed);
   - gitleaks found no leaks;
   - repo-manifest PASS, and CI contract 22 OK.

   The Bash tool moved the command to the background at its 600 s limit.
   It was not stopped, and its exit was awaited.

## Key numbers

| Measure | Value |
|---|---|
| blahaj refs / distinct objects | 122,994 / 2,382 (119,761 canonical carry, 3,233 native, 2 symrefs, 0 stashes, shallow) |
| Old-format header, modelled | 24,439,460 B, over 16 MiB, on the non-shallow path blahaj never takes; blahaj's old-format outcome not observed |
| New header equivalent (shallow-envelope manifest) | 265,281 B; inner inventory 2,393 lines (2,382 tips + table + 10 metadata) |
| pass 1 | `captured`; bundle 229,677,508 B; source read 6,008,771,154 B; CPU 61.08 s; wall 57.5 s |
| pass 2 (unchanged) | `capture-reused-after-census`; 0 bundle and 0 pack bytes; +19 B of private state; `source_bytes_read=0` is the hit path's constant; CPU 0.885 s (1.45 %) |
| apply | `refs-imported`; CPU 38.39 s; wall 35.9 s |
| Exact compare | 0 missing, 0 extra, 0 oid differences; HEAD and its symbolic branch equal; 80/80 nested worktrees equal; shallow frontier equal |
| Difference | 2 non-HEAD symrefs restored as plain refs at the identical oid (documented) |
| S2 | `.git` census identical 7 times (2,884 entries, sha256 `136cb1bd…`) |

## Shas

- origin/main measured: `40adca8043a1bc5075a59979743cdddd5dbadf8f`.
- Harness sha256 `e7597a76…` (the `census t0` step ran `576abfb7…`, which
  differs only by the per-step sha line).
- Round 1 evidence commit: `32f24d1`. Round 2 review-fix commit:
  `4e7c51e`. Round 2 validation record: `04339a6`. Round 3's note commit
  is the head of PR #184.
- Pre-#182 code cited in round 2: main `73952f9` (`shallow.rs` as at
  `3b634ba`).

## Open

1. **A changed rerun of a shallow checkout re-reads every seat** (from the
   code; not measured). `reuse_unavailable=shallow`, and every changed
   pass writes a whole envelope: about 6.0 GB read and 230 MB written per
   changed blahaj pass. This is an S3 delta cost of shallow custody, not of
   the ref format. A mutable copy of blahaj would measure it.
2. **Non-HEAD symref targets are not carried.** Carrying them would need a
   metadata ref, and an operator ruling.
3. **No workspace restore.** About 6.0 GB was not written to shared
   scratch.
4. **Counter placement.** The git half's source reads appear in the item
   receipt (`source_bytes_read`), not in the process counters'
   `read_source_file_bytes`, which is 0 for `estate-capture`.
5. **Linear.** The facts above belong on the Q54 owning issue, but this
   lane has no Linear write in its dispatch. The coordinator should post
   them.
6. **Retained old-format blahaj captures, before L5** (round 2). An old
   capture stays a reuse hit under the new binary, and from the code its
   roughly 24 MB manifest refuses `GIT_INVENTORY_OVER_CAP` at restore.
   Find any such capture in any corpus before L5, or observe the old
   outcome with a one-off pre-#182 capture into scratch.
7. **Pass 2's zero read is not measured** (round 2). Measuring it needs an
   independent read measure for the agent and its git children.

## Round 2: review fixes (2026-10-05, about 14:55Z to 15:12Z)

In this section, "Open 5" and "Open 6" are the evidence page's items. In
this note they are Open 6 and Open 7.

Rulings: OI-1003-Q54, OI-1003-Q55, OI-1003-Q35, R-N13. Docs only; no
agent, git or census command ran against blahaj in this round.

1. **Old-format claim restated** (OI-1003-Q54, R-N13; review medium 1).
   The page said the old format refused blahaj on every pass with
   `GIT_INVENTORY_MALFORMED` at a modelled 24.4 MB header. Nobody observed
   that. blahaj is shallow, so `shared::write_bundle` and
   `write_chained_capped` take `shallow::write_bundle` before `write_full`.
   Main before #182 (`73952f9`, the same `shallow.rs` as `3b634ba`) wrote
   the whole inventory into the envelope manifest with no cap. Only the
   restore-side readers capped it at 16 MiB, refusing `BUDGET_EXCEEDED`.
   Changes:
   - the page now says the 24.4 MB figure models the non-shallow path;
   - MET is scoped to "shallow-envelope path, empty corpus";
   - the page states which #182 header-cap code the run did not reach;
   - Open 5 is the retained-capture migration case.
2. **Pass 2's zero restated** (OI-1003-Q35, R-N13; review medium 2).
   - `source_bytes_read=0` is `Completion::clean`'s constant on the
     `Retained::Hit` path.
   - `read_source_file_bytes` cannot see the git half; it is 0 on pass 1
     too.
   - CPU and wall time are the only physical bound.
   - "Read and wrote 0 bytes" is now "0 bundle and 0 pack bytes; +19 B of
     private state", from the JSON's `state_bytes`, `flush_full_count=1`
     and `flush_dir_count=3`.
   - Open 6 asks for an independent read measure.
3. **Low findings not fixed** (dispatch). They are listed in the
   dispatch's structured result:
   - S2's census covers `.git` only;
   - the comparison's destination digest and metadata exclusion;
   - MET covers 1 of 121 worktree items;
   - hand-made JSON steps and the harness sha;
   - the harness path and ruff;
   - R-N56 versus Q55;
   - the headline is refs only;
   - the scratch inventory and check-fast's tree.
4. **Validation** (OI-1001-Q2). `just check-fast` in `nix develop
   .#default`, in the foreground under the shared flock at nice 10,
   15:06:05Z to 15:11:54Z, on exactly commit `4e7c51e`'s tree. **Green,
   exit 0:**
   - 29 cargo test results, all ok (747 passed, 0 failed);
   - gitleaks found no leaks;
   - repo-manifest PASS, and CI contract 22 OK.

   The commit after `4e7c51e` adds only this item to this note. The log is
   in scratch at `r2/check-fast.log`, which is not durable; the figures
   above are the record.

## Round 3: recheck and ship (2026-10-05, about 15:13Z to 15:35Z)

Rulings: OI-1003-Q54, OI-1003-Q55, OI-1003-Q35, R-N13. This round changed
docs only and opened a PR. No agent, git write or census touched blahaj.

1. **Recheck** (OI-1003-Q54, R-N13). Both medium findings are fixed. The
   code read (`git show`, read only) was main before #182 (`73952f9`) and
   `40adca8`:
   - **Medium 1.** At `73952f9`, `shallow::write_bundle` writes the whole
     `for-each-ref` inventory into the manifest with no cap.
     `custody_manifest` reads it with a 16 MiB limit, and
     `batch_objects::copy_into` refuses `BUDGET_EXCEEDED`. Its callers
     are all restore or attach paths: `import_verified`,
     `prepare_attachment`, `prepare_linked_attachment`, `restore_staged`,
     `restore_linked_staged`, `repair_missing_index_inner` and
     `registered::restore`. The old export path ends at
     `git bundle verify` and never reads the manifest.
   - **Medium 2.** At `40adca8`:
     - `Retained::Hit` returns `Completion::clean`, which has
       `bytes_read: 0`;
     - `retained_capture` on a hit reads only the outer header, through
       `requires_base`;
     - `custody_manifest` maps `BUDGET_EXCEEDED` to
       `GIT_INVENTORY_OVER_CAP`;
     - #182 did not change the capture key, so the premise of page Open 5
       holds.
   - The JSON matches the restated figures: state 709,758,497 B to
     709,758,516 B, corpus 229,677,780 B, `flush_full_count=1` and
     `flush_dir_count=3`.
   - No new medium or high defect was found. Round 3 corrected three small
     things: round 2's end time, its Open cross-references, and this
     note's "no PR" line. The ten low findings stay deferred (round 2,
     item 3).
2. **PR** (OI-1003-Q54, R-N13).
   [#184](https://github.com/Jesssullivan/bulkload/pull/184) was opened
   against main and is not merged. Its body covers what and why, how to
   run, the evidence and the review summary.
3. **Validation** (OI-1001-Q2). `just check-fast` in `nix develop
   .#default`, in the foreground under the shared flock at nice 10, on
   this note's final tree. Only the text of this item differed from the
   committed tree. The run was 15:18:30Z to 15:24:22Z. **Green, exit 0:**
   - 29 cargo test results, all ok (747 passed, 0 failed);
   - gitleaks found no leaks;
   - repo-manifest PASS, and CI contract 22 OK.

   The log is in scratch, which is not durable; these figures are the
   record.

## Workstreams (restated per AGENTS.md, 2026-10-05 about 14:40Z; rechecked 15:05Z and 15:17Z)

These rows come from `gh pr list` and `git worktree list` on sting. They
are **reported, not verified**. The Linear SSOT ledger was not read. At
the 15:17Z recheck:

- the open PRs were #164 and #136, plus this lane's #184 once opened;
- the L6a worktree was still at `40adca8`.

| Stream | Owner | Branch / PR | State (reported) | Next |
|---|---|---|---|---|
| **Q54 blahaj carry (this lane)** | this lane | `docs/q54-blahaj-carry-20261005`, PR #184 | measured; MET (shallow-envelope path, empty corpus); round 3 recheck CLEAN; PR open, not merged | coordinator review and merge; retained old-format captures (Open 6) before L5 |
| Q54 v1 header | lane | PR #182 | merged 13:32Z | none here |
| Q42 L6a decide | lane | worktree `q42-l6a-decide-20261005` at `40adca8` | unknown | unknown |
| Q42 L5 (delete carry_v2) | coordinator | none seen | waits on this proof | coordinator |
| Whitepaper | docs lane | PR #164 | open | review |
| #120 ingest token | lane | PR #136 | open, held by WP0(a) | held |
