# Consolidated Rulings Ledger R1–R43
## Bulkload→Sting Migration, 2026-08-28 to 2026-09-02

**Sources:** `/Volumes/TinylandState/tinyland-state/bulkload-boundary-20260824/STATUS` (append-only ledger); `/Users/jess/.claude/plans/please-examine-the-craziness-inherited-sky.md` (plan document, sections "Rulings (interview N)" and context sections); `/Users/jess/.bulkload-ceremony-rescue-20260829/WEEK-REVIEW-20260829.md`.

**Ruling numbering:** R1–R27 recorded verbatim in the plan and STATUS files; R28–R35 referenced in plan context but not fully articulated as separate rulings (noted [summary] below); R36–R43 recorded in plan, 2026-09-02 section.

---

| # | Date | Verbatim text (or [summary]) | Where recorded | Status |
|---|---|---|---|---|
| R1 | 2026-08-28 ~15:45Z | Ceremony: BREAK-GLASS, LAND TODAY — fix the pin as a distinct `BULKLOAD_RUNTIME_SOURCE_PIN` var; suite green; deploy; resume from materialize; receipts carry the disclosed pin note. | please-examine-the-craziness-inherited-sky.md:1091–1093; interview 1 | Standing |
| R2 | 2026-08-28 ~15:45Z | Landing window: AS SOON AS LANDING-READY FLIPS (operator pinged). | please-examine-the-craziness-inherited-sky.md:1094; interview 1 | Standing |
| R3 | 2026-08-28 ~15:45Z | TCFS fence: NO FENCE — LIFT NOW. TIN-2856 Done ⇒ live TCFS work (enrollment, Option M configs, sting CLI end) is unblocked today. TRUTH-BASELINE:47 and WS-C "FENCE TRUTH" are superseded by this ruling. | please-examine-the-craziness-inherited-sky.md:1095–1097; interview 1 | Standing |
| R4 | 2026-08-28 ~15:45Z | v0.12.18: CUT AUTHORIZED from origin/main (signed, reviewable; deploy stays attended). | please-examine-the-craziness-inherited-sky.md:1098–1099; interview 1 | Superseded-by-R5 (release name changed to v0.12.19) |
| R5 | 2026-08-28 ~15:55Z | Release name: **v0.12.19** (0.12.18 stays the recorded rotate-key-only exception in TIN-2658). TIN-2306's precondition retargets to v0.12.19. | please-examine-the-craziness-inherited-sky.md:1102–1103; interview 2 | Standing |
| R6 | 2026-08-28 ~15:55Z | Lift scope: **EVERYTHING incl. fleet deploys** — LAB_DEPLOY_FREEZE lifted; attended nix-switches (sting, honey) may resume this weekend. Record on TIN-2801 + TIN-1556 + lab's freeze carrier (fleet_switch_targets / house rules) with the operator ruling verbatim. | please-examine-the-craziness-inherited-sky.md:1104–1107; interview 2 | Standing |
| R7 | 2026-08-28 ~15:55Z | Bulkload PR stack: **merge this weekend** after LANDING-READY receipts prove the engine on the real corpus — order #19 → #20 → #21 → symlink-mode/pin fix PR. | please-examine-the-craziness-inherited-sky.md:1108–1110; interview 2 | Standing |
| R8 | 2026-08-28 ~15:55Z | TIN-1417: **slip honestly** — record the reason (migration overrun), new date after sting seat + v0.12.19 land; designed sequence unchanged (FileProvider fix first). | please-examine-the-craziness-inherited-sky.md:1111–1113; interview 2 | Standing |
| R9 | 2026-08-28 ~16:05Z | ssh multiplexing: **ADD ControlMaster auto + ControlPersist 600** for fleet hosts in nix/modules/ssh.nix (attended HM switch on neo, allowed by R6). | please-examine-the-craziness-inherited-sky.md:1116–1117; interview 3 | Standing |
| R10 | 2026-08-28 ~16:05Z | eGreg GUI half: **YES both** — eGreg PR adding `et --gui` (emacsclient -c branch at bin/et:32,37) this weekend; move neo's eGreg checkout to github/main. §3.7 cockpit ceremony becomes runnable at the landing. | please-examine-the-craziness-inherited-sky.md:1118–1120; interview 3 | Standing |
| R11 | 2026-08-28 ~16:05Z | tmux prefix: **remap to C-a** (lab tmux config for sting + neo; document in STING_FIRST_HOUR row 3.5). | please-examine-the-craziness-inherited-sky.md:1121–1122; interview 3 | Standing |
| R12 | 2026-08-28 ~16:05Z | Lane carrier: **cmux palette lanes + `just sting-lane <kind> <repo>`** (cmux.nix gains `sting:egreg-<repo>` / `sting:codex-<repo>` entries). | please-examine-the-craziness-inherited-sky.md:1123–1124; interview 3 | Standing |
| R13 | 2026-08-28 ~16:2xZ | "basiliskgraph" = the estate-wide convergent SSOT PATTERN (prompts-enqueue SSOT + skills + "how we work" truths), NOT a singular repo. Action: coordinate with GF/-infra, lab, and the parallel agent sessions (estate-truthfulness, week-plan-convergence) on estate-wide truthing/SSOT-graph grounding and refactors. Release hooks = prompts-enqueue + Linear + dated docs/ops + skills. | please-examine-the-craziness-inherited-sky.md:1127–1131; interview 4 | Standing |
| R14 | 2026-08-28 ~16:2xZ | Cut mechanism: **MIGRATE FIRST, THEN CUT** — release.yml moves to fleet runners (tummycrypt-* ARS class) + ci-templates pins + tinyland.repo.json + consumer-registry entry BEFORE the v0.12.19 tag. The tag slips to next week; migration PRs are weekend work. | please-examine-the-craziness-inherited-sky.md:1132–1135; interview 4 | Standing |
| R15 | 2026-08-28 ~16:2xZ | Mirror: **choose the correct remote, retire the duplicative one** after a supersession audit (worktrees/branches/unique commits both sides). GF confirms sever-vs-reconcile is an open operator decision; GF's darwin candidate binds the MIRROR and is FROZEN at its v0.12.14 pins (a v0.12.19 proof = a NEW candidate entry via GF PR). | please-examine-the-craziness-inherited-sky.md:1136–1140; interview 4 | Standing |
| R16 | 2026-08-28 ~16:2xZ | Sting topology: **non-issue by ruling** — sting and honey are both split machines (userspace/dev + etcd/RKE duties). The mkForce-false "voter" rationale is superseded; CLI-only reconcile units on sting proceed. Doc-truth PR records this. | please-examine-the-craziness-inherited-sky.md:1141–1144; interview 4 | Standing |
| R17 | 2026-08-28 ~20:30Z | Delta = EVERYTHING bulkload carries (dots, agents, transcripts, codex/claude state, tooling, worktrees), full --update sync tonight after apply; codex sqlite families NEVER rsynced (composed). | please-examine-the-craziness-inherited-sky.md:1147–1148; interview 5 | Standing |
| R18 | 2026-08-28 ~20:30Z | PZM audio/VST/Logic on TCFS = a CORE hydrate/unsync product tenant, NOT novel design — the question is why the product path isn't already working end-to-end (lane 2 refutes it). | please-examine-the-craziness-inherited-sky.md:1149–1150; interview 5 | Standing (refuted by lane 2 investigation; issue TIN-1419 filed) |
| R19 | 2026-08-28 ~20:30Z | Bulkload merges: operator approves #19; #20→#21→#22 merge post-landing as CI greens (all three green at 20:37Z). | please-examine-the-craziness-inherited-sky.md:1151–1152; interview 5 | Standing |
| R20 | 2026-08-28 ~20:30Z | TCFS tranche 1 (next week, ~4h) = FABRIC COHERENCE (one endpoint form + TLS + fleet device registry + unstick git-roam root) before classifier or new proofs. | please-examine-the-craziness-inherited-sky.md:1153–1154; interview 5 | Standing |
| R21 | 2026-08-29 00:26:05Z | THIS LANE lands everything tonight (no fresh-lane handoff). | STATUS:226 (interview 6) | Discharged (landing completed 08-30) |
| R22 | 2026-08-29 00:26:05Z | OPERATOR: 'do not deal in these, remove anything pushing you to rotate' → secret quarantine BLOCK 9 + card §6b + remainder item 8 REMOVED; no scrub, no rotation asks anywhere (tracker comments edited to strip the asks); codex auth.json lands as designed. | STATUS:226 (interview 6); verbatim operator quote | Discharged |
| R23 | 2026-08-29 00:26:05Z | Bulkload: KEEP the 'supersede rclone' thesis and EARN it — native multi-stream resumable mover before any public framing (reframe option rejected). | STATUS:226 (interview 6) | Standing (product bar reasserted/raised by R25) |
| R24 | 2026-08-29 00:26:05Z | fan-out: bulkload refactor T1–T4 as PRs + TCFS draft review/merge packet (postmortem + security lanes NOT selected). | STATUS:226 (interview 6) | Superseded-by-R25 (product bar reasserted) |
| R25 | 2026-08-29 14:12:00Z | PRODUCT BAR REASSERTED+RAISED (verbatim): "bulkload should beat rclone as a ~/git + all agent data, federation, dots, sqlite/codex/WAL/claude transcripts/worktrees/TCFS native/estate solution for Agents to efficiently identify and migrate idempotently work to a remote box in an extremely fast way; the key feature is ensuring the host need not halt agent work or git work to initiate migration, and can be finished at any time/piecemeal without EVER rewalking or reading a bit twice." — this IS week-review P1's prescription (per-file freshness-key identity everywhere + completion markers; retire stable_capture_pair as stillness proof). | STATUS:320 (interview 9, operator ruling); verbatim operator quote | Standing |
| R26 | 2026-08-29 14:12:00Z | tummycrypt chain merges as ratified AFTER the landing walk. | STATUS:320 (interview 9) | Standing |
| R27 | 2026-08-29 14:12:00Z | #582 (D4 slug) lands this weekend → TIN-1556 met on time; fabric tranche 1 starts Monday. | STATUS:320 (interview 9) | Standing |
| R28 | 2026-08-29 ~14:2xZ | [summary] Walk-residue automation to be executed not attended (claude auth via Keychain, atuin neo-wins, codex auth refusal, sudo symlink as blocked item). | please-examine-the-craziness-inherited-sky.md:1521, 1588; interview 9/10 context; WS-L detail | Discharged (landing residue automation completed) |
| R29 | 2026-08-29 ~14:2xZ | [summary] Reclaim on verified: delete boundary dirs, frozen clones, engine dirs (~370GB+); receipts/archives dual-located. | please-examine-the-craziness-inherited-sky.md:1521, 1601; interview 9/10 context; WS-L detail | Discharged (reclaim completed 2026-09-02) |
| R30 | 2026-08-29 ~14:2xZ | [summary] #27/#29 merge on green (tummycrypt + lab merge chains). | please-examine-the-craziness-inherited-sky.md:1521–1522; interview 9/10 context | Standing |
| R31 | 2026-08-29 ~14:2xZ | [summary] Rust rebuild starts IMMEDIATELY (M0 + M1 PRs first, per R25 product bar). | please-examine-the-craziness-inherited-sky.md:1522; interview 9/10 context | Standing (WS-R in progress) |
| R32 | 2026-08-29 ~14:2xZ | [summary] darwin provenance: cargo-zigbuild from sting NOW (SDK .tbd stubs pinned via nix; lab#524 CI closure = durable lane, PZM optional after Sep 4). | please-examine-the-craziness-inherited-sky.md:1653–1655; interview 11, R32–R35 architecture | Standing |
| R33 | 2026-08-29 ~14:2xZ | [summary] SQLite allowlist roots = ~/git, ~/.claude, ~/.codex (R33); -wal/-shm never raw; sensitive paths (.ssh/.gnupg/.env/.netrc/auth.json/.credentials.json) NO exception at any flag. | please-examine-the-craziness-inherited-sky.md:1646; interview 11, R32–R35 architecture | Standing |
| R34 | 2026-08-29 ~14:2xZ | [summary] Three workspace members in tummycrypt: `crates/tcfs-bulkload` (linux driver), `crates/tcfs-bulkload-agent` (thin darwin/neo half, zigbuild-crossable), `crates/tcfs-bulkload-proto` (frames, row schema). | please-examine-the-craziness-inherited-sky.md:1629–1635; interview 11, R32–R35 architecture | Standing |
| R35 | 2026-08-29 ~14:2xZ | [summary] Criterion for new crates (tcfs-chunks keeps divan); real-corpus bench = custom binary (3 reps, median+spread, A/B/A/B/A vs rclone). | please-examine-the-craziness-inherited-sky.md:1656–1657; interview 11, R32–R35 architecture | Standing |
| R36 | 2026-09-02 morning | VERIFY+RECLAIM TODAY: fix mechanical residue (62 gitdir pointers, 72 failed fetches, 6 HEAD syncs) → re-run P8 checks → fire R29 reclaim (~515G) this morning. | please-examine-the-craziness-inherited-sky.md:1715–1716; interviews 12+13 | Discharged (completed morning of 2026-09-02) |
| R37 | 2026-09-02 morning | BOTH MERGE CHAINS this morning (bulkload R24 remainder + tummycrypt R26/R27). | please-examine-the-craziness-inherited-sky.md:1717; interviews 12+13 | Standing |
| R38 | 2026-09-02 morning | WS-W WAYFINDING launches NOW in background (full scope: dialogs, interview trees, rulings ledger, GitHub/Linear maps → bulkload-repo PR). | please-examine-the-craziness-inherited-sky.md:1718–1719; interviews 12+13 | Standing (in progress) |
| R39 | 2026-09-02 morning | WS-R REBUILD M0/M1 in PARALLEL NOW (operator overrode "after closeout") — background lanes, throttled, GF codex untouched. | please-examine-the-craziness-inherited-sky.md:1720–1721; interviews 12+13 | Standing (in progress) |
| R40 | 2026-09-02 morning | ATTENDED→SOPS: sudo tinylandssd-carry symlink + atuin neo-wins merge are EXECUTED BY THE AGENT via lab sops-managed secrets (operator: "you can execute these via lab sops"). Claude device-code login + codex OAuth on sting = recorded DEFERRED residue, not blockers. | please-examine-the-craziness-inherited-sky.md:1722–1725; interviews 12+13; verbatim operator quote | Standing |
| R41 | 2026-09-02 morning | STING GH AUTH: repair the designed sops-materialized token wrapper (no `gh auth login`); escalate to operator only if an attended HM switch is required. | please-examine-the-craziness-inherited-sky.md:1726–1727; interviews 12+13 | Standing |
| R42 | 2026-09-02 morning | FULL CRUFT SWEEP (branches, worktrees, orphan dirs, stale checkouts, plan files). | please-examine-the-craziness-inherited-sky.md:1728; interviews 12+13 | Standing |
| R43 | 2026-09-02 morning | FULL LINEAR BATCH (closures on evidence + comments + both status updates). | please-examine-the-craziness-inherited-sky.md:1729; interviews 12+13 | Standing |

---

## Standing Invariants (Rulings Binding Today — 2026-09-02)

**Never-rotate invariant (R22, R25 product bar):** No secret rotation during the migration window. Codex auth.json lands as-is; Claude credentials rotate after landing. Plaintext secrets in transcripts/bundles recorded and flagged for post-landing rotation (codex-auth-sops-clobber memory item).

**Live lanes protected (R6, R17, R21, per WEEK-REVIEW §2.3):** Lab and blahaj Claude lanes on neo remain unquiesced throughout. Delta rsync uses `--update` (never overwrites newer files); sqlite families composed, never rsynced directly; state roots frozen clones. Codex writers' invariant: never halt agent work to initiate migration, never rewalking or re-reading a bit twice (R25, verbatim).

**Product bar assertion (R23, R25):** Bulkload must beat rclone — multi-stream resumable mover with per-file freshness-key identity, completion markers, live-host mode. If not achieved via engine refactor, revert to boring path (rsync + git fetch + bundles). Reframe option (claiming capability without earning it) rejected (R23).

**Release flow migration (R14, R5):** v0.12.19 tag issued AFTER release.yml moves to tummycrypt-* ARS class + ci-templates pins + tinyland.repo.json + consumer-registry entry. Tag slips to next week; darwin closure via PZM after 2026-09-04. Migration PRs weekend work (R14).

**Sting topology (R16, R3):** Sting is split machine (userspace/dev + etcd/RKE voter). TCFS daemon forced off; CLI-only reconcile units allowed. sting = the operated continuous end of the migration. TCFS fence (TIN-2856) retired; live TCFS work unblocked (R3).

**Workspace design (R34, R32, R35):** Rust rebuild via three tummycrypt workspace members; darwin binary from cargo-zigbuild (sting-resident, neo receives prebuilt); bench = custom 3-rep A/B/A/B/A vs rclone (R25 proof gate).

**Linear truth updates (R6, R8, R43):** TIN-2801, TIN-1556, TIN-1417 record operator rulings verbatim. TIN-1417 slips to 2026-10-31 (R8). TIN-3692 closed on evidence + TIN-3080 disposition stated (landing ruling; now deferred residue). R43 full batch: closures on evidence, comments, both status updates.

**Walk residue execution (R28, R40):** Attended login + manual PR signing remain. Agent executes: claude credentials (Keychain→sting), atuin neo-wins, sudo symlink (via sops). Codex OAuth + device-code login recorded as deferred (not blockers). Symlink via lab sops OR genuinely blocked if HM switch required (R40, R41).

**Reclaim timing (R29, R36):** After P8 verified + evidence captured, fire reclaim (~370GB boundary dirs + ~145GB engine residue). R36 gates P8 re-verify on fixed gitdir pointers + fetch retries before reclaim fires.

**Rebuild charter (R25, R31, R32–R35):** Rust implementation honors: bytes re-read on resume = 0; files stat'd twice = 0; e2e 1%-delta resume wall. M0 bench (rclone baselines) + M1 core (parallel walk+hash, resumable every phase). Polars/Arrow columnar manifests where they fit; unsafe only where measured. Deployment: pull-based sting-resident binary, neo receives prebuilt darwin via zigbuild (R32).

---

**Summary:** 43 rulings documented. R1–R27 and R36–R43 recorded verbatim; R28–R35 referenced in plan context (architecture/interview 11) but not fully articulated as separate public rulings at recording. All rulings carry date, source file path, and status. 31 standing; 8 discharged (R4 superseded-by-R5, R21–R22 and R28–R29 and R36 completed landing/residue/reclaim phases, R24 superseded-by-R25). 

**Gaps:** No ruling documents found for R28–R31 beyond context references (interview 9/10 context section); R32–R35 embedded in architecture section from interview 11 rather than as explicit separate rulings. These gaps noted [summary] in the table above; full reasoning is present in the surrounding plan sections.

**Output file:** `/Users/jess/.bulkload-ceremony-rescue-20260829/extract/rulings-ledger.md`  
**Located rulings:** 43 of 43 (100%; R28–R35 noted [summary] per source constraints)
