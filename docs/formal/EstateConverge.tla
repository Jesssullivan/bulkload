----------------------------- MODULE EstateConverge -----------------------------
(***************************************************************************)
(* Destination custody of estate convergence: a re-apply brings a git      *)
(* checkout bulkload itself landed, and nobody changed since, to a newer   *)
(* capture (design: docs/agent-notes/2026-10-08-estate-converge-design.md  *)
(* on branch feat/estate-converge-20261008, sections 2 to 11 as amended by *)
(* sections 16 to 19). GitCarry.tla models CORPUS custody; this module     *)
(* models what happens at the destination. Ruling OI-1003-Q144 (Linear    *)
(* TIN-4543, 2026-10-09): convergence may update a checkout bulkload       *)
(* landed only if it is unchanged locally since landing (content, not      *)
(* stat); otherwise a typed refusal; local branches and tags deleted at    *)
(* the source are kept and reported; remote-tracking refs follow the       *)
(* source; configuration is report-only. Merge of the convergence code     *)
(* waits on this model. See docs/formal/README.md, "EstateConverge".       *)
(*                                                                         *)
(* SCOPE                                                                   *)
(*   - One source repository and its landing at the destination: the main *)
(*     checkout (a standalone item, "main") and optionally one linked      *)
(*     worktree ("wt", HEAD only), sharing one ref ledger.                *)
(*   - The source over time: commits and moves of any ref, branch create, *)
(*     delete and rename (foo -> foo/bar and back, o/x -> o/x/y), HEAD     *)
(*     switches, worktree add and remove, stash push and drop, seat edits  *)
(*     (tracked, untracked and ignored alike: carried seats), directory to *)
(*     file and file to directory changes, and index (staged) changes.    *)
(*   - Capture passes: each item's {item}.capture moves to a new instant  *)
(*     when its projection of the source changed (otherwise reuse).       *)
(*   - Apply passes, one item at a time under the destination lock: the   *)
(*     estate-apply rows A3 to A12 (converge::apply_workspace), the       *)
(*     ownership proof, the workspace intent, the native ref step with    *)
(*     its ref intent (native_refs::step, commit, settle), the seat engine,*)
(*     the index compare-and-swap, HEAD and its branch, the ledgers and   *)
(*     the journal; and the settle of a pending intent on a rerun.        *)
(*   - Crashes of the apply host at every step boundary (the eleven       *)
(*     estate.converge.* fault points and more), a power loss that undoes *)
(*     Git renames no directory sync made durable (refs_unsynced), and    *)
(*     with ChildCrash a crash inside a Git ref transaction (any subset   *)
(*     of its lines committed, Git's lock files left for the rest; inside *)
(*     a stash store, refs/stash.lock left).                              *)
(*   - Operator edits at the destination: seat contents, the landed       *)
(*     directory removed or replaced by a file, untracked additions,      *)
(*     touches, racy writes, the index, refs (commits, moves, deletes, a  *)
(*     checked-out branch's too), the stash, HEAD (a switch, an orphan    *)
(*     switch, a rebase or bisect holding its branch), and restoring a    *)
(*     landed value; between applies or (OpConcurrent) while an apply is  *)
(*     in flight (a restore only between applies).                        *)
(*   - Review 3 (2026-10-09, findings 1 to 14): where the reviewers named *)
(*     the rule, the model holds that rule and the code's current         *)
(*     behaviour is a Mutation marked CODE TODAY in the catalogue         *)
(*     (finish_after_all_deletes, rebase_unguarded, ledger_reads_index,   *)
(*     unborn_head_untyped, seat_parent_untyped, refs_unsynced,           *)
(*     probe_left, settle_moves_checked_out, txn_no_symref_verify; and    *)
(*     txn_no_cas, head_no_cas, which the code already does right);       *)
(*     where the fix is open, a finding row fails on the code as it is.  *)
(*                                                                         *)
(* ABSTRACTIONS (what is NOT modelled)                                     *)
(*   - Values are opaque ids from one counter: a commit, a blob, an index *)
(*     state, a stash entry. Fast-forward and non-fast-forward moves are  *)
(*     one action; ff-ness only changes a custody word.                   *)
(*   - A seat's identity (ino, size, mtime, ctime) is a version that      *)
(*     every write moves, except a racy write: an operator write in the   *)
(*     same timestamp tick as bulkload's last write of that seat (fresh,  *)
(*     cleared by Tick) may keep it. Modes and directory modes are not    *)
(*     modelled. A content-equal write (an editor's identical save) is a  *)
(*     touch.                                                              *)
(*   - Two seats, "d" and "d/f", carry the type changes and the operator's*)
(*     untracked addition inside a landed directory. Gitlinks, exclude    *)
(*     (converged like a seat, digest old or new), configuration (report- *)
(*     only, Q6) and the shape refusal (A8) are not modelled.             *)
(*   - The linked worktree is HEAD only: its seats and index behave as    *)
(*     the main's do. Refs-only items, bare repositories, foreign         *)
(*     repositories (no `created` marker) and --adopt are not modelled.   *)
(*   - Symbolic refs and per-worktree refs are not modelled (never        *)
(*     restored natively). Tombstone retention (90 days) is not modelled.*)
(*   - Each atomic durable write (landed::write: temporary, fsync, rename,*)
(*     directory fsync) is one step. A Git transaction is one step,       *)
(*     atomic, unless a ChildCrash lands inside it.                       *)
(*   - Provenance (refs/carry/v1/<SOURCE>/<instance>/) is one set of      *)
(*     imported instants; an instance holds all of its capture's refs and *)
(*     stashes. CORPUS custody (chains, bases, GC) is GitCarry.tla's.     *)
(*   - Racy writes land only between applies (an operator write right     *)
(*     after a landing); the timestamp-granularity window inside one seat *)
(*     operation (between its content check and its rename) is not        *)
(*     modelled (README, "What EstateConverge does not prove").           *)
(*                                                                         *)
(* CODE MAP (on feat/estate-converge-20261008; C = crates/bulkload-agent/  *)
(* src/git_carry/converge.rs, N = .../git_carry/native_refs.rs,            *)
(* E = crates/bulkload-agent/src/estate.rs)                               *)
(*   Capture        E capture_item; git_carry.rs capture_administration    *)
(*   Begin          E apply_item (A2, A4 journal no-op, pending, settle_refs)*)
(*                  and C apply_workspace (A5 first landing, finding 13,  *)
(*                  A12, the lock, native_refs::settle)                   *)
(*   FirstLandMain  git_carry.rs restore_staged_landed, C land_standalone_ *)
(*                  refs, record_landing                                   *)
(*   FirstStepWt, FirstPlanWt, FirstAddWt, FirstLedgerWt                  *)
(*                  git_carry.rs restore_linked_staged_landed, C          *)
(*                  plan_linked, finish_landing                            *)
(*   Decide_        C apply_workspace (A3 settle start, A6, A7, prove)     *)
(*   Import         C converge (import_verified_instance, the clash and   *)
(*                  holds_unlanded checks)                                *)
(*   ProbeExchange  C probe_exchange, then head_plan                      *)
(*   WriteIntent    C converge (WorkspaceIntent, landed::write)            *)
(*   RefStep        C Place::refs; N step, plan, decide, stash_decision    *)
(*   RefFinish, RefTxnDeletes, RefTxnWrites, RefStash, RefLedger           *)
(*                  N commit, finish, transact, update_refs, stash_apply,  *)
(*                  settle                                                 *)
(*   SeatOp, SeatVerify   C Engine::run, remove, replace, add, sweep      *)
(*   IndexPublish   C publish_index                                        *)
(*   HeadSet        C set_head (fresh) and settle's HEAD check             *)
(*   BranchRow      C Place::branch_row                                    *)
(*   WriteLanded    C next_ledger, landed::write, landed::remove           *)
(*   Journal        E apply_item's journal write                           *)
(*   Src*, Op*, Crash, ChildCrashTxn, ChildCrashHead, Tick   the          *)
(*                  environment                                            *)
(*                                                                         *)
(* NEGATIVE CONFIGS set Mutation to break exactly one rule; each MUST      *)
(* produce a counterexample on its named property (README.md).            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Items,        \* {"main"} or {"main", "wt"}
    Refs,         \* the native refs in play, a subset of AllRefs
    Paths,        \* the seats in play, a subset of {"d", "d/f"}
    IndexOn,      \* the main's index is modelled
    SrcKinds,     \* the source changes the environment may make
    OpKinds,      \* the operator edits the environment may make
    MaxSrc,       \* source changes
    MaxOp,        \* operator edits
    MaxCrashes,   \* crashes of the apply host
    OpConcurrent, \* operator edits may land while an apply is in flight
    RacyEdits,    \* a write right after bulkload's may keep a seat's stat
    ChildCrash,   \* a crash may land inside a Git ref transaction
    Landed,       \* the run starts from a landed destination
    Mutation,     \* "none", or one deliberate rule break
    BudgetSeconds \* wall-clock budget, checked by WithinBudget

Mutations == {"none", "proof_stat_only", "proof_stat_strict",
              "overwrite_modified", "no_intent", "exchange_unchecked",
              "index_no_cas", "stash_clear_blind", "name_conflict_unchecked",
              "single_ref_txn", "head_branch_in_step", "no_checkout_guard",
              "converge_before_ref_settle", "dir_replace_unchecked",
              "prune_local", "drop_provenance", "noop_restages",
              "sibling_intent_reproves", "finish_after_all_deletes",
              "rebase_unguarded", "ledger_reads_index", "unborn_head_untyped",
              "seat_parent_untyped", "refs_unsynced", "probe_left",
              "settle_moves_checked_out", "txn_no_symref_verify",
              "txn_no_cas", "head_no_cas"}

(* The closed unions the catalogue types (tla-check requires these sets   *)
(* to equal the catalogue's labels).                                      *)
\* UNTYPED: a code that names no converge cause (GIT_CHILD_FAILED,
\* PATH_ESCAPES_ROOT, an Io refusal) where the cause is a converge one.
Refusals == {"MODIFIED", "OCCUPIED", "STALE", "INTERRUPTED", "UNTYPED"}
AllItems == {"main", "wt"}
AllRefs  == {"main", "dev", "foo", "foo/bar", "o/x", "o/x/y", "t"}
AllPaths == {"d", "d/f"}
SrcKindSet == {"move", "create", "delete", "rename", "switch", "stash",
               "seat", "dirfile", "filedir", "index", "worktree"}
OpKindSet == {"seat", "racy", "touch", "untracked", "index", "ref", "stash",
              "stashdrop", "head", "restore", "rebase", "dir"}

ASSUME /\ Items \subseteq AllItems /\ "main" \in Items
       /\ Refs \subseteq AllRefs /\ "main" \in Refs
       /\ ("wt" \in Items => "dev" \in Refs)
       /\ Paths \subseteq AllPaths /\ ("d/f" \in Paths => "d" \in Paths)
       /\ IndexOn \in BOOLEAN
       /\ SrcKinds \subseteq SrcKindSet /\ OpKinds \subseteq OpKindSet
       /\ MaxSrc \in Nat /\ MaxOp \in Nat /\ MaxCrashes \in Nat
       /\ OpConcurrent \in BOOLEAN /\ RacyEdits \in BOOLEAN
       /\ ChildCrash \in BOOLEAN /\ Landed \in BOOLEAN
       /\ Mutation \in Mutations /\ BudgetSeconds \in Nat

Mut(m) == Mutation = m

LocalRefs  == {"main", "dev", "foo", "foo/bar"}
RemoteRefs == {"o/x", "o/x/y"}
Class(r) == IF r \in LocalRefs THEN "local"
            ELSE IF r \in RemoteRefs THEN "remote" ELSE "tag"
Branches == Refs \cap LocalRefs

\* Git keeps refs in one directory tree: a name and a name under it cannot
\* both exist (N name_conflict).
Conflicting(a, b) == {a, b} \in {{"foo", "foo/bar"}, {"o/x", "o/x/y"}}
NameConflict(n, names) == \E m \in names : Conflicting(n, m)
RenamePairs == {<<"foo", "foo/bar">>, <<"foo/bar", "foo">>,
                <<"o/x", "o/x/y">>, <<"o/x/y", "o/x">>}

\* Seat values: 0 absent, 1 a directory, anything larger a file's content.
ABSENT == 0
DIR == 1
IsFile(v) == v >= 2

-----------------------------------------------------------------------------
(* Initial source: every ref but the conflicting partners exists at value  *)
(* 2; the main is on "main", the linked worktree on "dev".                *)

InitRefs == [r \in Refs |-> IF r \in {"main", "dev", "foo", "o/x", "t"}
                            THEN 2 ELSE 0]
InitSeats == [p \in Paths |-> IF p = "d" /\ "d/f" \in Paths THEN DIR ELSE 3]
S0 == [refs |-> InitRefs, head |-> "main",
       wt |-> IF "wt" \in Items THEN "dev" ELSE "none",
       stash |-> << >>, seats |-> InitSeats, index |-> IF IndexOn THEN 4 ELSE 0]

NoRow == [has |-> FALSE, oid |-> 0, t |-> 0]
NoBr == [on |-> FALSE, name |-> "main", old |-> 0, new |-> 0]
NoSop == [on |-> FALSE, old |-> << >>, new |-> << >>]
NoSLed == [has |-> FALSE, order |-> << >>, t |-> 0]
Det(v) == [sym |-> "det", oid |-> v]
NoSeat == [v |-> ABSENT, st |-> 0]
NoSeatRec == [v |-> ABSENT, st |-> 0, racy |-> FALSE]
NoSeats == [p \in Paths |-> ABSENT]
NoLed == [r \in Refs |-> NoRow]
NoPlan == [head |-> Det(0), br |-> NoBr, row |-> [set |-> FALSE, r |-> NoRow],
           line |-> "none"]
NoWsLed == [has |-> FALSE, t |-> 0, head |-> Det(0), index |-> 0,
            seats |-> [p \in Paths |-> NoSeatRec]]
NoWsInt == [on |-> FALSE, from |-> 0, to |-> 0, hold |-> Det(0),
            hnew |-> Det(0), br |-> NoBr, iold |-> 0]
NoRefInt == [on |-> FALSE, ops |-> {}, sop |-> NoSop, led |-> NoLed,
             sled |-> NoSLed]
Idle == [st |-> "idle", i |-> "main", c |-> 0, cur |-> 0, mode |-> "fresh",
         ops |-> << >>, k |-> 1, sub |-> 0, P |-> [p \in Paths |-> 0],
         wr |-> {}, ws |-> [p \in Paths |-> 0], F |-> NoSeats, T |-> NoSeats, plan |-> NoPlan,
         hold |-> Det(0), hnew |-> Det(0), br |-> NoBr, iold |-> 0,
         again |-> {}, fin |-> TRUE, nled |-> NoLed, nsled |-> NoSLed,
         sgo |-> FALSE, sexp |-> << >>, ret |-> "none"]

VARIABLES
    src,       \* the source repository now
    srcN,      \* source changes so far
    nextV,     \* the next fresh value id
    caps,      \* every capture taken: its instant is its index
    capt,      \* item -> the instant its {item}.capture record names
    nat,       \* destination native refs (0: absent)
    locks,     \* Git lock files a crashed Git child left (ref names, "HEAD:i")
    dstash,    \* destination stash reflog, newest first
    dseat,     \* destination seats: value and identity version
    temp,      \* each seat's .bulkload-converge temporary (v = 0: none)
    dindex,    \* destination index entries
    dhead,     \* item -> its worktree (on) and HEAD (branch, or detached oid)
    prov,      \* the provenance instances the destination holds
    refLed,    \* refs.landed rows (oid 0 with has: a tombstone)
    stashLed,  \* refs.landed's stash row
    refInt,    \* refs.intent
    wsLed,     \* item -> workspace.landed
    wsInt,     \* item -> workspace.intent
    jour,      \* item -> the instants with a STATE journal
    outcome,   \* item -> its last apply outcome
    why,       \* item -> its last refusal was justified (ghost)
    pc,        \* the apply in flight (volatile)
    crashes,   \* crashes so far
    opN,       \* operator edits so far
    fresh,     \* seats bulkload wrote in the current timestamp tick
    opLive,    \* ref, seat, index -> the operator's value, while live (ghost)
    opStash,   \* the operator's stash, while live (ghost)
    opHead,    \* item -> the operator's HEAD, while live (ghost)
    reread,    \* a journaled item's apply read content (ghost, R25)
    badLine,   \* a custody word blamed the destination with no edit (ghost)
    bkSet,     \* the local branches and tags bulkload deleted (ghost)
    everImported, \* every instance ever imported (ghost)
    dur,       \* what of nat, dhead and dstash survives a power loss
    edSince,   \* item -> the components the operator edited since its run
               \* began, or since its pending intent was written (ghost)
    probe      \* probe_exchange's file pairs in the main's worktree root
               \* (each pass's own tag: a pass removes only its own)

vars == <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
          dindex, dhead, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
          outcome, why, pc, crashes, opN, fresh, opLive, opStash, opHead,
          reread, badLine, bkSet, everImported, dur, edSince, probe>>


OpComps == Refs \cup Paths \cup {"index"}

-----------------------------------------------------------------------------
(* State functions                                                         *)

Live(row) == row.has /\ row.oid # 0
LiveNames == {r \in Refs : nat[r] # 0}

\* A worktree's HEAD as converge::head_of reads it: a branch resolves to
\* its native value (0: unborn).
HeadOf(i) == IF dhead[i].sym = "det" THEN Det(dhead[i].oid)
             ELSE [sym |-> dhead[i].sym, oid |-> nat[dhead[i].sym]]

\* The branches worktree j holds (C checked_out_by): its HEAD's branch, and
\* the branch an in-progress rebase or bisect detached it from
\* (rebase-merge/head-name, rebase-apply/head-name, BISECT_START: Git's
\* is_worktree_being_rebased and _bisected; review 3 finding 2). The code
\* today reads `worktree list --porcelain` only (rebase_unguarded).
CheckedBy(j) == (IF dhead[j].sym # "det" THEN {dhead[j].sym} ELSE {})
                \cup (IF dhead[j].held # "none" /\ ~Mut("rebase_unguarded")
                      THEN {dhead[j].held} ELSE {})
\* Branches some destination worktree has checked out or holds.
Checked == UNION {CheckedBy(j) : j \in {x \in Items : dhead[x].on}}

HeadBranchOf(i, c) == IF i = "main" THEN caps[c].head ELSE caps[c].wt

Proj(i, s) == IF i = "main"
              THEN [refs |-> s.refs, head |-> s.head, stash |-> s.stash,
                    seats |-> s.seats, index |-> s.index]
              ELSE [refs |-> s.refs, stash |-> s.stash, wt |-> s.wt]

IsSuffix(s, t) == Len(s) <= Len(t) /\ SubSeq(t, Len(t) - Len(s) + 1, Len(t)) = s

\* The values bulkload landed (or is landing) at a component: a write of
\* one of these by the operator restores landed content and is no edit.
LandedRef(r) == (IF Live(refLed[r]) THEN {refLed[r].oid} ELSE {})
                \cup UNION {{op.old, op.new} : op \in {o \in refInt.ops : o.name = r}}
                \* The branch a pending intent moves with HEAD: old or new until
                \* HEAD is on it, then new only.
                \cup UNION {{wsInt[i].br.new}
                            \cup (IF dhead[i].sym = wsInt[i].hnew.sym THEN {} ELSE {wsInt[i].br.old}) :
                            i \in {j \in Items : wsInt[j].on /\ wsInt[j].br.on /\ wsInt[j].br.name = r}}
\* Under a pending intent a seat is old, new, or (between a type change's
\* removal and its addition) absent: a settle finishes any of them forward.
TypeChange(F, T) == IsFile(F) # IsFile(T) \/ (F = DIR) # (T = DIR)
LandedSeatAt(p) == {wsLed["main"].seats[p].v}
                 \cup (IF wsInt["main"].on
                       THEN LET F == caps[wsInt["main"].from].seats[p]
                                T == caps[wsInt["main"].to].seats[p]
                            IN {F, T} \cup (IF TypeChange(F, T) THEN {ABSENT} ELSE {})
                       ELSE {})
\* While the main's own pass runs, a seat's landed value is the one
\* bulkload holds at that point (review 3 finding 14): before the seat
\* steps of a converge, the ledger's; during them, the value after the
\* last of the seat's operations done so far (settling, before any: every
\* value the settle finishes forward); after them, the new one.
AfterOp(op) == CASE op.a \in {"Remove", "RmDir"} -> ABSENT
                 [] op.a = "MkDir" -> DIR
                 [] OTHER -> op.n
SeatDone(p) == {k \in 1..Len(pc.ops) : pc.ops[k].p = p /\ (k < pc.k \/ (k = pc.k /\ pc.sub = 1))}
PreSeats == {"w_import", "w_probe", "w_intent", "w_refs", "r_fin", "r_txd", "r_txw",
             "r_stash", "r_led"}
LandedSeat(p) ==
    LET busy == pc.st # "idle" /\ pc.i = "main"
        D == SeatDone(p)
    IN IF busy /\ pc.mode = "fresh" /\ pc.st \in PreSeats THEN {wsLed["main"].seats[p].v}
       ELSE IF busy /\ pc.st = "w_seats" /\ D # {}
            THEN {AfterOp(pc.ops[CHOOSE k \in D : \A j \in D : j <= k])}
       ELSE IF busy /\ pc.st = "w_seats" /\ pc.mode = "fresh" THEN {pc.F[p]}
       ELSE IF busy /\ pc.st \in {"w_index", "w_head", "w_brow", "w_landed"} THEN {pc.T[p]}
       ELSE LandedSeatAt(p)
LandedIndex == {wsLed["main"].index}
               \cup (IF wsInt["main"].on THEN {wsInt["main"].iold, caps[wsInt["main"].to].index} ELSE {})
LandedVals(c) == IF c \in Refs THEN LandedRef(c)
                 ELSE IF c = "index" THEN LandedIndex ELSE LandedSeat(c)
LandedStash == (IF stashLed.has THEN {stashLed.order} ELSE {})
               \cup (IF refInt.sop.on THEN {refInt.sop.old, refInt.sop.new} ELSE {})
\* HEAD: the value bulkload holds for worktree i now (review 3 finding 14).
\* While i's own fresh converge is in flight before set_head, only the
\* proved HEAD; after set_head, only the new one (a switch back to the old
\* value then is an edit: the next proof refuses it); otherwise the
\* ledger's, and a pending intent's old and new (a settle finishes either).
FreshPreHead == {"w_import", "w_probe", "w_intent", "w_refs", "r_fin", "r_txd",
                 "r_txw", "r_stash", "r_led", "w_seats", "w_index", "w_head"}
LandedHead(i) ==
    LET busy == pc.st # "idle" /\ pc.i = i IN
    IF busy /\ pc.st \in {"w_brow", "w_landed"} THEN {pc.hnew}
    ELSE IF busy /\ pc.mode = "fresh" /\ pc.st \in FreshPreHead THEN {pc.hold}
    ELSE {wsLed[i].head} \cup (IF wsInt[i].on THEN {wsInt[i].hold, wsInt[i].hnew} ELSE {})

\* The components whose operator edit makes item i's proof fail.
ItemCause(i) ==
    \/ opHead[i].on
    \/ (wsLed[i].head.sym # "det" /\ opLive[wsLed[i].head.sym].on)
    \/ (i = "main" /\ \E p \in Paths : opLive[p].on)
    \/ (i = "main" /\ IndexOn /\ opLive["index"].on)

\* What justifies item i's INTERRUPTED at each kind of step (Interrupted):
\* an operator edit of the component since i's run began or its intent
\* was written, still live, or a Git lock left on it.
Ed(i, c) == c \in edSince[i] /\ opLive[c].on
EdHead(i, j) == ("HEAD:" \o j) \in edSince[i] /\ opHead[j].on
RefEdited(i, r) == \/ Ed(i, r)
                   \/ r \in locks
                   \/ \E q \in Refs : Conflicting(q, r) /\ Ed(i, q)
                   \/ \E j \in Items : dhead[j].on /\ EdHead(i, j) /\ r \in CheckedBy(j)
RefWhy(i, T) == \E op \in T : RefEdited(i, op.name)
SeatWhy(i) == \E q \in Paths : Ed(i, q)
IndexWhy(i) == IndexOn /\ Ed(i, "index")
HeadWhy(i, hold, hnew, br) ==
    \/ EdHead(i, i)
    \/ ("HEAD:" \o i) \in locks
    \/ \E r \in ({dhead[i].sym, hold.sym, hnew.sym} \cup (IF br.on THEN {br.name} ELSE {})) \cap Refs :
          RefEdited(i, r)

Clean == /\ \A c \in OpComps : ~opLive[c].on
         /\ ~opStash.on
         /\ \A i \in Items : ~opHead[i].on

-----------------------------------------------------------------------------
(* The per-ref decision (N decide, design 7.2 as amended).                 *)

Keep(line) == [w |-> "none", old |-> 0, new |-> 0, set |-> FALSE, row |-> NoRow,
               line |-> line]
SetRow(w, old, new, v, t, line) ==
    [w |-> w, old |-> old, new |-> new, set |-> TRUE,
     row |-> [has |-> TRUE, oid |-> v, t |-> t], line |-> line]

Decide(native, row, carried, t, class, checked) ==
    LET live == Live(row)
        tomb == row.has /\ row.oid = 0
    IN IF checked THEN Keep("checked-out-elsewhere")
       ELSE IF tomb /\ row.t >= t /\ carried # 0 THEN Keep("stale")
       ELSE IF native = 0 /\ carried = 0 /\ tomb /\ row.t < t
            THEN SetRow("none", 0, 0, 0, t, "none")
       ELSE IF ~live /\ carried = 0 THEN Keep("none")
       ELSE IF native = 0 /\ ~live THEN SetRow("create", 0, carried, carried, t, "created")
       ELSE IF native = 0 THEN Keep("deleted-at-destination")
       ELSE IF ~live /\ carried = native THEN SetRow("none", 0, 0, native, t, "adopted")
       ELSE IF ~live THEN Keep("kept-destination")
       ELSE IF native # row.oid THEN Keep("modified-at-destination")
       ELSE IF carried = native
            THEN IF t > row.t THEN SetRow("none", 0, 0, native, t, "none")
                 ELSE Keep("none")
       ELSE IF t <= row.t THEN Keep("stale")
       ELSE IF carried # 0 THEN SetRow("update", native, carried, carried, t, "moved")
       ELSE IF class = "remote" \/ Mut("prune_local")
            THEN SetRow("delete", native, 0, 0, t, "pruned")
       ELSE Keep("deleted-at-source")

\* N plan: every ref's decision, a create kept back when its name sits
\* above or below a name that exists after the plan (review 2 finding 3).
Plan(native, led, carried, t, skip) ==
    LET D0 == [r \in Refs |-> Decide(native[r], led[r], carried[r], t, Class(r), r \in skip)]
        after == ({r \in Refs : native[r] # 0} \ {r \in Refs : D0[r].w = "delete"})
                 \cup {r \in Refs : D0[r].w = "create"}
        D == [r \in Refs |->
                IF D0[r].w = "create" /\ ~Mut("name_conflict_unchecked")
                   /\ NameConflict(r, after \ {r})
                THEN Keep("name-conflict") ELSE D0[r]]
    IN [ops |-> {[name |-> r, old |-> D[r].old, new |-> D[r].new, before |-> led[r]] :
                 r \in {q \in Refs : D[q].w # "none"}},
        led |-> [r \in Refs |-> IF D[r].set THEN D[r].row ELSE led[r]],
        lines |-> [r \in Refs |-> D[r].line]]

\* N stash_decision (design 7.3 as amended by finding 4).
StashDecide(dst, sled, carried, t) ==
    LET op == [on |-> TRUE, old |-> dst, new |-> carried]
        row == [has |-> TRUE, order |-> carried, t |-> t]
        none(word) == [op |-> NoSop, word |-> word, sled |-> sled]
    IN IF ~sled.has
       THEN IF carried = << >> THEN none("none")
            ELSE IF dst = << >> THEN [op |-> op, word |-> "stash-restored", sled |-> row]
            ELSE none("stash-kept-destination")
       ELSE IF dst # sled.order THEN none("stash-modified-at-destination")
       ELSE IF sled.order = carried THEN none("none")
       ELSE IF t <= sled.t THEN none("stash-stale")
       ELSE [op |-> op, word |-> "stash-appended", sled |-> row]

\* One item's native ref step for capture c (C Place::refs, N step).
StepOf(c, skip) ==
    LET Pl == Plan(nat, refLed, caps[c].refs, c, skip)
        Sd == StashDecide(dstash, stashLed, caps[c].stash, c)
    IN [ops |-> Pl.ops, sop |-> Sd.op, led |-> Pl.led, sled |-> Sd.sled,
        lines |-> Pl.lines, sword |-> Sd.word]

\* The skip set of item i's ref step at capture c: every branch some
\* worktree has checked out (finding 1) and the branch this checkout's HEAD
\* moves to (review 2 finding 1).
SkipFor(i, c) == (IF Mut("no_checkout_guard") THEN {} ELSE Checked)
                 \cup (IF Mut("head_branch_in_step") THEN {} ELSE {HeadBranchOf(i, c)})

\* A custody word that blames the destination with no operator edit there.
BadLines(S) == \/ \E r \in Refs : S.lines[r] \in {"modified-at-destination", "deleted-at-destination"}
                                  /\ ~opLive[r].on
               \/ (S.sword = "stash-modified-at-destination" /\ ~opStash.on)

\* N transact's compare-and-swap: every line's old value holds, no lock is
\* left on it, and a create sits above or below no existing name (Git
\* checks the names that exist before the transaction). With
\* `symref-verify` lines for every worktree's HEAD (review 3 finding 10),
\* a branch some worktree checked out since the plan fails the
\* transaction; the code today has none (txn_no_symref_verify, and in the
\* settle settle_moves_checked_out).
TxnOK(T) == \A op \in T :
    /\ nat[op.name] = op.old \/ Mut("txn_no_cas")
    /\ op.name \notin locks
    /\ \/ op.name \notin Checked
       \/ Mut("txn_no_symref_verify")
       \/ (pc.fin /\ Mut("settle_moves_checked_out"))
    /\ (op.old = 0 /\ op.new # 0) =>
          ~NameConflict(op.name, LiveNames \cup {o.name : o \in {x \in T : x.new # 0 /\ x.name # op.name}})
TxnApply(T) == [r \in Refs |-> IF \E op \in T : op.name = r
                               THEN (CHOOSE op \in T : op.name = r).new ELSE nat[r]]
\* The ghost of the local branches and tags bulkload deleted (a
\* transaction's delete lines of a ref that is not remote-tracking).
BkSet(T) == bkSet \cup {op.name : op \in {o \in T : o.new = 0 /\ Class(o.name) # "remote"}}

-----------------------------------------------------------------------------
(* The workspace: delta, proof, head plan                                  *)

\* C delta's execution order: removals deepest first, directory removals,
\* directory creations, then replacements and additions in path order.
SeatOrder == << <<"Remove", "d/f">>, <<"Remove", "d">>, <<"RmDir", "d">>,
                <<"MkDir", "d">>, <<"Replace", "d">>, <<"Add", "d">>,
                <<"Replace", "d/f">>, <<"Add", "d/f">> >>
Needs(a, o, n) ==
    IF a = "Replace" THEN IsFile(o) /\ IsFile(n) /\ o # n
    ELSE IF a = "Remove" THEN IsFile(o) /\ ~IsFile(n)
    ELSE IF a = "RmDir" THEN o = DIR /\ n # DIR
    ELSE IF a = "MkDir" THEN n = DIR /\ o # DIR
    ELSE IsFile(n) /\ ~IsFile(o)
Delta(F, T) ==
    LET S == SelectSeq(SeatOrder, LAMBDA x : x[2] \in Paths /\ Needs(x[1], F[x[2]], T[x[2]]))
    IN [k \in 1..Len(S) |-> [a |-> S[k][1], p |-> S[k][2], o |-> F[S[k][2]], n |-> T[S[k][2]]]]

\* C prove (design 4.2 as amended): HEAD, the index entries, and every
\* landed seat: a print equal to the ledger's and not racy is trusted;
\* otherwise the content must be the landed content (Q144: content, not
\* stat).
SeatProved(p, s) ==
    IF s.v = ABSENT THEN TRUE
    ELSE IF s.v = DIR THEN dseat[p].v = DIR
    ELSE IF Mut("proof_stat_strict") THEN dseat[p].st = s.st /\ IsFile(dseat[p].v)
    ELSE IF dseat[p].st = s.st /\ IsFile(dseat[p].v) /\ (~s.racy \/ Mut("proof_stat_only"))
         THEN TRUE
    ELSE dseat[p].v = s.v
Proved(i) ==
    \/ Mut("overwrite_modified")
    \/ /\ HeadOf(i) = wsLed[i].head
       /\ (i = "main" /\ IndexOn) => dindex = wsLed[i].index
       /\ i = "main" => \A p \in Paths : SeatProved(p, wsLed[i].seats[p])
Prints == [p \in Paths |-> dseat[p].st]

\* C converge's checks after the import and before the intent: a third
\* party's seat where the source adds one, and (review 2 findings 2, 4) a
\* landed directory the source turns into a file that holds an entry that
\* is not a landed seat.
Clash(ops, F) == \E k \in 1..Len(ops) :
    LET op == ops[k] IN
    \/ /\ op.a \in {"Add", "MkDir"} /\ F[op.p] = ABSENT
       /\ dseat[op.p].v # ABSENT /\ (op.a = "Add" \/ dseat[op.p].v # DIR)
    \/ /\ op.a = "Add" /\ op.p = "d/f" /\ F["d/f"] = ABSENT
       /\ IsFile(dseat["d"].v) /\ F["d"] = ABSENT
    \/ /\ ~Mut("dir_replace_unchecked")
       /\ op.a = "Add" /\ op.p = "d" /\ F["d"] = DIR /\ "d/f" \in Paths
       /\ dseat["d"].v = DIR /\ dseat["d/f"].v # ABSENT /\ F["d/f"] = ABSENT

\* C head_plan (findings 11, 17; review 2 finding 3): the source's branch
\* when bulkload may put HEAD on it, otherwise detached at the captured head.
HeadPlan(i, c) ==
    LET b == HeadBranchOf(i, c)
        h1 == caps[c].refs[b]
        others == UNION {CheckedBy(j) : j \in {x \in Items \ {i} : dhead[x].on}}
        cur == nat[b]
        d == Decide(cur, refLed[b], h1, c, "local", FALSE)
        att(br, row) == [head |-> [sym |-> b, oid |-> h1], br |-> br, row |-> row, line |-> "none"]
        det(line) == [head |-> Det(h1), br |-> NoBr, row |-> [set |-> FALSE, r |-> NoRow], line |-> line]
    IN IF b \in others THEN det("head-detached-ref-conflict")
       ELSE IF cur = 0 /\ NameConflict(b, LiveNames) THEN det("head-detached-ref-conflict")
       ELSE IF d.w = "create"
            THEN att([on |-> TRUE, name |-> b, old |-> 0, new |-> h1], [set |-> TRUE, r |-> d.row])
       ELSE IF d.w = "update"
            THEN att([on |-> TRUE, name |-> b, old |-> cur, new |-> h1], [set |-> TRUE, r |-> d.row])
       ELSE IF cur = h1 THEN att(NoBr, [set |-> d.set, r |-> d.row])
       ELSE det(IF d.line = "stale" THEN "head-detached-ref-newer" ELSE "head-detached-ref-conflict")

\* C set_head: HEAD and its branch in one update-ref transaction, each line
\* a compare-and-swap; a branch already at its new value is dropped (review
\* 2 finding 1).
BrLine(br) == br.on /\ (Mut("head_branch_in_step") \/ nat[br.name] # br.new)
HeadLine(i, hold, hnew) == ~(hold.sym # "det" /\ hold.sym = hnew.sym)
                           /\ (hnew.sym # "det" \/ hold.sym # "det" \/ hold.oid # hnew.oid)
\* head_no_cas: neither line carries its old value (review 3 finding 11).
HeadTxnOK(i, hold, hnew, br) ==
    LET cur == HeadOf(i) IN
    /\ BrLine(br) =>
          /\ nat[br.name] = br.old \/ Mut("head_no_cas")
          /\ br.name \notin locks
          /\ br.old = 0 => ~NameConflict(br.name, LiveNames \ {br.name})
    /\ HeadLine(i, hold, hnew) =>
          /\ ("HEAD:" \o i) \notin locks
          /\ \/ Mut("head_no_cas")
             \/ IF hnew.sym = "det" THEN cur.oid = hold.oid
                ELSE IF hold.sym # "det" THEN cur.sym = hold.sym
                ELSE cur.sym = "det" /\ cur.oid = hold.oid
NewDhead(hnew) == IF hnew.sym = "det" THEN [on |-> TRUE, sym |-> "det", oid |-> hnew.oid, held |-> "none"]
                  ELSE [on |-> TRUE, sym |-> hnew.sym, oid |-> 0, held |-> "none"]

\* C next_ledger: written seats' prints now, kept seats' earlier prints
\* (the proof's when fresh, the old ledger's when settling), others now.
\* The index digest is the one publish_index installed (review 3 finding 3);
\* the code today re-reads the index after publishing it, so an operator's
\* `git add` in between is recorded as landed (ledger_reads_index).
NextLedger(i, c, hnew) ==
    [has |-> TRUE, t |-> c, head |-> hnew,
     index |-> IF i # "main" THEN 0
               ELSE IF Mut("ledger_reads_index") THEN dindex ELSE caps[c].index,
     seats |-> [p \in Paths |->
        LET n == caps[c].seats[p] IN
        IF i # "main" \/ n = ABSENT THEN NoSeatRec
        ELSE IF p \in pc.wr THEN [v |-> n, st |-> pc.ws[p], racy |-> p \in fresh]
        ELSE IF n # DIR /\ pc.F[p] # ABSENT
             THEN [v |-> n, st |-> IF pc.mode = "fresh" THEN pc.P[p]
                                   ELSE wsLed[i].seats[p].st,
                   racy |-> p \in fresh]
        ELSE [v |-> n, st |-> dseat[p].st, racy |-> p \in fresh]]]

\* Durability of Git's writes (review 3 finding 7). update-ref renames a
\* ref's lock into place under core.fsync=reference but never fsyncs the
\* directory; a power loss can undo the rename although the ledgers, which
\* bulkload writes with a directory fsync, record it. The rule: bulkload
\* syncs the directories its Git writes touched before any ledger write,
\* so each of its Git writes is durable at once. The code today does not
\* (refs_unsynced). The operator's own Git writes are taken as durable.
\* (Each of these is an action's last conjunct: it reads the new state.)
DurNow == [nat |-> nat', head |-> dhead', stash |-> dstash']
GitSync == dur' = IF Mut("refs_unsynced") THEN dur ELSE DurNow
OpDur == dur' = [nat |-> [r \in Refs |-> IF nat'[r] # nat[r] THEN nat'[r] ELSE dur.nat[r]],
                 head |-> [i \in Items |-> IF dhead'[i] # dhead[i] THEN dhead'[i] ELSE dur.head[i]],
                 stash |-> IF dstash' # dstash THEN dstash' ELSE dur.stash]

-----------------------------------------------------------------------------
(* Protocol                                                                *)

RefuseAs(i, code, j) ==
    /\ pc' = Idle
    /\ outcome' = [outcome EXCEPT ![i] = code]
    /\ why' = [why EXCEPT ![i] = j]
Refuse(i, code) == RefuseAs(i, code, CASE code = "MODIFIED" -> ItemCause(i)
                                       [] code = "STALE" -> TRUE
                                       [] OTHER -> FALSE)
\* GIT_CONVERGE_INTERRUPTED, justified by j: an operator edit of a
\* component the failing step writes, since the run began or the intent
\* was written and still live, or a Git lock left on it (review 3 finding
\* 6). An edit elsewhere, an edit put back, an edit older than the run,
\* or a crash with no lock left does not justify it.
Interrupted(i, j) == RefuseAs(i, "INTERRUPTED", j)

\* E capture_item: a new {item}.capture at the next instant when the
\* item's projection of the source moved; otherwise the capture reuses
\* (0 source bytes). A removed source worktree's capture refuses (the S4
\* lane's GIT_SOURCE_VANISHED) and keeps its record.
Capture(i) ==
    /\ i = "wt" => src.wt # "none"
    /\ Proj(i, src) # Proj(i, caps[capt[i]])
    /\ caps' = Append(caps, src)
    /\ capt' = [capt EXCEPT ![i] = Len(caps) + 1]
    /\ UNCHANGED <<src, srcN, nextV, nat, locks, dstash, dseat, temp, dindex, dhead,
                   prov, refLed, stashLed, refInt, wsLed, wsInt, jour, outcome, why,
                   pc, crashes, opN, fresh, opLive, opStash, opHead, reread,
                   badLine, bkSet, everImported, dur, probe, edSince>>

RefSettle(i, ret) == [Idle EXCEPT !.st = "r_fin", !.i = i, !.c = capt[i], !.cur = capt[i],
                                 !.mode = "settle", !.ret = ret, !.fin = FALSE,
                                 !.nsled = refInt.sled]

\* E apply_item and C apply_workspace up to the lock: A4's journal no-op
\* (metadata only; a sibling's ref intent is settled without proving,
\* review 2 finding 7), A5's first landing, finding 13, A12, and
\* native_refs::settle before anything else.
Begin(i) ==
    /\ pc.st = "idle"
    /\ i \in Items
    \* Mains run a level before their linked worktrees (E after_mains): a
    \* linked item starts while the main's intent is pending only after the
    \* main refused in this pass.
    /\ i = "wt" => (wsInt["main"].on => outcome["main"] \in Refusals)
    /\ LET c == capt[i]
           noop == c \in jour[i] /\ ~wsInt[i].on
           full == ~noop \/ Mut("noop_restages") \/ (refInt.on /\ Mut("sibling_intent_reproves"))
       IN /\ noop => (refInt.on \/ Mut("noop_restages"))
          /\ (Mut("noop_restages") /\ noop /\ ~refInt.on) => ~reread
          /\ reread' = (reread \/ (noop /\ full))
          \* C apply_workspace, under the lock and before anything else:
          \* the probe files a crashed pass left are removed (review 3
          \* finding 9; the code today never does: probe_left).
          /\ edSince' = IF wsInt[i].on THEN edSince ELSE [edSince EXCEPT ![i] = {}]
          /\ probe' = IF /\ full /\ i = "main" /\ dhead[i].on /\ wsLed[i].has
                         /\ ~Mut("probe_left")
                      THEN 0 ELSE probe
          /\ IF ~full
             THEN /\ pc' = RefSettle(i, "a4")
                  /\ UNCHANGED <<outcome, why>>
             ELSE IF ~dhead[i].on
             THEN IF jour[i] # {} THEN Refuse(i, "MODIFIED")
                  ELSE IF i = "main"
                  THEN /\ pc' = [Idle EXCEPT !.st = "f_land", !.i = i, !.c = c, !.cur = c]
                       /\ UNCHANGED <<outcome, why>>
                  ELSE /\ dhead["main"].on
                       /\ pc' = IF refInt.on THEN RefSettle(i, "f_step")
                                ELSE [Idle EXCEPT !.st = "f_step", !.i = i, !.c = c, !.cur = c]
                       /\ UNCHANGED <<outcome, why>>
             ELSE IF ~wsLed[i].has THEN Refuse(i, "OCCUPIED")
             ELSE IF refInt.on /\ ~Mut("converge_before_ref_settle")
             THEN /\ pc' = RefSettle(i, "w_decide")
                  /\ UNCHANGED <<outcome, why>>
             ELSE /\ pc' = [Idle EXCEPT !.st = "w_decide", !.i = i, !.c = c, !.cur = c]
                  /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, wsLed, wsInt,
                   jour, crashes, opN, fresh, opLive, opStash, opHead, badLine,
                   bkSet, everImported, dur>>

\* git_carry.rs restore_staged_landed: a standalone first landing in its
\* private stage (native refs, stash, HEAD, both ledgers), published with
\* no-replace: atomic.
FirstLandMain ==
    /\ pc.st = "f_land"
    /\ LET c == pc.c
           s == caps[c]
       IN /\ nat' = s.refs
          /\ refLed' = [r \in Refs |-> IF s.refs[r] # 0 THEN [has |-> TRUE, oid |-> s.refs[r], t |-> c]
                                      ELSE NoRow]
          /\ dstash' = s.stash
          /\ stashLed' = IF s.stash = << >> THEN NoSLed ELSE [has |-> TRUE, order |-> s.stash, t |-> c]
          /\ dseat' = [p \in Paths |-> [v |-> s.seats[p], st |-> 1]]
          /\ fresh' = {p \in Paths : s.seats[p] # ABSENT}
          /\ dindex' = s.index
          /\ dhead' = [dhead EXCEPT !["main"] = [on |-> TRUE, sym |-> s.head, oid |-> 0, held |-> "none"]]
          /\ wsLed' = [wsLed EXCEPT !["main"] =
                [has |-> TRUE, t |-> c, head |-> [sym |-> s.head, oid |-> s.refs[s.head]],
                 index |-> s.index,
                 seats |-> [p \in Paths |-> IF s.seats[p] = ABSENT THEN NoSeatRec
                                            ELSE [v |-> s.seats[p], st |-> 1, racy |-> TRUE]]]]
          /\ prov' = prov \cup {c}
          /\ everImported' = everImported \cup {c}
          /\ bkSet' = bkSet
          /\ pc' = [pc EXCEPT !.st = "w_journal"]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, locks, temp, refInt, wsInt, jour,
                   outcome, why, crashes, opN, opLive, opStash, opHead, reread,
                   badLine, probe, edSince>>
    /\ dur' = DurNow

\* Start a native ref step (C Place::refs, N step and commit): with writes,
\* the ref intent first, then the transactions; without, the ledger only.
StartStep(c, skip, ret) ==
    LET S == StepOf(c, skip)
        writes == S.ops # {} \/ S.sop.on
    IN /\ badLine' = (badLine \/ BadLines(S))
       /\ IF writes
          THEN /\ refInt' = [on |-> TRUE, ops |-> S.ops, sop |-> S.sop, led |-> S.led, sled |-> S.sled]
               /\ pc' = [pc EXCEPT !.st = "r_txd", !.again = S.ops, !.fin = FALSE,
                                   !.nled = S.led, !.nsled = S.sled, !.sgo = FALSE,
                                   !.ret = ret]
               /\ UNCHANGED <<refLed, stashLed>>
          ELSE /\ refLed' = S.led
               /\ stashLed' = S.sled
               /\ pc' = [pc EXCEPT !.st = ret]
               /\ UNCHANGED refInt

\* git_carry.rs restore_linked_staged_landed, C plan_linked: the import and
\* the native ref step before `worktree add`.
FirstStepWt ==
    /\ pc.st = "f_step"
    /\ prov' = prov \cup {pc.c}
    /\ everImported' = everImported \cup {pc.c}
    /\ StartStep(pc.c, Checked, "f_plan")
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, wsLed, wsInt, jour, outcome, why, crashes, opN,
                   fresh, opLive, opStash, opHead, reread, bkSet, dur, probe, edSince>>

\* C plan_linked: the HEAD plan; its branch line goes through
\* native_refs::commit (the ref intent, the compare-and-swap transaction,
\* finish, the ledger with the plan's row), so a crash or a killed Git
\* child inside it is settled like any ref step (review 3 finding 8).
FirstPlanWt ==
    /\ pc.st = "f_plan"
    /\ LET P == HeadPlan("wt", pc.c)
           led == IF P.row.set THEN [refLed EXCEPT ![HeadBranchOf("wt", pc.c)] = P.row.r]
                  ELSE refLed
           ops == {[name |-> P.br.name, old |-> P.br.old, new |-> P.br.new,
                    before |-> refLed[P.br.name]]}
       IN IF P.br.on
          THEN /\ refInt' = [on |-> TRUE, ops |-> ops, sop |-> NoSop, led |-> led,
                             sled |-> stashLed]
               /\ pc' = [pc EXCEPT !.st = "r_txd", !.again = ops, !.fin = FALSE,
                                   !.nled = led, !.nsled = stashLed, !.sgo = FALSE,
                                   !.ret = "f_add", !.plan = P]
               /\ UNCHANGED refLed
          ELSE /\ refLed' = led
               /\ pc' = [pc EXCEPT !.st = "f_add", !.plan = P]
               /\ UNCHANGED refInt
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, stashLed, wsLed, wsInt, jour, outcome, why,
                   crashes, opN, fresh, opLive, opStash, opHead, reread, badLine,
                   bkSet, everImported, dur, probe, edSince>>

\* `git worktree add --no-checkout` and the checkout: the worktree exists.
FirstAddWt ==
    /\ pc.st = "f_add"
    /\ dhead' = [dhead EXCEPT !["wt"] = NewDhead(pc.plan.head)]
    /\ pc' = [pc EXCEPT !.st = "f_ledger"]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, badLine, bkSet, everImported, probe, edSince>>
    /\ dur' = DurNow

\* C finish_landing: the linked worktree's ledger.
FirstLedgerWt ==
    /\ pc.st = "f_ledger"
    /\ wsLed' = [wsLed EXCEPT !["wt"] = [NoWsLed EXCEPT !.has = TRUE, !.t = pc.c,
                                                       !.head = pc.plan.head]]
    /\ pc' = [pc EXCEPT !.st = "w_journal"]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, wsInt, jour,
                   outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, badLine, bkSet, everImported, dur, probe, edSince>>

\* C head_of reads HEAD with `rev-parse --verify HEAD`, which fails on an
\* unborn branch (an orphan switch, `update-ref -d` of the checked-out
\* branch); prove (A6 and A9) and settle pass that error on as
\* GIT_CHILD_FAILED. The rule (review 3 finding 4): an unborn HEAD reads as
\* a HEAD with no oid, which the proof refuses MODIFIED and the settle
\* INTERRUPTED. The code today: unborn_head_untyped.
Unborn(i) == dhead[i].sym # "det" /\ nat[dhead[i].sym] = 0
UnbornUntyped(i) == Mut("unborn_head_untyped") /\ Unborn(i)

\* C apply_workspace after the ref settle: a pending workspace intent is
\* settled first (A3); then A6 (the ledger already names the capture:
\* prove, then the journal), A7 (stale), the proof (A9), or on to A10.
Decide_ ==
    /\ pc.st = "w_decide"
    /\ LET i == pc.i
           c == pc.cur
           L == wsLed[i]
           I == wsInt[i]
       IN IF I.on
          THEN /\ pc' = [pc EXCEPT !.st = "s_refs", !.mode = "settle", !.c = I.to,
                                   !.F = caps[L.t].seats, !.T = caps[I.to].seats,
                                   !.ops = IF i = "main" THEN Delta(caps[L.t].seats, caps[I.to].seats)
                                           ELSE << >>,
                                   !.k = 1, !.sub = 0, !.wr = {},
                                   !.hold = I.hold, !.hnew = I.hnew, !.br = I.br,
                                   !.iold = I.iold]
               /\ UNCHANGED <<outcome, why>>
          ELSE IF L.t = c
          THEN IF UnbornUntyped(i) THEN RefuseAs(i, "UNTYPED", FALSE)
               ELSE IF Proved(i)
               THEN /\ pc' = [pc EXCEPT !.st = "w_journal"]
                    /\ UNCHANGED <<outcome, why>>
               ELSE Refuse(i, "MODIFIED")
          ELSE IF c < L.t THEN Refuse(i, "STALE")
          ELSE IF UnbornUntyped(i) THEN RefuseAs(i, "UNTYPED", FALSE)
          ELSE IF ~Proved(i) THEN Refuse(i, "MODIFIED")
          ELSE /\ pc' = [pc EXCEPT !.st = "w_import", !.mode = "fresh", !.c = c,
                                   !.P = Prints, !.F = caps[L.t].seats, !.T = caps[c].seats,
                                   !.ops = IF i = "main" THEN Delta(caps[L.t].seats, caps[c].seats)
                                           ELSE << >>,
                                   !.k = 1, !.sub = 0, !.wr = {}, !.hold = L.head,
                                   !.iold = L.index]
               /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, wsLed, wsInt,
                   jour, crashes, opN, fresh, opLive, opStash, opHead, reread,
                   badLine, bkSet, everImported, dur, probe, edSince>>

\* C converge: import the capture (additive), then refuse a third party's
\* seat where the source adds one; with a replacement to make, the probe
\* (probe_exchange) comes next, otherwise the HEAD plan.
HasReplace(ops) == \E k \in 1..Len(ops) : ops[k].a = "Replace"
Import ==
    /\ pc.st = "w_import"
    /\ prov' = prov \cup {pc.c}
    /\ everImported' = everImported \cup {pc.c}
    /\ IF Clash(pc.ops, pc.F)
       THEN Refuse(pc.i, "MODIFIED") /\ UNCHANGED probe
       ELSE IF HasReplace(pc.ops)
       THEN /\ probe' = probe + 1
            /\ pc' = [pc EXCEPT !.st = "w_probe"]
            /\ UNCHANGED <<outcome, why>>
       ELSE /\ pc' = [pc EXCEPT !.st = "w_intent", !.plan = HeadPlan(pc.i, pc.c)]
            /\ UNCHANGED <<outcome, why, probe>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   crashes, opN, fresh, opLive, opStash, opHead, reread, badLine,
                   bkSet, dur, edSince>>

\* C probe_exchange: the two probe files in the worktree root are
\* exchanged and removed, before the intent (a crash between leaves them:
\* only the next apply's sweep in Begin removes them); then the HEAD plan.
ProbeExchange ==
    /\ pc.st = "w_probe"
    /\ probe' = probe - 1
    /\ pc' = [pc EXCEPT !.st = "w_intent", !.plan = HeadPlan(pc.i, pc.c)]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, badLine, bkSet, everImported, dur, edSince>>

\* C converge: the workspace intent, durable before any destination write.
WriteIntent ==
    /\ pc.st = "w_intent"
    /\ wsInt' = IF Mut("no_intent") THEN wsInt
                ELSE [wsInt EXCEPT ![pc.i] =
                        [on |-> TRUE, from |-> wsLed[pc.i].t, to |-> pc.c,
                         hold |-> pc.hold, hnew |-> pc.plan.head, br |-> pc.plan.br,
                         iold |-> pc.iold]]
    /\ pc' = [pc EXCEPT !.st = "w_refs", !.hnew = pc.plan.head, !.br = pc.plan.br]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, wsLed, jour,
                   outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, badLine, bkSet, everImported, dur, probe, edSince>>

\* C Place::refs inside a converge (w_refs) or a settle (s_refs).
RefStep ==
    /\ pc.st \in {"w_refs", "s_refs"}
    /\ StartStep(pc.c, SkipFor(pc.i, pc.c), "w_seats")
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, wsLed, wsInt, jour, outcome, why, crashes,
                   opN, fresh, opLive, opStash, opHead, reread, bkSet, everImported,
                   dur, probe, edSince>>

\* N finish: each op whose ref holds its new value is done, one at its old
\* value is redone, a create Git can never make is dropped (review 2
\* finding 3), anything else is the destination's: its row restored.
\* Review 3: an op on a branch some worktree now has checked out (or holds
\* for a rebase) is never redone; its row is restored and it is reported
\* checked-out-elsewhere (finding 10; the code today redoes it:
\* settle_moves_checked_out). The names that exist after the step are the
\* native ones less the deletes that will run, plus the creates that will
\* (finding 1; the code today removes every delete's name, whether it runs
\* or not: finish_after_all_deletes).
RefFinish ==
    /\ pc.st = "r_fin"
    /\ LET I == refInt
           chk(op) == op.name \in Checked /\ ~Mut("settle_moves_checked_out")
           runs(op) == nat[op.name] # op.new /\ nat[op.name] = op.old /\ ~chk(op)
           after == IF Mut("finish_after_all_deletes")
                    THEN LiveNames \ {op.name : op \in {o \in I.ops : o.new = 0}}
                    ELSE (LiveNames \ {op.name : op \in {o \in I.ops : o.new = 0 /\ runs(o)}})
                         \cup {op.name : op \in {o \in I.ops : o.old = 0 /\ o.new # 0 /\ runs(o)}}
           blocked(op) == /\ op.old = 0 /\ nat[op.name] = 0
                          /\ ~Mut("name_conflict_unchecked")
                          /\ NameConflict(op.name, after \ {op.name})
           again == {op \in I.ops : runs(op) /\ ~blocked(op)}
           bad == {op \in I.ops : nat[op.name] # op.new /\ ~(runs(op) /\ ~blocked(op))}
           nled == [r \in Refs |-> IF \E op \in bad : op.name = r
                                   THEN (CHOOSE op \in bad : op.name = r).before
                                   ELSE I.led[r]]
       IN /\ pc' = [pc EXCEPT !.st = "r_txd", !.again = again, !.fin = TRUE, !.nled = nled]
          /\ badLine' = (badLine \/ \E op \in bad : ~blocked(op) /\ ~chk(op) /\ ~opLive[op.name].on)
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, wsLed, wsInt,
                   jour, outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, bkSet, everImported, dur, probe, edSince>>

TxnDeletes == IF Mut("single_ref_txn") THEN pc.again ELSE {op \in pc.again : op.new = 0}
TxnWrites == IF Mut("single_ref_txn") THEN {} ELSE {op \in pc.again : op.new # 0}

\* N transact: the deletes in their own transaction first (review 2
\* finding 3), each line a compare-and-swap.
RefTxnDeletes ==
    /\ pc.st = "r_txd"
    /\ LET T == TxnDeletes IN
       IF T # {} /\ ~TxnOK(T)
       THEN /\ Interrupted(pc.i, RefWhy(pc.i, T))
            /\ UNCHANGED <<nat, bkSet>>
       ELSE /\ nat' = TxnApply(T)
            /\ bkSet' = BkSet(T)
            /\ pc' = [pc EXCEPT !.st = "r_txw", !.again = TxnWrites]
            /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, locks, dstash, dseat, temp, dindex,
                   dhead, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   crashes, opN, fresh, opLive, opStash, opHead, reread, badLine,
                   everImported, probe, edSince>>
    /\ GitSync

RefTxnWrites ==
    /\ pc.st = "r_txw"
    /\ LET T == pc.again IN
       IF T # {} /\ ~TxnOK(T)
       THEN /\ Interrupted(pc.i, RefWhy(pc.i, T))
            /\ UNCHANGED <<nat, bkSet>>
       ELSE /\ nat' = TxnApply(T)
            /\ bkSet' = BkSet(T)
            /\ pc' = [pc EXCEPT !.st = IF pc.fin THEN "r_stash" ELSE "r_fin"]
            /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, locks, dstash, dseat, temp, dindex,
                   dhead, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   crashes, opN, fresh, opLive, opStash, opHead, reread, badLine,
                   everImported, probe, edSince>>
    /\ GitSync

\* N stash_apply: re-read once; a reflog that is a suffix of the new order
\* is continued, one equal to the old order is deleted by compare-and-swap
\* and rebuilt, anything else is the destination's (finding 4). Every store
\* is a compare-and-swap on the tip this run saw.
StoreNext(op, dst) == << op.new[Len(op.new) - Len(dst)] >> \o dst
RefStash ==
    /\ pc.st = "r_stash"
    /\ LET op == refInt.sop
           dst == dstash
           fail == /\ pc' = [pc EXCEPT !.st = "r_led", !.nsled = NoSLed]
                   /\ badLine' = (badLine \/ ~opStash.on)
                   /\ UNCHANGED <<dstash>>
           go(s) == /\ dstash' = s
                    /\ pc' = [pc EXCEPT !.sgo = TRUE, !.sexp = s]
                    /\ UNCHANGED badLine
       IN IF ~op.on \/ dst = op.new
          THEN /\ pc' = [pc EXCEPT !.st = "r_led"]
               /\ UNCHANGED <<dstash, badLine>>
          \* A refs/stash.lock a killed store left fails every later store
          \* (stash_apply returns false; finish clears the stash row).
          ELSE IF "stash" \in locks THEN fail
          ELSE IF pc.sgo /\ dst # pc.sexp THEN fail
          ELSE IF IsSuffix(dst, op.new) THEN go(StoreNext(op, dst))
          ELSE IF ~pc.sgo /\ (dst = op.old \/ Mut("stash_clear_blind")) THEN go(<< >>)
          ELSE fail
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dseat, temp, dindex,
                   dhead, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, bkSet, everImported, probe, edSince>>
    /\ GitSync

\* N commit and settle's end: refs.landed written, refs.intent removed.
RefLedger ==
    /\ pc.st = "r_led"
    /\ refLed' = pc.nled
    /\ stashLed' = pc.nsled
    /\ refInt' = NoRefInt
    /\ IF pc.ret = "a4"
       THEN /\ pc' = Idle
            /\ outcome' = [outcome EXCEPT ![pc.i] = "done"]
            /\ why' = [why EXCEPT ![pc.i] = TRUE]
       ELSE /\ pc' = [pc EXCEPT !.st = pc.ret]
            /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, wsLed, wsInt, jour, crashes, opN, fresh,
                   opLive, opStash, opHead, reread, badLine, bkSet, everImported,
                   dur, probe, edSince>>

-----------------------------------------------------------------------------
(* The seat engine (C Engine): fresh, each seat checked against the        *)
(* proof's print; settling, each seat classified by its content.           *)

SeatsDone == pc.k > Len(pc.ops)
AfterSeats == IF pc.i = "main" /\ IndexOn THEN "w_index" ELSE "w_head"

SeatNext(wrote) ==
    pc' = [pc EXCEPT !.k = pc.k + 1, !.sub = 0, !.wr = pc.wr \cup wrote]
\* p written (or found new, settling) with print st: Engine's `written`
\* records the print when the seat is written, not when the ledger is.
SeatNextAt(p, st) ==
    pc' = [pc EXCEPT !.k = pc.k + 1, !.sub = 0, !.wr = pc.wr \cup {p}, !.ws[p] = st]

Write(p, v) == [dseat EXCEPT ![p] = [v |-> v, st |-> dseat[p].st + 1]]

SeatOp ==
    /\ pc.st = "w_seats" /\ pc.sub = 0
    /\ IF SeatsDone
       THEN /\ pc' = [pc EXCEPT !.st = AfterSeats]
            /\ UNCHANGED <<dseat, temp, fresh, outcome, why>>
       ELSE LET op == pc.ops[pc.k]
                p == op.p
                settle == pc.mode = "settle"
                cur == dseat[p]
                tp == temp[p]
                checked == Mut("exchange_unchecked") \/ cur.st = pc.P[p]
                \* Seat::at fails: the seat's parent is not a directory.
                orphan == p = "d/f" /\ dseat["d"].v # DIR
                \* Every refusal after the intent is INTERRUPTED (review 3
                \* finding 5); the code today passes Seat::at's error on
                \* (PATH_ESCAPES_ROOT, Io): seat_parent_untyped.
                halt == IF orphan /\ Mut("seat_parent_untyped")
                        THEN RefuseAs(pc.i, "UNTYPED", FALSE)
                        ELSE Interrupted(pc.i, SeatWhy(pc.i))
                stop == halt /\ UNCHANGED <<dseat, temp, fresh>>
                skip(wrote) == SeatNext(wrote) /\ UNCHANGED <<dseat, temp, fresh, outcome, why>>
            IN
            CASE op.a = "Remove" ->
                   IF orphan
                   THEN IF settle THEN skip({}) ELSE stop
                   ELSE IF settle /\ tp.v # ABSENT /\ tp.v # op.o
                   THEN \* sweep: a displaced file goes back when the seat is free
                        /\ temp' = IF cur.v = ABSENT THEN [temp EXCEPT ![p] = NoSeat] ELSE temp
                        /\ dseat' = IF cur.v = ABSENT THEN [dseat EXCEPT ![p] = tp] ELSE dseat
                        /\ halt /\ UNCHANGED fresh
                   ELSE IF cur.v = ABSENT
                   THEN IF settle
                        THEN SeatNext({}) /\ temp' = [temp EXCEPT ![p] = NoSeat]
                             /\ UNCHANGED <<dseat, fresh, outcome, why>>
                        ELSE stop
                   ELSE IF (settle /\ cur.v = op.o) \/ (~settle /\ checked)
                   THEN /\ dseat' = Write(p, ABSENT)
                        /\ temp' = [temp EXCEPT ![p] = NoSeat]
                        /\ SeatNext({}) /\ UNCHANGED <<fresh, outcome, why>>
                   ELSE stop
              [] op.a = "Replace" ->
                   IF orphan THEN stop
                   ELSE IF settle /\ tp.v # ABSENT /\ tp.v \notin {op.o, op.n}
                   THEN \* sweep: the displaced third-party file goes back
                        /\ dseat' = IF cur.v = op.n \/ cur.v = ABSENT
                                    THEN [dseat EXCEPT ![p] = tp] ELSE dseat
                        /\ temp' = IF cur.v = op.n \/ cur.v = ABSENT
                                   THEN [temp EXCEPT ![p] = NoSeat] ELSE temp
                        /\ halt /\ UNCHANGED fresh
                   ELSE IF settle /\ cur.v = op.n
                   THEN SeatNextAt(p, cur.st) /\ temp' = [temp EXCEPT ![p] = NoSeat]
                        /\ UNCHANGED <<dseat, fresh, outcome, why>>
                   ELSE IF (settle /\ cur.v = op.o) \/ (~settle /\ checked /\ cur.v # ABSENT)
                   THEN \* write the temporary, RENAME_EXCHANGE it with the seat
                        /\ temp' = [temp EXCEPT ![p] = cur]
                        /\ dseat' = [dseat EXCEPT ![p] = [v |-> op.n, st |-> cur.st + 1]]
                        /\ fresh' = fresh \cup {p}
                        /\ pc' = [pc EXCEPT !.sub = 1, !.P[p] = cur.st, !.ws[p] = cur.st + 1]
                        /\ UNCHANGED <<outcome, why>>
                   ELSE stop
              [] op.a = "Add" ->
                   IF orphan THEN stop
                   ELSE IF cur.v # ABSENT
                   THEN IF settle /\ cur.v = op.n
                        THEN SeatNextAt(p, cur.st) /\ UNCHANGED <<dseat, temp, fresh, outcome, why>>
                        ELSE stop
                   ELSE /\ dseat' = Write(p, op.n)
                        /\ fresh' = fresh \cup {p}
                        /\ SeatNextAt(p, cur.st + 1) /\ UNCHANGED <<temp, outcome, why>>
              [] op.a = "MkDir" ->
                   IF cur.v = DIR THEN skip({})
                   ELSE IF cur.v # ABSENT THEN stop
                   ELSE /\ dseat' = Write(p, DIR)
                        /\ SeatNext({}) /\ UNCHANGED <<temp, fresh, outcome, why>>
              [] OTHER -> \* RmDir: a directory still holding a destination entry stays
                   IF cur.v = DIR
                   THEN IF "d/f" \in Paths /\ dseat["d/f"].v # ABSENT
                        THEN skip({})
                        ELSE /\ dseat' = Write(p, ABSENT)
                             /\ SeatNext({}) /\ UNCHANGED <<temp, fresh, outcome, why>>
                   ELSE IF cur.v = ABSENT \/ settle THEN skip({})
                   ELSE stop
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dindex, dhead,
                   prov, refLed, stashLed, refInt, wsLed, wsInt, jour, crashes, opN,
                   opLive, opStash, opHead, reread, badLine, bkSet, everImported,
                   dur, probe, edSince>>

\* C Engine::replace after the exchange: the displaced file must be the
\* landed seat (the inode checked, its size and mtime); it is unlinked.
\* Anything else is exchanged back and the item refuses.
SeatVerify ==
    /\ pc.st = "w_seats" /\ pc.sub = 1
    /\ LET p == pc.ops[pc.k].p IN
       IF Mut("exchange_unchecked") \/ temp[p].st = pc.P[p]
       THEN /\ temp' = [temp EXCEPT ![p] = NoSeat]
            /\ SeatNext({p})
            /\ UNCHANGED <<dseat, outcome, why>>
       ELSE /\ dseat' = [dseat EXCEPT ![p] = temp[p]]
            /\ temp' = [temp EXCEPT ![p] = NoSeat]
            /\ Interrupted(pc.i, SeatWhy(pc.i))
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dindex, dhead,
                   prov, refLed, stashLed, refInt, wsLed, wsInt, jour, crashes, opN,
                   fresh, opLive, opStash, opHead, reread, badLine, bkSet,
                   everImported, dur, probe, edSince>>

\* C publish_index: under index.lock, compare-and-swap on the entries
\* (finding 3).
IndexPublish ==
    /\ pc.st = "w_index"
    /\ LET old == IF pc.mode = "fresh" THEN wsLed[pc.i].index ELSE pc.iold
           target == caps[pc.c].index
       IN IF dindex = target
          THEN /\ pc' = [pc EXCEPT !.st = "w_head"]
               /\ UNCHANGED <<dindex, outcome, why>>
          ELSE IF dindex # old /\ ~Mut("index_no_cas")
          THEN Interrupted(pc.i, IndexWhy(pc.i)) /\ UNCHANGED dindex
          ELSE /\ dindex' = target
               /\ pc' = [pc EXCEPT !.st = "w_head"]
               /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dhead, prov, refLed, stashLed, refInt, wsLed, wsInt, jour,
                   crashes, opN, fresh, opLive, opStash, opHead, reread, badLine,
                   bkSet, everImported, dur, probe, edSince>>

\* C set_head (fresh), or settle's check: HEAD at its new value is done,
\* at its old value is redone, anything else refuses.
HeadSet ==
    /\ pc.st = "w_head"
    /\ LET i == pc.i
           cur == HeadOf(i)
           skip == pc.mode = "settle" /\ cur = pc.hnew
       IN IF pc.mode = "settle" /\ UnbornUntyped(i)
          THEN RefuseAs(i, "UNTYPED", FALSE) /\ UNCHANGED <<nat, dhead, bkSet>>
          ELSE IF skip
          THEN /\ pc' = [pc EXCEPT !.st = "w_brow"]
               /\ UNCHANGED <<nat, dhead, bkSet, outcome, why>>
          ELSE IF (pc.mode = "settle" /\ cur # pc.hold) \/ ~HeadTxnOK(i, pc.hold, pc.hnew, pc.br)
          THEN /\ Interrupted(i, HeadWhy(i, pc.hold, pc.hnew, pc.br))
               /\ UNCHANGED <<nat, dhead, bkSet>>
          ELSE /\ nat' = IF BrLine(pc.br) THEN [nat EXCEPT ![pc.br.name] = pc.br.new] ELSE nat
               /\ bkSet' = bkSet
               /\ dhead' = IF HeadLine(i, pc.hold, pc.hnew) THEN [dhead EXCEPT ![i] = NewDhead(pc.hnew)]
                           ELSE dhead
               /\ pc' = [pc EXCEPT !.st = "w_brow"]
               /\ UNCHANGED <<outcome, why>>
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, locks, dstash, dseat, temp, dindex,
                   prov, refLed, stashLed, refInt, wsLed, wsInt, jour, crashes, opN,
                   fresh, opLive, opStash, opHead, reread, badLine, everImported,
                   probe, edSince>>
    /\ GitSync

\* C Place::branch_row: the HEAD branch's ref ledger row.
BranchRow ==
    /\ pc.st = "w_brow"
    /\ refLed' = IF pc.mode = "fresh"
                 THEN IF pc.plan.row.set THEN [refLed EXCEPT ![HeadBranchOf(pc.i, pc.c)] = pc.plan.row.r]
                      ELSE refLed
                 ELSE IF pc.br.on
                      THEN [refLed EXCEPT ![pc.br.name] = [has |-> TRUE, oid |-> pc.br.new, t |-> pc.c]]
                      ELSE refLed
    /\ pc' = [pc EXCEPT !.st = "w_landed"]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, stashLed, refInt, wsLed, wsInt, jour,
                   outcome, why, crashes, opN, fresh, opLive, opStash, opHead,
                   reread, badLine, bkSet, everImported, dur, probe, edSince>>

\* C next_ledger: workspace.landed for the capture landed, then the intent
\* removed. A settle goes on to decide the item's current capture. An
\* operator write equal to what bulkload lands here (a settle finds HEAD
\* already at its new value, a seat already at its new content) is landed
\* content from now on (Q144: content, not who wrote it): the ghost stops
\* holding it as an edit.
WriteLanded ==
    /\ pc.st = "w_landed"
    /\ wsLed' = [wsLed EXCEPT ![pc.i] = NextLedger(pc.i, pc.c, pc.hnew)]
    /\ opHead' = IF opHead[pc.i].on /\ opHead[pc.i].v = pc.hnew /\ HeadOf(pc.i) = pc.hnew
                 THEN [opHead EXCEPT ![pc.i] = [on |-> FALSE, v |-> Det(0)]] ELSE opHead
    /\ opLive' = [c \in OpComps |->
                    IF /\ pc.i = "main" /\ c \in Paths /\ opLive[c].on
                       /\ opLive[c].v = pc.T[c] /\ dseat[c].v = pc.T[c]
                    THEN [on |-> FALSE, v |-> 0] ELSE opLive[c]]
    /\ wsInt' = [wsInt EXCEPT ![pc.i] = NoWsInt]
    /\ pc' = IF pc.mode = "fresh" THEN [pc EXCEPT !.st = "w_journal"]
             ELSE [pc EXCEPT !.st = "w_decide", !.mode = "fresh", !.c = pc.cur]
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, prov, refLed, stashLed, refInt, jour, outcome,
                   why, crashes, opN, fresh, opStash, reread, badLine, bkSet,
                   everImported, dur, probe, edSince>>

\* E apply_item: the journal, then the outcome.
Journal ==
    /\ pc.st = "w_journal"
    /\ jour' = [jour EXCEPT ![pc.i] = jour[pc.i] \cup {pc.cur}]
    /\ outcome' = [outcome EXCEPT ![pc.i] = "done"]
    /\ why' = [why EXCEPT ![pc.i] = TRUE]
    /\ pc' = Idle
    /\ prov' = IF Mut("drop_provenance") THEN {pc.cur} ELSE prov
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat, temp,
                   dindex, dhead, refLed, stashLed, refInt, wsLed, wsInt, crashes,
                   opN, fresh, opLive, opStash, opHead, reread, badLine, bkSet,
                   everImported, dur, probe, edSince>>

-----------------------------------------------------------------------------
(* Environment: the source, the operator, crashes, time                    *)

SrcStep(s2) ==
    /\ srcN < MaxSrc
    /\ src' = s2
    /\ srcN' = srcN + 1
    /\ nextV' = nextV + 1
    /\ UNCHANGED <<caps, capt, nat, locks, dstash, dseat, temp, dindex, dhead, prov,
                   refLed, stashLed, refInt, wsLed, wsInt, jour, outcome, why, pc,
                   crashes, opN, fresh, opLive, opStash, opHead, reread, badLine,
                   bkSet, everImported, dur, probe, edSince>>

SrcLive == {r \in Refs : src.refs[r] # 0}

\* A commit, fetch, fast-forward or force-move of a ref that exists.
SrcMove(r) == /\ "move" \in SrcKinds /\ src.refs[r] # 0
              /\ SrcStep([src EXCEPT !.refs[r] = nextV])
SrcCreate(r) == /\ "create" \in SrcKinds /\ src.refs[r] = 0
                /\ ~NameConflict(r, SrcLive)
                /\ SrcStep([src EXCEPT !.refs[r] = nextV])
SrcDelete(r) == /\ "delete" \in SrcKinds /\ src.refs[r] # 0
                /\ r # src.head /\ r # src.wt
                /\ SrcStep([src EXCEPT !.refs[r] = 0])
\* `git branch -m a b` (HEAD follows), and a remote's rename (prune+fetch).
SrcRename(a, b) == /\ "rename" \in SrcKinds /\ <<a, b>> \in RenamePairs
                   /\ a \in Refs /\ b \in Refs /\ src.refs[a] # 0 /\ src.refs[b] = 0
                   /\ a # src.wt
                   /\ SrcStep([src EXCEPT !.refs[b] = src.refs[a], !.refs[a] = 0,
                                          !.head = IF src.head = a THEN b ELSE src.head])
SrcSwitch(b) == /\ "switch" \in SrcKinds /\ b \in Branches /\ src.refs[b] # 0
                /\ b # src.head /\ b # src.wt
                /\ SrcStep([src EXCEPT !.head = b])
SrcStashPush == /\ "stash" \in SrcKinds /\ Len(src.stash) < 2
                /\ SrcStep([src EXCEPT !.stash = << nextV >> \o src.stash])
SrcStashDrop == /\ "stash" \in SrcKinds /\ src.stash # << >>
                /\ SrcStep([src EXCEPT !.stash = Tail(src.stash)])
\* A seat edit (tracked, untracked or ignored alike), an addition, a
\* removal; directory <-> file changes below.
SrcSeat(p) ==
    /\ "seat" \in SrcKinds /\ p \in Paths
    /\ LET v == src.seats[p]
           parentOK == p = "d" \/ src.seats["d"] = DIR
       IN \/ /\ IsFile(v) /\ SrcStep([src EXCEPT !.seats[p] = nextV])
          \/ /\ v = ABSENT /\ parentOK /\ SrcStep([src EXCEPT !.seats[p] = nextV])
          \/ /\ IsFile(v) /\ SrcStep([src EXCEPT !.seats[p] = ABSENT])
SrcTypeChange ==
    /\ "d/f" \in Paths
    /\ \/ /\ "dirfile" \in SrcKinds /\ src.seats["d"] = DIR
          /\ SrcStep([src EXCEPT !.seats = [q \in Paths |-> IF q = "d" THEN nextV ELSE ABSENT]])
       \/ /\ "filedir" \in SrcKinds /\ IsFile(src.seats["d"])
          /\ SrcStep([src EXCEPT !.seats = [q \in Paths |-> IF q = "d" THEN DIR ELSE nextV]])
SrcStage == /\ "index" \in SrcKinds /\ IndexOn
            /\ SrcStep([src EXCEPT !.index = nextV])
\* `git worktree remove`, and `git worktree add` on a free branch.
SrcWorktree == /\ "worktree" \in SrcKinds /\ "wt" \in Items
               /\ \/ src.wt # "none" /\ SrcStep([src EXCEPT !.wt = "none"])
                  \/ \E b \in Branches : /\ src.wt = "none" /\ src.refs[b] # 0 /\ b # src.head
                                         /\ SrcStep([src EXCEPT !.wt = b])

Source == \/ \E r \in Refs : SrcMove(r) \/ SrcCreate(r) \/ SrcDelete(r)
          \/ \E a, b \in Refs : SrcRename(a, b)
          \/ \E b \in Refs : SrcSwitch(b)
          \/ SrcStashPush \/ SrcStashDrop
          \/ \E p \in Paths : SrcSeat(p)
          \/ SrcTypeChange \/ SrcStage \/ SrcWorktree

\* The operator edits the landing; a write of a landed value restores it
\* and is no edit (Q144: content).
OpGate(kind) == /\ kind \in OpKinds /\ opN < MaxOp
                /\ (pc.st = "idle" \/ OpConcurrent)
                /\ dhead["main"].on
MarkV(c, v) == IF v \in LandedVals(c) THEN [on |-> FALSE, v |-> 0] ELSE [on |-> TRUE, v |-> v]
Mark(c, v) == opLive' = [opLive EXCEPT ![c] = MarkV(c, v)]
MarkHead(i, h) == IF h \in LandedHead(i) THEN [on |-> FALSE, v |-> Det(0)] ELSE [on |-> TRUE, v |-> h]
\* An operator edit of components cs (each item's edSince records them).
OpStep(cs) == /\ opN' = opN + 1
              /\ edSince' = [i \in Items |-> edSince[i] \cup cs]
OpKeep == UNCHANGED <<src, srcN, caps, capt, locks, dhead, prov, refLed,
                      stashLed, refInt, wsLed, wsInt, jour, outcome, why, pc,
                      crashes, reread, badLine, bkSet, everImported, dur, probe>>

\* A landed file's content edited or the file removed; with "dir", the
\* landed directory removed (`rm -r`) or replaced by a file (review 3
\* finding 5).
OpSeat(p) ==
    /\ \/ /\ OpGate("seat")
          /\ LET v == dseat[p].v IN
             \/ /\ IsFile(v) /\ dseat' = Write(p, nextV) /\ Mark(p, nextV)
                /\ nextV' = nextV + 1
             \/ /\ IsFile(v) /\ dseat' = Write(p, ABSENT) /\ Mark(p, ABSENT)
                /\ UNCHANGED nextV
          /\ UNCHANGED temp
       \/ /\ OpGate("dir") /\ p = "d" /\ "d/f" \in Paths /\ dseat["d"].v = DIR
          /\ \E nv \in {ABSENT, nextV} :
               /\ dseat' = [dseat EXCEPT !["d"] = [v |-> nv, st |-> dseat["d"].st + 1],
                                         !["d/f"] = [v |-> ABSENT, st |-> dseat["d/f"].st + 1]]
               /\ opLive' = [opLive EXCEPT !["d"] = MarkV("d", nv),
                                           !["d/f"] = MarkV("d/f", ABSENT)]
               /\ nextV' = IF nv = ABSENT THEN nextV ELSE nextV + 1
          /\ temp' = [temp EXCEPT !["d/f"] = NoSeat]
    /\ OpStep({p} \cup {c \in OpComps : opLive'[c] # opLive[c]})
    /\ OpKeep /\ UNCHANGED <<nat, dstash, dindex, fresh, opStash, opHead>>
\* A write in the same tick as bulkload's write of the seat keeps its stat.
OpRacy(p) ==
    /\ OpGate("racy") /\ RacyEdits /\ pc.st = "idle"
    /\ p \in fresh /\ IsFile(dseat[p].v)
    /\ dseat' = [dseat EXCEPT ![p].v = nextV] /\ Mark(p, nextV)
    /\ nextV' = nextV + 1
    /\ OpStep({p}) /\ OpKeep /\ UNCHANGED <<nat, temp, dstash, dindex, fresh, opStash, opHead>>
\* `touch`, an identical save, `git status`: the stat moves, the content not.
OpTouch(p) ==
    /\ OpGate("touch") /\ IsFile(dseat[p].v)
    /\ dseat' = [dseat EXCEPT ![p].st = dseat[p].st + 1]
    /\ opN' = opN + 1
    /\ OpKeep /\ UNCHANGED <<nat, temp, nextV, dstash, dindex, fresh, opLive, opStash, opHead,
                             edSince>>
\* An untracked file the operator adds in a landed directory.
OpUntracked(p) ==
    /\ OpGate("untracked") /\ dseat[p].v = ABSENT
    /\ p = "d/f" => dseat["d"].v = DIR
    /\ dseat' = Write(p, nextV) /\ Mark(p, nextV)
    /\ nextV' = nextV + 1
    /\ OpStep({p}) /\ OpKeep /\ UNCHANGED <<nat, temp, dstash, dindex, fresh, opStash, opHead>>
\* `git add`.
OpIndex == /\ OpGate("index") /\ IndexOn
           /\ dindex' = nextV /\ Mark("index", nextV) /\ nextV' = nextV + 1
           /\ OpStep({"index"}) /\ OpKeep /\ UNCHANGED <<nat, temp, dstash, dseat, fresh, opStash, opHead>>

\* Operator Git writes: refs, the stash, HEAD (taken as durable: OpDur).
OpGitKeep == UNCHANGED <<src, srcN, caps, capt, locks, temp, prov, refLed,
                         stashLed, refInt, wsLed, wsInt, jour, outcome, why, pc,
                         crashes, reread, badLine, bkSet, everImported, probe,
                         dseat, dindex, fresh>>
\* A commit, `branch -f`, `update-ref -d`, a fetch, a tag: any native ref.
\* `update-ref -d` deletes a checked-out branch too: its worktree's HEAD is
\* then unborn (review 3 finding 4).
OpRef(r) ==
    /\ OpGate("ref") /\ r \notin locks
    /\ \E nv \in {nextV, 0} :
         /\ IF nv = 0 THEN nat[r] # 0 ELSE nat[r] # 0 \/ ~NameConflict(r, LiveNames)
         /\ nat' = [nat EXCEPT ![r] = nv] /\ Mark(r, nv)
         /\ nextV' = IF nv = 0 THEN nextV ELSE nextV + 1
         \* A commit on the branch the operator's HEAD is on moves that HEAD.
         /\ opHead' = [j \in Items |-> IF opHead[j].on /\ dhead[j].sym = r
                                       THEN [on |-> TRUE, v |-> [sym |-> r, oid |-> nv]]
                                       ELSE opHead[j]]
    /\ OpStep({r}) /\ OpGitKeep /\ UNCHANGED <<dhead, dstash, opStash>>
    /\ OpDur
MarkStash(s) == opStash' = IF s \in LandedStash THEN [on |-> FALSE, v |-> << >>]
                           ELSE [on |-> TRUE, v |-> s]
\* `git stash push` ("stash"); `git stash drop` and `git stash clear`
\* ("stashdrop"). A refs/stash.lock left by a killed store fails them.
OpStash ==
    /\ "stash" \notin locks
    /\ \/ /\ OpGate("stash") /\ Len(dstash) < 2
          /\ dstash' = << nextV >> \o dstash /\ nextV' = nextV + 1
          /\ MarkStash(<< nextV >> \o dstash)
       \/ /\ OpGate("stashdrop") /\ dstash # << >>
          /\ dstash' = Tail(dstash) /\ UNCHANGED nextV
          /\ MarkStash(Tail(dstash))
       \/ /\ OpGate("stashdrop") /\ dstash # << >>
          /\ dstash' = << >> /\ UNCHANGED nextV
          /\ MarkStash(<< >>)
    /\ OpStep({"stash"}) /\ OpGitKeep /\ UNCHANGED <<nat, dhead, opLive, opHead>>
    /\ OpDur
\* `git switch` in the main checkout, to a branch or (`switch --orphan`) to
\* an unborn one (review 3 finding 4); with "rebase", a `git rebase` or
\* `git bisect` started in a worktree: HEAD detached, its branch held, and
\* the branch's value the operation's own (its final update-ref expects
\* it; review 3 finding 2).
OpHead ==
    /\ \/ /\ OpGate("head") /\ dhead["main"].held = "none" /\ "HEAD:main" \notin locks
          /\ \E b \in Branches :
               /\ b # dhead["main"].sym /\ b \notin Checked
               /\ nat[b] # 0 \/ ~NameConflict(b, LiveNames)
               /\ dhead' = [dhead EXCEPT !["main"] = [on |-> TRUE, sym |-> b, oid |-> 0,
                                                     held |-> "none"]]
               /\ opHead' = [opHead EXCEPT !["main"] = MarkHead("main", [sym |-> b, oid |-> nat[b]])]
          /\ UNCHANGED opLive
       \/ /\ OpGate("rebase")
          /\ \E i \in Items :
               LET b == dhead[i].sym IN
               /\ dhead[i].on /\ b # "det" /\ ("HEAD:" \o i) \notin locks
               /\ nat[b] # 0
               /\ dhead' = [dhead EXCEPT ![i] = [on |-> TRUE, sym |-> "det", oid |-> nat[b],
                                                 held |-> b]]
               /\ opHead' = [opHead EXCEPT ![i] = MarkHead(i, Det(nat[b]))]
               /\ opLive' = [opLive EXCEPT ![b] = [on |-> TRUE, v |-> nat[b]]]
    /\ OpStep({"HEAD:" \o j : j \in {x \in Items : dhead'[x] # dhead[x]}}
              \cup {r \in Refs : opLive'[r] # opLive[r]})
    /\ OpGitKeep /\ UNCHANGED <<nat, nextV, dstash, opStash>>
    /\ OpDur
\* The operator puts back what bulkload landed (an undo, `git checkout`,
\* `git read-tree`, `update-ref` to the ledger's value), between applies.
OpRestore ==
    /\ OpGate("restore") /\ pc.st = "idle"
    /\ \/ \E p \in Paths :
            /\ opLive[p].on
            /\ p = "d/f" => dseat["d"].v = DIR
            /\ dseat' = Write(p, wsLed["main"].seats[p].v)
            /\ opLive' = [opLive EXCEPT ![p] = [on |-> FALSE, v |-> 0]]
            /\ UNCHANGED <<nat, dindex, opHead>>
       \/ /\ opLive["index"].on /\ dindex' = wsLed["main"].index
          /\ opLive' = [opLive EXCEPT !["index"] = [on |-> FALSE, v |-> 0]]
          /\ UNCHANGED <<nat, dseat, opHead>>
       \/ \E r \in Refs :
            /\ opLive[r].on /\ Live(refLed[r]) /\ r \notin locks
            /\ ~\E j \in Items : dhead[j].held = r
            \* A ref a pending intent moves has two landed values; putting
            \* back the ledger's is not modelled as a restore.
            /\ ~\E op \in refInt.ops : op.name = r
            /\ ~\E i \in Items : wsInt[i].on /\ wsInt[i].br.on /\ wsInt[i].br.name = r
            /\ nat' = [nat EXCEPT ![r] = refLed[r].oid]
            /\ opLive' = [opLive EXCEPT ![r] = [on |-> FALSE, v |-> 0]]
            /\ opHead' = [j \in Items |-> IF opHead[j].on /\ dhead[j].sym = r
                                          THEN [on |-> TRUE, v |-> [sym |-> r, oid |-> refLed[r].oid]]
                                          ELSE opHead[j]]
            /\ UNCHANGED <<dseat, dindex>>
    /\ OpStep({c \in OpComps : opLive'[c] # opLive[c]})
    /\ UNCHANGED <<src, srcN, caps, capt, locks, temp, prov, refLed, stashLed,
                   refInt, wsLed, wsInt, jour, outcome, why, pc, crashes, reread,
                   badLine, bkSet, everImported, nextV, dstash, dhead, fresh,
                   opStash, probe>>
    /\ OpDur

Operator == \/ \E p \in Paths : OpSeat(p) \/ OpRacy(p) \/ OpTouch(p) \/ OpUntracked(p)
            \/ OpIndex \/ (\E r \in Refs : OpRef(r)) \/ OpStash \/ OpHead \/ OpRestore

\* The apply host crashes between two steps: the volatile pc is lost. A
\* power loss also loses what of Git's writes is not durable (dur); under
\* the rule every bulkload Git write is, so it is the same crash. Under
\* refs_unsynced it may also strike between applies.
Crash ==
    /\ crashes < MaxCrashes
    /\ \/ /\ pc.st # "idle"
          /\ UNCHANGED <<nat, dhead, dstash>>
       \/ /\ Mut("refs_unsynced")
          /\ <<nat, dhead, dstash>> # <<dur.nat, dur.head, dur.stash>>
          /\ nat' = dur.nat /\ dhead' = dur.head /\ dstash' = dur.stash
    /\ pc' = Idle
    /\ outcome' = IF pc.st # "idle" THEN [outcome EXCEPT ![pc.i] = "crashed"] ELSE outcome
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, locks, dseat, temp, dindex, prov,
                   refLed, stashLed, refInt, wsLed, wsInt, jour, why, opN, fresh,
                   opLive, opStash, opHead, reread, badLine, bkSet, everImported,
                   dur, probe, edSince>>

\* stash_apply's next store would run (RefStash's go).
StashStores ==
    LET op == refInt.sop
        dst == dstash
    IN /\ op.on /\ dst # op.new /\ "stash" \notin locks
       /\ ~(pc.sgo /\ dst # pc.sexp)
       /\ \/ IsSuffix(dst, op.new)
          \/ ~pc.sgo /\ (dst = op.old \/ Mut("stash_clear_blind"))

\* A crash inside a Git ref transaction (a killed child, power loss): any
\* subset of its lines is committed, and Git's lock file stays for every
\* line it had locked but not renamed. Inside stash_apply's update-ref
\* (review 3 finding 8): the store is not made and refs/stash.lock stays.
ChildCrashTxn ==
    /\ ChildCrash /\ crashes < MaxCrashes
    /\ \/ /\ pc.st \in {"r_txd", "r_txw"}
          /\ LET T == IF pc.st = "r_txd" THEN TxnDeletes ELSE pc.again IN
             /\ T # {} /\ TxnOK(T)
             /\ \E S \in SUBSET T :
                  /\ S # T
                  /\ nat' = TxnApply(S)
                  /\ locks' = locks \cup {op.name : op \in T \ S}
       \/ /\ pc.st = "r_stash" /\ StashStores
          /\ locks' = locks \cup {"stash"}
          /\ UNCHANGED nat
    /\ pc' = Idle
    /\ outcome' = [outcome EXCEPT ![pc.i] = "crashed"]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, dstash, dseat, temp, dindex, dhead,
                   prov, refLed, stashLed, refInt, wsLed, wsInt, jour, why, opN,
                   fresh, opLive, opStash, opHead, reread, badLine, bkSet,
                   everImported, probe, edSince>>
    /\ GitSync

\* The same inside set_head's transaction: HEAD ("HEAD" sorts before
\* "refs/") or the branch line committed alone.
ChildCrashHead ==
    /\ ChildCrash /\ crashes < MaxCrashes /\ pc.st = "w_head"
    /\ LET i == pc.i IN
       /\ ~(pc.mode = "settle" /\ HeadOf(i) = pc.hnew)
       /\ ~(pc.mode = "settle" /\ HeadOf(i) # pc.hold)
       /\ ~(pc.mode = "settle" /\ UnbornUntyped(i))
       /\ HeadTxnOK(i, pc.hold, pc.hnew, pc.br)
       /\ BrLine(pc.br) /\ HeadLine(i, pc.hold, pc.hnew)
       /\ \/ /\ dhead' = [dhead EXCEPT ![i] = NewDhead(pc.hnew)]
             /\ locks' = locks \cup {pc.br.name}
             /\ UNCHANGED nat
          \/ /\ nat' = [nat EXCEPT ![pc.br.name] = pc.br.new]
             /\ locks' = locks \cup {"HEAD:" \o i}
             /\ UNCHANGED dhead
    /\ pc' = Idle
    /\ outcome' = [outcome EXCEPT ![pc.i] = "crashed"]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<src, srcN, nextV, caps, capt, dstash, dseat, temp, dindex, prov,
                   refLed, stashLed, refInt, wsLed, wsInt, jour, why, opN, fresh,
                   opLive, opStash, opHead, reread, badLine, bkSet, everImported,
                   probe, edSince>>
    /\ GitSync

\* The timestamp tick passes: no later write can keep a seat's stat.
Tick == /\ fresh # {}
        /\ fresh' = {}
        /\ UNCHANGED <<src, srcN, nextV, caps, capt, nat, locks, dstash, dseat,
                       temp, dindex, dhead, prov, refLed, stashLed, refInt, wsLed,
                       wsInt, jour, outcome, why, pc, crashes, opN, opLive, opStash,
                       opHead, reread, badLine, bkSet, everImported, dur, probe, edSince>>

\* Nothing in flight: the run may end here (a stutter, so a quiet state is
\* not a deadlock).
Quiet == /\ pc.st = "idle"
         /\ UNCHANGED vars

-----------------------------------------------------------------------------

Init ==
    /\ src = S0
    /\ srcN = 0
    /\ nextV = 5
    /\ caps = << S0 >>
    /\ capt = [i \in Items |-> 1]
    /\ locks = {}
    /\ temp = [p \in Paths |-> NoSeat]
    /\ refInt = NoRefInt
    /\ wsInt = [i \in Items |-> NoWsInt]
    /\ why = [i \in Items |-> TRUE]
    /\ pc = Idle
    /\ crashes = 0
    /\ opN = 0
    /\ opLive = [c \in OpComps |-> [on |-> FALSE, v |-> 0]]
    /\ opStash = [on |-> FALSE, v |-> << >>]
    /\ opHead = [i \in Items |-> [on |-> FALSE, v |-> Det(0)]]
    /\ reread = FALSE
    /\ badLine = FALSE
    /\ probe = 0
    /\ edSince = [i \in Items |-> {}]
    /\ IF Landed
       THEN /\ nat = S0.refs
            /\ dstash = << >>
            /\ dseat = [p \in Paths |-> [v |-> S0.seats[p], st |-> 1]]
            /\ fresh = {}
            /\ dindex = S0.index
            /\ dhead = [i \in Items |-> [on |-> TRUE, sym |-> IF i = "main" THEN S0.head ELSE S0.wt,
                                        oid |-> 0, held |-> "none"]]
            /\ prov = {1}
            /\ everImported = {1}
            /\ refLed = [r \in Refs |-> IF S0.refs[r] # 0 THEN [has |-> TRUE, oid |-> S0.refs[r], t |-> 1]
                                       ELSE NoRow]
            /\ stashLed = NoSLed
            /\ wsLed = [i \in Items |->
                  [has |-> TRUE, t |-> 1,
                   head |-> LET b == IF i = "main" THEN S0.head ELSE S0.wt
                            IN [sym |-> b, oid |-> S0.refs[b]],
                   index |-> IF i = "main" THEN S0.index ELSE 0,
                   seats |-> [p \in Paths |-> IF i = "main" /\ S0.seats[p] # ABSENT
                                              THEN [v |-> S0.seats[p], st |-> 1, racy |-> FALSE]
                                              ELSE NoSeatRec]]]
            /\ jour = [i \in Items |-> {1}]
            /\ outcome = [i \in Items |-> "done"]
            /\ bkSet = {}
       ELSE /\ nat = [r \in Refs |-> 0]
            /\ dstash = << >>
            /\ dseat = [p \in Paths |-> NoSeat]
            /\ fresh = {}
            /\ dindex = 0
            /\ dhead = [i \in Items |-> [on |-> FALSE, sym |-> "det", oid |-> 0, held |-> "none"]]
            /\ prov = {}
            /\ everImported = {}
            /\ refLed = NoLed
            /\ stashLed = NoSLed
            /\ wsLed = [i \in Items |-> NoWsLed]
            /\ jour = [i \in Items |-> {}]
            /\ outcome = [i \in Items |-> "none"]
            /\ bkSet = {}
    /\ dur = [nat |-> nat, head |-> dhead, stash |-> dstash]

\* The steps of the apply in flight (each checks pc.st).
InFlight == \/ FirstLandMain \/ FirstStepWt \/ FirstPlanWt \/ FirstAddWt
            \/ FirstLedgerWt \/ Decide_ \/ Import \/ ProbeExchange \/ WriteIntent \/ RefStep
            \/ RefFinish \/ RefTxnDeletes \/ RefTxnWrites \/ RefStash
            \/ RefLedger \/ SeatOp \/ SeatVerify \/ IndexPublish \/ HeadSet
            \/ BranchRow \/ WriteLanded \/ Journal

\* Every step of one item's apply, from its start.
Run(i) == Begin(i) \/ (pc.st # "idle" /\ pc.i = i /\ InFlight)

Protocol == \/ \E i \in Items : Capture(i)
            \/ \E i \in Items : Begin(i)
            \/ InFlight

Environment == Source \/ Operator \/ Crash \/ ChildCrashTxn \/ ChildCrashHead \/ Tick \/ Quiet

Next == Protocol \/ Environment

Spec == Init /\ [][Next]_vars

\* Fair captures and applies (strong fairness per item: a refusing item
\* must not starve the other), unfair environment and faults.
LiveSpec == /\ Spec
            /\ \A i \in Items : WF_vars(Capture(i)) /\ SF_vars(Run(i))

-----------------------------------------------------------------------------
(* PROPERTIES                                                              *)

HeadRec == [sym : Refs \cup {"det"}, oid : Nat]
Row == [has : BOOLEAN, oid : Nat, t : Nat]

TypeOK ==
    /\ src.refs \in [Refs -> Nat] /\ src.head \in Branches
    /\ src.wt \in Branches \cup {"none"}
    /\ caps \in Seq([refs : [Refs -> Nat], head : Branches, wt : Branches \cup {"none"},
                     stash : Seq(Nat), seats : [Paths -> Nat], index : Nat])
    /\ capt \in [Items -> 1..Len(caps)]
    /\ nat \in [Refs -> Nat]
    /\ locks \subseteq Refs \cup {"HEAD:main", "HEAD:wt", "stash"}
    /\ probe \in Nat
    /\ dstash \in Seq(Nat)
    /\ dseat \in [Paths -> [v : Nat, st : Nat]]
    /\ temp \in [Paths -> [v : Nat, st : Nat]]
    /\ refLed \in [Refs -> Row]
    /\ prov \subseteq 1..Len(caps)
    /\ \A i \in Items :
         /\ outcome[i] \in Refusals \cup {"none", "done", "crashed"}
         /\ jour[i] \subseteq 1..Len(caps)
         /\ wsLed[i].head \in HeadRec
         /\ dhead[i].on => dhead[i].sym \in Refs \cup {"det"}
    /\ pc.st \in {"idle", "f_land", "f_step", "f_plan", "f_add", "f_ledger",
                  "w_decide", "w_import", "w_probe", "w_intent", "w_refs", "s_refs",
                  "r_fin", "r_txd", "r_txw", "r_stash", "r_led", "w_seats",
                  "w_index", "w_head", "w_brow", "w_landed", "w_journal"}
    /\ crashes \in 0..MaxCrashes /\ opN \in 0..MaxOp /\ srcN \in 0..MaxSrc

\* Q144: an operator edit is never overwritten or removed. Every value the
\* operator wrote that is not a landed value (or deleted) is still where
\* they put it, or at its seat's temporary name, displaced and kept.
NoLocalWorkLost ==
    /\ \A c \in OpComps : opLive[c].on =>
          IF c \in Refs THEN nat[c] = opLive[c].v
          ELSE IF c = "index" THEN dindex = opLive[c].v
          ELSE dseat[c].v = opLive[c].v \/ (opLive[c].v # ABSENT /\ temp[c].v = opLive[c].v)
    /\ opStash.on => dstash = opStash.v
    /\ \A i \in Items : opHead[i].on => HeadOf(i) = opHead[i].v

\* Every capture ever imported keeps its provenance instance (every ref
\* tip and stash it carried); a local branch or tag bulkload set is never
\* deleted by bulkload, even when the source deleted it (Q144: kept and
\* reported); remote-tracking refs may follow the source (their values
\* stay at provenance).
NoSourceWorkLost ==
    /\ everImported \subseteq prov
    /\ bkSet = {}

\* Every refusal is a typed code, and the cause it names is real: MODIFIED
\* only over an operator edit of the item's checkout or HEAD branch;
\* INTERRUPTED only over an operator edit, since the item's run began or
\* its intent was written and still live, of a component the failing step
\* writes, or a Git lock left on one (Interrupted); never a code that
\* names no converge cause (UNTYPED); no custody word blames the
\* destination for a change nobody made there.
TypedRefusal ==
    /\ \A i \in Items : outcome[i] \in Refusals => why[i]
    /\ ~badLine

\* R25 (S3): an item whose journal names its current capture, with no
\* workspace intent of its own pending, reads no content: no stage, no
\* proof (E apply_item's A4, review 2 finding 7).
NoReRead == ~reread

\* Per item and per component, old or new after any crash: under a
\* pending intent every component is the intent's old or new value (or
\* absent between a type change's removal and its addition) or the
\* operator's; with no intent pending, every component is the ledger's or
\* the operator's.
CrashAtomicity ==
    /\ \A i \in Items : (dhead[i].on /\ pc.st = "idle") => wsLed[i].has
    \* No bulkload residue outside an intent: probe files only while the
    \* main's pass that made them runs, or until its next apply after a
    \* crash (review 3 finding 9).
    /\ probe > (IF pc.st = "w_probe" THEN 1 ELSE 0) => outcome["main"] = "crashed"
    /\ LET I == wsInt["main"]
           L == wsLed["main"]
       IN /\ \A p \in Paths : ~opLive[p].on =>
                IF I.on
                THEN LET F == caps[I.from].seats[p]
                         T == caps[I.to].seats[p]
                     IN dseat[p].v \in {F, T} \cup (IF TypeChange(F, T) THEN {ABSENT} ELSE {})
                ELSE L.has => dseat[p].v = L.seats[p].v
          /\ (IndexOn /\ L.has /\ ~opLive["index"].on) =>
                IF I.on THEN dindex \in {I.iold, caps[I.to].index} ELSE dindex = L.index
    /\ \A i \in Items :
         (wsLed[i].has /\ dhead[i].on /\ ~opHead[i].on
          /\ ~(wsLed[i].head.sym # "det" /\ opLive[wsLed[i].head.sym].on)
          /\ ~(wsInt[i].on /\ wsInt[i].hnew.sym # "det" /\ opLive[wsInt[i].hnew.sym].on)) =>
            IF wsInt[i].on THEN HeadOf(i) \in {wsInt[i].hold, wsInt[i].hnew}
            ELSE HeadOf(i) = wsLed[i].head
    \* The branch a pending intent moves with HEAD is a component of the
    \* checkout (finding 2): old or new until HEAD is on it.
    /\ \A i \in Items :
         (wsInt[i].on /\ wsInt[i].br.on /\ ~opLive[wsInt[i].br.name].on
          /\ HeadOf(i) # wsInt[i].hnew) =>
            nat[wsInt[i].br.name] \in {wsInt[i].br.old, wsInt[i].br.new}
    /\ IF refInt.on
       THEN \A op \in refInt.ops : ~opLive[op.name].on => nat[op.name] \in {op.old, op.new}
       ELSE \A r \in Refs : (Live(refLed[r]) /\ ~opLive[r].on) =>
               \/ nat[r] = refLed[r].oid
               \/ \E i \in Items : wsInt[i].on /\ wsInt[i].br.on /\ wsInt[i].br.name = r
                                   /\ nat[r] \in {wsInt[i].br.old, wsInt[i].br.new}

\* Wall-clock budget, evaluated on every state (README, "The budget is
\* state-level").
WithinBudget == srcN \in 0..MaxSrc => TLCGet("duration") < BudgetSeconds

\* REACHED witnesses (expected violated): the bound explores the state.
\* A converge (not a first landing) completed: a journal for a later capture.
Witness_Converged == ~\E c \in jour["main"] : c > 1 /\ wsLed["main"].t = c /\ c \in prov

\* A crashed converge settled forward and journaled by a rerun.
Witness_SettledAfterCrash == ~(crashes > 0 /\ ~wsInt["main"].on /\ \E c \in jour["main"] : c > 1)

\* A captured ref tip that never reached the destination: superseded
\* before any apply, it is neither native nor at provenance (CORPUS keeps
\* it as a chain link: GitCarry.tla's custody, not the destination's).
Witness_CapturedNotLanded ==
    ~\E k \in 1..Len(caps), r \in Refs :
        /\ k \notin prov /\ caps[k].refs[r] # 0
        /\ \A q \in Refs : nat[q] # caps[k].refs[r]
        /\ \A j \in prov, q \in Refs : caps[j].refs[q] # caps[k].refs[r]
        /\ \A i \in Items : capt[i] # k

\* Liveness (Q144): once the source and the operator stop, with no
\* operator edit left at the destination, every item reaches its source:
\* native refs equal the source's (a local branch or tag deleted at the
\* source kept, and a ref that name-conflicts with a kept one, are the
\* ruled exceptions), the stash, the checkout's seats, index and HEAD
\* (detached at the source's head only when its branch name conflicts).
Kept(r) == Class(r) # "remote" /\ src.refs[r] = 0 /\ nat[r] # 0
RefConverged(r) == \/ nat[r] = src.refs[r]
                   \/ Kept(r)
                   \/ (src.refs[r] # 0 /\ nat[r] = 0 /\ \E q \in Refs : Conflicting(r, q) /\ Kept(q))
HeadConverged(i, b) ==
    /\ dhead[i].on
    /\ HeadOf(i).oid = src.refs[b]
    /\ \/ HeadOf(i).sym = b
       \/ (HeadOf(i).sym = "det" /\ nat[b] = 0 /\ NameConflict(b, LiveNames))
Converged ==
    /\ pc.st = "idle" /\ ~refInt.on
    /\ \A i \in Items : (i = "main" \/ src.wt # "none") =>
         /\ Proj(i, caps[capt[i]]) = Proj(i, src)
         /\ capt[i] \in jour[i] /\ ~wsInt[i].on
         /\ HeadConverged(i, IF i = "main" THEN src.head ELSE src.wt)
    /\ \A r \in Refs : RefConverged(r)
    /\ dstash = src.stash
    /\ \A p \in Paths : dseat[p].v = src.seats[p]
    /\ IndexOn => dindex = src.index

ConvergesWhenQuiet == (<>[]Clean) => <>[]Converged

\* No item refuses forever unless an operator edit of its own checkout or
\* HEAD branch stays: in particular no crash leftover, ref name or plan
\* replayed from before a crash wedges it (the foo -> foo/bar blocker).
NoWedge == \A i \in Items :
    (<>[](outcome[i] \in Refusals /\ capt[i] \notin jour[i])) => <>[]ItemCause(i)

=============================================================================
