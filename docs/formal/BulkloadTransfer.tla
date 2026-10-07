--------------------------- MODULE BulkloadTransfer ---------------------------
(***************************************************************************)
(* Formal model of bulkload's wire v5 transfer, its destination group      *)
(* commit and its source ledger, across crashes and reruns.                *)
(*                                                                         *)
(* Proof package item "formal model" (docs/slo.md, OI-1003-Q7). Checked    *)
(* with TLC; see docs/formal/README.md for configs and results.            *)
(*                                                                         *)
(* SCOPE                                                                   *)
(*   - Wire v5 per entry: Entry, Decide (Reuse / Send / WantManifest /     *)
(*     Refuse), Manifest, NeedChunks, data + End, Refused, Held, and       *)
(*     SourceDone (crates/bulkload-proto/src/frame.rs, Control).           *)
(*   - Destination staging, file seal (fsync / F_BARRIERFSYNC), no-replace *)
(*     publish, directory seal, and the SQLite group commit of output      *)
(*     rows (synchronous=FULL, fullfsync=ON).                              *)
(*   - Held only after that group commit (OI-1001-Q15); the source's       *)
(*     digest-only capture ledger committed only on Held{true}.            *)
(*   - Crashes (power loss or process crash) of either host, or of both    *)
(*     at once (loopback copy), and the rerun that follows.                *)
(*   - Racy captures (#86, R-N76), source edits, third-party writes at the *)
(*     destination, full-disk group commit failures (#100) and the space   *)
(*     preflight refusal (OI-1001-Q2).                                     *)
(*   - S2 typed source access (WP0(b)), including the SQLite backup API's  *)
(*     shared read lock as the one bounded, counted exception.             *)
(*   - WP0(d) superseding publish as two designs: exchange (the code       *)
(*     since #187) and check-then-rename (rejected), and WP0(g) relaxed    *)
(*     source-ledger durability.                                           *)
(*   - The source store's authority: a random value in the source          *)
(*     store's settings table, created with the schema by Store::open and  *)
(*     part of every row key on both sides. A relaxed source store can     *)
(*     lose it (RelaxedAuthority), and so can a store whose state root's   *)
(*     directory entry is not sealed (StoreRootSealed = FALSE, the code    *)
(*     today); either loss re-keys every row.                              *)
(*                                                                         *)
(* ABSTRACTIONS (what is NOT modelled)                                     *)
(*   - Content is an opaque version number per seat. Chunk boundaries,     *)
(*     FastCDC, BLAKE3 digests and manifest_root are not modelled: a       *)
(*     digest check is equality of versions. Cross-seat chunk dedup is     *)
(*     abstracted, so NeedChunks is "none" or "some".                      *)
(*   - Credit flow control, the 1024-entry window, walk-ahead and the     *)
(*     retention budget are not modelled: they bound memory, not safety.   *)
(*     The streamed fallback for a manifest past the retention budget is   *)
(*     folded into the manifest path (both read the seat exactly once).    *)
(*   - A capture (stat check, read, racy test) is one atomic step. The     *)
(*     code's racy test uses the clock read before open, which also       *)
(*     covers writes during the read; see capture_file.                   *)
(*   - Stat identity (dev, ino, size, mtime, ctime) is a version counter   *)
(*     that only grows (ctime cannot be set back). The racy window is a    *)
(*     boolean "fresh" per seat, cleared by Tick.                          *)
(*   - Only regular files in one directory. Directory records (R-N102),   *)
(*     symlinks, Skip, engine temporaries, walk caps and devices other    *)
(*     than the store's are not modelled.                                 *)
(*   - Orphaned temporaries are swept at the next session start. Salvage  *)
(*     (#97) only saves wire bytes, so it is folded into the destination's *)
(*     free choice between Send and WantManifest for an absent output.    *)
(*   - The SQLite stores are sets of rows; a commit is atomic. The        *)
(*     destination store is durable at commit. The source ledger is       *)
(*     durable at commit unless RelaxedSourceLedger, in which case a       *)
(*     source power loss may drop ANY subset of its rows (a superset of   *)
(*     WAL synchronous=NORMAL losing a suffix of commits). A relaxed      *)
(*     store is assumed to lose rows, never to return a wrong one: torn   *)
(*     or reordered pages under fullfsync=OFF on Darwin are NOT modelled  *)
(*     (README, open questions).                                          *)
(*   - ASSUMPTION: each store's state root and database file exist        *)
(*     durably once the store's first commit returns. The code does not   *)
(*     establish this yet: private_dir creates the state root and never   *)
(*     seals its parent directory (README, "Code and design               *)
(*     disagreements"). StoreRootSealed = FALSE drops the assumption for  *)
(*     the source store, which then behaves like a relaxed creation      *)
(*     commit (MC_store_root_unsealed). Losing the destination store is   *)
(*     not modelled: HeldPhys needs one of its rows, so R25 could not see *)
(*     a re-read after such a loss.                                        *)
(*   - A source ledger commit never fails here. In the code the first     *)
(*     failed ledger group fails the whole session before SourceDone      *)
(*     (LedgerSink::commit, Committer::submit); README, "Not proven".     *)
(*   - "Held" means a committed destination row (HeldPhys). Bytes that    *)
(*     are durable at the final path with no row (a crash before          *)
(*     commit_outputs, a failed group whose files were renamed) were read *)
(*     again before #169; R25_StrictNoDurableReread, under                *)
(*     TrackStrictHeld, makes that gap visible (MC_r25_unrowed_no_adopt). *)
(*     Under AdoptUnrowed (the code since #169) each such output carries  *)
(*     its capture record and is adopted on resume without a read         *)
(*     (MC_r25_unrowed_bytes, MC_r25_strict_deep). The record is a field  *)
(*     of the file (rec): the walked row it was captured from (its stat   *)
(*     version, with no authority) and the bytes it names; the output's   *)
(*     hash check is equality of versions. A staged file gets it before   *)
(*     its seal (NewOut); an existing output adopted against a manifest   *)
(*     gets it once verified (RecvEnd), and counts as held strictly once  *)
(*     its adoption is sealed (SealAdopted). ASSUMPTION: the record can   *)
(*     be written. A file system without extended attributes, or an       *)
(*     adopted output its owner cannot write, keeps none and costs the    *)
(*     re-read of before #169 (MC_r25_unrowed_no_adopt); the strict       *)
(*     reading does not hold there. Salvaged temporaries are folded away  *)
(*     (see above) and not covered by it.                                 *)
(*   - A seal (file or directory) is durable at once. On Darwin a seal is *)
(*     F_BARRIERFSYNC, an ordering barrier, and the group's full flush or  *)
(*     the store commit's fullfsync is the durability point. That only     *)
(*     removes crash states before the commit, where no row exists yet;    *)
(*     the barrier model is io/crash_check.rs's job (R-N88).               *)
(*   - Row keys are integers: authority epoch * KeyBase + stat version.   *)
(*     The root's own (path, dev, ino) in the authority never changes.    *)
(*   - Background priority (WP0(f)) is a measured S2 budget, not a        *)
(*     safety property here; it is covered by P35.                        *)
(*   - Git carry v1/v2, the ingest journal (carry_v2, frozen by WP0(a))   *)
(*     and estate apply's .done journals are not modelled. Their journal  *)
(*     rule ("a record never precedes what it describes") is the same     *)
(*     ordering RecordImpliesBytes checks here. Estate capture enters    *)
(*     only through its typed source reads (git, SQLite backup).          *)
(*                                                                         *)
(* CODE MAP (each action below also cites its code)                        *)
(*   A = crates/bulkload-agent/src                                         *)
(*   Walk            A/transfer.rs walk_source, Outbound::offer;           *)
(*                   A/walk.rs Walker                                      *)
(*   RecvEntry       A/transfer.rs Inbound::entry, Inbound::admit,         *)
(*                   Inbound::adopt_unrowed; A/transfer/unrowed.rs prove; *)
(*                   A/materialize.rs Destination::identity;               *)
(*                   A/transfer_store.rs Store::output_matches            *)
(*   RecvDecide      A/transfer.rs Outbound::decide, run_job,              *)
(*                   send_capture, manifest_capture, capture_file;         *)
(*                   A/git_carry.rs racy; A/transfer_store.rs Store::capture*)
(*   RecvManifest    A/transfer.rs Inbound::manifest, plan_file            *)
(*   RecvNeed        A/transfer.rs Outbound::need_chunks, serve_chunks     *)
(*   RecvEnd         A/transfer.rs Inbound::end, end_streaming,            *)
(*                   end_filling, publish, adopt;                          *)
(*                   A/materialize.rs verify_existing;                     *)
(*                   A/transfer/unrowed.rs refresh                         *)
(*   RecvRefused     A/transfer.rs Inbound::refused                        *)
(*   SealTemp        A/materialize.rs StagedFile::seal;                    *)
(*                   A/io/durable.rs seal_file                             *)
(*   Publish         A/materialize.rs StagedFile::publish                  *)
(*                   (the capture record from A/transfer.rs publish,       *)
(*                   A/transfer/unrowed.rs write_record)                   *)
(*                   (io::publish_noreplace), PublishSink::commit          *)
(*   DirSeal         A/materialize.rs TouchedDevices::seal;                *)
(*                   A/io/durable.rs seal_dir                              *)
(*   SealAdopted     A/materialize.rs PublishSink::commit                  *)
(*                   (Publication::Adopted)                                *)
(*   Commit          A/transfer_store.rs StorePublisher::commit_outputs;   *)
(*                   A/io/durable.rs configure_sqlite, Committer           *)
(*   CommitFail      A/materialize.rs space_refusal (#100)                 *)
(*   AnswerHeld      A/transfer.rs Inbound::answer_held, settle_held, end, *)
(*                   finish_receive (an adopted unrowed output's outcome)  *)
(*   RecvHeld        A/transfer.rs Outbound::handle (Event::Held)          *)
(*   LedgerCommit    A/transfer_store.rs LedgerSink::publish,              *)
(*                   StorePublisher::commit_captures                       *)
(*   SendSourceDone  A/transfer.rs serve (committer.sync, SourceDone)      *)
(*   Finish          A/transfer.rs Inbound::run, finish_receive            *)
(*   StartRun        A/transfer.rs receive (sweep_root), serve (Open,      *)
(*                   Store::open, Store::authority);                       *)
(*                   A/transfer_store.rs Store::open (settings authority)  *)
(*   Crash*          A/io/crash_check.rs persistence model; A/fault.rs;    *)
(*                   Committer drop (A/io/durable.rs)                      *)
(*   GitRead         A/git_carry.rs git, git_env (--no-optional-locks,     *)
(*                   GIT_OPTIONAL_LOCKS=0, GIT_NO_LAZY_FETCH=1,            *)
(*                   core.hooksPath=/dev/null, core.fsmonitor=false,       *)
(*                   gc.auto=0, maintenance.auto=false)                    *)
(*   Backup*         A/provider_sqlite.rs snapshot (SQLITE_OPEN_READ_ONLY, *)
(*                   busy_timeout zero, backup.step(128) at most max_steps *)
(*                   times; each step holds the shared read lock only     *)
(*                   while it runs)                                        *)
(*   Exchange        A/transfer.rs plan_file (owned_output: the plan);     *)
(*                   A/materialize.rs PublishSink::supersede,              *)
(*                   StagedFile::prepare_supersede, StagedFile::exchange   *)
(*                   (io::exchange: RENAME_EXCHANGE / RENAME_SWAP);        *)
(*                   A/transfer_store.rs begin_supersedes, output_rows     *)
(*   VerifyDisp      A/materialize.rs StagedFile::exchange (is_owned on    *)
(*                   the displaced file; the exchange back and its seal)   *)
(*   StartRun's restore of a displaced foreign file:                       *)
(*                   A/materialize.rs Destination::settle_supersedes;      *)
(*                   A/transfer_store.rs supersede_intents,                *)
(*                   settle_supersede                                      *)
(*   CheckOwn, RenameReplace: WP0(d)'s rejected check-then-rename design;  *)
(*                   no code (MC_wp0d_check_rename is why).                *)
(*                                                                         *)
(* NEGATIVE CONFIGS set Mutation to break exactly one rule; each MUST      *)
(* produce a counterexample (see README.md). REACH CONFIGS check one       *)
(* Witness_ invariant, whose violation proves a scenario reachable.        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Seats,               \* source seats (regular files): model values
    MaxRuns,             \* transfer sessions in one behaviour
    MaxCrashes,          \* crash budget (either host, or both)
    MaxEdits,            \* per-seat source edits (stat change or racy rewrite)
    MaxForeign,          \* third-party writes or deletes at the destination
    MaxCommitFails,      \* destination group commits that fail (full disk)
    SpaceRefusals,       \* the space preflight may refuse an entry (typed)
    RelaxedSourceLedger, \* WP0(g): source ledger synchronous=NORMAL, fullfsync=OFF
    RelaxedAuthority,    \* WP0(g): the store-creation commit (schema and
                         \* authority) is relaxed too, not only the ledger rows
    SupersedeMode,       \* "off" (before #187) | "check_rename" (rejected)
                         \* | "exchange" (the code since #187)
    EstateReads,         \* model estate capture's typed reads (git, SQLite)
    MaxBackupSteps,      \* bound on SQLite backup steps under the shared lock
    Mutation,            \* "none", or one deliberate rule break
    BudgetSeconds,       \* wall-clock budget, checked by WithinBudget
    StoreRootSealed,     \* the source state root's directory entry is sealed
                         \* before the store's first commit returns (an
                         \* assumption the code does not meet yet; README)
    TrackStrictHeld,     \* ghost: mark bulkload's own durable outputs from
                         \* non-racy captures, for R25_StrictNoDurableReread
    AdoptUnrowed         \* #169: each non-racy staged file carries its capture
                         \* record, and a resume adopts a durable unrowed
                         \* output whose record proves it (the code since #169)

Mutations == {"none", "held_before_commit", "commit_before_fsync",
              "commit_before_dirseal", "adopt_without_seal",
              "ledger_before_held", "reread_durable",
              "reread_ignore_ledger",
              "src_ledger_carries_r25", "record_racy", "source_write",
              "git_optional_locks", "pause_writer", "unbounded_backup",
              "supersede_unchecked", "sweep_displaced", "double_read",
              "untyped_space", "done_before_sync", "skip_output_row",
              "adopt_unkeyed", "adopt_unverified", "reuse_ignores_row",
              "adopt_unrecorded"}

ASSUME /\ IsFiniteSet(Seats) /\ Seats # {}
       /\ MaxRuns \in Nat /\ MaxCrashes \in Nat /\ MaxEdits \in Nat
       /\ MaxForeign \in Nat /\ MaxCommitFails \in Nat
       /\ SpaceRefusals \in BOOLEAN /\ RelaxedSourceLedger \in BOOLEAN
       /\ RelaxedAuthority \in BOOLEAN
       /\ StoreRootSealed \in BOOLEAN /\ TrackStrictHeld \in BOOLEAN
       /\ AdoptUnrowed \in BOOLEAN
       /\ SupersedeMode \in {"off", "check_rename", "exchange"}
       /\ EstateReads \in BOOLEAN /\ MaxBackupSteps \in Nat
       /\ Mutation \in Mutations /\ BudgetSeconds \in Nat

(* Content versions are 0..MaxEdits. Sentinels stay far above them. *)
GARBAGE == 98        \* bytes a power loss left behind un-synced data
FOREIGN == 99        \* content a third party wrote
FirstForeignId == 50 \* bulkload's own output identities are run numbers
KeyBase == 10        \* row key = authority epoch * KeyBase + stat version
ASSUME MaxEdits < KeyBase

\* A file's capture record (#169, only under AdoptUnrowed): the walked row
\* the capture was made from and the bytes it names. Key 0 is no record.
NoRecord == [key |-> 0, data |-> 0]

\* A capture record's key for the entry of row key k
\* (A/transfer/unrowed.rs record_key): the walked row alone, its stat
\* version here, with no store authority, so a source store that lost its
\* authority still finds its outputs' records. Never 0.
RecordKey(k) == (k % KeyBase) + 1

\* A file at a destination path. cl (ghost, only under TrackStrictHeld):
\* bulkload published it from a non-racy capture, or verified it against
\* one and sealed that adoption. rec: its capture record.
NoFile == [pres |-> FALSE, id |-> 0, data |-> 0, dd |-> TRUE, nd |-> TRUE,
           cl |-> FALSE, rec |-> NoRecord]
NoTmp  == [st |-> "none", data |-> 0]
NoCap  == [data |-> 0, racy |-> FALSE, rec |-> FALSE, led |-> FALSE]
NoRec  == [key |-> 0, data |-> 0, racy |-> FALSE, id |-> 0, kind |-> "none"]
Msg(t, v, b, c) == [t |-> t, v |-> v, b |-> b, c |-> c]
NoMsg  == Msg("none", 0, FALSE, "none")

TypedCodes == {"SOURCE_CHANGED_AFTER_SNAPSHOT", "DESTINATION_OCCUPIED",
               "DESTINATION_SPACE_INSUFFICIENT"}

(* WP0(b): every source access is one of these kinds. *)
AllowedSourceOps == {"stat", "read", "git_read", "sqlite_backup"}

VARIABLES
    \* source environment: live writers on the source host
    srcStat,      \* seat -> stat identity version
    edits,        \* seat -> content version (edits made)
    fresh,        \* seat -> stamped within the racy window of "now"
    \* source process (serve) and its private digest-only ledger
    sEnt,         \* seat -> source entry phase
    sRow,         \* seat -> stat identity the walk offered (row key)
    sCap,         \* seat -> the capture offered: data, racy, rec, led
    sPend,        \* captures Held{true} and submitted, not yet committed
    srcLedger,    \* committed source ledger rows [seat, key, data]
    srcDone,      \* SourceDone sent this session
    ledgerLost,   \* ghost: ledger rows a source power loss dropped
                  \* [seat, key, run]; kept beside srcLedger
    srcAuth,      \* the source store's authority epoch (0: no store yet)
    srcStore,     \* "none" | "volatile" (creation not durable) | "durable"
    \* wire: at most one in-flight control frame per entry (request/response)
    msg,
    \* destination process, filesystem and store
    dEnt,         \* seat -> destination entry phase
    dKey,         \* seat -> row key of the entry
    dPlan,        \* seat -> plan for a manifest: write | adopt | supersede
    dPub,         \* seat -> group-commit pipeline stage
    dRec,         \* seat -> output record to commit
    tmp,          \* seat -> staged temporary beside the output
    out,          \* seat -> the file at the final path
    outPrev,      \* seat -> durable view of the path before a pending rename
    disp,         \* seat -> output displaced by an exchange (WP0(d))
    dstRows,      \* committed output rows [seat, key, id, data]
    \* session and budgets
    run, sess, crashes, foreign, cfails,
    \* estate capture's typed reads (S2)
    sqlPhase, sqlLock, sqlSteps, sqlBackups, gitReads,
    \* history and ghost state for the properties
    reads,        \* history of source content reads
    srcOps,       \* every kind of source access performed
    rc,           \* seat -> content reads this session
    runReads,     \* seats read this session
    heldAtStart,  \* seats the destination held durably when the run began
    changedRun,   \* seats edited or tampered with during this run
    committedRun, \* seat -> its output's group commit returned this run
    everCommitted,\* [seat, key] pairs whose output row ever committed
    outc,         \* seat -> this run's outcome
    clobbered     \* bulkload replaced or removed a file it does not own

srcVars  == <<srcStat, edits, fresh>>
sVars    == <<sEnt, sRow, sCap, sPend, srcLedger, srcDone, ledgerLost>>
authVars == <<srcAuth, srcStore>>
dVars    == <<dEnt, dKey, dPlan, dPub, dRec, tmp, out, outPrev, disp, dstRows>>
runVars  == <<run, sess, crashes, foreign, cfails>>
estVars  == <<sqlPhase, sqlLock, sqlSteps, sqlBackups, gitReads>>
ghostVars == <<reads, srcOps, rc, runReads, heldAtStart, changedRun,
               committedRun, everCommitted, outc, clobbered>>
vars == <<srcVars, sVars, authVars, msg, dVars, runVars, estVars, ghostVars>>

-----------------------------------------------------------------------------
(* Helpers *)

\* Identities of outputs this store committed a row for, at path s.
OwnIds(s) == {r.id : r \in {x \in dstRows : x.seat = s}}

\* Store::output_matches(key, identity) with the current output's identity.
RowMatches(s, k) ==
    /\ out[s].pres
    /\ \E r \in dstRows : r.seat = s /\ r.key = k /\ r.id = out[s].id

\* The destination holds seat s at key k durably: a committed row vouches
\* for the output now at the path, whose data and name are durable.
DestHolds(s, k) ==
    /\ RowMatches(s, k)
    /\ out[s].dd /\ out[s].nd /\ out[s].data # GARBAGE

\* The physical fact R25 is about, independent of how bulkload names it: a
\* committed row recorded from stat version v of seat s (under any source
\* authority) vouches for the output now at the path, durably. A rerun that
\* lost the authority no longer finds the row, but the bytes are still held.
HeldPhys(s, v) ==
    /\ out[s].pres /\ out[s].dd /\ out[s].nd /\ out[s].data # GARBAGE
    /\ \E r \in dstRows : r.seat = s /\ r.key % KeyBase = v /\ r.id = out[s].id

\* The strict reading of "held durably" (OI-1002-Q33, which does not say
\* whether bytes with no row count as held): HeldPhys, or bulkload's own
\* output from a non-racy capture is durable at the path with the seat's
\* current bytes, whether or not a row records it. Equal to HeldPhys
\* unless TrackStrictHeld.
StrictHeld(s) ==
    \/ HeldPhys(s, sRow[s])
    \/ /\ out[s].pres /\ out[s].cl /\ out[s].dd /\ out[s].nd
       /\ out[s].data = edits[s]

\* Row keys (A/transfer_store.rs row_key over the authority from Start and
\* the walked row): the key the walk offers, and the key of the walked row.
WireKey(s) == srcAuth * KeyBase + srcStat[s]
SKey(s) == srcAuth * KeyBase + sRow[s]

LedgerRows(s, k) == {r \in srcLedger : r.seat = s /\ r.key = k}

\* No pipeline work in flight on either side: a new session may open the
\* store's exclusive publisher (StorePublisher::open, flock).
Quiescent ==
    /\ \A s \in Seats : dPub[s] \in {"none", "committed", "failed_occupied", "failed_space"}
    /\ sPend = {}

\* Extra source accesses a mutation performs alongside a content read.
MutOps == (IF Mutation = "source_write" THEN {"write"} ELSE {})
          \cup (IF Mutation = "pause_writer" THEN {"procctl"} ELSE {})

\* capture_file and serve_chunks check the seat's stat identity against the
\* walked row before reading any byte (SourceChangedAfterSnapshot).
CaptureOK(s) == srcStat[s] = sRow[s]

\* One content read of seat s, recorded in the history with what the
\* destination and the source ledger held at that moment.
DoRead(s, kind) ==
    /\ reads' = reads \cup {[run |-> run, seat |-> s, key |-> SKey(s),
                             kind |-> kind,
                             held |-> HeldPhys(s, sRow[s]),
                             strict |-> StrictHeld(s),
                             committed |-> /\ LedgerRows(s, SKey(s)) # {}
                                           /\ DestHolds(s, SKey(s))]}
    /\ rc' = [rc EXCEPT ![s] = @ + 1]
    /\ runReads' = runReads \cup {s}
    /\ srcOps' = srcOps \cup {"read"} \cup MutOps

\* The output a publish of seat s's staged capture creates in this run.
\* Its capture record: A/transfer.rs publish writes one on a non-racy
\* capture's staged file before its seal (A/transfer/unrowed.rs write_record).
NewOut(s, d, sealed) ==
    [pres |-> TRUE, id |-> run, data |-> d, dd |-> sealed, nd |-> FALSE,
     cl |-> TrackStrictHeld /\ ~dRec[s].racy,
     rec |-> IF AdoptUnrowed /\ ~dRec[s].racy
             THEN [key |-> RecordKey(dRec[s].key), data |-> d] ELSE NoRecord]

Durable(f) == IF f.pres THEN [f EXCEPT !.dd = TRUE, !.nd = TRUE] ELSE f

-----------------------------------------------------------------------------
Init ==
    /\ srcStat = [s \in Seats |-> 0]
    /\ edits = [s \in Seats |-> 0]
    /\ fresh \in [Seats -> BOOLEAN]       \* a seat may be newly written
    /\ sEnt = [s \in Seats |-> "idle"]
    /\ sRow = [s \in Seats |-> 0]
    /\ sCap = [s \in Seats |-> NoCap]
    /\ sPend = {}
    /\ srcLedger = {}
    /\ srcDone = FALSE
    /\ ledgerLost = {}
    /\ srcAuth = 0
    /\ srcStore = "none"
    /\ msg = [s \in Seats |-> NoMsg]
    /\ dEnt = [s \in Seats |-> "idle"]
    /\ dKey = [s \in Seats |-> 0]
    /\ dPlan = [s \in Seats |-> "none"]
    /\ dPub = [s \in Seats |-> "none"]
    /\ dRec = [s \in Seats |-> NoRec]
    /\ tmp = [s \in Seats |-> NoTmp]
    /\ out = [s \in Seats |-> NoFile]
    /\ outPrev = [s \in Seats |-> NoFile]
    /\ disp = [s \in Seats |-> NoFile]
    /\ dstRows = {}
    /\ run = 0 /\ sess = "idle" /\ crashes = 0 /\ foreign = 0 /\ cfails = 0
    /\ sqlPhase = "none" /\ sqlLock = "none" /\ sqlSteps = 0 /\ sqlBackups = 0
    /\ gitReads = 0
    /\ reads = {} /\ srcOps = {} /\ rc = [s \in Seats |-> 0]
    /\ runReads = {} /\ heldAtStart = {} /\ changedRun = {}
    /\ committedRun = [s \in Seats |-> FALSE]
    /\ everCommitted = {}
    /\ outc = [s \in Seats |-> "none"]
    /\ clobbered = FALSE

-----------------------------------------------------------------------------
(* Session start.                                                          *)
(* A/transfer.rs receive: Store::open, the exclusive publisher, sweep_root *)
(* (orphaned temporaries are salvaged for chunks and removed at finish),  *)
(* then Open; serve answers Start. WP0(d) exchange design: a file an       *)
(* interrupted exchange displaced is restored when it is not this store's  *)
(* own (A/materialize.rs Destination::settle_supersedes, which runs        *)
(* before the sweep; Mutation sweep_displaced: the sweep removes it like   *)
(* any tagged temporary, which is what the sweep before #187 would do).    *)

RestoredOut(s) ==
    IF /\ disp[s].pres /\ disp[s].id \notin OwnIds(s)
       /\ Mutation # "sweep_displaced"
       /\ out[s].pres /\ out[s].id < FirstForeignId /\ out[s].id \notin OwnIds(s)
    THEN Durable(disp[s])                  \* swap back over our uncommitted file
    ELSE out[s]

KeptDisp(s) ==
    \* A foreign displaced file that cannot be swapped back stays where it is,
    \* reported, never removed.
    IF /\ disp[s].pres /\ disp[s].id \notin OwnIds(s)
       /\ Mutation # "sweep_displaced"
       /\ RestoredOut(s) = out[s]
    THEN disp[s]
    ELSE NoFile

\* serve's Store::open creates the source store when it has none: one
\* transaction writes the schema and a fresh random authority (INSERT OR
\* IGNORE into settings). Under synchronous=FULL that commit is durable when
\* it returns; relaxed (RelaxedAuthority) it is not until a later sync. Its
\* durability also needs the state root's own directory entry, which
\* private_dir creates without sealing the parent: StoreRootSealed = FALSE
\* models the code as it is, where the store stays "volatile" (lost whole
\* or kept whole by the next crash). The destination store's root is
\* assumed durable (header, ABSTRACTIONS).
StartRun ==
    /\ run < MaxRuns
    /\ sess \in {"idle", "done", "broken"}
    /\ Quiescent
    /\ IF srcStore = "none"
       THEN /\ srcAuth' = srcAuth + 1
            /\ srcStore' = IF \/ RelaxedSourceLedger /\ RelaxedAuthority
                              \/ ~StoreRootSealed
                           THEN "volatile" ELSE "durable"
       ELSE UNCHANGED authVars
    /\ run' = run + 1
    /\ sess' = "on"
    /\ sEnt' = [s \in Seats |-> "unwalked"]
    /\ sCap' = [s \in Seats |-> NoCap]
    /\ srcDone' = FALSE
    /\ msg' = [s \in Seats |-> NoMsg]
    /\ dEnt' = [s \in Seats |-> "none"]
    /\ dPlan' = [s \in Seats |-> "none"]
    /\ dPub' = [s \in Seats |-> "none"]
    /\ dRec' = [s \in Seats |-> NoRec]
    /\ tmp' = [s \in Seats |-> NoTmp]
    /\ out' = [s \in Seats |-> RestoredOut(s)]
    /\ disp' = [s \in Seats |-> KeptDisp(s)]
    /\ clobbered' = \/ clobbered
                    \/ \E s \in Seats : /\ Mutation = "sweep_displaced"
                                        /\ disp[s].pres
                                        /\ disp[s].id \notin OwnIds(s)
    /\ heldAtStart' = {s \in Seats : HeldPhys(s, srcStat[s])}
    /\ runReads' = {}
    /\ changedRun' = {}
    /\ rc' = [s \in Seats |-> 0]
    /\ committedRun' = [s \in Seats |-> FALSE]
    /\ outc' = [s \in Seats |-> "pending"]
    /\ UNCHANGED <<srcVars, sRow, sPend, srcLedger, ledgerLost, dKey, outPrev,
                   dstRows, crashes, foreign, cfails, estVars, reads, srcOps,
                   everCommitted>>

-----------------------------------------------------------------------------
(* Source: walk one seat and offer it.                                     *)
(* A/transfer.rs walk_source, Outbound::walked, Outbound::offer            *)
(* (Control::Entry); A/walk.rs Walker. The walk is metadata only (stat).   *)
(* The Entry carries the row; the destination keys it with the authority   *)
(* from Start (A/transfer.rs receive; A/transfer_store.rs row_key),        *)
(* modelled as WireKey. The Entry's b flag reports whether the source      *)
(* ledger holds the row key; only Mutation src_ledger_carries_r25 lets the *)
(* destination read it.                                                    *)
Walk(s) ==
    /\ sess = "on" /\ sEnt[s] = "unwalked" /\ msg[s] = NoMsg
    /\ sEnt' = [sEnt EXCEPT ![s] = "offered"]
    /\ sRow' = [sRow EXCEPT ![s] = srcStat[s]]
    /\ msg' = [msg EXCEPT ![s] = Msg("entry", WireKey(s),
                                      LedgerRows(s, WireKey(s)) # {}, "none")]
    /\ srcOps' = srcOps \cup {"stat"}
    /\ UNCHANGED <<srcVars, authVars, sCap, sPend, srcLedger, srcDone, ledgerLost,
                   dVars, runVars,
                   estVars, reads, rc, runReads, heldAtStart, changedRun,
                   committedRun, everCommitted, outc, clobbered>>

(* Destination: decide one entry.                                          *)
(* A/transfer.rs Inbound::entry: Reuse iff the output at the path has the  *)
(* identity this store recorded under the row key (Destination::identity, *)
(* Store::output_matches). #169 (AdoptUnrowed): otherwise an existing      *)
(* output whose capture record names this entry's row and whose own bytes *)
(* hash to the record's root is adopted, also answered Reuse              *)
(* (Inbound::adopt_unrowed, A/transfer/unrowed.rs prove): it is queued as  *)
(* an adopted publication, sealed before its row commits, and the source   *)
(* reads nothing. Mutations adopt_unkeyed / adopt_unverified skip the key  *)
(* or the hash check. Otherwise WantManifest when an output exists; for an *)
(* absent output Send, or WantManifest when published outputs or salvaged  *)
(* temporaries could fill chunks (free choice here). The space preflight   *)
(* (Inbound::admit) may refuse DESTINATION_SPACE_INSUFFICIENT.             *)
(* Mutations reread_durable / reread_ignore_ledger: the destination never  *)
(* answers Reuse, by its row or by a capture record. Mutation              *)
(* src_ledger_carries_r25: either Reuse also needs the source ledger's     *)
(* row. Mutation reuse_ignores_row: only the row check is gone             *)
(* (Store::output_matches always false), so a rowed output is adopted      *)
(* again from its record, hashed, sealed and re-rowed on every run, with   *)
(* no source read to show it (AdoptOnlyUnrowed).                           *)
RecvEntry(s) ==
    /\ sess = "on" /\ dEnt[s] = "none" /\ msg[s].t = "entry"
    /\ LET k == msg[s].v
           reuse == /\ RowMatches(s, k)
                    /\ Mutation \notin {"reread_durable", "reread_ignore_ledger",
                                         "reuse_ignores_row"}
                    /\ (Mutation = "src_ledger_carries_r25" => msg[s].b)
           adopt == /\ AdoptUnrowed
                    /\ Mutation \notin {"reread_durable", "reread_ignore_ledger"}
                    /\ (Mutation = "src_ledger_carries_r25" => msg[s].b)
                    /\ out[s].pres /\ out[s].rec.key # 0
                    /\ (out[s].rec.key = RecordKey(k) \/ Mutation = "adopt_unkeyed")
                    /\ (out[s].data = out[s].rec.data \/ Mutation = "adopt_unverified")
           choices == IF reuse THEN {"reuse"}
                      ELSE IF adopt THEN {"adopt_unrowed"}
                      ELSE (IF out[s].pres THEN {"manifest"} ELSE {"send", "manifest"})
                           \cup (IF SpaceRefusals THEN {"refuse"} ELSE {})
       IN \E d \in choices :
            /\ msg' = [msg EXCEPT ![s] = Msg("decide", 0, FALSE,
                                             IF d = "adopt_unrowed" THEN "reuse" ELSE d)]
            /\ dKey' = [dKey EXCEPT ![s] = k]
            /\ dEnt' = [dEnt EXCEPT ![s] =
                          CASE d = "send" -> "streaming"
                            [] d = "manifest" -> "await_manifest"
                            [] d = "adopt_unrowed" -> "adopt_queued"
                            [] OTHER -> "done"]
            /\ outc' = [outc EXCEPT ![s] =
                          CASE d = "reuse" -> "applied"
                            [] d = "refuse" -> "DESTINATION_SPACE_INSUFFICIENT"
                            [] OTHER -> "pending"]
            /\ IF d = "adopt_unrowed"
               THEN /\ dPub' = [dPub EXCEPT ![s] = "adopt_wait"]
                    /\ dRec' = [dRec EXCEPT ![s] =
                                  [key |-> k,
                                   data |-> IF Mutation = "adopt_unverified"
                                            THEN out[s].rec.data ELSE out[s].data,
                                   racy |-> FALSE, id |-> out[s].id, kind |-> "adopt"]]
               ELSE UNCHANGED <<dPub, dRec>>
    /\ UNCHANGED <<srcVars, authVars, sVars, dPlan, tmp, out, outPrev, disp,
                   dstRows, runVars, estVars, reads, srcOps, rc, runReads,
                   heldAtStart, changedRun, committedRun, everCommitted,
                   clobbered>>

(* Source: act on a decision, with the capture it starts.                  *)
(* A/transfer.rs Outbound::decide; run_job; send_capture; manifest_capture *)
(* (ledger manifest without reading via Store::capture, else one read     *)
(* whose chunks are retained); capture_file (stat check before reading,   *)
(* racy per git_carry::racy). A racy capture is sent but never recorded.  *)
(* Mutation reread_ignore_ledger: manifest_capture ignores the ledger (and *)
(* RecvEntry never answers Reuse).                                         *)
RecvDecide(s) ==
    /\ sess = "on" /\ sEnt[s] = "offered" /\ msg[s].t = "decide"
    /\ LET c == msg[s].c
           racy == fresh[s]
           rec == ~racy \/ Mutation = "record_racy"
           row == [seat |-> s, key |-> SKey(s), data |-> edits[s]]
           useLedger == /\ LedgerRows(s, SKey(s)) # {}
                        /\ Mutation # "reread_ignore_ledger"
       IN
       CASE c \in {"reuse", "refuse"} ->
              \* Retired: nothing is read or sent.
              /\ sEnt' = [sEnt EXCEPT ![s] = "done"]
              /\ msg' = [msg EXCEPT ![s] = NoMsg]
              /\ UNCHANGED <<sCap, sPend, reads, rc, runReads, srcOps>>
         [] c = "send" /\ CaptureOK(s) ->
              /\ DoRead(s, "full")
              /\ msg' = [msg EXCEPT ![s] = Msg("end", edits[s], racy, "none")]
              /\ sEnt' = [sEnt EXCEPT ![s] = "await_held"]
              /\ sCap' = [sCap EXCEPT ![s] = [data |-> edits[s], racy |-> racy,
                                              rec |-> rec, led |-> FALSE]]
              /\ sPend' = IF Mutation = "ledger_before_held" /\ rec
                          THEN sPend \cup {row} ELSE sPend
         [] c = "manifest" /\ useLedger ->
              \* Manifest from the ledger: no source read.
              LET r == CHOOSE x \in LedgerRows(s, SKey(s)) : TRUE IN
              /\ msg' = [msg EXCEPT ![s] = Msg("manifest", r.data, FALSE, "none")]
              /\ sEnt' = [sEnt EXCEPT ![s] = "manifested"]
              /\ sCap' = [sCap EXCEPT ![s] = [data |-> r.data, racy |-> FALSE,
                                              rec |-> FALSE, led |-> TRUE]]
              /\ UNCHANGED <<sPend, reads, rc, runReads, srcOps>>
         [] c = "manifest" /\ ~useLedger /\ CaptureOK(s) ->
              /\ DoRead(s, "full")
              /\ msg' = [msg EXCEPT ![s] = Msg("manifest", edits[s], racy, "none")]
              /\ sEnt' = [sEnt EXCEPT ![s] = "manifested"]
              /\ sCap' = [sCap EXCEPT ![s] = [data |-> edits[s], racy |-> racy,
                                              rec |-> rec, led |-> FALSE]]
              /\ sPend' = IF Mutation = "ledger_before_held" /\ rec
                          THEN sPend \cup {row} ELSE sPend
         [] OTHER ->
              \* The seat moved since the walk: refused before any byte is read.
              /\ msg' = [msg EXCEPT ![s] = Msg("refused", 0, FALSE,
                                               "SOURCE_CHANGED_AFTER_SNAPSHOT")]
              /\ sEnt' = [sEnt EXCEPT ![s] = "done"]
              /\ UNCHANGED <<sCap, sPend, reads, rc, runReads, srcOps>>
    /\ UNCHANGED <<srcVars, authVars, sRow, srcLedger, srcDone, ledgerLost, dVars,
                   runVars, estVars, heldAtStart, changedRun, committedRun,
                   everCommitted, outc, clobbered>>

(* Destination: plan a manifest and ask only for what it cannot fill.     *)
(* A/transfer.rs Inbound::manifest, plan_file: an existing output is       *)
(* adopted (verified at End), an absent one staged. WP0(d) designs: an     *)
(* existing output whose identity is this store's own is superseded.       *)
RecvManifest(s) ==
    /\ sess = "on" /\ dEnt[s] = "await_manifest" /\ msg[s].t = "manifest"
    /\ LET plan == IF ~out[s].pres THEN "write"
                   ELSE IF /\ SupersedeMode # "off"
                           /\ ~disp[s].pres
                           /\ (out[s].id \in OwnIds(s) \/ Mutation = "supersede_unchecked")
                        THEN "supersede"
                        ELSE "adopt"
       IN /\ dPlan' = [dPlan EXCEPT ![s] = plan]
          /\ msg' = [msg EXCEPT ![s] = Msg("need", 0, plan # "adopt", "none")]
    /\ dEnt' = [dEnt EXCEPT ![s] = "filling"]
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPub, dRec, tmp, out, outPrev, disp,
                   dstRows, runVars, estVars, ghostVars>>

(* Source: serve the requested chunks, then End.                           *)
(* A/transfer.rs Outbound::need_chunks, serve_chunks: retained chunks are  *)
(* served from memory; chunks of a ledger manifest are re-read with pread  *)
(* under an unchanged stat identity and re-verified against their digest.  *)
(* Mutation double_read drops the retained chunks of a fresh manifest, so  *)
(* they are read again (the #77 review F1 defect).                         *)
RecvNeed(s) ==
    /\ sess = "on" /\ sEnt[s] = "manifested" /\ msg[s].t = "need"
    /\ IF msg[s].b /\ (sCap[s].led \/ Mutation = "double_read")
       THEN (CASE CaptureOK(s) /\ edits[s] = sCap[s].data ->
                   /\ DoRead(s, "chunks")
                   /\ msg' = [msg EXCEPT ![s] = Msg("end", sCap[s].data, sCap[s].racy, "none")]
                   /\ sEnt' = [sEnt EXCEPT ![s] = "await_held"]
              [] CaptureOK(s) /\ edits[s] # sCap[s].data ->
                   \* Read, then the digest check fails.
                   /\ DoRead(s, "chunks")
                   /\ msg' = [msg EXCEPT ![s] = Msg("refused", 0, FALSE,
                                                    "SOURCE_CHANGED_AFTER_SNAPSHOT")]
                   /\ sEnt' = [sEnt EXCEPT ![s] = "done"]
              [] OTHER ->
                   /\ msg' = [msg EXCEPT ![s] = Msg("refused", 0, FALSE,
                                                    "SOURCE_CHANGED_AFTER_SNAPSHOT")]
                   /\ sEnt' = [sEnt EXCEPT ![s] = "done"]
                   /\ UNCHANGED <<reads, rc, runReads, srcOps>>)
       ELSE /\ msg' = [msg EXCEPT ![s] = Msg("end", sCap[s].data, sCap[s].racy, "none")]
            /\ sEnt' = [sEnt EXCEPT ![s] = "await_held"]
            /\ UNCHANGED <<reads, rc, runReads, srcOps>>
    /\ UNCHANGED <<srcVars, authVars, sRow, sCap, sPend, srcLedger, srcDone, ledgerLost,
                   dVars, runVars, estVars, heldAtStart, changedRun, committedRun,
                   everCommitted, outc, clobbered>>

(* Destination: an entry's data is complete.                               *)
(* A/transfer.rs Inbound::end, end_streaming, end_filling: a written or    *)
(* superseding output is staged and queued for its group commit (publish); *)
(* an adopted one is verified byte for byte (verify_existing) and queued,  *)
(* or refused DESTINATION_OCCUPIED with Held{false} at once. #169: a       *)
(* verified output of a non-racy capture gets that capture's record before *)
(* it is queued (Inbound::adopt, A/transfer/unrowed.rs refresh), as a      *)
(* staged file does in NewOut, so an adoption whose row never commits is   *)
(* adopted from it next run. A power loss before the adoption's seal may   *)
(* lose the record; that leaves the state a crash before this step leaves. *)
(* Mutation adopt_unrecorded: no record is written here (the code before   *)
(* the #169 review).                                                       *)
RecvEnd(s) ==
    /\ sess = "on" /\ msg[s].t = "end" /\ dEnt[s] \in {"streaming", "filling"}
    /\ LET d == msg[s].v
           racy == msg[s].b
           plan == IF dEnt[s] = "streaming" THEN "write" ELSE dPlan[s]
       IN
       CASE plan \in {"write", "supersede"} ->
              /\ tmp' = [tmp EXCEPT ![s] = [st |-> "written", data |-> d]]
              /\ dPub' = [dPub EXCEPT ![s] = "staged"]
              /\ dRec' = [dRec EXCEPT ![s] = [key |-> dKey[s], data |-> d,
                                              racy |-> racy, id |-> 0, kind |-> plan]]
              /\ dEnt' = [dEnt EXCEPT ![s] = "queued"]
              /\ msg' = [msg EXCEPT ![s] = NoMsg]
              /\ UNCHANGED <<out, outc>>
         [] plan = "adopt" /\ out[s].pres /\ out[s].data = d ->
              /\ dPub' = [dPub EXCEPT ![s] = "adopt_wait"]
              /\ dRec' = [dRec EXCEPT ![s] = [key |-> dKey[s], data |-> d,
                                              racy |-> racy, id |-> out[s].id,
                                              kind |-> "adopt"]]
              /\ out' = [out EXCEPT ![s].rec =
                           IF AdoptUnrowed /\ ~racy /\ Mutation # "adopt_unrecorded"
                           THEN [key |-> RecordKey(dKey[s]), data |-> d] ELSE @]
              /\ dEnt' = [dEnt EXCEPT ![s] = "queued"]
              /\ msg' = [msg EXCEPT ![s] = NoMsg]
              /\ UNCHANGED <<tmp, outc>>
         [] OTHER ->
              /\ msg' = [msg EXCEPT ![s] = Msg("held", 0, FALSE, "none")]
              /\ dEnt' = [dEnt EXCEPT ![s] = "done"]
              /\ outc' = [outc EXCEPT ![s] = "DESTINATION_OCCUPIED"]
              /\ UNCHANGED <<tmp, dPub, dRec, out>>
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPlan, outPrev, disp, dstRows,
                   runVars, estVars, reads, srcOps, rc, runReads, heldAtStart,
                   changedRun, committedRun, everCommitted, clobbered>>

(* Destination: the source refused an entry's capture.                     *)
(* A/transfer.rs Inbound::refused (staged bytes discarded).                *)
RecvRefused(s) ==
    /\ sess = "on" /\ msg[s].t = "refused"
    /\ dEnt[s] \in {"streaming", "await_manifest", "filling"}
    /\ dEnt' = [dEnt EXCEPT ![s] = "done"]
    /\ outc' = [outc EXCEPT ![s] = msg[s].c]
    /\ msg' = [msg EXCEPT ![s] = NoMsg]
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPlan, dPub, dRec, tmp, out, outPrev,
                   disp, dstRows, runVars, estVars, reads, srcOps, rc, runReads,
                   heldAtStart, changedRun, committedRun, everCommitted,
                   clobbered>>

-----------------------------------------------------------------------------
(* Destination group-commit pipeline (A/io/durable.rs Committer,           *)
(* A/materialize.rs PublishSink::commit). These steps also run after the  *)
(* session broke: dropping the Committer commits what is pending.          *)

PipeUnchanged == <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dstRows, runVars,
                   estVars, reads, srcOps, rc, runReads, heldAtStart,
                   changedRun, committedRun, everCommitted, outc>>

(* A/materialize.rs StagedFile::seal -> io/durable.rs seal_file.           *)
SealTemp(s) ==
    /\ dPub[s] = "staged"
    /\ tmp' = [tmp EXCEPT ![s].st = "sealed"]
    /\ dPub' = [dPub EXCEPT ![s] = "sealed"]
    /\ UNCHANGED <<PipeUnchanged, dRec, out, outPrev, disp, cfails, clobbered>>

(* A/materialize.rs StagedFile::publish: io::publish_noreplace; an        *)
(* occupied leaf refuses DESTINATION_OCCUPIED and the temporary goes.      *)
(* Mutation commit_before_fsync renames a temporary that was never sealed. *)
Publish(s) ==
    /\ dRec[s].kind = "write"
    /\ \/ dPub[s] = "sealed"
       \/ Mutation = "commit_before_fsync" /\ dPub[s] = "staged"
    /\ IF out[s].pres
       THEN /\ dPub' = [dPub EXCEPT ![s] = "failed_occupied"]
            /\ UNCHANGED <<out, outPrev, dRec>>
       ELSE /\ out' = [out EXCEPT ![s] = NewOut(s, tmp[s].data, tmp[s].st = "sealed")]
            /\ outPrev' = [outPrev EXCEPT ![s] = out[s]]
            /\ dRec' = [dRec EXCEPT ![s].id = run]
            /\ dPub' = [dPub EXCEPT ![s] = "renamed"]
    /\ tmp' = [tmp EXCEPT ![s] = NoTmp]
    /\ UNCHANGED <<PipeUnchanged, disp, cfails, clobbered>>

(* A/materialize.rs TouchedDevices::seal -> io/durable.rs seal_dir: the    *)
(* rename (and any exchange in the same directory) becomes durable.        *)
DirSeal(s) ==
    /\ dPub[s] = "renamed"
    /\ out' = [out EXCEPT ![s] = IF out[s].pres /\ out[s].id = dRec[s].id
                                 THEN [out[s] EXCEPT !.nd = TRUE] ELSE out[s]]
    /\ disp' = [disp EXCEPT ![s] = IF disp[s].pres THEN [disp[s] EXCEPT !.nd = TRUE]
                                   ELSE disp[s]]
    /\ dPub' = [dPub EXCEPT ![s] = "ready"]
    /\ UNCHANGED <<PipeUnchanged, dRec, tmp, outPrev, cfails, clobbered>>

(* A/materialize.rs PublishSink::commit, Publication::Adopted: seal_file  *)
(* on the verified descriptor, then its directory is sealed with the group.*)
(* From here the destination holds a non-racy capture's verified bytes     *)
(* durably, row or no row (ghost cl, under TrackStrictHeld).               *)
SealAdopted(s) ==
    /\ dPub[s] = "adopt_wait"
    /\ out' = [out EXCEPT ![s] =
                 IF /\ out[s].pres /\ out[s].id = dRec[s].id
                    /\ Mutation # "adopt_without_seal"
                 THEN [Durable(out[s]) EXCEPT
                         !.cl = @ \/ (TrackStrictHeld /\ ~dRec[s].racy)]
                 ELSE out[s]]
    /\ dPub' = [dPub EXCEPT ![s] = "ready"]
    /\ UNCHANGED <<PipeUnchanged, dRec, tmp, outPrev, disp, cfails, clobbered>>

(* WP0(d) design "check_rename": check the identity, then rename over it.  *)
(* A third-party write can land between the two steps (TOCTOU).            *)
CheckOwn(s) ==
    /\ SupersedeMode = "check_rename" /\ dRec[s].kind = "supersede"
    /\ dPub[s] = "sealed"
    /\ dPub' = [dPub EXCEPT ![s] =
                  IF ~out[s].pres \/ out[s].id \in OwnIds(s)
                     \/ Mutation = "supersede_unchecked"
                  THEN "checked" ELSE "failed_occupied"]
    /\ UNCHANGED <<PipeUnchanged, dRec, tmp, out, outPrev, disp, cfails, clobbered>>

RenameReplace(s) ==
    /\ dPub[s] = "checked"
    /\ clobbered' = (clobbered \/ (out[s].pres /\ out[s].id \notin OwnIds(s)))
    /\ out' = [out EXCEPT ![s] = NewOut(s, tmp[s].data, TRUE)]
    /\ outPrev' = [outPrev EXCEPT ![s] = out[s]]
    /\ dRec' = [dRec EXCEPT ![s].id = run]
    /\ tmp' = [tmp EXCEPT ![s] = NoTmp]
    /\ dPub' = [dPub EXCEPT ![s] = "renamed"]
    /\ UNCHANGED <<PipeUnchanged, disp, cfails>>

(* WP0(d) design "exchange", the code since #187: RENAME_EXCHANGE          *)
(* (renameat2 / renameatx_np RENAME_SWAP) puts the new file in place and   *)
(* the old one under a displaced name; the displaced identity is then      *)
(* checked against this store's rows, and a foreign file is swapped        *)
(* back. In the code a store commit comes first (begin_supersedes): it     *)
(* records the intent and moves the output's rows out of the outputs       *)
(* table into it, and the intent, not the rows, then says which identity   *)
(* is this store's own (VerifyDisp, and StartRun's restore). An intent     *)
(* whose exchange did not take effect is settled by putting the rows       *)
(* back. Here the rows simply stay: a row binds only an output with its    *)
(* identity (RowMatches), and the seat's stat version has moved past the   *)
(* old row's, so no property reads the difference. The code also drops     *)
(* the old rows when the new row commits; here they stay as OwnIds of a    *)
(* file that no longer exists. An owned output whose bytes already equal   *)
(* the manifest's is adopted in place by the code (plan_file), where       *)
(* this model exchanges it for an equal file.                              *)
Exchange(s) ==
    /\ SupersedeMode = "exchange" /\ dRec[s].kind = "supersede"
    /\ dPub[s] = "sealed"
    /\ out' = [out EXCEPT ![s] = NewOut(s, tmp[s].data, TRUE)]
    /\ outPrev' = [outPrev EXCEPT ![s] = out[s]]
    /\ disp' = [disp EXCEPT ![s] = IF out[s].pres THEN [out[s] EXCEPT !.nd = FALSE]
                                   ELSE NoFile]
    /\ dRec' = [dRec EXCEPT ![s].id = run]
    /\ tmp' = [tmp EXCEPT ![s] = NoTmp]
    /\ dPub' = [dPub EXCEPT ![s] = IF out[s].pres THEN "exchanged" ELSE "renamed"]
    /\ UNCHANGED <<PipeUnchanged, cfails, clobbered>>

VerifyDisp(s) ==
    /\ dPub[s] = "exchanged"
    /\ IF disp[s].id \in OwnIds(s) \/ Mutation = "supersede_unchecked"
       THEN \* the displaced output is this store's own: remove it
            /\ clobbered' = (clobbered \/ disp[s].id \notin OwnIds(s))
            /\ disp' = [disp EXCEPT ![s] = NoFile]
            /\ dPub' = [dPub EXCEPT ![s] = "renamed"]
            /\ UNCHANGED <<out, outPrev>>
       ELSE \* foreign: exchange back and seal, if our file is still in place
            /\ IF out[s].pres /\ out[s].id = dRec[s].id
               THEN /\ out' = [out EXCEPT ![s] = Durable(disp[s])]
                    /\ disp' = [disp EXCEPT ![s] = NoFile]
               ELSE UNCHANGED <<out, disp>>   \* kept aside, reported
            /\ outPrev' = [outPrev EXCEPT ![s] = NoFile]
            /\ dPub' = [dPub EXCEPT ![s] = "failed_occupied"]
            /\ UNCHANGED clobbered
    /\ UNCHANGED <<PipeUnchanged, dRec, tmp, cfails>>

(* A/transfer_store.rs StorePublisher::commit_outputs: one SQLite           *)
(* transaction (synchronous=FULL, fullfsync=ON) for a group: insert each    *)
(* row, or for a racy capture delete any row under its key (#86). It runs  *)
(* only after every file and directory of the group is sealed. Mutations  *)
(* commit_before_fsync / commit_before_dirseal let it run earlier.          *)
Ready(s) ==
    \/ dPub[s] = "ready"
    \/ Mutation = "commit_before_dirseal" /\ dPub[s] = "renamed"

\* Mutation skip_output_row: the store commit records no output row.
Recorded(G) == IF Mutation = "skip_output_row" THEN {}
               ELSE {x \in G : ~dRec[x].racy \/ Mutation = "record_racy"}

Commit ==
    \E G \in SUBSET Seats :
       /\ G # {}
       /\ \A s \in G : Ready(s)
       /\ dstRows' = {r \in dstRows : ~\E s \in G : r.seat = s /\ r.key = dRec[s].key}
                     \cup {[seat |-> s, key |-> dRec[s].key, id |-> dRec[s].id,
                            data |-> dRec[s].data] : s \in Recorded(G)}
       /\ dPub' = [s \in Seats |-> IF s \in G THEN "committed" ELSE dPub[s]]
       /\ committedRun' = [s \in Seats |-> committedRun[s] \/ s \in G]
       /\ everCommitted' = everCommitted
                           \cup {[seat |-> s, key |-> dRec[s].key] : s \in Recorded(G)}
       /\ UNCHANGED <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dRec, tmp, out,
                      outPrev, disp, runVars, estVars, reads, srcOps, rc,
                      runReads, heldAtStart, changedRun, outc, clobbered>>

(* A group whose seal or store commit fails (a full disk) is refused       *)
(* DESTINATION_SPACE_INSUFFICIENT and records nothing (#100,               *)
(* A/materialize.rs space_refusal). Its files may already be published.    *)
CommitFail ==
    /\ cfails < MaxCommitFails
    /\ \E G \in SUBSET Seats :
         /\ G # {}
         /\ \A s \in G : Ready(s)
         /\ dPub' = [s \in Seats |-> IF s \in G THEN "failed_space" ELSE dPub[s]]
    /\ cfails' = cfails + 1
    /\ UNCHANGED <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dRec, tmp, out,
                   outPrev, disp, dstRows, run, sess, crashes, foreign, estVars,
                   ghostVars>>

(* A/transfer.rs Inbound::answer_held / settle_held: Held{true} only once   *)
(* the output's group commit has returned; Held{false} for a failure.      *)
(* Mutation held_before_commit answers as soon as the output is queued.    *)
(* #169: an adopted unrowed output was answered Reuse, so no Held is sent; *)
(* its group's outcome reaches the session report (finish_receive):        *)
(* completed, or its typed refusal.                                        *)
AnswerHeld(s) ==
    /\ sess = "on" /\ dEnt[s] \in {"queued", "adopt_queued"} /\ msg[s] = NoMsg
    /\ \/ /\ dEnt[s] = "adopt_queued"
          /\ dPub[s] \in {"committed", "failed_space"}
          /\ UNCHANGED msg
          /\ outc' = [outc EXCEPT ![s] =
                        CASE dPub[s] = "committed" -> "applied"
                          [] Mutation = "untyped_space" -> "IO"
                          [] OTHER -> "DESTINATION_SPACE_INSUFFICIENT"]
       \/ /\ dEnt[s] = "queued"
          /\ dPub[s] = "committed"
          /\ msg' = [msg EXCEPT ![s] = Msg("held", 0, TRUE, "none")]
          \* applied; "applied_racy" marks a racy capture (no row kept), a
          \* ghost distinction the closure report does not make.
          /\ outc' = [outc EXCEPT ![s] = IF dRec[s].racy THEN "applied_racy"
                                         ELSE "applied"]
       \/ /\ dEnt[s] = "queued"
          /\ dPub[s] \in {"failed_occupied", "failed_space"}
          /\ msg' = [msg EXCEPT ![s] = Msg("held", 0, FALSE, "none")]
          \* materialize.rs space_refusal types ENOSPC (#100); Mutation
          \* untyped_space leaves it a bare IO, which closes nothing (S4).
          /\ outc' = [outc EXCEPT ![s] =
                        CASE dPub[s] = "failed_space" /\ Mutation = "untyped_space" -> "IO"
                          [] dPub[s] = "failed_space" -> "DESTINATION_SPACE_INSUFFICIENT"
                          [] OTHER -> "DESTINATION_OCCUPIED"]
       \/ /\ dEnt[s] = "queued" /\ Mutation = "held_before_commit"
          /\ dPub[s] \in {"staged", "sealed", "renamed", "ready", "adopt_wait"}
          /\ msg' = [msg EXCEPT ![s] = Msg("held", 0, TRUE, "none")]
          /\ outc' = [outc EXCEPT ![s] = "applied"]
    /\ dEnt' = [dEnt EXCEPT ![s] = "done"]
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPlan, dPub, dRec, tmp, out, outPrev,
                   disp, dstRows, runVars, estVars, reads, srcOps, rc, runReads,
                   heldAtStart, changedRun, committedRun, everCommitted,
                   clobbered>>

-----------------------------------------------------------------------------
(* Source: Held, the ledger and SourceDone.                                *)

(* A/transfer.rs Outbound::handle, Event::Held: a capture is submitted to  *)
(* the ledger only on Held{true}, and only one not already in the ledger   *)
(* (not from a ledger manifest) and not racy.                              *)
RecvHeld(s) ==
    /\ sess = "on" /\ sEnt[s] = "await_held" /\ msg[s].t = "held"
    /\ sPend' = IF /\ msg[s].b /\ sCap[s].rec
                   /\ Mutation # "ledger_before_held"
                THEN sPend \cup {[seat |-> s, key |-> SKey(s), data |-> sCap[s].data]}
                ELSE sPend
    /\ sEnt' = [sEnt EXCEPT ![s] = "done"]
    /\ msg' = [msg EXCEPT ![s] = NoMsg]
    /\ UNCHANGED <<srcVars, authVars, sRow, sCap, srcLedger, srcDone, ledgerLost,
                   dVars, runVars, estVars, ghostVars>>

(* A/transfer_store.rs LedgerSink::publish -> commit_captures: one        *)
(* transaction per group. Also runs after the session broke (Committer     *)
(* drop). Durable at commit unless RelaxedSourceLedger (see CrashSrc).     *)
(* It never fails here. In the code a failed group (a full or failing     *)
(* source state disk) is sticky: LedgerSink drops every later capture,    *)
(* and Committer::submit / sync then fail the session before SourceDone.  *)
(* So a lost ledger write costs the session, not one re-read (README,     *)
(* "Not proven here" and the WP0(g) conditions).                          *)
LedgerCommit ==
    /\ sPend # {}
    /\ srcLedger' = {r \in srcLedger : ~\E p \in sPend : p.seat = r.seat /\ p.key = r.key}
                    \cup sPend
    /\ sPend' = {}
    /\ UNCHANGED <<srcVars, authVars, sEnt, sRow, sCap, srcDone, ledgerLost, msg, dVars,
                   runVars, estVars, ghostVars>>

(* A/transfer.rs serve: committer.sync() returns before SourceDone.        *)
(* Mutation done_before_sync sends it with captures still pending.         *)
SendSourceDone ==
    /\ sess = "on" /\ ~srcDone
    /\ \A s \in Seats : sEnt[s] = "done"
    /\ (sPend = {} \/ Mutation = "done_before_sync")
    /\ srcDone' = TRUE
    /\ UNCHANGED <<srcVars, authVars, sEnt, sRow, sCap, sPend, srcLedger, ledgerLost,
                   msg, dVars, runVars, estVars, ghostVars>>

(* A/transfer.rs Inbound::run (SourceDone once nothing is incoming) and    *)
(* finish_receive (committer.finish, remove_salvaged).                      *)
Finish ==
    /\ sess = "on" /\ srcDone
    /\ \A s \in Seats : dEnt[s] = "done" /\ msg[s] = NoMsg
    /\ \A s \in Seats : dPub[s] \in {"none", "committed", "failed_occupied", "failed_space"}
    /\ sess' = "done"
    /\ tmp' = [s \in Seats |-> NoTmp]
    /\ UNCHANGED <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dPub, dRec, out,
                   outPrev, disp, dstRows, run, crashes, foreign, cfails,
                   estVars, ghostVars>>

-----------------------------------------------------------------------------
(* Estate capture's typed source reads (S2, WP0(b)).                       *)

(* A/git_carry.rs git, built from the one git_env table: the flag          *)
(* --no-optional-locks and GIT_OPTIONAL_LOCKS=0, GIT_NO_LAZY_FETCH=1, and  *)
(* -c core.hooksPath=/dev/null, core.fsmonitor=false, gc.auto=0,           *)
(* maintenance.auto=false. Mutation git_optional_locks drops the optional- *)
(* locks guard: a status refresh takes index.lock and rewrites the index.  *)
GitRead ==
    /\ EstateReads /\ gitReads < 1
    /\ gitReads' = gitReads + 1
    /\ srcOps' = srcOps \cup {"git_read"}
                 \cup (IF Mutation = "git_optional_locks" THEN {"lock", "write"} ELSE {})
    /\ UNCHANGED <<srcVars, authVars, sVars, msg, dVars, runVars, sqlPhase, sqlLock, sqlSteps,
                   sqlBackups, reads, rc, runReads, heldAtStart, changedRun,
                   committedRun, everCommitted, outc, clobbered>>

(* A/provider_sqlite.rs snapshot: the source is opened                    *)
(* SQLITE_OPEN_READ_ONLY with busy_timeout 0, and backup.step(128) runs at *)
(* most max_steps times. sqlite3_backup_step takes the source's shared     *)
(* read lock and releases it before it returns, so the lock is held only   *)
(* inside one step. That lock is the one stated exception to "no locks"    *)
(* (WP0(b), OI-1003-Q16): shared, bounded in steps, and counted.           *)
BackupFrame == <<srcVars, authVars, sVars, msg, dVars, runVars, gitReads>>

BackupBegin ==
    /\ EstateReads /\ sqlPhase = "none" /\ sqlBackups < 1
    /\ sqlPhase' = "open"
    /\ sqlBackups' = sqlBackups + 1
    /\ sqlSteps' = 0
    /\ srcOps' = srcOps \cup {"sqlite_backup"}
    /\ UNCHANGED <<BackupFrame, sqlLock, reads, rc, runReads, heldAtStart,
                   changedRun, committedRun, everCommitted, outc, clobbered>>

\* One backup.step(128): the shared read lock is taken ...
BackupStepLock ==
    /\ sqlPhase = "open" /\ sqlLock = "none"
    /\ \/ sqlSteps < MaxBackupSteps
       \/ Mutation = "unbounded_backup" /\ sqlSteps < MaxBackupSteps + 1
    /\ sqlLock' = "shared"
    /\ UNCHANGED <<BackupFrame, sqlPhase, sqlSteps, sqlBackups, ghostVars>>

\* ... and released before the step returns, whatever it returned.
BackupStepUnlock ==
    /\ sqlLock = "shared"
    /\ sqlLock' = "none"
    /\ sqlSteps' = sqlSteps + 1
    /\ UNCHANGED <<BackupFrame, sqlPhase, sqlBackups, ghostVars>>

\* Done, a busy writer (SqliteStateChanged) or the step budget
\* (BudgetExceeded): the backup ends between steps, holding no lock.
BackupEnd ==
    /\ sqlPhase = "open" /\ sqlLock = "none"
    /\ sqlPhase' = "done"
    /\ UNCHANGED <<BackupFrame, sqlLock, sqlSteps, sqlBackups, ghostVars>>

-----------------------------------------------------------------------------
(* Environment: live writers on the source, third parties at the          *)
(* destination, time, and faults.                                          *)

EnvUnchanged == <<sVars, authVars, msg, dVars, runVars, estVars, reads, srcOps, rc,
                  runReads, heldAtStart, committedRun, everCommitted, outc,
                  clobbered>>

\* A source write: new content, new stat identity, stamped "now".
Edit(s) ==
    /\ edits[s] < MaxEdits
    /\ srcStat' = [srcStat EXCEPT ![s] = @ + 1]
    /\ edits' = [edits EXCEPT ![s] = @ + 1]
    /\ fresh' = [fresh EXCEPT ![s] = TRUE]
    /\ changedRun' = changedRun \cup {s}
    /\ UNCHANGED EnvUnchanged

\* A same-size rewrite within one timestamp tick keeps the stat identity
\* (Git's racy index). Possible only while the seat is fresh.
SilentRewrite(s) ==
    /\ fresh[s] /\ edits[s] < MaxEdits
    /\ edits' = [edits EXCEPT ![s] = @ + 1]
    /\ changedRun' = changedRun \cup {s}
    /\ UNCHANGED <<srcStat, fresh, EnvUnchanged>>

\* Time passes beyond the 2 s racy allowance (RACY_GRANULARITY_NS).
Tick ==
    /\ \E s \in Seats : fresh[s]
    /\ fresh' = [s \in Seats |-> FALSE]
    /\ UNCHANGED <<srcStat, edits, changedRun, EnvUnchanged>>

DstUnchanged == <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dPub, dRec, tmp,
                  dstRows, run, sess, crashes, cfails, estVars, reads, srcOps,
                  rc, runReads, heldAtStart, committedRun, everCommitted, outc,
                  clobbered>>

\* A third party writes the destination path: a copy of the source's bytes
\* or other bytes, under a new identity. Its data is not yet durable; the
\* rename that put it there is (namespace operations in one directory are
\* ordered, so an earlier pending rename there is durable too). Under
\* AdoptUnrowed it may instead rewrite an existing file in place, which keeps
\* the file's capture record (an extended attribute) over other bytes.
ForeignWrite(s) ==
    /\ foreign < MaxForeign
    /\ \E d \in {FOREIGN, edits[s]},
         rec \in IF AdoptUnrowed /\ out[s].pres THEN {NoRecord, out[s].rec}
                 ELSE {NoRecord} :
         out' = [out EXCEPT ![s] = [pres |-> TRUE, id |-> FirstForeignId + foreign + 1,
                                    data |-> d, dd |-> FALSE, nd |-> TRUE,
                                    cl |-> FALSE, rec |-> rec]]
    /\ disp' = [disp EXCEPT ![s] = Durable(disp[s])]
    /\ outPrev' = [outPrev EXCEPT ![s] = NoFile]
    /\ foreign' = foreign + 1
    /\ changedRun' = changedRun \cup {s}
    /\ UNCHANGED DstUnchanged

ForeignDelete(s) ==
    /\ foreign < MaxForeign /\ out[s].pres
    /\ out' = [out EXCEPT ![s] = NoFile]
    /\ disp' = [disp EXCEPT ![s] = Durable(disp[s])]
    /\ outPrev' = [outPrev EXCEPT ![s] = NoFile]
    /\ foreign' = foreign + 1
    /\ changedRun' = changedRun \cup {s}
    /\ UNCHANGED DstUnchanged

(* Power loss at the destination (A/io/crash_check.rs persistence model):  *)
(* a rename not yet sealed by its directory may be lost (revert to the     *)
(* durable view) or kept; data not sealed may be garbage. Temporaries are  *)
(* swept by the next session; the receiver's pipeline is lost. The         *)
(* destination store is durable at commit (synchronous=FULL, fullfsync=ON).*)
CrashChoice(s) ==
    {c \in {"persist", "garbage", "revert"} :
        /\ (c = "garbage" => out[s].pres /\ ~out[s].dd)
        /\ (c = "revert" => out[s].pres /\ ~out[s].nd)}

OutAfter(c, s) ==
    CASE c = "revert" -> Durable(outPrev[s])
      [] c = "garbage" -> [out[s] EXCEPT !.data = GARBAGE, !.dd = TRUE, !.nd = TRUE]
      [] OTHER -> Durable(out[s])

DispAfter(c, s) == IF c = "revert" THEN NoFile ELSE Durable(disp[s])

DstLoss(f) ==
    /\ out' = [s \in Seats |-> OutAfter(f[s], s)]
    /\ disp' = [s \in Seats |-> DispAfter(f[s], s)]
    /\ outPrev' = [s \in Seats |-> NoFile]
    /\ tmp' = [s \in Seats |-> NoTmp]
    /\ dPub' = [s \in Seats |-> "none"]
    /\ dEnt' = [s \in Seats |-> "idle"]
    /\ dPlan' = [s \in Seats |-> "none"]
    /\ dRec' = [s \in Seats |-> NoRec]

\* The source host's power loss: the process and its pending ledger items
\* are gone; a relaxed ledger may lose any of its committed rows. A store
\* whose creation (its commit of schema and authority, or its state root's
\* directory entry) was never synced may be lost whole, with every row after
\* it; the next Store::open then makes a new authority, so every row key
\* either side holds is a different key. ledgerLost records what was lost.
SrcLoss ==
    /\ sPend' = {}
    /\ IF srcStore = "volatile"
       THEN \/ /\ srcStore' = "none" /\ srcLedger' = {}
            \/ /\ srcStore' = "durable"
               /\ srcLedger' \in IF RelaxedSourceLedger THEN SUBSET srcLedger
                                ELSE {srcLedger}
       ELSE /\ UNCHANGED srcStore
            /\ IF RelaxedSourceLedger
               THEN srcLedger' \in SUBSET srcLedger
               ELSE UNCHANGED srcLedger
    /\ ledgerLost' = ledgerLost \cup {[seat |-> r.seat, key |-> r.key, run |-> run] :
                                        r \in srcLedger \ srcLedger'}
    /\ UNCHANGED srcAuth

ResetSourceSession ==
    /\ sEnt' = [s \in Seats |-> "idle"]
    /\ sCap' = [s \in Seats |-> NoCap]
    /\ srcDone' = FALSE

CanCrash == crashes < MaxCrashes /\ run > 0

\* The source dies: the receiver sees the stream end, returns an error, and
\* its dropped Committer still commits the pending group (no Held is sent).
CrashSrc ==
    /\ CanCrash
    /\ SrcLoss
    /\ ResetSourceSession
    /\ msg' = [s \in Seats |-> NoMsg]
    /\ dEnt' = [s \in Seats |-> "idle"]
    /\ sess' = IF sess = "on" THEN "broken" ELSE sess
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<srcVars, sRow, dKey, dPlan, dPub, dRec, tmp, out, outPrev,
                   disp, dstRows, run, foreign, cfails, estVars, ghostVars>>

\* The destination dies: the source sees PeerFailed, and its dropped ledger
\* Committer still commits what was Held{true} (LedgerCommit stays enabled).
CrashDst ==
    /\ CanCrash
    /\ \E f \in [Seats -> {"persist", "garbage", "revert"}] :
         /\ \A s \in Seats : f[s] \in CrashChoice(s)
         /\ DstLoss(f)
    /\ ResetSourceSession
    /\ msg' = [s \in Seats |-> NoMsg]
    /\ sess' = IF sess = "on" THEN "broken" ELSE sess
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<srcVars, authVars, sRow, sPend, srcLedger, ledgerLost, dKey, dstRows,
                   run, foreign, cfails, estVars, ghostVars>>

\* One host runs both ends (loopback copy) and loses power.
CrashBoth ==
    /\ CanCrash
    /\ \E f \in [Seats -> {"persist", "garbage", "revert"}] :
         /\ \A s \in Seats : f[s] \in CrashChoice(s)
         /\ DstLoss(f)
    /\ SrcLoss
    /\ ResetSourceSession
    /\ msg' = [s \in Seats |-> NoMsg]
    /\ sess' = IF sess = "on" THEN "broken" ELSE sess
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<srcVars, sRow, dKey, dstRows, run, foreign, cfails, estVars,
                   ghostVars>>

-----------------------------------------------------------------------------
SeatStep(s) ==
    \/ Walk(s) \/ RecvEntry(s) \/ RecvDecide(s) \/ RecvManifest(s)
    \/ RecvNeed(s) \/ RecvEnd(s) \/ RecvRefused(s) \/ AnswerHeld(s)
    \/ RecvHeld(s) \/ SealTemp(s) \/ Publish(s) \/ DirSeal(s)
    \/ SealAdopted(s) \/ CheckOwn(s) \/ RenameReplace(s) \/ Exchange(s)
    \/ VerifyDisp(s)

Protocol ==
    \/ StartRun \/ Commit \/ LedgerCommit \/ SendSourceDone \/ Finish
    \/ \E s \in Seats : SeatStep(s)
    \/ GitRead \/ BackupBegin \/ BackupStepLock \/ BackupStepUnlock
    \/ BackupEnd

Environment ==
    \/ \E s \in Seats : Edit(s) \/ SilentRewrite(s) \/ ForeignWrite(s) \/ ForeignDelete(s)
    \/ Tick \/ CommitFail \/ CrashSrc \/ CrashDst \/ CrashBoth

\* Every run has been made: the behaviour may stop here.
Terminated ==
    /\ run = MaxRuns /\ sess \in {"done", "broken"} /\ Quiescent
    /\ sqlLock = "none"
    /\ UNCHANGED vars

Next == Protocol \/ Environment \/ Terminated

Spec == Init /\ [][Next]_vars

\* Fair protocol, unfair environment and faults.
LiveSpec == Spec /\ WF_vars(Protocol)

-----------------------------------------------------------------------------
(* PROPERTIES                                                               *)

TypeOK ==
    /\ run \in 0..MaxRuns /\ crashes \in 0..MaxCrashes
    /\ sess \in {"idle", "on", "broken", "done"}
    /\ srcStore \in {"none", "volatile", "durable"}
    /\ srcAuth \in 0..(MaxCrashes + 1)
    /\ sqlPhase \in {"none", "open", "done"} /\ sqlLock \in {"none", "shared"}
    /\ \A s \in Seats : srcStat[s] <= edits[s] /\ edits[s] <= MaxEdits

(* R25 / R-N58 (I3): no source content read of a seat at a stat identity   *)
(* the destination already holds durably: a committed output row, recorded *)
(* from that stat identity, vouching for the durable output at the path.   *)
(* Stated physically (HeldPhys), so losing the authority cannot hide a     *)
(* re-read behind a new key. This is R25 with "held" read as "a committed  *)
(* destination row": bytes that are durable with no row are outside it     *)
(* (R25_StrictNoDurableReread below). No ruling yet fixes that reading     *)
(* (README, "Code and design disagreements"). It is the operative R25     *)
(* check in code shape.                                                     *)
R25_NoDurableReread == \A r \in reads : ~r.held

(* docs/slo.md wording: no committed capture is re-read (a source ledger   *)
(* row whose destination output is still the one the destination holds).   *)
(* Vacuous while SupersedeMode = "off" (the transfer before #187): the     *)
(* source reads                                                            *)
(* with its ledger row present only to serve chunks for an absent output.  *)
(* MC_neg_reread_ignore_ledger and MC_neg_reread_exchange show it can fail. *)
R25_NoCommittedCaptureReread == \A r \in reads : ~r.committed

(* R25 under the strict reading of "held durably" (OI-1002-Q33), where     *)
(* bulkload's own durable output from a non-racy capture, published or     *)
(* adopted and sealed, counts as held with or without a row. Meaningful    *)
(* only under TrackStrictHeld. The code before #169 fails it               *)
(* (MC_r25_unrowed_no_adopt), the gap between R25_NoDurableReread and the  *)
(* strict reading; with the capture record's adoption (AdoptUnrowed, #169) *)
(* it holds (MC_r25_unrowed_bytes, MC_r25_strict_deep, MC_r25_strict_main, *)
(* and MC_r25_strict_unsealed across a lost source authority), for         *)
(* non-racy captures whose record could be written (header, ABSTRACTIONS). *)
(* OI-1003-Q40 keeps R25_NoDurableReread the SLO's obligation.             *)
R25_StrictNoDurableReread == \A r \in reads : ~r.strict

(* #169: the capture record's adoption is for outputs with no row. An      *)
(* output this store holds a matching row for is answered Reuse from the   *)
(* row (Store::output_matches) and is never adopted, hashed and re-rowed   *)
(* again. Such a re-adoption reads no source byte, so no R25 or S3         *)
(* property sees it; this one does (MC_neg_reuse_ignores_row). Trivial     *)
(* unless AdoptUnrowed.                                                    *)
AdoptOnlyUnrowed ==
    \A s \in Seats :
        (dEnt[s] = "adopt_queued" /\ dPub[s] = "adopt_wait")
            => ~\E r \in dstRows :
                    r.seat = s /\ r.key = dRec[s].key /\ r.id = dRec[s].id

(* A seat is read at most once a session (P23).                            *)
ReadOnce == \A s \in Seats : rc[s] <= 1

(* S3 / WP0(c): a run reads only seats that were not held durably under    *)
(* their stat identity when it began (changed, racy or never carried) or   *)
(* that changed on either side during the run. An unchanged estate reads  *)
(* 0 content bytes.                                                        *)
S3_ReadsOnlyChanged == runReads \subseteq ((Seats \ heldAtStart) \cup changedRun)
S3_UnchangedReadsZero ==
    (heldAtStart = Seats /\ changedRun = {}) => runReads = {}

(* S3, not vacuous: a session that closed with every seat applied, none   *)
(* racy and nothing changed leaves every seat held, so the next run on an  *)
(* unchanged source starts with heldAtStart = Seats and reads nothing.     *)
S3_ClosedPassIsHeld ==
    (/\ sess = "done" /\ changedRun = {}
     /\ \A s \in Seats : outc[s] = "applied")
        => \A s \in Seats : HeldPhys(s, srcStat[s])

(* Durability ordering (I1): a committed output row describes bytes that   *)
(* are durable, data and name, at the path it names.                       *)
RecordImpliesBytes ==
    \A r \in dstRows :
        (out[r.seat].pres /\ out[r.seat].id = r.id)
            => /\ out[r.seat].dd /\ out[r.seat].nd
               /\ out[r.seat].data = r.data

(* OI-1001-Q15: Held{true} is sent only after the group commit returned.   *)
HeldAfterCommit ==
    \A s \in Seats : (msg[s].t = "held" /\ msg[s].b) => committedRun[s]

(* The source ledger trails the destination: every source row, committed   *)
(* or pending, names a capture whose output row committed first.           *)
LedgerAfterHeld ==
    \A r \in srcLedger \cup sPend : [seat |-> r.seat, key |-> r.key] \in everCommitted

(* SourceDone follows the ledger's last commit.                            *)
DoneAfterLedger == srcDone => sPend = {}

(* A Reuse is never stale: a durably held output under the seat's current  *)
(* stat identity has the seat's current bytes (#86 racy rule).             *)
ReuseSound ==
    \A s \in Seats : HeldPhys(s, srcStat[s]) => out[s].data = edits[s]

(* A ledger manifest served without reading is the seat's current content. *)
LedgerSound ==
    \A r \in srcLedger : r.key = WireKey(r.seat) => r.data = edits[r.seat]

(* No-clobber, and WP0(d): bulkload replaces or removes a destination file *)
(* only when its identity is one this store recorded.                      *)
NoClobber == ~clobbered

(* S2 / WP0(b): source access is typed reads only: no write, no lock or    *)
(* lease, no process control.                                              *)
S2_TypedSourceAccess == srcOps \subseteq AllowedSourceOps

(* The SQLite backup's read lock is the stated exception: only ever        *)
(* shared, held only inside a step of a counted backup, and taken at most  *)
(* max_steps times.                                                        *)
S2_BackupLockBounded ==
    /\ sqlSteps <= MaxBackupSteps
    /\ sqlLock \in {"none", "shared"}
    /\ (sqlLock = "shared" => sqlPhase = "open" /\ sqlBackups >= 1)

(* S4 shape: a finished session leaves every seat applied or typed-refused.*)
ClosureAccounted ==
    sess = "done" => \A s \in Seats : outc[s] \in {"applied", "applied_racy"} \cup TypedCodes

(* Wall-clock budget, checked by TLC on every state it explores. It is     *)
(* state-level on purpose: TLC evaluates a zero-arity definition that      *)
(* names no variable once, at startup, so a budget written only over        *)
(* TLCGet("duration") never trips. The conjunct over run makes TLC          *)
(* evaluate it per state. A trip is reported as INCONCLUSIVE, never as a   *)
(* pass or a caught mutant (README.md; MC_budget_selftest proves it).      *)
WithinBudget == run \in 0..MaxRuns => TLCGet("duration") < BudgetSeconds

(* Liveness: with no crash, a stable source and a fair protocol, every     *)
(* started run reaches closure, and every run is made.                     *)
RunsClose == (sess = "on") ~> (sess = "done")
AllRunsFinish == <>(run = MaxRuns /\ sess = "done")

(* Reachability witnesses. Each states that a scenario never happens; a   *)
(* reach row's REACHED outcome is its violation, which proves the scenario *)
(* is reachable within that row's bound (README, "Coverage").              *)

\* The source served a manifest from its ledger, reading nothing.
Witness_LedgerManifest == ~\E s \in Seats : sCap[s].led

\* The source re-read a ledger manifest's chunks to serve them (pread).
Witness_LedgerChunkRead == \A r \in reads : r.kind # "chunks"

\* WP0(g): a later run consulted the ledger for a row a source power loss
\* had dropped, missed, and read the seat to build its manifest. Only a
\* relaxed ledger (or a lost store) puts rows in ledgerLost.
Witness_LostRowRead ==
    ~\E s \in Seats : \E x \in ledgerLost :
        /\ x.seat = s /\ x.key = SKey(s) /\ x.run < run
        /\ sEnt[s] = "manifested" /\ ~sCap[s].led

\* Symmetry over seats, for the safety configs only.
SeatSymmetry == Permutations(Seats)
=============================================================================
