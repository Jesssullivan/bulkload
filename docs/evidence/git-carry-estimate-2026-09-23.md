# Git carry v2 estimate, cohort 1: neo → sting, 2026-09-23

Lane W6-M0 of Bulkload M2 (bulkload#48, Linear TIN-4545). Ruling R-N60 sets
this as the measurement baseline for negotiated thin packs.

## Verdict

**The gate holds.** The cohort-1 history on disk is **1,465.14 MB**. The
design lane's figure was 1,465 MB.

Against sting's current refs, neo is now missing **3,769 objects, 14.80 MB**.
The design lane measured 3,141 objects and about 11 MB. The whole difference
comes from commits made on neo after the design lane measured. crs310 and
asfirewire-legalab match the design lane to the byte. glorious.build matches
(5.297 MB) once its commits after 13:26Z are left out. The history cut is
still **99.0 %** (14.80 of 1,465.14 MB).

## Identity and command

- **Revision:** bulkload `8472eae` (`feat/m2-w6-git-carry-estimate`). The
  release binary was built from that tree.
- **Command:** `bulkload-agent git-carry-estimate /Users/jess/git/<repo>
  sting:/srv/fast-local/jess/git/<repo>`, run once per repository.
- **Window:** 2026-09-23T16:26:22Z to 16:26:42Z, 20 s for all 26 repositories.
  blahaj took the longest, at 7 s.
- **Source (neo):** only read-only Git runs, with `--no-optional-locks` and
  `gc.auto=0`:
  - `for-each-ref`
  - `reflog show refs/stash`
  - `rev-parse --git-path shallow`, followed by a read of that file
  - `cat-file --batch-check`
  - `rev-list --objects --no-object-names --missing=allow-any --all <stash
    entries> --not <haves>`
- **Destination (sting):** two ssh reads per repository:
  - `ssh -T -oBatchMode=yes sting "git -C '<path>' for-each-ref
    '--format=%(objectname)'"`
  - `ssh -T -oBatchMode=yes sting 'bash -s'`, whose script only `cat`s
    `<path>/.git/shallow` when it exists
- **No writes on either host.** Nothing was fetched and no ref was updated.
- **Units:** MB means 10^6 bytes. Sizes are sums of `%(objectsize:disk)`, the
  size each object takes in neo's store: a pack delta or a compressed loose
  object. That is not the size of the thin pack that would actually be sent.
- **Repository list:** cohort 1 as the TIN-3692 receipt records it (comment
  `572099a3`, 2026-09-22T23:08Z). That is 22 imported repositories plus the 4
  refused ones (medical-massage-specialists-infra, printstack,
  asfirewire-legalab, crs310-8g-2s-in).

## Per repository

| Repository | History on disk (MB) | Objects | Dest tips | Haves used | Tips unknown to neo | Stash entries | Missing objects | Missing (MB) | Commits / trees / blobs / tags |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| DarwinNicUtil | 1.07 | 416 | 65 | 56 | 9 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| MassageIthaca | 199.37 | 21,443 | 1,792 | 1,786 | 6 | 2 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| account-controller | 1.53 | 832 | 32 | 22 | 10 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| blahaj | 34.11 | 39,787 | 2,280 | 1,783 | 497 | 4 | 38 | 0.029 | 3 / 23 / 12 / 0 |
| bulkload | 6.80 | 2,546 | 78 | 68 | 10 | 2 | 495 | 0.896 | 91 / 243 / 160 / 1 |
| ci-templates | 5.05 | 2,091 | 213 | 182 | 31 | 2 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| claude-kvm | 205.06 | 2,013 | 32 | 26 | 6 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| dcx2496-control | 0.47 | 901 | 47 | 41 | 6 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| glorious-build-infra | 0.39 | 358 | 55 | 21 | 34 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| glorious.build | 19.90 | 4,463 | 126 | 105 | 21 | 0 | 202 | 8.141 | 15 / 75 / 112 / 0 |
| great-falls-tool-bus-infra | 1.77 | 3,250 | 245 | 227 | 18 | 0 | 41 | 0.084 | 4 / 22 / 15 / 0 |
| greatfallstoolbus.org | 8.67 | 5,897 | 308 | 291 | 17 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| legalab | 3.99 | 2,592 | 154 | 148 | 6 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| massage-ithaca-portal | 1.13 | 887 | 43 | 30 | 13 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| meta | 10.77 | 2,078 | 72 | 51 | 21 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| owner-overlay-controller | 1.29 | 1,726 | 37 | 26 | 11 | 1 | 58 | 0.323 | 4 / 21 / 33 / 0 |
| prompts-enqueue | 2.81 | 2,284 | 62 | 40 | 22 | 2 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| qutebrowser | 64.50 | 182,869 | 173 | 167 | 6 | 1 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| softconnect | 0.33 | 194 | 8 | 2 | 6 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| tailnet-acl | 0.17 | 324 | 34 | 28 | 6 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| tinyland-infra | 1.94 | 2,707 | 96 | 74 | 22 | 2 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| tinyland.dev | 834.67 | 38,337 | 445 | 382 | 63 | 3 | 77 | 0.123 | 7 / 52 / 18 / 0 |
| medical-massage-specialists-infra | 5.10 | 4,168 | 124 | 124 | 0 | 0 | 0 | 0.000 | 0 / 0 / 0 / 0 |
| printstack | 1.75 | 2,624 | 30 | 25 | 5 | 2 | 10 | 0.070 | 2 / 4 / 4 / 0 |
| asfirewire-legalab | 45.02 | 19,483 | 61 | 60 | 1 | 0 | 2,234 | 1.587 | 140 / 1098 / 996 / 0 |
| crs310-8g-2s-in | 7.51 | 6,015 | 273 | 273 | 0 | 1 | 614 | 3.550 | 66 / 267 / 281 / 0 |
| **Total (26)** | **1,465.14** | **350,285** | **6,885** | **6,038** | **847** | **22** | **3,769** | **14.804** | 332 / 1805 / 1631 / 1 |

- **"Tips unknown to neo"** counts destination tips that neo does not hold as
  objects. Most are sting-side `refs/carry/*` capture commits, which exist only
  in the carried bundles. A follow-up refname read sorted the unknown tips by
  whether any non-carry ref also names them:

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
- **blahaj** is shallow on both sides: one shallow line on neo and one on sting.
- **medical-massage-specialists-infra** has no missing objects, even though
  its cohort-1 apply was refused. Sting's own refs already cover every object
  neo holds.

## Gate comparison

The design lane's per-repository figures are not recorded on TIN-3692,
bulkload#48 or TIN-4545. The only ones available are the three quoted in the
W6-M0 lane brief.

| Repository | Design lane | This run | Match |
|---|---:|---:|---|
| crs310-8g-2s-in | 3.55 MB | 3.550 MB, 614 objects | **yes** (exact) |
| asfirewire-legalab | 1.59 MB | 1.587 MB, 2,234 objects | **yes** (exact) |
| glorious.build | 5.30 MB | 8.141 MB, 202 objects | no: neo moved |
| Cohort history | 1,465 MB | 1,465.14 MB | **yes** |
| Cohort missing | 3,141 objects, ≈ 11 MB | 3,769 objects, 14.80 MB | no: neo moved |

**Why the figures moved.** Every missing commit was attributed by its
committer date. neo committed 91 bulkload commits (60 of them on
2026-09-23, from the M2 lanes), 10 glorious.build commits on 2026-09-23, and
a handful in great-falls-tool-bus-infra, owner-overlay-controller and
tinyland.dev.

**Reconstruction as of the design lane's measurement.** The same walk was
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

- The M1 acceptance bar is pack bytes ≤ 1.1× this estimate for the same
  source and destination state. The key to compare is `missing_bytes_disk`.
- The bundle carry repacks the whole closure on every capture: 1,465 MB of
  history, against 14.8 MB actually missing today. The carry-v2 negotiation
  removes that repacking.
