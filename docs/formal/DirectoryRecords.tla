--------------------------- MODULE DirectoryRecords ---------------------------
(***************************************************************************)
(* Directory records (R-N102, R-N119) and their batching (OI-1003-Q143     *)
(* item 2). Checked with TLC; see docs/formal/README.md, "Directory        *)
(* records", for configs and results.                                      *)
(*                                                                         *)
(* SCOPE                                                                   *)
(*   - The destination's directories of one carried tree: two siblings     *)
(*     under the root, and (Deep) a child under each, so a level has two   *)
(*     parents. Every directory is created as Destination::directory       *)
(*     creates one: a tagged mkdirat, the parent sealed, a record bound to *)
(*     the new inode committed, an exclusive rename into place, the parent *)
(*     sealed again; or, on a file system with no exclusive rename         *)
(*     (NoReplaceRename = FALSE), an intent committed first, a plain       *)
(*     mkdirat at the final name, the parent sealed, the record bound.     *)
(*   - Batching (BatchCreate): one store commit binds several records      *)
(*     (Store::record_directories_created), and one seal per distinct      *)
(*     parent covers every directory under it; BatchFinish: one commit     *)
(*     completes every finished directory (Store::complete_directories).   *)
(*   - A directory is decided only once its parent was decided (the walk   *)
(*     is depth-first) and, if the parent was created or adopted, once its *)
(*     rename is sealed. Deciding an existing directory sweeps it of this  *)
(*     store's temporaries (records bound to a temporary cleared first,    *)
(*     then the temporary removed if empty, then the directory sealed) and *)
(*     adopts it only when its record names its inode, sealing its parent  *)
(*     first (#74 round 2, N1); a stale record is cleared; a directory     *)
(*     with no owning record is refused on a divergent mode. The root is   *)
(*     swept when a run starts.                                            *)
(*   - Finishing: deepest first, each directory's final mode applied and   *)
(*     sealed before the commit that completes it; only in a run that     *)
(*     refused nothing.                                                    *)
(*   - Two kinds of crash. Stop: the process dies, the page cache keeps    *)
(*     every name. PowerLoss: every name change and mode change not yet    *)
(*     sealed is kept or lost on its own (a directory's mkdir and rename   *)
(*     as a prefix of the two), except that nothing survives inside a      *)
(*     directory whose own creation was lost. This is crash_check's       *)
(*     persistence model (an unsynced namespace operation is optionally   *)
(*     persisted) and BulkloadTransfer's CrashChoice (persist or revert),  *)
(*     per name. EnvSeal(p): any other fsync of p, or a journal force,     *)
(*     makes p's names durable without the protocol's knowledge (the      *)
(*     committer's group seal of a shared parent, an xfs log force).      *)
(*   - Inode numbers are reused: a number is free once no name, durable or *)
(*     not, holds its inode, and a new directory, ours or a third party's, *)
(*     may take any free number (#74 N3).                                  *)
(*   - A third party may create a directory at a free final name, or      *)
(*     replace its own directory there (a new inode).                     *)
(*                                                                         *)
(* ABSTRACTIONS (what is NOT modelled)                                     *)
(*   - Files. An output committed inside a directory is ChildCommit, which *)
(*     needs the directory decided; the output's own durability is         *)
(*     BulkloadTransfer's. Symlinks, Skip and engine temporaries are not   *)
(*     modelled.                                                          *)
(*   - Store commits are atomic and durable when they return; the         *)
(*     database's own files are not modelled.                             *)
(*   - One device: a parent off the store's device gets a full flush in    *)
(*     the code (seal_entry); here every seal is the store device's.      *)
(*   - A fallback directory reverting to a temporary name in a power loss  *)
(*     (PowerLoss allows any prefix of mkdir and rename) is over-          *)
(*     approximated: the fallback has no temporary.                       *)
(*   - On a file system with no exclusive rename the code first makes a   *)
(*     temporary and fails its rename; that temporary is discarded as an   *)
(*     EEXIST one is (record cleared, then the temporary removed), which   *)
(*     the EEXIST path checks. The fallback is modelled from its intent.  *)
(*   - A third party's directory never has mode 0700, so the fallback's   *)
(*     fresh_fallback check (empty, 0700, this user) tells it from ours.   *)
(*     A same-user 0700 empty directory made at the leaf between an       *)
(*     intent and its mkdirat is R-N119's accepted limit.                 *)
(*   - Levels. Record(S) commits any set of sealed directories (BatchCreate)*)
(*     and the code commits one level; the model's behaviours include the  *)
(*     code's.                                                            *)
(*                                                                         *)
(* CODE MAP (A = crates/bulkload-agent/src)                                *)
(*   Mkdir, SealParent, Record, Rename   A/materialize.rs directory,      *)
(*       create_directories (seal_entry, record_directories_created,       *)
(*       rename_exclusive); A/transfer.rs DirectoryBatch, close_batch     *)
(*   DiscardClear, DiscardUnlink         A/materialize.rs discard_directory*)
(*   Intent, FallbackMkdir, Bind         A/materialize.rs fallback_directory*)
(*   DecideExisting, StartRun's sweep    A/materialize.rs existing_directory,*)
(*       sweep, sweep_root; A/transfer_store.rs clear_directories_bound_to*)
(*   Chmod, SealDir, Complete            A/materialize.rs finish_directories*)
(*       (complete_directories)                                          *)
(*   ChildCommit                         A/transfer.rs publish (an output's*)
(*       group commit inside the directory)                              *)
(*   Stop, PowerLoss, EnvSeal, Foreign, ForeignReplace   the environment  *)
(*                                                                         *)
(* NEGATIVE CONFIGS set Mutation to break exactly one rule; each MUST      *)
(* produce a counterexample on its named property (README.md).            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Inos,            \* inode numbers (model values), reused once free
    Deep,            \* a child under each sibling: a level with two parents
    BatchCreate,     \* one commit binds several directories' records
    BatchFinish,     \* one commit completes every finished directory
    NoReplaceRename, \* the file system has an exclusive rename (R-N119)
    MaxRuns,         \* receive sessions
    MaxCrashes,      \* Stop and PowerLoss crashes
    MaxForeign,      \* third-party directory creations and replacements
    Mutation,        \* "none", or one deliberate rule break
    BudgetSeconds    \* wall-clock budget, checked by WithinBudget

Mutations == {"none", "record_before_seal", "rename_before_record",
              "child_before_dirseal", "complete_before_seal",
              "level_seals_one_parent", "level_rename_seals_one_parent",
              "adopt_unbound", "discard_unlink_before_clear",
              "admit_under_held"}

ASSUME /\ IsFiniteSet(Inos) /\ Inos # {} /\ Deep \in BOOLEAN
       /\ BatchCreate \in BOOLEAN /\ BatchFinish \in BOOLEAN
       /\ NoReplaceRename \in BOOLEAN
       /\ MaxRuns \in Nat /\ MaxCrashes \in Nat /\ MaxForeign \in Nat
       /\ Mutation \in Mutations /\ BudgetSeconds \in Nat

Root == "root"
Dirs == IF Deep THEN {"a", "b", "c", "e"} ELSE {"a", "b"}
Parent(d) == CASE d = "c" -> "a" [] d = "e" -> "b" [] OTHER -> Root
Children(p) == {d \in Dirs : Parent(d) = p}

NoIno == "noino"
IntentIno == "intent"     \* materialize::INTENT, a record bound to no inode
Names == {"none", "tmp", "final"}
Modes == {"0700", "final"}

\* A run's phases of one directory.
Creating == {"mkdir", "sealed", "recorded", "renamed", "eexist", "eexistC",
             "eexistU", "intent", "fbmkdir", "fbsealed"}
Finishing == {"chmod", "fsealed"}
Phases == {"todo", "ready", "existing", "refused", "done"} \cup Creating
          \cup Finishing

VARIABLES
    run,      \* sessions started
    live,     \* a session is running
    crashes,  \* Stop and PowerLoss so far
    foreign,  \* third-party creations and replacements so far
    loc,      \* d -> where our inode for d is named now: none | tmp | final
    dloc,     \* d -> where it is named durably (what a power loss keeps)
    ino,      \* d -> our inode for d, or NoIno
    mode,     \* d -> our directory's mode now
    dmode,    \* d -> its durable mode
    fino,     \* d -> a third party's directory at d's final name, or NoIno
    fmode,    \* d -> that directory's mode
    rec,      \* d -> the committed record: NoIno, IntentIno or an inode
    ph,       \* d -> this session's phase
    child,    \* d -> an output committed inside d (ghost)
    everDone, \* d -> a completion of d committed (ghost)
    multiRec, \* d -> d's record was committed with another's (ghost)
    adoptedForeign, \* a third party's directory was adopted (ghost)
    adoptedMulti,   \* a directory bound in a shared commit was adopted (ghost)
    partial   \* a power loss kept one sibling's rename and lost another's (ghost)

vars == <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode, fino,
          fmode, rec, ph, child, everDone, multiRec, adoptedForeign,
          adoptedMulti, partial>>

InoSymmetry == Permutations(Inos)

-----------------------------------------------------------------------------
(* Helpers *)

Used == {ino[d] : d \in {x \in Dirs : loc[x] # "none" \/ dloc[x] # "none"}}
        \cup {fino[d] : d \in Dirs}
Free == Inos \ Used

\* The inode at d's final name, ours or a third party's.
AtFinal(d) == IF loc[d] = "final" THEN ino[d] ELSE fino[d]
Occupied(d) == loc[d] = "final" \/ fino[d] # NoIno

\* d's parent is the root or a directory whose name holds now.
ParentExists(d) == IF Parent(d) = Root THEN TRUE ELSE Occupied(Parent(d))

\* Directories inside d may be decided: d is decided and named (sealed when
\* it is ours), or refused with a third party's directory at its name,
\* which the code then opens as a parent, as it does without batching.
OpenAsParent(p) ==
    IF p = Root THEN TRUE
    ELSE \/ ph[p] \in {"ready", "existing"}
         \/ ph[p] = "refused" /\ fino[p] # NoIno

\* The batch rule (#217 review, finding 1): a directory joins only under the
\* root, a member, or a directory decided before; the mutation admits one
\* under an existing directory still held, undecided.
CanDecide(d) ==
    \/ OpenAsParent(Parent(d))
    \/ /\ Mutation = "admit_under_held"
       /\ Parent(d) # Root
       /\ ph[Parent(d)] = "todo"
       /\ loc[Parent(d)] = "final"

\* Our inode for d is gone everywhere: forget its number and mode.
Forget(d, l, dl) == l = "none" /\ dl = "none"

\* The states a name can hold after a power loss: a prefix of the
\* operations from its durable state to its current one.
Between(a, b) ==
    CASE a = b -> {a}
      [] a = "none" /\ b = "tmp" -> {"none", "tmp"}
      [] a = "none" /\ b = "final" -> {"none", "tmp", "final"}
      [] a = "tmp" /\ b = "final" -> {"tmp", "final"}
      [] a = "tmp" /\ b = "none" -> {"tmp", "none"}
      [] OTHER -> {a, b}

\* Make every name inside p durable (a seal of p).
SealNames(p) == [x \in Dirs |-> IF Parent(x) = p THEN loc[x] ELSE dloc[x]]

ClearBound(r, gone) == [x \in Dirs |-> IF r[x] \in gone THEN NoIno ELSE r[x]]

-----------------------------------------------------------------------------
Init ==
    /\ run = 0 /\ live = FALSE /\ crashes = 0 /\ foreign = 0
    /\ loc = [d \in Dirs |-> "none"] /\ dloc = [d \in Dirs |-> "none"]
    /\ ino = [d \in Dirs |-> NoIno]
    /\ mode = [d \in Dirs |-> "0700"] /\ dmode = [d \in Dirs |-> "0700"]
    /\ fino = [d \in Dirs |-> NoIno] /\ fmode = [d \in Dirs |-> "final"]
    /\ rec = [d \in Dirs |-> NoIno]
    /\ ph = [d \in Dirs |-> "todo"]
    /\ child = [d \in Dirs |-> FALSE] /\ everDone = [d \in Dirs |-> FALSE]
    /\ multiRec = [d \in Dirs |-> FALSE]
    /\ adoptedForeign = FALSE /\ adoptedMulti = FALSE /\ partial = FALSE

(* A session starts: the root is swept of this store's directory           *)
(* temporaries. Records bound to them are cleared first, then each empty   *)
(* one is removed, and the root is sealed.                                  *)
StartRun ==
    /\ ~live /\ run < MaxRuns
    /\ LET swept == {d \in Children(Root) : loc[d] = "tmp"}
           empty == {d \in swept : \A c \in Children(d) : loc[c] = "none"}
           loc1 == [x \in Dirs |-> IF x \in empty THEN "none" ELSE loc[x]]
       IN /\ rec' = ClearBound(rec, {ino[d] : d \in swept})
          /\ loc' = loc1
          /\ dloc' = IF swept = {} THEN dloc
                     ELSE [x \in Dirs |-> IF Parent(x) = Root THEN loc1[x]
                                          ELSE dloc[x]]
          /\ ino' = [x \in Dirs |-> IF x \in empty THEN NoIno ELSE ino[x]]
    /\ run' = run + 1 /\ live' = TRUE
    /\ ph' = [d \in Dirs |-> "todo"]
    /\ UNCHANGED <<crashes, foreign, mode, dmode, fino, fmode, child, everDone,
                   multiRec, adoptedForeign, adoptedMulti, partial>>

(* An existing directory at d's name is decided: swept, then adopted when   *)
(* its record names its inode (its parent sealed first), else its stale     *)
(* record cleared and its mode checked.                                     *)
DecideExisting(d) ==
    /\ live /\ ph[d] = "todo" /\ CanDecide(d) /\ Occupied(d)
    /\ LET swept == {c \in Children(d) : loc[c] = "tmp" /\ ph[c] = "todo"}
           empty == {c \in swept : \A g \in Children(c) : loc[g] = "none"}
           loc1 == [x \in Dirs |-> IF x \in empty THEN "none" ELSE loc[x]]
           dloc1 == IF empty = {} THEN dloc
                    ELSE [x \in Dirs |-> IF Parent(x) = d THEN loc1[x]
                                         ELSE dloc[x]]
           rec1 == ClearBound(rec, {ino[c] : c \in swept})
           at == AtFinal(d)
           ours == loc[d] = "final"
           bound == rec1[d] = at
           fresh == /\ rec1[d] = IntentIno /\ ours /\ mode[d] = "0700"
                    /\ \A c \in Children(d) : loc1[c] = "none"
           owned == bound \/ fresh \/ Mutation = "adopt_unbound"
           m == IF ours THEN mode[d] ELSE fmode[d]
       IN /\ loc' = loc1
          /\ ino' = [x \in Dirs |-> IF x \in empty THEN NoIno ELSE ino[x]]
          /\ IF owned
             THEN /\ dloc' = [x \in Dirs |-> IF Parent(x) = Parent(d)
                                             THEN loc1[x] ELSE dloc1[x]]
                  /\ rec' = [rec1 EXCEPT ![d] = at]
                  /\ ph' = [ph EXCEPT ![d] = "ready"]
                  /\ adoptedForeign' = (adoptedForeign \/ ~ours)
                  /\ adoptedMulti' = (adoptedMulti \/ multiRec[d])
             ELSE /\ dloc' = dloc1
                  /\ rec' = [rec1 EXCEPT ![d] = NoIno]
                  /\ ph' = [ph EXCEPT ![d] = IF m = "final" THEN "existing"
                                             ELSE "refused"]
                  /\ UNCHANGED <<adoptedForeign, adoptedMulti>>
    /\ multiRec' = [x \in Dirs |-> multiRec[x] /\ rec'[x] # NoIno]
    /\ UNCHANGED <<run, live, crashes, foreign, mode, dmode, fino, fmode,
                   child, everDone, partial>>

(* A new directory: a tagged mkdirat in its parent. *)
Mkdir(d) ==
    /\ live /\ NoReplaceRename /\ ph[d] = "todo" /\ CanDecide(d)
    /\ ~Occupied(d) /\ loc[d] = "none" /\ dloc[d] = "none"
    /\ \E i \in Free :
         /\ ino' = [ino EXCEPT ![d] = i]
         /\ loc' = [loc EXCEPT ![d] = "tmp"]
         /\ mode' = [mode EXCEPT ![d] = "0700"]
         /\ dmode' = [dmode EXCEPT ![d] = "0700"]
    /\ ph' = [ph EXCEPT ![d] = "mkdir"]
    /\ UNCHANGED <<run, live, crashes, foreign, dloc, fino, fmode, rec, child,
                   everDone, multiRec, adoptedForeign, adoptedMulti, partial>>

(* One seal of p: every name in p is durable. The temporaries made in p    *)
(* become sealed, and the directories renamed into p ready. The mutations *)
(* advance every directory of the level, though only p was sealed.         *)
SealParent(p) ==
    /\ live
    /\ \E d \in Children(p) : ph[d] \in {"mkdir", "renamed", "fbmkdir"}
    /\ dloc' = SealNames(p)
    /\ ph' = [x \in Dirs |->
                CASE ph[x] = "mkdir"
                       /\ (Parent(x) = p \/ Mutation = "level_seals_one_parent")
                       -> "sealed"
                  [] ph[x] = "renamed"
                       /\ (Parent(x) = p
                           \/ Mutation = "level_rename_seals_one_parent")
                       -> "ready"
                  [] ph[x] = "fbmkdir" /\ Parent(x) = p -> "fbsealed"
                  [] OTHER -> ph[x]]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, ino, mode, dmode, fino,
                   fmode, rec, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* One store commit binds the records of S, each to its directory's inode. *)
Record(S) ==
    /\ live /\ S # {}
    /\ BatchCreate \/ Cardinality(S) = 1
    /\ \A d \in S : \/ ph[d] = "sealed"
                    \/ Mutation = "record_before_seal" /\ ph[d] = "mkdir"
    /\ rec' = [d \in Dirs |-> IF d \in S THEN ino[d] ELSE rec[d]]
    /\ multiRec' = [d \in Dirs |-> IF d \in S THEN Cardinality(S) > 1
                                   ELSE multiRec[d]]
    /\ ph' = [d \in Dirs |-> IF d \in S THEN "recorded" ELSE ph[d]]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode,
                   fino, fmode, child, everDone, adoptedForeign, adoptedMulti,
                   partial>>

(* The exclusive rename into place; a taken name refuses it (EEXIST). *)
Rename(d) ==
    /\ live
    /\ \/ ph[d] = "recorded"
       \/ Mutation = "rename_before_record" /\ ph[d] = "sealed"
    /\ IF Occupied(d)
       THEN /\ ph' = [ph EXCEPT ![d] = "eexist"]
            /\ UNCHANGED loc
       ELSE /\ loc' = [loc EXCEPT ![d] = "final"]
            /\ ph' = [ph EXCEPT ![d] = "renamed"]
    /\ UNCHANGED <<run, live, crashes, foreign, dloc, ino, mode, dmode, fino,
                   fmode, rec, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* A refused rename's temporary is discarded: its record cleared, then the *)
(* temporary removed (discard_directory). The mutation removes it first.   *)
DiscardClear(d) ==
    /\ live
    /\ \/ ph[d] = "eexist" /\ Mutation # "discard_unlink_before_clear"
       \/ ph[d] = "eexistU"
    /\ rec' = [rec EXCEPT ![d] = IF rec[d] = ino[d] THEN NoIno ELSE rec[d]]
    /\ multiRec' = [multiRec EXCEPT ![d] = FALSE]
    /\ ph' = [ph EXCEPT ![d] = IF ph[d] = "eexist" THEN "eexistC" ELSE "refused"]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode,
                   fino, fmode, child, everDone, adoptedForeign, adoptedMulti,
                   partial>>

DiscardUnlink(d) ==
    /\ live
    /\ \/ ph[d] = "eexistC"
       \/ ph[d] = "eexist" /\ Mutation = "discard_unlink_before_clear"
    /\ loc' = [loc EXCEPT ![d] = "none"]
    /\ ino' = [ino EXCEPT ![d] = IF dloc[d] = "none" THEN NoIno ELSE ino[d]]
    /\ ph' = [ph EXCEPT ![d] = IF ph[d] = "eexistC" THEN "refused" ELSE "eexistU"]
    /\ UNCHANGED <<run, live, crashes, foreign, dloc, mode, dmode, fino, fmode,
                   rec, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* R-N119's fallback: an intent first, then a plain mkdirat at the final   *)
(* name, the parent sealed (SealParent), then the record bound.            *)
Intent(d) ==
    /\ live /\ ~NoReplaceRename /\ ph[d] = "todo" /\ CanDecide(d)
    /\ ~Occupied(d) /\ loc[d] = "none" /\ dloc[d] = "none"
    /\ rec' = [rec EXCEPT ![d] = IntentIno]
    /\ multiRec' = [multiRec EXCEPT ![d] = FALSE]
    /\ ph' = [ph EXCEPT ![d] = "intent"]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode,
                   fino, fmode, child, everDone, adoptedForeign, adoptedMulti,
                   partial>>

FallbackMkdir(d) ==
    /\ live /\ ph[d] = "intent"
    /\ IF Occupied(d) \/ Free = {}
       THEN \* EEXIST, or any other mkdirat failure: the intent is cleared
            \* at once, so it never claims a directory someone else made.
            /\ rec' = [rec EXCEPT ![d] = NoIno]
            /\ ph' = [ph EXCEPT ![d] = "refused"]
            /\ UNCHANGED <<loc, ino, mode, dmode>>
       ELSE /\ \E i \in Free :
                 /\ ino' = [ino EXCEPT ![d] = i]
                 /\ loc' = [loc EXCEPT ![d] = "final"]
                 /\ mode' = [mode EXCEPT ![d] = "0700"]
                 /\ dmode' = [dmode EXCEPT ![d] = "0700"]
            /\ ph' = [ph EXCEPT ![d] = "fbmkdir"]
            /\ UNCHANGED rec
    /\ UNCHANGED <<run, live, crashes, foreign, dloc, fino, fmode, child,
                   everDone, multiRec, adoptedForeign, adoptedMulti, partial>>

Bind(d) ==
    /\ live /\ ph[d] = "fbsealed"
    /\ rec' = [rec EXCEPT ![d] = ino[d]]
    /\ ph' = [ph EXCEPT ![d] = "ready"]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode,
                   fino, fmode, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* An output's group commit inside a decided directory. *)
ChildCommit(d) ==
    /\ live /\ ~child[d]
    /\ \/ ph[d] \in {"ready", "existing"}
       \/ Mutation = "child_before_dirseal" /\ ph[d] = "renamed"
    /\ child' = [child EXCEPT ![d] = TRUE]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode,
                   fino, fmode, rec, ph, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

\* finish_receive runs finish_directories only once every directory was
\* decided and nothing was refused.
AllDecided == \A d \in Dirs : ph[d] \in {"ready", "existing", "done"} \cup Finishing

(* Finishing, deepest first: the final mode, then a seal of the directory. *)
Chmod(d) ==
    /\ live /\ AllDecided /\ ph[d] = "ready"
    /\ \A c \in Children(d) : ph[c] \notin {"ready", "chmod"}
    /\ mode' = [mode EXCEPT ![d] = "final"]
    /\ ph' = [ph EXCEPT ![d] = "chmod"]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, dmode, fino,
                   fmode, rec, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

SealDir(d) ==
    /\ live /\ ph[d] = "chmod"
    /\ dmode' = [dmode EXCEPT ![d] = mode[d]]
    /\ dloc' = SealNames(d)
    /\ ph' = [ph EXCEPT ![d] = "fsealed"]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, ino, mode, fino, fmode,
                   rec, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* One commit completes S: alone, or (BatchFinish) every finished one. *)
Complete(S) ==
    /\ live /\ S # {}
    /\ \A d \in S : \/ ph[d] = "fsealed"
                    \/ Mutation = "complete_before_seal" /\ ph[d] = "chmod"
    /\ IF BatchFinish
       THEN /\ \A d \in Dirs : ph[d] \notin {"ready", "chmod"} \/ d \in S
            /\ \A d \in Dirs : ph[d] = "fsealed" => d \in S
       ELSE Cardinality(S) = 1
    /\ rec' = [d \in Dirs |-> IF d \in S THEN NoIno ELSE rec[d]]
    /\ multiRec' = [d \in Dirs |-> IF d \in S THEN FALSE ELSE multiRec[d]]
    /\ everDone' = [d \in Dirs |-> everDone[d] \/ d \in S]
    /\ ph' = [d \in Dirs |-> IF d \in S THEN "done" ELSE ph[d]]
    /\ UNCHANGED <<run, live, crashes, foreign, loc, dloc, ino, mode, dmode,
                   fino, fmode, child, adoptedForeign, adoptedMulti, partial>>

(* The session ends, with nothing half made. *)
Exit ==
    /\ live
    /\ \A d \in Dirs : ph[d] \in {"todo", "ready", "existing", "refused", "done"}
    /\ live' = FALSE
    /\ UNCHANGED <<run, crashes, foreign, loc, dloc, ino, mode, dmode, fino,
                   fmode, rec, ph, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

-----------------------------------------------------------------------------
(* The environment *)

(* The process dies; the page cache keeps every name. *)
Stop ==
    /\ live /\ crashes < MaxCrashes
    /\ live' = FALSE /\ crashes' = crashes + 1
    /\ UNCHANGED <<run, foreign, loc, dloc, ino, mode, dmode, fino, fmode, rec,
                   ph, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* Power is lost: each unsealed name and mode change is kept or lost on   *)
(* its own; nothing survives inside a directory whose creation was lost. *)
PowerLoss ==
    /\ crashes < MaxCrashes
    /\ \E f \in [Dirs -> Names], g \in [Dirs -> Modes] :
         /\ \A d \in Dirs : f[d] \in Between(dloc[d], loc[d])
         /\ \A d \in Dirs : Parent(d) # Root /\ f[Parent(d)] = "none"
                              /\ fino[Parent(d)] = NoIno => f[d] = "none"
         /\ \A d \in Dirs : g[d] \in {dmode[d], mode[d]}
         /\ loc' = f /\ dloc' = f
         /\ ino' = [d \in Dirs |-> IF f[d] = "none" THEN NoIno ELSE ino[d]]
         /\ mode' = [d \in Dirs |-> IF f[d] = "none" THEN "0700" ELSE g[d]]
         /\ dmode' = [d \in Dirs |-> IF f[d] = "none" THEN "0700" ELSE g[d]]
         /\ partial' = (partial \/
               \E x, y \in Dirs :
                  /\ x # y /\ Parent(x) = Parent(y)
                  /\ loc[x] = "final" /\ dloc[x] # "final"
                  /\ loc[y] = "final" /\ dloc[y] # "final"
                  /\ f[x] = "final" /\ f[y] # "final")
    /\ live' = FALSE /\ crashes' = crashes + 1
    /\ UNCHANGED <<run, foreign, fino, fmode, rec, ph, child, everDone,
                   multiRec, adoptedForeign, adoptedMulti>>

(* Any other seal of p, or a journal force: p's names become durable. *)
EnvSeal(p) ==
    /\ \E d \in Children(p) : loc[d] # dloc[d]
    /\ dloc' = SealNames(p)
    /\ UNCHANGED <<run, live, crashes, foreign, loc, ino, mode, dmode, fino,
                   fmode, rec, ph, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

(* A third party makes a directory at d's free final name. *)
Foreign(d) ==
    /\ foreign < MaxForeign /\ ~Occupied(d) /\ ParentExists(d)
    /\ \E i \in Free, m \in {"final", "other"} :
         /\ fino' = [fino EXCEPT ![d] = i]
         /\ fmode' = [fmode EXCEPT ![d] = m]
    /\ foreign' = foreign + 1
    /\ UNCHANGED <<run, live, crashes, loc, dloc, ino, mode, dmode, rec, ph,
                   child, everDone, multiRec, adoptedForeign, adoptedMulti,
                   partial>>

(* It replaces its own directory there: a new inode, any free number. *)
ForeignReplace(d) ==
    /\ foreign < MaxForeign /\ fino[d] # NoIno
    /\ \A c \in Children(d) : loc[c] = "none" /\ dloc[c] = "none"
    /\ \E i \in Free : fino' = [fino EXCEPT ![d] = i]
    /\ foreign' = foreign + 1
    /\ UNCHANGED <<run, live, crashes, loc, dloc, ino, mode, dmode, fmode,
                   rec, ph, child, everDone, multiRec, adoptedForeign,
                   adoptedMulti, partial>>

\* Every run has been made: the behaviour may stop here.
Terminated ==
    /\ ~live /\ run = MaxRuns
    /\ UNCHANGED vars

Protocol ==
    \/ StartRun \/ Exit
    \/ \E d \in Dirs :
         \/ DecideExisting(d) \/ Mkdir(d) \/ Rename(d) \/ DiscardClear(d)
         \/ DiscardUnlink(d) \/ Intent(d) \/ FallbackMkdir(d) \/ Bind(d)
         \/ ChildCommit(d) \/ Chmod(d) \/ SealDir(d)
    \/ \E p \in Dirs \cup {Root} : SealParent(p)
    \/ \E S \in SUBSET Dirs : Record(S) \/ Complete(S)

Environment ==
    \/ Stop \/ PowerLoss
    \/ \E p \in Dirs \cup {Root} : EnvSeal(p)
    \/ \E d \in Dirs : Foreign(d) \/ ForeignReplace(d)

Next == Protocol \/ Environment \/ Terminated

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* PROPERTIES                                                               *)

TypeOK ==
    /\ run \in 0..MaxRuns /\ live \in BOOLEAN /\ crashes \in 0..MaxCrashes
    /\ foreign \in 0..MaxForeign
    /\ loc \in [Dirs -> Names] /\ dloc \in [Dirs -> Names]
    /\ ino \in [Dirs -> Inos \cup {NoIno}]
    /\ mode \in [Dirs -> Modes] /\ dmode \in [Dirs -> Modes]
    /\ fino \in [Dirs -> Inos \cup {NoIno}]
    /\ fmode \in [Dirs -> {"final", "other"}]
    /\ rec \in [Dirs -> Inos \cup {NoIno, IntentIno}]
    /\ ph \in [Dirs -> Phases]
    /\ child \in [Dirs -> BOOLEAN] /\ everDone \in [Dirs -> BOOLEAN]
    /\ multiRec \in [Dirs -> BOOLEAN]
    /\ adoptedForeign \in BOOLEAN /\ adoptedMulti \in BOOLEAN
    /\ partial \in BOOLEAN

(* A record bound to an inode names d's own inode, under a durable name   *)
(* (power_loss.rs: "recorded directory inode is not named").              *)
RecordNamesDirectory ==
    \A d \in Dirs : rec[d] \in Inos => (ino[d] = rec[d] /\ dloc[d] # "none")

(* R-N102's core: a directory of ours durable under its final name, never  *)
(* completed, is bound by its record (or an intent, R-N119), so a resume  *)
(* can always adopt it.                                                     *)
NoStrandedDirectory ==
    \A d \in Dirs : (dloc[d] = "final" /\ ~everDone[d])
                      => rec[d] \in {ino[d], IntentIno}

(* A completed directory has its final name and its final mode, durably. *)
CompleteImpliesFinal ==
    \A d \in Dirs : everDone[d] => (dloc[d] = "final" /\ dmode[d] = "final")

\* d is reached from the root by durable final names (ours or a third party's).
NamedDur(d) ==
    /\ dloc[d] = "final" \/ fino[d] # NoIno
    /\ IF Parent(d) = Root THEN TRUE
       ELSE dloc[Parent(d)] = "final" \/ fino[Parent(d)] # NoIno

(* An output committed inside d means d is durably named (#74 N1). *)
ChildImpliesNamed == \A d \in Dirs : child[d] => NamedDur(d)

(* No third party's directory is ever adopted (R-N102, N3). *)
NeverAdoptForeign == ~adoptedForeign

(* Nothing of ours is durable inside a directory of ours whose final name *)
(* is not: a temporary a sweep could not remove (#217 review, finding 1).  *)
NoDirectoryUnderTemporary ==
    \A d \in Dirs :
        (Parent(d) # Root /\ dloc[d] # "none")
            => (dloc[Parent(d)] = "final" \/ fino[Parent(d)] # NoIno)

WithinBudget == run \in 0..MaxRuns => TLCGet("duration") < BudgetSeconds

(* Reachability witnesses: each holds until the bound reaches its state.  *)
Witness_PartialLevel == ~partial
Witness_AdoptBatched == ~adoptedMulti
===============================================================================
