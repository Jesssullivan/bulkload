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
(*   - The destination store's records of the exchange design (#187 and    *)
(*     its review, OI-1003-Q100..Q102), all only under SupersedeMode       *)
(*     "exchange": the intent of a superseding publish (table supersedes), *)
(*     committed before the exchange with the output's rows moved into it  *)
(*     and settled by the group commit or by the next session's sweep; the *)
(*     ownership row (table owned_outputs: an output this store owns with  *)
(*     no reuse row, after an interrupted supersede whose staged file is   *)
(*     at the leaf, and for a racy publish, OI-1003-Q101); the remembered  *)
(*     refusal of an entry for what its path holds (table                  *)
(*     refused_outputs); and the refusal of a superseding seat, before     *)
(*     anything is staged, on a destination with no atomic exchange        *)
(*     (ExchangeSupported = FALSE, OI-1003-Q100: no fallback).             *)
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
(*   - The superseding publish's records. One intent per seat (the code    *)
(*     keys an intent by its staged name, so a path whose displaced file   *)
(*     is kept aside could be superseded again; here it is not).           *)
(*     begin_supersedes is one commit per seat, not per group, and never   *)
(*     fails. The publish's last look at the output is folded into the     *)
(*     exchange: every third-party change after the intent lands in the    *)
(*     window before the exchange, so the displaced file's check           *)
(*     (VerifyDisp) is what answers it. An identity is a number any write  *)
(*     moves, so the intent's stamp (size, mtime) of the staged file and   *)
(*     the ctime the exchange moves are not modelled, and neither is the   *)
(*     destination's racy window for a remembered refusal: a third-party   *)
(*     write always moves the identity. Ownership is read by path alone    *)
(*     (OwnIds), without the source authority that keys the code's rows.   *)
(*     The exchange probe is one constant, not one answer per device.      *)
(*     An owned output whose bytes equal the manifest's is superseded by   *)
(*     an equal file where the code adopts it in place, except on a        *)
(*     destination with no exchange, where the plan's byte check decides   *)
(*     between adopting and refusing, as in the code.                      *)
(*   - Background priority (WP0(f)) is a measured S2 budget, not a        *)
(*     safety property here; it is covered by P35.                        *)
(*   - Git carry v1 and estate apply's .done journals are not modelled    *)
(*     here (v1's chain and base custody is GitCarry.tla's; carry_v2 and  *)
(*     its ingest journal are deleted, OI-1003-Q44/Q56). The journal rule *)
(*     ("a record never precedes what it describes") is the same          *)
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
(*   RecvManifest's plan for an owned output:                              *)
(*                   A/transfer.rs plan_file; A/materialize.rs             *)
(*                   owned_output, Destination::exchange_supported (the    *)
(*                   probe; Plan::NoExchange when there is no exchange);   *)
(*                   A/transfer_store.rs Store::output_rows (reuse rows    *)
(*                   and the path's ownership row, owner_key)              *)
(*   BeginSupersede  A/materialize.rs PublishSink::supersede,              *)
(*                   StagedFile::prepare_supersede;                        *)
(*                   A/transfer_store.rs begin_supersedes (the intent, and *)
(*                   the path's rows moved into it, in one commit)         *)
(*   Exchange        A/materialize.rs StagedFile::exchange                 *)
(*                   (io::exchange: RENAME_EXCHANGE / RENAME_SWAP)         *)
(*   VerifyDisp      A/materialize.rs StagedFile::exchange (is_owned on    *)
(*                   the displaced file; the exchange back and its seal;   *)
(*                   Exchanged::Done, Restored, Stranded)                  *)
(*   Commit's settle of an intent, and a racy output's ownership row:      *)
(*                   A/transfer_store.rs commit_outputs, settle_in,        *)
(*                   row_in, own_in, owner_key                             *)
(*   StartRun's sweep of the intents a crash or a failed group left:       *)
(*                   A/materialize.rs Destination::settle_supersedes       *)
(*                   (is_owned, is_staged, published), ahead of            *)
(*                   Destination::sweep; A/transfer_store.rs               *)
(*                   supersede_intents, settle_supersede                   *)
(*   RecvEntry's remembered refusal, and RecvEnd's record of one:          *)
(*                   A/transfer.rs Inbound::remembered_refusal,            *)
(*                   Inbound::remember_refusal, verify_settled;            *)
(*                   A/transfer_store.rs Store::refused_output,            *)
(*                   remember_refused_output (RefusedOutput)               *)
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
    AdoptUnrowed,        \* #169: each non-racy staged file carries its capture
                         \* record, and a resume adopts a durable unrowed
                         \* output whose record proves it (the code since #169)
    ExchangeSupported    \* OI-1003-Q100: the destination file system has the
                         \* atomic exchange of two names. FALSE: a seat whose
                         \* output would be superseded is refused
                         \* DESTINATION_EXCHANGE_UNSUPPORTED before anything
                         \* is staged; there is no fallback. Read only under
                         \* SupersedeMode "exchange"

Mutations == {"none", "held_before_commit", "commit_before_fsync",
              "commit_before_dirseal", "adopt_without_seal",
              "ledger_before_held", "reread_durable",
              "reread_ignore_ledger",
              "src_ledger_carries_r25", "record_racy", "source_write",
              "git_optional_locks", "pause_writer", "unbounded_backup",
              "supersede_unchecked", "sweep_displaced", "double_read",
              "untyped_space", "done_before_sync", "skip_output_row",
              "adopt_unkeyed", "adopt_unverified", "reuse_ignores_row",
              "adopt_unrecorded", "owned_ignores_identity",
              "sweep_drops_ownership", "exchange_before_intent",
              "own_is_reuse", "refusal_unbound", "late_exchange_refusal"}

ASSUME /\ IsFiniteSet(Seats) /\ Seats # {}
       /\ MaxRuns \in Nat /\ MaxCrashes \in Nat /\ MaxEdits \in Nat
       /\ MaxForeign \in Nat /\ MaxCommitFails \in Nat
       /\ SpaceRefusals \in BOOLEAN /\ RelaxedSourceLedger \in BOOLEAN
       /\ RelaxedAuthority \in BOOLEAN
       /\ StoreRootSealed \in BOOLEAN /\ TrackStrictHeld \in BOOLEAN
       /\ AdoptUnrowed \in BOOLEAN /\ ExchangeSupported \in BOOLEAN
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
               "DESTINATION_SPACE_INSUFFICIENT",
               "DESTINATION_EXCHANGE_UNSUPPORTED"}

\* A superseding publish's intent (A/transfer_store.rs SupersedeIntent, table
\* supersedes): the staged file's identity (new), the identity this store's
\* rows recorded for the output it replaces (old), and the path's rows,
\* moved out of the store when the intent commits: its reuse rows (rows) and
\* its ownership row (own, 0 for none).
NoIntent == [st |-> "none", new |-> 0, old |-> 0, rows |-> {}, own |-> 0]
\* Ghost: the two files of a seat's last superseding publish.
NoSup == [old |-> 0, new |-> 0]

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
    dstRows,      \* committed output rows [seat, key, id, data]: reuse rows
                  \* (table outputs)
    dOwn,         \* seat -> identity in the path's ownership row, 0 for none
                  \* (table owned_outputs): this store's own output with no
                  \* reuse row. Never a reuse key
    dRefused,     \* remembered refusals [seat, key, id, code] (table
                  \* refused_outputs): the entry under key was refused with
                  \* code for the file of identity id at its path
    intent,       \* seat -> the intent of a superseding publish not yet
                  \* settled (table supersedes)
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
    clobbered,    \* bulkload replaced or removed a file it does not own
    lastSup,      \* seat -> the old and the new file of its last superseding
                  \* publish (ghost, for SupersedeAtomic)
    reuseBad,     \* a Reuse was answered with no reuse row for the entry's
                  \* key and the output's identity
    wrongRefusal  \* a remembered refusal was answered for an unchanged seat
                  \* whose bytes the path held, or whose path held nothing

srcVars  == <<srcStat, edits, fresh>>
sVars    == <<sEnt, sRow, sCap, sPend, srcLedger, srcDone, ledgerLost>>
authVars == <<srcAuth, srcStore>>
storeSup == <<dOwn, dRefused, intent>>
ghostSup == <<lastSup, reuseBad, wrongRefusal>>
dVars    == <<dEnt, dKey, dPlan, dPub, dRec, tmp, out, outPrev, disp, dstRows,
              storeSup>>
runVars  == <<run, sess, crashes, foreign, cfails>>
estVars  == <<sqlPhase, sqlLock, sqlSteps, sqlBackups, gitReads>>
ghostVars == <<reads, srcOps, rc, runReads, heldAtStart, changedRun,
               committedRun, everCommitted, outc, clobbered, ghostSup>>
vars == <<srcVars, sVars, authVars, msg, dVars, runVars, estVars, ghostVars>>

-----------------------------------------------------------------------------
(* Helpers *)

\* Identities of outputs this store holds a row for, at path s: its reuse
\* rows and the path's ownership row (A/transfer_store.rs Store::output_rows).
\* A file at the path is this store's own, untouched since, exactly when its
\* identity is one of these (A/materialize.rs owned_output).
OwnIds(s) == {r.id : r \in {x \in dstRows : x.seat = s}}
             \cup (IF dOwn[s] # 0 THEN {dOwn[s]} ELSE {})

\* The same for rows held by an intent, moved out of the store until the
\* intent is settled.
IntentIds(it) == {r.id : r \in it.rows} \cup (IF it.own # 0 THEN {it.own} ELSE {})

\* Every identity this store recorded for an output at path s, wherever the
\* row now is. NoClobber is stated over these.
RecordedIds(s) == OwnIds(s) \cup IntentIds(intent[s])

\* No stage of seat s's group commit is pending.
PipeIdle(s) == dPub[s] \in {"none", "committed", "failed_occupied", "failed_space"}

\* The remembered refusals that answer the entry of key k at seat s
\* (A/transfer_store.rs Store::refused_output; A/transfer.rs
\* Inbound::remembered_refusal): recorded under that row key, for the file
\* now at the path, by its identity; a refusal for a missing exchange stands
\* only while there is none. At most one: the table's key is the row key.
\* Mutation refusal_unbound: the file's identity is not compared.
Remembered(s, k) ==
    {x \in dRefused :
        /\ x.seat = s /\ x.key = k /\ out[s].pres
        /\ (x.id = out[s].id \/ Mutation = "refusal_unbound")
        /\ (x.code = "DESTINATION_EXCHANGE_UNSUPPORTED" => ~ExchangeSupported)}

\* remember_refused_output: one record per row key.
Remember(s, k, id, code) ==
    {x \in dRefused : ~(x.seat = s /\ x.key = k)}
        \cup {[seat |-> s, key |-> k, id |-> id, code |-> code]}

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
    /\ dOwn = [s \in Seats |-> 0]
    /\ dRefused = {}
    /\ intent = [s \in Seats |-> NoIntent]
    /\ run = 0 /\ sess = "idle" /\ crashes = 0 /\ foreign = 0 /\ cfails = 0
    /\ sqlPhase = "none" /\ sqlLock = "none" /\ sqlSteps = 0 /\ sqlBackups = 0
    /\ gitReads = 0
    /\ reads = {} /\ srcOps = {} /\ rc = [s \in Seats |-> 0]
    /\ runReads = {} /\ heldAtStart = {} /\ changedRun = {}
    /\ committedRun = [s \in Seats |-> FALSE]
    /\ everCommitted = {}
    /\ outc = [s \in Seats |-> "none"]
    /\ clobbered = FALSE
    /\ lastSup = [s \in Seats |-> NoSup]
    /\ reuseBad = FALSE
    /\ wrongRefusal = FALSE

-----------------------------------------------------------------------------
(* Session start.                                                          *)
(* A/transfer.rs receive: Store::open, the exclusive publisher, sweep_root *)
(* (orphaned temporaries are salvaged for chunks and removed at finish),  *)
(* then Open; serve answers Start.                                         *)
(*                                                                         *)
(* WP0(d) exchange design, the sweep of unsettled intents                  *)
(* (A/materialize.rs Destination::settle_supersedes, which runs in each    *)
(* directory before its temporaries are swept and before any entry is      *)
(* decided; A/transfer_store.rs supersede_intents, settle_supersede). An   *)
(* intent a crash or a failed group commit left says what the staged name  *)
(* (disp here, once the exchange took effect) and the leaf (out) may hold: *)
(*   - nothing displaced, the leaf is still the owned output (SwIntact):   *)
(*     the exchange did not take effect. The intent's rows go back: the    *)
(*     old output with its old rows.                                       *)
(*   - nothing displaced, or the owned output displaced (SwOurs, which is  *)
(*     then removed), and the leaf holds the staged file (SwStaged,        *)
(*     materialize.rs published): the exchange took effect and the new     *)
(*     row did not commit. The leaf gets the path's ownership row          *)
(*     (owner_key): the new output is this store's own, with no reuse row. *)
(*     Mutation sweep_drops_ownership: the intent is deleted without it.   *)
(*   - another file displaced (not this store's): exchanged back while the *)
(*     leaf still holds the staged file (SwBack); otherwise kept where it  *)
(*     is with its intent, reported, never removed (SwAside).              *)
(* Every case but the last deletes the intent. Mutation sweep_displaced:   *)
(* no intent is read, so the sweep removes a displaced file like any       *)
(* tagged temporary, which is what the sweep before #187 would do.         *)

SwBegun(s)  == intent[s].st = "begun" /\ Mutation # "sweep_displaced"
SwIntact(s) == out[s].pres /\ out[s].id = intent[s].old
SwStaged(s) == out[s].pres /\ out[s].id = intent[s].new
SwOurs(s)   == disp[s].pres /\ disp[s].id = intent[s].old
SwBack(s)   == SwBegun(s) /\ disp[s].pres /\ ~SwOurs(s) /\ SwStaged(s)
SwAside(s)  == SwBegun(s) /\ disp[s].pres /\ ~SwOurs(s) /\ ~SwStaged(s)
SwRestore(s) == SwBegun(s) /\ ~disp[s].pres /\ SwIntact(s)
SwOwn(s)    == /\ SwBegun(s) /\ (~disp[s].pres \/ SwOurs(s)) /\ SwStaged(s)
               /\ Mutation # "sweep_drops_ownership"

RestoredOut(s) ==
    IF SwBack(s) THEN Durable(disp[s])     \* swap back over our uncommitted file
    ELSE out[s]

KeptDisp(s) ==
    \* A foreign displaced file that cannot be swapped back stays where it is,
    \* reported, never removed.
    IF SwAside(s) THEN disp[s] ELSE NoFile

\* The reuse rows the sweep puts back (INSERT OR IGNORE: a row already under
\* the key wins).
RestoredRows ==
    UNION {{r \in intent[s].rows :
              ~\E x \in dstRows : x.seat = r.seat /\ x.key = r.key} :
           s \in {x \in Seats : SwRestore(x)}}

SweptOwn(s) ==
    CASE SwOwn(s) -> intent[s].new
      [] SwRestore(s) /\ dOwn[s] = 0 -> intent[s].own
      [] OTHER -> dOwn[s]

\* HeldPhys over what the sweep leaves.
HeldAfterSweep(s) ==
    LET o == RestoredOut(s) IN
    /\ o.pres /\ o.dd /\ o.nd /\ o.data # GARBAGE
    /\ \E r \in dstRows \cup RestoredRows :
          r.seat = s /\ r.key % KeyBase = srcStat[s] /\ r.id = o.id

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
    /\ dstRows' = dstRows \cup RestoredRows
    /\ dOwn' = [s \in Seats |-> SweptOwn(s)]
    /\ intent' = [s \in Seats |-> IF SwBegun(s) /\ ~SwAside(s) THEN NoIntent
                                   ELSE intent[s]]
    \* A displaced file the sweep removes (not one it swaps back or keeps
    \* aside) must be one this store recorded.
    /\ clobbered' = \/ clobbered
                    \/ \E s \in Seats : /\ disp[s].pres
                                        /\ ~SwBack(s) /\ ~SwAside(s)
                                        /\ disp[s].id \notin RecordedIds(s)
    /\ heldAtStart' = {s \in Seats : HeldAfterSweep(s)}
    /\ runReads' = {}
    /\ changedRun' = {}
    /\ rc' = [s \in Seats |-> 0]
    /\ committedRun' = [s \in Seats |-> FALSE]
    /\ outc' = [s \in Seats |-> "pending"]
    /\ UNCHANGED <<srcVars, sRow, sPend, srcLedger, ledgerLost, dKey, outPrev,
                   dRefused, crashes, foreign, cfails, estVars, reads, srcOps,
                   everCommitted, ghostSup>>

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
                   committedRun, everCommitted, outc, clobbered, ghostSup>>

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
(* #187 review: an entry refused before for what its path holds, whose     *)
(* seat (row key) and whose file there (identity) have not moved since,    *)
(* is refused again with the same code and the source reads nothing        *)
(* (Inbound::remembered_refusal, Store::refused_output). It is asked       *)
(* after the reuse row and ahead of the capture record. An ownership row   *)
(* (dOwn) is never asked here: it names no seat, so it is never a reuse    *)
(* key (Store::output_matches reads table outputs alone). Mutation         *)
(* own_is_reuse: an output the path's ownership row names is answered      *)
(* Reuse.                                                                  *)
RecvEntry(s) ==
    /\ sess = "on" /\ dEnt[s] = "none" /\ msg[s].t = "entry"
    /\ LET k == msg[s].v
           ownReuse == /\ Mutation = "own_is_reuse"
                       /\ out[s].pres /\ dOwn[s] # 0 /\ out[s].id = dOwn[s]
           remembered == Remembered(s, k)
           reuse == /\ (RowMatches(s, k) \/ ownReuse)
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
                      ELSE IF remembered # {} THEN {"remembered"}
                      ELSE IF adopt THEN {"adopt_unrowed"}
                      ELSE (IF out[s].pres THEN {"manifest"} ELSE {"send", "manifest"})
                           \cup (IF SpaceRefusals THEN {"refuse"} ELSE {})
       IN \E d \in choices :
            /\ msg' = [msg EXCEPT ![s] = Msg("decide", 0, FALSE,
                                             CASE d = "adopt_unrowed" -> "reuse"
                                               [] d = "remembered" -> "refuse"
                                               [] OTHER -> d)]
            /\ dKey' = [dKey EXCEPT ![s] = k]
            /\ dEnt' = [dEnt EXCEPT ![s] =
                          CASE d = "send" -> "streaming"
                            [] d = "manifest" -> "await_manifest"
                            [] d = "adopt_unrowed" -> "adopt_queued"
                            [] OTHER -> "done"]
            /\ outc' = [outc EXCEPT ![s] =
                          CASE d = "reuse" -> "applied"
                            [] d = "refuse" -> "DESTINATION_SPACE_INSUFFICIENT"
                            [] d = "remembered" ->
                                 (CHOOSE x \in remembered : TRUE).code
                            [] OTHER -> "pending"]
            /\ reuseBad' = (reuseBad \/ (d = "reuse" /\ ~RowMatches(s, k)))
            \* The refusal must still be the right answer: the seat is the
            \* one walked, and the path holds a file with other bytes.
            /\ wrongRefusal' =
                  \/ wrongRefusal
                  \/ /\ d = "remembered" /\ srcStat[s] = sRow[s]
                     /\ ~(out[s].pres /\ out[s].data # edits[s])
            /\ IF d = "adopt_unrowed"
               THEN /\ dPub' = [dPub EXCEPT ![s] = "adopt_wait"]
                    /\ dRec' = [dRec EXCEPT ![s] =
                                  [key |-> k,
                                   data |-> IF Mutation = "adopt_unverified"
                                            THEN out[s].rec.data ELSE out[s].data,
                                   racy |-> FALSE, id |-> out[s].id, kind |-> "adopt"]]
               ELSE UNCHANGED <<dPub, dRec>>
    /\ UNCHANGED <<srcVars, authVars, sVars, dPlan, tmp, out, outPrev, disp,
                   dstRows, storeSup, runVars, estVars, reads, srcOps, rc,
                   runReads, heldAtStart, changedRun, committedRun,
                   everCommitted, clobbered, lastSup>>

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
                   everCommitted, outc, clobbered, ghostSup>>

(* Destination: plan a manifest and ask only for what it cannot fill.     *)
(* A/transfer.rs Inbound::manifest, plan_file: an existing output is       *)
(* adopted (verified at End), an absent one staged. WP0(d) designs: an     *)
(* existing output whose identity is this store's own (a reuse row or the  *)
(* path's ownership row, A/materialize.rs owned_output) is superseded.     *)
(* Mutation supersede_unchecked: any existing output is. Mutation          *)
(* owned_ignores_identity: any file at a path that has an ownership row    *)
(* is, whatever its identity.                                              *)
(* OI-1003-Q100 (Destination::exchange_supported, Plan::NoExchange): on a  *)
(* destination with no atomic exchange, an owned output that holds the     *)
(* manifest's bytes is adopted in place, and one that holds other bytes    *)
(* is refused DESTINATION_EXCHANGE_UNSUPPORTED when the entry ends:        *)
(* nothing is staged and no chunk is asked of the source. Mutation         *)
(* late_exchange_refusal: no probe, so the seat is staged and its chunks   *)
(* asked for (the code before the #187 review).                            *)
RecvManifest(s) ==
    /\ sess = "on" /\ dEnt[s] = "await_manifest" /\ msg[s].t = "manifest"
    /\ LET owned == \/ out[s].id \in OwnIds(s)
                    \/ Mutation = "supersede_unchecked"
                    \/ Mutation = "owned_ignores_identity" /\ dOwn[s] # 0
           plan == IF ~out[s].pres THEN "write"
                   ELSE IF /\ SupersedeMode # "off"
                           /\ ~disp[s].pres /\ intent[s].st = "none"
                           /\ owned
                        THEN IF \/ SupersedeMode = "check_rename"
                                \/ ExchangeSupported
                                \/ Mutation = "late_exchange_refusal"
                             THEN "supersede"
                             ELSE IF out[s].data = msg[s].v THEN "adopt"
                             ELSE "noexchange"
                        ELSE "adopt"
       IN /\ dPlan' = [dPlan EXCEPT ![s] = plan]
          /\ msg' = [msg EXCEPT ![s] = Msg("need", 0,
                                           plan \notin {"adopt", "noexchange"},
                                           "none")]
    /\ dEnt' = [dEnt EXCEPT ![s] = "filling"]
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPub, dRec, tmp, out, outPrev, disp,
                   dstRows, storeSup, runVars, estVars, ghostVars>>

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
                   everCommitted, outc, clobbered, ghostSup>>

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
(* #187 review (Inbound::remember_refusal, verify_settled;                 *)
(* Store::remember_refused_output): a refusal for what the path holds,     *)
(* DESTINATION_OCCUPIED or, with no atomic exchange (OI-1003-Q100),        *)
(* DESTINATION_EXCHANGE_UNSUPPORTED, is remembered under the entry's row   *)
(* key with the identity of the file at the path, when the capture was     *)
(* not racy. The store commit of that record is durable at once and        *)
(* claims nothing about any file.                                          *)
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
              /\ UNCHANGED <<out, outc, dRefused>>
         [] plan = "noexchange" ->
              \* Plan::NoExchange: this store's own output with other bytes,
              \* and no atomic exchange to supersede it with. It keeps its row.
              /\ msg' = [msg EXCEPT ![s] = Msg("held", 0, FALSE, "none")]
              /\ dEnt' = [dEnt EXCEPT ![s] = "done"]
              /\ outc' = [outc EXCEPT ![s] = "DESTINATION_EXCHANGE_UNSUPPORTED"]
              /\ dRefused' = IF /\ ~racy /\ out[s].pres
                                /\ out[s].id \in OwnIds(s) /\ out[s].data # d
                             THEN Remember(s, dKey[s], out[s].id,
                                           "DESTINATION_EXCHANGE_UNSUPPORTED")
                             ELSE dRefused
              /\ UNCHANGED <<tmp, dPub, dRec, out>>
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
              /\ UNCHANGED <<tmp, outc, dRefused>>
         [] OTHER ->
              /\ msg' = [msg EXCEPT ![s] = Msg("held", 0, FALSE, "none")]
              /\ dEnt' = [dEnt EXCEPT ![s] = "done"]
              /\ outc' = [outc EXCEPT ![s] = "DESTINATION_OCCUPIED"]
              /\ dRefused' = IF SupersedeMode = "exchange" /\ ~racy /\ out[s].pres
                             THEN Remember(s, dKey[s], out[s].id,
                                           "DESTINATION_OCCUPIED")
                             ELSE dRefused
              /\ UNCHANGED <<tmp, dPub, dRec, out>>
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPlan, outPrev, disp, dstRows,
                   dOwn, intent, runVars, estVars, reads, srcOps, rc, runReads,
                   heldAtStart, changedRun, committedRun, everCommitted,
                   clobbered, ghostSup>>

(* Destination: the source refused an entry's capture.                     *)
(* A/transfer.rs Inbound::refused (staged bytes discarded).                *)
RecvRefused(s) ==
    /\ sess = "on" /\ msg[s].t = "refused"
    /\ dEnt[s] \in {"streaming", "await_manifest", "filling"}
    /\ dEnt' = [dEnt EXCEPT ![s] = "done"]
    /\ outc' = [outc EXCEPT ![s] = msg[s].c]
    /\ msg' = [msg EXCEPT ![s] = NoMsg]
    /\ UNCHANGED <<srcVars, authVars, sVars, dKey, dPlan, dPub, dRec, tmp, out, outPrev,
                   disp, dstRows, storeSup, runVars, estVars, reads, srcOps, rc,
                   runReads, heldAtStart, changedRun, committedRun,
                   everCommitted, clobbered, ghostSup>>

-----------------------------------------------------------------------------
(* Destination group-commit pipeline (A/io/durable.rs Committer,           *)
(* A/materialize.rs PublishSink::commit). These steps also run after the  *)
(* session broke: dropping the Committer commits what is pending.          *)

PipeBase == <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, runVars,
              estVars, reads, srcOps, rc, runReads, heldAtStart,
              changedRun, committedRun, everCommitted, outc, dRefused,
              reuseBad, wrongRefusal>>
PipeUnchanged == <<PipeBase, dstRows, dOwn, intent, lastSup>>

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

(* WP0(d) design "exchange", the code since #187 (A/materialize.rs         *)
(* PublishSink::supersede): an intent first, then RENAME_EXCHANGE          *)
(* (renameat2 / renameatx_np RENAME_SWAP) puts the new file in place and   *)
(* the old one under the staged name, then the displaced file is checked   *)
(* against the identity the intent recorded, and a file that is not this   *)
(* store's is swapped back.                                                *)
(*                                                                         *)
(* Step 1 and 2 (StagedFile::prepare_supersede, then                       *)
(* A/transfer_store.rs begin_supersedes): the leaf must still hold this    *)
(* store's own output, or the entry is refused DESTINATION_OCCUPIED and    *)
(* the staged file removed. One store commit then writes the intent and    *)
(* moves the path's rows (reuse rows and ownership row) out of the store   *)
(* into it. From here until the intent is settled no row vouches for the   *)
(* path, whichever file a power loss leaves there, and the intent alone    *)
(* says which identity is this store's own. Mutation supersede_unchecked   *)
(* and owned_ignores_identity: as at the plan. Mutation                    *)
(* exchange_before_intent: nothing is committed here; the intent follows   *)
(* the exchange (VerifyDisp), so a crash between the two leaves the new    *)
(* file beside the old rows. Mutation late_exchange_refusal reaches this   *)
(* step with no exchange to use, and is refused here.                      *)
BeginSupersede(s) ==
    /\ SupersedeMode = "exchange" /\ dRec[s].kind = "supersede"
    /\ dPub[s] = "sealed"
    /\ LET owned == /\ out[s].pres /\ ExchangeSupported
                    /\ \/ out[s].id \in OwnIds(s)
                       \/ Mutation = "supersede_unchecked"
                       \/ Mutation = "owned_ignores_identity" /\ dOwn[s] # 0
           rows == {r \in dstRows : r.seat = s}
       IN IF owned
          THEN /\ lastSup' = [lastSup EXCEPT ![s] = [old |-> out[s].id, new |-> run]]
               /\ IF Mutation = "exchange_before_intent"
                  THEN UNCHANGED <<dstRows, dOwn, intent>>
                  ELSE /\ intent' = [intent EXCEPT ![s] =
                                       [st |-> "begun", new |-> run,
                                        old |-> out[s].id, rows |-> rows,
                                        own |-> dOwn[s]]]
                       /\ dstRows' = dstRows \ rows
                       /\ dOwn' = [dOwn EXCEPT ![s] = 0]
               /\ dPub' = [dPub EXCEPT ![s] = "intent"]
               /\ UNCHANGED tmp
          ELSE /\ dPub' = [dPub EXCEPT ![s] = "failed_occupied"]
               /\ tmp' = [tmp EXCEPT ![s] = NoTmp]
               /\ UNCHANGED <<dstRows, dOwn, intent, lastSup>>
    /\ UNCHANGED <<PipeBase, dRec, out, outPrev, disp, cfails, clobbered>>

(* Step 3 (StagedFile::exchange, io::exchange): the staged name and the    *)
(* leaf trade files in one atomic step. The code looks at the output once  *)
(* more just before; here that look is folded away, so a third-party write *)
(* since the intent always lands in the window the look cannot close, and  *)
(* the displaced file's check answers it. A leaf that vanished exchanges   *)
(* nothing (ENOENT, Exchanged::Undone): the staged file is removed, the    *)
(* entry refused DESTINATION_OCCUPIED, and the intent settled without its  *)
(* rows, which vouch for nothing now (in the code that settle rides in the *)
(* group's commit; a crash before it leaves the sweep the same answer).    *)
Exchange(s) ==
    /\ SupersedeMode = "exchange" /\ dRec[s].kind = "supersede"
    /\ dPub[s] = "intent"
    /\ IF out[s].pres
       THEN /\ out' = [out EXCEPT ![s] = NewOut(s, tmp[s].data, TRUE)]
            /\ outPrev' = [outPrev EXCEPT ![s] = out[s]]
            /\ disp' = [disp EXCEPT ![s] = [out[s] EXCEPT !.nd = FALSE]]
            /\ dRec' = [dRec EXCEPT ![s].id = run]
            /\ dPub' = [dPub EXCEPT ![s] = "exchanged"]
            /\ UNCHANGED intent
       ELSE /\ intent' = [intent EXCEPT ![s] = NoIntent]
            /\ dPub' = [dPub EXCEPT ![s] = "failed_occupied"]
            /\ UNCHANGED <<out, outPrev, disp, dRec>>
    /\ tmp' = [tmp EXCEPT ![s] = NoTmp]
    /\ UNCHANGED <<PipeBase, dstRows, dOwn, lastSup, cfails, clobbered>>

(* Step 4 (StagedFile::exchange, is_owned on the displaced file): the      *)
(* displaced file is this store's own when it has the identity the intent  *)
(* recorded; it is then removed (Exchanged::Done), and the group's seals   *)
(* and commit follow. Any other file is exchanged back while the leaf      *)
(* still holds the staged file, the directory sealed and the staged file   *)
(* removed (Exchanged::Restored; the intent is settled without its rows:   *)
(* the output they named is gone); otherwise it stays under the staged     *)
(* name with its intent, for the sweep to keep aside and report            *)
(* (Exchanged::Stranded). Either way the entry is refused                  *)
(* DESTINATION_OCCUPIED. Mutation supersede_unchecked removes the          *)
(* displaced file without the check.                                       *)
VerifyDisp(s) ==
    /\ dPub[s] = "exchanged"
    /\ LET late == Mutation = "exchange_before_intent"
           rows == {r \in dstRows : r.seat = s}
           it == IF late
                 THEN [st |-> "begun", new |-> lastSup[s].new,
                       old |-> lastSup[s].old, rows |-> rows, own |-> dOwn[s]]
                 ELSE intent[s]
           storeRows == IF late THEN dstRows \ rows ELSE dstRows
           storeOwn == IF late THEN [dOwn EXCEPT ![s] = 0] ELSE dOwn
       IN /\ dstRows' = storeRows
          /\ dOwn' = storeOwn
          /\ IF disp[s].id = it.old \/ Mutation = "supersede_unchecked"
             THEN \* the displaced output is this store's own: remove it
                  /\ clobbered' = (clobbered \/ disp[s].id \notin IntentIds(it))
                  /\ disp' = [disp EXCEPT ![s] = NoFile]
                  /\ intent' = [intent EXCEPT ![s] = it]
                  /\ dPub' = [dPub EXCEPT ![s] = "renamed"]
                  /\ UNCHANGED <<out, outPrev>>
             ELSE \* foreign: exchange back and seal, if our file is still in place
                  /\ IF out[s].pres /\ out[s].id = dRec[s].id
                     THEN /\ out' = [out EXCEPT ![s] = Durable(disp[s])]
                          /\ disp' = [disp EXCEPT ![s] = NoFile]
                          /\ intent' = [intent EXCEPT ![s] = NoIntent]
                     ELSE /\ intent' = [intent EXCEPT ![s] = it]
                          /\ UNCHANGED <<out, disp>>   \* kept aside, reported
                  /\ outPrev' = [outPrev EXCEPT ![s] = NoFile]
                  /\ dPub' = [dPub EXCEPT ![s] = "failed_occupied"]
                  /\ UNCHANGED clobbered
    /\ UNCHANGED <<PipeBase, lastSup, dRec, tmp, cfails>>

(* A/transfer_store.rs StorePublisher::commit_outputs: one SQLite           *)
(* transaction (synchronous=FULL, fullfsync=ON) for a group: insert each    *)
(* row, or for a racy capture delete any row under its key (#86). It runs  *)
(* only after every file and directory of the group is sealed. Mutations  *)
(* commit_before_fsync / commit_before_dirseal let it run earlier.          *)
(* Since #187 the same transaction settles the group's superseding         *)
(* publishes (settle_in): each intent is deleted beside the new output's   *)
(* row, and the old output's rows, moved into the intent, are dropped with *)
(* it. OI-1003-Q101 (row_in, own_in, owner_key): an output published or    *)
(* adopted from a racy capture gets the path's ownership row, and no reuse *)
(* row, so a later change of its seat supersedes it; only under            *)
(* SupersedeMode "exchange", the code since #187.                          *)
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
       /\ dOwn' = [s \in Seats |->
                     IF /\ s \in G /\ SupersedeMode = "exchange" /\ dRec[s].racy
                        /\ Mutation \notin {"record_racy", "skip_output_row"}
                     THEN dRec[s].id ELSE dOwn[s]]
       /\ intent' = [s \in Seats |->
                       IF s \in G /\ dRec[s].kind = "supersede" THEN NoIntent
                       ELSE intent[s]]
       /\ dPub' = [s \in Seats |-> IF s \in G THEN "committed" ELSE dPub[s]]
       /\ committedRun' = [s \in Seats |-> committedRun[s] \/ s \in G]
       /\ everCommitted' = everCommitted
                           \cup {[seat |-> s, key |-> dRec[s].key] : s \in Recorded(G)}
       /\ UNCHANGED <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dRec, tmp, out,
                      outPrev, disp, dRefused, runVars, estVars, reads, srcOps,
                      rc, runReads, heldAtStart, changedRun, outc, clobbered,
                      ghostSup>>

(* A group whose seal or store commit fails (a full disk) is refused       *)
(* DESTINATION_SPACE_INSUFFICIENT and records nothing (#100,               *)
(* A/materialize.rs space_refusal). Its files may already be published.    *)
(* A superseding publish in the group keeps its intent (nothing settled    *)
(* it): its new file is at the leaf with no row, and the next session's    *)
(* sweep gives it the path's ownership row.                                *)
CommitFail ==
    /\ cfails < MaxCommitFails
    /\ \E G \in SUBSET Seats :
         /\ G # {}
         /\ \A s \in G : Ready(s)
         /\ dPub' = [s \in Seats |-> IF s \in G THEN "failed_space" ELSE dPub[s]]
    /\ cfails' = cfails + 1
    /\ UNCHANGED <<srcVars, authVars, sVars, msg, dEnt, dKey, dPlan, dRec, tmp, out,
                   outPrev, disp, dstRows, storeSup, run, sess, crashes, foreign,
                   estVars, ghostVars>>

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
                   disp, dstRows, storeSup, runVars, estVars, reads, srcOps, rc,
                   runReads, heldAtStart, changedRun, committedRun,
                   everCommitted, clobbered, ghostSup>>

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
                   outPrev, disp, dstRows, storeSup, run, crashes, foreign, cfails,
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
                   committedRun, everCommitted, outc, clobbered, ghostSup>>

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
                   changedRun, committedRun, everCommitted, outc, clobbered,
                   ghostSup>>

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
                  clobbered, ghostSup>>

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
                  dstRows, storeSup, run, sess, crashes, cfails, estVars, reads,
                  srcOps, rc, runReads, heldAtStart, committedRun, everCommitted,
                  outc, clobbered, ghostSup>>

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
(* destination store is durable at commit (synchronous=FULL, fullfsync=ON):*)
(* its reuse rows, ownership rows, remembered refusals and intents all     *)
(* survive, and an intent is what the next session's sweep reads           *)
(* (StartRun).                                                             *)
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
                   disp, dstRows, storeSup, run, foreign, cfails, estVars,
                   ghostVars>>

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
                   storeSup, run, foreign, cfails, estVars, ghostVars>>

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
    /\ UNCHANGED <<srcVars, sRow, dKey, dstRows, storeSup, run, foreign, cfails,
                   estVars, ghostVars>>

-----------------------------------------------------------------------------
SeatStep(s) ==
    \/ Walk(s) \/ RecvEntry(s) \/ RecvDecide(s) \/ RecvManifest(s)
    \/ RecvNeed(s) \/ RecvEnd(s) \/ RecvRefused(s) \/ AnswerHeld(s)
    \/ RecvHeld(s) \/ SealTemp(s) \/ Publish(s) \/ DirSeal(s)
    \/ SealAdopted(s) \/ CheckOwn(s) \/ RenameReplace(s) \/ Exchange(s)
    \/ VerifyDisp(s) \/ BeginSupersede(s)

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
    /\ \A s \in Seats : /\ intent[s].st \in {"none", "begun"}
                        /\ dOwn[s] \in 0..(FirstForeignId + MaxForeign)
    /\ reuseBad \in BOOLEAN /\ wrongRefusal \in BOOLEAN

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
(* only when its identity is one this store recorded: in a reuse row, in   *)
(* the path's ownership row, or in a row an unsettled intent holds         *)
(* (RecordedIds). The ghost clobbered is set by every step that unnames a  *)
(* file at a leaf or under a staged name without that: RenameReplace, the  *)
(* removal of a displaced file (VerifyDisp) and the sweep's (StartRun).    *)
(* The no-replace publish, the exchange itself and an exchange back unname *)
(* nothing.                                                                *)
NoClobber == ~clobbered

(* #187 (OI-1003-Q18, OI-1003-Q102): a superseding publish is atomic       *)
(* across any crash. Once it is settled (no intent recorded, no stage of   *)
(* its group pending), the path holds the old output with its old rows or  *)
(* the new output with a row of its own, never one beside the other's      *)
(* rows: while the new file is at the leaf this store holds a row for it   *)
(* (its reuse row, or the ownership row the sweep gives it) and none for   *)
(* the old output; while the old output is at the leaf this store still    *)
(* holds a row for it and none for the new file. A leaf a third party has  *)
(* written since is neither file, and says nothing.                        *)
SupersedeAtomic ==
    \A s \in Seats :
        (intent[s].st = "none" /\ PipeIdle(s) /\ lastSup[s].new # 0 /\ out[s].pres)
            => /\ (out[s].id = lastSup[s].new)
                    => /\ lastSup[s].new \in OwnIds(s)
                       /\ lastSup[s].old \notin OwnIds(s)
               /\ (out[s].id = lastSup[s].old)
                    => /\ lastSup[s].old \in OwnIds(s)
                       /\ lastSup[s].new \notin OwnIds(s)

(* OI-1003-Q101, R25 / #86: ownership never implies reuse. Reuse is        *)
(* answered only from a reuse row for the entry's key and the output's     *)
(* identity. The ownership row of a racy publish, or of an interrupted     *)
(* supersede, names no seat and is never a reuse source.                   *)
OwnershipNeverReuse == ~reuseBad

(* #187 review: a remembered refusal is answered only while it is still    *)
(* the right answer: for an unchanged seat, the path holds a file whose    *)
(* bytes are not the seat's.                                               *)
RememberedRefusalSound == ~wrongRefusal

(* OI-1003-Q100: on a destination with no atomic exchange a superseding    *)
(* seat is refused up front. Nothing is ever staged to supersede an        *)
(* output, no intent is recorded and nothing is displaced.                 *)
ExchangeRefusedUpFront ==
    ~ExchangeSupported
        => \A s \in Seats : /\ dRec[s].kind # "supersede"
                             /\ intent[s].st = "none" /\ ~disp[s].pres

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

\* OI-1003-Q100: a seat was refused for want of an atomic exchange.
Witness_ExchangeRefused ==
    \A s \in Seats : outc[s] # "DESTINATION_EXCHANGE_UNSUPPORTED"

\* #187 review: an entry was refused from its remembered refusal, when it
\* was offered (the space preflight is the only other Refuse decision).
Witness_RememberedRefusal ==
    ~\E s \in Seats :
        /\ msg[s].t = "decide" /\ msg[s].c = "refuse"
        /\ outc[s] \in {"DESTINATION_OCCUPIED", "DESTINATION_EXCHANGE_UNSUPPORTED"}

\* OI-1003-Q101: a superseding publish began on an output this store owned
\* by its ownership row alone (a racy publish).
Witness_OwnershipSuperseded ==
    ~\E s \in Seats :
        intent[s].st = "begun" /\ intent[s].rows = {} /\ intent[s].own # 0

\* The sweep gave an interrupted supersede's new file its ownership row: a
\* file with a capture record (so not a racy publish, which gets its
\* ownership row at its commit) is owned by the ownership row.
Witness_SweepOwnership ==
    ~\E s \in Seats :
        /\ out[s].pres /\ out[s].rec.key # 0
        /\ dOwn[s] = out[s].id /\ lastSup[s].new = out[s].id

\* The sweep put an interrupted supersede's rows back: the exchange did not
\* take effect, and the old output is at the leaf, settled.
Witness_SweepRestore ==
    ~\E s \in Seats :
        /\ intent[s].st = "none" /\ PipeIdle(s) /\ lastSup[s].new # 0
        /\ out[s].pres /\ out[s].id = lastSup[s].old

\* Symmetry over seats, for the safety configs only.
SeatSymmetry == Permutations(Seats)
=============================================================================
