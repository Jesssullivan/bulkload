------------------------------- MODULE GitCarry -------------------------------
(***************************************************************************)
(* Custody model of v1 git carry's capture chains and plan bases (Q42 lane *)
(* L4, OI-1003-Q43; re-root and GC per OI-1003-Q46). Checked with TLC; see *)
(* docs/formal/README.md, "GitCarry", for configs and results.             *)
(*                                                                         *)
(* Roles (OI-1003-Q43): the decision a capture makes is a pure function,   *)
(* decide(Inputs) -> Decision. Its reference copy is hs/GitCarryCore.hs,   *)
(* whose pinned rows a Rust property test (P67, lane L6) checks against    *)
(* the code; catalogue/Types.dhall types its Basis and Decision unions.    *)
(* This module checks only what TLC is for here: custody of the chain      *)
(* links and the plan base, and crash order. DecideCore below is a         *)
(* transliteration of that decide; `just formal-nv` explores this          *)
(* module's core with the Haskell decide in its successor relation and    *)
(* requires TLC's state count.                                             *)
(*                                                                         *)
(* SCOPE                                                                   *)
(*   - One plan group: one item, or several items whose sources share a   *)
(*     common repository and so a plan base (estate::capture_groups,       *)
(*     prepare_base, shared-{group}.base).                                 *)
(*   - Capture passes under estate.lock, one item at a time: the decision  *)
(*     (prepare_base, retained_capture, chainable, chain_offer, the        *)
(*     ExportOptions match in git_carry::export_pass), then publish_bundle,*)
(*     the dependency sidecars ({bundle}.base, {bundle}.prior) and the     *)
(*     {item}.capture record, each a separate durable step.                *)
(*   - Chain depth (chain::CHAIN_DEPTH_LIMIT), re-basing at the limit, and *)
(*     Q46's re-root policy (RootWindow > 0, no code yet: lane L8).        *)
(*   - Q46's CORPUS GC between passes (GCOn, no code yet: lane L8).        *)
(*   - L6b's fix 2, a chain kept under a plan base (ChainUnderBase, no     *)
(*     code yet).                                                          *)
(*   - Crashes of the capture host between any two durable steps, in      *)
(*     particular between the sidecars and the record.                    *)
(*   - Third-party damage to CORPUS: a bundle deleted, or rewritten in     *)
(*     place (a new identity and digest).                                  *)
(*   - Source history moving (a new tip) and being rewritten (the          *)
(*     retained tips pruned at the source: chain::source_held_tips).       *)
(*   - Restore (estate::apply_item, import_base, chain_links under         *)
(*     LinkBinding::Digest, chain::flatten), evaluated as a state function *)
(*     in every state rather than run as an action.                        *)
(*                                                                         *)
(* ABSTRACTIONS (what is NOT modelled)                                     *)
(*   - Content. A bundle records the source tip it captured, as (ver,     *)
(*     gen); its prerequisites are its prior link's tips or its base's.   *)
(*     Pack contents and the object-set laws are P64/P65's job (the Rust  *)
(*     pack scan); there is no PackExcludesHeld here.                      *)
(*   - Each durable write (estate::write: temporary, fsync, rename, then  *)
(*     a directory seal) is one atomic step, durable at once. publish_     *)
(*     bundle's hard link is durable when it returns: its directory entry *)
(*     is sealed by the next write in CORPUS, before any record names it, *)
(*     and an orphan that a crash drops is never depended on.             *)
(*   - The two dependency sidecars are one step (`side`). Only fix 2      *)
(*     writes both; a crash between them leaves an orphan either way.     *)
(*   - The capture key is the source tip. Drift, racy seats and the pass  *)
(*     start (.drift, .parts) are inputs of the decision core that the    *)
(*     model holds fixed (no drift, settled, start recorded); the pinned  *)
(*     rows vary them. Shallow sources are not modelled here either.      *)
(*   - Items capture one at a time (estate.lock, jobs = 1); the code may  *)
(*     run two jobs inside the lock, which shares no custody state but    *)
(*     the plan base, created once under the group's mutex.              *)
(*   - STATE attempt directories are never depended on: publish_bundle    *)
(*     and prepare_base hard-link into CORPUS, so collecting an attempt   *)
(*     under the lock cannot break custody. STATE GC is not modelled.     *)
(*   - Sidecars are never damaged; only bundle files are.                 *)
(*                                                                         *)
(* CODE MAP (A = crates/bulkload-agent/src)                                *)
(*   StartBase, BaseRecord  A/estate.rs prepare_base, retained_base,      *)
(*                          group_base; A/git_carry/shared.rs export_base *)
(*   Capture                A/estate.rs capture_item, retained_capture,   *)
(*                          chainable, chain_links (LinkBinding::Custody),*)
(*                          chain_offer; A/git_carry.rs ExportOptions,    *)
(*                          export_pass; A/git_carry/shared.rs            *)
(*                          write_chained, write_bundle, requires_base;   *)
(*                          A/git_carry/chain.rs CHAIN_DEPTH_LIMIT,       *)
(*                          source_held_tips; DecideCore: decide (L6,     *)
(*                          pending; hs/GitCarryCore.hs is the reference) *)
(*   Publish                A/estate.rs publish_bundle                    *)
(*   Sidecars               A/estate.rs publish_prior, publish_sidecars,  *)
(*                          capture_item's {bundle}.base write            *)
(*   Record                 A/estate.rs capture_item's {item}.capture     *)
(*   GC                     Q46 STATE/CORPUS GC (lane L8, no code yet)    *)
(*   ApplyOutcome           A/estate.rs apply_item, import_base,          *)
(*                          chain_links (LinkBinding::Digest);            *)
(*                          A/git_carry/chain.rs flatten;                 *)
(*                          A/git_carry.rs stage_bundle                   *)
(*   Advance, Rewrite, Damage, Crash   the environment                    *)
(*                                                                         *)
(* NEGATIVE CONFIGS set Mutation to break exactly one rule; each MUST      *)
(* produce a counterexample on its named property (README.md).            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Items,            \* plan items (model values); two or more share a base
    DepthLimit,       \* chain::CHAIN_DEPTH_LIMIT (8 in the code)
    RootWindow,       \* Q46: captures per root; 0 is v1 (never re-roots)
    ChainUnderBase,   \* L6b fix 2: a based capture still chains on its prior
    GCOn,             \* Q46: CORPUS GC between capture passes
    MaxCommits,       \* source tip moves
    MaxRewrites,      \* source history rewrites (retained tips pruned)
    MaxCrashes,       \* capture-host crashes inside a pass
    MaxDamage,        \* third-party deletes or rewrites of CORPUS bundles
    DamageBase,       \* damage may reach a plan base bundle
    BaseMissingTyped, \* apply refuses a missing plan base by name (an
                      \* assumption the code does not meet: import_base
                      \* stages it with a bare IO; README)
    Mutation,         \* "none", or one deliberate rule break
    BudgetSeconds     \* wall-clock budget, checked by WithinBudget

Mutations == {"none", "chain_ignores_depth", "gc_deletes_depended",
              "base_replaced_live", "sidecar_after_record",
              "skip_flatten_verify", "hit_ignores_chain"}

(* The decision core's closed unions: catalogue/Types.dhall types them, and *)
(* tla-check requires these sets to equal the catalogue's labels.          *)
Decisions == {"Hit", "Export", "Refuse"}
Bases     == {"SelfContained", "Base", "Chain", "BaseAndChain"}
Rebases   == {"NoRebase", "NewRoot", "Reroot"}
Reuses    == {"NoRetained", "BlobReuse", "PassStartUnrecorded"}
Refusals  == {"ReceiptBindingInvalid"}

ASSUME /\ IsFiniteSet(Items) /\ Items # {}
       /\ DepthLimit \in Nat /\ DepthLimit > 0 /\ RootWindow \in Nat
       /\ ChainUnderBase \in BOOLEAN /\ GCOn \in BOOLEAN
       /\ MaxCommits \in Nat /\ MaxRewrites \in Nat /\ MaxCrashes \in Nat
       /\ MaxDamage \in Nat /\ DamageBase \in BOOLEAN
       /\ BaseMissingTyped \in BOOLEAN
       /\ Mutation \in Mutations /\ BudgetSeconds \in Nat

\* Two or more items share one common repository, so one plan base.
Grouped == Cardinality(Items) > 1

VARIABLES
    src,      \* the source repository's tip: [ver, gen]
    meta,     \* bundle id -> what its header and sidecars record (immutable)
    corpus,   \* bundle id -> "staged" | "ok" | "missing" | "replaced"
              \*              | "collected"
    side,     \* bundles whose dependency sidecars (.base, .prior) exist
    rec,      \* item -> the bundle its {item}.capture record names (0: none)
    baseRec,  \* the bundle shared-{group}.base names (0: none)
    cap,      \* the capture pass in flight (one at a time: estate.lock)
    crashes,  \* crashes so far
    damage    \* third-party damage so far

vars == <<src, meta, corpus, side, rec, baseRec, cap, crashes, damage>>

Ids == 1..Len(meta)

Idle == [st |-> "idle", item |-> CHOOSE x \in Items : TRUE, b |-> 0]

-----------------------------------------------------------------------------
(* Custody helpers *)

\* A file is at the bundle's CORPUS name (bytes possibly rewritten).
Present(b) == corpus[b] \in {"ok", "replaced"}

\* The {bundle}.prior sidecar: the bundle chains on an earlier link.
HasPrior(b) == b \in side /\ meta[b].prior # 0

\* shared::requires_base: the bundle's header declares prerequisites.
DeclaresPrereqs(b) == meta[b].prior # 0 \/ meta[b].base # 0

\* What a bundle depends on: its prior link and its plan base, transitively.
RECURSIVE Closure(_)
Closure(b) ==
    IF b = 0 THEN {}
    ELSE {b} \cup Closure(meta[b].prior) \cup Closure(meta[b].base)

\* Every bundle some item's record depends on.
Depended == UNION {Closure(rec[i]) : i \in Items}

\* The prerequisites a bundle's header declares: its prior link's tips, else
\* its base's commits (shared::prerequisites).
Prereqs(b) ==
    IF meta[b].prior # 0 THEN {meta[b].prior}
    ELSE IF meta[b].base # 0 THEN {meta[b].base}
    ELSE {}

\* estate::chain_links from bundle `cur`: walk the .prior sidecars, oldest
\* link first. Under LinkBinding::Custody (capture side) a link rewritten
\* in place is refused; under Digest (apply side) flatten checks digests.
\* Recorded depths are consistent by construction (meta is immutable), so
\* the walk's `expected` check is folded away.
RECURSIVE ChainWalk(_, _, _)
ChainWalk(cur, acc, custody) ==
    IF ~HasPrior(cur)
    THEN IF acc = <<>> THEN [code |-> "ok", links |-> acc]
         ELSE [code |-> "ReceiptBindingInvalid", links |-> <<>>]
    ELSE LET p == meta[cur].prior
             d == meta[p].depth
         IN IF d >= DepthLimit
            THEN [code |-> "ReceiptBindingInvalid", links |-> <<>>]
            ELSE IF ~Present(p)
            THEN [code |-> "SealedObjectMissing", links |-> <<>>]
            ELSE IF custody /\ corpus[p] = "replaced"
            THEN [code |-> "ReceiptBindingInvalid", links |-> <<>>]
            ELSE IF d = 0
            THEN IF HasPrior(p)
                 THEN [code |-> "ReceiptBindingInvalid", links |-> <<>>]
                 ELSE [code |-> "ok", links |-> <<p>> \o acc]
            ELSE ChainWalk(p, <<p>> \o acc, custody)

\* The oldest link of b's intact chain (b itself when it is unchained).
RootOf(b) ==
    LET w == ChainWalk(b, <<>>, TRUE)
    IN IF w.code = "ok" /\ w.links # <<>> THEN w.links[1] ELSE b

-----------------------------------------------------------------------------
(* Restore: what estate-apply would do with item i's record now.          *)
(* "restored", "refused" (a typed refusal) or "io" (a bare IO error).     *)

\* stage_bundle of a plan base: a missing file is canonicalize's ENOENT.
BaseOutcome(b) ==
    IF ~Present(b) THEN IF BaseMissingTyped THEN "refused" ELSE "io"
    ELSE IF corpus[b] = "replaced" THEN "refused"   \* DIGEST_MISMATCH
    ELSE "restored"

\* chain::flatten over chain_links(Digest): every link digest-checked, the
\* oldest self-contained (fix 2: its base imported first), every later
\* link's prerequisites satisfied by the links before it (verify_bundle).
FlattenOutcome(h) ==
    LET w == ChainWalk(h, <<>>, FALSE)
    IN IF w.code # "ok" THEN "refused"
       ELSE IF Mutation = "skip_flatten_verify" THEN "restored"
       ELSE IF \E k \in 1..Len(w.links) : corpus[w.links[k]] = "replaced"
       THEN "refused"                               \* DIGEST_MISMATCH
       ELSE IF meta[w.links[1]].base = 0 THEN "restored"
       ELSE IF ~ChainUnderBase THEN "refused"       \* RECEIPT_BINDING_INVALID
       ELSE BaseOutcome(meta[w.links[1]].base)

ApplyOutcome(i) ==
    LET h == rec[i]
    IN IF h = 0 THEN "refused"                       \* SEALED_OBJECT_MISSING
       ELSE IF ~Present(h) THEN "refused"            \* SEALED_OBJECT_MISSING
       ELSE IF corpus[h] = "replaced" THEN "refused" \* DIGEST_MISMATCH
       ELSE IF HasPrior(h) THEN FlattenOutcome(h)
       ELSE IF DeclaresPrereqs(h)
       THEN IF h \notin side THEN "io"   \* import_base reads an absent .base
            ELSE BaseOutcome(meta[h].base)
       ELSE "restored"

Restorable(i) == ApplyOutcome(i) = "restored"

\* The bundles a completed restore read, in the order it applied them.
ApplySeq(i) ==
    LET h == rec[i]
    IN IF HasPrior(h)
       THEN LET links == ChainWalk(h, <<>>, FALSE).links
                b0 == meta[links[1]].base
            IN (IF b0 # 0 /\ ChainUnderBase THEN <<b0>> ELSE <<>>)
               \o links \o <<h>>
       ELSE IF DeclaresPrereqs(h) THEN <<meta[h].base, h>>
       ELSE <<h>>

\* Every bundle's prerequisites name an intact bundle applied before it.
Satisfied(seq) ==
    \A k \in 1..Len(seq) : \A p \in Prereqs(seq[k]) :
        \E j \in 1..(k - 1) : seq[j] = p /\ corpus[p] = "ok"

\* Every bundle in item i's custody is at its recorded bytes, with its
\* dependency sidecars.
Intact(i) ==
    /\ rec[i] # 0
    /\ \A b \in Closure(rec[i]) :
           corpus[b] = "ok" /\ (DeclaresPrereqs(b) => b \in side)

-----------------------------------------------------------------------------
(* The decision core: DecideCore(Inputs(i)) is hs/GitCarryCore.hs's       *)
(* decide (the Rust decide.rs is lane L6), on the inputs the code reads.  *)

Inputs(i) ==
    LET h == rec[i]
        held == h # 0 /\ corpus[h] = "ok"
        chained == held /\ HasPrior(h)
        walk == ChainWalk(h, <<>>, TRUE)
        shape == IF ~held THEN "Unchained"
                 ELSE IF HasPrior(h) THEN "Chained"
                 ELSE IF DeclaresPrereqs(h) THEN "Based"
                 ELSE "Unchained"
        bound == meta[h].base
    IN [grouped |-> Grouped,
        base |-> IF ~Grouped THEN "NoGroup"
                 ELSE IF baseRec = 0 THEN "Absent"
                 ELSE IF corpus[baseRec] = "ok" THEN "Retained"
                 ELSE "Lost",
        retained |-> IF h = 0 THEN "NoRecord"
                     ELSE IF ~held THEN "BundleGone"
                     ELSE "Held",
        keyEqual |-> held /\ meta[h].ver = src.ver /\ meta[h].gen = src.gen,
        drifted |-> FALSE,
        settled |-> TRUE,
        passStart |-> TRUE,
        shape |-> shape,
        chainIntact |-> walk.code = "ok",
        prevBase |-> IF shape = "Unchained" THEN "None"
                     ELSE IF h \notin side THEN "Unreadable"
                     ELSE IF bound = 0 THEN "None"
                     ELSE IF corpus[bound] = "ok" THEN "Retained"
                     ELSE "Lost",
        depth |-> IF chained /\ walk.code = "ok" THEN Len(walk.links) ELSE 0,
        age |-> IF held THEN meta[h].age ELSE 0,
        tipsHeld |-> held /\ meta[h].gen = src.gen,
        rootHeld |-> held /\ meta[RootOf(h)].gen = src.gen,
        shallow |-> FALSE]

Outcome(d, basis, depth, rebase, reuse, refusal) ==
    [d |-> d, basis |-> basis, depth |-> depth, rebase |-> rebase,
     reuse |-> reuse, refusal |-> refusal]

HitOutcome == Outcome("Hit", "none", 0, "none", "none", "none")
RefuseOutcome == Outcome("Refuse", "none", 0, "none", "none",
                         "ReceiptBindingInvalid")
IoOutcome == Outcome("Io", "none", 0, "none", "none", "none")
Plan(basis, depth, rebase, reuse) ==
    Outcome("Export", basis, depth, rebase, reuse, "none")

\* A fresh root: a plan base's delta, else self-contained (a shallow source
\* is always self-contained: shared::write_bundle ignores the base).
RootBasis(inp) ==
    IF inp.grouped /\ ~inp.shallow THEN "Base" ELSE "SelfContained"

\* retained_capture's `restorable`: a chained bundle is a hit only while
\* its whole chain is retained (chain_links under Custody).
ChainOK(inp) ==
    inp.shape # "Chained" \/ inp.chainIntact \/ Mutation = "hit_ignores_chain"

\* The bound base a hit must find retained: `!chained && requires_base`
\* (fix 2: a chained capture's base too).
NeedsBoundBase(inp) ==
    inp.shape = "Based" \/ (inp.shape = "Chained" /\ ChainUnderBase)

HitPath(inp) ==
    /\ inp.retained = "Held" /\ inp.keyEqual /\ ~inp.drifted /\ inp.settled
    /\ ChainOK(inp)

\* chainable, chain_offer and the export_pass match, under Q46's re-root
\* policy (RootWindow > 0) and fix 2 (ChainUnderBase).
Extend(inp) ==
    LET reuse == IF inp.passStart THEN "BlobReuse" ELSE "PassStartUnrecorded"
        linkable == CASE inp.shape = "Chained" -> inp.chainIntact
                      [] inp.shape = "Based" -> ChainUnderBase
                      [] OTHER -> TRUE
        offered == linkable /\ (~inp.grouped \/ ChainUnderBase)
        chainBasis == IF inp.grouped THEN "BaseAndChain" ELSE "Chain"
        windowEnds == RootWindow > 0 /\ inp.age + 1 >= RootWindow
    IN IF inp.shallow \/ ~offered THEN Plan(RootBasis(inp), 0, "NoRebase", reuse)
       ELSE IF windowEnds THEN Plan(RootBasis(inp), 0, "NewRoot", reuse)
       ELSE IF inp.depth < DepthLimit \/ Mutation = "chain_ignores_depth"
       THEN IF inp.tipsHeld THEN Plan(chainBasis, inp.depth + 1, "NoRebase", reuse)
            ELSE Plan(RootBasis(inp), 0, "NoRebase", reuse)
       ELSE IF RootWindow > 0 /\ inp.rootHeld
       THEN Plan(chainBasis, 1, "Reroot", reuse)
       ELSE Plan(RootBasis(inp), 0, "NewRoot", reuse)

DecideCore(inp) ==
    IF inp.base = "Lost" THEN RefuseOutcome          \* prepare_base
    ELSE IF inp.retained # "Held"
    THEN Plan(RootBasis(inp), 0, "NoRebase", "NoRetained")
    ELSE IF HitPath(inp)
    THEN IF NeedsBoundBase(inp) /\ inp.prevBase = "Lost" THEN RefuseOutcome
         ELSE HitOutcome
    ELSE Extend(inp)

\* A hit that must read an absent {bundle}.base sidecar fails with a bare IO
\* before any decision (retained_capture's read).
CaptureOutcome(i) ==
    LET inp == Inputs(i)
    IN IF /\ inp.base # "Lost" /\ HitPath(inp) /\ NeedsBoundBase(inp)
          /\ inp.prevBase = "Unreadable"
       THEN IoOutcome
       ELSE DecideCore(inp)

\* The bundle an exported capture records.
NewMeta(i, o) ==
    LET h == rec[i]
        link == IF o.basis \in {"Chain", "BaseAndChain"}
                THEN IF o.rebase = "Reroot" THEN RootOf(h) ELSE h
                ELSE 0
    IN [kind |-> "capture", ver |-> src.ver, gen |-> src.gen,
        prior |-> link,
        base |-> IF o.basis \in {"Base", "BaseAndChain"} THEN baseRec ELSE 0,
        depth |-> o.depth,
        age |-> IF o.depth = 0 THEN 0 ELSE meta[h].age + 1]

-----------------------------------------------------------------------------
(* Protocol *)

BaseNeeded ==
    IF baseRec = 0 THEN TRUE
    ELSE Mutation = "base_replaced_live" /\ corpus[baseRec] # "ok"

\* prepare_base: export_base into a STATE attempt, hard-link the bundle into
\* CORPUS and seal it. Its record follows (BaseRecord). With a record whose
\* base is missing or rewritten, prepare_base refuses instead of replacing
\* it (DecideCore's first branch); base_replaced_live replaces it.
StartBase(i) ==
    /\ cap.st = "idle" /\ Grouped /\ BaseNeeded
    /\ meta' = Append(meta, [kind |-> "base", ver |-> src.ver, gen |-> src.gen,
                             prior |-> 0, base |-> 0, depth |-> 0, age |-> 0])
    /\ corpus' = Append(corpus, "ok")
    /\ cap' = [st |-> "baserec", item |-> i, b |-> Len(meta) + 1]
    /\ UNCHANGED <<src, side, rec, baseRec, crashes, damage>>

\* prepare_base's write of shared-{group}.base; the pass goes on.
BaseRecord ==
    /\ cap.st = "baserec"
    /\ baseRec' = cap.b
    /\ cap' = [cap EXCEPT !.st = "decide", !.b = 0]
    /\ UNCHANGED <<src, meta, corpus, side, rec, crashes, damage>>

\* capture_item: decide, then export into a STATE attempt (or reuse the
\* retained capture, or refuse by name).
Capture(i) ==
    /\ \/ cap.st = "idle" /\ ~(Grouped /\ BaseNeeded)
       \/ cap.st = "decide" /\ cap.item = i
    /\ LET o == CaptureOutcome(i)
       IN IF o.d = "Export"
          THEN /\ meta' = Append(meta, NewMeta(i, o))
               /\ corpus' = Append(corpus, "staged")
               /\ cap' = [st |-> "publish", item |-> i, b |-> Len(meta) + 1]
          ELSE /\ cap' = Idle
               /\ UNCHANGED <<meta, corpus>>
    /\ UNCHANGED <<src, side, rec, baseRec, crashes, damage>>

\* publish_bundle: hard link into CORPUS, sealed.
Publish ==
    /\ cap.st = "publish"
    /\ corpus' = [corpus EXCEPT ![cap.b] = "ok"]
    /\ cap' = [cap EXCEPT !.st =
                 IF DeclaresPrereqs(cap.b) /\ Mutation # "sidecar_after_record"
                 THEN "sidecars" ELSE "record"]
    /\ UNCHANGED <<src, meta, side, rec, baseRec, crashes, damage>>

\* {bundle}.base and publish_prior's {bundle}.prior, before the record.
Sidecars ==
    /\ cap.st = "sidecars"
    /\ side' = side \cup {cap.b}
    /\ cap' = IF Mutation = "sidecar_after_record" THEN Idle
              ELSE [cap EXCEPT !.st = "record"]
    /\ UNCHANGED <<src, meta, corpus, rec, baseRec, crashes, damage>>

\* The {item}.capture record names the new bundle.
Record ==
    /\ cap.st = "record"
    /\ rec' = [rec EXCEPT ![cap.item] = cap.b]
    /\ cap' = IF Mutation = "sidecar_after_record" /\ DeclaresPrereqs(cap.b)
              THEN [cap EXCEPT !.st = "sidecars"] ELSE Idle
    /\ UNCHANGED <<src, meta, corpus, side, baseRec, crashes, damage>>

\* Q46 GC under estate.lock: remove every CORPUS bundle (and its sidecars)
\* that no record and no base record depends on. gc_deletes_depended keeps
\* only what the records name directly, forgetting their chain links.
GCLive ==
    IF Mutation = "gc_deletes_depended"
    THEN {rec[i] : i \in Items} \cup {baseRec}
    ELSE Depended \cup Closure(baseRec)

Garbage == {b \in Ids : Present(b) /\ b \notin GCLive}

GC ==
    /\ GCOn /\ cap.st = "idle" /\ Garbage # {}
    /\ corpus' = [b \in Ids |-> IF b \in Garbage THEN "collected" ELSE corpus[b]]
    /\ side' = side \ Garbage
    /\ UNCHANGED <<src, meta, rec, baseRec, cap, crashes, damage>>

(* Environment *)

\* The source tip moves (a commit, a ref update).
Advance ==
    /\ src.ver < MaxCommits
    /\ src' = [src EXCEPT !.ver = @ + 1]
    /\ UNCHANGED <<meta, corpus, side, rec, baseRec, cap, crashes, damage>>

\* The source history is rewritten and pruned: no retained tip is held.
Rewrite ==
    /\ src.gen < MaxRewrites
    /\ src' = [src EXCEPT !.gen = @ + 1]
    /\ UNCHANGED <<meta, corpus, side, rec, baseRec, cap, crashes, damage>>

\* A third party deletes a CORPUS bundle or rewrites it in place.
Damage ==
    /\ damage < MaxDamage
    /\ \E b \in Ids :
          /\ corpus[b] = "ok"
          \* An IF, not a disjunction: TLC branches on a disjunction inside an
          \* action and would count each successor twice (README, "GitCarry").
          /\ IF DamageBase THEN TRUE ELSE meta[b].kind = "capture"
          /\ \E how \in {"missing", "replaced"} :
                corpus' = [corpus EXCEPT ![b] = how]
    /\ damage' = damage + 1
    /\ UNCHANGED <<src, meta, side, rec, baseRec, cap, crashes>>

\* The capture host crashes inside a pass: every durable step stands, the
\* STATE attempt is left behind, the pass is lost.
Crash ==
    /\ crashes < MaxCrashes /\ cap.st # "idle"
    /\ cap' = Idle
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<src, meta, corpus, side, rec, baseRec, damage>>

Init ==
    /\ src = [ver |-> 0, gen |-> 0]
    /\ meta = <<>>
    /\ corpus = <<>>
    /\ side = {}
    /\ rec = [i \in Items |-> 0]
    /\ baseRec = 0
    /\ cap = Idle
    /\ crashes = 0
    /\ damage = 0

Protocol ==
    \/ \E i \in Items : StartBase(i)
    \/ BaseRecord
    \/ \E i \in Items : Capture(i)
    \/ Publish \/ Sidecars \/ Record \/ GC

Environment == Advance \/ Rewrite \/ Damage \/ Crash

Next == Protocol \/ Environment

Spec == Init /\ [][Next]_vars

\* Fair protocol, unfair environment and faults.
LiveSpec == Spec /\ WF_vars(Protocol)

-----------------------------------------------------------------------------
(* PROPERTIES                                                               *)

MetaRec == [kind : {"base", "capture"}, ver : 0..MaxCommits,
            gen : 0..MaxRewrites, prior : Nat, base : Nat, depth : Nat,
            age : Nat]

OutcomeRec == [d : Decisions \cup {"Io"}, basis : Bases \cup {"none"},
               depth : Nat, rebase : Rebases \cup {"none"},
               reuse : Reuses \cup {"none"}, refusal : Refusals \cup {"none"}]

\* Also evaluates the decision core and the restore on every state, so a
\* partial or ill-typed definition is a TLC error, not a silent gap.
TypeOK ==
    /\ src \in [ver : 0..MaxCommits, gen : 0..MaxRewrites]
    /\ meta \in Seq(MetaRec)
    /\ Len(corpus) = Len(meta)
    /\ \A b \in Ids :
          /\ corpus[b] \in {"staged", "ok", "missing", "replaced", "collected"}
          /\ meta[b].prior < b /\ meta[b].base < b
    /\ side \subseteq Ids
    /\ rec \in [Items -> 0..Len(meta)]
    /\ baseRec \in 0..Len(meta)
    /\ cap.st \in {"idle", "baserec", "decide", "publish", "sidecars", "record"}
    /\ cap.item \in Items /\ cap.b \in 0..Len(meta)
    /\ crashes \in 0..MaxCrashes /\ damage \in 0..MaxDamage
    /\ \A i \in Items :
          /\ CaptureOutcome(i) \in OutcomeRec
          /\ ApplyOutcome(i) \in {"restored", "refused", "io"}

\* Every bundle's chain is at most DepthLimit links deep (a restore stages
\* at most DepthLimit + 1 bundles); under Q46 its root is younger than the
\* window, so a full re-pack happens at most once per window.
ChainDepthBounded ==
    \A b \in Ids :
        /\ meta[b].depth <= DepthLimit
        /\ RootWindow > 0 => meta[b].age < RootWindow

\* A restore that completes applied every bundle after the bundles its
\* prerequisites name, each at its recorded bytes: the oldest link is
\* self-contained (or its base came first), and each later link's
\* prerequisites are satisfied by the links before it (chain::flatten).
PrereqsSatisfiedByEarlierLinks ==
    \A i \in Items : rec[i] # 0 /\ Restorable(i) => Satisfied(ApplySeq(i))

\* A capture never reuses a retained record whose custody is broken: a
\* missing or rewritten link, a lost base, a missing sidecar.
BrokenLinkNeverReuseHit ==
    \A i \in Items : CaptureOutcome(i).d = "Hit" => Intact(i)

\* The plan base record never moves while a record depends on its base.
BaseNotReplacedWhileDepended ==
    \A i \in Items : \A b \in Closure(rec[i]) :
        meta[b].kind = "base" => b = baseRec

\* GC never removes a bundle a record or the base record depends on.
GCNeverDeletesDepended ==
    \A b \in Depended \cup Closure(baseRec) : corpus[b] # "collected"

\* A record names a published bundle whose dependency sidecars exist.
SidecarsBeforeRecord ==
    \A i \in Items : rec[i] # 0 =>
        /\ corpus[rec[i]] # "staged"
        /\ DeclaresPrereqs(rec[i]) => rec[i] \in side

\* Every record restores; or its item's next capture recaptures (exports a
\* new record) or refuses by name, keeping the missing custody visible. An
\* apply that does not restore is a typed refusal, never a bare IO.
RestoreOrRecapture ==
    \A i \in Items : rec[i] # 0 =>
        /\ ApplyOutcome(i) # "io"
        /\ Restorable(i) \/ CaptureOutcome(i).d \in {"Export", "Refuse"}

\* Wall-clock budget, evaluated on every state (README, "The budget is
\* state-level"): the conjunct over src makes it state-level.
WithinBudget == src.ver \in 0..MaxCommits => TLCGet("duration") < BudgetSeconds

\* Liveness: under a fair protocol, once the environment stops, every item
\* whose record does not restore (or has none) gets one that does.
ChainRecovery == \A i \in Items : ~Restorable(i) ~> Restorable(i)

=============================================================================
