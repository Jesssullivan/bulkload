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
(*     (git_carry::decide on what prepare_base, retained_capture and       *)
(*     chainable read; chain_offer and shared::write_capture act on it),   *)
(*     then publish_bundle, the dependency sidecars ({bundle}.base,        *)
(*     {bundle}.prior), the reuse manifest ({bundle}.reuse, lane L7:       *)
(*     ReuseManifest) and the {item}.capture record, each a separate       *)
(*     durable step.                                                       *)
(*   - Content names. A bundle's CORPUS name is its content               *)
(*     ({identity}-{digest}.bundle, shared-{digest}.bundle), so a          *)
(*     re-export of the same content lands on the same name: publish_      *)
(*     bundle and prepare_base reuse a file at that digest, link a deleted *)
(*     or collected one again, and refuse DIGEST_MISMATCH for a name that  *)
(*     holds other bytes. publish_prior keeps an intact chain already      *)
(*     recorded for the name.                                              *)
(*   - What a bundle's header declares (its prerequisite tips, `pre`) and  *)
(*     what its .prior sidecar names (`prior`) are separate values, each   *)
(*     set from the decision's chain path.                                 *)
(*   - Chain depth (chain::CHAIN_DEPTH_LIMIT), re-basing at the limit, and *)
(*     Q46's re-root policy (RootWindow > 0, no code yet: lane L8).        *)
(*   - Q46's CORPUS GC between passes (GCOn, no code yet: lane L8), one    *)
(*     bundle per step.                                                    *)
(*   - L6b's fix 2, a chain kept under a plan base (ChainUnderBase; the   *)
(*     code's only policy since lane L6b).                                 *)
(*   - Crashes of the capture host between any two durable steps, in      *)
(*     particular between the sidecars and the record.                    *)
(*   - Third-party damage to CORPUS: a bundle deleted, or rewritten in     *)
(*     place (a new identity and digest at the same name).                 *)
(*   - Source history moving (a new tip) and being rewritten (the          *)
(*     retained tips pruned at the source: chain::source_held_tips).       *)
(*   - Restore (estate::apply_item, import_base, chain_links under         *)
(*     LinkBinding::Digest, chain::flatten), evaluated as a state function *)
(*     in every state rather than run as an action.                        *)
(*                                                                         *)
(* ABSTRACTIONS (what is NOT modelled)                                     *)
(*   - Pack content. A bundle's name is (kind, item, the tip it captured  *)
(*     as (ver, gen), its basis, its declared prerequisite tips): the     *)
(*     writer and its inputs, which fix the bytes. Pack contents and the  *)
(*     object-set laws are P64/P65's job (the Rust pack scan); there is no *)
(*     PackExcludesHeld here.                                              *)
(*   - Identity. A file linked again under its name (after a delete or     *)
(*     GC) keeps one identity here. The code gives it a new StatIdentity, *)
(*     so a capture-side (Custody) reference recorded before it sees a     *)
(*     changed file and recaptures, as it does for a rewritten link, while *)
(*     apply binds by digest and sees what the model sees.                 *)
(*   - Source tips only move forward. A source returning to an earlier tip *)
(*     re-exports an earlier name; its outcomes are those of the same-tip *)
(*     re-export the model reaches (reuse, a new link, DIGEST_MISMATCH),  *)
(*     plus the identity effect above on links recorded before.           *)
(*   - Each durable write (estate::write: temporary, fsync, rename, then  *)
(*     a directory seal) is one atomic step, durable at once. publish_     *)
(*     bundle's hard link is durable when it returns: its directory entry *)
(*     is sealed by the next write in CORPUS, before any record names it, *)
(*     and an orphan that a crash drops is never depended on.             *)
(*   - The two dependency sidecars are one step (`sidecar`). Only fix 2   *)
(*     writes both; a crash between them leaves an orphan either way.     *)
(*   - The capture key is the source tip. Drift, racy seats and the pass  *)
(*     start (.drift, .parts) are inputs of the decision core that the    *)
(*     model holds fixed (no drift, settled, start recorded); the pinned  *)
(*     rows vary them. Shallow sources are not modelled here either.      *)
(*   - Items capture one at a time (estate.lock, jobs = 1); the code may  *)
(*     run two jobs inside the lock, which shares no custody state but    *)
(*     the plan base, created once under the group's mutex.              *)
(*   - One CORPUS writer. estate.lock lives in STATE, and two STATE dirs  *)
(*     may share one CORPUS (estate.rs, two_state_dirs_sharing_a_corpus_   *)
(*     fail_closed_on_interleaved_records). GC is safe here only because   *)
(*     no capture or apply runs beside it: L8 must make GC take a CORPUS-  *)
(*     level exclusive lock that every capture and every apply also take. *)
(*     A GC racing a second STATE's pass, or an apply, is not modelled.   *)
(*   - GC deletes one bundle with its sidecars per step and recomputes    *)
(*     what is garbage before each; a crash between two deletions is a    *)
(*     stop between steps. The order of a bundle and its sidecars inside  *)
(*     one deletion is not modelled.                                      *)
(*   - STATE attempt directories are never depended on: publish_bundle    *)
(*     and prepare_base hard-link into CORPUS, so collecting an attempt   *)
(*     under the lock cannot break custody. STATE GC is not modelled.     *)
(*   - Sidecars are never damaged; only bundle files are.                 *)
(*   - What a reuse manifest lists, and what a pass reuses from it. The    *)
(*     manifest is a custody fact here only as a file that exists or not:  *)
(*     no restore and no decision reads it (ApplyOutcome and DecideCore do *)
(*     not mention it), so either publication order is safe for a restore. *)
(*     That a manifest matches its bundle and that reuse reads no bundle   *)
(*     byte are P70's and P69's (Rust). The model checks the order alone:  *)
(*     a record never lacks its manifest.                                  *)
(*                                                                         *)
(* CODE MAP (A = crates/bulkload-agent/src)                                *)
(*   StartBase, BaseRecord  A/estate.rs prepare_base, retained_base,      *)
(*                          group_base; A/git_carry/shared.rs export_base *)
(*   Capture                A/estate.rs capture_item, retained_capture,   *)
(*                          chainable, chain_links (LinkBinding::Custody),*)
(*                          chain_offer; A/git_carry.rs ExportOptions,    *)
(*                          export_pass; A/git_carry/shared.rs            *)
(*                          write_capture, write_bundle, requires_base;   *)
(*                          A/git_carry/chain.rs CHAIN_DEPTH_LIMIT,       *)
(*                          source_held_tips; DecideCore:                 *)
(*                          A/git_carry/decide.rs decide (L6a; the        *)
(*                          reference is hs/GitCarryCore.hs's)            *)
(*   Publish                A/estate.rs publish_bundle                    *)
(*   Sidecars               A/estate.rs publish_prior, publish_sidecars,  *)
(*                          capture_item's {bundle}.base write            *)
(*   ReuseSidecar           A/estate.rs publish_reuse (lane L7); read by   *)
(*                          retained_manifest, reuse_offer                 *)
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
    DamageRewrites,   \* damage may rewrite a bundle in place (else it deletes)
    BaseMissingTyped, \* apply refuses a missing plan base by name, as the
                      \* code does since lane L6b (stage_base, #181);
                      \* FALSE is the bare IO before it (README)
    ReuseManifest,    \* L7: a pass publishes {bundle}.reuse before its record
    Mutation,         \* "none", or one deliberate rule break
    BudgetSeconds     \* wall-clock budget, checked by WithinBudget

Mutations == {"none", "chain_ignores_depth", "gc_deletes_depended",
              "base_replaced_live", "sidecar_after_record",
              "skip_flatten_verify", "hit_ignores_chain",
              "reroot_pre_mismatch", "reuse_after_record"}

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
       /\ DamageRewrites \in BOOLEAN /\ BaseMissingTyped \in BOOLEAN
       /\ ReuseManifest \in BOOLEAN
       /\ Mutation \in Mutations /\ BudgetSeconds \in Nat

\* Two or more items share one common repository, so one plan base.
Grouped == Cardinality(Items) > 1

\* The item field of a plan base's name: shared-{digest} names the group.
Group == CHOOSE x \in Items : TRUE

\* A bundle's dependency sidecars: its .prior (the link it chains on, and
\* that link's depth as the .prior records it) and its .base (the plan
\* base it was bound to), with Q46's root age. NoSidecar: no sidecar file.
NoSidecar == [prior |-> 0, pdepth |-> 0, base |-> 0, age |-> 0]

VARIABLES
    src,      \* the source repository's tip: [ver, gen]
    meta,     \* bundle id -> its CORPUS name, i.e. its bytes (immutable)
    corpus,   \* bundle id -> the file at that name: "staged" (exported,
              \*   never published) | "ok" | "missing" | "replaced"
              \*   | "collected"
    sidecar,  \* bundle id -> its dependency sidecars, or NoSidecar
    manifest, \* the bundle ids whose {bundle}.reuse manifest exists (L7)
    rec,      \* item -> the bundle its {item}.capture record names (0: none)
    baseRec,  \* the bundle shared-{group}.base names (0: none)
    cap,      \* the capture pass in flight (one at a time: estate.lock)
    crashes,  \* crashes so far
    damage    \* third-party damage so far

vars == <<src, meta, corpus, sidecar, manifest, rec, baseRec, cap, crashes,
          damage>>

Ids == 1..Len(meta)

Idle == [st |-> "idle", item |-> Group, b |-> 0, plan |-> NoSidecar]

-----------------------------------------------------------------------------
(* Custody helpers *)

\* A file is at the bundle's CORPUS name (bytes possibly rewritten).
Present(b) == corpus[b] \in {"ok", "replaced"}

\* The bundle's dependency sidecars exist.
HasSidecars(b) == sidecar[b] # NoSidecar

\* What its .prior and .base sidecars name (0: no such sidecar).
Link(b) == sidecar[b].prior
Bound(b) == sidecar[b].base

\* The {bundle}.prior sidecar: the bundle chains on an earlier link (0 is
\* no bundle: an item with no record).
HasPrior(b) == b # 0 /\ Link(b) # 0

\* Its chain's depth, as its .prior records it.
Depth(b) == IF HasPrior(b) THEN sidecar[b].pdepth + 1 ELSE 0

\* The source tip a bundle captured.
Tip(b) == [ver |-> meta[b].ver, gen |-> meta[b].gen]

\* shared::requires_base: the bundle's header declares prerequisites.
DeclaresPrereqs(b) == meta[b].pre # {}

\* What a bundle depends on, as its sidecars name it: its prior link and its
\* plan base, transitively. A link's tip is below its bundle's, so this ends.
RECURSIVE Closure(_)
Closure(b) ==
    IF b = 0 THEN {}
    ELSE {b} \cup Closure(Link(b)) \cup Closure(Bound(b))

\* Every bundle some item's record depends on.
Depended == UNION {Closure(rec[i]) : i \in Items}

Walked(code, links) == [code |-> code, links |-> links]

\* estate::chain_links from bundle `cur`: walk the .prior sidecars, oldest
\* link first. `exp` is the depth cur's own link must record once the walk
\* has left the head (acc # <<>>). Under LinkBinding::Custody (capture side)
\* a link rewritten in place is refused; under Digest (apply side) flatten
\* checks digests.
RECURSIVE ChainWalk(_, _, _, _)
ChainWalk(cur, acc, exp, custody) ==
    IF ~HasPrior(cur)
    THEN IF acc = <<>> THEN Walked("ok", acc)
         ELSE Walked("ReceiptBindingInvalid", <<>>)
    ELSE LET p == Link(cur)
             d == sidecar[cur].pdepth
         IN IF d >= DepthLimit \/ (acc # <<>> /\ d # exp)
            THEN Walked("ReceiptBindingInvalid", <<>>)
            ELSE IF ~Present(p)
            THEN Walked("SealedObjectMissing", <<>>)
            ELSE IF custody /\ corpus[p] = "replaced"
            THEN Walked("ReceiptBindingInvalid", <<>>)
            ELSE IF d = 0
            THEN IF HasPrior(p)
                 THEN Walked("ReceiptBindingInvalid", <<>>)
                 ELSE Walked("ok", <<p>> \o acc)
            ELSE ChainWalk(p, <<p>> \o acc, d - 1, custody)

Walk(b, custody) == ChainWalk(b, <<>>, 0, custody)

\* The oldest link of b's intact chain (b itself when it is unchained).
RootOf(b) ==
    LET w == Walk(b, TRUE)
    IN IF w.code = "ok" /\ w.links # <<>> THEN w.links[1] ELSE b

-----------------------------------------------------------------------------
(* Restore: what estate-apply would do with item i's record now.          *)
(* "restored", "refused" (a typed refusal) or "io" (a bare IO error).     *)
(* verify_bundle's prerequisite check is not part of it: that it never    *)
(* fails on a chain whose digests check is PrereqsSatisfiedByEarlierLinks.*)

\* stage_bundle of a plan base: a missing file is canonicalize's ENOENT.
BaseOutcome(b) ==
    IF ~Present(b) THEN IF BaseMissingTyped THEN "refused" ELSE "io"
    ELSE IF corpus[b] = "replaced" THEN "refused"   \* DIGEST_MISMATCH
    ELSE "restored"

\* chain::flatten over chain_links(Digest): every link digest-checked, the
\* oldest self-contained by its header (fix 2: its base imported first).
FlattenOutcome(h) ==
    LET w == Walk(h, FALSE)
    IN IF w.code # "ok" THEN "refused"
       ELSE IF Mutation = "skip_flatten_verify" THEN "restored"
       ELSE IF \E k \in 1..Len(w.links) : corpus[w.links[k]] = "replaced"
       THEN "refused"                               \* DIGEST_MISMATCH
       ELSE IF ~DeclaresPrereqs(w.links[1]) THEN "restored"
       ELSE IF ~ChainUnderBase THEN "refused"       \* RECEIPT_BINDING_INVALID
       ELSE IF Bound(w.links[1]) = 0 THEN "io"      \* an absent .base
       ELSE BaseOutcome(Bound(w.links[1]))

ApplyOutcome(i) ==
    LET h == rec[i]
    IN IF h = 0 THEN "refused"                       \* SEALED_OBJECT_MISSING
       ELSE IF ~Present(h) THEN "refused"            \* SEALED_OBJECT_MISSING
       ELSE IF corpus[h] = "replaced" THEN "refused" \* DIGEST_MISMATCH
       ELSE IF HasPrior(h) THEN FlattenOutcome(h)
       ELSE IF DeclaresPrereqs(h)
       THEN IF Bound(h) = 0 THEN "io"    \* import_base reads an absent .base
            ELSE BaseOutcome(Bound(h))
       ELSE "restored"

Restorable(i) == ApplyOutcome(i) = "restored"

\* The bundles a completed restore read, in the order it applied them: the
\* sidecars' chain, and the base the oldest link (or the head) is bound to.
ApplySeq(i) ==
    LET h == rec[i]
    IN IF HasPrior(h)
       THEN LET links == Walk(h, FALSE).links
                b0 == Bound(links[1])
            IN (IF b0 # 0 /\ ChainUnderBase THEN <<b0>> ELSE <<>>)
               \o links \o <<h>>
       ELSE IF DeclaresPrereqs(h) THEN <<Bound(h), h>>
       ELSE <<h>>

\* Every prerequisite tip a bundle's header declares is the tip of an intact
\* bundle applied before it (what verify_bundle checks).
Satisfied(seq) ==
    \A k \in 1..Len(seq) : \A t \in meta[seq[k]].pre :
        \E j \in 1..(k - 1) : Tip(seq[j]) = t /\ corpus[seq[j]] = "ok"

\* Every bundle in item i's custody is at its recorded bytes, with its
\* dependency sidecars.
Intact(i) ==
    /\ rec[i] # 0
    /\ \A b \in Closure(rec[i]) :
           corpus[b] = "ok" /\ (DeclaresPrereqs(b) => HasSidecars(b))

-----------------------------------------------------------------------------
(* The decision core: DecideCore(Inputs(i)) is hs/GitCarryCore.hs's       *)
(* decide, as is A/git_carry/decide.rs's (lane L6a, P67), on the inputs   *)
(* the code reads.                                                        *)

Inputs(i) ==
    LET h == rec[i]
        held == h # 0 /\ corpus[h] = "ok"
        chained == held /\ HasPrior(h)
        walk == Walk(h, TRUE)
        shape == IF ~held THEN "Unchained"
                 ELSE IF HasPrior(h) THEN "Chained"
                 ELSE IF DeclaresPrereqs(h) THEN "Based"
                 ELSE "Unchained"
        bound == Bound(h)
    IN [grouped |-> Grouped,
        base |-> IF ~Grouped THEN "NoGroup"
                 ELSE IF baseRec = 0 THEN "Absent"
                 ELSE IF corpus[baseRec] = "ok" THEN "Retained"
                 ELSE "Lost",
        retained |-> IF h = 0 THEN "NoRecord"
                     ELSE IF ~held THEN "BundleGone"
                     ELSE "Held",
        keyEqual |-> held /\ Tip(h) = src,
        drifted |-> FALSE,
        settled |-> TRUE,
        passStart |-> TRUE,
        shape |-> shape,
        chainIntact |-> walk.code = "ok",
        prevBase |-> IF shape = "Unchained" THEN "None"
                     ELSE IF ~HasSidecars(h) THEN "Unreadable"
                     ELSE IF bound = 0 THEN "None"
                     ELSE IF corpus[bound] = "ok" THEN "Retained"
                     ELSE "Lost",
        depth |-> IF chained /\ walk.code = "ok" THEN Len(walk.links) ELSE 0,
        age |-> IF held THEN sidecar[h].age ELSE 0,
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

\* v1's chainable, chain_offer and export_pass match (git_carry::decide's
\* extend since L6a), under Q46's re-root policy (RootWindow > 0) and fix 2
\* (ChainUnderBase).
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

\* The link an exported capture chains on: its .prior.
LinkOf(i, o) ==
    IF o.basis \in {"Chain", "BaseAndChain"}
    THEN IF o.rebase = "Reroot" THEN RootOf(rec[i]) ELSE rec[i]
    ELSE 0

\* The prerequisite tips its header declares (shared::prerequisites): the
\* tips of the link the chain path chose, else its base's commits.
\* reroot_pre_mismatch declares the head's tips on a re-root while the
\* .prior names the root.
DeclaredTips(i, o) ==
    IF o.basis \in {"Chain", "BaseAndChain"}
    THEN {Tip(IF o.rebase = "Reroot" /\ Mutation = "reroot_pre_mismatch"
              THEN rec[i] ELSE LinkOf(i, o))}
    ELSE IF o.basis = "Base" THEN {Tip(baseRec)}
    ELSE {}

\* The CORPUS name an exported capture lands on: its content.
NewMeta(i, o) ==
    [kind |-> "capture", item |-> i, ver |-> src.ver, gen |-> src.gen,
     basis |-> o.basis, pre |-> DeclaredTips(i, o)]

\* The sidecars the pass writes for it, unless publish_prior keeps the ones
\* already recorded for that name.
NewPlan(i, o) ==
    LET link == LinkOf(i, o)
    IN [prior |-> link,
        pdepth |-> IF link = 0 THEN 0 ELSE o.depth - 1,
        base |-> IF o.basis \in {"Base", "BaseAndChain"} THEN baseRec ELSE 0,
        age |-> IF o.depth = 0 THEN 0 ELSE sidecar[rec[i]].age + 1]

-----------------------------------------------------------------------------
(* Protocol *)

BaseNeeded ==
    IF baseRec = 0 THEN TRUE
    ELSE Mutation = "base_replaced_live" /\ corpus[baseRec] # "ok"

\* The CORPUS name of a plan base exported now.
BaseMeta ==
    [kind |-> "base", item |-> Group, ver |-> src.ver, gen |-> src.gen,
     basis |-> "SelfContained", pre |-> {}]

\* prepare_base: export_base into a STATE attempt, hard-link the bundle into
\* CORPUS under its content name and seal it. A name that holds other bytes
\* refuses DIGEST_MISMATCH (the pass ends, nothing changes). Its record
\* follows (BaseRecord). With a record whose base is missing or rewritten,
\* prepare_base refuses instead of replacing it (DecideCore's first branch);
\* base_replaced_live replaces it.
StartBase(i) ==
    /\ cap.st = "idle" /\ Grouped /\ BaseNeeded
    /\ IF \E b \in Ids : meta[b] = BaseMeta
       THEN LET b == CHOOSE b \in Ids : meta[b] = BaseMeta
            IN IF corpus[b] = "replaced"
               THEN UNCHANGED <<meta, corpus, sidecar, cap>>
               ELSE /\ corpus' = [corpus EXCEPT ![b] = "ok"]
                    /\ cap' = [Idle EXCEPT !.st = "baserec", !.item = i, !.b = b]
                    /\ UNCHANGED <<meta, sidecar>>
       ELSE /\ meta' = Append(meta, BaseMeta)
            /\ corpus' = Append(corpus, "ok")
            /\ sidecar' = Append(sidecar, NoSidecar)
            /\ cap' = [Idle EXCEPT !.st = "baserec", !.item = i,
                                   !.b = Len(meta) + 1]
    /\ UNCHANGED <<src, manifest, rec, baseRec, crashes, damage>>

\* prepare_base's write of shared-{group}.base; the pass goes on.
BaseRecord ==
    /\ cap.st = "baserec"
    /\ baseRec' = cap.b
    /\ cap' = [cap EXCEPT !.st = "decide", !.b = 0]
    /\ UNCHANGED <<src, meta, corpus, sidecar, manifest, rec, crashes, damage>>

\* capture_item: decide, then export into a STATE attempt (or reuse the
\* retained capture, or refuse by name). The export's name is its content:
\* an existing name is the same bundle.
Capture(i) ==
    /\ \/ cap.st = "idle" /\ ~(Grouped /\ BaseNeeded)
       \/ cap.st = "decide" /\ cap.item = i
    /\ LET o == CaptureOutcome(i)
       IN IF o.d = "Export"
          THEN LET m == NewMeta(i, o)
                   plan == NewPlan(i, o)
               IN IF \E b \in Ids : meta[b] = m
                  THEN /\ cap' = [st |-> "publish", item |-> i,
                                  b |-> CHOOSE b \in Ids : meta[b] = m,
                                  plan |-> plan]
                       /\ UNCHANGED <<meta, corpus, sidecar>>
                  ELSE /\ meta' = Append(meta, m)
                       /\ corpus' = Append(corpus, "staged")
                       /\ sidecar' = Append(sidecar, NoSidecar)
                       /\ cap' = [st |-> "publish", item |-> i,
                                  b |-> Len(meta) + 1, plan |-> plan]
          ELSE /\ cap' = Idle
               /\ UNCHANGED <<meta, corpus, sidecar>>
    /\ UNCHANGED <<src, manifest, rec, baseRec, crashes, damage>>

\* The step after a bundle's dependency sidecars: its reuse manifest (lane
\* L7), then its record. reuse_after_record writes the record first.
BeforeRecord ==
    IF ReuseManifest /\ Mutation # "reuse_after_record" THEN "reuse"
    ELSE "record"

\* publish_bundle: hard link into CORPUS under the content name, sealed. A
\* file already there at the same digest is reused; one rewritten in place
\* refuses DIGEST_MISMATCH and the pass ends with no record.
Publish ==
    /\ cap.st = "publish"
    /\ IF corpus[cap.b] = "replaced"
       THEN /\ cap' = Idle
            /\ UNCHANGED corpus
       ELSE /\ corpus' = [corpus EXCEPT ![cap.b] = "ok"]
            /\ cap' = [cap EXCEPT !.st =
                         IF DeclaresPrereqs(cap.b)
                            /\ Mutation # "sidecar_after_record"
                         THEN "sidecars" ELSE BeforeRecord]
    /\ UNCHANGED <<src, meta, sidecar, manifest, rec, baseRec, crashes, damage>>

\* {bundle}.base and publish_prior's {bundle}.prior, before the record. An
\* intact chain already recorded for this name stands (identical bytes
\* declare identical prerequisites); otherwise the pass's own link is
\* written. A pass that does not chain writes no .prior.
Sidecars ==
    /\ cap.st = "sidecars"
    /\ LET b == cap.b
           keep == \/ cap.plan.prior = 0
                   \/ HasPrior(b) /\ Walk(b, TRUE).code = "ok"
       IN sidecar' = [sidecar EXCEPT ![b] =
                        IF keep THEN [@ EXCEPT !.base = cap.plan.base]
                        ELSE cap.plan]
    /\ cap' = IF Mutation = "sidecar_after_record" THEN Idle
              ELSE [cap EXCEPT !.st = BeforeRecord]
    /\ UNCHANGED <<src, meta, corpus, manifest, rec, baseRec, crashes, damage>>

\* publish_reuse (lane L7): the bundle's {bundle}.reuse manifest, bound to
\* it by digest, before the record. A re-export onto a name that has one
\* writes the same list again.
ReuseSidecar ==
    /\ cap.st = "reuse"
    /\ manifest' = manifest \cup {cap.b}
    /\ cap' = IF Mutation = "reuse_after_record" THEN Idle
              ELSE [cap EXCEPT !.st = "record"]
    /\ UNCHANGED <<src, meta, corpus, sidecar, rec, baseRec, crashes, damage>>

\* The {item}.capture record names the new bundle.
Record ==
    /\ cap.st = "record"
    /\ rec' = [rec EXCEPT ![cap.item] = cap.b]
    /\ cap' = IF Mutation = "sidecar_after_record" /\ DeclaresPrereqs(cap.b)
              THEN [cap EXCEPT !.st = "sidecars"]
              ELSE IF Mutation = "reuse_after_record" /\ ReuseManifest
              THEN [cap EXCEPT !.st = "reuse"]
              ELSE Idle
    /\ UNCHANGED <<src, meta, corpus, sidecar, manifest, baseRec, crashes,
                   damage>>

\* Q46 GC under estate.lock: remove a CORPUS bundle (and its sidecars) that
\* no record and no base record depends on, one per step. gc_deletes_
\* depended keeps only what the records name directly, forgetting their
\* chain links.
GCLive ==
    IF Mutation = "gc_deletes_depended"
    THEN {rec[i] : i \in Items} \cup {baseRec}
    ELSE Depended \cup Closure(baseRec)

Garbage == {b \in Ids : Present(b) /\ b \notin GCLive}

GC ==
    /\ GCOn /\ cap.st = "idle"
    /\ \E b \in Garbage :
          /\ corpus' = [corpus EXCEPT ![b] = "collected"]
          /\ sidecar' = [sidecar EXCEPT ![b] = NoSidecar]
          /\ manifest' = manifest \ {b}
    /\ UNCHANGED <<src, meta, rec, baseRec, cap, crashes, damage>>

(* Environment *)

\* The source tip moves (a commit, a ref update).
Advance ==
    /\ src.ver < MaxCommits
    /\ src' = [src EXCEPT !.ver = @ + 1]
    /\ UNCHANGED <<meta, corpus, sidecar, manifest, rec, baseRec, cap, crashes,
                   damage>>

\* The source history is rewritten and pruned: no retained tip is held.
Rewrite ==
    /\ src.gen < MaxRewrites
    /\ src' = [src EXCEPT !.gen = @ + 1]
    /\ UNCHANGED <<meta, corpus, sidecar, manifest, rec, baseRec, cap, crashes,
                   damage>>

\* A third party deletes a CORPUS bundle or rewrites it in place.
Damage ==
    /\ damage < MaxDamage
    /\ \E b \in Ids :
          /\ corpus[b] = "ok"
          \* An IF, not a disjunction: TLC branches on a disjunction inside an
          \* action and would count each successor twice (README, "GitCarry").
          /\ IF DamageBase THEN TRUE ELSE meta[b].kind = "capture"
          /\ \E how \in IF DamageRewrites THEN {"missing", "replaced"}
                        ELSE {"missing"} :
                corpus' = [corpus EXCEPT ![b] = how]
    /\ damage' = damage + 1
    /\ UNCHANGED <<src, meta, sidecar, manifest, rec, baseRec, cap, crashes>>

\* The capture host crashes inside a pass: every durable step stands, the
\* STATE attempt is left behind, the pass is lost.
Crash ==
    /\ crashes < MaxCrashes /\ cap.st # "idle"
    /\ cap' = Idle
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<src, meta, corpus, sidecar, manifest, rec, baseRec, damage>>

Init ==
    /\ src = [ver |-> 0, gen |-> 0]
    /\ meta = <<>>
    /\ corpus = <<>>
    /\ sidecar = <<>>
    /\ manifest = {}
    /\ rec = [i \in Items |-> 0]
    /\ baseRec = 0
    /\ cap = Idle
    /\ crashes = 0
    /\ damage = 0

Protocol ==
    \/ \E i \in Items : StartBase(i)
    \/ BaseRecord
    \/ \E i \in Items : Capture(i)
    \/ Publish \/ Sidecars \/ ReuseSidecar \/ Record \/ GC

Environment == Advance \/ Rewrite \/ Damage \/ Crash

Next == Protocol \/ Environment

Spec == Init /\ [][Next]_vars

\* Fair protocol, unfair environment and faults.
LiveSpec == Spec /\ WF_vars(Protocol)

-----------------------------------------------------------------------------
(* PROPERTIES                                                               *)

TipRec == [ver : 0..MaxCommits, gen : 0..MaxRewrites]

MetaRec == [kind : {"base", "capture"}, item : Items, ver : 0..MaxCommits,
            gen : 0..MaxRewrites, basis : Bases, pre : SUBSET TipRec]

SidecarRec == [prior : Nat, pdepth : Nat, base : Nat, age : Nat]

OutcomeRec == [d : Decisions \cup {"Io"}, basis : Bases \cup {"none"},
               depth : Nat, rebase : Rebases \cup {"none"},
               reuse : Reuses \cup {"none"}, refusal : Refusals \cup {"none"}]

\* Also evaluates the decision core and the restore on every state, so a
\* partial or ill-typed definition is a TLC error, not a silent gap.
TypeOK ==
    /\ src \in TipRec
    /\ meta \in Seq(MetaRec)
    /\ Len(corpus) = Len(meta) /\ Len(sidecar) = Len(meta)
    /\ \A b \in Ids :
          /\ corpus[b] \in {"staged", "ok", "missing", "replaced", "collected"}
          /\ sidecar[b] \in SidecarRec
          /\ Link(b) \in 0..Len(meta) /\ Bound(b) \in 0..Len(meta)
          /\ Cardinality(meta[b].pre) <= 1
    \* One bundle per CORPUS name.
    /\ \A b, c \in Ids : meta[b] = meta[c] => b = c
    /\ rec \in [Items -> 0..Len(meta)]
    /\ baseRec \in 0..Len(meta)
    /\ manifest \subseteq Ids
    /\ cap.st \in {"idle", "baserec", "decide", "publish", "sidecars", "reuse",
                  "record"}
    /\ cap.item \in Items /\ cap.b \in 0..Len(meta) /\ cap.plan \in SidecarRec
    /\ crashes \in 0..MaxCrashes /\ damage \in 0..MaxDamage
    /\ \A i \in Items :
          /\ CaptureOutcome(i) \in OutcomeRec
          /\ ApplyOutcome(i) \in {"restored", "refused", "io"}

\* Every bundle's chain is at most DepthLimit links deep (a restore stages
\* at most DepthLimit + 1 bundles); under Q46 its root is younger than the
\* window, so a full re-pack happens at most once per window.
ChainDepthBounded ==
    \A b \in Ids :
        /\ Depth(b) <= DepthLimit
        /\ RootWindow > 0 => sidecar[b].age < RootWindow

\* A restore whose digests check never fails verify_bundle: every bundle it
\* applies, the oldest link's base first (fix 2), declares only prerequisite
\* tips of intact bundles applied before it. The header's tips and the
\* sidecars' chain are written separately, so this is the claim that the
\* capture side keeps them in step.
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
        /\ DeclaresPrereqs(rec[i]) => HasSidecars(rec[i])

\* Lane L7: a record names a bundle whose reuse manifest exists, so the pass
\* after any crash reuses that capture's blobs from its manifest and never
\* fetches the bundle to learn what it holds (P69). A restore never reads
\* the manifest, so either order restores; this is the order's own claim.
ReuseManifestBeforeRecord ==
    ReuseManifest => \A i \in Items : rec[i] # 0 => rec[i] \in manifest

\* Every record restores; or its item's next capture recaptures (exports a
\* new record) or refuses by name, keeping the missing custody visible. An
\* export whose content name holds rewritten bytes ends in publish_bundle's
\* DIGEST_MISMATCH, a refusal by name, so it counts here too; that it can
\* repeat on every pass is ChainRecovery's to show (MC_gc_live_rewritten).
\* An apply that does not restore is a typed refusal, never a bare IO.
RestoreOrRecapture ==
    \A i \in Items : rec[i] # 0 =>
        /\ ApplyOutcome(i) # "io"
        /\ Restorable(i) \/ CaptureOutcome(i).d \in {"Export", "Refuse"}

\* Wall-clock budget, evaluated on every state (README, "The budget is
\* state-level"): the conjunct over src makes it state-level.
WithinBudget == src.ver \in 0..MaxCommits => TLCGet("duration") < BudgetSeconds

\* REACHED witnesses (expected violated): the bound explores the state.
\* A capture chained on a link of a re-rooted chain: a link whose root age
\* exceeds its depth.
Witness_RerootExtended ==
    ~\E b \in Ids : HasPrior(b) /\ sidecar[Link(b)].age > Depth(Link(b))

\* A chain re-rooted twice: its age exceeds its depth by two depth limits.
Witness_SecondReroot ==
    ~\E b \in Ids : HasPrior(b) /\ sidecar[b].age >= Depth(b) + 2 * DepthLimit

\* Fix 2 over several links: a record whose chain has two links before its
\* head, the oldest bound to a plan base, restores (flatten imports the base,
\* then applies both links).
Witness_BasedChainRestored ==
    ~\E i \in Items :
        /\ rec[i] # 0 /\ HasPrior(rec[i]) /\ Restorable(i)
        /\ LET links == Walk(rec[i], FALSE).links
           IN Len(links) >= 2 /\ Bound(links[1]) # 0

\* Liveness: under a fair protocol, once the environment stops, every item
\* whose record does not restore (or has none) gets one that does.
ChainRecovery == \A i \in Items : ~Restorable(i) ~> Restorable(i)

=============================================================================
