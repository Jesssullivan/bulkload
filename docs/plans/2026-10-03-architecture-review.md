# bulkload architecture review and refactor plan (2026-10-03)

Ruling: OI-1003-Q14 (adversarial architecture review and sprawl audit), R-N13. This is analysis only: nothing was edited, committed, commented or filed.
Base: `origin/main` @ `727493a` (64,782 Rust lines). The charter is PR #137 `docs/slo.md` (still open).
Inputs: 63 critic findings from four lenses (sprawl, architecture, parallelism, slo-provability). After deduplication they reduce to 11 work packages.
User bar: migrating neo to sting should feel like "just another execution of something we trust", not a major scheduled event.

## 0. Verification ledger (judge spot-checks against 727493a)

**Confirmed against the code:**

| Claim | What I checked |
|---|---|
| carry_v2 has no product caller | Outside its own module, carry_v2 is used only by `tests/git_carry_v2.rs:38` and `tests/fault_harness/git_ingest.rs:30`. It is 4,399 production lines. The W6 frames are "(reserved, W6)" in `frame.rs:47,61,74-80`. |
| v1 `git()` misses part of the hardening | `git_carry.rs:28-65` lacks `GIT_NO_LAZY_FETCH`, `maintenance.auto=false`, `GIT_CEILING_DIRECTORIES` and `LC_ALL`. `estimate.rs:718` `hardened()` adds them. There are 0 `LAZY\|promisor\|partialclone` hits in `git_carry.rs` and `estate.rs`. |
| No background priority anywhere | 0 hits for `setpriority\|ioprio\|IOPOL\|setiopolicy\|taskpolicy\|SCHED_IDLE` in `crates/*/src`. `set_thread_qos` is called only from `io/tests.rs:415-422`. On Linux it is a `const fn` no-op (`sys_linux.rs:416`). |
| 4 metadata censuses per changed git item | `capture_key_parts` (`git_carry.rs:677`) is called at `estate.rs:913` and `:970`, and `export_repository_inner` censuses at `git_carry.rs:1073` and `:1096`. No `census_walks` counter exists. |
| v1 rerun re-packs full history | `shared.rs:149-156` runs `bundle create --all` when there is no `base`. The private repo uses `objects/info/alternates` → source (`git_carry.rs:3952`). The prerequisite comes from the plan's `base` (`estate.rs:939`), not from the retained capture. |
| Four S3 counters are never incremented | `SourcePackReadback`, `SourceCaptureReuseRead`, `OtherChunkRead` and `SourcePackWrite` have 0 references outside `counters.rs`. |
| Every git failure is the same refusal | `output()` maps any non-zero git exit to `GitInventoryMalformed` (`git_carry.rs:66-73`). That code appears 149 times in `git_carry.rs`. There are 104 non-test `Io(None)` sites, and 35 of them are in `provider_sqlite*`. |
| Closure parses Display text | `closure.rs:84-94` uses `split_whitespace().next()`. `passes()` is only `remaining_unaccounted()==0` (`:191`), so `Refused` counts as closed. |
| `serve` writes state before refusing an overlap | `transfer.rs:686-689` calls `Store::open(&state)` before the `SnapshotRootsOverlap` check. |
| Test clock runs one hour ahead | Under `cfg(test)`, `capture_clock` returns +3.6e12 ns (`transfer.rs:528-540`). |
| No-clobber blocks updates | The rerun test asserts `GIT_DESTINATION_OCCUPIED` (`transfer/tests.rs:1218`). The bench preconditions with `remove_delta_targets` outside timing (`bench main.rs:519,747,896`). |
| HEAD and index drift still refuse | `drift_only` requires `head ==` and `index ==` (`git_carry.rs:454-466`). `GitAuthorityChanged` is returned at `:1105-1110` (#38). |
| Nine refusal variants are never constructed | Each has 0 non-test references in agent and bench. |
| 35 stale W4 `dead_code` allowances | `"wired in by W4"` appears 35 times. |
| Property tests are rare | `proptest!` appears in 3 files only. |
| Other single-site facts | `CAPTURE_WORKERS=4` (`transfer.rs:67`). Estate `jobs` is capped `1..=2` (`estate.rs:1095`). `estate.rs:619` duplicates `hash.rs:62` `hash_file`. `copy_hashing` touches no counter. handoff spawns sops, kubectl, gpg, gh, claude and codex. `closure.rs` has its own `Json`/`JsonParser`, and handoff has its own `json_escape`. The `m1-spike` feature and `git_m1_spike` test target exist. 26 verbs are dispatched in `main.rs`. |

**Plausible, not independently re-measured (kept, flagged):**
- The M0 timing numbers: BLAKE3 at 1.15 GB/s, F_FULLFSYNC at 53–85 ms, and the 28 MB/s link ceiling. These come from the critics' reading of the evidence docs.
- The SQLite read-only WAL lock and `-shm` behaviour. This is standard SQLite semantics, but no repo test demonstrates it.
- That `serve`'s stdout `LineWriter` splits binary frames at 0x0A. Std `Stdout` is line-buffered and serve uses `std::io::stdout()` at `main.rs:220`. Whether the transfer layer wraps it first was not traced.

**Line-number drift, claim upheld:** the census call sites (finding 5) moved slightly. The 4x count still holds via `capture_census_planned`.

**Dropped:** none of the top claims failed verification. Two claims are discounted rather than dropped: the "load1 +4–6" estimate (an unmeasured projection) and the parallel-walk payoff (speculative until the receive side is fixed).

## 1. Executive summary

**What bulkload gets right**
- **A real source/destination custody model on the file path.** Transfer and walk use fd-relative `openat_beneath` with `O_NOFOLLOW` per component, StatIdentity pinning, racy-seat exclusion (R25) and no-clobber publication.
- **Durability is designed, not hoped for.** Group commit, a data-before-record discipline, an `io-trace` recorder and the R-N88 crash-state checker cover copy and pull.
- **Hard-cut wire versioning.** There is no dual stack, the refusal taxonomy is typed at the proto layer, and the dependency wall is enforced by test.
- **Drift is already a concept.** R-N30/R-N72 `captured-with-drift` exists for seats, and the closure report exists at all.

**The five biggest structural problems**
1. **S2 is held by convention, not by construction, and part of it is false today.**
   - The charter claims background priority, but none exists in any verb.
   - v1 export runs on the live source without lazy-fetch or partial-clone protection.
   - One untyped `git(&Path)` builder serves source, private and destination repos, including the `index.lock` writer.
   - `serve` writes its state store before checking that it does not overlap the source.
   - The charter's literal "no locks" wording cannot hold for the mandated SQLite backup API.
2. **There are two Git engines, and the one in production is the one that cannot do incremental work cheaply.**
   - v1 bundles re-pack all history from the source on every changed rerun, run 4 censuses per item, and count none of those pack reads.
   - carry_v2 (about 9k lines including tests) is reachable from no verb.
   - That leaves two resume stories, two hardening stacks and two flush stacks.
3. **Refusals stop being values at the estate boundary.**
   - 104 `Io(None)` sites and 149 catch-all `GIT_INVENTORY_MALFORMED` sites; stderr is discarded.
   - Closure re-parses Display text, so a `Refused` item passes as closed.
   - Closure covers only Git estate items; files and SQLite have no ledger.
   - As written, S4 ("0 unaccounted, every refusal dispositioned") is unprovable.
4. **The migration is a choreography, not a run.**
   - Users drive 26 verbs, 7 of them restore/import/attach side doors outside the ledger.
   - There is no single `sync` and no partition between file walk and estate items.
   - A changed destination file can never be updated (`GIT_DESTINATION_OCCUPIED`).
   - HEAD or index movement on a live repo still refuses (#38).
   - So S3 and S5 reruns produce refusals instead of converging. This is the direct opposite of "just another execution".
5. **The S1–S5 claims are not yet measured where it matters.**
   - The S3 byte accounting is spread across 5 uncoordinated counters, and child-process reads are invisible.
   - The bench has no S3 ratio, no S2 sampler and no remote (gate b) arm. Its 23-file corpus never exercises per-entry costs.
   - It sidesteps the update path by deleting delta targets.
   - Proptest is used in 3 files only.

The codebase grew from 27k to 65k lines in 10 days. About **15k lines (around 23%) can go with no loss of product capability** (section 3).

## 2. Ranked work packages

Ranking is by payoff toward "the migration is just another trusted run", per unit of cost and risk. Rulings come first because several packages cannot start without them.

### WP0. Rulings batch (no code). Prerequisite for WP3, WP4, WP6 and WP9
- **Findings merged:** v1 vs v2 (sprawl 1, arch 3); SQLite S2 carve-out (sprawl 14, arch 9, slo 3); S3 delta-clause restatement (slo 8); superseding publish (slo 7); estate-shaped S1 corpus (par 4); priority mode vs S1 (sprawl 6, arch 10, par 5, slo 1); source-ledger durability (par 6); W6 gate retirement.
- **SLOs:** all.
- **Payoff:** unblocks most of the plan. **Cost:** one operator interview. **Risk:** none.
- **Recommended defaults:**
  - (a) One Git engine: v1 plus auto-prerequisite. Retire v2, the W6 gates and #48.
  - (b) Amend S2 with a typed per-kind `SourceAccess`, with SQLite shared-read as a bounded and measured exception.
  - (c) Restate S3(delta) as two inequalities: source reads ≤ Σ changed or racy seat sizes; wire bytes ≤ Σ absent chunks.
  - (d) Allow superseding publish only when the destination output's (dev, ino, stat) equals this store's ledger row.
  - (e) Add an estate-shaped corpus to S1 alongside R23.
  - (f) Source-side verbs run at background priority by default. S1 samples record the class. Gate (a) may opt out only through an explicit, recorded flag.
  - (g) The source ledger may run `synchronous=NORMAL` and `fullfsync=OFF`, because destination Reuse carries R25.
- **PR breakdown:** a dated amendment to `docs/slo.md` (on PR #137 or right after it) plus a design.md W6 retirement note.

### WP1. S2 source-safety quick wins. Highest payoff per line
- **Findings merged:** sprawl 2; arch 2 (hardening drift and partial clone); sprawl 6, arch 10, par 5 and slo 1 (priority, four critics agree); slo 4 (serve overlap ordering); arch 8 (gc/repack under alternates becomes drift); par 6 (F_NOCACHE/DONTNEED, source ledger fsync).
- **SLOs:** S2 primarily, S5.
- **Payoff:** high. **Cost:** low, about 300–500 changed lines.
- **Risk:**
  - `GIT_NO_LAZY_FETCH` needs Git ≥2.45 or a backport, so it refuses on old Git. That is the intended fail-safe.
  - IDLE IO can starve gate (b), which is why the class must be recorded.
- **Dependencies:** WP0(f) for the default class and WP0(g) for the ledger change. Everything else can start now.
- **PRs:**
  1. One `GitEnv` table feeds `git_carry::git()` and generates or tests the `PROBE_SCRIPT` preamble. Delete the duplicate layering in `estimate::hardened` and `carry_v2::pinned`. Add a typed `GIT_SOURCE_PARTIAL_CLONE` refusal in v1 export and estate-capture. Add a partial-clone fixture test.
  2. `io::sys::enter_background()` as the first statement of `main` for source-side verbs (serve, estate-capture, snapshot, git-carry-estimate), inherited by children. On Linux: `setpriority(19)` plus `ioprio IDLE`. On Darwin: `IOPOL_THROTTLE` plus `QOS_CLASS_BACKGROUND`. Report the class in counters. Delete the dead `Qos` plumbing it replaces.
  3. Check the serve/copy state-vs-root overlap on canonical paths before any create. Add a proptest showing the source census is unchanged.
  4. Record the identity of the pack-dir listing with the authority. On a git child failure where that identity changed, classify as `DriftKind::ObjectStoreRewritten` rather than as a refusal.
  5. Behind ruling (g): set `FADV_DONTNEED`/`F_NOCACHE` after each consumed range on capture fds, and relax the source-ledger fsync.
- **Issues subsumed:** none directly; this advances #34. It is adjacent to #105 (same-build preflight) and to #134.

### WP2. One Git carry engine; make v1 incremental and accounted
- **Findings merged:** sprawl 1; arch 3; slo 15 (repack every rerun); sprawl 16 (copy_hashing uncounted); slo 6 partial (child reads); sprawl 5 counter part (`census_walks`).
- **SLOs:** S3, S4, S2 (less source pack IO).
- **Payoff:** high. It removes about 9.1k lines plus 2.6k of spike and gives one resume story. **Cost:** medium.
- **Risk:**
  - Option A discards W6 M1 work. Tag the commit first.
  - An auto-prerequisite chain makes restore depend on retained base bundles (the R-N72 binding question). Use bounded chain depth with periodic re-base.
- **Dependencies:** ruling WP0(a).
- **Resolved conflict:** the sprawl critic says delete v2, the architecture critic says wire v2 and retire v1.
  - **The judge sides with delete.** v1 is the engine with cohort evidence behind it and all the restore/closure plumbing.
  - The S3 gap that motivates v2 (full re-pack) is closed far more cheaply by deriving the bundle prerequisite from the previous retained capture's tips.
  - Wiring v2 means a new wire frame family, a new destination crash surface and a corpus format migration, all before the migration can be "routine".
  - Revisit v2 only if WP6 measurements show bundle cost still breaks S3 after the auto-prerequisite.
- **PRs:**
  1. *(Independent of the ruling, land first.)* Count pack-objects/bundle source reads into `SourcePackReadback`. Route `copy_hashing` through counters. Add a `census_walks` counter.
  2. Auto-prerequisite from the retained capture's tips. Bounded chain depth. Restore verifies the chain.
  3. Delete carry_v2 (all 7 files), `tests/git_carry_v2.rs`, `fault_harness/git_ingest.rs`, `fault.rs` v2 points, reserved frames and `TAG_PACK_DATA`; bump `wire_id`. Also delete `tests/git_m1_spike.rs`, the `m1-spike` feature and its `[[test]]` entry.
  4. design.md: retire W6 gate rows with a dated note. Close the v2-only issues as superseded.
- **Issues subsumed (closed as obsolete if Option A is ratified):**
  - #48 (M2 W6 epic)
  - v2 ingest: #83, #84, #85, #89, #90, #91, #93, #120 (with open PR #136, which should be paused pending the ruling), #121, #122
  - Partly #70: the stderr store stays for estimate.
  - Advances #134 (space check undercounts history).

### WP3. Typed refusals, typed outcomes, closure over every item kind (S4 backbone)
- **Findings merged:** arch 7; slo 20 (internal invariants reported as bare IO); sprawl 14 (provider_sqlite `Io(None)`); slo 19 (closure passes with undispositioned refusals); arch 8b (closure is git-only); sprawl 12 (dead refusal codes); sprawl 13 (hand JSON parser).
- **SLOs:** S4.
- **Payoff:** high; it makes S4 provable. **Cost:** medium. Hundreds of mechanical `?` sites change; the compiler drives it.
- **Risk:**
  - The outcome record codec changes, so old ledgers need a reader for the string form.
  - Refusal code changes cross the wire, so a hard-cut bump is needed (combine it with the WP2 bump).
- **Dependencies:** none for PRs 1–3. PR 4 composes with WP4.
- **PRs:**
  1. Delete the 9 dead refusal variants. Add a test that every variant has a constructor. Add `ProtocolStateViolation`, `WorkerLost`, `SqliteBackupFailed(code)` and `GitChildFailed(stderr_class)`.
  2. Remove the blanket `From<io::Error>` and `From<postcard::Error>` in the agent. Add a `.refuse_at("site")` extension trait. Add an `Io(None)` allowlist-count test that shrinks monotonically. Route v1 git children through the estimate `run_child`/StderrStore classifier instead of `Stdio::null` plus catch-all.
  3. Persist a typed `Outcome` and `Refusal{code, site, errno}` with postcard; closure matches the enum. Add `RefusedPendingReview` plus a disposition ledger (accept / re-carry / abandon, reviewer, date, standing-policy rows). Attestation requires plan and source binding and cannot attest away untyped IO. Replace `closure::Json` with serde_json or delete it when the attestation shrinks (WP4).
  4. Transfer refusals and SQLite provider outcomes are emitted into the same ledger format.
- **Issues subsumed:** #133 (via PR #135, land first), #126, #127, #106 (catch-all refusal half), and #124 and #129 (disposition path for conservative refusals).

### WP4. Close the side doors; one `sync` verb
- **Findings merged:** sprawl 3 (7 restore/attach verbs bypass the ledger); sprawl 4 and arch 8 (no single entry point, 26 verbs); par 4-adjacent partition (the file walk has no `.git` partition).
- **SLOs:** S3 (a rerun is the same command), S4 (one ledger per run), S5 (the delta pass is the next sync).
- **Payoff:** highest of all for the user's bar. **Cost:** high. **Risk:** the orchestrator becoming a second engine. Keep it to sequencing plus ledger only.
- **Dependencies:** WP3 PR 3 (typed outcomes); WP2 (one git engine). Ideally WP5 before it is advertised as routine.
- **PRs:**
  1. Fold attach-matching, attach-standalone and registered-payload into estate-apply item modes chosen from the plan, so they write the native journal.
  2. Put `git-export`, `git-import`, `git-restore` and `git-restore-linked` behind a `debug-verbs` feature (or delete them). Shrink the attestation ledger to the historical cohort rows.
  3. `sync PLAN HOST`, a thin orchestrator. Steps:
     - preflight: same build on both ends (#105), device and space (#39, #134)
     - estate-capture, pull corpus, estate-apply
     - provider-state steps
     - ordinary-file pull of the remainder, with estate item roots excluded from the walk
     - one closure ledger
     The component verbs become unlisted plumbing.
  4. Make the skill and runbook the single command; the migration run becomes "run `sync` until closure passes".
- **Issues subsumed:** #102, #103, #105, #132, #39, and part of #40 (handoff leaves the agent, see WP10).

### WP5. Live-source convergence: drift custody instead of refusals
- **Findings merged:** slo 16 (HEAD and index drift, #38); slo 17 (transfer vanished or changed becomes bare IO); slo 7 (superseding publish); slo 22 (convergence metrics); arch 8 (ObjectStoreRewritten, landed in WP1).
- **SLOs:** S5, S3.
- **Payoff:** high. Without it a live neo produces refusals on every rerun. **Cost:** medium-high.
- **Risk:** superseding publish weakens the no-clobber invariant that R-N88/R-N119 rely on. It needs ruling WP0(d) and new crash_check traces.
- **Dependencies:** WP0(d); WP3 (typed drift outcomes).
- **PRs:**
  1. Make HEAD and index drift classes (a `DriftRow::Authority` that carries the before-snapshot). Proptest by interleaving commit and `git add` at each `mid_pass::Stage`.
  2. Transfer drift outcomes `Vanished` and `ChangedDuringRead` (ENOENT/ESTALE on source open), never bare IO. Real EIO/EACCES stay typed refusals.
  3. Superseding publish guarded by identity comparison against the ledger row; add an `outputs_superseded` counter. Change the bench delta phase to run against the existing outputs (remove the `remove_delta_targets` preconditioning).
  4. Per-item `drift_seats`, `racy_seats` and `passes_since_clean`, plus a stated liveness assumption in the formal model.
- **Issues subsumed:** #38, #41 (ignore policy as custody), #131, #125.

### WP6. Measurement and proof harness for S1–S3
- **Findings merged:** slo 6 (fragmented S3 accounting, child reads); slo 13 (bench verdict lacks the S3 ratio and gate b); slo 2 (S2 budget sampler, governor); par 4 (estate-shaped corpus); slo 23 (proptests); arch 11 and slo 12 (inject the clock instead of `cfg(test)`); slo 10 (memoize refused seats, sniff counter); sprawl 18 (`--durability=strict` to a bench feature).
- **SLOs:** S1, S2, S3.
- **Payoff:** high. Until this lands, every SLO verdict is an example rather than a measurement. **Cost:** medium.
- **Risk:** a load governor makes A/B less deterministic; pin it off or at a fixed level for gated samples.
- **Dependencies:** WP0(c) and (e). WP2 PR 1 (pack counters).
- **PRs:**
  1. One counter set is the source of truth. Route raw_tree reads to `SourceFileRead`. Add `source_child_read_bytes` from rusage or `/proc/<pid>/io` (labelled as a lower bound on Darwin). Delete the never-incremented counters, or wire them in WP2. Rename the agent field to `transferred_content_bytes`. Add a debug-assert cross-check.
  2. `Pass{started_ns, clock}` threaded from the verb entry; delete the `cfg(test)` capture clock. Memoize refused seats by StatIdentity and add a separate `source_sniff_bytes` counter.
  3. Three core proptests over generated corpora that include nested git repos:
     - P-S2: the lstat census, including `.git/index`, is unchanged.
     - P-S3: idempotence, with zero source and wire bytes.
     - P-S3-delta: the two inequalities from WP0(c).
     Then retire the example tests these properties subsume.
  4. Bench: an `s3_ratio ≤ 0.10` verdict, the walk share, a `--remote` pull-vs-rclone-sftp arm, an estate-capture phase, an estate-shaped deterministic corpus generator, and an S2 sampler (a reference workload's p95 and load1 at 1 s intervals). Move strict durability behind a bench feature.
- **Issues subsumed:** #88, #128, #115, #109; gate rows of #46 and #47; measurement half of #34. This feeds #49.

### WP7. Source access in the types; one walker; split git_carry.rs
- **Findings merged:** arch 1 and slo 5 (SourceRepo/PrivateRepo/DestRepo); arch 4 (SourceFile choke point, O_NOATIME, one byte counter); arch 5 (SourceDir write syscalls); sprawl 5 (census becomes walk.rs consumer with re-stats, 4 walks to 1); sprawl 9 (12k-line file); sprawl 10 (wrapper families into options structs).
- **SLOs:** S2 (compile-time), S3 (≤10% metadata), S4 (typed depth and length refusals).
- **Payoff:** high. **Cost:** medium, about 150 call sites, compiler-driven.
- **Risk:**
  - Drift-classification error kinds change (ELOOP instead of an identity mismatch), so re-baseline.
  - Pre-key == post-key equality must be proven by a property test when 4 censuses become 1 plus re-stats.
- **Dependencies:** WP2 (delete v2 first, so dead code is not reorganised); WP1 PR 1 (GitEnv).
- **PRs:**
  1. `SourceRepo::git(ReadCmd)`, a closed allowlist, plus `PrivateRepo` and `DestRepo`. Add a PATH-shimmed git argv-recording test.
  2. `SourceDir` and `SourceFile` (`O_NOATIME` with a counted fallback). Route raw_tree, registered, hash, transfer and materialize-verify through them.
  3. The census consumes walk.rs rows (fd-relative, depth and length caps) and computes the key once plus seat re-stats. Target `census_walks==1`.
  4. Mechanical split along the roles: `source/{census,export}`, `private/`, `dest/{import,restore,repair}`. Collapse the wrapper families into `capture_key(&KeyOptions)`, `export(&ExportOptions)` and `restore(&StagedBundle,&RestoreTarget)`. No behaviour change.
- **Issues subsumed:** #130, #72, and the S2 slice of #49.

### WP8. One durability façade with proof tokens
- **Findings merged:** sprawl 7 (three flush APIs plus raw `sync_all`); arch 6 (`Sealed` tokens, `record_capture` bypass, estate's hand-sequenced order); sprawl 8 (one `private_dir`); arch 12 (process-global durability and space config); sprawl 16 (estate `hash_file` duplicate).
- **SLOs:** S4 (the R-N88 checker covers the estate path), S1 (group mode applies to estate).
- **Payoff:** medium-high. **Cost:** medium.
- **Risk:**
  - Moving estate from a full flush to a barrier on Darwin changes semantics; extend crash_check first.
  - The stricter `private_dir` may refuse directories created by the older, laxer path; needs a migration note.
- **Dependencies:** WP7 PR 4 (the split) makes this easier but is not required.
- **PRs:**
  1. `io::private_dir` (fd-relative, `O_NOFOLLOW`, mode, uid, no ACL), used by all state roots.
  2. `seal_file → Sealed<FileId>`, `SealedDir`, `durable::publish_file`. `OutputRecord::new` requires tokens. Move `record_capture` under `cfg(test)`.
  3. Port estate, registered, shared and git_carry to the façade. Delete `CountedSync` and the raw `sync_all` sites. Move `Config` (durability, space floor) into Committer/Store.
- **Issues subsumed:** the crash-proof slice of #49; #66 (stage-parent trust, disk-full fallback).

### WP9. Destination throughput for estate-shaped trees (S1). Measure first
- **Findings merged:**
  - par 1: directory create is a serial chain of flush plus SQLite commit
  - par 3: QD1 seals; unused kick
  - par 8: control-frame flush per frame; BufWriter coalescing
  - par 7: O(depth) opens (openat2, parent-fd LRU)
  - par 11: rerun sweeps and seals every directory; `prepare_cached`
  - par 2: serial receive stage split into reader plus K workers
- **SLOs:** S1 (estate corpus, gate b), S3 (rerun ratio).
- **Payoff:** high on estate-shaped corpora and near zero on R23. **Cost:** medium per PR.
- **Risk:** every batching change must re-run the crash ordering proofs (I1/I2, R-N102, R-N119, R-N103).
- **Dependencies:** WP6 PR 4 (the estate corpus must show the bottleneck first); WP8 (tokens make batching safe).
- **PRs, in order of expected payoff per risk:**
  1. BufWriter plus flush-when-idle on both ends; BufReader on pull's ChildStdout. No wire change.
  2. Rerun: a `rename_sealed` directory mark, scoped sweep, `prepare_cached`.
  3. Directory creation batched into the group committer.
  4. Write-back kick at End, concurrent seals within a group, pipelined groups, byte- and idle-sized groups.
  5. `openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS)` / `O_NOFOLLOW_ANY` or a parent-fd LRU.
  6. *(Only if still bound after 1–5)* split the receive stage into a reader plus K verify-and-write workers.
- **Issues subsumed:** #47, #111, and the throughput part of #46.

### WP10. Sprawl removals not covered above
- **Findings merged:** sprawl 15 and arch 13 (handoff out of the agent); sprawl 11 (dead io code); par 10 (unwired chunker and slab pool); sprawl 17 (M0 scripts, micro); sprawl 13 (JSON escaper duplicate); sprawl 14b (provider verbs with no contract).
- **SLOs:** S2 (smaller source-side binary), iteration efficiency.
- **Payoff:** medium. **Cost:** low. **Risk:** low.
- **Dependencies:** none, except that the chunker decision waits on #88 and the gate (a) evidence.
- **PRs:**
  1. Move `handoff.rs` into its own binary (bulkload-handoff or bench). Share one child-drain helper with estimate. The agent no longer spawns claude or codex.
  2. Delete the 35 W4 allowances and everything that then fails to compile in non-test builds: `TempFile`, `open_tmpfile`, `link_tmpfile`, `socket_buffers`, and `preallocate`/`read_advise`/`kick` unless WP9 PR 4 wires them. Retarget crash_check tests at `publish_noreplace`.
  3. Chunker and slab pool (`io/chunker.rs` plus `io/buf.rs`, 559 lines): wire `chunk_segmented` for files ≥32 MiB only if gate (a) needs the tail latency. Otherwise delete it. Either way, do not keep it under `allow(dead_code)`.
  4. Archive `m0_gate_a.py` and its test, and the `micro` durable items, after tagging. Use one JSON escaper.
  5. Ruling on compose-state, hydrate-state and apply-state-candidate: give them a design.md contract and typed refusals (WP3), or delete them.
- **Issues subsumed:** #40 (relocated, then fixed in its new home).

### Deferred, not ranked: WP11 wire compression and session dedup (par 9)
Gate (b) is link-bound at about 28 MB/s, so wire bytes do translate into wall-clock time. However, this needs a v6 hard cut and competes with the S2 CPU budget. Revisit after WP1 (priority) and WP6 (remote arm) give real numbers. If the WP2 or WP3 wire bump lands, consider adding the DataHeader compression flag in the same cut, but only as a reserved bit.

**Sequencing at a glance:** WP0 → (WP1 ∥ WP2-PR1 ∥ WP3-PR1/2 ∥ WP10-PR1/2) → WP2 → WP3-PR3/4 → WP6 → WP5 → WP4 → WP7 → WP8 → WP9.
The first migration rehearsal "as just another run" is credible after WP1–WP6 plus WP4 PR 3.

## 3. Sprawl budget (estimated lines deleted from origin/main)

| Removal | Lines | Condition |
|---|---:|---|
| carry_v2 production (7 files) | 4,399 | Ruling WP0(a) |
| tests/git_carry_v2.rs | 4,167 | WP0(a) |
| tests/fault_harness/git_ingest.rs, plus v2 fault points | ~450 | WP0(a) |
| Reserved W6 frames and TAG_PACK_DATA in bulkload-proto | ~150 | WP0(a), wire bump |
| tests/git_m1_spike.rs, plus the m1-spike feature | 2,612 | Unconditional (evidence doc stays) |
| handoff.rs leaves the agent crate (moved, not deleted) | 1,628 | Unconditional |
| m0_gate_a.py plus test_m0_gate_a.py | 908 | After tagging |
| micro.rs durable and durable-corpus items | ~400 | After evidence is recorded |
| io dead code (TempFile, tmpfile, link, socket_buffers, sys stubs across 3 platforms, Qos) | ~450 | Unconditional, except helpers WP9 wires |
| io/chunker.rs plus io/buf.rs, with their tests | ~560 plus tests | Only if gate (a) does not need them |
| 9 dead refusal variants and code() arms | ~60 | Unconditional |
| closure.rs Json/JsonParser (replaced or removed) | ~300 | WP3/WP4 |
| Side-door verbs: attach/registered folded in, debug verbs gated, main.rs USAGE and dispatch | ~600 | WP4 |
| Attestation ledger shrink | ~150 | WP4 |
| Wrapper families in git_carry | ~150 | WP7 |
| Duplicate hash_file, json_escape, CountedSync, private_dir copies | ~200 | WP8/WP10 |
| `--durability=strict` product paths, moved to a bench feature | ~80 | WP6 |
| Example tests subsumed by P-S2/P-S3 proptests | ~500–1,500 | WP6, measured when done |

- **Unconditional or near-unconditional:** about 6.1k lines (spike, handoff move, io dead code, refusal codes, M0 scripts).
- **With the v2 ruling:** a further 9.2k lines.
- **Total:** about 15–17k of 64.8k Rust lines (≈23–26%), plus about 1.3k Python. That excludes the conditional chunker/buf removal and the test retirements.
- **Issues closed as obsolete with WP0(a):** about 11 open issues.

## 4. What NOT to do (rejected proposals and why)
1. **Do not wire carry_v2 as the estate transport now** (arch 3, sprawl option B).
   - It adds a new frame family, destination crash surface and corpus format while v1 is the evidenced path.
   - The S3 motive is met by the auto-prerequisite (WP2 PR 2).
   - Re-open only if measured bundle cost still breaks S3.
2. **Do not keep both Git engines "for later".** Two hardening stacks have already drifted (partial-clone exposure). Two engines also mean two resume stories for the formal model to prove.
3. **Do not parallelise the walk or adopt `getattrlistbulk` yet** (par 12).
   - The receive side is the bottleneck.
   - `getattrlistbulk`'s attribute set risks silently missing every reuse key.
   - Parallel walk threatens the deterministic order and depth-cap properties (#110).
4. **Do not split the receive thread first** (par 2). Cheaper wins come first (WP9 PRs 1–5), and the split re-places fault points and ordering checks. Do it only when the estate corpus proves the need.
5. **Do not ship wire compression or session dedup in this cycle** (par 9). It is a hard cut competing for the S2 CPU budget, and there is no measured remote arm yet (WP6 first).
6. **Do not make background priority a silent default that gated S1 samples inherit unrecorded.** It must be recorded per run, with ruling WP0(f). Otherwise S1 and S2 are measured in different modes.
7. **Do not relax the source ledger, adopt superseding publish or carve out SQLite locks without a dated ruling.** All three change proof obligations (R25/R-N58, R-N88/R-N119, the S2 wording). Code-first would be an unratified act under R-N13.
8. **Do not replace the SQLite backup API with snapshot plus `immutable=1`** (slo 3, alternative). AGENTS.md mandates the backup API, and a snapshot changes consistency semantics. Use the typed carve-out instead.
   - *Note 2026-10-06 (#157, unratified).* Branch `feat/s2-shm-counter-20261006` keeps the backup API but opens a WAL-mode source that has no `-wal` with `immutable=1`, which takes no lock. That changes consistency semantics in the way this item warns of, and item 7 requires a dated ruling for it. None exists; `docs/design.md` (source safety) marks it as pending an operator decision.
9. **Do not reorganise git_carry.rs before the deletions** (sprawl 9 sequencing). Split after WP2, so dead paths are not moved.
10. **Do not build `sync` as a second engine** (sprawl 4 risk). It does sequencing and the ledger only; item logic stays in the existing functions until WP3 and WP7 make it typed.
11. **Do not add a load governor to gated A/B runs** (slo 2). Keep it fixed or off in S1 samples, and use it only in product runs, with `source_governor_wait_ns` counted.
12. **Do not land PR #136 (#120 quarantine token) before WP0(a).** If v2 is retired, that work is moot; pause it rather than merge-then-delete.
