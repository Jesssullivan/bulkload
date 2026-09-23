# Git carry v2 estimate, cohort 1: neo → sting, 2026-09-23

Lane W6-M0 of Bulkload M2 (bulkload#48, Linear TIN-4545). Ruling R-N60 sets
this as the measurement baseline for negotiated thin packs. Rulings R-N74
(the thin-pack gate metric) and R-N75 (refuse destinations whose refs do not
prove their history) followed the PR #55 review, and this page was re-measured
under them.

## Verdict

**Gate metric (R-N74): `missing_thin_pack_bytes`.** It is the byte count of
the thin pack that `pack-objects --stdout --thin --revs --delta-base-offset`
builds for the missing set, counted in flight and never written. Against
sting's current refs, neo is missing **3,944 objects**, and their thin pack is
**8.530 MB**. The informational stored size of the same objects
(`missing_bytes_disk`) is 15.028 MB. The thin pack is smaller because it sends
deltas against objects sting already holds. crs310-8g-2s-in shows this most
clearly: 0.449 MB thin against 3.550 MB stored.

**The W6 M1 gate is now:** sent pack bytes ≤ 1.1× `missing_thin_pack_bytes`,
and the missing object count equal (`missing_thin_pack_objects` =
`missing_objects`), for the same source and destination state (R-N74).

The cohort-1 history on disk is 1,467.47 MB (informational), so the carry
moves 0.58 % of it. No pair was refused (R-N75): blahaj is shallow on both
sides at the same frontier, so it estimates, and no destination is a partial
clone.

## Identity and command

- **Revision:** bulkload `f65313b` (`feat/m2-w6-git-carry-estimate`, after the
  PR #55 review fixes). The release binary was built from that tree.
- **Command:** `bulkload-agent git-carry-estimate /Users/jess/git/<repo>
  sting:/srv/fast-local/jess/git/<repo>`, run once per repository.
- **Window:** 2026-09-23T17:56:23Z to 17:57:35Z, 72 s for all 26
  repositories. blahaj took the longest, at 20 s.
- **Probe:** one POSIX script, run through `bash -s -- PATH`. It runs locally
  for the source and over **one** ssh session per repository for sting
  (`ssh -T -oBatchMode=yes -oConnectTimeout=15 sting "env GIT_NO_LAZY_FETCH=1
  bash -s -- '<path>'"`). It:
  - refuses a path that is not the repository's root, with
    `GIT_CEILING_DIRECTORIES` set to its parent;
  - reads every ref, every worktree's `HEAD` and per-worktree refs, the
    shallow file (`rev-parse --path-format=absolute --git-path shallow`) and
    the partial-clone configuration.
- **Source (neo):** only read-only Git runs:
  - the probe
  - `for-each-ref refs/stash` and `reflog show refs/stash`
  - `cat-file --batch-check`
  - `rev-list --objects --no-object-names --missing=print --stdin`
  - `pack-objects --stdout --thin --revs --delta-base-offset
    --missing=allow-any` with `pack.useSparse=false`, streamed into a byte
    counter
- **Hardening:** every Git call on either host runs with
  `GIT_NO_LAZY_FETCH=1`, `--no-optional-locks`, `-c maintenance.auto=false`,
  `-c gc.auto=0` and `-c core.hooksPath=/dev/null`. The probe refuses Git older
  than 2.44 (neo 2.52.0, sting 2.54.0).
- **No writes on either host.** After the run, `find` of every repository's
  git dir for files newer than the start of the window found:
  - **sting:** nothing.
  - **neo:** one file, `bulkload/.git/worktrees/review-pr53b/index`, written
    at 17:57:07Z. That is about 12 s after bulkload's pair finished. It is
    another lane's worktree, and the verb never writes an index.

  A first run at 17:42:27Z–17:43:21Z, from `5098e3e`, also wrote nothing on
  sting. On neo, the only new files were blahaj's
  `worktrees/electrical-powerdown-20260923/*` and `config`. They were written
  at 17:43:25Z, after that run ended, when another session ran
  `worktree add -b`.
- **Units:** MB means 10^6 bytes. `missing_thin_pack_bytes` is the whole pack
  stream: header, entries and trailer. `missing_bytes_disk` and "history on
  disk" are sums of `%(objectsize:disk)`, the size each object takes in neo's
  store. **They are informational, not the gate.**
- **Repository list:** cohort 1 as the TIN-3692 receipt records it (comment
  `572099a3`, 2026-09-22T23:08Z). That is 22 imported repositories plus the 4
  refused ones (medical-massage-specialists-infra, printstack,
  asfirewire-legalab, crs310-8g-2s-in).

## Per repository

| Repository | History on disk (MB, info) | Objects | Dest tips | Haves used | Tips unknown to neo | Stash entries | Missing objects | **`missing_thin_pack_bytes` (MB)** | Thin-pack objects | `missing_bytes_disk` (MB, info) | Commits / trees / blobs / tags |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| DarwinNicUtil | 1.07 | 416 | 65 | 56 | 9 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| MassageIthaca | 199.37 | 21,443 | 1,793 | 1,787 | 6 | 2 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| account-controller | 1.53 | 832 | 32 | 22 | 10 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| blahaj | 34.11 | 39,787 | 2,281 | 1,784 | 497 | 4 | 38 | **0.013** | 38 | 0.029 | 3 / 23 / 12 / 0 |
| bulkload | 7.81 | 2,723 | 78 | 68 | 10 | 2 | 672 | **0.426** | 672 | 1.909 | 115 / 335 / 221 / 1 |
| ci-templates | 5.05 | 2,091 | 213 | 182 | 31 | 2 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| claude-kvm | 205.06 | 2,013 | 32 | 26 | 6 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| dcx2496-control | 0.47 | 901 | 47 | 41 | 6 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| glorious-build-infra | 0.39 | 358 | 59 | 21 | 38 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| glorious.build | 21.21 | 4,554 | 136 | 115 | 21 | 0 | 200 | **6.281** | 200 | 7.353 | 27 / 100 / 73 / 0 |
| great-falls-tool-bus-infra | 1.77 | 3,250 | 245 | 227 | 18 | 0 | 41 | **0.012** | 41 | 0.084 | 4 / 22 / 15 / 0 |
| greatfallstoolbus.org | 8.67 | 5,897 | 309 | 292 | 17 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| legalab | 3.99 | 2,592 | 154 | 148 | 6 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| massage-ithaca-portal | 1.13 | 887 | 43 | 30 | 13 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| meta | 10.77 | 2,078 | 72 | 51 | 21 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| owner-overlay-controller | 1.29 | 1,726 | 37 | 26 | 11 | 1 | 58 | **0.049** | 58 | 0.323 | 4 / 21 / 33 / 0 |
| prompts-enqueue | 2.81 | 2,284 | 62 | 40 | 22 | 2 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| qutebrowser | 64.50 | 182,869 | 173 | 167 | 6 | 1 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| softconnect | 0.33 | 194 | 8 | 2 | 6 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| tailnet-acl | 0.17 | 324 | 34 | 28 | 6 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| tinyland-infra | 1.94 | 2,707 | 99 | 74 | 25 | 2 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| tinyland.dev | 834.67 | 38,337 | 445 | 382 | 63 | 3 | 77 | **0.017** | 77 | 0.123 | 7 / 52 / 18 / 0 |
| medical-massage-specialists-infra | 5.10 | 4,168 | 125 | 125 | 0 | 0 | 0 | **0.000** | 0 | 0.000 | 0 / 0 / 0 / 0 |
| printstack | 1.75 | 2,624 | 30 | 25 | 5 | 2 | 10 | **0.025** | 10 | 0.070 | 2 / 4 / 4 / 0 |
| asfirewire-legalab | 45.02 | 19,483 | 61 | 60 | 1 | 0 | 2,234 | **1.258** | 2,234 | 1.587 | 140 / 1098 / 996 / 0 |
| crs310-8g-2s-in | 7.51 | 6,015 | 273 | 273 | 0 | 1 | 614 | **0.449** | 614 | 3.550 | 66 / 267 / 281 / 0 |
| **Total (26)** | **1,467.47** | **350,553** | **6,906** | **6,052** | **854** | **22** | **3,944** | **8.530** | **3,944** | **15.028** | 368 / 1922 / 1653 / 1 |

- **Thin-pack objects equal missing objects in every repository.** In the
  first run, from `5098e3e`, bulkload packed 659 objects against 658 walked.
  Git's default sparse edge marking (`pack.useSparse`) had packed one object
  that sting already reaches. `f65313b` pins `pack.useSparse=false`, and it
  refuses (`CONTRACT_SELF_INCONSISTENT`) if the counts still differ. The M1
  sender must build its pack the same way, or its object count can exceed
  `missing_objects`.
- **Moving repositories:** bulkload and glorious.build gained commits between
  the two runs; the table is the second run.
- **"Tips unknown to neo"** counts destination tips that neo does not hold as
  objects. Most are sting-side `refs/carry/*` capture commits, which exist only
  in the carried bundles. A follow-up refname read, made during the first
  baseline run (`8472eae`), sorted the unknown tips by whether any non-carry ref
  also names them:

  | Repository | Unknown tips | Named only by `refs/carry/*` |
  |---|---:|---:|
  | blahaj | 497 | 491 |
  | tinyland.dev | 63 | 60 |
  | ci-templates | 31 | 31 |
  | bulkload | 10 | 10 |
  | glorious.build | 21 | 6 |

  glorious.build's other 15 are sting-native branches, such as
  `codex/pretext-scan-integration-proof`. The estimate does not probe the
  ancestors of unknown tips (`GitHaveQuery` belongs to M1), so this is the
  first-round, conservative figure.
- **blahaj** is shallow on both sides at the same frontier, `bf9acf31`, so
  R-N75 lets it estimate. A differing frontier would refuse with
  `GIT_HAVES_UNPROVABLE`.
- **greatfallstoolbus.org** is a partial-clone source
  (`remote.<url>.promisor=true`, `blob:none`), and it holds every object its
  refs reach: `source_unavailable_objects=0`. Its newest pack is from
  2026-09-22, so the first baseline run did not lazily fetch into it. The
  verb now runs with `GIT_NO_LAZY_FETCH=1`, so it cannot.
- **Destination tips rose from 6,885 to 6,906** because the probe now reads
  every worktree's `HEAD` and per-worktree refs.
- **medical-massage-specialists-infra** has no missing objects, even though
  its cohort-1 apply was refused. Sting's own refs already cover every object
  neo holds.

## Gate comparison

The design lane's per-repository figures are not recorded on TIN-3692,
bulkload#48 or TIN-4545. The only ones available are the three quoted in the
W6-M0 lane brief.

The design lane's figures are stored sizes, so they compare with
`missing_bytes_disk` (informational). The last column is the gate metric.

| Repository | Design lane (stored) | `missing_bytes_disk` (info) | Match | `missing_thin_pack_bytes` (gate) |
|---|---:|---:|---|---:|
| crs310-8g-2s-in | 3.55 MB | 3.550 MB, 614 objects | **yes** (exact) | 0.449 MB |
| asfirewire-legalab | 1.59 MB | 1.587 MB, 2,234 objects | **yes** (exact) | 1.258 MB |
| glorious.build | 5.30 MB | 7.353 MB, 200 objects | no: neo moved | 6.281 MB |
| Cohort history | 1,465 MB | 1,467.47 MB | **yes** (+2.3 MB of new commits) | — |
| Cohort missing | 3,141 objects, ≈ 11 MB | 3,944 objects, 15.03 MB | no: neo moved | 8.530 MB |

**Why the figures moved.** In the first baseline run (`8472eae`), every missing commit was attributed by its
committer date. neo committed 91 bulkload commits (60 of them on
2026-09-23, from the M2 lanes), 10 glorious.build commits on 2026-09-23, and
a handful in great-falls-tool-bus-infra, owner-overlay-controller and
tinyland.dev.

**Reconstruction as of the design lane's measurement** (stored sizes,
informational; computed during the first baseline run). The same walk was
limited to missing commits committed before 2026-09-23T13:26Z, the time the
R-N53–R-N64 rulings comment was posted. It used today's sting haves. It gives:

| Repository | Objects | MB |
|---|---:|---:|
| crs310-8g-2s-in | 614 | 3.550 |
| asfirewire-legalab | 2,234 | 1.587 |
| glorious.build | 162 | 5.297 |
| bulkload | 276 | 0.172 |
| owner-overlay-controller | 39 | 0.204 |
| tinyland.dev | 77 | 0.123 |
| printstack | 10 | 0.070 |
| great-falls-tool-bus-infra | 24 | 0.035 |
| blahaj | 20 | 0.002 |
| **Total** | **3,456** | **11.04** |

The 11.04 MB matches the design lane's ≈ 11 MB, and glorious.build's
5.297 MB matches its 5.30 MB.

The object count is 315 higher than 3,141. This reconstruction cannot be exact,
for three reasons:

- neo's refs at the design lane's moment are not recorded.
- A committer-date filter admits old-dated commits that reached a ref only
  later. This may be the case for bulkload's 31 missing commits dated
  2026-09-02 to 2026-09-22.
- sting's refs may also have changed since.

**Not reproduced: the design lane's ≈ 17.6 MB against non-carry refs only.**
The verb uses every destination ref, as the v2 negotiation does.

## Reading the result for M1

- **Gate (R-N74):** sent pack bytes ≤ 1.1× `missing_thin_pack_bytes`, and the
  missing object count equal, for the same source and destination state. Use
  `missing_objects`; `missing_thin_pack_objects` is always equal to it.
  Measure both sides in the same window: bulkload and glorious.build moved
  between the two runs on this page.
- `missing_bytes_disk` and `source_history_bytes` are informational. A stored
  size can be larger than the thin pack (crs310-8g-2s-in, 7.9×) or smaller,
  because pack entry headers and deltas against haves differ from how the
  source stores the object.
- **Refusals (R-N75):** the verb refuses (`GIT_HAVES_UNPROVABLE`) a
  partial-clone destination, and a shallow destination whose frontier differs
  from the source's. Such a destination's refs do not prove it holds their
  history.
- The bundle carry repacks the whole closure on every capture: 1,467 MB of
  history, against an 8.5 MB thin pack actually missing today. The carry-v2
  negotiation removes that repacking.
