# bulkload product spec

Status: **Ratified 2026-10-16 (OI-1003-Q194).** This file is the first thing
every lane reads. It outranks `docs/slo.md`, Linear documents and every lane
plan on *what* bulkload must achieve. `slo.md` stays the charter for *how well* and *how it is proven*.
Change this file only by a new dated ruling.

Every requirement below cites the operator's words or selections in
[intent-ledger.md](intent-ledger.md) by entry ID (for example `RES-12`). A
requirement with no ledger citation is not a requirement. Where a line rests
only on an agent's design, it is marked **design, not an operator
requirement**. Q- and R-numbers name rulings recorded in `slo.md` or on
Linear (TIN-3692, TIN-4543); the ledger gives the operator's words or the
selection behind each. Where the operator's statements pulled against each
other, the conflicts
([round-2 list](intent-ledger.md#conflicts-for-the-operator-round-2-2026-10-16),
C1–C20) were put to the operator, who ruled on all twenty on 2026-10-16
(OI-1003-Q194; ledger RUL-01 to RUL-20). The rulings are listed under
"Rulings" below and folded into the requirements they change. An agent does
not resolve a new conflict; it is listed for the operator and asked once, as
a dated question.

## What bulkload is for

The operator, 2026-10-16, after the agent said `claude --resume` on sting
would not find the neo conversations (RES-12):

> whiy not?  that is fundamentally the only core user expectation. [...]
> instenrt there a nclean spec?  (PUR-12)

> that and the objective to prove our system can be faster than rclone for
> complex networks and extensive multifile system, staging work data pre TCFS
> launch.  thats really it.  (PUR-13)

bulkload has exactly two objectives:

1. **Carry on at the destination (R0).** Every work lane on SOURCE continues
   on DEST: conversations resume, transcripts are carried intact, and repos,
   worktrees and uncommitted work are intact. Nothing is dropped except truly
   recreatable content. (RES-12, PUR-12, RES-02, RES-05, RES-13, RES-16,
   RES-17, KEEP-14)
2. **Prove bulkload is faster than rclone (R3)** on complex networks and on
   extensive many-file estates, staging work data before tcfs launches.
   (PUR-13, PUR-10, PUR-08, ONCE-01, FAST-09, FAST-18)

The operator's 2026-10-03 priority answer (SEAT-10, OI-1003-Q4) states the
same two objectives from the product side: provable, definable SLOs and
proofs first, and "migration in full" in the same bar, with the full run
"just another execuion of somthing we trust" (SEAT-11). The full neo→sting
run does not wait for the proofs (C6).

tcfs is the long-run product. It will make this class of work unnecessary,
and bulkload is the interim step until then (PUR-01, PUR-02, PUR-03, PUR-05,
PUR-06, PUR-09, PUR-11, PUR-15, PUR-16, PUR-20, RES-01). bulkload is done
when the full run meets R0 and R3 is proven; after that it is
maintenance-only once tcfs launches (C20; PUR-08, PUR-10). bulkload is a
separate repository from tcfs and tummycrypt (PUR-19). Work that serves
neither objective is out of scope (PROC-05, PROC-07, PROC-12, PROC-14). Moves
go through bulkload, not ad hoc copiers (PROC-17, PROC-23, PROC-27, SEAT-20).

**How the objectives are met is not optional.** R1 (nothing dropped) and R2
(both hosts stay live; nothing is read twice) are not lower priorities to be
traded for speed or convenience. The operator called never halting the
source "the crux bulkload seeks to solf" (LIVE-10), "the key feature"
(ONCE-01) and "a golden requiremnt" (LIVE-22). A run that meets R0 by
halting SOURCE, clobbering DEST or dropping content fails.

## Why this file exists

The operator stated the user outcome from July on (RES-02, RES-03, RES-05,
SEAT-01, RES-08, SEAT-06, RES-14). Lanes and audits measured proxies
instead: copies, listings, bytes, counts and closure. Where resume was
tested, the test was weaker than carrying on: one representative session in
July, a native thread read on 09-30 that the operator's own `codex resume`
then failed (RES-18), and on 10-01 a picker-load test with no turn sent
(intent-ledger L1). The verbatim record was deleted from `main`, beyond what
the R-N55 selection covered (L4). A single-run reading was attached to
OI-1002-Q24 (L10). Standing rulings were re-asked (L3). This spec makes the
outcome requirement 0, ties every requirement to the operator's words, and
lists the standing rulings once, with the 2026-10-16 rulings on the twenty
conflicts.

As of 2026-10-16 the migration is not complete. The operator said so
repeatedly (SEAT-15 to SEAT-18, SEAT-24, SEAT-28, SEAT-29, PROC-20,
PROC-21), and the failed runs cost a week of client work (FOOT-08, FOOT-09).

## R0: carry on at the destination

After a run from SOURCE to DEST, **every work lane on SOURCE continues on
DEST.** A lane is any repo or worktree under SOURCE's `~/git`, with its agent
sessions if it has any. R0 also covers dots, configs and loose home files,
whether or not a session is attached (SEAT-05, SEAT-06, RES-06, KEEP-01,
KEEP-10, KEEP-26). R0 covers the session driving the migration too: it is
carried and resumable (C19; RES-12, KEEP-13). KEEP-38 and RES-16 mean only
that it need not move first. For each lane:

1. **Conversations resume.** (RES-12, RES-08, RES-09, RES-10, RES-11,
   SEAT-02, PUR-07)
   - In the lane's DEST working directory, claude (`claude --resume`,
     `claude --continue`) and codex (`codex resume`) resume the lane's
     sessions (RES-09, RES-10, RES-11). pi and Junie state is carried too
     (SEAT-02, SEAT-20, "juni"); resuming pi and Junie sessions with their own
     resume commands is **design, not an operator requirement**.
   - "Resumes" means the newest session loads **and the conversation can be
     carried on** (RES-12: "carry on converations"; RES-08: "enter the most
     recent [...] session [...] and continue work"). Appearing in a picker,
     or loading without a continued turn, is not enough (RES-18, SEAT-25,
     L1). The operator's real resume command, run by the agent itself, is the
     acceptance test for any repair of agent state (RES-07, a principle from
     the 09-27 neo database recovery, not a migration ruling).
   - Transcripts are carried byte for byte under their original session IDs
     (C8; RES-12: "byte identical transcripts"). bulkload makes no per-repo
     forks with rewritten `sessionId` or `cwd` (RES-20 is not adopted). The
     forks the 10-01 repair already made are agent-session data and are kept
     (KEEP-31, KEEP-32).
   - A compressed or symlinked session store does not count as identical,
     and bulkload never recompresses or symlinks an agent session file in
     place (C9).
   - A lane's conversations may appear in that repo's own resume picker even
     when the work happened in a broader umbrella conversation (RES-19), but
     only by means that leave the original transcript and session ID
     unchanged (C8). How is **design**.
   - Subagent, workflow, plan and todo files that the session refers to are
     present beside it (KEEP-10, KEEP-11).
2. **The work lane is intact at its DEST path.** (RES-13, RES-14, RES-17,
   SEAT-06, RES-04, PUR-01) For each repo and worktree:
   - HEAD, branch, every ref, stashes with their messages, and the index;
   - dirty, untracked and ignored files, byte for byte (KEEP-10, KEEP-27);
   - linked worktrees still linked, under their names (RES-14).

   SOURCE's state is fully present and restorable on DEST, "reporting the
   correct state as on neo" (RES-17). Where DEST already holds newer or
   different work, R2.2 applies. A run replaces DEST's checkout at the
   canonical path only when DEST's copy is preserved intact beside it and
   the swap is recorded (C11; CONF-11 bounded by LIVE-15, LIVE-18, LIVE-30
   to LIVE-33).
3. **The seat works.** Dots, configs, credentials and agent settings are
   carried verbatim (KEEP-02, KEEP-06, KEEP-07, KEEP-12, KEEP-21, KEEP-23,
   KEEP-25, PUR-07). Rekeying is not a requirement (KEEP-07). The Claude
   account login is carried. Re-auth on DEST is expected only where a token
   is bound to its host; any other login that fails on DEST is a defect (C7;
   KEEP-07, KEEP-12; CONF-04, CONF-17 and RES-15 apply only to a host-bound
   token).
   - DEST machine identity (host keys, Home Manager-generated files) is
     preserved, not overwritten. **Design, not an operator requirement**
     (repo rule, AGENTS.md "Estate rules"). Preserving it never leaves
     SOURCE's files out: SOURCE's versions are carried beside DEST's
     (KEEP-20, KEEP-25, L6).
   - Platform-built caches such as `.direnv` are rebuilt on DEST before the
     lane counts as working; the 09-30 hand-offs show they do not work on
     arrival (L1). **Design, not an operator requirement.**
4. **Paths are translated.** Agent state that names SOURCE paths is made to
   name the DEST paths: session indexes, Codex state rows, Claude project
   keys, and skill and other symlinks. The operator's bar is being "back at
   it wht the agent and file histry and paths just like that" (RES-05); which
   records are rewritten, and ledgering each rewrite, is **design, not an
   operator requirement**, read from that one phrase. Transcript bytes are
   never rewritten, and bulkload never removes an existing transcript file
   (C8, C9).
5. **The agent is told where it is.** bulkload writes a dated hand-off note at
   the top of the lane's `AGENTS.md` and `CLAUDE.md` (HAND-01). HAND-01 adds
   "maybe even readme"; whether bulkload also writes it into a README is
   **design, not an operator requirement**. The note gives:
   - absolute SOURCE→DEST paths;
   - high-level differences from any copy already on DEST;
   - the commands to finish converging.

   The note tells the agent to converge and consolidate completely before any
   other work. bulkload adds no privacy gate to the note (HAND-02). "Ready"
   needs commits plus clean trees (HAND-03); a carried note is cleared only
   on request (HAND-04); only new commits are flagged (HAND-05).
6. **The operator's seat works on DEST.** From neo, the teletype, the
   operator can ssh to DEST and attach to tmux and cmux, use emacs and eGreg
   (GUI and TUI), and have MCP servers and Linear reachable (SEAT-01,
   SEAT-02, SEAT-03, SEAT-04, SEAT-13, SEAT-14, LIVE-11). bulkload's own work
   on DEST does not starve that seat of memory, disk, ssh or IO (FOOT-14).
   - In a migrated lane on DEST the operator can make a commit and push it.
     **Design**, read from RES-03 ("authenticated developemtn [...] from
     sting") and SEAT-01; SEAT-09 is an agent-worded plan the operator
     approved and is not the basis on its own.

**R0 is the acceptance test.** A run is done only when the R0 check passes
for every lane (see "Acceptance"). Byte and closure counts are necessary but
not sufficient (PROC-02).

## R1: nothing is dropped (standing; never re-ask)

- **Everything is carried, regardless of content** (KEEP-12, KEEP-13,
  KEEP-09, KEEP-19, KEEP-21, KEEP-22). That includes secrets, keys, tokens,
  cookies, OAuth caches, auth files and SQLite stores (KEEP-02, KEEP-06,
  KEEP-07, KEEP-23, KEEP-25). It also includes all history, transcripts,
  plans, notes and dots (KEEP-01, KEEP-04, KEEP-05, KEEP-10, KEEP-11,
  KEEP-14, KEEP-20, KEEP-26, KEEP-27), uncommitted and ignored files
  (KEEP-10), named families such as [repo] and the platform/payment portal
  work (KEEP-24), and anything that exists nowhere else (KEEP-08). KEEP-03 is
  about neo's own storage and is not cited here.
- **Agent-session data is never aged, deleted or lossily compressed** by
  bulkload, on either host (KEEP-31, KEEP-32, KEEP-33). Lab's lossless
  cold-tier moves behind symlinks are not ageing (KEEP-34), but bulkload
  never recompresses or symlinks an agent session file in place, and a
  compressed or symlinked store does not count as identical (C9). Claude
  Code's 30-day transcript cleanup stays off on SOURCE (C16; neo's
  `cleanupPeriodDays` was 3650 on 2026-10-16).
- **Only truly recreatable content may be left out** (KEEP-14: "truely
  recreateable conteent can be dropped"). The Q192 record lists it, in the
  coordinator's wording (KEEP-15); "Application Support caches and binaries"
  and `vm_bundles` are the coordinator's examples, not the operator's words:
  - build outputs (`target/`, `cargo-target-*`, `dist`/`build` artefacts);
  - dependency directories (`node_modules`, `.venv`, `.uv-cache` and
    similar);
  - caches (`~/.cache`, Application Support caches and binaries,
    `vm_bundles`).

  The same record names what is never dropped: dots, history, transcripts,
  plans, notes, rescue and decommission directories, loose home files, and
  agent state.
- **Drops the operator named**, and nothing else:
  - Darwin `~/Library` databases of **built-in** apps, a conditional drop
    (KEEP-16). Third-party app state under `~/Library`, such as Chrome
    (Login Data included) and the Linear app, is carried (C12); the 09-30
    widening (KEEP-29, L6) is superseded.
  - an unreadable, generated `__pycache__` (KEEP-30);
  - bulkload's own bench litter and build outputs (KEEP-35).
- **Ruled carried on 2026-10-16:**
  - the neo-only Mac cohort, now; it is still tcfs's first user later (C1;
    CONF-02 now governs only that later role);
  - content on GDrive or NFS mounts, once reachable (C13; KEEP-28 was a
    time-boxed 09-30 scope);
  - the trees Q179 left out (old run state, `neo-relocated/state`, the tcfs
    pilot, Desktop, Documents, Downloads and Pictures), except content in
    them that is truly recreatable, such as cargo targets (C14, Q192);
  - the 35 corrupt SQLite rescue copies and the two corrupt `logs_2` copies,
    carried as files and marked corrupt in closure (C14; replaces CONF-19,
    CONF-20);
  - the neo Codex recovery trees, carried to DEST and verified by hash.
    R-C122's 2026-11-02 deletion of them is cancelled until then; after
    that the decision goes back to the operator (C15; CONF-13, CONF-15).
- **What bulkload cannot capture natively is carried as files**, exactly as
  it is, and recorded `carried-as-files`. **Design, not an operator
  requirement**: a coordinator decision under Q185 (Linear TIN-4543 comment
  68248cf1), following KEEP-12 and KEEP-13. For the corrupt copies in C14
  the operator selected the same form ("Carry all as files (Recommended)",
  RUL-14).
- A tree is left out of a run only if it is truly recreatable (KEEP-14) or
  the operator has ruled it out, for example the families already moved and
  worked on DEST in the Q142 fresh-root run (KEEP-37).
- Secret *values* are never printed in logs, receipts, Linear or agent output
  (repo rule, AGENTS.md "Estate rules"). This governs output, not what is
  carried.

## R2: live hosts, piecemeal, never twice (standing; never re-ask)

1. **SOURCE never stops.** A run never halts, freezes, quiesces, locks or
   signals agent, git, tmux or session work on SOURCE, and never schedules
   a quiet window (LIVE-01, LIVE-02, LIVE-04, LIVE-06, LIVE-07, LIVE-08,
   LIVE-10, LIVE-12, LIVE-13, LIVE-16, LIVE-17, LIVE-21, LIVE-22, LIVE-23,
   LIVE-26, LIVE-36, RES-17, ONCE-01). The earlier quiet-window selections
   (CONF-01, CONF-06, CONF-07, CONF-08) are superseded by never-halt (C18).
   - No locks, no signals, bounded impact: at most +25 % p95 latency and
     +2.0 load1 on the source (LIVE-24, LIVE-25; `slo.md` S2).
   - SQLite stores that are being written live, including agent state and
     shell history, are captured consistently while writers keep writing
     (LIVE-08, LIVE-17, KEEP-02).
   - A ref, HEAD, index or stash that moves mid-pass is carried as drift, not
     refused (LIVE-16, LIVE-27, LIVE-28).
   - Source safety outranks a speed win: no reflink of source inodes
     (LIVE-29).
   - bulkload may ask the operator whether to end their own work on SOURCE
     for a family that has moved (CONF-09, CONF-10), but never requires it,
     and no run waits on the answer (C17; LIVE-02, LIVE-16, LIVE-26,
     LIVE-36). Operator-scheduled host maintenance is the operator's, not
     bulkload's (LIVE-38).
2. **DEST is live too, and is never clobbered.** A run never halts or kills
   DEST sessions (LIVE-14, LIVE-22, LIVE-34, LIVE-37). LIVE-19 and LIVE-20
   protect live work on DEST during host maintenance; they are not bulkload
   statements. A run never overwrites, resets or deletes DEST work that
   SOURCE does not hold (LIVE-15, LIVE-18, KEEP-08):
   - a rerun replaces a DEST file only when it is bulkload's own output,
     unchanged since it wrote it (LIVE-30);
   - a DEST seat the run never captured is left as it is and named in the
     receipt (LIVE-31);
   - a diverged DEST copy is set aside intact, with its unpushed branches
     imported create-only (LIVE-32), or diffed and preserved before it moves
     forward (LIVE-33).

   Where both sides changed, the run keeps both: DEST's refs, dirt, sessions
   and rows stay, SOURCE's land beside them, and the hand-off note names the
   difference. Every run is a lossless union into DEST, possibly from more
   than one SOURCE host: never a mirror, and never clobbering DEST (C10;
   LIVE-15, LIVE-18, CONF-05, KEEP-36). A file is never deleted on DEST
   because it disappeared on SOURCE. A checkout is replaced at its canonical
   path only as R0.2 says (C11).
3. **Piecemeal.** Any run can stop at any point and resume later. A live lane
   is caught up by repeated passes as its agents and projects finish, with no
   appointment needed (ONCE-01, LIVE-04, LIVE-07, LIVE-09, LIVE-12, ONCE-07,
   ONCE-08, SEAT-11, PUR-06, PUR-18).
4. **Never twice, as selected.** A rerun reads **0 content bytes** for
   unchanged work. Its cost is a metadata (stat-only) walk, as the operator
   selected (ONCE-11, OI-1003-Q6), and on an unchanged estate it finishes in
   at most 10 % of the first pass's wall-clock (ONCE-12). Already-copied
   bytes are reused, never streamed again (ONCE-02, ONCE-09). Catching up a
   mostly moved estate takes minutes, not hours (ONCE-04, ONCE-05, ONCE-06).
   The 10-03 selection stands as the reading of ONCE-01's "without EVER
   rewalking or reading a bit twice": 0 content bytes re-read, a
   metadata-only re-walk allowed, a rerun at most 10 % (C5; ONCE-11 to
   ONCE-15). This answers the question tracked as bulkload#169.
5. **bulkload's own scratch** on SOURCE is bounded. After a batch is pulled,
   verified and applied on DEST, its payload (corpus and bulky private-state
   blobs) is removed by literal path, with a receipt (ONCE-16, ONCE-17). The
   small per-item reuse records that let a later pass skip unchanged items
   are kept on SOURCE, so converge passes stay incremental (C4; ONCE-01,
   ONCE-02, ONCE-11). This replaces Q193's whole private-state drop.

## R3: faster than rclone, where it matters

The claim to prove (PUR-13): bulkload "can be faster than rclone for complex
networks and extensive multifile system". It is about staging an agent-heavy
development estate before tcfs, which is what makes it publishable (PUR-10,
PUR-07). bulkload exists because plain rclone or rsync could not move a live
estate without holding it hostage; if it only dressed up rsync, it would have
no reason to exist (LIVE-07, FAST-01, FAST-02, PROC-23). The comparison
covers the agent and operator experience as well as wall-clock time:
"supercedes AX wise base rclone" (PUR-07). Performance is the bar (FAST-18).

- **The headline is the cross-host gate (b) on an estate-shaped many-file
  corpus** (C2; PUR-13, ONCE-04, FAST-22). Gate (a), the local copy on the
  rig, is a regression guard. Both gates (FAST-07) are measured against
  rclone as shipped, cloning allowed (FAST-19, R-N57). S1 stays a gate: the
  operator rejected making it a benchmark (FAST-20, Q150). There is no hard
  wall-clock SLA (FAST-16).
- **Q97 is reopened** (C2). Under it the hermetic rig and the sealed corpus
  v1 decided both gates, and a neo→sting run was only a field confirmation
  (FAST-21, FAST-12, from FAST-10). The ruling does not say which hosts
  carry gate (b)'s gated samples; that is a new dated question if it
  matters.
- **Corpora must look like the estate** (PUR-13, ONCE-04): many repos,
  worktrees, small files, transcripts and SQLite stores. Corpus v1 is still
  measured beside the estate-shaped corpus (FAST-22). A claim is stated only
  for the corpus it was measured on.
- **The 1 % delta is a real in-place change to about 1 % of files** (C3). It
  replaces the corpus-bytes reading (FAST-14) and the agent's XOR draft, and
  answers the question in Linear TIN-4543 comment eacc6552: the delta row
  times a real in-place change, not a file deleted at the destination first.
- **Speed comes from the engine:** parallelism, a smarter wire, native walks
  and unsafe Rust where it wins (FAST-03, FAST-05, FAST-08, FAST-11, FAST-17,
  FAST-23), on the R2 contract (LIVE-07, LIVE-29).
- **Samples run on live hosts.** The operator rejected quiet windows for
  benchmarks (LIVE-40, LIVE-41, LIVE-42); a sample taken under load is
  recorded as such.
- **Claims are earned only by gated samples** (`slo.md` S1, OI-1002-Q30,
  Q143). Informational runs are never cited as wins (FAST-04, FAST-06).

## R4: both hosts stay usable

- A run never fills either host's development disk, and never stalls the
  SOURCE storage that holds `/nix` (FOOT-02, FOOT-04, FOOT-06, FOOT-12,
  FOOT-15, FOOT-19, FOOT-21).
- DEST is the development seat. Disk and memory pressure that blocks
  development there is an emergency, whoever causes it (SEAT-26, SEAT-27,
  FOOT-16, FOOT-17, FOOT-20). Recording bulkload's share per run is
  **design, not an operator requirement**.
- neo is the teletype. Builds, tests and heavy work do not run there, and a
  run leaves no staging residue on it (FOOT-01, FOOT-03, FOOT-10, LIVE-11).
- On DEST each tree lands once, at its live path (FOOT-18, PUR-20). There
  are no duplicate landing archives, stale run directories or rollback copies
  left behind without an operator ruling (FOOT-05, FOOT-06, FOOT-07, FOOT-11,
  FOOT-20). Residue that may be the only copy is never deleted to make room
  (KEEP-08, FOOT-13).
- Each run records its residual bytes on both hosts. **Design, not an
  operator requirement.**

## Lane mapping

A run plan maps each lane's SOURCE cwd to a DEST cwd. R0 is checked at the
DEST cwd (RES-08, RES-12).

- **Default:** the same path relative to the home `git` directory. For
  example, SOURCE `~/git/lab` → DEST `~/git/lab`. **Design, not an operator
  requirement.**
- **When DEST already holds a copy of the lane,** the run adopts or converges
  it in place (`estate-apply --adopt`, `estate-diff`), under R2.2. Landing it
  at the live path is bulkload's own job and needs no hand-written script
  (PUR-20, PUR-11, RES-06).
- Sessions are re-seated under DEST's project key for that cwd, in DEST's
  live agent-state directory, not only in a landing archive (RES-10, RES-12,
  RES-19, L1). Each session keeps its original ID and bytes, and no new
  fork is made (C8). Within C8 and C10 (**design**, not an operator
  requirement):
  - every placement is ledgered, so later passes keep it current;
  - an existing DEST transcript is extended only when it is a verified
    byte prefix of SOURCE's;
  - a DEST transcript that has diverged is kept, and SOURCE's copy lands
    beside it;
  - the 10-01 forks and the originals are both kept (KEEP-31, KEEP-32).

## Acceptance (what "done" means for a run)

1. **The closure report shows 0 unaccounted** and every refusal is typed and
   dispositioned (SEAT-12, `slo.md` S4). `carried-as-files` counts as carried
   (R1; a coordinator decision under Q185, and RUL-14 for the corrupt
   copies, which closure also marks corrupt).
2. **An R0 check runs on DEST for every lane.** It records a verdict per lane
   and is the concrete form of the "#40 daily-work verdict" in the S4
   selection (SEAT-12). The check covers:
   - each harness's newest session for the lane loads in the DEST cwd and
     accepts a continued turn (RES-08, RES-12; a load-only test is not
     enough, L1). **Design**: how the turn is sent without spending a model
     call is the check's to define;
   - transcripts match SOURCE's last pass byte for byte under their
     original session IDs; a compressed or symlinked copy does not count
     (C8, C9; RES-12);
   - the session driving the migration is checked like every other lane
     (C19);
   - SOURCE's git state is present and restorable, and DEST's own state is
     unchanged (R0.2, R2.2, RES-17);
   - no link or index points at a SOURCE-only path (R0.4);
   - the hand-off note is present and "ready" means commits plus clean trees
     (HAND-01, HAND-03).
3. **A seat check is run with the operator** in at least one migrated lane:
   tmux attach, emacs, a resumed claude session and a resumed codex session
   (SEAT-01, SEAT-03), then a commit and push from DEST (**design**, R0.6).
4. **An unchanged rerun reads 0 content bytes**, and finishes in at most 10 %
   of the first pass's wall-clock (ONCE-11, ONCE-12; `slo.md` S3).
5. **Residual bytes on both hosts are recorded** (R4; **design**).

No lane, audit or agent reports a run, lane or family as "done", "ready" or
"verified" without these receipts (PROC-02, L1).

**Product done** means both objectives are met (PUR-08, SEAT-10):

- (a) the full neo→sting run passes acceptance for every neo lane, and the
  operator works daily from sting (SEAT-01, SEAT-05, SEAT-06, SEAT-14);
- (b) R3 is proven: a gated pass of the headline gate (C2), and the product
  proofs are in place (SEAT-10).

(a) does not wait for (b) (C6). The operator asked for (a) as soon as
possible (SEAT-02, SEAT-05, SEAT-07, SEAT-08, SEAT-16, SEAT-21, SEAT-22,
SEAT-32) and also put the proofs first (SEAT-10, SEAT-30); the earlier
selections on order were SEAT-31 (Q142) and SEAT-33. bulkload is done when
the full run meets R0 and R3 is proven. After that it is maintenance-only
once tcfs launches (C20; PUR-01, PUR-02, PUR-06, PUR-08, PUR-09, PUR-15,
PUR-20; PUR-07 and PUR-10 for the publishable work).

## Standing operator rulings (do not re-ask)

No interview asks these again, and no interview offers an option that
contradicts them (PROC-13, PROC-22, KEEP-12, KEEP-14, L3). The 2026-10-16
rulings on C1–C20 (OI-1003-Q194) are standing rulings too. No conflict is
open; a new one may be asked once, as a dated question.

| Ruling | Rule | Ledger |
|---|---|---|
| R25 (#34) | Live host, piecemeal, never read twice, beat rclone (read through the 10-03 selection: C5) | ONCE-01, ONCE-10, RUL-05 |
| OI-1003-Q4 | Completion: provable product and the migration in full; the full run does not wait for the proofs (C6); bulkload is done when the full run meets R0 and R3 is proven, maintenance-only once tcfs launches (C20) | SEAT-10, RUL-06, RUL-20 |
| OI-1003-Q13 | A full migration is just another run of a trusted tool, rerun as lanes finish | SEAT-11, ONCE-08, PUR-06, PUR-18 |
| OI-1003-Q1 (S4) | 0 unaccounted; every refusal dispositioned | SEAT-12 |
| OI-1003-Q2, Q152 (S5) | No freeze window; deltas only; the old quiet-window selections are superseded (C18) | LIVE-21, LIVE-23, LIVE-26, RUL-18 |
| OI-1003-Q5, Q9 (S2) | No locks, no signals; ≤ +25 % p95, ≤ +2.0 load1 | LIVE-24, LIVE-25 |
| OI-1003-Q6, Q10 (S3) | 0 content bytes re-read; metadata-only re-walk; rerun ≤ 10 % | ONCE-11, ONCE-12 |
| — | Never ask a SOURCE lane to stop, park or exit, and never schedule a quiet window; bulkload may ask the operator to end work for a moved family, never require it (C17) | LIVE-02, LIVE-04, LIVE-05, LIVE-07, LIVE-16, LIVE-36, RUL-17 |
| — | Never kill or signal another agent's process, tmux or live session, on either host | LIVE-01, LIVE-02, LIVE-14, LIVE-22 |
| R-N15 (coordinator-recorded) | Never clobber DEST work from SOURCE; every run is a lossless union, never a mirror (C10) | LIVE-15, LIVE-18, RUL-10 |
| WP0(d) and later | A rerun supersedes only its own unchanged outputs; DEST-only seats are left and named; diverged copies are set aside intact; a checkout is replaced only with DEST's copy kept beside it and recorded (C11) | LIVE-30, LIVE-31, LIVE-32, LIVE-33, RUL-11 |
| #38, #39, 2026-10-15 | Mid-pass drift is carried, not refused | LIVE-27, LIVE-28 |
| 2026-10-15 | No reflink of source inodes | LIVE-29 |
| Q185 (supersedes Q180) | Nothing is ever excluded by content; third-party `~/Library` app state (C12) and GDrive/NFS content once reachable (C13) are carried | KEEP-12, KEEP-13, RUL-12, RUL-13 (Q180: CONF-17) |
| Q192 | Only truly recreatable content may be dropped; the non-recreatable Q179 trees and the corrupt copies are carried as files (C14) | KEEP-14, KEEP-15, RUL-14 |
| TIN-5268 hold | Agent-session data is never aged, deleted or lossily compressed; session files are never recompressed or symlinked in place (C9); R-C122's deletion of the neo Codex recovery trees is cancelled until they are carried and hash-verified on DEST (C15); the 30-day transcript cleanup stays off (C16) | KEEP-31, KEEP-32, KEEP-33, RUL-09, RUL-15, RUL-16 |
| 2026-09-12, Q123 | Rekeying is not a requirement; ssh keys are carried; the Claude account login is carried, re-auth only for a host-bound token (C7) | KEEP-06, KEEP-07, KEEP-25, RUL-07 |
| Q183, Q188–Q191 | Hand-off note: always written, no privacy guard; "ready" needs commits plus clean trees; carried notes are cleared only on request; only new commits are flagged | HAND-01, HAND-02, HAND-03, HAND-04, HAND-05 |
| Q186, Q193 as narrowed by C4 | Git captured in batches; after a verified apply a batch's payload is dropped and its per-item reuse records are kept (C4) | ONCE-16, ONCE-17, RUL-04 |
| R-N57 | Two gates, each against rclone as shipped, cloning allowed | FAST-07, FAST-19 |
| OI-1003-Q194 C2 (reopens Q97) | S1's headline is the cross-host gate (b) on an estate-shaped many-file corpus; the local rig gate (a) is a regression guard | RUL-02, PUR-13, FAST-22 (Q97: FAST-21, FAST-12) |
| OI-1003-Q194 C3 | The 1 % delta is a real in-place change to about 1 % of files | RUL-03 (replaces FAST-14) |
| Q3, Q150 | No hard SLA; S1 stays a gate, not a benchmark | FAST-16, FAST-20 |
| 2026-09-23 | Unsafe-first Rust; performance is the bar | FAST-17, FAST-18 |
| 2026-09-23 | bulkload is a separate repo from tcfs/tummycrypt (supersedes R34 and the 09-12 "Rust in tummycrypt" selection) | PUR-19 |
| — | Moves go through bulkload | PROC-17, PROC-23, PROC-27, PUR-04, SEAT-20 |
| OI-1003-Q194 C1 (narrows Q156) | The Mac-only cohort is carried now; it is still tcfs's first user later | RUL-01 (CONF-02) |
| OI-1003-Q194 C8 | Original session IDs byte for byte; no per-repo forks | RUL-08, RES-12 |
| OI-1003-Q194 C19 | R0 covers the session driving the migration | RUL-19 |

## Not superseded, and what is corrected

The first draft of this spec said it superseded the `slo.md` Completion bar
(OI-1003-Q4). It does not. Q4 is the operator's own answer (SEAT-10), and
`slo.md` lists "Migration in full" as part of the bar. What this spec does
correct:

- the `slo.md` phrase "not when a migration happens to succeed" is agent
  wording that is not in Q4. It must not be read as "the migration does not
  count";
- the agent reading of OI-1002-Q24 as a *single* run (L10). Q24's own words
  are "run this once [...] complete", meaning when;
- the Linear SSOT document's product-shape section, on what done means.

On order, C6 supersedes OI-1002-Q24 (as relaxed by Q142, SEAT-31) and R-N56
(CONF-21): the full run does not wait for the proofs.

## Rulings (OI-1003-Q194, 2026-10-16)

On 2026-10-16 the operator accepted every recommended reading of C1–C20 as a
standing ruling ("Accept all (Recommended)"), and answered three questions
in the same answer set: "Cancel until carried (Recommended)" (C15), "Keep
reuse records (Recommended)" (C4) and "Carry all as files (Recommended)"
(C14). The record is Linear TIN-4543 comment 14484c4a; ledger entries
RUL-01 to RUL-20 hold the selections, and the
[round-2 list](intent-ledger.md#conflicts-for-the-operator-round-2-2026-10-16)
keeps the evidence on each side. Each ruling is folded into the requirement
it changes, and none is asked again.

1. C1: The Mac-only cohort is carried now; it is still tcfs's first user later (narrows Q156 to placement). RUL-01; KEEP-12, KEEP-13, KEEP-14; narrows CONF-02.
2. C2: S1's headline is the cross-host gate (b) on an estate-shaped many-file corpus; the local rig gate (a) is a regression guard (reopens Q97). RUL-02; PUR-13, ONCE-04, FAST-22, FAST-07; reopens FAST-21, FAST-12.
3. C3: The "1 % delta" is a real in-place change to about 1 % of files. RUL-03; replaces FAST-14 and the `slo.md` XOR draft.
4. C4: Keep the per-item reuse records on SOURCE and drop only payload; this replaces Q193's whole private-state drop. RUL-04; ONCE-01, ONCE-02, ONCE-11; narrows ONCE-16, ONCE-17.
5. C5: The 10-03 selection stands: 0 content bytes re-read, a metadata-only re-walk allowed, a rerun at most 10 %. RUL-05; ONCE-11 to ONCE-15; reads ONCE-01, ONCE-10.
6. C6: The full run does not wait for the proofs. RUL-06; SEAT-02, SEAT-05, SEAT-21, SEAT-22, SEAT-32, RES-06, RES-16; supersedes CONF-21 and Q24 on order.
7. C7: The Claude account login is carried; re-auth only if a token is host-bound. RUL-07; KEEP-02, KEEP-06, KEEP-07, KEEP-12, KEEP-25; supersedes the R-N47 reading of CONF-04, and CONF-17.
8. C8: Original session IDs are kept byte for byte; no per-repo forks. RUL-08; RES-12; RES-20 not adopted.
9. C9: Compressed or symlinked session stores are not identical; session files are never recompressed or symlinked in place. RUL-09; RES-12, KEEP-31, KEEP-32; narrows CONF-12, KEEP-34, KEEP-35.
10. C10: Every run is a lossless union: never a mirror, never clobbering DEST. RUL-10; LIVE-15, LIVE-18, CONF-05, KEEP-36.
11. C11: A DEST checkout is replaced only with DEST's copy preserved intact beside it, and the swap recorded. RUL-11; CONF-11, LIVE-15, LIVE-18, LIVE-30 to LIVE-33.
12. C12: Third-party `~/Library` app state (Chrome, Linear) is carried. RUL-12; KEEP-12, KEEP-13; bounds KEEP-16, supersedes KEEP-29.
13. C13: GDrive/NFS content is carried once reachable. RUL-13; KEEP-12, KEEP-13; supersedes KEEP-28.
14. C14: The non-recreatable trees Q179 left out are carried; the 35 corrupt SQLite rescue copies and the two corrupt `logs_2` copies are carried as files marked corrupt; recreatable content stays out (Q192). RUL-14; KEEP-08, KEEP-13, KEEP-14, KEEP-15; supersedes CONF-18 (in part), CONF-19, CONF-20.
15. C15: R-C122's deletion of the neo Codex recovery trees is cancelled until they are carried and hash-verified on DEST; then it goes back to the operator. RUL-15; KEEP-08, KEEP-13, KEEP-31, KEEP-32; suspends CONF-13, CONF-15.
16. C16: The 30-day transcript cleanup stays off. RUL-16; KEEP-13, KEEP-14; supersedes CONF-16.
17. C17: bulkload may ask the operator to end work on SOURCE for a moved family, never require it. RUL-17; CONF-09, CONF-10, LIVE-02, LIVE-16, LIVE-36.
18. C18: The old quiet-window selections are superseded by never-halt. RUL-18; LIVE-07, LIVE-10, LIVE-16, LIVE-23, LIVE-26, LIVE-40 to LIVE-43; supersedes CONF-01, CONF-06, CONF-07, CONF-08.
19. C19: R0 covers the session driving the migration. RUL-19; RES-12, KEEP-13; KEEP-38, RES-16 read narrowly.
20. C20: bulkload is done when the full run meets R0 and R3 is proven; then maintenance-only once tcfs launches. RUL-20; PUR-01, PUR-02, PUR-06, PUR-08, PUR-09, PUR-15, PUR-20, SEAT-10, PUR-07, PUR-10.
