# Git carry engine decision packet (OI-1003-Q15), 2026-10-04

Rulings: OI-1003-Q15 (measure S3 on the estate corpus first, then choose v1,
hybrid or v2 on the numbers; carry_v2 stays frozen until then), OI-1003-Q18
(the S3 delta inequalities), OI-1003-Q35 (byte counters, `census_walks`,
pack bytes and the rusage CPU ratio are admissible; wall time is
informational), OI-1003-Q38 (the overnight scope, sprint 2 plus fix lanes
for #161 and #162, as recorded in main's
`docs/agent-notes/2026-10-03-coordinator.md` since #167), R-N13.

This packet asks the operator for the Q15 ruling. The three OI-1003-Q18
questions at the end are separate rulings and do not block Q15. The packet
decides nothing itself.

- **Evidence:** [S3 on the estate corpus, sting, 2026-10-04](../evidence/s3-estate-sting-2026-10-04.md),
  measured with `crates/bulkload-bench/scripts/s3_estate.py` and a release
  build of main `4a10bb8` (PRs #150 and #151 were still open). The branch
  later merged main `dfb9604`. Nothing was re-measured on it, and the line
  counts below stay those at `4a10bb8`.
- **Label:** informational, ungated. No gated sample, no power or load gate
  (load1 was 16 to 47 during the passes).
- **Revised after review (same day).** The review changed the conclusions
  but not the measured counters:
  - The verdicts were re-evaluated from the recorded JSON.
  - The large git excesses are re-attributed to v1's group-base exclusion,
    not to the snapshot layer.
  - Two designed v1 history costs the runs could not reach are added: the
    chained re-base, and grouped items that never chain. Scratch probes of
    the measured binary show both.
  - The bare-mirror coverage gap is stated.
  - The metadata allowance is reframed and decoupled from Q15.

## The question

[docs/slo.md](../slo.md) WP0(a) holds three options open:

- **v1:** the production engine. One bundle per estate item per changed
  capture: refs, objects new since the prerequisite, and the item's staged,
  worktree, untracked and ignored snapshot. Restore, closure and the cohort
  evidence are built on it.
- **Hybrid:** the v1 estate layer (the per-item snapshot, census, drift
  custody, closure) with v2's negotiation for history (thin packs against the
  destination's tips).
- **v2:** carry_v2 as the estate transport. It is 4,767 production lines at
  main, compiled into every build and reachable from no verb.

## Per-engine numbers on the same corpus

v1 is measured: `estate-capture` at main `4a10bb8`. v2 is a projection:
`git-carry-estimate` of every repository against a model of what a v2
destination would hold after the pass before (an empty bare repository for
the first pass; a `clone --mirror --no-local` plus every stash entry
afterwards). `missing_thin_pack_bytes` is what v2's sender would put on the
wire for refs and history (R-N74, R-N97). v2 carries no worktree, index,
untracked or ignored state, so for the estate it would still need v1's
snapshot layer; its column is a lower bound for v2 and the history half of
hybrid. carry_v2 itself was neither enabled nor run.

**Coverage differs.** v1 cannot capture a bare repository today. Plan
`rest` refused all 4 bare mirrors (`IO (errno 2)` at `4a10bb8`) and exited
`CONTRACT_SELF_INCONSISTENT` on every pass, while the v2 projection covers
them. The table therefore gives v2 both with and without the mirrors. The
estate rules say an unresolved state class is not an implicit exclusion, so
this is a v1 gap (follow-up 5), not a scope choice.

Bytes are bundle bytes for v1 (`write_source_pack_bytes`) and
`missing_thin_pack_bytes` for v2. The v1 figures are for **unfixed v1**:
they include the base blobs that the group-base exclusion re-packs
(follow-up 1). "Hybrid" is v2's history bytes plus v1's snapshot layer,
bounded from below by the round's changed worktree bytes. It is derived,
not measured. CPU is rusage user plus system of the git half:
`estate-capture` for v1, `git-carry-estimate` for v2. The estimate probes
all 32 repositories every pass, nests and bare mirrors included, hence its
floor of about 5 s.

**Scale estate** (24 repositories plus history-heavy, 58 items, 4.19 GB):

| Engine | First pass | Each unchanged rerun | `mutate-1` (one history-heavy commit) | `mutate-10` (10 kinds) |
|---|---:|---:|---:|---:|
| v1 bytes (measured, unfixed) | 932,119,799 | 0 | 39,352 | 70,619,800 |
| v1 re-base, amortised (derived; not reached in these runs) | — | 0 | about 19.9 M | about 20.1 M |
| v1 CPU s | 118.96 | 4.65 to 5.19 | 18.70 | 15.02 |
| v1 census walks (expected) | 216 (216) | 54 (54) | 57 (57) | 66 (66) |
| v1 worktree-seat bytes read (bound) | 711,653,958 | 0 (0) | 3,209 (3,209) | 67,115,992 (67,115,992) |
| v1 object-store readback, outside the row above (lower bound) | 506,966,016 | 0 | 32,768 | 16,859,136 |
| v2 bytes (projection) | 350,103,767 | 0 | 499 | 1,065 |
| v2 bytes without the 4 bare mirrors v1 refuses | 343,676,638 | 0 | 499 | 1,065 |
| v2 CPU s (the estimate) | 20.65 | 5.15 to 7.18 | 6.65 | 5.19 |
| Hybrid bytes (derived) | 350.1 M + v1's snapshot layer (not separable in unfixed v1) | 0 | about 3,700 + metadata | about 67.1 M + metadata |
| v1 without history-heavy | 752,710,458 | 0 | 0 | 70,580,320 |
| v2 without history-heavy | 170,742,243 | 0 | 0 | 503 |

The re-base row is the item's full bundle divided by
`CHAIN_DEPTH_LIMIT + 1` (9), per changed capture of a chained item. A
chained item writes a self-contained bundle, its whole history, every ninth
changed capture. The figures use history-heavy's 179,409,341 B first-pass
bundle at `mutate-1`. At `mutate-10` they add `r05-bytes`'s 1,388,806 B.
The runs made at most 2 changed captures per item, so they never reached
depth 8. A scratch probe of the measured binary shows the re-base (evidence,
"Probes after the run").

The table has no row for grouped items, the other designed history cost,
because no grouped repository changed twice in these runs. An item in a
multi-item group never chains, and its base is never replaced. So every
changed capture of a grouped item re-packs all history added since the
first pass, once per item in the group. In the probe (2 items, ten 512 KiB
commits), a commit cost about 10.5 MB by the tenth pass, where v2 would
send about 525 KB.

**Scale small** (3 repositories plus history-heavy, 10 items, 18.7 MB): v1
6,977,621 / 0 / 5,753 / 2,239,880 bytes against v2 2,448,913 / 0 / 422 /
947 bytes, in the same column order (v2 without its one bare mirror: 2,407,663
on the first pass).

Most of v1's first-pass excess over v2 is not snapshot layer. The eight
`r00` and `r12` item bundles (546.7 MB) each re-pack the 64 MiB blob that
their group base already holds, along with 876 to 1,530 other blobs. The
group-base path does not exclude the base's trees from the parentless
snapshot commits (evidence finding 4). With that exclusion, about 537 MB
would go, leaving v1's first pass near 395 MB against v2's 343.7 MB over
the same repositories (derived, not measured).

### What the numbers say

1. **Unchanged reruns: no difference.** Both engines send 0 bytes. v1
   reads 0 source bytes and walks one census per item, at about 5 s of CPU
   for 54 items. The projection's probe costs about the same.
2. **History deltas: one history-heavy commit (#48's ref count is not
   modelled).** With #146's auto-prerequisite chain, a history-heavy commit
   costs v1 39,352 B against v2's 499 B. The difference is capture
   metadata: snapshot commits and trees, carry refs, and a bundle header
   that lists the whole private ref inventory. It was 29,570 B at estate,
   1,087 B at small, and 40,324 B for `r05-bytes` at `mutate-10`. It grows
   with ref and tree count, so it is not constant. At gate (b)'s 28 MB/s
   link, that is about 1.4 ms per changed repository per pass.

   In these runs v1 did not re-pack history-heavy's history: before #146
   the same round would have re-packed history-heavy's 243 MB. v1 still
   re-packs history in two designed cases these runs could not reach:
   - a chained item re-bases every ninth changed capture: about 19.9 MB
     per changed capture, amortised, for history-heavy;
   - a grouped item re-packs all history since its first pass on every
     changed capture, once per item in its group.

   19.9 MB is about 500 times the 30 KB to 40 KB metadata difference, and
   about 40,000 times v2's 499 B. Follow-ups 2 and 3 address these costs.
3. **v1's CPU spike is its reuse fetch, not its history pack.** v1 spent
   13.8 s on that 5-object change, because it fetched its whole 179 MB
   retained bundle to reuse blobs. One round later, with a 39 KB retained
   bundle, the same capture took 0.45 s. That cost belongs to v1's reuse
   mechanism and can be fixed there.
4. **First-pass history.** v1's shared group bases plus history-heavy come
   to 335 MB. At most 50 MB more history sits inside the other item
   bundles. About 537 MB more is base blobs re-packed into grouped item
   bundles (item 5). v2's thin packs come to 350.1 MB, or 343.7 MB without
   the four bare mirrors that v1 does not carry.
5. **The large excesses come from v1's group-base exclusion, not from the
   snapshot layer.** `write_with_prerequisites` writes a grouped item's
   bundle with plain `git bundle create --all --stdin` and `^tip` lines.
   That marks only edge commits' trees uninteresting, so the parentless
   snapshot commits re-pack every tracked blob they share with HEAD. The
   chained path already excludes those trees with
   `rev-list --objects-edge-aggressive` (`write_excluding_tip_trees`). The
   excesses this causes:
   - a recaptured linked worktree in a multi-item group re-packs its whole
     checkout: 1.83 MB against a 3.5 KB change at estate;
   - each linked-worktree item of a repository repeats the base's large
     blobs on the first pass: 546.7 MB of 932 MB.

   A hybrid built on unfixed v1 would carry the same bytes. v2's
   negotiation touches neither. Once fixed, the residual is capture
   metadata (item 2).

## WP2 PR 3 deletion inventory (line counts at main 4a10bb8)

WP2 PR 3 of the [architecture review](2026-10-03-architecture-review.md)
deletes v2 if v1 is chosen. Measured with `git show 4a10bb8:<path> | wc -l`:

| What | Lines | Notes |
|---|---:|---|
| `src/git_carry/carry_v2.rs` | 426 | module root |
| `src/git_carry/carry_v2/ingest.rs` | 2,164 | quarantine ingest, journal replay |
| `src/git_carry/carry_v2/journal.rs` | 696 | |
| `src/git_carry/carry_v2/plan.rs` | 628 | |
| `src/git_carry/carry_v2/retry.rs` | 316 | |
| `src/git_carry/carry_v2/negotiate.rs` | 252 | |
| `src/git_carry/carry_v2/lists.rs` | 161 | |
| `src/git_carry/carry_v2/send.rs` | 124 | |
| **carry_v2 production, 8 files** | **4,767** | the review counted 4,399 at 727493a |
| `tests/git_carry_v2.rs` | 4,235 | the review counted 4,167 |
| `tests/fault_harness/git_ingest.rs` | 416 | plus its `mod` and `include_str!` lines in `tests/fault_harness.rs` |
| `src/fault.rs`, the 7 `git_ingest.*` points | about 35 | doc rows, enum variants, the `ALL` table and names |
| `bulkload-proto` `frame.rs`, reserved W6 frames | about 150 | `TAG_PACK_DATA`, `PackDataHeader`, `SubKind`, `RefUpdate`, controls 12 to 19; 12 references in `frame/tests.rs`; a `wire_id` bump |
| `tests/git_m1_spike.rs` | 2,612 | unconditional in the review; evidence doc stays |
| `m1-spike` feature and its `[[test]]` | 10 | `crates/bulkload-agent/Cargo.toml` lines 22 to 25 and 40 to 45, plus two `check-optional` lines in the justfile |
| **Total** | **about 12,225** | |

Not in the inventory: `src/git_carry/estimate.rs` (3,376 lines) and its
`stderr_store`. `git-carry-estimate` is a read-only product verb, and it is
the tool that produced the v2 column above. Keeping it is what lets the
"evidence that would change this" below be gathered on neo's real estate.

## Recommendation

**v1, then WP2 PR 3: delete carry_v2 and the M1 spike** (about 12,200
lines, the inventory above), with five v1 follow-ups under WP2. Each has a
target that can be measured without an unruled allowance:

1. **Group-base prerequisites go through the tip-tree exclusion.**
   `write_with_prerequisites` writes grouped items' bundles with plain
   `bundle create ^tip`. Route it through
   `rev-list --objects-edge-aggressive`, as `write_excluding_tip_trees`
   already does for chained items. This one fix covers both the re-packed
   checkout after a head-move and the repeated first-pass blobs, which the
   first version of this packet listed as two follow-ups. Target: item
   bundles carry 0 copies of base blobs, checked by a pack scan, on the
   first pass and after a `head-move`.
2. **Grouped items chain against the previous capture's tips as well as
   the base.** Today `chain_offer` drops the link whenever a plan base
   exists, and `prepare_base` never replaces the base. Target, over
   several rounds: N successive commits to a grouped repository (N ≥ 10, as
   in the probe) give per-item bundle bytes flat in N. Each capture should
   cost about the commit's new objects plus metadata, not all history since
   the first pass.
3. **Re-base without a full re-pack, or accept the cost.** For example, at
   the depth limit, chain against the previous self-contained bundle's tips
   instead of writing a new one. Restore depth stays bounded, at the cost
   of re-packing at most 8 captures' history. Target: over 20 changed
   captures of a chained item, no capture packs more than the history since
   its chain root. If this is not done, the price of v1 is the item's
   history ÷ 9 per changed capture: about 19.9 MB for history-heavy.
4. **Reuse without fetching the whole retained bundle.** Target: a
   one-commit history delta costs about an unchanged rerun's CPU, not 13.8 s.
5. **Capture bare repositories.** v1 refused every bare mirror on every
   pass, and plan `rest` exited `CONTRACT_SELF_INCONSISTENT`. #151 may type
   the refusal differently at `dfb9604`. Target: plan `rest` captures all 4
   mirrors and exits 0.

Reasons:

- **In these runs, v2's projection saved the capture metadata.** That was
  28 KB to 40 KB per changed chained item per pass at estate.
- **Beyond these runs, v1 re-packs history in two designed cases.** A
  chained item re-bases every ninth changed capture: history ÷ 9 per
  changed capture, about 19.9 MB for history-heavy against v2's 499 B. A
  grouped item re-packs all history since its first pass on every changed
  capture, once per item in its group: about 10.5 MB for a 512 KiB commit
  by the tenth commit in a 2-item probe. Follow-ups 2 and 3 remove both.
  Until they land, they are the price of v1, and they are the strongest
  measured case for v2.
- **First pass.** With follow-up 1, v1 is derived at about 395 MB against
  v2's 343.7 MB over the same repositories. This is not measured.
- **The largest measured git-half failures are a v1 defect, not the
  snapshot layer.** The 1.83 MB head-move and the 546.7 MB of first-pass
  duplication come from the group-base exclusion. Follow-up 1 fixes it
  with a pattern v1 already uses. The remaining residual is capture
  metadata, which v2's projection does not carry because it has no
  snapshot layer. A hybrid would carry it too.
- **v2 would add a large new surface.** That means a frame family, a
  destination ingest and crash surface, a journal, and a second resume
  story for the formal model. The review rejected that (section 4, items 1
  and 2), and it buys the savings above.
- **Hybrid keeps both engines and both hardening stacks,** which the
  review's item 2 also rejects. It saves the metadata bytes and, until
  follow-ups 2 and 3 land, the re-base and grouped-history costs.

**Order.** The follow-ups are v1 work whichever way Q15 goes. PR 3 removes
carry_v2, the fallback if follow-ups 2 and 3 miss their targets over
several rounds. So the operator may prefer to hold PR 3 until those two
targets are met.

Two observations go with this recommendation:

- At main, carry_v2 is not behind a feature: `pub mod carry_v2` is compiled
  into every build. WP0(a) says it stays frozen behind one. It is reachable
  from no verb, so nothing runs it, but PR 3 removes it from the binary as
  well.
- `git-carry-estimate` should stay (see the inventory note). It is
  read-only, and it moved nothing in S2 on any pass here.

## OI-1003-Q18 questions the git half raises (separate from Q15)

These change how the git half's S3 verdicts read. They do not change the
measured bytes, and none of them blocks Q15.

1. **Granularity of git inequality 2.** The file half bounds wire bytes by
   absent CDC chunks. A Git pack carries whole blobs, so the harness
   reports the git half both ways. At estate `mutate-10`:
   - chunk granularity: bound 806,672 B, excess 69,813,128 B;
   - object granularity: bound 67,129,040 B, excess 3,490,760 B.

   The difference is almost all `r00-delta`'s 64-byte edit in a 64 MiB
   blob. v2 carries no uncommitted edit at all. Which granularity applies
   to the git half?
2. **Object-store reads in git inequality 1.** The receipt counter covers
   worktree seats only. The git children also read the source object store
   to pack: readback was at least 16,859,136 B at estate `mutate-10`, and
   pack bytes were 70,619,800 B (logical). Do those count as "source content
   bytes"? If they do, inequality 1 fails at both estate delta passes.
3. **A metadata allowance in inequality 2.** A changed capture's own
   commits, trees and ref header are wire bytes with no absent source chunk
   behind them. A flat allowance sized on this corpus would be wrong, for
   three reasons:
   - The residual is 1 KB to 40 KB here (`r05-bytes`, a chained item:
     40,324 B).
   - The header lists the whole private ref inventory, so the cost is per
     ref. At #48's 119,761 refs, a header line of about 85 B (oid, space,
     refname) is about 10 MB per changed capture per item.
   - Grouped items multiply it by the number of items that recapture.

   Two forms are possible:
   - a formula: a per-capture constant, plus a per-ref term, plus a
     per-changed-tree term, counted once per absent object rather than once
     per item;
   - deferral until a large-ref hazard corpus measures it.

   An allowance ruled as "whatever the metadata is" would pre-approve the
   cost that "Ref count changes the picture" below says could reverse the
   engine decision.

## What evidence would change it

Any of these would reopen hybrid, and the first four could reopen v2:

- **History deltas on neo's real estate are large relative to v1's
  capture.** Run `git-carry-estimate` against sting at each sync, beside the
  v1 bundle bytes per changed repository. After follow-ups 1 to 4, v1 might
  still routinely exceed 1.1 × (v2 thin pack + changed worktree bytes +
  the metadata allowance once ruled) over a week of syncs. If so, the
  snapshot layer is not the whole story.
- **The re-base cost on real repositories.** Until follow-up 3, a chained
  item costs its history ÷ 9 per changed capture. Measure, on neo, the
  history size of chained items and how often they change. The cost may
  dominate a routine sync's git bytes.
- **Grouped growth on neo.** neo's agent repositories are worktree-heavy.
  If follow-up 2 cannot hold per-item bundle bytes flat over successive
  commits there, every commit to such a repository costs all history since
  the first pass, times its items.
- **History rewrites break v1's chain often.** That means a rebase or a
  force-push, followed by a gc or prune that drops the old tips. Only
  source-held tips of the retained capture become prerequisites, so after
  such a rewrite v1 can fall back toward re-packing the repository. v2
  negotiates against whatever tips the destination holds. Measure the rate
  of these fallbacks, and the bytes each one costs, on neo.
- **Gate (b) shows git bytes dominating sync time.** On the neo → sting pull
  at about 28 MB/s, git bytes might be most of a routine sync's wall time.
  That has not been measured; WP6 adds the remote arm.
- **Ref count changes the picture.** #48 measured 119,761 refs, and this
  corpus has tens per repository. v1 puts the whole ref inventory into every
  capture key and bundle header, so its per-capture overhead could grow far
  past 30 KB. A v1.1 corpus hazard set with a large ref count would test
  this.
- **The S2 budget is breached on the source.** v1 runs 4 censuses per
  changed item, and S2 allows at most +2.0 load1. If a breach is attributed
  to them rather than to the file walk, v2's probe-only source side would
  count for v2.
