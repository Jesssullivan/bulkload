# 2026-10-07 — Q42 lane L7: reuse from a manifest, sidecar before record (P69, P70)

Lane `q42-l7-manifest-reuse`, branch `feat/q42-l7-manifest-reuse-20261007`,
worktree `bulkload.worktrees/q42-l7-manifest-reuse-20261007`, from
`origin/main` `2a795cf` (#199), merged with `600c765` (#164, docs only).
Pushed; no PR opened (the ship stage opens it).

Rulings: OI-1003-Q42, OI-1003-Q45, OI-1003-Q94, R-N13. Validation under
OI-1003-Q85. The lane ran in two sessions: the first wrote the tests, the
fix, the mutants and the model; the second re-read the work, ran
`formal-nv`, wrote the formal README and this note, merged main and ran
`check-fast`. A third session answered the review (see "Review round"
below).

## Commits

| Sha | What |
| -- | -- |
| `798d1bb` | P69 lands red: test only (`tests/git_reuse_cost.rs`) |
| `90789b9` | The fix: `git_carry.rs`, `estate.rs`, `counters.rs`, `fault.rs`, the P70 fault scenarios, the moved pin |
| `2168935` | `GitCarry.tla`, the catalogue and its rendering, `GitCarryCore.hs`, the formal README, `design.md`, the property plan, the decision packet, the verb's help text |
| `5d62e1e` | Merge of `origin/main` `600c765` (#164: the whitepaper and two agent notes; no conflict) |
| `bde91f8` | This note, as first written |
| `test(git-carry): the manifest path's seat rule, and P69's unimproved miss (Q42 L7)` | Review round: the lib test and the seventh P69 row. Its sha is in the branch log and the lane's return receipt: a note cannot name the commit that carries it, and nothing is committed before `check-fast` ends |
| `docs(q42-l7): target 4 is partly done; a miss costs what it did (Q42 L7)` | Review round: the decision packet, the property plan row, `design.md` and this note |

All are signed. No force-push, no rebase.

## What changed

Before this lane a changed capture fetched its whole retained bundle to
learn which blobs it held (179 MB and 13.8 s of CPU for a 5-object change
to the estate's history-heavy item, `docs/plans/2026-10-04-git-engine-decision.md`).

- A capture publishes `{bundle}.reuse`: its regular seats, each with its
  census row and its blob, bound to the bundle by digest
  (`estate::ReuseManifest`, `git_carry::ManifestSeat`, `publish_reuse`).
- The next changed capture reads that list (`retained_manifest`, counted in
  `read_reuse_manifest_bytes`), takes the seats it may reuse by the rule
  `retained_blobs` already had (regular, unchanged stat identity, not
  racy), and asks which blobs it can read with one `cat-file --batch-check`
  in its private repository (`manifest_blobs`). That child comes from the
  `git` builder; it reads the source store through `alternates`, writes
  nothing to the source and takes no lock.
- No miss: the bundle is not opened (`read_source_capture_reuse_bytes` 0).
- A miss (a reusable seat whose blob only the retained bundle holds: dirty
  content that has not moved): counted per seat in `reuse_dirty_misses`,
  and the bundle is fetched as before.
- No manifest (a capture from before L7, a shallow one, a list over
  256 MiB): the bundle is fetched as before. Old STATE and CORPUS need no
  migration.
- A manifest that does not decode, is bound to another digest or names a
  non-blob is never reused: `reuse_unavailable=manifest-mismatch`
  (`ReuseUnavailable::ManifestMismatch`, a value). The pass reads every
  seat and still captures.
- Old readers of a new corpus **ignore the sidecar safely** (they do not
  fail closed): a restore and a whole-capture hit never read it, and an
  engine from before L7 fetches the bundle. Tested by
  `a_restore_and_a_hit_never_read_the_reuse_manifest` and P69's
  no-manifest row.
- Order: the manifest is durable before the `{item}.capture` record. Four
  fault points: `estate.after_bundle_publish`,
  `estate.before_reuse_sidecar`, `estate.after_reuse_sidecar`,
  `estate.after_capture_record`.

## The pin that moved

`tests/git_capture_counters.rs`,
`a_capture_counts_its_pack_its_censuses_and_its_reuse_reads`: after a clean
commit, `read_source_capture_reuse_bytes` was pinned to `size` (the whole
retained bundle) and is now pinned to `0`, with `reuse_dirty_misses == 0`
and `read_reuse_manifest_bytes` equal to the sidecar's length. This is the
P69 law and the lane's intended change. The other pins in that test
(inequality 1's byte pins, `census_walks` 4 for a changed capture and 1 for
a hit) are unchanged.

## Evidence

- **P69 red on main `2a795cf`** (`798d1bb`, 0 of 6 rows pass):
  `read_source_capture_reuse_bytes` 264,202 B (clean commit), 264,204 B
  (ref only) and 268,430 B (dirty seat edited) where 0 is the law;
  history-heavy 36,365,508 B fetched and 2,273 ms of CPU against a 476 ms
  thin rerun (4.8 times).
- **P69 green** (`90789b9`): history-heavy 306 ms against a 288 ms thin
  rerun; 0 bundle bytes read on the no-miss rows.
- **Scratch mutants** (copies under `/srv/cache/jess/q42-l7-manifest-reuse-mut`,
  never the lane worktree):
  - a reuse that fetches the bundle with no miss: P69 fails 4 of 6 rows
    (the three no-miss rows and the CPU row), and all 8 P70 fault rows fail;
  - the record written before the manifest: the 4
    `estate_before_reuse_sidecar_*` and `estate_after_reuse_sidecar_*`
    rows fail ("the record did not move"); P69 stays green, as it should
    (it does not crash);
  - a manifest accepted whatever digest it is bound to:
    `p70_a_manifest_that_does_not_match_its_capture_is_never_reused` fails
    (`("captured", 0, None)` where `("captured", 52,
    Some("manifest-mismatch"))` is the law); P69 stays green.
- **Formal.** `just tla-check`, all 25 GitCarry rows, 1,009 s: 10 PASS, 3
  REACHED, 11 FAIL and the INCONCLUSIVE self-test, each as expected.
  `MC_gc_reuse` passes at 588,517 distinct states (the explorer reaches the
  same count by hand); `MC_gc_neg_reuse_after_record` fails
  `ReuseManifestBeforeRecord` alone. Every earlier pass row keeps its
  count of record. `just tla-render --check`: 81 files current. `just
  formal-nv`: 67 rows matched, 0 differing; `decide_rows.tsv` is
  byte-identical (363 rows; the reference `decide` did not change, only
  the explorer). L7's pending symbol is grounded; the 4 left are L8's.
- **check-fast** ran on this branch's head before the push; its result is
  in the lane's return receipt, since this note is part of what it checks.

## Review round (third session)

Three medium findings; all three were right about the claim.

- **The racy guard had no test on the manifest path.** Added the lib test
  `a_same_size_rewrite_in_the_capture_tick_is_never_reused_from_a_manifest`.
  It calls `manifest_blobs` on a clean checkout, so no case can fall back
  to the bundle path and its own guard, with the same four injected clocks
  as the bundle test (racy tick, tick + 1, settled, future stamp), then a
  settled seat whose mtime, size or mode moved, then `reusable_blobs` end
  to end with no pack read into the private repository.
  Scratch mutants of `manifest_blobs`, in
  `/srv/cache/jess/q42-l7-manifest-reuse-mut` (never the lane worktree),
  filter `in_the_capture_tick`:
  - without `!racy(..)`: the new test fails ("a racy seat is never reused
    from a manifest"); the two older racy tests pass, as the reviewer found;
  - without `seat_equivalent(..)`: the new test fails ("mtime moved");
  - without the `mode_of` comparison: nothing fails. It is an equivalent
    mutant: `seat_equivalent` already compares `mode`, and `mode_of` is a
    function of `kind` and `mode`. The comparison is kept because
    `retained_blobs` has it too.
- **Target 4 was recorded Done on the best case only.** The decision
  packet now says partly done: done for a checkout with no stationary
  dirty seat, unchanged otherwise, not measured on the estate. P69 has a
  seventh row,
  `p69_history_heavy_with_an_unchanged_dirty_seat_still_fetches_its_whole_bundle`,
  which pins the unimproved cost as P68 pins its re-base cost: one 4 KiB
  untracked file beside the 64 MiB history gives `reuse_dirty_misses=1`,
  `read_source_capture_reuse_bytes` = the whole 36,369,739 B bundle and
  `source_bytes_read` = the 9 B the change moved. Measured this session:
  1,700 ms of CPU for that row against 320 ms for the clean history-heavy
  row (thin median 297 ms, hit 53 ms).
- **The code fix the review offered was not taken.** It was: on a miss,
  drop the missed seats and read them from the worktree. That reads a seat
  whose stat identity has not moved, which S3 forbids as written ("a rerun
  reads 0 content bytes for unchanged seats", R25 / R-N58, `docs/slo.md`),
  and every P69 row pins `source_bytes_read` to what the change moved. It
  is sound and it is cheaper; it needs a ruling, not a lane's judgement.
  The review's alternative (re-word the target, pin the cost) is what
  landed.

## Open

- **Ruling wanted:** may a stationary dirty seat whose blob only the
  retained bundle holds be read again from the worktree when its bytes are
  fewer than the bundle's? Today R25 says no and the pass fetches the
  bundle. Until that is ruled, or the dirty blobs are held somewhere
  cheaper, target 4 stays partly done.
- Not measured on the estate corpus. The 179 MB and 13.8 s figures are
  the decision packet's; this lane's numbers are P69's fixture (a 36 MB
  whole-history bundle). The measurement that closes target 4 is the
  history-heavy item's `reuse_dirty_misses` and CPU on a changed capture.
- Review findings left alone, by instruction (low): the lost
  `retained-unreadable` signal on the no-miss path; the CPU ratio as a
  timing flake in `check-fast`; `design.md`'s "No manifest" bullet saying
  a shallow capture's bundle is fetched (it is not: `offered_reuse`
  answers `Shallow` first); the `for-each-ref` assertion removed from
  `apply_and_compare` inside the fix commit `90789b9`, unmentioned until
  now; P70 restoring only after convergence; and the prune race below.
- A changed capture still costs more than a hit (four censuses and an
  export). That is WP7's, not this lane's.
- A dirty seat that does not move is a miss on every pass, so such an item
  still fetches its retained bundle each time. It is counted
  (`reuse_dirty_misses`), not removed. Removing it needs the dirty blobs
  held somewhere cheaper than the bundle; no ruling asks for that yet.
- A blob the source held at the presence check can be pruned by the source
  before the pack is written. I expected the pass to end as any pass does
  when the source's object store is rewritten under it (the
  `ObjectStoreRewritten` drift custody). The review doubts that for a
  loose-only prune, which does not change `objects/pack`, and I have not
  checked. No test drives the race; it is unverified either way.
- The model checks the manifest's order only. What a manifest lists and
  what a pass reuses from it are P69's and P70's (Rust), as the formal
  README says.
- `.reuse` is not authenticated, like every other sidecar (`design.md`,
  "Known limit"). A forged manifest can only name blobs the source holds
  at a seat's exact identity; the binding test forges three.
- `docs/whitepaper/bulkload.md` (#164) still lists L7 as pending. It is
  pinned to a main sha and belongs to its own lane.
- L8 (the re-root window, GC and its CORPUS lock) is untouched.
