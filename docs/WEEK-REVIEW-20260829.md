# WEEK REVIEW — Bulkload boundary ceremony, 2026-08-25 → 2026-08-29

**Written:** 2026-08-29 ~13:45Z (judge lane, opus). **Read-only review.** No host was touched.
**Window of record:** `STATUS:1` 2026-08-25T03:11:50Z → `STATUS:317` 2026-08-29T13:42:52Z = **106 h 31 m 02 s = 6,391 minutes**.
**State at writing:** `STATUS.current` = `v4.14-apply`. **LANDING-READY has never been set** (`DELTA-PACKET-0720.md:5`). Apply is on its **15th launch**, ledger-named run-13.

**Sources of record (line-cited throughout):** `STATUS` (317 lines, append-only), `BASES-20260828.md` §B/D/E, `SLO-SLA-LEDGER-20260829.md`, `INTERVIEW-PACKET-20260829.md`, `DELTA-PACKET-0720.md`, `product-review/VERDICTS.md`, `landing/lanes/L1-full-delta-runbook.md`, `/Users/jess/.claude/plans/please-examine-the-craziness-inherited-sky.md`.

**Custody warning:** this file sits on the boundary volume that `TIN-4189`'s own custody note says the post-landing cleanup deletes, and that silently truncated >4 MB writes on 08-29 (`STATUS:244`). Copy it to a durable location before cleanup.

---

## 1. LAP TABLE AND TOTALS

### 1.1 Hard counts (mechanically verified against `STATUS`)

| Metric | Value | Source |
|---|---|---|
| Wall clock, first ledger line → last | **6,391 min (106 h 31 m)** | `STATUS:1`, `STATUS:317` |
| Distinct `FAILED:` events | **45** | `grep -c 'FAILED:'` = 49, minus 4 supervisor echo lines (`STATUS:256,270,274,278`) |
| Pipeline launches (`*-preflight` markers) | **≥62** | 21 × `v2-preflight`, 15 × `v4.14-preflight`, 5 × `preflight`, 21 × v4.1–v4.11 |
| Orchestrator scripts hand-written | **16** (`orchestrate-prelim` → `v4.14` + 2 supervisors) | `ls orchestrate-*.sh` |
| Apply-verb launches | **15** (ledger names run-1 … run-13) | `STATUS:250…315` |
| Distinct engine defect classes named in the ledger | **13 numbered** (`STATUS:310` "13th DEFECT") **+ ≥7 unnumbered** | see §1.3 |
| Engine commits hot-deployed to both hosts mid-ceremony | **≥20** | SHAs enumerated §1.3 |
| Completed ceremonies | **0** | `DELTA-PACKET-0720.md:5` |

### 1.2 Non-overlapping lap table — every one of the 6,391 minutes assigned exactly once

| # | Phase | Window (UTC) | Minutes | % | Launches | FAILED | What it produced |
|---|---|---|---|---|---|---|---|
| 0 | **Wrong-charter prologue** — quiesced A/B pair, retired as "unpairable on a living source" | 08-25 03:11:50 → 10:55:35 | **464** | 7.3% | 3 | 1 | Nothing. Mode abandoned (`STATUS:6`). |
| 1 | **V2 capture-A lap loop** — empty-destination fast path | 08-25 11:09:40 → 08-26 13:15:30 | **1,566** | 24.5% | 21 | 19 | One source-A seal. The successful run took **114 min** (`STATUS:110→111`). **1,452 min was re-executed prefix.** |
| 2 | **Capture-B / seal-chain thrash** — B killed by own agents' git ops, A2 compat refusal, chained-B swap-thrash killed at 5h01m | 08-26 13:15:30 → 08-27 09:58:57 | **1,244** | 19.5% | 6 | 3 | One source-B seal. |
| 3 | **Plan → preseed prepare/push → file-seat defect → forced full recapture** | 08-27 09:58:57 → 21:05:18 | **666** | 10.4% | 3 | 2 | 84 G pushed into `.transport-quarantine`. |
| 4a | **DARK — no ledger entry, nothing executing** | 08-27 21:07:18 → 08-28 16:49:24 | **1,182** | 18.5% | 0 | 0 | Nothing. **The single largest block of the ceremony.** |
| 4b | **Preseed materialize defect loop** — symlink mode, launcher pin clobber, O(n) walk recompute, case-fold ×2 | 08-28 16:49:24 → 23:44:43 | **417** | 6.5% | 6 | 4 | `v4.9-preseed-ok` (`STATUS:213`). |
| 5 | **Final stage / push** — live-fence false-refuse ×2, disclosed break-glass | 08-28 23:44:44 → 08-29 01:25:33 | **101** | 1.6% | 3 | 2 | `final-push.json` receipt. |
| 6 | **Final materialize** — 6th symlink-mode site; neo enclosure incident + emergency reboot rides inside | 08-29 01:25:33 → 04:56:34 | **211** | 3.3% | 3 | 2 | `final-stage.json`, `ready_for_apply=true` (`STATUS:249`). |
| 7 | **APPLY — 15 launches, 7 new defect classes, still running** | 08-29 04:56:40 → 13:42:52 | **526** | 8.2% | 15 | 13 | ~156+ repos installed (`STATUS:306`); **not landed.** |
| — | boundary rounding | | **14** | 0.2% | | | |
| | **TOTAL** | | **6,391** | 100% | **≥62** | **45** | **no landing** |

**Read the table this way:** 464 + 1,566 + 1,244 = **3,274 minutes (51 % of the ceremony) went to producing two capture seals** — artifacts that `VERDICTS.md:595` prices at **5.1 min** for a single cold hash-everything walk of the same 104.3 GiB and that `VERDICTS.md:641` says are replaceable by a **122-second** `(dev,ino,size,mtime_ns,ctime_ns)` census. Add the 1,182-min dark block and **70 % of the week bought nothing that survives.**

### 1.3 Defect classes, in ledger order, with the engine SHA deployed to fix each

| # | Class | Named at | Fix SHA |
|---|---|---|---|
| — | v4 fatal: no path map for `.git-boundary` (planner git-root-binding-divergence) | `STATUS:123` | contract amend |
| — | A2 seal-chain compat refusal (`scanner.py:3204-3208`) | `STATUS:132` | fresh A2 |
| — | chained-B swap-thrash: 13.3 G peak vs 8 G RAM | `STATUS:136` | fresh B |
| — | latent stock file-seat plan-path shape (`executor.py:209`) | `STATUS:146` | `b13a685` (forced full recapture) |
| 1 | darwin symlink mode 0755 vs linux 0777 — custody side | `STATUS:159` | `c81de89` |
| — | launcher clobbered its own pin (`bulkload.py:97`) — **caused by the previous emergency fix** | `STATUS:163` | `df3fcb2` |
| 4 | APFS case-fold vs XFS (`GloriousFlywheel.worktrees`) | `STATUS:167`, `:197` | `6b362c5`/`467dbb7` |
| — | `_plan_source_paths` recomputed 3× per materialize (~2.16 M ops each) | `STATUS:172` | `6490c80` |
| — | live-snapshot-fence censuses a different tree than the seal ("T1", no path named) | `STATUS:226`, `:227` | `6c2910b` → `6e9c940` break-glass |
| 6 | symlink mode in `_verify_record` (`executor.py:801`) — 2nd site | `STATUS:241` | `3fababb` |
| — | symlink mode in `_same_record` — 3rd site (pre-empted by refuter, not by a lap) | `STATUS:259` | `9799c1e` |
| 7 | journal reseal per item → **~20-day ETA** for 1,742,918 mutations | `STATUS:252` | `94daf87` |
| 8 | cross-device reflink hard-stop (`/srv/fast-local` 64771 vs `/home` 64773) | `STATUS:262` | `e14df6a` |
| 9 | journal write per git object — same O(n²) class, 3rd location | `STATUS:269` | `06ac65f` |
| 10 | shallow-clone grafts (31 repos), zero shallow-awareness | `STATUS:287`, `:292` | `98e95a9` |
| 11 | unborn repo HEAD = all-zeros OID | `STATUS:300` | `e5f9955` |
| 12 | 2005-era tagless tags in a kernel tree | `STATUS:306` | `165fe69` |
| 13 | blob:none promisor partial clone + `gitattributesMissing` | `STATUS:310`, `:314` | `235b8c3`, `070a7b4` |
| — | lap prefix re-fscks every completed repo (**the operator's 60-min complaint**) | `STATUS:316` | `b4ea518` — deployed 13:41Z, **not loaded by run-13** |
| — | fish→bash wrapper false-failure class | `STATUS:249` | bulkload#33 |

**13 of these 20 were found and fixed inside the last 15 h 45 m** (22:00Z 08-28 → now). That is **one new defect class every ~70 minutes, sustained, with no sign of decay** — classes 10, 11, 12 and 13 all landed in the final four hours.

---

## 2. ASSERTION GRADES — operator's claims vs the evidence

| # | Assertion | Grade |
|---|---|---|
| 1 | "this should take a few minutes — most of the data is already moved" | **NO CARRIER / FALSE ON THE MERITS** |
| 2 | "this is definitely still slower than rclone. ick." | **TRUE — and understated** |
| 3 | "the lab and blahaj lanes must never be stopped/frozen" | **TRUE — honored in execution, contradicted in the artifacts** |
| 4 | "nothing I state is novel; you keep forgetting the bases" | **TRUE — proven by the agent's own regrounding lane** |
| 5 | "a simple migration was never completed" | **TRUE — still true at 13:45Z** |
| 6 | "60 minutes per iteration is insane and unacceptable" | **TRUE — root cause named, fixed only on day 5** |

**Score: 5 of 6 TRUE on the record. Zero refuted. The sixth is not in the record at all.**

**1 — "a few minutes / most of the data is already moved" — NO CARRIER; FALSE ON THE MERITS.**
Grep for `few minutes`, `already moved`, `most of the data` across `STATUS`, `BASES-20260828.md`, `INTERVIEW-PACKET-20260829.md`, `DELTA-PACKET-0720.md` and the plan file returns **zero hits**. If the operator said it, this ledger did not record it — which is itself a defect of the record, since 13 other operator quotes *are* transcribed verbatim.
On the merits it is false in both halves. (a) Nothing had moved: `L1:15,21` measured `/srv/fast-local/jess/git` = **0 entries**, all three state roots empty, "Nothing has been applied"; at 07:20Z `DELTA-PACKET-0720.md:28` still measured **0 entries** and `/home/jess/.pi` mtime **2026-05-09**. The 84 G lived in `.transport-quarantine`, which is staging, not destination. (b) The engine's own most optimistic projection, made at the moment it first reached the payload phase, was "custody ~55m + staging ~110m → LANDING-READY ~00:45Z" (`STATUS:186`) — **165 minutes, not "a few."** Actual elapsed since that projection: **15 h 45 m and not landed.**

**2 — "still slower than rclone" — TRUE, verbatim, and understated.**
`STATUS:236` @ 2026-08-29T01:38:53Z: *"OPERATOR (during final round): 'this is definitely still slower than rclone. ick.' — acknowledged and durably recorded."* The structural refutation is `VERDICTS.md:7`: **Bulkload does not move bytes; rsync does** — `--rsync-path` is a required CLI arg (`cli.py:450`), transport is `rsync -a --from0 --files-from=- --delay-updates` over ssh (`executor.py:661-683`). Measured: **9.2 MB/s effective** (84,112,374,824 B / 152 min, `L1:437`) on a link independently measured at **14.8–18.5 MiB/s** (`VERDICTS.md:605`). Verify side: `rclone check --checksum --checkers 16` does 268,406 files in **33.8 s**; the engine's own primitive does the same corpus in 59.4 s single-threaded and read custody back at **24.5 MiB/s** (`STATUS:186`). Product judge: **NO-SHIP** (`VERDICTS.md:5`, `:811`). R23 (`STATUS:226`) keeps the thesis but forbids asserting it before the M0 benchmark — correct, and the benchmark does not exist yet.

**3 — "never stop the lab/blahaj lanes" — TRUE, and honored by construction.**
`SLO-SLA-LEDGER-20260829.md:11-12`: *"The lab and blahaj Claude lanes on neo are NOT stopped tonight and never need to be… This is D-1 of BASES (stated 9× now)."* Mechanism: capture roots are frozen clones (`STATUS:83` LAP-14, `STATUS:93` LAP-17), deltas are `--update`, sqlite families untouched (`STATUS:237`). **The requirement was met.** What it exposes: `TRUTH-BASELINE-20260826.md:7` still charters Bulkload as a **QUIESCED ONE-SHOT**, and `--acknowledge-writers-quiesced` still ships — a flag the same baseline rules *"unsatisfiable on a living host"* (`VERDICTS.md:277`). `BASES` E-1 names the root cause: commit `16cdabe` flipped five docs to the live contract and missed `AGENTS.md`. **You cannot ship a public product whose primary safety flag is a trap** (`VERDICTS.md:277`).

**4 — "you keep forgetting the bases" — TRUE, and the agent conceded it in writing.**
`STATUS:207` @ 22:52:02Z, operator verbatim: *"do you still have the actual bulkload repo plans, linear initiatives, interviews, dialog trees? nothing I state is novel; you keep forgetting the bases."* Agent response, same line: *"HONEST: context compacted ≥2x since 08-24; the session runs on summaries-of-summaries."* `BASES-20260828.md:1715-1720` then proved it: nine recurring items, oldest **84 days**, newest 5 days, **eight of nine already written in durable citable artifacts** — *"The failure is not a missing decision — it is that the decisions live in nine different files and the session has been re-deriving them from compacted summaries instead of citing them."*
Aggravating: the very next execution pass (06:20Z 08-29) reproduced the same class of defect **inside** the corrective artifact — `DELTA-PACKET-0720.md:59` finds the "hierarchical-alias refutation" obligation has exactly one carrier in the world, a line on this doomed volume, with zero Linear and zero GitHub carrier: *"the packet's finding #11 recurring inside the pass that applied §5."*

**5 — "a simple migration was never completed" — TRUE, and still true.**
`STATUS:317` @ 13:42:52Z records it as an operator assertion of record with the agent's own *"ACCEPTED — the ledger corroborates."* Mechanically: `STATUS.current` = `v4.14-apply`; LANDING-READY never set (`DELTA-PACKET-0720.md:5`); 45 recorded failures; 106 h 31 m; the apply verb is on launch 15. Three landing-time projections were published and all three were wrong: `SLO-SLA-LEDGER:6` "≈02:30–03:00Z" (missed by >10 h), `DELTA-PACKET:27` "≈10:00–11:00Z" (missed by >2 h 45 m), `STATUS:186` "~00:45Z" (missed by >13 h).

**6 — "60 minutes per iteration is insane and unacceptable" — TRUE, root cause named, fixed too late to help.**
`STATUS:316` @ 13:41:30Z: agent's own adjudication is *"CORRECT — the lap prefix re-fscks every completed repo."* Measured lap costs after run-8: **57 m, 26 m, 49 m, 40 m, 39 m**. One kernel fsck pass alone costs **~25 min** (`STATUS:314`). Fix `b4ea518` (`journal.git_entries_done` skip markers) was deployed at 13:41Z — **after thirteen apply runs**, and the ledger states plainly that **run-13 keeps its loaded code** and does not benefit.

---

## 3. SYSTEMIC PATTERNS, RANKED BY MINUTES BURNED

Patterns cut across phases, so components are named and overlap is disclosed. Ranking is by attributable wall-clock, not by severity of the code defect.

### P1 — No resume, no incremental identity: every failure re-runs a completed prefix. **≈2,320 min (36 %)**

| Component | Minutes | Evidence |
|---|---|---|
| V2 capture-A: 19 laps to produce a 114-min artifact | **1,452** | `STATUS:11–111` |
| Forced full recapture after a contract change (legal ceremony boundary) | **267** | `STATUS:146` v4.4, 11:13:45Z→15:39:05Z |
| Chained-B swap-thrash, killed at 5 h 01 m, zero output | **301** | `STATUS:136` |
| The contractual A/B pair's own second 109 GiB read | **148** | `VERDICTS.md:639` — 2 h 28 m to discover **705 changed files of 1,574,829 (0.044 %)** |
| Apply lap re-fsck of already-completed repos (runs 8–13, conservative) | **≥150** | `STATUS:300,306,314,316` |

`VERDICTS.md` states the architecture flatly: *"No resume for capture, plan, or materialize — only apply is journaled, so every failure restarts a multi-hour phase from zero."* The 22:21:02Z failure had **already completed the full 109 GiB custody read and then discarded it** over a required-set key mismatch (`STATUS:196-197`). The cheap replacement — a `(dev,ino,size,mtime_ns,ctime_ns)` census — is recorded in `TRUTH-BASELINE` as **"already computed and unused"** and costs **122 s** (`VERDICTS.md:641`).

**One change that kills it:** retire `stable_capture_pair` as the stillness proof; make the per-file freshness key the identity in capture, materialize *and* apply; extend the `git_entries_done` completion-marker technique (invented at 13:41Z, five days late) to every phase.

### P2 — Coordination dark time and a wrong charter: **≈1,646 min (26 %)**

| Component | Minutes | Evidence |
|---|---|---|
| One unbroken block with **zero ledger entries** | **1,182** | `STATUS:162` 08-27T21:07:18Z → `STATUS:163` 08-28T16:49:24Z |
| Quiesced-pair prologue, abandoned as unpairable | **464** | `STATUS:1–6` |

The 1,182-minute gap is 18.5 % of the entire ceremony and produced nothing. The 464-minute prologue was spent executing a charter (`TRUTH-BASELINE:7` "quiesced one-shot") that `BASES` E-1 proves is **false and inverts its own source** (`TIN-3692 92e84ed6`: "snapshot-source, uninhibited"). Cluster and substrate outages, by contrast, are **not** in this bucket: the 11:33Z honey+bumble drop was exonerated by three independent proofs (`STATUS:303`) and the etcd incident was exonerated at `STATUS:212` ("CEREMONY EXONERATED"). **Do not blame the rack for this week.**

### P3 — No read-only preflight: statically-discoverable corpus facts found by multi-hour transactional failure. **≈910 min net-new (1,180 gross)**

| Component | Minutes | Evidence |
|---|---|---|
| `.git-boundary` path-map miss killed v4 at P4 | **169** | `STATUS:122→123` |
| symlink-mode 0755/0777 (three sites, three discoveries) | **~152** | `STATUS:159,241,259` |
| APFS case-fold: killed v4.6 at 2 h 27 m, then again at 67 m *after* the full custody read | **214** | `VERDICTS.md:289`, `STATUS:166,196` |
| file-seat plan-path shape (discovery lap; the 267-min recapture it forced is counted in P1) | **371** | `STATUS:145,146` |
| shallow / unborn / tagless / promisor — apply runs 5–13 | **272** | `STATUS:284–314` |

`VERDICTS.md:288`: *"Zero `doctor`/`preflight`/`check` verbs: `bulkload --help` lists exactly seven, all transaction verbs."* And `:885`: *"Four of the five cross-kernel defects this week were statically discoverable."* Every one of these is a property of a corpus that was sitting on disk, unread, for four days. Thirteen of the first twenty laps were sqlite sidecar sweeps — also a read-only scan.

**One change that kills it:** `bulkload doctor --role {source,destination} --peer-ssh-host H` (T4), run once over the real corpus **before** the first capture.

### P4 — O(n²) and per-item durable writes: **≈455 min (7 %)**

`_plan_source_paths` recomputed 3× per materialize (~2.16 M ops each, 98.7 % CPU in pure Python, **zero quarantine bytes read in 1 h 43 m**) — `STATUS:172`, ≈250 min. Journal reseal per item: 1.29 GB rewritten after every one of 1,742,918 mutations, **~20-day projected ETA** — `STATUS:252`, ≈94 min. Journal write per git object: one 11-object repo took **15 minutes at 96 % CPU with zero I/O** — `STATUS:269`, ≈110 min. The same class was fixed at **three separate locations** on three separate days, each time only after it stopped a lap.

### P5 — Anonymous refusals: the engine cannot say what went wrong, so a lap is spent making it say. **≈70 min direct; `VERDICTS` prices the class at ~480 min**

Run-2's `apply.log` was **destroyed by the supervisor's own relaunch before anyone read it** (`DELTA-PACKET-0720.md:20`) → run-3 launched purely to reproduce (13 min). Run-5 refused *"Git mutation/verification failed (fsck)"* with **the repo not named and git stderr discarded by `_git()`** (`STATUS:284`) → run-6 launched purely to reproduce (13 min). Run-8 refused with an **anonymous raise, no repo, no diff** (`STATUS:296`) → run-9 launched purely to reproduce (26 min). The T1 final-push refusal named **no path** (`STATUS:226`).
Structurally: **296 of 378 `raise BulkloadError` sites (78.3 %, AST-counted) are fully-static strings naming no path, no root, no record, no expected/observed** (`VERDICTS.md:277`). The instrumentation fixes (`01b355a` 09:10Z, `6ee220c` 10:23Z) landed **on the last day of a five-day ceremony**. `VERDICTS.md:882` prices the class: *"`r2/required-diff.log` shows the operator hand-computing that diff in **53 s** to close a failure that cost ~8 h of laps."*

### P6 — Engine surgery on the critical path. Not separately priceable; it is the mechanism by which P1–P5 became wall-clock.

**≥20 engine commits hot-deployed to both sting runners mid-ceremony**, every one fleet-synchronous because `runtime_source_digest` (X1) must match on both hosts. This is not a neutral delivery method: **at least one defect class was created by a previous emergency fix** — `STATUS:163`, the launcher's unconditional pin overwrite at `bulkload.py:97`, which clobbered v4.5's own pin. Meanwhile the counterfactual sat in the same directory: the operator's **170-line rsync script** (`landing/delta-topup.sh`; 169 lines pre-judge) delivered the real payload delta — 17 verified bundles, 1.8 G, 4 stash patches, 9 dirty-tree artifacts — in one evening **while the engine was on lap 19** (`STATUS:175`, `VERDICTS.md:37`).

---

## 4. THE COUNTERFACTUAL TRADE LINE

Priced from the record, not asserted.

**What the engine path has cost.** 106 h 31 m end-to-end. 45 recorded failures. ≥62 pipeline launches across 16 hand-written orchestrator scripts. ≥20 fleet-synchronous engine hot-fixes. 20 distinct defect classes, 13 of them in the last 15 h 45 m at a sustained rate of one new class every ~70 minutes with no observable decay. Three published landing-time projections, all wrong, missed by >10 h, >2 h 45 m and >13 h respectively. **And it has not landed.** As of 13:45Z the apply verb is on launch 15, `STATUS.current` = `v4.14-apply`, and LANDING-READY has never been set.

**What the boring path costs.** Two estimates exist, produced independently by the same operator nine and a half hours apart from the same mechanism, and they agree. Plan file `:1494`, written 04:05Z: *"skip apply; land by hand per lanes/L1 — plain rsync delta + GitHub fetches + 15 bundles + 34 `git worktree add`. More attended work (~2–3 h of hands); no engine risk."* `STATUS:317`, written 13:42:52Z: *"the engine is killed and the landing finishes the boring way (adopt the installed repos, GitHub-fetch the remainder, delta-topup state, L1 §7 bundles/worktrees) — est 2-3h, cannot loop."* `L1:435` prices its own blocks 0–8 at **55–95 min for ≈234,600 files / ≈7.68 GiB**, and `L1:350` shows why the git half is nearly free: the fully-pushed worktrees are recreated on sting from origin and **"transfer nothing."** The honest correction to L1's number is that it assumes the 84 G payload already applied and does not price the GitHub re-clone of the un-adopted repos — the one step whose rate is measured nowhere in this ledger. Corrected estimate: **3 hours, worst case 4, all attended.** Against the engine path's 106 h 31 m and counting, from the 08-28 22:00Z reference point alone it is **15 h 45 m versus ~3 h — a 5× overrun on an open-ended tail.**

**The asymmetry that actually matters is not the multiple. It is that one path can loop and the other cannot.** Every engine lap this week ended in one of two states: landed, or a new defect class requiring a new commit deployed to two hosts before the next lap could start. Thirteen times in fifteen hours it was the second. `rsync --update`, `git fetch`, and `git bundle` have bounded failure modes: a step fails, you read the error, you rerun the step. There is no fourteenth defect class waiting inside `git clone`.

**What the engine path uniquely bought, checked rather than asserted.** One capability is real and load-bearing: **688 git worktree gitdir-pointer rewrites, 0 torn** (`planner.py:191-273`, `executor.py:1317-1428`), against a corpus where `L1:328` states that of 30 new top-level entries, 24 are worktrees whose `.git` gitfile points at an absolute `/Users/jess/...` path and *"Rsyncing them lands unusable worktrees."* rclone cannot do this. But `VERDICTS.md:19` adds the caveat the ceremony never acknowledged: *"the runbook's own §5 solves it in bash with `git worktree add`, per worktree, in minutes."* The SQLite WAL-consistent capture is real and is *"~6 lines of Python; the operator's own BLOCK 5 hand-writes the two-line sqlite3 equivalent"* (`VERDICTS.md:20`). The SQLite row-level union with conflict detection — genuinely sophisticated code — was **never exercised**: the ceremony deliberately emptied the destination, forcing `source_authoritative=True`, so *"every conflict-resolution capability was a no-op by construction"* (`VERDICTS.md:21`). The digest-bound custody chain with rollback is the one differentiated, actually-exercised win — and it is simultaneously the direct cause of most of the failures, because the A/B pair, the fsck-every-lap prefix, the per-mutation journal seal and the required-set case-fold comparison are precisely what manufactured defect classes 4, 6, 7, 8, 9, 10, 11, 12 and 13.

**Of the ~20 defect classes, roughly half are portable knowledge worth keeping** — darwin/Linux symlink-mode semantics, APFS case-fold vs XFS, cross-device reflink fallback, shallow and promisor-aware fsck. Any serious cross-platform git migration tool eventually needs all four. **The other half are self-inflicted by the engine's own architecture** — the per-item journal seal, the same O(n²) class at three sites, the live-fence censusing a different tree than the seal, the launcher clobbering its own trust channel, the mandatory A/B pair. Those are not problems in the boring path; plain rsync and git have no immutable-seal chain to disagree with itself.

**The trade line, stated plainly.** The engine spent 106 hours to not deliver a migration that a 170-line rsync script plus `git fetch` delivers in three, and the one capability that justified the engine — worktree pointer rewriting — is replicated by the runbook's own bash in minutes. `VERDICTS.md:29` reached this conclusion independently, before the apply phase even started: ship the git-workspace layer as a standalone `bulkload git-repair --from-manifest` verb that runs against a corpus rclone already moved, because *"it does not need custody, seals, plan digests, or an A/B pair to be correct."* The operator arrived at the identical trade twice, in-band, 9.5 hours apart, unprompted. **The record does not contain a single argument for continuing the engine path that survives contact with the record.** The only honest reason to let run-13 finish is that it may be nearly done — and that is a bet, not an argument, and this week has priced that bet at roughly one-in-eight.

---

## 5. BINDING STOP-RULE — FOR OPERATOR RATIFICATION

**Recommendation: ratify the operator's existing 16:00Z deadline unchanged, and add the four clauses it is missing. Do not extend it to 17:00Z.**

`STATUS:317` already sets a binding deadline of 16:00Z. Moving it to 17:00Z would be softening, and the base rate forbids it: **13 new defect classes in the 15 hours before run-13 launched = one every ~70 minutes.** Run-13 started 13:23:48Z. At that hazard rate the probability it reaches LANDING-READY by 16:00Z without a fourteenth class is **≈10–12 %** — worse than one in eight. The counter-argument, that the corpus's git pathologies are a finite set and the hazard should be decaying, is not supported: classes 10, 11, 12 and 13 all landed in the final four hours. **Run-13 is also running the *old* code** — the lap-cost fix `b4ea518` was deployed at 13:41Z and `STATUS:316` states run-13 keeps its loaded code — so run-13 is still paying the full re-fsck prefix the operator called unacceptable.

### 5.1 The rule

**TRIGGER — the rule fires on the FIRST of these to occur:**

- **(A) TIME.** 2026-08-29T16:00:00Z is reached and `STATUS.current` != `v4-LANDING-READY`.
- **(B) NEW DEFECT CLASS.** Any lap at or after run-13 fails on a defect class not already numbered or named in `STATUS:123–316` — i.e. a 14th class.
- **(C) NEW ENGINE COMMIT.** Any proposed remedy requires an engine commit that is not already deployed on both sting runners as of 13:41:30Z (`b4ea518`). The moment the fix is "deploy a new SHA to both hosts," the rule fires. This clause is the one that actually binds: every one of the last 13 laps died here.
- **(D) LAP STALL.** Any single lap at or after run-13 exceeds **45 minutes** wall without the git phase advancing past its previous high-water repo count.

**ON TRIGGER — mandatory, no interview, no re-derivation:**

1. Append the trigger letter and the UTC timestamp to `STATUS`. Set `STATUS.current` = `boring-landing`.
2. Kill `orchestrate-v4.14.sh`, its supervisor, and the sting-side apply pid. **DO NOT run rollback.** The repos apply has already installed are the asset; rolling them back re-buys the GitHub fetch you are about to skip.
3. Execute §5.2 to completion. **Nothing in §5.2 may be unblocked by an engine commit.** If a step fails, it is fixed with git, rsync or bash, or the loss is recorded explicitly in `STATUS` and the landing proceeds.

**IF LANDING-READY IS REACHED BEFORE 16:00Z:** the engine path finishes normally (autopilot + `walk-unattended.sh`), and clauses (B), (C) and (D) survive as standing rules over the post-landing chain.

### 5.2 The boring path — exact sequence, priced. **Est. 3 h (range 2 h 15 m – 4 h). Attended. Cannot loop.**

| Step | Action | Est. | Basis |
|---|---|---|---|
| 0 | Kill + ledger + `STATUS.current` = `boring-landing`. No rollback. | **5 min** | — |
| 1 | Census what apply actually installed: enumerate `/srv/fast-local/jess/git/*`, `git -C … rev-parse HEAD` each; write the adopted-repo manifest to the **internal** rescue dir `/Users/jess/.bulkload-ceremony-rescue-20260829` (not TinylandState — `STATUS:244` silent truncation). | **10 min** | `STATUS:306` ≥156 repos installed |
| 2 | GitHub-fetch the remainder: every payload repo absent from the adopted manifest that has a remote, cloned on sting from origin, 8-way parallel. **Transfers nothing over the neo→sting link.** | **40–70 min** | `L1:350` "Recreate on sting from origin, transfer nothing". **This is the one step whose rate is measured nowhere in this ledger — treat the estimate as the least reliable number in this table.** |
| 3 | `landing/delta-topup.sh` (170 lines, judge-verified 08-28T19:08Z: `sb()` fish→bash fix, de-slashed excludes, `--mkpath` ×6, L1 30-name exclude list, `--update`) — L1 BLOCKS 1–6: codex sessions, claude root, pi agent, file-seats + `~/emacs`, atuin, **229,399 git working-tree files / 6.00 GiB**. | **55–95 min** | `L1:435` total for these blocks; `STATUS:208` for the judge fixes |
| 4 | L1 §7: fetch the **17 staged bundles** (`landing/bundles`, 1.8 G, verified `STATUS:175`) as `refs/bundle/*`; apply the **4 stash patches**; restore the **9 dirty-tree artifacts** (`landing/dirty-trees`); run `landing/lanes/worktree-adds.sh`. **Honest caveat: that script automates only the 4 fully-pushed worktrees. The ~13 bundle-backed worktrees each need a branch name and are hand work — the script says so in its own comments.** | **20–35 min** | `L1:349-389`, `worktree-adds.sh` |
| 5 | Reconciliation, L1 BLOCK 8: case-fold `ls` returns **1** not 2; `find … -name .git -type f -exec grep -l "/Users/jess"` returns **0**; bundle count **17**; `git fsck` spot-check; `rclone check --checksum` count parity. | **5–10 min** | `L1:317-319, 396-399` |
| | **TOTAL** | **2 h 15 m – 3 h 45 m** | |

### 5.3 What ratifying this costs you — stated so nobody claims it was hidden

- **You lose digest-bound custody and the rollback journal** for everything the boring path moves. Verification degrades to `git fsck` + `rclone check --checksum` + count parity. That is materially weaker than what the engine offers, and it is still stronger than what most migrations get.
- **You lose automated gitdir-pointer rewriting** for repos apply has not installed. They become hand `git worktree add`, and only 4 of ~34 are scripted today.
- **You lose nothing else.** The A/B seals, the 84 G quarantine, the receipts and `final-stage.json` stay on sting and stay valid. The week's ~20 engine fixes are committed and open as PRs (#30/#31/#32 plus the T1–T4 chain); killing the run does not kill them. The portable cross-kernel knowledge is durable in `STATUS` and in this file.

### 5.4 Two clauses to ratify for the *next* ceremony, or this recurs verbatim

- **No ceremony starts until `bulkload doctor` (T4) exists and has been run read-only over the real corpus.** Four of the five cross-kernel defects this week were statically discoverable (`VERDICTS.md:885`). This clause is worth ~910 minutes by §3 P3's own arithmetic.
- **No ceremony starts until capture, plan and materialize carry resume markers, and `stable_capture_pair` is retired as the stillness proof** (`VERDICTS.md:641`; `TRUTH-BASELINE` already rules the A/B pair "unsatisfiable on a living host"). This clause is worth ~2,320 minutes by §3 P1's own arithmetic.

Together those two clauses address **51 % of the 6,391 minutes this week burned.** Everything else in §3 is a rounding error by comparison.

---

*Prepared read-only from the sources named at the head of this file. Every number is traceable to a cited line. Nothing was executed, deployed, or modified on any host.*
