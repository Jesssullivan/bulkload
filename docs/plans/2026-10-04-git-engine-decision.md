# Git carry engine decision packet (OI-1003-Q15), 2026-10-04

Rulings: OI-1003-Q15 (measure S3 on the estate corpus first, then choose v1,
hybrid or v2 on the numbers; carry_v2 stays frozen until then), OI-1003-Q18
(the S3 delta inequalities), OI-1003-Q35 (byte counters, `census_walks`, pack
bytes and the rusage CPU ratio are admissible; wall time is informational),
OI-1003-Q38 (cited by the Sprint 2 dispatch; its text is not in the repo),
R-N13.

This packet asks the operator for the Q15 ruling, and with it for the
allowance in item 4 of the recommendation. It decides nothing itself.

- **Evidence:** [S3 on the estate corpus, sting, 2026-10-04](../evidence/s3-estate-sting-2026-10-04.md),
  measured with `crates/bulkload-bench/scripts/s3_estate.py` and a release
  build of main `4a10bb8` (PR #151 was still open).
- **Label:** informational, ungated. No gated sample, no power or load gate
  (load1 was 16 to 47 during the passes).

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

Bytes are bundle bytes for v1 (`write_source_pack_bytes`) and
`missing_thin_pack_bytes` for v2. "Hybrid" is v2's history bytes plus v1's
snapshot layer, bounded from below by the round's changed worktree bytes;
it is derived, not measured. CPU is rusage user plus system of the git
half: `estate-capture` for v1, `git-carry-estimate` for v2 (which probes all
32 repositories every pass, nests and bare mirrors included, hence its
floor of about 5 s).

**Scale estate** (24 repositories plus history-heavy, 58 items, 4.19 GB):

| Engine | First pass | Each unchanged rerun | `mutate-1` (one history-heavy commit) | `mutate-10` (10 kinds) |
|---|---:|---:|---:|---:|
| v1 bytes (measured) | 932,119,799 | 0 | 39,352 | 70,619,800 |
| v1 CPU s | 118.96 | 4.65 to 5.19 | 18.70 | 15.02 |
| v1 census walks (expected) | 216 (216) | 54 (54) | 57 (57) | 66 (66) |
| v1 source bytes read (bound) | 711,653,958 | 0 (0) | 3,209 (3,209) | 67,115,992 (67,115,992) |
| v2 bytes (projection) | 350,103,767 | 0 | 499 | 1,065 |
| v2 CPU s (the estimate) | 20.65 | 5.15 to 7.18 | 6.65 | 5.19 |
| Hybrid bytes (derived lower bound) | about 932 M | 0 | about 3,700 + metadata | about 67.1 M + metadata |
| v1 without history-heavy | 752,710,458 | 0 | 0 | 70,580,320 |
| v2 without history-heavy | 170,742,243 | 0 | 0 | 503 |

**Scale small** (3 repositories plus history-heavy, 10 items, 18.7 MB): v1
6,977,621 / 0 / 5,753 / 2,239,880 bytes against v2 2,448,913 / 0 / 422 /
947 bytes, in the same column order.

The hybrid's first pass is about v1's, because 547 MB to 597 MB of v1's
932 MB is snapshot layer that hybrid keeps. Of that, 546.7 MB is the eight
`r00` and `r12` item bundles, each carrying the repository's 64 MiB blob.

### What the numbers say

1. **Unchanged reruns: no difference.** Both engines send 0 bytes. v1
   reads 0 source bytes and walks one census per item, at about 5 s of CPU
   for 54 items. The projection's probe costs about the same.
2. **History deltas, the #48 case: v1 is close enough.** With #146's
   auto-prerequisite chain, a history-heavy commit costs v1 39,352 B against
   v2's 499 B. The difference is a constant capture overhead (snapshot
   commits and trees, carry refs, the bundle header), about 30 KB at estate
   and 1 KB at small. At gate (b)'s 28 MB/s link, that is about 1.4 ms per
   changed repository per pass. v1 no longer re-packs history: before #146
   the same round would have re-packed history-heavy's 243 MB.
3. **v1's CPU spike is its reuse fetch, not its history pack.** v1 spent
   13.8 s on that 5-object change, because it fetched its whole 179 MB
   retained bundle to reuse blobs. One round later, with a 39 KB retained
   bundle, the same capture took 0.45 s. That cost belongs to v1's reuse
   mechanism and can be fixed there.
4. **First-pass history: comparable.** v1's shared group bases plus
   history-heavy come to 335 MB, with at most 50 MB more history inside the
   other item bundles. v2's thin packs come to 350 MB.
5. **Every large excess is in the snapshot layer, which v2 lacks and hybrid
   keeps:**
   - a recaptured linked worktree in a multi-item group re-packs its whole
     checkout: 1.83 MB against a 3.5 KB change at estate;
   - each linked-worktree item of a repository repeats the checkout's large
     blobs on the first pass: 546.7 MB of 932 MB.

   v2's negotiation touches neither. A hybrid would carry the same bytes as
   v1, plus a second engine.

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
lines, the inventory above), with four v1 follow-ups under WP2:

1. **Grouped items stop re-packing their checkout.** A linked worktree takes
   the previous capture's tips (the chain) as well as the group base, or its
   snapshot commits get a parent so the prerequisite trees become edges.
   Target: estate `mutate-10` git inequality 2 within the metadata allowance
   (item 4).
2. **One copy of shared blobs per repository on the first pass.** Linked
   worktrees of one repository share the group's snapshot blobs. Target:
   `r00` and `r12` carry their 64 MiB blob once each, not four times.
3. **Reuse without fetching the whole retained bundle.** Target: a
   one-commit history delta costs about an unchanged rerun's CPU, not 13.8 s.
4. **A ruled metadata allowance in OI-1003-Q18 inequality 2.** A changed
   capture's own commits, trees and ref header (1 KB to 30 KB here) are wire
   bytes with no absent source chunk behind them. This needs an operator
   ruling, not code.

Reasons:

- On refs and history, v2 saved about 30 KB to 40 KB per changed repository
  per pass in these runs, and nothing on the first pass.
- Every measured S3 failure on the git half is in the snapshot layer, which
  v1 and hybrid share and v2 does not have.
- v2 would add a frame family, a destination ingest and crash surface, a
  journal, and a second resume story for the formal model. The review
  rejected that (section 4, items 1 and 2), and it buys the saving above.
- Hybrid keeps both engines and both hardening stacks, which the review's
  item 2 also rejects. It saves the same 40 KB.

Two observations go with this recommendation:

- At main, carry_v2 is not behind a feature: `pub mod carry_v2` is compiled
  into every build. WP0(a) says it stays frozen behind one. It is reachable
  from no verb, so nothing runs it, but PR 3 removes it from the binary as
  well.
- `git-carry-estimate` should stay (see the inventory note). It is
  read-only, and it moved nothing in S2 on any pass here.

## What evidence would change it

Any of these would reopen hybrid, and the first two could reopen v2:

- **History deltas on neo's real estate are large relative to v1's
  capture.** Run `git-carry-estimate` against sting at each sync, beside the
  v1 bundle bytes per changed repository. After follow-ups 1 to 3, v1 might
  still routinely exceed 1.1 × (v2 thin pack + changed worktree bytes +
  the allowance) over a week of syncs. If so, the snapshot layer is not the
  whole story.
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
