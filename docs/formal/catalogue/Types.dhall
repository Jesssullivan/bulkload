{- Types of docs/formal's typed catalogue (OI-1003-Q32, OI-1003-Q43).

   Catalogue.dhall uses these to describe every TLC config of each module
   (BulkloadTransfer.tla, and GitCarry.tla through GitCarry.dhall) and the
   traceability row of every model property. `just tla-render` renders each
   module's run order (configs.tsv, configs_gc.tsv) and every MC_*.cfg from
   it, and `just tla-check` refuses to run TLC unless the committed files
   equal that rendering and every name below is grounded in its module
   (README.md, "Hybrid roles").

   Union labels are the TLA+ names themselves: `showConstructor` turns a
   label into the name TLC reads, so the catalogue cannot misspell one, and
   tla-check's grounding step finds every label in the .tla. The names are
   frozen (README.md, "Frozen names"); do not rename a label.
-}
let Seat = < a | b >

let SupersedeMode = < off | check_rename | exchange >

{- Every deliberate rule break the spec's `Mutations` set knows, except
   "none" (Constants.Mutation is Optional); tla-check's grounding step
   requires the two sets to be equal. Catalogue.dhall maps each label to the
   property it must violate and to its primary MC_neg_ row, each with a total
   `merge`, so a label added here without a verdict or without a row is a
   type error.
-}
let Mutation =
      < held_before_commit
      | commit_before_fsync
      | commit_before_dirseal
      | adopt_without_seal
      | ledger_before_held
      | done_before_sync
      | reread_durable
      | reread_ignore_ledger
      | skip_output_row
      | double_read
      | src_ledger_carries_r25
      | record_racy
      | untyped_space
      | source_write
      | pause_writer
      | git_optional_locks
      | unbounded_backup
      | supersede_unchecked
      | sweep_displaced
      | adopt_unkeyed
      | adopt_unverified
      >

{- Model properties a config can name: the frozen safety invariants, the
   wall-clock budget, the strict R25 reading and the temporal properties.
   The reachability witnesses are a separate type (Witness), so a fail row
   can never name one and a reach row can name nothing else.
-}
let Property =
      < TypeOK
      | R25_NoDurableReread
      | R25_NoCommittedCaptureReread
      | ReadOnce
      | S3_ReadsOnlyChanged
      | S3_UnchangedReadsZero
      | S3_ClosedPassIsHeld
      | RecordImpliesBytes
      | HeldAfterCommit
      | LedgerAfterHeld
      | DoneAfterLedger
      | ReuseSound
      | LedgerSound
      | NoClobber
      | S2_TypedSourceAccess
      | S2_BackupLockBounded
      | ClosureAccounted
      | WithinBudget
      | R25_StrictNoDurableReread
      | RunsClose
      | AllRunsFinish
      >

let Witness =
      < Witness_LedgerManifest | Witness_LedgerChunkRead | Witness_LostRowRead >

-- The 37 actions of Next, in the sorted order of configs.tsv's never column.
let Action =
      < AnswerHeld
      | BackupBegin
      | BackupEnd
      | BackupStepLock
      | BackupStepUnlock
      | CheckOwn
      | Commit
      | CommitFail
      | CrashBoth
      | CrashDst
      | CrashSrc
      | DirSeal
      | Edit
      | Exchange
      | Finish
      | ForeignDelete
      | ForeignWrite
      | GitRead
      | LedgerCommit
      | Publish
      | RecvDecide
      | RecvEnd
      | RecvEntry
      | RecvHeld
      | RecvManifest
      | RecvNeed
      | RecvRefused
      | RenameReplace
      | SealAdopted
      | SealTemp
      | SendSourceDone
      | SilentRewrite
      | StartRun
      | Terminated
      | Tick
      | VerifyDisp
      | Walk
      >

let Specification = < Spec | LiveSpec >

-- One value per CONSTANT of BulkloadTransfer.tla, rendered in this order.
let Constants =
      { Seats : List Seat
      , MaxRuns : Natural
      , MaxCrashes : Natural
      , MaxEdits : Natural
      , MaxForeign : Natural
      , MaxCommitFails : Natural
      , SpaceRefusals : Bool
      , RelaxedSourceLedger : Bool
      , RelaxedAuthority : Bool
      , SupersedeMode : SupersedeMode
      , EstateReads : Bool
      , MaxBackupSteps : Natural
      , Mutation : Optional Mutation
      , BudgetSeconds : Natural
      , StoreRootSealed : Bool
      , TrackStrictHeld : Bool
      , AdoptUnrowed : Bool
      }

{- What a row expects, and only what that expectation needs:
   - pass: the safety invariants and temporal properties it checks, and
     the exact actions its coverage must show never enabled;
   - fail: the one property it must violate (TypeOK rides along);
   - reach: the one witness it must violate (TypeOK rides along);
   - simulate: the invariants a seeded simulation checks;
   - inconclusive: the budget self-test (TypeOK and the budget only).
-}
let Pass =
      { invariants : List Property
      , properties : List Property
      , never : List Action
      }

let Expect =
      < pass : Pass
      | fail : Property
      | reach : Witness
      | simulate : List Property
      | inconclusive
      >

let Row =
      { name : Text
      , expect : Expect
      , comment : List Text
      , constants : Constants
      , spec : Specification
      , symmetry : Bool
      , flags : Optional Text
      }

{- A mutation config. Its row is MC_neg_<mutation>, and it must violate the
   mutation's verdict, unless `also` names another property the same
   mutation must violate (a second config, MC_neg_<suffix>).
-}
let Against = < verdict | also : { suffix : Text, property : Property } >

let NegRow =
      { mutation : Mutation
      , against : Against
      , constants : Constants
      , comment : { head : Text, tail : List Text }
      }

{- Traceability of a model property: the SLOs it serves (docs/slo.md; plus
   AGENTS.md's durability rule, which is not one of S1-S5), the rulings it
   carries, the code symbols it is about (each must be found by `git grep -w`
   under crates/) and the property tests that check the same claim on the
   real code (docs/plans/2026-10-03-property-test-plan.md).
-}
let Slo = < S1 | S2 | S3 | S4 | S5 | Durability >

let InvariantRow =
      { tla : Property
      , slo : List Slo
      , ruling : List Text
      , codeSymbol : List Text
      , ptest : List Text
      }

{- How a property is used:
   - safety: a frozen safety invariant (README.md, "Frozen names"); TypeOK
     is the sanity check. Every pass row checks all of them, and every one
     but TypeOK must have a fail row and a traceability row.
   - budget: the wall-clock bound (WithinBudget), checked by every config.
   - finding: R25's strict reading, beside the safety invariants: checked
     by its finding row (the code before #169) and by the pass rows that
     model #169's capture-record adoption.
   - temporal: a temporal property, checked under PROPERTY.
-}
let PropertyClass = < safety | budget | finding | temporal >

{- The property table: one field per Property label, holding that label's
   value, its position (for equality; Dhall has none on unions) and its
   class. `propertyIndex` and `propertyClass` merge it over Property, so it
   must have exactly one field per label: a label without a field is a
   "Missing handler" error, a field without a label an "Unused handler"
   error. Catalogue.dhall lists every property from it (`toMap`) and asserts
   that each field holds its own label and that the positions run 0, 1, 2,
   ... with no gap, so no list of the union is written by hand.
-}
let PropertyEntry =
      { property : Property, index : Natural, class : PropertyClass }

let propertyTable =
      { TypeOK =
        { property = Property.TypeOK
        , index = 0
        , class = PropertyClass.safety
        }
      , R25_NoDurableReread =
        { property = Property.R25_NoDurableReread
        , index = 1
        , class = PropertyClass.safety
        }
      , R25_NoCommittedCaptureReread =
        { property = Property.R25_NoCommittedCaptureReread
        , index = 2
        , class = PropertyClass.safety
        }
      , ReadOnce =
        { property = Property.ReadOnce
        , index = 3
        , class = PropertyClass.safety
        }
      , S3_ReadsOnlyChanged =
        { property = Property.S3_ReadsOnlyChanged
        , index = 4
        , class = PropertyClass.safety
        }
      , S3_UnchangedReadsZero =
        { property = Property.S3_UnchangedReadsZero
        , index = 5
        , class = PropertyClass.safety
        }
      , S3_ClosedPassIsHeld =
        { property = Property.S3_ClosedPassIsHeld
        , index = 6
        , class = PropertyClass.safety
        }
      , RecordImpliesBytes =
        { property = Property.RecordImpliesBytes
        , index = 7
        , class = PropertyClass.safety
        }
      , HeldAfterCommit =
        { property = Property.HeldAfterCommit
        , index = 8
        , class = PropertyClass.safety
        }
      , LedgerAfterHeld =
        { property = Property.LedgerAfterHeld
        , index = 9
        , class = PropertyClass.safety
        }
      , DoneAfterLedger =
        { property = Property.DoneAfterLedger
        , index = 10
        , class = PropertyClass.safety
        }
      , ReuseSound =
        { property = Property.ReuseSound
        , index = 11
        , class = PropertyClass.safety
        }
      , LedgerSound =
        { property = Property.LedgerSound
        , index = 12
        , class = PropertyClass.safety
        }
      , NoClobber =
        { property = Property.NoClobber
        , index = 13
        , class = PropertyClass.safety
        }
      , S2_TypedSourceAccess =
        { property = Property.S2_TypedSourceAccess
        , index = 14
        , class = PropertyClass.safety
        }
      , S2_BackupLockBounded =
        { property = Property.S2_BackupLockBounded
        , index = 15
        , class = PropertyClass.safety
        }
      , ClosureAccounted =
        { property = Property.ClosureAccounted
        , index = 16
        , class = PropertyClass.safety
        }
      , WithinBudget =
        { property = Property.WithinBudget
        , index = 17
        , class = PropertyClass.budget
        }
      , R25_StrictNoDurableReread =
        { property = Property.R25_StrictNoDurableReread
        , index = 18
        , class = PropertyClass.finding
        }
      , RunsClose =
        { property = Property.RunsClose
        , index = 19
        , class = PropertyClass.temporal
        }
      , AllRunsFinish =
        { property = Property.AllRunsFinish
        , index = 20
        , class = PropertyClass.temporal
        }
      }

let propertyIndex = \(p : Property) -> (merge propertyTable p).index

let propertyClass = \(p : Property) -> (merge propertyTable p).class

let isSafety =
      \(p : Property) ->
        merge
          { safety = True, budget = False, finding = False, temporal = False }
          (propertyClass p)

let isTemporal =
      \(p : Property) ->
        merge
          { safety = False, budget = False, finding = False, temporal = True }
          (propertyClass p)

{- A position per mutation: the run order of the primary MC_neg_ rows.
   Catalogue.dhall asserts that the primary rows' positions run 0, 1, 2, ...
   with one row per label, however many labels the union has.
-}
let mutationIndex =
      \(m : Mutation) ->
        merge
          { held_before_commit = 0
          , commit_before_fsync = 1
          , commit_before_dirseal = 2
          , adopt_without_seal = 3
          , ledger_before_held = 4
          , done_before_sync = 5
          , reread_durable = 6
          , reread_ignore_ledger = 7
          , skip_output_row = 8
          , double_read = 9
          , src_ledger_carries_r25 = 10
          , record_racy = 11
          , untyped_space = 12
          , source_write = 13
          , pause_writer = 14
          , git_optional_locks = 15
          , unbounded_backup = 16
          , supersede_unchecked = 17
          , sweep_displaced = 18
          , adopt_unkeyed = 19
          , adopt_unverified = 20
          }
          m

{- The action table: one field per Action label, its value and its place in
   the sorted never column. Total, like the property table.
-}
let ActionEntry = { action : Action, index : Natural }

let actionTable =
      { AnswerHeld = { action = Action.AnswerHeld, index = 0 }
      , BackupBegin = { action = Action.BackupBegin, index = 1 }
      , BackupEnd = { action = Action.BackupEnd, index = 2 }
      , BackupStepLock = { action = Action.BackupStepLock, index = 3 }
      , BackupStepUnlock = { action = Action.BackupStepUnlock, index = 4 }
      , CheckOwn = { action = Action.CheckOwn, index = 5 }
      , Commit = { action = Action.Commit, index = 6 }
      , CommitFail = { action = Action.CommitFail, index = 7 }
      , CrashBoth = { action = Action.CrashBoth, index = 8 }
      , CrashDst = { action = Action.CrashDst, index = 9 }
      , CrashSrc = { action = Action.CrashSrc, index = 10 }
      , DirSeal = { action = Action.DirSeal, index = 11 }
      , Edit = { action = Action.Edit, index = 12 }
      , Exchange = { action = Action.Exchange, index = 13 }
      , Finish = { action = Action.Finish, index = 14 }
      , ForeignDelete = { action = Action.ForeignDelete, index = 15 }
      , ForeignWrite = { action = Action.ForeignWrite, index = 16 }
      , GitRead = { action = Action.GitRead, index = 17 }
      , LedgerCommit = { action = Action.LedgerCommit, index = 18 }
      , Publish = { action = Action.Publish, index = 19 }
      , RecvDecide = { action = Action.RecvDecide, index = 20 }
      , RecvEnd = { action = Action.RecvEnd, index = 21 }
      , RecvEntry = { action = Action.RecvEntry, index = 22 }
      , RecvHeld = { action = Action.RecvHeld, index = 23 }
      , RecvManifest = { action = Action.RecvManifest, index = 24 }
      , RecvNeed = { action = Action.RecvNeed, index = 25 }
      , RecvRefused = { action = Action.RecvRefused, index = 26 }
      , RenameReplace = { action = Action.RenameReplace, index = 27 }
      , SealAdopted = { action = Action.SealAdopted, index = 28 }
      , SealTemp = { action = Action.SealTemp, index = 29 }
      , SendSourceDone = { action = Action.SendSourceDone, index = 30 }
      , SilentRewrite = { action = Action.SilentRewrite, index = 31 }
      , StartRun = { action = Action.StartRun, index = 32 }
      , Terminated = { action = Action.Terminated, index = 33 }
      , Tick = { action = Action.Tick, index = 34 }
      , VerifyDisp = { action = Action.VerifyDisp, index = 35 }
      , Walk = { action = Action.Walk, index = 36 }
      }

let actionIndex = \(a : Action) -> (merge actionTable a).index

-- The witness table: one field per Witness label, its value. Total.
let witnessTable =
      { Witness_LedgerManifest = Witness.Witness_LedgerManifest
      , Witness_LedgerChunkRead = Witness.Witness_LedgerChunkRead
      , Witness_LostRowRead = Witness.Witness_LostRowRead
      }

let witnessSelf = \(w : Witness) -> merge witnessTable w

{- The modules (specs) the catalogue holds (OI-1003-Q43). Each renders its
   own run order and configs, and tla-check runs each config against its own
   module, with its own budget self-test first. The table is merged over the
   union, so a module added without an entry does not type-check.
-}
let Module = < BulkloadTransfer | GitCarry >

{- How tla-check grounds a module's code symbols. Either way only a Rust
   source under crates/ outside a tests/ directory counts, never a data
   file such as decide_rows.tsv:
   - definition: an item definition, `fn`, `const`, `static`, `struct`,
     `enum`, `trait`, `type` or `mod` followed by the symbol;
   - code: the symbol as a whole word on a line that is not a comment.
     BulkloadTransfer's symbols include enum variants, fields and
     parameters, which have no item definition of their own.
-}
let SymbolMatch = < definition | code >

let ModuleEntry =
      { module : Module
      , index : Natural
      , spec : Text
      , tsv : Text
      , symbols : SymbolMatch
      }

let moduleTable =
      { BulkloadTransfer =
        { module = Module.BulkloadTransfer
        , index = 0
        , spec = "BulkloadTransfer.tla"
        , tsv = "configs.tsv"
        , symbols = SymbolMatch.code
        }
      , GitCarry =
        { module = Module.GitCarry
        , index = 1
        , spec = "GitCarry.tla"
        , tsv = "configs_gc.tsv"
        , symbols = SymbolMatch.definition
        }
      }

let moduleEntry = \(m : Module) -> merge moduleTable m

-- GitCarry.tla (Q42 lane L4) ---------------------------------------------------

-- Plan items: model values, as Seat is for BulkloadTransfer.
let Item = < i1 | i2 >

{- Every rule break GitCarry.tla's `Mutations` set knows, except "none". As
   for Mutation, GitCarry.dhall maps each label to its verdict and to its
   primary MC_gc_neg_ row with total `merge`s.
-}
let GcMutation =
      < chain_ignores_depth
      | gc_deletes_depended
      | base_replaced_live
      | sidecar_after_record
      | skip_flatten_verify
      | hit_ignores_chain
      | reroot_pre_mismatch
      >

-- GitCarry.tla's properties: its safety invariants, its budget and ChainRecovery.
let GcProperty =
      < TypeOK
      | ChainDepthBounded
      | PrereqsSatisfiedByEarlierLinks
      | BrokenLinkNeverReuseHit
      | BaseNotReplacedWhileDepended
      | GCNeverDeletesDepended
      | SidecarsBeforeRecord
      | RestoreOrRecapture
      | WithinBudget
      | ChainRecovery
      >

{- GitCarry.tla's reachability witnesses: each holds until the bound explores
   the state it names, so a reach row's REACHED shows that state is reached
   (OI-1003-Q46: a chain extended past a re-root, and a chain re-rooted
   twice; fix 2: a restore of two links under a plan base).
-}
let GcWitness =
      < Witness_RerootExtended
      | Witness_SecondReroot
      | Witness_BasedChainRestored
      >

-- The witness table: one field per GcWitness label, its value. Total.
let gcWitnessTable =
      { Witness_RerootExtended = GcWitness.Witness_RerootExtended
      , Witness_SecondReroot = GcWitness.Witness_SecondReroot
      , Witness_BasedChainRestored = GcWitness.Witness_BasedChainRestored
      }

let gcWitnessSelf = \(w : GcWitness) -> merge gcWitnessTable w

-- The 11 actions of GitCarry.tla's Next, in the sorted order of the never column.
let GcAction =
      < Advance
      | BaseRecord
      | Capture
      | Crash
      | Damage
      | GC
      | Publish
      | Record
      | Rewrite
      | Sidecars
      | StartBase
      >

-- One value per CONSTANT of GitCarry.tla, rendered in this order.
let GcConstants =
      { Items : List Item
      , DepthLimit : Natural
      , RootWindow : Natural
      , ChainUnderBase : Bool
      , GCOn : Bool
      , MaxCommits : Natural
      , MaxRewrites : Natural
      , MaxCrashes : Natural
      , MaxDamage : Natural
      , DamageBase : Bool
      , DamageRewrites : Bool
      , BaseMissingTyped : Bool
      , Mutation : Optional GcMutation
      , BudgetSeconds : Natural
      }

let GcPass =
      { invariants : List GcProperty
      , properties : List GcProperty
      , never : List GcAction
      }

{- A GitCarry row's expectation: pass, fail on one property, reach one
   witness, or the budget self-test.
-}
let GcExpect =
      < pass : GcPass | fail : GcProperty | reach : GcWitness | inconclusive >

let GcRow =
      { name : Text
      , expect : GcExpect
      , comment : List Text
      , constants : GcConstants
      , spec : Specification
      , flags : Optional Text
      }

let GcAgainst =
      < verdict | also : { suffix : Text, property : GcProperty } >

let GcNegRow =
      { mutation : GcMutation
      , against : GcAgainst
      , constants : GcConstants
      , comment : { head : Text, tail : List Text }
      }

{- Typed property-test ids, on the GitCarry rows only (the plan's Q42
   allocation, P64-P71, and P42 REUSE-SAFETY). A P-id outside the union does
   not type-check.
-}
let PId = < P42 | P64 | P65 | P66 | P67 | P68 | P69 | P70 | P71 >

-- The lane that lands a symbol a GitCarry row cites before the code has it.
let Lane = < L6a | L6b | L7 | L8 >

let PendingSymbol = { symbol : Text, lands : Lane }

{- A GitCarry property's traceability: as InvariantRow, plus the symbols it
   is about that no code has yet (not grepped; tla-check prints them as
   pending), and typed P-ids.
-}
let GcInvariantRow =
      { tla : GcProperty
      , slo : List Slo
      , ruling : List Text
      , codeSymbol : List Text
      , pending : List PendingSymbol
      , ptest : List PId
      }

let GcPropertyEntry =
      { property : GcProperty, index : Natural, class : PropertyClass }

let gcPropertyTable =
      { TypeOK =
        { property = GcProperty.TypeOK, index = 0, class = PropertyClass.safety }
      , ChainDepthBounded =
        { property = GcProperty.ChainDepthBounded
        , index = 1
        , class = PropertyClass.safety
        }
      , PrereqsSatisfiedByEarlierLinks =
        { property = GcProperty.PrereqsSatisfiedByEarlierLinks
        , index = 2
        , class = PropertyClass.safety
        }
      , BrokenLinkNeverReuseHit =
        { property = GcProperty.BrokenLinkNeverReuseHit
        , index = 3
        , class = PropertyClass.safety
        }
      , BaseNotReplacedWhileDepended =
        { property = GcProperty.BaseNotReplacedWhileDepended
        , index = 4
        , class = PropertyClass.safety
        }
      , GCNeverDeletesDepended =
        { property = GcProperty.GCNeverDeletesDepended
        , index = 5
        , class = PropertyClass.safety
        }
      , SidecarsBeforeRecord =
        { property = GcProperty.SidecarsBeforeRecord
        , index = 6
        , class = PropertyClass.safety
        }
      , RestoreOrRecapture =
        { property = GcProperty.RestoreOrRecapture
        , index = 7
        , class = PropertyClass.safety
        }
      , WithinBudget =
        { property = GcProperty.WithinBudget
        , index = 8
        , class = PropertyClass.budget
        }
      , ChainRecovery =
        { property = GcProperty.ChainRecovery
        , index = 9
        , class = PropertyClass.temporal
        }
      }

let gcPropertyIndex = \(p : GcProperty) -> (merge gcPropertyTable p).index

let gcPropertyClass = \(p : GcProperty) -> (merge gcPropertyTable p).class

let gcIsSafety =
      \(p : GcProperty) ->
        merge
          { safety = True, budget = False, finding = False, temporal = False }
          (gcPropertyClass p)

let gcIsTemporal =
      \(p : GcProperty) ->
        merge
          { safety = False, budget = False, finding = False, temporal = True }
          (gcPropertyClass p)

let gcMutationIndex =
      \(m : GcMutation) ->
        merge
          { chain_ignores_depth = 0
          , gc_deletes_depended = 1
          , base_replaced_live = 2
          , sidecar_after_record = 3
          , skip_flatten_verify = 4
          , hit_ignores_chain = 5
          , reroot_pre_mismatch = 6
          }
          m

let GcActionEntry = { action : GcAction, index : Natural }

let gcActionTable =
      { Advance = { action = GcAction.Advance, index = 0 }
      , BaseRecord = { action = GcAction.BaseRecord, index = 1 }
      , Capture = { action = GcAction.Capture, index = 2 }
      , Crash = { action = GcAction.Crash, index = 3 }
      , Damage = { action = GcAction.Damage, index = 4 }
      , GC = { action = GcAction.GC, index = 5 }
      , Publish = { action = GcAction.Publish, index = 6 }
      , Record = { action = GcAction.Record, index = 7 }
      , Rewrite = { action = GcAction.Rewrite, index = 8 }
      , Sidecars = { action = GcAction.Sidecars, index = 9 }
      , StartBase = { action = GcAction.StartBase, index = 10 }
      }

let gcActionIndex = \(a : GcAction) -> (merge gcActionTable a).index

-- The decision core's closed unions (OI-1003-Q43) ------------------------------

{- What git carry's pure decision core, decide(Inputs) -> Decision, returns.
   The labels are the constructors of hs/GitCarryCore.hs (its `schema`) and
   the strings of GitCarry.tla's Decisions, Bases, Rebases, Reuses and
   Refusals sets; formal-nv and tla-check require all three to be equal, and
   decide_rows.tsv's output columns use them. The Rust decide.rs (lane L6)
   is to use the same labels.
   - Basis: what a capture's bundle depends on: nothing, a plan base, a
     chain link, or (L6b's fix 2) both.
   - Rebase: NewRoot when the depth limit (v1) or the Q46 window ends the
     chain and the capture re-packs a fresh root; Reroot when, under Q46, it
     chains on its chain's root instead.
   - ReuseEligibility: the retained capture's blobs offered for reuse.
   - Refusal: the typed refusal (R33) the core can return.
-}
let Basis = < SelfContained | Base | Chain | BaseAndChain >

let Rebase = < NoRebase | NewRoot | Reroot >

let ReuseEligibility = < NoRetained | BlobReuse | PassStartUnrecorded >

let Refusal = < ReceiptBindingInvalid >

let Plan =
      { basis : Basis
      , depth : Natural
      , rebase : Rebase
      , reuse : ReuseEligibility
      }

let Decision = < Hit | Export : Plan | Refuse : Refusal >

{- One field per label, holding that label's value. GitCarry.dhall lists
   each union from its table (toMap) and asserts every field holds its own
   label. Each table is also merged over its union below (basisSelf,
   rebaseSelf, reuseSelf, refusalSelf, decisionSelf), as witnessTable is:
   a label without a field is a "Missing handler" error, and a field
   without a label an "Unused handler" error (for Decision, a failed
   assert). So each table, and the label list GitCarry.dhall takes from
   it, holds exactly its union's labels.
-}
let basisTable =
      { SelfContained = Basis.SelfContained
      , Base = Basis.Base
      , Chain = Basis.Chain
      , BaseAndChain = Basis.BaseAndChain
      }

let rebaseTable =
      { NoRebase = Rebase.NoRebase
      , NewRoot = Rebase.NewRoot
      , Reroot = Rebase.Reroot
      }

let reuseTable =
      { NoRetained = ReuseEligibility.NoRetained
      , BlobReuse = ReuseEligibility.BlobReuse
      , PassStartUnrecorded = ReuseEligibility.PassStartUnrecorded
      }

let refusalTable = { ReceiptBindingInvalid = Refusal.ReceiptBindingInvalid }

let decisionTable =
      { Hit = Decision.Hit
      , Export =
          Decision.Export
            { basis = Basis.SelfContained
            , depth = 0
            , rebase = Rebase.NoRebase
            , reuse = ReuseEligibility.NoRetained
            }
      , Refuse = Decision.Refuse Refusal.ReceiptBindingInvalid
      }

let basisSelf = \(b : Basis) -> merge basisTable b

let rebaseSelf = \(r : Rebase) -> merge rebaseTable r

let reuseSelf = \(r : ReuseEligibility) -> merge reuseTable r

let refusalSelf = \(r : Refusal) -> merge refusalTable r

{- Decision's labels carry payloads, so its table cannot be the handler
   record itself: each handler returns that label's table entry, so a label
   added to Decision needs a handler, and its handler a table entry. A
   table field without a label fails GitCarry.dhall's assert that every
   field holds its own label.
-}
let decisionSelf =
      \(d : Decision) ->
        merge
          { Hit = decisionTable.Hit
          , Export = \(_ : Plan) -> decisionTable.Export
          , Refuse = \(_ : Refusal) -> decisionTable.Refuse
          }
          d

in  { Seat
    , SupersedeMode
    , Mutation
    , Property
    , Witness
    , Action
    , Specification
    , Constants
    , Pass
    , Expect
    , Row
    , Against
    , NegRow
    , Slo
    , InvariantRow
    , PropertyClass
    , PropertyEntry
    , propertyTable
    , propertyIndex
    , propertyClass
    , isSafety
    , isTemporal
    , mutationIndex
    , ActionEntry
    , actionTable
    , actionIndex
    , witnessTable
    , witnessSelf
    , Module
    , SymbolMatch
    , ModuleEntry
    , moduleTable
    , moduleEntry
    , Item
    , GcMutation
    , GcWitness
    , gcWitnessTable
    , gcWitnessSelf
    , GcProperty
    , GcAction
    , GcConstants
    , GcPass
    , GcExpect
    , GcRow
    , GcAgainst
    , GcNegRow
    , PId
    , Lane
    , PendingSymbol
    , GcInvariantRow
    , GcPropertyEntry
    , gcPropertyTable
    , gcPropertyIndex
    , gcPropertyClass
    , gcIsSafety
    , gcIsTemporal
    , gcMutationIndex
    , GcActionEntry
    , gcActionTable
    , gcActionIndex
    , Basis
    , Rebase
    , ReuseEligibility
    , Refusal
    , Plan
    , Decision
    , basisTable
    , rebaseTable
    , reuseTable
    , refusalTable
    , decisionTable
    , basisSelf
    , rebaseSelf
    , reuseSelf
    , refusalSelf
    , decisionSelf
    }
