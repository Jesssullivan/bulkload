{- EstateConverge.tla's part of docs/formal's typed catalogue (estate
   convergence, ruling OI-1003-Q144): every TLC config of the destination
   custody model, the verdict of every mutation, the traceability row of
   every property, and the labels of the module's closed unions.

   Catalogue.dhall imports this file and renders it beside BulkloadTransfer's
   and GitCarry's configs. Its run order is configs_ec.tsv, and its configs
   are MC_ec_*.cfg. The same checks hold as for GitCarry: every mutation has
   a verdict and one primary row (total merges), every safety invariant but
   TypeOK has a fail row, and every traced property has one traceability
   row, in table order. The module's types live here, not in Types.dhall:
   only its Module entry is shared (no symbol is pending: the Converge
   lane's symbols are on feat/estate-converge-20261008 since 2026-10-09).
-}
let T = ./Types.dhall

let Lib = ./Lib.dhall

let map = Lib.map

let concatMap = Lib.concatMap

let filter = Lib.filter

let any = Lib.any

let all = Lib.all

let natEq = Lib.natEq

let isEmpty = Lib.isEmpty

let join = Lib.join

let unlines = Lib.unlines

let showBool = Lib.showBool

let range = Lib.range

let ordered = Lib.ordered

let S = T.Slo

let Lane = T.Lane

let provenance =
      { cfg =
          "\\* Rendered from catalogue/EstateConverge.dhall; expected outcome in configs_ec.tsv."
      , tsv =
          "# Rendered from catalogue/EstateConverge.dhall by `just tla-render`; edit the catalogue, not this file."
      }

-- The model's values ------------------------------------------------------------

-- Plan items: the main checkout and its linked worktree.
let Item = < main | wt >

{- Refs: Dhall labels, each rendered as its git ref name (a name with a
   slash has an underscore label). The union is closed: a label without a
   name is a "Missing handler" error.
-}
let Ref = < main | dev | foo | foo_bar | o_x | o_x_y | t >

let showRef =
      \(r : Ref) ->
        merge
          { main = "main"
          , dev = "dev"
          , foo = "foo"
          , foo_bar = "foo/bar"
          , o_x = "o/x"
          , o_x_y = "o/x/y"
          , t = "t"
          }
          r

let Path = < d | d_f >

let showPath = \(p : Path) -> merge { d = "d", d_f = "d/f" } p

{- The closed unions EstateConverge.tla's sets hold (tla-check requires
   each set equal to its table's labels): the source changes, the
   operator edits and the refusal codes the model names.
-}
let SrcKind =
      < move
      | create
      | delete
      | rename
      | switch
      | stash
      | seat
      | dirfile
      | filedir
      | index
      | worktree
      | race
      | stashrace
      | carried
      >

let srcKindTable =
      { move = SrcKind.move
      , create = SrcKind.create
      , delete = SrcKind.delete
      , rename = SrcKind.rename
      , switch = SrcKind.switch
      , stash = SrcKind.stash
      , seat = SrcKind.seat
      , dirfile = SrcKind.dirfile
      , filedir = SrcKind.filedir
      , index = SrcKind.index
      , worktree = SrcKind.worktree
      , race = SrcKind.race
      , stashrace = SrcKind.stashrace
      , carried = SrcKind.carried
      }

let OpKind =
      < seat
      | racy
      | touch
      | untracked
      | index
      | ref
      | stash
      | stashdrop
      | head
      | restore
      | rebase
      | dir
      | lock
      | wtwork
      | block
      >

let opKindTable =
      { seat = OpKind.seat
      , racy = OpKind.racy
      , touch = OpKind.touch
      , untracked = OpKind.untracked
      , index = OpKind.index
      , ref = OpKind.ref
      , stash = OpKind.stash
      , stashdrop = OpKind.stashdrop
      , head = OpKind.head
      , restore = OpKind.restore
      , rebase = OpKind.rebase
      , dir = OpKind.dir
      , lock = OpKind.lock
      , wtwork = OpKind.wtwork
      , block = OpKind.block
      }

let Refusal = < MODIFIED | OCCUPIED | STALE | INTERRUPTED | UNTYPED | DIVERGED >

let refusalTable =
      { MODIFIED = Refusal.MODIFIED
      , OCCUPIED = Refusal.OCCUPIED
      , STALE = Refusal.STALE
      , INTERRUPTED = Refusal.INTERRUPTED
      , UNTYPED = Refusal.UNTYPED
      , DIVERGED = Refusal.DIVERGED
      }

-- Each table holds exactly its union's labels (a total merge).
let _ = \(k : SrcKind) -> merge srcKindTable k

let _ = \(k : OpKind) -> merge opKindTable k

let _ = \(r : Refusal) -> merge refusalTable r

{- Every rule break EstateConverge.tla's `Mutations` set knows, except
   "none". Each is mapped to its verdict and its primary MC_ec_neg_ row by
   total merges below.
-}
let Mutation =
      < proof_stat_only
      | proof_stat_strict
      | overwrite_modified
      | no_intent
      | exchange_unchecked
      | index_no_cas
      | stash_clear_blind
      | name_conflict_unchecked
      | single_ref_txn
      | head_branch_in_step
      | no_checkout_guard
      | converge_before_ref_settle
      | dir_replace_unchecked
      | prune_local
      | drop_provenance
      | noop_restages
      | sibling_intent_reproves
      | finish_after_all_deletes
      | rebase_unguarded
      | ledger_reads_index
      | unborn_head_untyped
      | seat_parent_untyped
      | refs_unsynced
      | probe_left
      | settle_moves_checked_out
      | txn_no_symref_verify
      | txn_no_cas
      | head_no_cas
      | settle_remove_dir
      | step_moves_intent_branch
      | settle_replays_branch
      | stale_lock_kept
      | set_head_one_txn
      | stash_settle_any_suffix
      | old_head_branch_skipped
      | no_landing_intent
      | head_not_recorded
      | head_attach_unchecked
      | intent_head_lock_kept
      | empty_lock_removed
      | packed_new_kept
      | stash_reflog_unsynced
      | recovery_overwrites
      | drift_key_reused
      | drift_applied
      | adopt_without_proof
      | adopt_ref_unequal
      | adopt_untyped
      | adopt_ledger_before_refs
      | adopt_r1_no_row
      | adopt_r2_not_adopted
      | adopt_no_recheck
      | adopt_t0_no_row
      | handoff_no_overlay
      | handoff_overlay_any_block
      | handoff_clear_unchecked
      | handoff_exchange_unchecked
      | handoff_no_intent
      | handoff_retake_unchecked
      | handoff_keep_racy_unchecked
      | handoff_strip_no_overlay
      | handoff_strip_unchecked
      >

let mutationIndex =
      \(m : Mutation) ->
        merge
          { proof_stat_only = 0
          , proof_stat_strict = 1
          , overwrite_modified = 2
          , no_intent = 3
          , exchange_unchecked = 4
          , index_no_cas = 5
          , stash_clear_blind = 6
          , name_conflict_unchecked = 7
          , single_ref_txn = 8
          , head_branch_in_step = 9
          , no_checkout_guard = 10
          , converge_before_ref_settle = 11
          , dir_replace_unchecked = 12
          , prune_local = 13
          , drop_provenance = 14
          , noop_restages = 15
          , sibling_intent_reproves = 16
          , finish_after_all_deletes = 17
          , rebase_unguarded = 18
          , ledger_reads_index = 19
          , unborn_head_untyped = 20
          , seat_parent_untyped = 21
          , refs_unsynced = 22
          , probe_left = 23
          , settle_moves_checked_out = 24
          , txn_no_symref_verify = 25
          , txn_no_cas = 26
          , head_no_cas = 27
          , settle_remove_dir = 28
          , step_moves_intent_branch = 29
          , settle_replays_branch = 30
          , stale_lock_kept = 31
          , set_head_one_txn = 32
          , stash_settle_any_suffix = 33
          , old_head_branch_skipped = 34
          , no_landing_intent = 35
          , head_not_recorded = 36
          , head_attach_unchecked = 37
          , intent_head_lock_kept = 38
          , empty_lock_removed = 39
          , packed_new_kept = 40
          , stash_reflog_unsynced = 41
          , recovery_overwrites = 42
          , drift_key_reused = 43
          , drift_applied = 44
          , adopt_without_proof = 45
          , adopt_ref_unequal = 46
          , adopt_untyped = 47
          , adopt_ledger_before_refs = 48
          , adopt_r1_no_row = 49
          , adopt_r2_not_adopted = 50
          , adopt_no_recheck = 51
          , adopt_t0_no_row = 52
          , handoff_no_overlay = 53
          , handoff_overlay_any_block = 54
          , handoff_clear_unchecked = 55
          , handoff_exchange_unchecked = 56
          , handoff_no_intent = 57
          , handoff_retake_unchecked = 58
          , handoff_keep_racy_unchecked = 59
          , handoff_strip_no_overlay = 60
          , handoff_strip_unchecked = 61
          }
          m

-- EstateConverge.tla's properties: its safety invariants, its budget and liveness.
let Property =
      < TypeOK
      | NoLocalWorkLost
      | NoSourceWorkLost
      | TypedRefusal
      | NoReRead
      | CrashAtomicity
      | OneCheckout
      | NoDriftedLanding
      | AdoptClaimsOnlyEqual
      | ForeignRefsUnowned
      | HandoffOwnsOnlyItsBlock
      | WithinBudget
      | ConvergesWhenQuiet
      | NoWedge
      >

let PropertyEntry = { property : Property, index : Natural, class : T.PropertyClass }

let propertyTable =
      { TypeOK =
        { property = Property.TypeOK, index = 0, class = T.PropertyClass.safety }
      , NoLocalWorkLost =
        { property = Property.NoLocalWorkLost
        , index = 1
        , class = T.PropertyClass.safety
        }
      , NoSourceWorkLost =
        { property = Property.NoSourceWorkLost
        , index = 2
        , class = T.PropertyClass.safety
        }
      , TypedRefusal =
        { property = Property.TypedRefusal
        , index = 3
        , class = T.PropertyClass.safety
        }
      , NoReRead =
        { property = Property.NoReRead, index = 4, class = T.PropertyClass.safety }
      , CrashAtomicity =
        { property = Property.CrashAtomicity
        , index = 5
        , class = T.PropertyClass.safety
        }
      , OneCheckout =
        { property = Property.OneCheckout
        , index = 6
        , class = T.PropertyClass.safety
        }
      , NoDriftedLanding =
        { property = Property.NoDriftedLanding
        , index = 7
        , class = T.PropertyClass.safety
        }
      , AdoptClaimsOnlyEqual =
        { property = Property.AdoptClaimsOnlyEqual
        , index = 8
        , class = T.PropertyClass.safety
        }
      , ForeignRefsUnowned =
        { property = Property.ForeignRefsUnowned
        , index = 9
        , class = T.PropertyClass.safety
        }
      , HandoffOwnsOnlyItsBlock =
        { property = Property.HandoffOwnsOnlyItsBlock
        , index = 10
        , class = T.PropertyClass.safety
        }
      , WithinBudget =
        { property = Property.WithinBudget
        , index = 11
        , class = T.PropertyClass.budget
        }
      , ConvergesWhenQuiet =
        { property = Property.ConvergesWhenQuiet
        , index = 12
        , class = T.PropertyClass.temporal
        }
      , NoWedge =
        { property = Property.NoWedge, index = 13, class = T.PropertyClass.temporal }
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

let propertyEq =
      \(p : Property) -> \(q : Property) -> natEq (propertyIndex p) (propertyIndex q)

{- Reachability witnesses: each holds until the bound explores the state it
   names, so a reach row's REACHED shows that state is reached.
-}
let Witness =
      < Witness_Converged
      | Witness_SettledAfterCrash
      | Witness_CapturedNotLanded
      | Witness_DriftCarried
      | Witness_KeyDriftLanded
      | Witness_AdoptedConverged
      | Witness_HandoffConverged
      | Witness_HandoffCleared
      | Witness_HandoffCarriedCleared
      >

let witnessTable =
      { Witness_Converged = Witness.Witness_Converged
      , Witness_SettledAfterCrash = Witness.Witness_SettledAfterCrash
      , Witness_CapturedNotLanded = Witness.Witness_CapturedNotLanded
      , Witness_DriftCarried = Witness.Witness_DriftCarried
      , Witness_KeyDriftLanded = Witness.Witness_KeyDriftLanded
      , Witness_AdoptedConverged = Witness.Witness_AdoptedConverged
      , Witness_HandoffConverged = Witness.Witness_HandoffConverged
      , Witness_HandoffCleared = Witness.Witness_HandoffCleared
      , Witness_HandoffCarriedCleared = Witness.Witness_HandoffCarriedCleared
      }

-- The 59 actions of EstateConverge.tla's Next, in the sorted order of the never column.
let Action =
      < Begin
      | BranchRow
      | Capture
      | ChildCrashHead
      | ChildCrashTxn
      | Crash
      | Decide_
      | FirstAddWt
      | FirstLandMain
      | FirstLedgerWt
      | FirstPlanWt
      | FirstStepWt
      | HandoffCheck
      | HandoffCrash
      | HandoffExchange
      | HandoffFinish
      | HandoffRecord
      | HandoffSettle
      | HandoffStart
      | HeadSet
      | Import
      | IndexPublish
      | Journal
      | OpBlock
      | OpHead
      | OpIndex
      | OpLock
      | OpRacy
      | OpRef
      | OpRestore
      | OpSeat
      | OpStash
      | OpTouch
      | OpUntracked
      | OpWtWork
      | ProbeExchange
      | Quiet
      | RefFinish
      | RefLedger
      | RefStash
      | RefStep
      | RefTxnDeletes
      | RefTxnWrites
      | SeatOp
      | SeatVerify
      | SrcCreate
      | SrcDelete
      | SrcMove
      | SrcRename
      | SrcSeat
      | SrcStage
      | SrcStashDrop
      | SrcStashPush
      | SrcSwitch
      | SrcTypeChange
      | SrcWorktree
      | Tick
      | WriteIntent
      | WriteLanded
      >

let actionIndex =
      \(a : Action) ->
        merge
          { Begin = 0
          , BranchRow = 1
          , Capture = 2
          , ChildCrashHead = 3
          , ChildCrashTxn = 4
          , Crash = 5
          , Decide_ = 6
          , FirstAddWt = 7
          , FirstLandMain = 8
          , FirstLedgerWt = 9
          , FirstPlanWt = 10
          , FirstStepWt = 11
          , HandoffCheck = 12
          , HandoffCrash = 13
          , HandoffExchange = 14
          , HandoffFinish = 15
          , HandoffRecord = 16
          , HandoffSettle = 17
          , HandoffStart = 18
          , HeadSet = 19
          , Import = 20
          , IndexPublish = 21
          , Journal = 22
          , OpBlock = 23
          , OpHead = 24
          , OpIndex = 25
          , OpLock = 26
          , OpRacy = 27
          , OpRef = 28
          , OpRestore = 29
          , OpSeat = 30
          , OpStash = 31
          , OpTouch = 32
          , OpUntracked = 33
          , OpWtWork = 34
          , ProbeExchange = 35
          , Quiet = 36
          , RefFinish = 37
          , RefLedger = 38
          , RefStash = 39
          , RefStep = 40
          , RefTxnDeletes = 41
          , RefTxnWrites = 42
          , SeatOp = 43
          , SeatVerify = 44
          , SrcCreate = 45
          , SrcDelete = 46
          , SrcMove = 47
          , SrcRename = 48
          , SrcSeat = 49
          , SrcStage = 50
          , SrcStashDrop = 51
          , SrcStashPush = 52
          , SrcSwitch = 53
          , SrcTypeChange = 54
          , SrcWorktree = 55
          , Tick = 56
          , WriteIntent = 57
          , WriteLanded = 58
          }
          a

let actionTable =
      { Begin = Action.Begin
      , BranchRow = Action.BranchRow
      , Capture = Action.Capture
      , ChildCrashHead = Action.ChildCrashHead
      , ChildCrashTxn = Action.ChildCrashTxn
      , Crash = Action.Crash
      , Decide_ = Action.Decide_
      , FirstAddWt = Action.FirstAddWt
      , FirstLandMain = Action.FirstLandMain
      , FirstLedgerWt = Action.FirstLedgerWt
      , FirstPlanWt = Action.FirstPlanWt
      , FirstStepWt = Action.FirstStepWt
      , HandoffCheck = Action.HandoffCheck
      , HandoffCrash = Action.HandoffCrash
      , HandoffExchange = Action.HandoffExchange
      , HandoffFinish = Action.HandoffFinish
      , HandoffRecord = Action.HandoffRecord
      , HandoffSettle = Action.HandoffSettle
      , HandoffStart = Action.HandoffStart
      , HeadSet = Action.HeadSet
      , Import = Action.Import
      , IndexPublish = Action.IndexPublish
      , Journal = Action.Journal
      , OpBlock = Action.OpBlock
      , OpHead = Action.OpHead
      , OpIndex = Action.OpIndex
      , OpLock = Action.OpLock
      , OpRacy = Action.OpRacy
      , OpRef = Action.OpRef
      , OpRestore = Action.OpRestore
      , OpSeat = Action.OpSeat
      , OpStash = Action.OpStash
      , OpTouch = Action.OpTouch
      , OpUntracked = Action.OpUntracked
      , OpWtWork = Action.OpWtWork
      , ProbeExchange = Action.ProbeExchange
      , Quiet = Action.Quiet
      , RefFinish = Action.RefFinish
      , RefLedger = Action.RefLedger
      , RefStash = Action.RefStash
      , RefStep = Action.RefStep
      , RefTxnDeletes = Action.RefTxnDeletes
      , RefTxnWrites = Action.RefTxnWrites
      , SeatOp = Action.SeatOp
      , SeatVerify = Action.SeatVerify
      , SrcCreate = Action.SrcCreate
      , SrcDelete = Action.SrcDelete
      , SrcMove = Action.SrcMove
      , SrcRename = Action.SrcRename
      , SrcSeat = Action.SrcSeat
      , SrcStage = Action.SrcStage
      , SrcStashDrop = Action.SrcStashDrop
      , SrcStashPush = Action.SrcStashPush
      , SrcSwitch = Action.SrcSwitch
      , SrcTypeChange = Action.SrcTypeChange
      , SrcWorktree = Action.SrcWorktree
      , Tick = Action.Tick
      , WriteIntent = Action.WriteIntent
      , WriteLanded = Action.WriteLanded
      }

let actionEq = \(a : Action) -> \(b : Action) -> natEq (actionIndex a) (actionIndex b)

let showP = \(p : Property) -> showConstructor p

let showA = \(a : Action) -> showConstructor a

let showM = \(m : Mutation) -> showConstructor m

let showW = \(w : Witness) -> showConstructor w

-- Every value of each union, from its table ---------------------------------------

let PropertyRow = { mapKey : Text, mapValue : PropertyEntry }

let propertyRows = toMap propertyTable

let _ =
        assert
      :     map PropertyRow Text (\(e : PropertyRow) -> e.mapKey) propertyRows
        ===  map
               PropertyRow
               Text
               (\(e : PropertyRow) -> showP e.mapValue.property)
               propertyRows

let allProperties =
      ordered
        Property
        propertyIndex
        ( map
            PropertyRow
            Property
            (\(e : PropertyRow) -> e.mapValue.property)
            propertyRows
        )

let _ =
        assert
      :     map Property Natural propertyIndex allProperties
        ===  range (List/length PropertyRow propertyRows)

let ActionRow = { mapKey : Text, mapValue : Action }

let actionRows = toMap actionTable

let _ =
        assert
      :     map ActionRow Text (\(e : ActionRow) -> e.mapKey) actionRows
        ===  map ActionRow Text (\(e : ActionRow) -> showA e.mapValue) actionRows

let allActions =
      ordered
        Action
        actionIndex
        (map ActionRow Action (\(e : ActionRow) -> e.mapValue) actionRows)

let _ =
        assert
      :     map Action Natural actionIndex allActions
        ===  range (List/length ActionRow actionRows)

let WitnessRow = { mapKey : Text, mapValue : Witness }

let witnessRows = toMap witnessTable

let _ =
        assert
      :     map WitnessRow Text (\(e : WitnessRow) -> e.mapKey) witnessRows
        ===  map WitnessRow Text (\(e : WitnessRow) -> showW e.mapValue) witnessRows

let allWitnesses = map WitnessRow Witness (\(e : WitnessRow) -> e.mapValue) witnessRows

let labels =
      \(a : Type) ->
      \(rows : List { mapKey : Text, mapValue : a }) ->
        map
          { mapKey : Text, mapValue : a }
          Text
          (\(e : { mapKey : Text, mapValue : a }) -> e.mapKey)
          rows

let shown =
      \(a : Type) ->
      \(show : a -> Text) ->
      \(rows : List { mapKey : Text, mapValue : a }) ->
        map
          { mapKey : Text, mapValue : a }
          Text
          (\(e : { mapKey : Text, mapValue : a }) -> show e.mapValue)
          rows

let srcKindRows = toMap srcKindTable

let opKindRows = toMap opKindTable

let refusalRows = toMap refusalTable

let _ =
        assert
      :     labels SrcKind srcKindRows
        ===  shown SrcKind (\(k : SrcKind) -> showConstructor k) srcKindRows

let _ =
        assert
      :     labels OpKind opKindRows
        ===  shown OpKind (\(k : OpKind) -> showConstructor k) opKindRows

let _ =
        assert
      :     labels Refusal refusalRows
        ===  shown Refusal (\(r : Refusal) -> showConstructor r) refusalRows

{- Each closed union's labels, with the name of the EstateConverge.tla set
   that must hold exactly them.
-}
let closed =
      [ { name = "SrcKindSet", labels = labels SrcKind srcKindRows }
      , { name = "OpKindSet", labels = labels OpKind opKindRows }
      , { name = "Refusals", labels = labels Refusal refusalRows }
      ]

-- Constants -----------------------------------------------------------------------

-- One value per CONSTANT of EstateConverge.tla, rendered in this order.
let Constants =
      { Items : List Item
      , Refs : List Ref
      , Paths : List Path
      , IndexOn : Bool
      , SrcKinds : List SrcKind
      , OpKinds : List OpKind
      , MaxSrc : Natural
      , MaxOp : Natural
      , MaxCrashes : Natural
      , OpConcurrent : Bool
      , RacyEdits : Bool
      , ChildCrash : Bool
      , Landed : Bool
      , Adopt : Bool
      , Handoff : Bool
      , Mutation : Optional Mutation
      , BudgetSeconds : Natural
      }

let strings =
      \(xs : List Text) ->
        "{${join ", " (map Text Text (\(x : Text) -> "\"${x}\"") xs)}}"

let constantLines =
      \(c : Constants) ->
        let mutation =
              merge { None = "none", Some = \(m : Mutation) -> showM m } c.Mutation

        in  [ { name = "Items"
              , pad = "        "
              , value =
                  strings (map Item Text (\(i : Item) -> showConstructor i) c.Items)
              }
            , { name = "Refs", pad = "         ", value = strings (map Ref Text showRef c.Refs) }
            , { name = "Paths"
              , pad = "        "
              , value = strings (map Path Text showPath c.Paths)
              }
            , { name = "IndexOn", pad = "      ", value = showBool c.IndexOn }
            , { name = "SrcKinds"
              , pad = "     "
              , value =
                  strings
                    (map SrcKind Text (\(k : SrcKind) -> showConstructor k) c.SrcKinds)
              }
            , { name = "OpKinds"
              , pad = "      "
              , value =
                  strings
                    (map OpKind Text (\(k : OpKind) -> showConstructor k) c.OpKinds)
              }
            , { name = "MaxSrc", pad = "       ", value = Natural/show c.MaxSrc }
            , { name = "MaxOp", pad = "        ", value = Natural/show c.MaxOp }
            , { name = "MaxCrashes", pad = "   ", value = Natural/show c.MaxCrashes }
            , { name = "OpConcurrent", pad = " ", value = showBool c.OpConcurrent }
            , { name = "RacyEdits", pad = "    ", value = showBool c.RacyEdits }
            , { name = "ChildCrash", pad = "   ", value = showBool c.ChildCrash }
            , { name = "Landed", pad = "       ", value = showBool c.Landed }
            , { name = "Adopt", pad = "        ", value = showBool c.Adopt }
            , { name = "Handoff", pad = "      ", value = showBool c.Handoff }
            , { name = "Mutation", pad = "     ", value = "\"${mutation}\"" }
            , { name = "BudgetSeconds", pad = "", value = Natural/show c.BudgetSeconds }
            ]

{- The code on feat/estate-converge-20261008, on the smallest landing: the
   main checkout alone, landed from the initial capture, with only its
   "main" branch; nothing moves. Every row says what it adds.
-}
let defaults
    : Constants
    = { Items = [ Item.main ]
      , Refs = [ Ref.main ]
      , Paths = [] : List Path
      , IndexOn = False
      , SrcKinds = [] : List SrcKind
      , OpKinds = [] : List OpKind
      , MaxSrc = 0
      , MaxOp = 0
      , MaxCrashes = 0
      , OpConcurrent = False
      , RacyEdits = False
      , ChildCrash = False
      , Landed = True
      , Adopt = False
      , Handoff = False
      , Mutation = None Mutation
      , BudgetSeconds = 1800
      }

{- Refs: every ref class (local, the foo -> foo/bar pair, a remote-tracking
   pair, a tag), the source moving, creating, deleting, renaming and
   switching, a crash.
-}
let refs =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.foo_bar, Ref.o_x, Ref.o_x_y, Ref.t ]
          , SrcKinds =
            [ SrcKind.move
            , SrcKind.create
            , SrcKind.delete
            , SrcKind.rename
            , SrcKind.switch
            ]
          , MaxSrc = 2
          , MaxCrashes = 1
          }

{- Refs under operator edits: the local pair, the operator moving,
   creating or deleting a ref and putting a landed value back, a crash.
   No source switch (it stays this row's bound): a switch with a put-back
   ref is MC_ec_settle_branch_conflict's.
-}
let refsOp =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.foo_bar ]
          , SrcKinds = [ SrcKind.move, SrcKind.delete, SrcKind.rename ]
          , OpKinds = [ OpKind.ref, OpKind.restore ]
          , MaxSrc = 2
          , MaxOp = 2
          , MaxCrashes = 1
          }

{- Seats and the index: a landed directory and its file, content edits,
   additions, removals and a directory turned into a file at the source;
   operator edits, touches, untracked additions and `git add`; a crash.
-}
let seats =
          defaults
      //  { Paths = [ Path.d, Path.d_f ]
          , IndexOn = True
          , SrcKinds = [ SrcKind.seat, SrcKind.dirfile, SrcKind.index ]
          , OpKinds =
            [ OpKind.seat, OpKind.touch, OpKind.untracked, OpKind.index ]
          , MaxSrc = 2
          , MaxOp = 1
          , MaxCrashes = 1
          }

-- The stash: pushes and drops at the source, an operator push in flight, a crash.
let stash =
          defaults
      //  { SrcKinds = [ SrcKind.stash ]
          , OpKinds = [ OpKind.stash ]
          , OpConcurrent = True
          , MaxSrc = 2
          , MaxOp = 1
          , MaxCrashes = 1
          }

{- The linked worktree: a main and a linked worktree on "dev" in one
   repository, commits, switches, worktree remove and add, a crash.
-}
let linked =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev, Ref.foo ]
          , SrcKinds = [ SrcKind.move, SrcKind.switch, SrcKind.worktree ]
          , MaxSrc = 2
          , MaxCrashes = 1
          }

-- Operator edits while an apply is in flight: a seat and the index.
let concurrent =
          defaults
      //  { Paths = [ Path.d ]
          , IndexOn = True
          , SrcKinds = [ SrcKind.seat, SrcKind.index ]
          , OpKinds = [ OpKind.seat, OpKind.index ]
          , OpConcurrent = True
          , MaxSrc = 1
          , MaxOp = 1
          }

-- A racy write: the operator's, in the timestamp tick of bulkload's.
let racy =
          defaults
      //  { Paths = [ Path.d ]
          , SrcKinds = [ SrcKind.seat ]
          , OpKinds = [ OpKind.racy ]
          , RacyEdits = True
          , MaxSrc = 2
          , MaxOp = 1
          }

-- First landings, from an empty destination: the main, then its linked worktree.
let first =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev ]
          , Paths = [ Path.d ]
          , SrcKinds = [ SrcKind.move ]
          , MaxSrc = 1
          , Landed = False
          }

{- Liveness, one checkout: refs moving, deleted and renamed (foo ->
   foo/bar), a seat edit, a touch, a crash.
-}
let liveC =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.foo_bar, Ref.o_x ]
          , Paths = [ Path.d ]
          , SrcKinds =
            [ SrcKind.move, SrcKind.delete, SrcKind.rename, SrcKind.seat ]
          , OpKinds = [ OpKind.touch ]
          , MaxSrc = 2
          , MaxOp = 1
          , MaxCrashes = 1
          }

-- Liveness, a main and its linked worktree (no switches: see the findings).
let liveLinked =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev, Ref.foo ]
          , SrcKinds = [ SrcKind.move ]
          , MaxSrc = 2
          , MaxCrashes = 1
          }

{- Review 3 (2026-10-09). Finding 1: o/x -> o/x/y, the operator moving
   o/x while the rename's ref intent is pending (a crash): the create of
   o/x/y must be dropped, not retried forever.
-}
let renameOp =
          defaults
      //  { Refs = [ Ref.main, Ref.o_x, Ref.o_x_y ]
          , SrcKinds = [ SrcKind.rename ]
          , OpKinds = [ OpKind.ref ]
          , MaxSrc = 1
          , MaxOp = 1
          , MaxCrashes = 1
          }

{- Finding 2: a `git rebase` or `git bisect` in the main detaches its HEAD
   and holds its branch; the linked worktree's ref step must leave it.
-}
let rebase =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev, Ref.foo ]
          , SrcKinds = [ SrcKind.move ]
          , OpKinds = [ OpKind.rebase ]
          , MaxSrc = 1
          , MaxOp = 1
          , MaxCrashes = 1
          }

-- Finding 3: `git add` while an apply is in flight, over two converges.
let indexConcurrent =
          defaults
      //  { IndexOn = True
          , SrcKinds = [ SrcKind.index ]
          , OpKinds = [ OpKind.index ]
          , OpConcurrent = True
          , MaxSrc = 2
          , MaxOp = 1
          }

{- Findings 4 and 10: the operator switches HEAD (to a branch, or to an
   unborn one) and moves or deletes refs, a checked-out branch too, around
   a crashed converge.
-}
let headOp =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , SrcKinds = [ SrcKind.move ]
          , OpKinds = [ OpKind.ref, OpKind.head ]
          , MaxSrc = 1
          , MaxOp = 2
          , MaxCrashes = 1
          }

{- Finding 5: the operator removes the landed directory or replaces it with
   a file while a seat under it is pending, then puts it back.
-}
let seatParent =
          defaults
      //  { Paths = [ Path.d, Path.d_f ]
          , SrcKinds = [ SrcKind.seat ]
          , OpKinds = [ OpKind.dir, OpKind.restore ]
          , MaxSrc = 1
          , MaxOp = 3
          , MaxCrashes = 1
          }

{- Findings 10, 11 and 14: the operator switches HEAD while an apply that
   moves and switches is in flight.
-}
let headConcurrent =
          defaults
      //  { Refs = [ Ref.main, Ref.dev, Ref.foo ]
          , SrcKinds = [ SrcKind.move, SrcKind.switch ]
          , OpKinds = [ OpKind.head ]
          , OpConcurrent = True
          , MaxSrc = 2
          , MaxOp = 1
          }

-- Finding 11: a ref edit while the ref step is in flight.
let refsConcurrent =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.t ]
          , SrcKinds = [ SrcKind.move ]
          , OpKinds = [ OpKind.ref ]
          , OpConcurrent = True
          , MaxSrc = 1
          , MaxOp = 1
          }

{- Finding 13: crashes inside Git's ref transactions (partial commits, lock
   files left) without HEAD switches or a stash (findings 3 and 8 of the
   rows below).
-}
let childCrashRefs =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.o_x, Ref.o_x_y ]
          , SrcKinds = [ SrcKind.move, SrcKind.rename, SrcKind.delete ]
          , MaxSrc = 2
          , MaxCrashes = 1
          , ChildCrash = True
          }

{- Finding 12: liveness after seat and HEAD edits that the operator puts
   back (ref edits put back are the documented limit
   MC_ec_finding_restored_ref's).
-}
let liveOps =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , Paths = [ Path.d ]
          , SrcKinds = [ SrcKind.move, SrcKind.seat ]
          , OpKinds = [ OpKind.seat, OpKind.head, OpKind.restore ]
          , MaxSrc = 1
          , MaxOp = 2
          , MaxCrashes = 1
          }

{- The convergence lane's fixes (2026-10-09): the bounds of the earlier
   finding rows, now a pass row each (the fixed code) and the primary row
   of the mutation that is the code before the fix.
-}
let fileToDir =
          defaults
      //  { Paths = [ Path.d, Path.d_f ]
          , SrcKinds = [ SrcKind.dirfile, SrcKind.filedir ]
          , MaxSrc = 2
          , MaxCrashes = 1
          }

let staleBranch =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev, Ref.foo ]
          , IndexOn = True
          , SrcKinds = [ SrcKind.move, SrcKind.switch ]
          , OpKinds = [ OpKind.index ]
          , MaxSrc = 3
          , MaxOp = 1
          , MaxCrashes = 1
          }

{- The simulated rows: the pass row explores 20,000 seeded traces (the
   finding's simulation reached its violation within a few hundred); the
   mutation's row stops at its first violation.
-}
let simulateFlags = "-simulate num=200000 -depth 40 -seed 20261009"

let simulatePassFlags = "-simulate num=20000 -depth 40 -seed 20261009"

let branchConflict =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.foo_bar ]
          , SrcKinds = [ SrcKind.rename, SrcKind.switch ]
          , OpKinds = [ OpKind.ref, OpKind.restore ]
          , MaxSrc = 2
          , MaxOp = 2
          , MaxCrashes = 1
          }

let childCrashLive =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , SrcKinds = [ SrcKind.move ]
          , MaxSrc = 1
          , MaxCrashes = 1
          , ChildCrash = True
          }

let stashLock =
          defaults
      //  { SrcKinds = [ SrcKind.stash ]
          , MaxSrc = 1
          , MaxCrashes = 1
          , ChildCrash = True
          }

let setHeadPartial =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , SrcKinds = [ SrcKind.move, SrcKind.switch ]
          , MaxSrc = 2
          , MaxCrashes = 1
          , ChildCrash = True
          }

let stashRevert =
          defaults
      //  { SrcKinds = [ SrcKind.stash ]
          , OpKinds = [ OpKind.stashdrop ]
          , MaxSrc = 2
          , MaxOp = 1
          , MaxCrashes = 1
          }

let oldHeadBranch =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , SrcKinds = [ SrcKind.move, SrcKind.switch ]
          , MaxSrc = 2
          }

let firstLinkedCrash =
      first // { Paths = [] : List Path, SrcKinds = [] : List SrcKind, MaxSrc = 0, MaxCrashes = 1 }

{- Review 4 (2026-10-09, later). The blocker: the source moves its linked
   worktree to another branch while the operator switches a worktree, with
   applies in flight (OneCheckout).
-}
let oneCheckout =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev, Ref.foo ]
          , SrcKinds = [ SrcKind.worktree ]
          , OpKinds = [ OpKind.head ]
          , OpConcurrent = True
          , MaxSrc = 2
          , MaxOp = 1
          }

-- ... with commits and a crash, the operator's switch between applies.
let oneCheckoutCrash =
          oneCheckout
      //  { SrcKinds = [ SrcKind.worktree, SrcKind.move ]
          , MaxCrashes = 1
          , OpConcurrent = False
          }

{- A killed Git child in a pass that goes on, a main and its linked
   worktree: set_head's HEAD lock, then the linked worktree's ref step.
-}
let childCrashLinked =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev, Ref.foo, Ref.t ]
          , SrcKinds = [ SrcKind.move, SrcKind.switch ]
          , MaxSrc = 2
          , MaxCrashes = 1
          , ChildCrash = True
          }

{- Empty locks: a remote-tracking ref pruned at the source (a delete), a
   killed delete child, and the operator's live writer holding an empty
   ref lock or packed-refs.lock, or removing a stale one.
-}
let emptyLock =
          defaults
      //  { Refs = [ Ref.main, Ref.o_x ]
          , SrcKinds = [ SrcKind.delete ]
          , OpKinds = [ OpKind.lock ]
          , MaxSrc = 1
          , MaxOp = 2
          , MaxCrashes = 1
          , ChildCrash = True
          }

-- The operator's work in a linked worktree whose first landing a crash cut short.
let firstLinkedCrashOp = firstLinkedCrash // { OpKinds = [ OpKind.wtwork ], MaxOp = 1 }

-- The stash, one push, a crash that may be a power loss.
{- #38 (2026-10-15, S5 under OI-1003-Q152): the main's capture pass is
   raced by the source (HEAD switched, or the index staged, under it);
   commits and switches between passes; a crash. A raced pass records
   drift custody (a drifted capture no apply lays down), and the next
   pass carries the move.
-}
let raced =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , IndexOn = True
          , SrcKinds = [ SrcKind.race, SrcKind.switch, SrcKind.move ]
          , MaxSrc = 2
          , MaxCrashes = 1
          }

{- #38: a raced switch away and the source switched back, so the source
   equals the drifted capture's state; no crash.
-}
let racedBack =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , SrcKinds = [ SrcKind.race, SrcKind.switch ]
          , MaxSrc = 2
          }

{- OI-1003-Q161: the source pushes or drops a stash while the main's
   capture pass runs, and pushes and drops between passes; a crash.
-}
let racedStash =
          defaults
      //  { SrcKinds = [ SrcKind.stashrace, SrcKind.stash ]
          , MaxSrc = 2
          , MaxCrashes = 1
          }

{- OI-1003-Q161: a raced stash push and the source dropping it again, so
   the source equals the drifted capture's state; no crash.
-}
let racedStashBack =
      defaults // { SrcKinds = [ SrcKind.stashrace, SrcKind.stash ], MaxSrc = 2 }

let stashCrash =
      defaults // { SrcKinds = [ SrcKind.stash ], MaxSrc = 1, MaxCrashes = 1 }

{- #226 (estate adopt, OI-1003-Q171/Q172, 2026-10-15): the run starts from
   a destination bulkload never landed (Adopt), its main checkout present,
   every component the first capture's or the operator's, and the main is
   adopted in place by proof. Refs of every class the R1, R2, R3 and R10
   rows meet, a seat and the index; the source moves after the copy (a
   capture the destination no longer equals refuses DIVERGED) or after the
   adoption (a converge in place); a crash at any step.
-}
let adopt =
          defaults
      //  { Refs = [ Ref.main, Ref.foo, Ref.t ]
          , Paths = [ Path.d ]
          , IndexOn = True
          , SrcKinds = [ SrcKind.move, SrcKind.seat ]
          , MaxSrc = 1
          , MaxCrashes = 1
          , Landed = False
          , Adopt = True
          }

{- #226: the GFTB shape of a neo-only worktree: the main adopted, its
   linked worktree (absent at the destination) first-landed into the
   adopted repository; the destination's HEAD on dev makes it land
   detached.
-}
let adoptLinked =
          defaults
      //  { Items = [ Item.main, Item.wt ]
          , Refs = [ Ref.main, Ref.dev ]
          , SrcKinds = [ SrcKind.move ]
          , MaxSrc = 1
          , MaxCrashes = 1
          , Landed = False
          , Adopt = True
          }

{- #226: operator edits of a seat, the HEAD branch and the index while the
   adoption is in flight, and a crash. One ref: a create in flight under a
   concurrent ref edit is the stated limit of the in-flight ghost for refs
   (README, "What EstateConverge does not prove").
-}
let adoptOp =
          defaults
      //  { Refs = [ Ref.main ]
          , Paths = [ Path.d ]
          , IndexOn = True
          , SrcKinds = [ SrcKind.seat ]
          , OpKinds = [ OpKind.seat, OpKind.ref, OpKind.index ]
          , MaxSrc = 1
          , MaxOp = 1
          , MaxCrashes = 1
          , OpConcurrent = True
          , Landed = False
          , Adopt = True
          }

-- #226: liveness of an adoption and the converge after it.
let adoptLive =
          defaults
      //  { Refs = [ Ref.main, Ref.foo ]
          , Paths = [ Path.d ]
          , SrcKinds = [ SrcKind.move, SrcKind.seat ]
          , MaxSrc = 1
          , MaxCrashes = 1
          , Landed = False
          , Adopt = True
          }

{- #226 (review of 2026-10-16, finding 10): the source holds a stash at
   the copy (with "stash" in SrcKinds under Adopt), the destination's
   reflog empty (T1), the operator's (T2) or equal to it (T0, adopted with
   no write); the source then pushes or drops one; a crash.
-}
let adoptStash =
          defaults
      //  { SrcKinds = [ SrcKind.stash ]
          , MaxSrc = 1
          , MaxCrashes = 1
          , Landed = False
          , Adopt = True
          }

{- TIN-6569 (OI-1003-Q183, 2026-10-16): estate-handoff on the landed main.
   Its target seat HP (here "d") carries bulkload's block over the landed
   bytes, recorded in handoff.landed with its intent durable first; the
   source edits, removes or re-adds the target, so a converge replaces it
   under the block, removes it, or lifts a created block before its add;
   a render or a clear, and a crash at any step of either or of the apply.
-}
let handoff =
          defaults
      //  { Paths = [ Path.d ]
          , SrcKinds = [ SrcKind.seat ]
          , MaxSrc = 2
          , MaxCrashes = 1
          , Handoff = True
          }

{- TIN-6569: the operator edits the handed-off target (inside the block,
   which makes the file theirs; below it; or removes the block by hand) or
   the seat, between applies or while one is in flight; a source edit; a
   crash.
-}
let handoffOp =
          handoff
      //  { OpKinds = [ OpKind.block, OpKind.seat ]
          , MaxSrc = 1
          , MaxOp = 1
          , OpConcurrent = True
          }

{- OI-1003-Q190 (2026-10-16): the source's target starts with a carried
   block (another landing's, Cb(v)); --clear --clear-carried removes it
   from the landed value as a recorded edit (handoff.stripped) that converge
   accepts, alone or under this landing's block; the source then edits the
   target again, so a converge replaces the stripped seat; a crash at any
   step.
-}
let handoffCarried = handoff // { SrcKinds = [ SrcKind.seat, SrcKind.carried ] }

{- OI-1003-Q190: as handoffCarried, with the operator's edits of the
   target (inside the block, below it with or without a carried block of
   their own, the block removed by hand) and of the seat, also while an
   apply is in flight.
-}
let handoffCarriedOp = handoffOp // { SrcKinds = [ SrcKind.seat, SrcKind.carried ] }

-- Properties -------------------------------------------------------------------------

let safety = filter Property isSafety allProperties

let temporal = filter Property isTemporal allProperties

{- The verdict of every mutation: the one property its MC_ec_neg_<mutation>
   config must violate. A total merge.
-}
let verdict
    : Mutation -> Property
    = \(m : Mutation) ->
        merge
          { proof_stat_only = Property.NoLocalWorkLost
          , proof_stat_strict = Property.TypedRefusal
          , overwrite_modified = Property.NoLocalWorkLost
          , no_intent = Property.CrashAtomicity
          , exchange_unchecked = Property.NoLocalWorkLost
          , index_no_cas = Property.NoLocalWorkLost
          , stash_clear_blind = Property.NoLocalWorkLost
          , name_conflict_unchecked = Property.NoWedge
          , single_ref_txn = Property.NoWedge
          , head_branch_in_step = Property.TypedRefusal
          , no_checkout_guard = Property.TypedRefusal
          , converge_before_ref_settle = Property.TypedRefusal
          , dir_replace_unchecked = Property.TypedRefusal
          , prune_local = Property.NoSourceWorkLost
          , drop_provenance = Property.NoSourceWorkLost
          , noop_restages = Property.NoReRead
          , sibling_intent_reproves = Property.NoReRead
          , finish_after_all_deletes = Property.NoWedge
          , rebase_unguarded = Property.NoLocalWorkLost
          , ledger_reads_index = Property.NoLocalWorkLost
          , unborn_head_untyped = Property.TypedRefusal
          , seat_parent_untyped = Property.TypedRefusal
          , refs_unsynced = Property.TypedRefusal
          , probe_left = Property.CrashAtomicity
          , settle_moves_checked_out = Property.NoLocalWorkLost
          , txn_no_symref_verify = Property.NoLocalWorkLost
          , txn_no_cas = Property.NoLocalWorkLost
          , head_no_cas = Property.NoLocalWorkLost
          , settle_remove_dir = Property.NoWedge
          , step_moves_intent_branch = Property.CrashAtomicity
          , settle_replays_branch = Property.TypedRefusal
          , stale_lock_kept = Property.NoWedge
          , set_head_one_txn = Property.CrashAtomicity
          , stash_settle_any_suffix = Property.NoLocalWorkLost
          , old_head_branch_skipped = Property.ConvergesWhenQuiet
          , no_landing_intent = Property.NoWedge
          , head_not_recorded = Property.NoLocalWorkLost
          , head_attach_unchecked = Property.OneCheckout
          , intent_head_lock_kept = Property.TypedRefusal
          , empty_lock_removed = Property.NoLocalWorkLost
          , packed_new_kept = Property.NoWedge
          , stash_reflog_unsynced = Property.TypedRefusal
          , recovery_overwrites = Property.NoLocalWorkLost
          , drift_key_reused = Property.ConvergesWhenQuiet
          , drift_applied = Property.NoDriftedLanding
          , adopt_without_proof = Property.AdoptClaimsOnlyEqual
          , adopt_ref_unequal = Property.ForeignRefsUnowned
          , adopt_untyped = Property.TypedRefusal
          , adopt_ledger_before_refs = Property.ConvergesWhenQuiet
          , adopt_r1_no_row = Property.ConvergesWhenQuiet
          , adopt_r2_not_adopted = Property.ConvergesWhenQuiet
          , adopt_no_recheck = Property.AdoptClaimsOnlyEqual
          , adopt_t0_no_row = Property.ConvergesWhenQuiet
          , handoff_no_overlay = Property.TypedRefusal
          , handoff_overlay_any_block = Property.NoLocalWorkLost
          , handoff_clear_unchecked = Property.NoLocalWorkLost
          , handoff_exchange_unchecked = Property.NoLocalWorkLost
          , handoff_no_intent = Property.HandoffOwnsOnlyItsBlock
          , handoff_retake_unchecked = Property.NoLocalWorkLost
          , handoff_keep_racy_unchecked = Property.NoLocalWorkLost
          , handoff_strip_no_overlay = Property.TypedRefusal
          , handoff_strip_unchecked = Property.NoLocalWorkLost
          }
          m

-- Rows ---------------------------------------------------------------------------------

let Pass = { invariants : List Property, properties : List Property, never : List Action }

{- simulate: a seeded or random -simulate run that must end with no
   violation of the invariants it names (never a model-checking result);
   for a scenario BFS cannot reach at any affordable bound.
-}
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
      , spec : T.Specification
      , flags : Optional Text
      }

let Against = < verdict | also : { suffix : Text, property : Property } >

let NegRow =
      { mutation : Mutation
      , against : Against
      , constants : Constants
      , comment : { head : Text, tail : List Text }
      }

{- TIN-6569: the hand-off's actions, never enabled in a row whose
   constants leave Handoff FALSE (every row before it): appended to each
   such pass row's never set, so those rows are written as before.
-}
let handoffActions =
      [ Action.HandoffCheck
      , Action.HandoffCrash
      , Action.HandoffExchange
      , Action.HandoffFinish
      , Action.HandoffRecord
      , Action.HandoffSettle
      , Action.HandoffStart
      , Action.OpBlock
      ]

let row =
      \(name : Text) ->
      \(expect : Expect) ->
      \(comment : List Text) ->
      \(constants : Constants) ->
        let expect =
              if    constants.Handoff
              then  expect
              else  merge
                      { pass =
                          \(p : Pass) -> Expect.pass (p // { never = p.never # handoffActions })
                      , fail = Expect.fail
                      , reach = Expect.reach
                      , simulate = Expect.simulate
                      , inconclusive = Expect.inconclusive
                      }
                      expect

        in    { name, expect, comment, constants, spec = T.Specification.Spec, flags = None Text }
            : Row

let liveRow = \(r : Row) -> r // { spec = T.Specification.LiveSpec }

let pass =
      \(never : List Action) ->
        Expect.pass { invariants = safety, properties = [] : List Property, never }

let passLive =
      \(never : List Action) ->
        Expect.pass { invariants = safety, properties = temporal, never }

let line = \(head : Text) -> { head, tail = [] : List Text }

-- Actions never enabled in a row: groups the rows share.
let firstLanding =
      [ Action.FirstAddWt
      , Action.FirstLandMain
      , Action.FirstLedgerWt
      , Action.FirstPlanWt
      , Action.FirstStepWt
      ]

let childCrash = [ Action.ChildCrashHead, Action.ChildCrashTxn ]

let negProperty =
      \(n : NegRow) ->
        merge
          { verdict = verdict n.mutation
          , also = \(a : { suffix : Text, property : Property }) -> a.property
          }
          n.against

let negName =
      \(n : NegRow) ->
        let suffix =
              merge
                { verdict = showM n.mutation
                , also = \(a : { suffix : Text, property : Property }) -> a.suffix
                }
                n.against

        in  "MC_ec_neg_${suffix}"

let neg =
      \(n : NegRow) ->
        let property = negProperty n

        let r =
              row
                (negName n)
                (Expect.fail property)
                (   [ "NEGATIVE: ${n.comment.head}" ]
                  # n.comment.tail
                  # [ "Mutation ${showM n.mutation} must violate ${showP property}." ]
                )
                (n.constants // { Mutation = Some n.mutation })

        let r = if isTemporal property then liveRow r else r

        -- BFS cannot reach this mutation's trace at an affordable bound.
        in  if    natEq
                    (mutationIndex n.mutation)
                    (mutationIndex Mutation.step_moves_intent_branch)
            then  r // { flags = Some simulateFlags }
            else  r

-- The configs, in run order ---------------------------------------------------------

let budgetSelftest =
      row
        "MC_ec_budget_selftest"
        Expect.inconclusive
        [ "BUDGET SELF-TEST (expected INCONCLUSIVE). MC_ec_linked's constants with"
        , "a 5 s budget: the search needs far longer, so WithinBudget, and"
        , "nothing else, must trip, which proves EstateConverge.tla's budget is"
        , "evaluated per state. The bound is MC_ec_linked's, so a broken budget"
        , "still ends (as a PASS, which tla-check rejects)."
        ]
        (linked // { BudgetSeconds = 5 })

let positives =
      [ row
          "MC_ec_refs"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.SeatVerify, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick , Action.ProbeExchange, Action.OpLock, Action.OpWtWork ])
          [ "Refs (Q144: remote-tracking refs follow the source; local branches"
          , "and tags deleted at the source are kept and reported): every class,"
          , "the foo -> foo/bar rename both ways, o/x -> o/x/y, HEAD switches and"
          , "a crash at any step."
          ]
          refs
      , row
          "MC_ec_refs_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Refs under operator edits (Q144: a ref the operator changed is kept"
          , "and reported, ref-modified-at-destination): the operator moves,"
          , "creates or deletes a ref (a checked-out one too), or puts a landed"
          , "value back, between the source's moves, deletes and renames; a"
          , "crash. Source switches with a put-back ref are"
          , "MC_ec_settle_branch_conflict's."
          ]
          refsOp
      , row
          "MC_ec_seats"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpStash, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
          [ "The checkout (Q144: content, not stat): seat edits, additions,"
          , "removals and a directory turned into a file at the source; operator"
          , "edits, touches, an untracked file in a landed directory and"
          , "`git add`; a crash at any step, settled by content."
          ]
          seats
      , row
          "MC_ec_stash"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick , Action.ProbeExchange, Action.OpLock, Action.OpWtWork ])
          [ "The stash (findings 4, 8): pushes and drops at the source, every"
          , "store a compare-and-swap; an operator push while the step runs, and"
          , "a crash. Operator drops are MC_ec_stash_settle_revert's."
          ]
          stash
      , row
          "MC_ec_linked"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.Tick , Action.ProbeExchange, Action.OpLock, Action.OpWtWork ])
          [ "A main and a linked worktree in one repository (findings 1, 11,"
          , "review 2 finding 1): commits, switches, worktree remove and add,"
          , "each item's ref step leaving the other's checked-out branch alone,"
          , "a crash at any step."
          ]
          linked
      , row
          "MC_ec_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
          [ "Operator edits of a seat and of the index while an apply is in"
          , "flight (findings 3, 10): the exchange's displaced check and the"
          , "index compare-and-swap keep them."
          ]
          concurrent
      , row
          "MC_ec_racy"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
          [ "A racy write (design 4.2): the operator writes a seat in the"
          , "timestamp tick of bulkload's write, keeping its stat; the proof"
          , "reads a racy seat's content, so the edit refuses."
          ]
          racy
      , row
          "MC_ec_first"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcSeat, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree , Action.ProbeExchange, Action.OpLock, Action.OpWtWork ])
          [ "First landings (#215): the main from an empty destination, then its"
          , "linked worktree, natively, with no carry/* branch; then a converge."
          , "No crash: a crashed linked first landing (#216's residual) is"
          , "MC_ec_first_linked_crash's."
          ]
          first
      , liveRow
          ( row
              "MC_ec_live"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpUntracked, Action.SrcCreate, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
              [ "Liveness under fair captures and applies (SF per item): once the"
              , "source and the operator stop, with no operator edit left, every"
              , "item converges (ConvergesWhenQuiet), and no item refuses forever"
              , "without an operator edit of its own (NoWedge). Refs move, are"
              , "deleted and renamed (foo -> foo/bar), a seat changes, a touch, a"
              , "crash."
              ]
              liveC
          )
      , liveRow
          ( row
              "MC_ec_live_linked"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick , Action.ProbeExchange, Action.OpLock, Action.OpWtWork ])
              [ "Liveness, a main and its linked worktree: commits on both"
              , "checkouts' branches, a crash. Switches are MC_ec_old_head_branch's"
              , "and the documented limit MC_ec_finding_detached_reattach's."
              ]
              liveLinked
          )
      , liveRow
          ( row
              "MC_ec_refs_rename_op"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
              [ "Review 3 finding 1: o/x -> o/x/y with the operator moving o/x"
              , "while the ref intent is pending (a crash). finish keeps o/x (its"
              , "delete will not run), so the create of o/x/y is dropped"
              , "(ref-name-conflict), never retried: no wedge."
              ]
              renameOp
          )
      , row
          "MC_ec_rebase"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 finding 2: a rebase or bisect detaches the main's HEAD and"
          , "holds its branch; every ref step, the linked worktree's included,"
          , "leaves the held branch (rebase-merge/head-name, BISECT_START)."
          ]
          rebase
      , row
          "MC_ec_index_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 finding 3: `git add` while an apply is in flight, over two"
          , "converges: the ledger records the index publish_index installed, so"
          , "an add after the publish fails the next proof (MODIFIED)."
          ]
          indexConcurrent
      , row
          "MC_ec_head_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 findings 4 and 10: the operator switches HEAD (to a branch"
          , "or an unborn one) and moves or deletes refs, a checked-out branch"
          , "too, around a crashed converge: the settle never moves a branch"
          , "some worktree has checked out, and an unborn HEAD refuses typed."
          ]
          headOp
      , liveRow
          ( row
              "MC_ec_seat_parent"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
              [ "Review 3 finding 5: the operator removes the landed directory or"
              , "replaces it with a file while a seat under it is pending; the"
              , "settle refuses GIT_CONVERGE_INTERRUPTED (never a path code), and"
              , "converges once the operator puts the directory back."
              ]
              seatParent
          )
      , row
          "MC_ec_head_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 findings 10, 11 and 14: the operator switches HEAD while an"
          , "apply that moves and switches is in flight: every worktree's HEAD"
          , "held for the ref transaction, and set_head's compare-and-swap, keep"
          , "it."
          ]
          headConcurrent
      , row
          "MC_ec_head_concurrent_crash"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "MC_ec_head_concurrent's bound with a crash: a settle's HEAD phase,"
          , "after the branch's transaction, finds HEAD already at its new value"
          , "(the operator switched to the very branch) and is done, never an"
          , "unjustified refusal (found by a probe of this bound, 2026-10-09)."
          ]
          (headConcurrent // { MaxCrashes = 1 })
      , row
          "MC_ec_refs_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 finding 11: a ref edit while the ref step is in flight:"
          , "the transaction's compare-and-swap keeps it."
          ]
          refsConcurrent
      , row
          "MC_ec_child_crash"
          (pass [ Action.ChildCrashHead, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 finding 13: every safety invariant with crashes inside"
          , "Git's ref transactions (any partial commit, lock files left), no"
          , "HEAD switch and no stash (MC_ec_set_head_partial and"
          , "MC_ec_stash_lock have those)."
          ]
          childCrashRefs
      , liveRow
          ( row
              "MC_ec_live_ops"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
              [ "Review 3 finding 12: liveness with seat edits and HEAD switches"
              , "the operator puts back, and a crash."
              ]
              liveOps
          )
      , liveRow
          ( row
              "MC_ec_file_to_dir_settle"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcSeat, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcWorktree, Action.OpLock, Action.OpWtWork ])
              [ "The earlier report's finding 1, fixed: the source turns the landed"
              , "directory into a file and back, a crash at any step. A crash after"
              , "the MkDir of a file turned into a directory leaves the new"
              , "directory at the seat; the settle's Remove (Engine::remove,"
              , "settling) finds the new side there and is done, never refusing."
              ]
              fileToDir
          )
      ,     row
              "MC_ec_settle_stale_branch"
              (Expect.simulate safety)
              [ "SIMULATION (BFS needs depth 25 past millions of states): the"
              , "earlier report's finding 2, fixed. The main's intent moves HEAD to"
              , "a branch it also moves (foo, 2 -> 5); a crash, then an operator"
              , "`git add`, makes its settle refuse once; the linked worktree's ref"
              , "step skips foo (a pending workspace intent names it:"
              , "intent_branches), and a settle whose branch line can no longer"
              , "apply re-plans HEAD. Every safety invariant holds on every trace."
              ]
              staleBranch
        //  { flags = Some simulatePassFlags }
      , row
          "MC_ec_settle_branch_conflict"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3's finding 19, fixed: the source renames foo to foo/bar and"
          , "switches to it; the operator deletes foo, so the converge plans"
          , "HEAD onto a new foo/bar; a crash before set_head; the operator puts"
          , "foo back. The settle's `create foo/bar` can no longer apply, so it"
          , "re-plans HEAD (detached at the captured head, the branch recorded:"
          , "head-detached-ref-conflict) instead of refusing on every rerun."
          ]
          branchConflict
      , liveRow
          ( row
              "MC_ec_child_crash_live"
              (passLive [ Action.ChildCrashHead, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
              [ "The earlier report's finding 3, fixed: a crash inside Git's"
              , "update-ref child leaves its lock; the settle removes a lock whose"
              , "content is the value its pending op writes (bulkload's own), so"
              , "no rerun refuses forever."
              ]
              childCrashLive
          )
      , row
          "MC_ec_stash_lock"
          (pass [ Action.ChildCrashHead, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 finding 8, fixed: a crash inside a stash store leaves"
          , "refs/stash.lock; the settle removes it (its content is a stash entry"
          , "the op stores), and a store that meets a lock refuses typed instead"
          , "of clearing the stash row."
          ]
          stashLock
      , row
          "MC_ec_set_head_partial"
          (pass [ Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "Review 3 finding 13 (the earlier report's finding 3), fixed:"
          , "set_head commits the branch line in its own transaction, then HEAD,"
          , "so a killed Git child never leaves HEAD on a branch at a value that"
          , "is neither the intent's old nor its new one."
          ]
          setHeadPartial
      , row
          "MC_ec_stash_settle_revert"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
          [ "The earlier report's finding 4, fixed: each stash store's reflog is"
          , "made durable in the ref intent, and a settle continues only from"
          , "that progress (or one store past it); an operator's `git stash drop`"
          , "or `clear` after a crash is kept, never undone."
          ]
          stashRevert
      , liveRow
          ( row
              "MC_ec_old_head_branch"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.OpLock, Action.OpWtWork ])
              [ "The earlier report's finding 5 (review 2 finding 1's stated"
              , "limit), fixed: a commit on the checkout's branch, then a switch to"
              , "another. A second ref step after set_head brings the branch HEAD"
              , "left to the capture, so it converges while the source holds still."
              ]
              oldHeadBranch
          )
      , liveRow
          ( row
              "MC_ec_first_linked_crash"
              (passLive [ Action.BranchRow, Action.Capture, Action.ChildCrashHead, Action.ChildCrashTxn, Action.HeadSet, Action.Import, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefStep, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SeatOp, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.WriteIntent, Action.WriteLanded, Action.OpLock, Action.OpWtWork ])
              [ "The earlier report's finding 7 (#216's residual), fixed: a landing"
              , "intent is durable before `worktree add`, so a crash before the"
              , "linked worktree's ledger leaves a worktree the rerun finishes"
              , "forward (recover_linked_landing), never GIT_DESTINATION_OCCUPIED."
              ]
              firstLinkedCrash
          )
      , row
          "MC_ec_one_checkout"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.Tick ])
          [ "Review 4, the blocker: the source moves its linked worktree to"
          , "another branch while the operator switches a worktree, applies in"
          , "flight. set_head never attaches HEAD to a branch another worktree"
          , "has checked out (OneCheckout): it refuses, and the settle re-plans"
          , "HEAD detached with the branch recorded."
          ]
          oneCheckout
      , row
          "MC_ec_one_checkout_crash"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.Tick ])
          [ "MC_ec_one_checkout's bound with commits and a crash, the operator's"
          , "switch between applies: a settle whose"
          , "HEAD branch another worktree checked out since the intent re-plans"
          , "HEAD (head-detached-ref-conflict), branch line done or not."
          ]
          oneCheckoutCrash
      , row
          "MC_ec_child_crash_linked"
          (pass [ Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 4: a killed Git child in a pass that goes on (the item"
          , "refuses INTERRUPTED, the next runs), a main and its linked"
          , "worktree. Every pending intent's HEAD and branch locks a killed"
          , "set_head left are removed before any ref transaction, which holds"
          , "every worktree's HEAD."
          ]
          childCrashLinked
      , liveRow
          ( row
              "MC_ec_empty_lock"
              (passLive [ Action.ChildCrashHead, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
              [ "Review 4: an empty lock (a killed delete's, packed-refs.lock, an"
              , "operator's live writer's) is never removed by bulkload; the item"
              , "refuses typed until its owner, or the operator as Git"
              , "instructs, removes it (NoWedge's stated exception), and an"
              , "orphan packed-refs.new is removed once no lock holds it."
              ]
              emptyLock
          )
      , row
          "MC_ec_first_linked_crash_op"
          (pass [ Action.BranchRow, Action.Capture, Action.ChildCrashHead, Action.ChildCrashTxn, Action.HeadSet, Action.Import, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefStep, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SeatOp, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick, Action.WriteIntent, Action.WriteLanded ])
          [ "Review 4: the operator's work in a linked worktree whose first"
          , "landing a crash cut short (a staged file, a write over a seat):"
          , "the recovery refuses GIT_DESTINATION_MODIFIED and changes nothing."
          ]
          firstLinkedCrashOp
      , liveRow
          ( row
              "MC_ec_capture_raced"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
              [ "#38 (S5 under OI-1003-Q152, no freeze window): the source switches"
              , "HEAD or stages its index while the main's capture pass runs, and"
              , "commits and switches between passes; a crash. A raced pass is"
              , "drift custody, never a refusal: export drift (a capture never laid"
              , "down, NoDriftedLanding) or key drift (a coherent capture that"
              , "applies), its key poisoned either way, and once the source holds"
              , "still the next pass's capture converges the landing to it."
              ]
              raced
          )
      , liveRow
          ( row
              "MC_ec_capture_raced_stash"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
              [ "OI-1003-Q161 (S5 under OI-1003-Q152): the source pushes or drops a"
              , "stash while the main's capture pass runs, and between passes; a"
              , "crash. The same drift custody as #38's HEAD and index, in the same"
              , "two classes (git_carry.rs stash_drift): never laid down when the"
              , "export saw it, its key poisoned either way, and once the source"
              , "holds still the next pass carries the stash in the source's order."
              ]
              racedStash
          )
      , row
          "MC_ec_adopt"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "#226 (estate adopt, OI-1003-Q171/Q172): a destination bulkload never"
          , "landed, each component the first capture's or the operator's: the"
          , "main adopted in place by content (DIVERGED with nothing written when"
          , "it differs, AdoptClaimsOnlyEqual), R1 creates, R2 adopts, R3 and R10"
          , "keep (ForeignRefsUnowned); the source moves before or after the"
          , "adoption, and a crash at any step."
          ]
          adopt
      , row
          "MC_ec_adopt_linked"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstLandMain, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "#226: the main adopted, its linked worktree (absent at the"
          , "destination: neo-only) first-landed into the adopted repository;"
          , "the destination's HEAD on dev lands it detached; a crash."
          ]
          adoptLinked
      , row
          "MC_ec_adopt_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpLock, Action.OpRacy, Action.OpRestore, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "#226: operator edits of a seat, the HEAD branch and the index while"
          , "the adoption is in flight, and a crash: the proof is rechecked under"
          , "the lock, the ledger records the proof's prints, so an edit after"
          , "them refuses MODIFIED at the next converge, and the operator's ref"
          , "is never owned."
          ]
          adoptOp
      , liveRow
          ( row
              "MC_ec_adopt_live"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
              [ "#226: liveness of an adoption and the converge after it, with the"
              , "stated exceptions AdoptBehind (no older-capture baseline) and"
              , "AdoptKept (a ref R3 or R10 left at adoption stays the"
              , "destination's: the ghost of what its ref step left, review of"
              , "2026-10-16)."
              ]
              adoptLive
          )
      , liveRow
          ( row
              "MC_ec_adopt_stash"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpWtWork, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
              [ "#226 (review of 2026-10-16, finding 10): the source holds a stash at"
              , "the copy; the destination's reflog is empty (T1), the operator's"
              , "(T2) or equal to it (T0, adopted with no write); the source pushes"
              , "or drops one before or after the adoption; a crash. Liveness with"
              , "the stated exception AdoptStashKept (a reflog T2 kept, or one a"
              , "capture carrying none left, stays the destination's, as AdoptKept"
              , "for refs)."
              ]
              adoptStash
          )
      , row
          "MC_ec_handoff"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpBlock, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "TIN-6569 (OI-1003-Q183): estate-handoff writes bulkload's block over"
          , "the landed target, recorded with its intent durable first; the"
          , "source edits, removes and re-adds the target, so a converge"
          , "replaces it under the block (the displaced block goes with it),"
          , "removes it, or lifts a created block before the add; a render or a"
          , "clear, and a crash at any step: no seat is refused for an intact"
          , "block (TypedRefusal), and the block stands only while a record or"
          , "intent names it (HandoffOwnsOnlyItsBlock)."
          ]
          handoff
      , row
          "MC_ec_handoff_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "TIN-6569: the operator edits inside the block (the file becomes"
          , "theirs: converge refuses MODIFIED, the hand-off never writes it),"
          , "below it, or removes it by hand, or edits the seat, while an apply"
          , "or a hand-off is in flight; a crash: the operator's bytes are kept"
          , "under or without the block (NoLocalWorkLost)."
          ]
          handoffOp
      , row
          "MC_ec_handoff_carried"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpBlock, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "OI-1003-Q190: the source's target starts with a carried block;"
          , "--clear --clear-carried removes it from the landed value only, as a"
          , "recorded edit converge accepts (alone or under the block), and the"
          , "source then edits the target again; a crash at any step: no seat"
          , "is refused for bulkload's own edit (TypedRefusal), and every"
          , "component is old or new (CrashAtomicity)."
          ]
          handoffCarried
      , row
          "MC_ec_handoff_carried_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "OI-1003-Q190: as MC_ec_handoff_carried, with the operator's edits"
          , "(below the block, with or without a carried block of their own,"
          , "inside it, removed by hand) also while an apply is in flight:"
          , "--clear-carried never removes a carried block from the operator's"
          , "bytes (NoLocalWorkLost)."
          ]
          handoffCarriedOp
      , liveRow
          ( row
              "MC_ec_handoff_live"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpBlock, Action.OpHead, Action.OpIndex, Action.OpLock, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.OpWtWork, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
              [ "TIN-6569: liveness with the hand-off on: a quiet estate converges"
              , "with bulkload's block standing over the landed target"
              , "(Converged accepts the block on the source's value)."
              ]
              (handoff // { MaxSrc = 1 })
          )
      ]

-- Reachability witnesses: expected REACHED.
let witnesses =
      [ row
          "MC_ec_reach_converged"
          (Expect.reach Witness.Witness_Converged)
          [ "REACH (expected REACHED): at MC_ec_seats' bound a converge (not a"
          , "first landing) completes and journals a later capture."
          ]
          seats
      , row
          "MC_ec_reach_settled"
          (Expect.reach Witness.Witness_SettledAfterCrash)
          [ "REACH (expected REACHED): at MC_ec_seats' bound a converge a crash"
          , "interrupted is settled forward and journaled by a rerun."
          ]
          seats
      , row
          "MC_ec_reach_captured_not_landed"
          (Expect.reach Witness.Witness_CapturedNotLanded)
          [ "REACH (expected REACHED): a ref tip a capture carried, superseded by"
          , "a later capture before any apply, is at the destination neither"
          , "natively nor at provenance: only CORPUS (GitCarry.tla) keeps it."
          ]
          (refs // { MaxCrashes = 0 })
      , row
          "MC_ec_reach_drift_carried"
          (Expect.reach Witness.Witness_DriftCarried)
          [ "REACH (expected REACHED): #38's drift custody is carried: a drifted"
          , "capture is followed by a converge that journals a later capture."
          ]
          racedBack
      , row
          "MC_ec_reach_key_drift_landed"
          (Expect.reach Witness.Witness_KeyDriftLanded)
          [ "REACH (expected REACHED): #38's key-drift class (the move outside"
          , "the export's window: key poisoned, no in-band marker) is a coherent"
          , "capture an apply lays down and journals, as the code does."
          ]
          racedBack
      , row
          "MC_ec_reach_stash_drift_carried"
          (Expect.reach Witness.Witness_DriftCarried)
          [ "REACH (expected REACHED): OI-1003-Q161's stash drift custody is"
          , "carried: a capture a stash push or drop raced is followed by a"
          , "converge that journals a later capture."
          ]
          racedStashBack
      , row
          "MC_ec_reach_adopted_converged"
          (Expect.reach Witness.Witness_AdoptedConverged)
          [ "REACH (expected REACHED): #226: an adopted main converges in place to"
          , "a later capture: its journal holds the adoption's capture and the"
          , "converge's."
          ]
          adopt
      , row
          "MC_ec_reach_handoff_converged"
          (Expect.reach Witness.Witness_HandoffConverged)
          [ "REACH (expected REACHED): TIN-6569: a converge journals a later"
          , "capture while bulkload's recorded block stands on the target, and"
          , "the block is still there."
          ]
          handoff
      , row
          "MC_ec_reach_handoff_cleared"
          (Expect.reach Witness.Witness_HandoffCleared)
          [ "REACH (expected REACHED): TIN-6569: a hand-off is cleared, and a"
          , "converge journals a later capture."
          ]
          handoff
      , row
          "MC_ec_reach_handoff_carried_cleared"
          (Expect.reach Witness.Witness_HandoffCarriedCleared)
          [ "REACH (expected REACHED): OI-1003-Q190: a recorded --clear-carried"
          , "removed a carried block from the landed target, and a converge"
          , "replaced the stripped seat with the source's next value."
          ]
          handoffCarried
      ]

{- Documented limits of the code on feat/estate-converge-20261008 (the
   convergence lane's fixes, 2026-10-09): each row is the code at a bound
   that reaches the limit, expected to fail its named property. Both need
   an A4 no-op that reads the ledgers; the next capture converges them.
   README.md, "Findings (EstateConverge)".
-}
let findings =
      [ liveRow
          ( row
              "MC_ec_finding_detached_reattach"
              (Expect.fail Property.ConvergesWhenQuiet)
              [ "DOCUMENTED LIMIT (expected to fail; finding 11's re-attach): the"
              , "source moves its linked worktree off dev and the main onto dev. The"
              , "main, applied first, detaches (dev is the destination worktree's);"
              , "the worktree then leaves dev, but the main's journal makes every"
              , "later apply a no-op (A4 reads nothing), so it re-attaches only on"
              , "the next capture, not while the source holds still."
              ]
              (     linked
                //  { SrcKinds = [ SrcKind.switch, SrcKind.worktree ]
                    , MaxSrc = 3
                    , MaxCrashes = 0
                    }
              )
          )
      , liveRow
          ( row
              "MC_ec_finding_restored_ref"
              (Expect.fail Property.ConvergesWhenQuiet)
              [ "DOCUMENTED LIMIT (expected to fail; review 3 finding 12): the"
              , "source moves foo; the operator moves it too, so the converge keeps"
              , "it (ref-modified-at-destination) and journals the capture. The"
              , "operator then puts foo back to its landed value: A4's journal no-op"
              , "never plans foo again, so it converges only on the next capture."
              ]
              (     defaults
                //  { Refs = [ Ref.main, Ref.foo ]
                    , SrcKinds = [ SrcKind.move ]
                    , OpKinds = [ OpKind.ref, OpKind.restore ]
                    , MaxSrc = 1
                    , MaxOp = 2
                    , MaxCrashes = 1
                    }
              )
          )
      , row
          "MC_ec_neg_live_unfair"
          (Expect.fail Property.ConvergesWhenQuiet)
          [ "NEGATIVE: without fairness a behaviour may stop before any capture"
          , "or apply, so ConvergesWhenQuiet depends on the fairness assumption."
          ]
          (     defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.move ]
                , MaxSrc = 1
                }
          )
      ]

{- The primary row of every mutation, keyed by its label. A total merge over
   Mutation.
-}
let Primary =
      { mutation : Mutation
      , constants : Constants
      , comment : { head : Text, tail : List Text }
      }

let seat1 =
          defaults
      //  { Paths = [ Path.d ], SrcKinds = [ SrcKind.seat ], MaxSrc = 1 }

let primary =
      { proof_stat_only =
        { mutation = Mutation.proof_stat_only
        , constants = racy
        , comment =
            line "prove trusts an unchanged print even when the seat is racy."
        }
      , proof_stat_strict =
        { mutation = Mutation.proof_stat_strict
        , constants = seat1 // { OpKinds = [ OpKind.touch ], MaxOp = 1 }
        , comment =
          { head = "prove refuses any print change (WP0(d)-strict identity), so a"
          , tail =
            [ "`touch` or an identical save refuses GIT_DESTINATION_MODIFIED;"
            , "Q144 rules content, not stat."
            ]
          }
        }
      , overwrite_modified =
        { mutation = Mutation.overwrite_modified
        , constants = seat1 // { OpKinds = [ OpKind.seat ], MaxOp = 1 }
        , comment = line "converge proceeds when the proof fails."
        }
      , no_intent =
        { mutation = Mutation.no_intent
        , constants = seat1 // { MaxCrashes = 1 }
        , comment =
            line "converge writes no workspace intent before its first write."
        }
      , exchange_unchecked =
        { mutation = Mutation.exchange_unchecked
        , constants =
            seat1 // { OpKinds = [ OpKind.seat ], MaxOp = 1, OpConcurrent = True }
        , comment =
            line "a replacement neither checks the seat's print nor the displaced file."
        }
      , index_no_cas =
        { mutation = Mutation.index_no_cas
        , constants =
                defaults
            //  { IndexOn = True
                , SrcKinds = [ SrcKind.index ]
                , OpKinds = [ OpKind.index ]
                , OpConcurrent = True
                , MaxSrc = 1
                , MaxOp = 1
                }
        , comment = line "publish_index renames the new index over whatever is there."
        }
      , stash_clear_blind =
        { mutation = Mutation.stash_clear_blind
        , constants = stash // { MaxCrashes = 0 }
        , comment =
            line "a rewrite runs `stash clear`, not a compare-and-swap delete (design T5)."
        }
      , name_conflict_unchecked =
        { mutation = Mutation.name_conflict_unchecked
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo, Ref.foo_bar ]
                , SrcKinds = [ SrcKind.rename ]
                , MaxSrc = 1
                }
        , comment =
          { head = "plan and finish keep no create back above or below a ref that"
          , tail =
            [ "stays: foo -> foo/bar (foo kept, deleted at the source) retries a"
            , "create Git can never make (review 2 finding 3, the blocker)."
            ]
          }
        }
      , single_ref_txn =
        { mutation = Mutation.single_ref_txn
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.o_x, Ref.o_x_y ]
                , SrcKinds = [ SrcKind.rename ]
                , MaxSrc = 1
                }
        , comment =
          { head = "transact runs deletes and writes in one transaction: Git refuses"
          , tail = [ "a prune of o/x and a create of o/x/y together (review 2 finding 3)." ]
          }
        }
      , head_branch_in_step =
        { mutation = Mutation.head_branch_in_step
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.move, SrcKind.switch ]
                , MaxSrc = 2
                }
        , comment =
          { head = "the ref step moves the branch HEAD switches to, and set_head"
          , tail =
            [ "keeps its compare-and-swap line (review 2 finding 1, the blocker):"
            , "the converge refuses GIT_CONVERGE_INTERRUPTED with no operator edit."
            , "Its wedge (NoWedge before) is gone since a settle re-plans a branch"
            , "line that can no longer apply (finding 19), so the row names the"
            , "unjustified refusal."
            ]
          }
        }
      , no_checkout_guard =
        { mutation = Mutation.no_checkout_guard
        , constants =
                defaults
            //  { Items = [ Item.main, Item.wt ]
                , Refs = [ Ref.main, Ref.dev ]
                , SrcKinds = [ SrcKind.move ]
                , MaxSrc = 1
                }
        , comment =
            line "a ref step moves a branch another worktree has checked out (finding 1)."
        }
      , converge_before_ref_settle =
        { mutation = Mutation.converge_before_ref_settle
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.move ]
                , MaxSrc = 1
                , MaxCrashes = 1
                }
        , comment =
            line "apply_workspace converges before settling a pending ref intent."
        }
      , dir_replace_unchecked =
        { mutation = Mutation.dir_replace_unchecked
        , constants =
                defaults
            //  { Paths = [ Path.d, Path.d_f ]
                , SrcKinds = [ SrcKind.seat, SrcKind.dirfile ]
                , OpKinds = [ OpKind.untracked ]
                , MaxSrc = 2
                , MaxOp = 1
                }
        , comment =
          { head = "no holds_unlanded check: a landed directory the source turns into"
          , tail =
            [ "a file, holding an operator's file, refuses only after writes"
            , "(review 2 findings 2, 4)."
            ]
          }
        }
      , prune_local =
        { mutation = Mutation.prune_local
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.delete ]
                , MaxSrc = 1
                }
        , comment =
            line "a local branch deleted at the source is deleted (Q144: kept)."
        }
      , drop_provenance =
        { mutation = Mutation.drop_provenance
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.move ]
                , MaxSrc = 1
                }
        , comment =
            line "a converge drops the provenance of the instance it replaced (Q5)."
        }
      , noop_restages =
        { mutation = Mutation.noop_restages
        , constants = defaults
        , comment =
            line "an item whose journal names its capture is staged and proved again."
        }
      , sibling_intent_reproves =
        { mutation = Mutation.sibling_intent_reproves
        , constants =
                defaults
            //  { Items = [ Item.main, Item.wt ]
                , Refs = [ Ref.main, Ref.dev, Ref.foo ]
                , SrcKinds = [ SrcKind.move ]
                , MaxSrc = 2
                , MaxCrashes = 1
                }
        , comment =
            line "another item's pending ref intent re-proves a finished item (review 2 finding 7)."
        }
      , finish_after_all_deletes =
        { mutation = Mutation.finish_after_all_deletes
        , constants = renameOp
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 1): finish removes every delete's"
          , tail =
            [ "name from the names after the step, whether the delete runs or"
            , "not, so a create beside a kept o/x is retried forever."
            ]
          }
        }
      , rebase_unguarded =
        { mutation = Mutation.rebase_unguarded
        , constants = rebase // { MaxCrashes = 0 }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 2): the skip set is `worktree list"
          , tail =
            [ "--porcelain`'s branches only, so a branch under a rebase or bisect"
            , "(its worktree detached) is moved by another item's ref step."
            ]
          }
        }
      , ledger_reads_index =
        { mutation = Mutation.ledger_reads_index
        , constants = indexConcurrent
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 3): next_ledger re-reads the index"
          , tail =
            [ "after publishing it, so an operator's `git add` in between is"
            , "recorded as landed and replaced by the next converge."
            ]
          }
        }
      , unborn_head_untyped =
        { mutation = Mutation.unborn_head_untyped
        , constants =
                defaults
            //  { SrcKinds = [ SrcKind.move ]
                , OpKinds = [ OpKind.ref ]
                , MaxSrc = 1
                , MaxOp = 1
                }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 4): head_of's `rev-parse --verify"
          , tail = [ "HEAD` fails on an unborn HEAD: GIT_CHILD_FAILED, not MODIFIED." ]
          }
        }
      , seat_parent_untyped =
        { mutation = Mutation.seat_parent_untyped
        , constants = seatParent // { OpKinds = [ OpKind.dir ], MaxOp = 1 }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 5): Engine::run passes Seat::at's"
          , tail =
            [ "error on after the intent (PATH_ESCAPES_ROOT, Io) when a seat's"
            , "parent is no longer a directory."
            ]
          }
        }
      , refs_unsynced =
        { mutation = Mutation.refs_unsynced
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.move ]
                , MaxSrc = 2
                , MaxCrashes = 1
                }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 7): no directory sync after"
          , tail =
            [ "update-ref; a power loss undoes a ref rename the ledgers record,"
            , "and the next apply blames the destination."
            ]
          }
        }
      , probe_left =
        { mutation = Mutation.probe_left
        , constants = seat1 // { MaxCrashes = 1 }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 9): probe_exchange's files a crash"
          , tail = [ "leaves in the worktree root are never removed." ]
          }
        }
      , settle_moves_checked_out =
        { mutation = Mutation.settle_moves_checked_out
        , constants = headOp // { OpKinds = [ OpKind.head ], MaxOp = 1 }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 10, the blocker): finish redoes an"
          , tail =
            [ "op on a branch the operator checked out after the crash, and its"
            , "transaction has no symref-verify line."
            ]
          }
        }
      , txn_no_symref_verify =
        { mutation = Mutation.txn_no_symref_verify
        , constants =
                headOp
            //  { OpKinds = [ OpKind.head ]
                , MaxOp = 1
                , MaxCrashes = 0
                , OpConcurrent = True
                }
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 10): the ref step's transaction has"
          , tail =
            [ "no symref-verify line, so a `git switch` between the plan and the"
            , "update-ref has its branch moved under it."
            ]
          }
        }
      , txn_no_cas =
        { mutation = Mutation.txn_no_cas
        , constants = refsConcurrent
        , comment =
            line "transact's lines carry no old value (review 3 finding 11)."
        }
      , head_no_cas =
        { mutation = Mutation.head_no_cas
        , constants =
            headConcurrent // { SrcKinds = [ SrcKind.switch ], MaxSrc = 1 }
        , comment =
            line "set_head's HEAD and branch lines carry no old value (review 3 finding 11)."
        }
      , settle_remove_dir =
        { mutation = Mutation.settle_remove_dir
        , constants = fileToDir
        , comment =
          { head = "CODE BEFORE THE FIX (the earlier report's finding 1): a settle's"
          , tail =
            [ "Remove of a file the source turned into a directory refuses when"
            , "the seat already holds the new directory, on every rerun."
            ]
          }
        }
      , step_moves_intent_branch =
        { mutation = Mutation.step_moves_intent_branch
        , constants = staleBranch
        , comment =
          { head = "CODE BEFORE THE FIX (the earlier report's finding 2; by"
          , tail =
            [ "simulation): a ref step skips only checked-out branches, so the"
            , "linked worktree's step moves the branch the main's pending intent"
            , "moves with HEAD."
            ]
          }
        }
      , settle_replays_branch =
        { mutation = Mutation.settle_replays_branch
        , constants = branchConflict
        , comment =
          { head = "CODE BEFORE THE FIX (finding 19): the settle replays the"
          , tail =
            [ "intent's `create foo/bar` beside a put-back foo, refusing"
            , "GIT_CONVERGE_INTERRUPTED on every rerun with every edit undone."
            ]
          }
        }
      , stale_lock_kept =
        { mutation = Mutation.stale_lock_kept
        , constants = childCrashLive
        , comment =
          { head = "CODE BEFORE THE FIX (the earlier report's finding 3): nothing"
          , tail =
            [ "removes the lock a killed update-ref child left, so every"
            , "settle's compare-and-swap fails."
            ]
          }
        }
      , set_head_one_txn =
        { mutation = Mutation.set_head_one_txn
        , constants = setHeadPartial
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 13): set_head's one"
          , tail =
            [ "transaction can commit HEAD without its branch line when the Git"
            , "child dies."
            ]
          }
        }
      , stash_settle_any_suffix =
        { mutation = Mutation.stash_settle_any_suffix
        , constants = stashRevert
        , comment =
          { head = "CODE BEFORE THE FIX (the earlier report's finding 4): a settle"
          , tail =
            [ "continues the stash from any suffix of the new order, so an"
            , "operator's drop or clear after a crash is undone."
            ]
          }
        }
      , old_head_branch_skipped =
        { mutation = Mutation.old_head_branch_skipped
        , constants = oldHeadBranch
        , comment =
          { head = "CODE BEFORE THE FIX (the earlier report's finding 5): no ref step"
          , tail =
            [ "after set_head, so the branch HEAD left stays behind while the"
            , "source holds still."
            ]
          }
        }
      , head_not_recorded =
        { mutation = Mutation.head_not_recorded
        , constants = headConcurrent // { MaxCrashes = 1 }
        , comment =
          { head = "CODE BEFORE THE FIX (found by the model, 2026-10-09): nothing"
          , tail =
            [ "records HEAD at its new value after set_head, so after a crash a"
            , "settle moves HEAD off the branch the operator switched back to"
            , "while the pass still ran."
            ]
          }
        }
      , no_landing_intent =
        { mutation = Mutation.no_landing_intent
        , constants = firstLinkedCrash
        , comment =
          { head = "CODE BEFORE THE FIX (#216's residual): no landing intent, so a"
          , tail =
            [ "crash between `worktree add` and the linked worktree's ledger"
            , "leaves a worktree every rerun refuses GIT_DESTINATION_OCCUPIED."
            ]
          }
        }
      , head_attach_unchecked =
        { mutation = Mutation.head_attach_unchecked
        , constants = oneCheckout
        , comment =
          { head = "CODE BEFORE THE FIX (review 4, the blocker): set_head attaches"
          , tail =
            [ "HEAD to a branch with no check of the other worktrees, and the"
            , "settle replays it once the branch line is done, so an operator's"
            , "`git switch` since the plan ends with two worktrees on one branch."
            ]
          }
        }
      , intent_head_lock_kept =
        { mutation = Mutation.intent_head_lock_kept
        , constants = childCrashLinked
        , comment =
          { head = "CODE BEFORE THE FIX (review 4): a HEAD.lock a killed set_head"
          , tail =
            [ "left is removed only by that item's own workspace settle, so the"
            , "linked worktree's ref transaction (which holds every HEAD) meets"
            , "it and refuses with no cause it writes, and the ref settle that"
            , "runs first never reaches the workspace settle."
            ]
          }
        }
      , empty_lock_removed =
        { mutation = Mutation.empty_lock_removed
        , constants = emptyLock
        , comment =
          { head = "CODE BEFORE THE FIX (review 4): the settle removes an empty lock"
          , tail =
            [ "of its intent's deletes, and packed-refs.lock, when older than a"
            , "timestamp tick: a live writer's (the operator's update-ref"
            , "--stdin session) is taken from it."
            ]
          }
        }
      , packed_new_kept =
        { mutation = Mutation.packed_new_kept
        , constants = emptyLock
        , comment =
          { head = "CODE BEFORE THE FIX (review 4): a packed-refs.new a killed"
          , tail =
            [ "delete left is never removed, so once the operator removed the"
            , "stale packed-refs.lock every rerun's delete still fails on it."
            ]
          }
        }
      , stash_reflog_unsynced =
        { mutation = Mutation.stash_reflog_unsynced
        , constants = stashCrash
        , comment =
          { head = "CODE BEFORE THE FIX (review 4): update-ref appends the stash's"
          , tail =
            [ "reflog and never syncs it, so a power loss leaves the reflog"
            , "behind the progress and ledger that name it, and the stash is"
            , "read as the operator's (stash-modified-at-destination)."
            ]
          }
        }
      , drift_key_reused =
        { mutation = Mutation.drift_key_reused
        , constants = racedBack
        , comment =
          { head = "#38: a drifted capture records the key its pass computed (not"
          , tail =
            [ "the poisoned one), so when the source comes back to the state the"
            , "raced snapshot read, the next pass is a reuse hit on a capture no"
            , "apply lays down, and the landing never converges."
            ]
          }
        }
      , drift_applied =
        { mutation = Mutation.drift_applied
        , constants = racedBack
        , comment =
          { head = "#38 review finding 11: Begin is enabled on a capture its export"
          , tail =
            [ "saw drift (the in-band marker not read), so an apply lays down a"
            , "capture that does not hold what moved under it."
            ]
          }
        }
      , adopt_without_proof =
        { mutation = Mutation.adopt_without_proof
        , constants =
                defaults
            //  { Paths = [ Path.d ], IndexOn = True, Landed = False, Adopt = True }
        , comment =
          { head = "#226: the adoption writes its ledger from the destination as"
          , tail =
            [ "found, with no proof, so the ledger claims the operator's seat,"
            , "index or HEAD as bulkload's."
            ]
          }
        }
      , adopt_ref_unequal =
        { mutation = Mutation.adopt_ref_unequal
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ], Landed = False, Adopt = True }
        , comment =
            line "#226: R2 adopts a destination ref whose value is not the carried one."
        }
      , adopt_untyped =
        { mutation = Mutation.adopt_untyped
        , constants =
            defaults // { Paths = [ Path.d ], Landed = False, Adopt = True }
        , comment =
            line "#226: a destination that differs refuses with no converge code."
        }
      , adopt_ledger_before_refs =
        { mutation = Mutation.adopt_ledger_before_refs
        , constants =
                defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , MaxCrashes = 1
                , Landed = False
                , Adopt = True
                }
        , comment =
          { head = "#226: the adoption writes the workspace ledger before its ref"
          , tail =
            [ "step; a crash between leaves a ledger whose rerun is A6, so the"
            , "carried refs the destination lacks are never created while the"
            , "source holds still."
            ]
          }
        }
      , adopt_r1_no_row =
        { mutation = Mutation.adopt_r1_no_row
        , constants = adoptLive
        , comment =
          { head = "#226 (review of 2026-10-16, finding 6): R1 creates a carried ref"
          , tail =
            [ "the destination lacks and writes no ledger row, so the ref never"
            , "follows the source; AdoptKept excuses only the refs the adoption"
            , "left (R3, R10), never this one."
            ]
          }
        }
      , adopt_r2_not_adopted =
        { mutation = Mutation.adopt_r2_not_adopted
        , constants = adoptLive
        , comment =
          { head = "#226 (review of 2026-10-16, finding 6): a destination ref equal to"
          , tail =
            [ "the carried one gets no ledger row (R2 lost), so it never follows"
            , "the source."
            ]
          }
        }
      , adopt_no_recheck =
        { mutation = Mutation.adopt_no_recheck
        , constants = adoptOp
        , comment =
          { head = "#226 (review of 2026-10-16, finding 9): no recheck under the lock"
          , tail =
            [ "(C adopt::still): an operator edit between the proof and the"
            , "import is claimed. NoLocalWorkLost still holds (the next"
            , "converge refuses MODIFIED), so the claim is the property."
            ]
          }
        }
      , adopt_t0_no_row =
        { mutation = Mutation.adopt_t0_no_row
        , constants = adoptStash
        , comment =
          { head = "#226 (review of 2026-10-16, finding 10): T0 adopts an equal"
          , tail =
            [ "destination stash reflog and writes no stash row, so the stash"
            , "never follows the source."
            ]
          }
        }
      , handoff_no_overlay =
        { mutation = Mutation.handoff_no_overlay
        , constants = handoff
        , comment =
          { head = "TIN-6569: converge's proof ignores the hand-off record (the issue's"
          , tail =
            [ "bug: a naive prepend), so a quiet estate's converge refuses"
            , "GIT_DESTINATION_MODIFIED for bulkload's own block, an operator"
            , "edit nobody made."
            ]
          }
        }
      , handoff_overlay_any_block =
        { mutation = Mutation.handoff_overlay_any_block
        , constants = handoffOp
        , comment =
          { head = "TIN-6569: the overlay accepts a block the operator edited as"
          , tail =
            [ "bulkload's, so a converge replaces the target and drops the"
            , "operator's edit inside the block."
            ]
          }
        }
      , handoff_clear_unchecked =
        { mutation = Mutation.handoff_clear_unchecked
        , constants = handoffOp
        , comment =
          { head = "TIN-6569: --clear strips a block the operator edited, dropping"
          , tail = [ "their edit inside it." ]
          }
        }
      , handoff_exchange_unchecked =
        { mutation = Mutation.handoff_exchange_unchecked
        , constants = handoffOp // { OpKinds = [ OpKind.seat ] }
        , comment =
          { head = "TIN-6569: the hand-off's exchange never checks the displaced"
          , tail =
            [ "file against the print it read, so an operator save between the"
            , "read and the exchange is unlinked."
            ]
          }
        }
      , handoff_retake_unchecked =
        { mutation = Mutation.handoff_retake_unchecked
        , constants = handoffOp
        , comment =
          { head = "TIN-6569, review of 2026-10-16 (BLOCKER): a rerun over an intact"
          , tail =
            [ "block re-takes the record's print and keeps its old base, so the"
            , "print vouches for an operator's edit below the block and a"
            , "converge that replaces the target drops the edit."
            ]
          }
        }
      , handoff_keep_racy_unchecked =
        { mutation = Mutation.handoff_keep_racy_unchecked
        , constants = handoffOp
        , comment =
          { head = "TIN-6569, re-review of 2026-10-16 (finding 1): a rerun over an"
          , tail =
            [ "intact block keeps a racy record print equal to the seat's"
            , "unchecked and re-dates it; the print caught an operator's edit"
            , "below the block made between the exchange and the record, so it"
            , "vouches for the edit and a converge that replaces the target"
            , "drops it."
            ]
          }
        }
      , handoff_strip_no_overlay =
        { mutation = Mutation.handoff_strip_no_overlay
        , constants = handoffCarried
        , comment =
          { head = "OI-1003-Q190: converge's proof ignores handoff.stripped, so"
          , tail =
            [ "after a recorded --clear-carried a quiet estate's converge"
            , "refuses GIT_DESTINATION_MODIFIED for bulkload's own edit, an"
            , "operator edit nobody made."
            ]
          }
        }
      , handoff_strip_unchecked =
        { mutation = Mutation.handoff_strip_unchecked
        , constants = handoffCarriedOp
        , comment =
          { head = "OI-1003-Q190: --clear-carried removes a carried block from any"
          , tail =
            [ "bytes under the block, not only from the landed value, so an"
            , "operator's bytes below the block that start with a carried block"
            , "lose it."
            ]
          }
        }
      , handoff_no_intent =
        { mutation = Mutation.handoff_no_intent
        , constants = handoff
        , comment =
          { head = "TIN-6569: the hand-off writes its block before any intent, so"
          , tail =
            [ "a crash leaves a block no record or intent names: converge"
            , "would refuse it as the operator's."
            ]
          }
        }
      , recovery_overwrites =
        { mutation = Mutation.recovery_overwrites
        , constants = firstLinkedCrashOp
        , comment =
          { head = "CODE BEFORE THE FIX (review 4): the recovery of a crashed first"
          , tail =
            [ "linked landing checks HEAD only, and lays the landing's index and"
            , "seats over the operator's work in the worktree."
            ]
          }
        }
      }

let primaryOf
    : Mutation -> NegRow
    = \(m : Mutation) ->
        let p = merge primary m

        in  { mutation = p.mutation
            , against = Against.verdict
            , constants = p.constants
            , comment = p.comment
            }

let PrimaryRow = { mapKey : Text, mapValue : Primary }

let primaryRowsByLabel = toMap primary

let _ =
        assert
      :     map PrimaryRow Text (\(e : PrimaryRow) -> e.mapKey) primaryRowsByLabel
        ===  map
               PrimaryRow
               Text
               (\(e : PrimaryRow) -> showM e.mapValue.mutation)
               primaryRowsByLabel

let allMutations =
      map PrimaryRow Mutation (\(e : PrimaryRow) -> e.mapValue.mutation) primaryRowsByLabel

let negRows
    : List NegRow
    = [ primaryOf Mutation.proof_stat_only
      , primaryOf Mutation.proof_stat_strict
      , primaryOf Mutation.overwrite_modified
      , primaryOf Mutation.no_intent
      , primaryOf Mutation.exchange_unchecked
      , primaryOf Mutation.index_no_cas
      , primaryOf Mutation.stash_clear_blind
      , primaryOf Mutation.name_conflict_unchecked
      , primaryOf Mutation.single_ref_txn
      , primaryOf Mutation.head_branch_in_step
      , primaryOf Mutation.no_checkout_guard
      , primaryOf Mutation.converge_before_ref_settle
      , primaryOf Mutation.dir_replace_unchecked
      , primaryOf Mutation.prune_local
      , primaryOf Mutation.drop_provenance
      , primaryOf Mutation.noop_restages
      , primaryOf Mutation.sibling_intent_reproves
      , primaryOf Mutation.finish_after_all_deletes
      , primaryOf Mutation.rebase_unguarded
      , primaryOf Mutation.ledger_reads_index
      , primaryOf Mutation.unborn_head_untyped
      , primaryOf Mutation.seat_parent_untyped
      , primaryOf Mutation.refs_unsynced
      , primaryOf Mutation.probe_left
      , primaryOf Mutation.settle_moves_checked_out
      , primaryOf Mutation.txn_no_symref_verify
      , primaryOf Mutation.txn_no_cas
      , primaryOf Mutation.head_no_cas
      , primaryOf Mutation.settle_remove_dir
      , primaryOf Mutation.step_moves_intent_branch
      , primaryOf Mutation.settle_replays_branch
      , primaryOf Mutation.stale_lock_kept
      , { mutation = Mutation.stale_lock_kept
        , against =
            Against.also { suffix = "stale_stash_lock", property = Property.TypedRefusal }
        , constants = stashLock
        , comment =
          { head = "CODE BEFORE THE FIX (review 3 finding 8): a killed stash store's"
          , tail =
            [ "refs/stash.lock stays; every later store fails, and finish clears"
            , "the stash row and reports stash-modified-at-destination with no"
            , "operator edit."
            ]
          }
        }
      , primaryOf Mutation.set_head_one_txn
      , primaryOf Mutation.stash_settle_any_suffix
      , primaryOf Mutation.old_head_branch_skipped
      , primaryOf Mutation.no_landing_intent
      , primaryOf Mutation.head_not_recorded
      , primaryOf Mutation.head_attach_unchecked
      , primaryOf Mutation.intent_head_lock_kept
      , primaryOf Mutation.empty_lock_removed
      , primaryOf Mutation.packed_new_kept
      , primaryOf Mutation.stash_reflog_unsynced
      , primaryOf Mutation.recovery_overwrites
      , primaryOf Mutation.drift_key_reused
      , { mutation = Mutation.drift_key_reused
        , against =
            Against.also
              { suffix = "stash_drift_key_reused"
              , property = Property.ConvergesWhenQuiet
              }
        , constants = racedStashBack
        , comment =
          { head = "OI-1003-Q161: a capture a stash push raced records the key its"
          , tail =
            [ "pass computed (not the poisoned one), so when the source drops"
            , "the stash again, the next pass is a reuse hit on a capture no"
            , "apply lays down, and the landing never converges."
            ]
          }
        }
      , primaryOf Mutation.drift_applied
      , { mutation = Mutation.drift_applied
        , against =
            Against.also
              { suffix = "stash_drift_applied"
              , property = Property.NoDriftedLanding
              }
        , constants = racedStashBack
        , comment =
          { head = "OI-1003-Q161: Begin is enabled on a capture whose export a stash"
          , tail =
            [ "push or drop raced (the in-band marker not read), so an apply"
            , "lays down a stash the source no longer holds."
            ]
          }
        }
      , primaryOf Mutation.adopt_without_proof
      , primaryOf Mutation.adopt_ref_unequal
      , primaryOf Mutation.adopt_untyped
      , primaryOf Mutation.adopt_ledger_before_refs
      , primaryOf Mutation.adopt_r1_no_row
      , primaryOf Mutation.adopt_r2_not_adopted
      , primaryOf Mutation.adopt_no_recheck
      , primaryOf Mutation.adopt_t0_no_row
      , primaryOf Mutation.handoff_no_overlay
      , primaryOf Mutation.handoff_overlay_any_block
      , primaryOf Mutation.handoff_clear_unchecked
      , primaryOf Mutation.handoff_exchange_unchecked
      , primaryOf Mutation.handoff_no_intent
      , primaryOf Mutation.handoff_retake_unchecked
      , primaryOf Mutation.handoff_keep_racy_unchecked
      , primaryOf Mutation.handoff_strip_no_overlay
      , primaryOf Mutation.handoff_strip_unchecked
      ]

let primaryPositions =
      List/fold
        NegRow
        negRows
        (List Natural)
        ( \(n : NegRow) ->
          \(acc : List Natural) ->
            merge
              { verdict = [ mutationIndex n.mutation ] # acc
              , also = \(_ : { suffix : Text, property : Property }) -> acc
              }
              n.against
        )
        ([] : List Natural)

let _ =
        assert
      :     primaryPositions
        ===  range (List/length PrimaryRow primaryRowsByLabel)

let rows =
        [ budgetSelftest ]
      # positives
      # witnesses
      # findings
      # map NegRow Row neg negRows

-- Checks over the rows ------------------------------------------------------------------

let failProperty =
      \(r : Row) ->
        merge
          { pass = \(_ : Pass) -> [] : List Property
          , fail = \(p : Property) -> [ p ]
          , reach = \(_ : Witness) -> [] : List Property
          , simulate = \(_ : List Property) -> [] : List Property
          , inconclusive = [] : List Property
          }
          r.expect

let failing = concatMap Row Property failProperty rows

-- Every safety invariant but TypeOK, and every temporal property, is shown falsifiable.
let _ =
        assert
      :     all
              Property
              ( \(p : Property) ->
                      propertyEq p Property.TypeOK
                  ||  any Property (\(q : Property) -> propertyEq p q) failing
              )
              (safety # temporal)
        ===  True

-- Rendering -----------------------------------------------------------------------------

let checks =
      \(r : Row) ->
        merge
          { pass =
              \(p : Pass) ->
                { invariants = map Property Text showP p.invariants
                , properties = map Property Text showP p.properties
                }
          , fail =
              \(p : Property) ->
                if    isTemporal p
                then  { invariants = [ "TypeOK" ], properties = [ showP p ] }
                else  { invariants = [ "TypeOK", showP p ], properties = [] : List Text }
          , reach =
              \(w : Witness) ->
                { invariants = [ "TypeOK", showW w ], properties = [] : List Text }
          , simulate =
              \(ps : List Property) ->
                { invariants = map Property Text showP ps, properties = [] : List Text }
          , inconclusive = { invariants = [ "TypeOK" ], properties = [] : List Text }
          }
          r.expect

let cfgText =
      \(r : Row) ->
        let c = checks r

        in  unlines
              (   map Text Text (\(l : Text) -> "\\* ${l}") r.comment
                # [ provenance.cfg
                  , "SPECIFICATION ${showConstructor r.spec}"
                  , "CONSTANTS"
                  ]
                # map
                    { name : Text, pad : Text, value : Text }
                    Text
                    ( \(k : { name : Text, pad : Text, value : Text }) ->
                        "    ${k.name}${k.pad} = ${k.value}"
                    )
                    (constantLines r.constants)
                # map Text Text (\(i : Text) -> "INVARIANT ${i}") c.invariants
                # [ "INVARIANT WithinBudget" ]
                # map Text Text (\(p : Text) -> "PROPERTY ${p}") c.properties
              )

let neverColumn =
      \(never : List Action) ->
        let sorted = filter Action (\(a : Action) -> any Action (actionEq a) never) allActions

        in  if isEmpty Action sorted then "-" else join "," (map Action Text showA sorted)

let tsvRow =
      \(r : Row) ->
        join
          "\t"
          [ r.name
          , showConstructor r.expect
          , merge
              { pass = \(_ : Pass) -> "all"
              , fail = showP
              , reach = showW
              , simulate = \(_ : List Property) -> "all"
              , inconclusive = "WithinBudget"
              }
              r.expect
          , merge
              { pass = \(p : Pass) -> neverColumn p.never
              , fail = \(_ : Property) -> "*"
              , reach = \(_ : Witness) -> "*"
              , simulate = \(_ : List Property) -> "*"
              , inconclusive = "*"
              }
              r.expect
          , merge { None = "-", Some = \(f : Text) -> f } r.flags
          ]

let tsv =
      unlines
        (   [ "# EstateConverge.tla's TLC configs, checked by `just tla-check` in this order (README.md, \"EstateConverge\")."
            , provenance.tsv
            , "# Tab-separated columns, as configs.tsv's:"
            , "#   name            the config, MC_<name>.cfg"
            , "#   expect          pass | fail | reach | simulate | inconclusive"
            , "#   named-property  fail: the one property it must violate;"
            , "#                   reach: the Witness_ invariant it must violate;"
            , "#                   inconclusive: WithinBudget; otherwise all"
            , "#   never           pass: the actions coverage must show never enabled,"
            , "#                   comma-separated and sorted (- for none);"
            , "#                   * for any other row (not checked)"
            , "#   flags           extra TLC flags, one argv element per"
            , "#                   space-separated word (- for none)"
            , "name\texpect\tnamed-property\tnever\tflags"
            ]
          # map Row Text tsvRow rows
        )

-- Traceability (README.md, "EstateConverge") ------------------------------------------

{- A property's traceability: the SLOs it serves, the rulings it carries, the
   code symbols it is about (`definition` grounding: each a Rust definition
   in crates/, the convergence's on this branch since the merge of
   feat/formal-estate-converge-20261009 into feat/estate-converge-20261008,
   2026-10-09; none pending), and the property tests the design names
   (P80-P85; section 13) with the Rust rows that pin each fix.
-}
let InvariantRow =
      { tla : Property
      , slo : List S
      , ruling : List Text
      , codeSymbol : List Text
      , pending : List T.PendingSymbol
      , ptest : List Text
      }

let none = [] : List T.PendingSymbol

let invariants
    : List InvariantRow
    = [ { tla = Property.NoLocalWorkLost
        , slo = [ S.S4, S.Durability ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q18", "OI-1003-Q100", "OI-1003-Q102" ]
        , codeSymbol =
          [ "apply_item"
          , "restore_staged"
          , "restore_linked_staged"
          , "prove"
          , "Engine"
          , "publish_index"
          , "set_head"
          , "stash_apply"
          , "decide"
          , "checked_out_by"
          , "held_branches"
          , "next_ledger"
          , "installed_index"
          , "transact"
          , "HeadLocks"
          , "finish"
          , "empty_index_digest"
          , "LANDING_TEMPORARY"
          ]
        , pending = none
        , ptest =
          [ "P82"
          , "P84"
          , "P85"
          , "a_settle_never_moves_a_branch_checked_out_since_the_crash"
          , "a_transaction_never_moves_a_checked_out_or_held_branch"
          , "a_stash_settle_never_undoes_an_operators_drop"
          , "the_ledger_records_the_installed_index_not_a_re_read"
          , "estate_converge_linked_added_operator"
          ]
        }
      , { tla = Property.NoSourceWorkLost
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q2", "OI-1003-Q11" ]
        , codeSymbol =
          [ "import_verified", "import_staged", "capture_item", "decide", "plan" ]
        , pending = none
        , ptest = [ "P80", "P81", "P84" ]
        }
      , { tla = Property.TypedRefusal
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q1", "OI-1003-Q74", "OI-1003-Q75" ]
        , codeSymbol =
          [ "apply_item"
          , "BulkloadRefusal"
          , "interrupted"
          , "holds_unlanded"
          , "finish"
          , "head_of"
          , "Engine"
          , "update_refs"
          , "remove_stale_locks"
          , "stash_apply"
          ]
        , pending = none
        , ptest =
          [ "P82"
          , "an_unborn_head_refuses_modified"
          , "a_seat_parent_replaced_after_the_intent_refuses_interrupted"
          , "a_settle_re_plans_a_head_branch_a_put_back_ref_blocks"
          , "a_settle_removes_a_killed_childs_lock_and_keeps_anyone_elses"
          , "a_ref_write_syncs_the_directories_its_renames_changed"
          ]
        }
      , { tla = Property.NoReRead
        , slo = [ S.S3 ]
        , ruling = [ "OI-1003-Q6", "OI-1003-Q10", "R25" ]
        , codeSymbol =
          [ "apply_item", "journal_path", "stage_bundle", "pending", "settle_refs" ]
        , pending = none
        , ptest = [ "P83" ]
        }
      , { tla = Property.CrashAtomicity
        , slo = [ S.Durability, S.S4 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q102" ]
        , codeSymbol =
          [ "apply_item"
          , "WorkspaceIntent"
          , "RefIntent"
          , "LandingIntent"
          , "settle"
          , "probe_exchange"
          , "sweep_probes"
          , "remove_intent_head_locks"
          , "settle_repository"
          , "publish_lock"
          , "sweep_lock_temps"
          , "ref_files"
          , "Point"
          ]
        , pending = none
        , ptest =
          [ "P81"
          , "the estate_converge fault-harness rows"
          , "the_probe_sweep_removes_only_probe_files"
          , "probe_files_a_crashed_pass_left_are_removed"
          , "a_lock_bulkload_takes_holds_its_marker_from_the_moment_it_exists"
          , "a_stash_store_syncs_its_reflog"
          , "set_head_never_commits_head_without_its_branch"
          ]
        }
      , { tla = Property.OneCheckout
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q144" ]
        , codeSymbol =
          [ "set_head", "branch_line_fails", "HeadLocks", "checked_out_by", "head_plan", "settle" ]
        , pending = none
        , ptest =
          [ "set_head_never_puts_a_second_worktree_on_a_branch"
          , "estate_converge_head_branch_double_checkout"
          ]
        }
      , { tla = Property.NoDriftedLanding
        , slo = [ S.S5, S.S4 ]
        , ruling = [ "OI-1003-Q11", "OI-1003-Q152", "OI-1003-Q161", "R-N72" ]
        , codeSymbol =
          [ "apply_item", "capture_item", "mark_drift", "pass_drift", "stash_drift" ]
        , pending = none
        , ptest =
          [ "P87"
          , "stash_pushed_during_capture_is_drift_custody"
          , "deleting_the_drift_sidecar_never_makes_a_drifted_capture_a_hit"
          , "a_key_only_drifted_capture_is_a_coherent_snapshot_and_applies"
          ]
        }
      , { tla = Property.AdoptClaimsOnlyEqual
        , slo = [ S.S4, S.Durability ]
        , ruling = [ "OI-1003-Q171", "OI-1003-Q172", "OI-1003-Q144", "OI-1003-Q145" ]
        , codeSymbol =
          [ "prove_adoptable"
          , "Scratch"
          , "still"
          , "workspace"
          , "related_repository"
          , "foreign_shape"
          , "rekey_refusal"
          ]
        , pending = none
        , ptest =
          [ "P90"
          , "P91"
          , "P92"
          , "P97"
          , "P98"
          , "P99"
          , "P100"
          , "the_adopt_proof_names_each_differing_component"
          , "the_adopt_scratch_builds_the_index_a_landing_installs"
          , "adopt_rechecks_the_proof_under_the_lock"
          , "the_adopt_proof_refuses_a_seat_that_moved_while_it_was_read"
          , "adopt_rechecks_under_the_lock_that_no_ledger_appeared"
          , "a_rekey_never_takes_a_tree_from_an_item_of_the_same_plan"
          , "the estate_adopt fault-harness rows"
          ]
        }
      , { tla = Property.ForeignRefsUnowned
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q171", "OI-1003-Q172", "OI-1003-Q144" ]
        , codeSymbol = [ "decide", "mark", "Adopted", "stash_decision", "STASH_ADOPTED" ]
        , pending = none
        , ptest =
          [ "P90"
          , "P84"
          , "P101"
          , "the_stash_table_adopts_an_equal_destination_reflog"
          ]
        }
      , { tla = Property.HandoffOwnsOnlyItsBlock
        , slo = [ S.S5, S.S4, S.Durability ]
        , ruling = [ "OI-1003-Q183", "OI-1003-Q185", "OI-1003-Q190", "OI-1003-Q144", "OI-1003-Q149" ]
        , codeSymbol =
          [ "landed_sides"
          , "matching"
          , "holds"
          , "prove"
          , "HandoffLanded"
          , "HandoffIntent"
          , "render_workspace"
          , "exchange"
          , "lift"
          , "clear"
          , "after_apply"
          , "retaken"
          , "settle"
          , "HandoffStripped"
          , "carried_len"
          , "landed_match"
          , "classify_head"
          ]
        , pending = none
        , ptest =
          [ "H0"
          , "H3"
          , "H4"
          , "H5"
          , "H6"
          , "H7"
          , "H8"
          , "H17"
          , "holds_accepts_the_recorded_block_then_the_landed_blob_only"
          , "a_racy_record_print_is_proved_by_reading"
          , "the_exchange_is_undone_when_the_target_moved_after_the_read"
          , "render_then_clear_is_identity"
          , "H20"
          , "H21"
          , "H24"
          , "H25"
          , "H32"
          , "the estate_handoff fault-harness rows (rerender_over_edit, clear_noop_apply)"
          , "H40"
          , "H41"
          , "H42"
          , "H46"
          , "H47"
          , "H49"
          , "holds_accepts_a_recorded_removal_of_carried_blocks_only"
          , "the estate_handoff fault-harness rows (clear_carried_after_intent, clear_carried_after_exchange)"
          ]
        }
      , { tla = Property.ConvergesWhenQuiet
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q13" ]
        , codeSymbol =
          [ "apply_item", "capture_item", "converge", "head_plan", "left_branch" ]
        , pending = none
        , ptest = [ "P81", "P83", "the_branch_head_left_converges_with_it" ]
        }
      , { tla = Property.NoWedge
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q144" ]
        , codeSymbol =
          [ "apply_item"
          , "plan"
          , "transact"
          , "settle"
          , "finish"
          , "remove_stale_locks"
          , "remove_orphan_packed_new"
          , "recover_linked_landing"
          , "branch_line_fails"
          , "intent_branches"
          , "after_intent"
          , "remove_owned_lock"
          ]
        , pending = none
        , ptest =
          [ "P84"
          , "the estate_converge fault-harness rows"
          , "a_create_beside_a_delete_that_will_not_run_is_dropped"
          , "a_file_turned_directory_settles_after_an_interrupted_converge"
          , "a_pending_intents_head_branch_is_left_to_its_item"
          , "estate_converge_linked_added"
          , "an_empty_lock_is_never_removed_even_when_old"
          , "an_orphan_packed_refs_new_is_removed_once_no_lock_holds_it"
          , "a_create_beside_a_symbolic_ref_is_dropped"
          , "a_killed_set_heads_lock_beside_a_pending_ref_intent_settles"
          , "estate_converge_index_locked_fresh_lock"
          , "estate_converge_after_ref_intent_packed_new"
          ]
        }
      ]

let traced =
      \(p : Property) ->
        (isSafety p && propertyEq p Property.TypeOK == False) || isTemporal p

let _ =
        assert
      :     map InvariantRow Natural (\(r : InvariantRow) -> propertyIndex r.tla) invariants
        ===  map Property Natural propertyIndex (filter Property traced allProperties)

-- Outputs ---------------------------------------------------------------------------------

let constantNames =
      map
        { name : Text, pad : Text, value : Text }
        Text
        (\(k : { name : Text, pad : Text, value : Text }) -> k.name)
        (constantLines defaults)

let module = T.moduleEntry T.Module.EstateConverge

in  { files =
          [ { name = module.tsv, text = tsv } ]
        # map Row { name : Text, text : Text } (\(r : Row) -> { name = "${r.name}.cfg", text = cfgText r }) rows
    , grounding =
      { module = showConstructor module.module
      , spec = module.spec
      , tsv = module.tsv
      , operators =
            map Property Text showP allProperties
          # map Witness Text showW allWitnesses
          # map Action Text showA allActions
          # [ showConstructor T.Specification.Spec
            , showConstructor T.Specification.LiveSpec
            , "Init"
            , "Next"
            , "Protocol"
            , "Environment"
            , "InFlight"
            , "Run"
            , "Decide"
            , "Plan"
            , "StashDecide"
            , "HeadPlan"
            , "Proved"
            , "Converged"
            ]
      , constants = constantNames
      , mutations = map Mutation Text showM allMutations
      , codeSymbols =
          concatMap InvariantRow Text (\(r : InvariantRow) -> r.codeSymbol) invariants
      , symbolMatch = showConstructor module.symbols
      , pendingSymbols =
          concatMap
            InvariantRow
            Text
            ( \(r : InvariantRow) ->
                map
                  T.PendingSymbol
                  Text
                  (\(s : T.PendingSymbol) -> "${s.symbol} (${showConstructor s.lands})")
                  r.pending
            )
            invariants
      , labelSets = closed
      }
    , invariants
    }
