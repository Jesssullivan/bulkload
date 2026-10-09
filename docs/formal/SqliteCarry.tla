----------------------------- MODULE SqliteCarry -----------------------------
(***************************************************************************)
(* The SQLite snapshot seat of wire v6 (#218), as built on                  *)
(* feat/sqlite-carry-20261008: docs/agent-notes/2026-10-08-sqlite-carry-    *)
(* design.md, sections 0 and 16. Model first (OI-1003-Q102's precedent).   *)
(*                                                                         *)
(* One live store, one destination path, a bounded number of runs. The     *)
(* store's committed state is a logical version lv. Its main file and its  *)
(* -wal have stat identities that only grow (ctime cannot be set back): a  *)
(* WAL commit moves the -wal, a checkpoint moves the main file and not lv, *)
(* a WAL reset moves the -wal, and a rollback-mode commit moves the main   *)
(* file. A run walks the store in two separate steps, main then -wal (the  *)
(* code stats them apart), decides, and when it must read takes a stepped  *)
(* backup (OI-1003-Q16: the shared lock held inside one step), pre- and    *)
(* post-stats the store, publishes the snapshot (its capture record, then  *)
(* its row, a crash possible between), and keys its reuse row on the       *)
(* walked main identity and the settled -wal identity.                     *)
(*                                                                         *)
(* A changed store's snapshot supersedes the output this store landed for  *)
(* it (OI-1003-Q146, 2026-10-09) through WP0(d)'s exchange: only while that *)
(* output is this store's own and untouched (a third party may write it,   *)
(* DestTouch), and only when no sidecar sits beside it at Decide and at    *)
(* the last look immediately before the exchange (a third party may drop  *)
(* one at any time, SidecarAppear). The intent takes the old rows out, the *)
(* exchange trades the files in one step, the row commit follows; a crash  *)
(* at any step leaves the old snapshot or the new one at the path, whole,  *)
(* and the next session's sweep gives an old one still in place its rows   *)
(* back.                                                                   *)
(*                                                                         *)
(* NOT MODELLED (README.md, "SqliteCarry"):                                *)
(*   - SQLite's internals: that a backup whose steps all saw one version   *)
(*     copies that version is an axiom here (checked by P80), not proved.  *)
(*   - Clocks: the racy rule is folded into "settled" (a moved identity);  *)
(*     same-tick rewrites are the racy rule's, as for files (#86).         *)
(*   - The section 7.5 path race; chunking, credit and the wire; other     *)
(*     seats (BulkloadTransfer.tla holds the file seat's properties and    *)
(*     WP0(d)'s exchange for files, MC_wp0d_exchange).                     *)
(*   - The window between the last sidecar lstat and the exchange: the     *)
(*     look and the exchange are one step here (a stated residual).        *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS
    Wal,                 \* TRUE: WAL mode; FALSE: rollback (DELETE) mode
    SnapshotMode,        \* TRUE: --sqlite=snapshot; FALSE: refuse (v5)
    AsRoot,              \* the source half runs with euid 0 (OI-1003-Q76)
    OwnedByReader,       \* the database's owner is the reader (R4)
    BaseIsDb,            \* the base has the SQLite magic (R2: else a raw file)
    V5Record,            \* a refuse-mode run remembered the seat (tag 1, R8)
    MaxCommits,          \* writer commits, all runs together
    MaxCheckpoints,      \* checkpoints (and WAL resets) in WAL mode
    MaxRuns,             \* runs (sessions); also bounds backups (>= 2)
    MaxCrashes,          \* crashes, at any step
    Pages,               \* backup steps one complete copy takes (>= 2)
    StepBudget,          \* steps a backup may take, restarts included
    DestSidecars,        \* a third party may drop a sidecar at the destination
    DestTouches,         \* a third party may write the published output
    Mutation,            \* "none", or one deliberate rule break
    BudgetSeconds        \* wall-clock budget, checked by WithinBudget

Mutations == {"none",
              "sqlite_raw_send",                \* send the live file
              "sqlite_key_main_only",           \* key on the main file alone
              "sqlite_record_unsettled",        \* keep a row for an unsettled capture
              "sqlite_capture_record_main_only",\* R1: the record keyed on main alone
              "wal_lock_exclusive",             \* a WAL-mode backup blocks writers
              "sqlite_unbounded_pinned",        \* the lock held across steps
              "sqlite_as_root",                 \* no root refusal
              "sqlite_not_owner_opened",        \* no owner refusal (R4)
              "sqlite_publish_beside_sidecar",  \* no destination sidecar check at all
              "sqlite_supersede_no_recheck",    \* Q146: no sidecar look before the exchange
              "sqlite_supersede_unowned",       \* Q146: supersede without the ownership proof
              "sqlite_supersede_unlink_rename", \* Q146: remove the old, then rename the new
              "sidecar_covered_by_name",        \* R2: coverage by name alone
              "sqlite_header_refusal_in_snapshot_mode", \* honour tag 1 (R8)
              "sqlite_reuse_ignored"}           \* never Reuse (S3)

ASSUME /\ Wal \in BOOLEAN /\ SnapshotMode \in BOOLEAN /\ AsRoot \in BOOLEAN
       /\ OwnedByReader \in BOOLEAN /\ BaseIsDb \in BOOLEAN /\ V5Record \in BOOLEAN
       /\ DestSidecars \in BOOLEAN /\ DestTouches \in BOOLEAN
       /\ MaxCommits \in Nat /\ MaxCheckpoints \in Nat /\ MaxCrashes \in Nat
       /\ MaxRuns >= 2 /\ Pages >= 2 /\ StepBudget >= Pages
       /\ Mutation \in Mutations /\ BudgetSeconds \in Nat

None == "none"
\* Versions are naturals; these are not versions.
NoVer == -1
Torn == -2
Foreign == -3   \* bytes a third party wrote over the published output
\* An absent row or capture record.
NoRecord == [valid |-> FALSE, key |-> <<0, 0>>, ver |-> NoVer]
Rec(k, v) == [valid |-> TRUE, key |-> k, ver |-> v]

VARIABLES
    \* The source store.
    lv, mainId, walId, commits, checkpoints, resets,
    \* One run.
    phase,        \* "idle", "walked1", "walked2", "copy", "copied", "staged",
                  \* "intent", "unlinked", "exchanged", "done", "refused"
    runs, crashes,
    wkMain, wkWal, lvAtWalk,  \* the walk's two stats, and lv at the first
    session,      \* "open", "refused" (root, before anything)
    \* The backup.
    connOpen, stepLock, progress, copyVer, steps, lockSpan, backups,
    preMain, preWal, postWal, snapVer, settled,
    \* The destination (durable): output, capture record, reuse row, sidecar.
    out, record, row, destSidecar,
    \* Q146: the output is the one this store landed, untouched; a
    \* superseding publish's intent, the rows it took out, the old output.
    owned, intent, savedRow, prevOut,
    \* Ghosts for the properties.
    reusedVer, decided, supersedes, besideSidecar, covered, refusedByHeader,
    backupsThisRun, provenAtDecide, everOpened, landed, clobbered

vars == <<lv, mainId, walId, commits, checkpoints, resets, phase, runs, crashes,
          wkMain, wkWal, lvAtWalk, session, connOpen, stepLock, progress,
          copyVer, steps, lockSpan, backups, preMain, preWal, postWal, snapVer,
          settled, out, record, row, destSidecar, owned, intent, savedRow,
          prevOut, reusedVer, decided, supersedes, besideSidecar, covered,
          refusedByHeader, backupsThisRun, provenAtDecide, everOpened, landed,
          clobbered>>

\* The source and the run, untouched by a destination-only step.
srcVars == <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
             wkMain, wkWal, lvAtWalk, session, connOpen, stepLock, progress,
             copyVer, steps, lockSpan, backups, preMain, preWal, postWal,
             snapVer, settled>>
\* The ghosts a destination step leaves alone unless it says otherwise.
runGhosts == <<reusedVer, decided, covered, refusedByHeader, backupsThisRun,
               provenAtDecide, everOpened>>

\* The key a reuse row or a capture record carries.
Key(m, w) == IF Mutation = "sqlite_key_main_only" THEN <<m, 0>> ELSE <<m, w>>
RecordKey(m, w) ==
    IF Mutation = "sqlite_capture_record_main_only" THEN <<m, 0>> ELSE Key(m, w)

\* The capture record a settled capture's file carries (R1).
NewRecord == IF settled THEN Rec(RecordKey(wkMain, postWal), snapVer) ELSE NoRecord

Versions == {NoVer, Torn, Foreign} \cup (0..MaxCommits)

TypeOK ==
    /\ lv \in 0..MaxCommits /\ commits \in 0..MaxCommits
    /\ mainId \in Nat /\ walId \in Nat
    /\ phase \in {"idle", "walked1", "walked2", "copy", "copied", "staged",
                  "intent", "unlinked", "exchanged", "done", "refused"}
    /\ session \in {"open", "refused"}
    /\ connOpen \in BOOLEAN /\ stepLock \in BOOLEAN
    /\ out \in Versions /\ prevOut \in Versions
    /\ owned \in BOOLEAN /\ intent \in BOOLEAN
    /\ decided \in {None, "reuse", "copy", "refuse"}

Init ==
    /\ lv = 0 /\ mainId = 1 /\ walId = IF Wal THEN 1 ELSE 0
    /\ commits = 0 /\ checkpoints = 0 /\ resets = 0
    /\ phase = "idle" /\ runs = 0 /\ crashes = 0
    /\ wkMain = 0 /\ wkWal = 0 /\ lvAtWalk = 0
    /\ session = "open"
    /\ connOpen = FALSE /\ stepLock = FALSE /\ progress = 0 /\ copyVer = 0
    /\ steps = 0 /\ lockSpan = 0 /\ backups = 0
    /\ preMain = 0 /\ preWal = 0 /\ postWal = 0 /\ snapVer = NoVer
    /\ settled = FALSE
    /\ out = NoVer /\ record = NoRecord /\ row = NoRecord /\ destSidecar = FALSE
    /\ owned = FALSE /\ intent = FALSE /\ savedRow = NoRecord /\ prevOut = NoVer
    /\ reusedVer = NoVer /\ decided = None /\ supersedes = 0
    /\ besideSidecar = FALSE /\ covered = FALSE /\ refusedByHeader = FALSE
    /\ backupsThisRun = 0 /\ provenAtDecide = FALSE /\ everOpened = FALSE
    /\ landed = FALSE /\ clobbered = FALSE

----------------------------------------------------------------------------
(* The environment: the provider's writer and checkpointer, third parties. *)

\* A WAL commit appends to the -wal: never blocked by a reader (R3), unless
\* the backup took a lock it must not.
SqlCommitWal ==
    /\ Wal /\ commits < MaxCommits
    /\ ~(Mutation = "wal_lock_exclusive" /\ connOpen)
    /\ lv' = lv + 1 /\ walId' = walId + 1 /\ commits' = commits + 1
    /\ UNCHANGED <<mainId, checkpoints, resets, phase, runs, crashes, wkMain, wkWal,
                   lvAtWalk, session, connOpen, stepLock, progress, copyVer,
                   steps, lockSpan, backups, preMain, preWal, postWal, snapVer, settled,
                   out, record, row, destSidecar, owned, intent, savedRow, prevOut,
                   reusedVer, decided, supersedes, besideSidecar, covered,
                   refusedByHeader, backupsThisRun, provenAtDecide, everOpened,
                   landed, clobbered>>

\* A rollback-mode commit writes the main file: it waits while a step
\* holds SHARED (busy timeout 0 on our side; the writer's own wait).
SqlCommitRollback ==
    /\ ~Wal /\ commits < MaxCommits /\ ~stepLock
    /\ lv' = lv + 1 /\ mainId' = mainId + 1 /\ commits' = commits + 1
    /\ UNCHANGED <<walId, checkpoints, resets, phase, runs, crashes, wkMain, wkWal,
                   lvAtWalk, session, connOpen, stepLock, progress, copyVer,
                   steps, lockSpan, backups, preMain, preWal, postWal, snapVer, settled,
                   out, record, row, destSidecar, owned, intent, savedRow, prevOut,
                   reusedVer, decided, supersedes, besideSidecar, covered,
                   refusedByHeader, backupsThisRun, provenAtDecide, everOpened,
                   landed, clobbered>>

\* A FULL or TRUNCATE checkpoint copies the -wal into the main file: it
\* waits for every reader's read mark, so it is held back while a step
\* reads, never longer (R3). It moves the main file, not lv.
SqlCheckpoint ==
    /\ Wal /\ checkpoints < MaxCheckpoints /\ ~stepLock
    /\ mainId' = mainId + 1 /\ checkpoints' = checkpoints + 1
    /\ UNCHANGED <<lv, walId, commits, resets, phase, runs, crashes, wkMain,
                   wkWal, lvAtWalk, session, connOpen, stepLock, progress,
                   copyVer, steps, lockSpan, backups, preMain, preWal, postWal,
                   snapVer, settled, out, record, row, destSidecar, owned, intent,
                   savedRow, prevOut, reusedVer, decided, supersedes, besideSidecar,
                   covered, refusedByHeader, backupsThisRun, provenAtDecide,
                   everOpened, landed, clobbered>>

\* After a checkpoint, the next writer rewrites the -wal from its start.
SqlWalReset ==
    /\ Wal /\ resets < checkpoints
    /\ walId' = walId + 1 /\ resets' = resets + 1
    /\ UNCHANGED <<lv, mainId, commits, checkpoints, phase, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain, preWal,
                   postWal, snapVer, settled, out, record, row, destSidecar, owned,
                   intent, savedRow, prevOut, reusedVer, decided, supersedes,
                   besideSidecar, covered, refusedByHeader, backupsThisRun,
                   provenAtDecide, everOpened, landed, clobbered>>

\* A third party opens the published file with SQLite at the destination
\* (or drops a stale sidecar there), at any step of a run.
SidecarAppear ==
    /\ DestSidecars /\ ~destSidecar /\ out /= NoVer
    /\ destSidecar' = TRUE
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, phase, runs,
                   crashes, wkMain, wkWal, lvAtWalk, session, connOpen,
                   stepLock, progress, copyVer, steps, lockSpan, backups,
                   preMain, preWal, postWal, snapVer, settled, out, record, row,
                   owned, intent, savedRow, prevOut, reusedVer, decided,
                   supersedes, besideSidecar, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened, landed, clobbered>>

\* The operator removes it (R5: the next run converges).
SidecarRemoved ==
    /\ destSidecar /\ phase = "idle"
    /\ destSidecar' = FALSE
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, phase, runs,
                   crashes, wkMain, wkWal, lvAtWalk, session, connOpen,
                   stepLock, progress, copyVer, steps, lockSpan, backups,
                   preMain, preWal, postWal, snapVer, settled, out, record, row,
                   owned, intent, savedRow, prevOut, reusedVer, decided,
                   supersedes, besideSidecar, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened, landed, clobbered>>

\* A third party writes the published output in place, at any step of a
\* run (Q146): its bytes and identity are no longer what this store
\* landed, and the capture record on it no longer proves them.
DestTouch ==
    /\ DestTouches /\ out \notin {NoVer, Foreign}
    /\ out' = Foreign /\ owned' = FALSE /\ record' = NoRecord
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, phase, runs,
                   crashes, wkMain, wkWal, lvAtWalk, session, connOpen,
                   stepLock, progress, copyVer, steps, lockSpan, backups,
                   preMain, preWal, postWal, snapVer, settled, row, destSidecar,
                   intent, savedRow, prevOut, reusedVer, decided, supersedes,
                   besideSidecar, covered, refusedByHeader, backupsThisRun,
                   provenAtDecide, everOpened, landed, clobbered>>

----------------------------------------------------------------------------
(* A run.                                                                  *)

\* Open: a snapshot-mode session as root is refused before anything is
\* opened (OI-1003-Q76, design D5); the walk stats the main file.
WalkMain ==
    /\ phase = "idle" /\ runs < MaxRuns
    /\ runs' = runs + 1
    /\ IF SnapshotMode /\ AsRoot /\ Mutation /= "sqlite_as_root"
         THEN /\ session' = "refused" /\ phase' = "refused"
              /\ UNCHANGED <<wkMain, lvAtWalk>>
         ELSE /\ session' = "open" /\ phase' = "walked1"
              /\ wkMain' = mainId /\ lvAtWalk' = lv
    /\ backupsThisRun' = 0 /\ decided' = None /\ reusedVer' = NoVer
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, crashes, wkWal,
                   connOpen, stepLock, progress, copyVer, steps, lockSpan,
                   backups, preMain, preWal, postWal, snapVer, settled, out, record,
                   row, destSidecar, owned, intent, savedRow, prevOut, supersedes,
                   besideSidecar, covered, refusedByHeader, provenAtDecide,
                   everOpened, landed, clobbered>>

\* The -wal is stat'ed in its own step (R11): commits may land between.
WalkWal ==
    /\ phase = "walked1"
    /\ wkWal' = walId /\ phase' = "walked2"
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, lvAtWalk, session, connOpen, stepLock, progress,
                   copyVer, steps, lockSpan, backups, preMain, preWal, postWal, snapVer,
                   settled, out, record, row, destSidecar, owned, intent, savedRow,
                   prevOut, reusedVer, decided, supersedes, besideSidecar, covered,
                   refusedByHeader, backupsThisRun, provenAtDecide, everOpened,
                   landed, clobbered>>

\* What the destination's records prove for the walked key: its reuse row,
\* or (R1) an unrowed output whose capture record names the key. Either
\* only for the output this store landed, untouched (its identity).
RowProves == row.valid /\ row.key = Key(wkMain, wkWal) /\ out /= NoVer /\ owned
RecordProves ==
    /\ record.valid /\ out /= NoVer /\ owned
    /\ IF Mutation = "sqlite_capture_record_main_only"
         THEN record.key[1] = wkMain
         ELSE record.key = Key(wkMain, wkWal)

\* Decide. Refuse mode, or a raw base, carries nothing here: the file seat
\* is BulkloadTransfer's. In snapshot mode a v5 record is ignored (R8), a
\* destination sidecar refuses the path before anything is read (R5, and
\* Q146's first look), and another user's database is refused before
\* SQLite opens it (R4).
Decide ==
    /\ phase = "walked2"
    /\ provenAtDecide' = (RowProves \/ RecordProves)
    /\ CASE ~SnapshotMode \/ ~BaseIsDb ->
              /\ decided' = "refuse" /\ phase' = "refused"
              /\ UNCHANGED <<reusedVer, refusedByHeader, row, record>>
         [] V5Record /\ Mutation = "sqlite_header_refusal_in_snapshot_mode" ->
              /\ decided' = "refuse" /\ phase' = "refused"
              /\ refusedByHeader' = TRUE
              /\ UNCHANGED <<reusedVer, row, record>>
         [] (RowProves \/ RecordProves) /\ Mutation /= "sqlite_reuse_ignored" ->
              \* Reuse; an unrowed output is adopted: its row commits now.
              /\ decided' = "reuse" /\ phase' = "done"
              /\ reusedVer' = IF RowProves THEN row.ver ELSE record.ver
              /\ row' = IF RowProves THEN row
                        ELSE Rec(Key(wkMain, wkWal), record.ver)
              /\ UNCHANGED <<refusedByHeader, record>>
         [] destSidecar /\ Mutation /= "sqlite_publish_beside_sidecar" ->
              /\ decided' = "refuse" /\ phase' = "refused"
              /\ UNCHANGED <<reusedVer, refusedByHeader, row, record>>
         [] ~OwnedByReader /\ Mutation /= "sqlite_not_owner_opened" ->
              /\ decided' = "refuse" /\ phase' = "refused"
              /\ UNCHANGED <<reusedVer, refusedByHeader, row, record>>
         [] OTHER ->
              /\ decided' = "copy" /\ phase' = "copy"
              /\ UNCHANGED <<reusedVer, refusedByHeader, row, record>>
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain,
                   preWal, postWal, snapVer, settled, out, destSidecar, owned,
                   intent, savedRow, prevOut, supersedes, besideSidecar, covered,
                   backupsThisRun, everOpened, landed, clobbered>>

\* Pre-stat, open the source connection read-only (one backup at a time:
\* there is one seat), and begin.
BackupBegin ==
    /\ phase = "copy" /\ ~connOpen
    /\ preMain' = mainId /\ preWal' = walId
    /\ connOpen' = TRUE /\ everOpened' = TRUE
    /\ progress' = 0 /\ steps' = 0 /\ copyVer' = lv
    /\ backups' = backups + 1 /\ backupsThisRun' = backupsThisRun + 1
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, phase, runs,
                   crashes, wkMain, wkWal, lvAtWalk, session, stepLock,
                   lockSpan, postWal, snapVer, settled, out, record, row, destSidecar,
                   owned, intent, savedRow, prevOut, reusedVer, decided, supersedes,
                   besideSidecar, covered, refusedByHeader, provenAtDecide, landed,
                   clobbered>>

\* One step: take the shared lock (or read mark), copy a page run of the
\* current version; a version that moved since the last step restarts the
\* copy from page 1. Past the budget the capture is refused.
BackupStep ==
    /\ phase = "copy" /\ connOpen /\ progress < Pages
    /\ steps < StepBudget
    /\ ~stepLock \/ Mutation = "sqlite_unbounded_pinned"
    /\ stepLock' = TRUE
    /\ lockSpan' = lockSpan + 1
    /\ steps' = steps + 1
    /\ IF lv = copyVer
         THEN /\ progress' = progress + 1 /\ UNCHANGED copyVer
         ELSE /\ progress' = 1 /\ copyVer' = lv
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, phase, runs,
                   crashes, wkMain, wkWal, lvAtWalk, session, connOpen,
                   backups, preMain, preWal, postWal, snapVer, settled, out, record,
                   row, destSidecar, owned, intent, savedRow, prevOut, reusedVer,
                   decided, supersedes, besideSidecar, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened, landed, clobbered>>

\* The lock is released at the end of every step (OI-1003-Q16, D2).
StepUnlock ==
    /\ stepLock /\ Mutation /= "sqlite_unbounded_pinned"
    /\ stepLock' = FALSE /\ lockSpan' = 0
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, phase, runs,
                   crashes, wkMain, wkWal, lvAtWalk, session, connOpen,
                   progress, copyVer, steps, backups, preMain, preWal, postWal, snapVer,
                   settled, out, record, row, destSidecar, owned, intent, savedRow,
                   prevOut, reusedVer, decided, supersedes, besideSidecar, covered,
                   refusedByHeader, backupsThisRun, provenAtDecide, everOpened,
                   landed, clobbered>>

\* Out of budget: BUDGET_EXCEEDED, not remembered; the connection closes.
BackupRefused ==
    /\ phase = "copy" /\ connOpen /\ progress < Pages /\ steps >= StepBudget
    /\ ~stepLock \/ Mutation = "sqlite_unbounded_pinned"
    /\ connOpen' = FALSE /\ stepLock' = FALSE /\ lockSpan' = 0
    /\ phase' = "refused"
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, progress, copyVer, steps,
                   backups, preMain, preWal, postWal, snapVer, settled, out, record, row,
                   destSidecar, owned, intent, savedRow, prevOut, reusedVer, decided,
                   supersedes, besideSidecar, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened, landed, clobbered>>

\* The copy is complete; the connection closes; the post-stat decides
\* whether the capture settled (design 5.4): the walked main, the pre- and
\* the post-stat agree, and the -wal did not move. A raw send (the
\* mutation) carries the live file instead, torn when a commit landed.
BackupEnd ==
    /\ phase = "copy" /\ connOpen /\ progress = Pages
    /\ ~stepLock \/ Mutation = "sqlite_unbounded_pinned"
    /\ connOpen' = FALSE /\ stepLock' = FALSE /\ lockSpan' = 0
    /\ settled' = (preMain = wkMain /\ mainId = preMain /\ walId = preWal)
    /\ postWal' = walId
    /\ snapVer' = IF Mutation = "sqlite_raw_send" /\ lv /= copyVer
                    THEN Torn ELSE copyVer
    /\ phase' = "copied"
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, progress, copyVer, steps,
                   backups, preMain, preWal, out, record, row, destSidecar, owned,
                   intent, savedRow, prevOut, reusedVer, decided, supersedes,
                   besideSidecar, covered, refusedByHeader, backupsThisRun,
                   provenAtDecide, everOpened, landed, clobbered>>

\* The verified snapshot is published (verify_snapshot's sidecar look, then
\* the group commit). Beside a destination sidecar: refused (R5). Into a
\* free leaf: renamed without replacement, its capture record on it (R1).
\* Over the same bytes: adopted. Over this store's own output, untouched:
\* WP0(d)'s prepare and intent (Q146), which takes the old rows out. Over
\* anything else: DESTINATION_OCCUPIED, kept.
Publish ==
    /\ phase = "copied"
    /\ CASE destSidecar /\ Mutation /= "sqlite_publish_beside_sidecar" ->
              /\ phase' = "refused"
              /\ UNCHANGED <<out, record, row, owned, intent, savedRow, prevOut,
                             besideSidecar, landed, clobbered>>
         [] out = NoVer \/ out = snapVer ->
              /\ phase' = "staged"
              /\ out' = snapVer /\ owned' = TRUE /\ landed' = TRUE
              /\ besideSidecar' = (besideSidecar \/ destSidecar)
              /\ record' = NewRecord
              /\ UNCHANGED <<row, intent, savedRow, prevOut, clobbered>>
         [] owned \/ Mutation = "sqlite_supersede_unowned" ->
              /\ phase' = "intent" /\ intent' = TRUE
              /\ savedRow' = row /\ row' = NoRecord /\ prevOut' = out
              /\ UNCHANGED <<out, record, owned, besideSidecar, landed, clobbered>>
         [] OTHER ->
              /\ phase' = "refused"
              /\ UNCHANGED <<out, record, row, owned, intent, savedRow, prevOut,
                             besideSidecar, landed, clobbered>>
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain,
                   preWal, postWal, snapVer, settled, destSidecar, reusedVer,
                   decided, supersedes, covered, refusedByHeader, backupsThisRun,
                   provenAtDecide, everOpened>>

\* The exchange (Q146, WP0(d) steps 3 and 4), after the intent committed.
\* The last look: the output must still be this store's own (else refused,
\* no rows back: they described other bytes), and no sidecar may sit beside
\* it (else refused, the old rows back). Then the staged file and the old
\* output trade names in one step; the displaced old output is removed.
\* The unlink-then-rename mutation does it in two steps instead.
Exchange ==
    /\ phase = "intent"
    /\ CASE ~owned /\ Mutation /= "sqlite_supersede_unowned" ->
              /\ phase' = "refused" /\ intent' = FALSE
              /\ UNCHANGED <<out, record, row, owned, supersedes, besideSidecar,
                             landed, clobbered>>
         [] destSidecar /\ Mutation \notin {"sqlite_supersede_no_recheck",
                                            "sqlite_publish_beside_sidecar"} ->
              /\ phase' = "refused" /\ intent' = FALSE /\ row' = savedRow
              /\ UNCHANGED <<out, record, owned, supersedes, besideSidecar,
                             landed, clobbered>>
         [] Mutation = "sqlite_supersede_unlink_rename" ->
              /\ phase' = "unlinked" /\ out' = NoVer /\ owned' = FALSE
              /\ record' = NoRecord
              /\ clobbered' = (clobbered \/ ~owned)
              /\ UNCHANGED <<row, intent, supersedes, besideSidecar, landed>>
         [] OTHER ->
              /\ phase' = "exchanged"
              /\ out' = snapVer /\ owned' = TRUE /\ record' = NewRecord
              /\ supersedes' = supersedes + 1
              /\ besideSidecar' = (besideSidecar \/ destSidecar)
              /\ clobbered' = (clobbered \/ ~owned)
              /\ UNCHANGED <<row, intent, landed>>
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain,
                   preWal, postWal, snapVer, settled, destSidecar, savedRow,
                   prevOut, reusedVer, decided, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened>>

\* The mutation's second step: the new file renamed into the emptied leaf.
RenameAfterUnlink ==
    /\ phase = "unlinked"
    /\ phase' = "exchanged"
    /\ out' = snapVer /\ owned' = TRUE /\ record' = NewRecord
    /\ supersedes' = supersedes + 1
    /\ besideSidecar' = (besideSidecar \/ destSidecar)
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain,
                   preWal, postWal, snapVer, settled, row, destSidecar, intent,
                   savedRow, prevOut, reusedVer, decided, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened, landed, clobbered>>

\* The group commit: the reuse row under the walked main and the settled
\* -wal, or none (an ownership row, not modelled) for an unsettled one;
\* a superseding publish's intent is settled with it.
CommitRow ==
    /\ phase \in {"staged", "exchanged"}
    /\ row' = IF settled \/ Mutation = "sqlite_record_unsettled"
                THEN Rec(Key(wkMain, postWal), snapVer)
                ELSE NoRecord
    /\ intent' = FALSE
    /\ phase' = "done"
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain,
                   preWal, postWal, snapVer, settled, out, record, destSidecar,
                   owned, savedRow, prevOut, reusedVer, decided, supersedes,
                   besideSidecar, covered, refusedByHeader, backupsThisRun,
                   provenAtDecide, everOpened, landed, clobbered>>

\* The session ends: a sidecar beside the base is covered by its base's
\* outcome (R2), never by its name.
RunEnd ==
    /\ phase \in {"done", "refused"}
    /\ covered' = IF Mutation = "sidecar_covered_by_name"
                    THEN TRUE
                    ELSE (decided \in {"reuse", "copy"} /\ phase = "done"
                          /\ BaseIsDb /\ SnapshotMode)
    /\ phase' = "idle"
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, crashes,
                   wkMain, wkWal, lvAtWalk, session, connOpen, stepLock,
                   progress, copyVer, steps, lockSpan, backups, preMain,
                   preWal, postWal, snapVer, settled, out, record, row, destSidecar,
                   owned, intent, savedRow, prevOut, reusedVer, decided, supersedes,
                   besideSidecar, refusedByHeader, backupsThisRun, provenAtDecide,
                   everOpened, landed, clobbered>>

\* A crash anywhere: the connection and lock go with the process; the
\* destination's durable state stays (an output published before its row
\* committed keeps its record, R1). The next session's sweep settles a
\* superseding publish's intent first: an old output still in place, and
\* still this store's own, gets its rows back; a new one in place keeps
\* none (it is adopted from its capture record while its seat holds).
Crash ==
    /\ crashes < MaxCrashes /\ phase \notin {"idle", "refused"}
    /\ crashes' = crashes + 1
    /\ phase' = "idle" /\ connOpen' = FALSE /\ stepLock' = FALSE
    /\ lockSpan' = 0
    /\ intent' = FALSE
    /\ row' = IF intent /\ phase = "intent" /\ owned THEN savedRow ELSE row
    /\ UNCHANGED <<lv, mainId, walId, commits, checkpoints, resets, runs, wkMain,
                   wkWal, lvAtWalk, session, progress, copyVer, steps, backups,
                   preMain, preWal, postWal, snapVer, settled, out, record,
                   destSidecar, owned, savedRow, prevOut, reusedVer, decided,
                   supersedes, besideSidecar, covered, refusedByHeader,
                   backupsThisRun, provenAtDecide, everOpened, landed, clobbered>>

Next ==
    \/ SqlCommitWal \/ SqlCommitRollback \/ SqlCheckpoint \/ SqlWalReset
    \/ SidecarAppear \/ SidecarRemoved \/ DestTouch
    \/ WalkMain \/ WalkWal \/ Decide
    \/ BackupBegin \/ BackupStep \/ StepUnlock \/ BackupRefused \/ BackupEnd
    \/ Publish \/ Exchange \/ RenameAfterUnlink \/ CommitRow \/ RunEnd \/ Crash

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Properties.                                                             *)

\* G1: a published output is a committed version, never a torn one.
SqliteNeverTorn == out /= Torn

\* The key's soundness (5.2, R1, R11): a Reuse, from a row or from an
\* adopted record, is of the version the store had when its main file was
\* walked. Defined at the walk, not "now": a commit after the walk is the
\* next run's.
SqliteReuseSound == decided = "reuse" => reusedVer = lvAtWalk

\* R3: in WAL mode no lock of the backup's holds a writer back.
WalWriterNeverBlocked ==
    (Wal /\ commits < MaxCommits) => ENABLED SqlCommitWal

\* S2 (OI-1003-Q16, D2): the lock is held inside one step only, and the
\* connection (SHARED on a WAL main file) for one bounded backup.
S2_BackupLockBounded == lockSpan <= 1 /\ steps <= StepBudget

\* OI-1003-Q76: as root a snapshot-mode session opens nothing.
SqliteRootRefusedUpFront == (AsRoot /\ SnapshotMode) => ~everOpened

\* R4: another user's database is never opened.
SqliteOwnerRefused == ~OwnedByReader => ~everOpened

\* R5, Q146: a snapshot is never published, by a rename or an exchange,
\* while a sidecar sits beside its path.
SqliteNoForeignSidecar == ~besideSidecar

\* Q146: only the output this store landed, untouched, is ever replaced:
\* a file a third party wrote is never superseded.
SqliteNoClobber == ~clobbered

\* Q146: once a snapshot was published at the path, the path always holds
\* a whole database (a committed version, or the third party's own bytes),
\* never nothing and never torn; during a superseding publish, exactly the
\* old output or the new snapshot.
SqliteOldOrNewWhole ==
    /\ landed => out \notin {NoVer, Torn}
    /\ phase \in {"intent", "exchanged"} => out \in {prevOut, snapVer, Foreign}

\* R2: a sidecar is covered only by its base's snapshot.
SidecarCoverageSound == covered => (BaseIsDb /\ SnapshotMode)

\* R8: snapshot mode does not honour a refuse-mode record.
SnapshotModeIgnoresV5Record == ~refusedByHeader

\* S3: a run whose walked key the destination already proves takes no
\* backup.
S3_UnchangedSqliteZero == provenAtDecide => backupsThisRun = 0

\* The search's wall-clock budget (README.md, "The budget is state-level"):
\* a state predicate, so TLC evaluates it on every state.
WithinBudget == lv \in Nat => TLCGet("duration") < BudgetSeconds

\* Reachability witnesses (expected to be violated by a reach config, so
\* that it shows the state is reached): a WAL reuse, a restart, a
\* superseding publish of a changed store's snapshot.
Witness_Reuse == ~(decided = "reuse" /\ Wal)
Witness_Restart == ~(progress = 1 /\ steps >= 2)
Witness_Superseded == ~(supersedes >= 1 /\ phase = "done")

=============================================================================
