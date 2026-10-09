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
   only its Module entry and the Converge lane are shared.
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
      }

let Refusal = < MODIFIED | OCCUPIED | STALE | INTERRUPTED | UNTYPED >

let refusalTable =
      { MODIFIED = Refusal.MODIFIED
      , OCCUPIED = Refusal.OCCUPIED
      , STALE = Refusal.STALE
      , INTERRUPTED = Refusal.INTERRUPTED
      , UNTYPED = Refusal.UNTYPED
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
      , WithinBudget =
        { property = Property.WithinBudget
        , index = 6
        , class = T.PropertyClass.budget
        }
      , ConvergesWhenQuiet =
        { property = Property.ConvergesWhenQuiet
        , index = 7
        , class = T.PropertyClass.temporal
        }
      , NoWedge =
        { property = Property.NoWedge, index = 8, class = T.PropertyClass.temporal }
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
      < Witness_Converged | Witness_SettledAfterCrash | Witness_CapturedNotLanded >

let witnessTable =
      { Witness_Converged = Witness.Witness_Converged
      , Witness_SettledAfterCrash = Witness.Witness_SettledAfterCrash
      , Witness_CapturedNotLanded = Witness.Witness_CapturedNotLanded
      }

-- The 49 actions of EstateConverge.tla's Next, in the sorted order of the never column.
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
      | HeadSet
      | Import
      | IndexPublish
      | Journal
      | OpHead
      | OpIndex
      | OpRacy
      | OpRef
      | OpRestore
      | OpSeat
      | OpStash
      | OpTouch
      | OpUntracked
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
          , HeadSet = 12
          , Import = 13
          , IndexPublish = 14
          , Journal = 15
          , OpHead = 16
          , OpIndex = 17
          , OpRacy = 18
          , OpRef = 19
          , OpRestore = 20
          , OpSeat = 21
          , OpStash = 22
          , OpTouch = 23
          , OpUntracked = 24
          , ProbeExchange = 25
          , Quiet = 26
          , RefFinish = 27
          , RefLedger = 28
          , RefStash = 29
          , RefStep = 30
          , RefTxnDeletes = 31
          , RefTxnWrites = 32
          , SeatOp = 33
          , SeatVerify = 34
          , SrcCreate = 35
          , SrcDelete = 36
          , SrcMove = 37
          , SrcRename = 38
          , SrcSeat = 39
          , SrcStage = 40
          , SrcStashDrop = 41
          , SrcStashPush = 42
          , SrcSwitch = 43
          , SrcTypeChange = 44
          , SrcWorktree = 45
          , Tick = 46
          , WriteIntent = 47
          , WriteLanded = 48
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
      , HeadSet = Action.HeadSet
      , Import = Action.Import
      , IndexPublish = Action.IndexPublish
      , Journal = Action.Journal
      , OpHead = Action.OpHead
      , OpIndex = Action.OpIndex
      , OpRacy = Action.OpRacy
      , OpRef = Action.OpRef
      , OpRestore = Action.OpRestore
      , OpSeat = Action.OpSeat
      , OpStash = Action.OpStash
      , OpTouch = Action.OpTouch
      , OpUntracked = Action.OpUntracked
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
   No source switch: with one, a put-back ref wedges the settle
   (MC_ec_finding_settle_branch_conflict).
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
   back (ref edits put back are MC_ec_finding_restored_ref's).
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
          , head_branch_in_step = Property.NoWedge
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
          }
          m

-- Rows ---------------------------------------------------------------------------------

let Pass = { invariants : List Property, properties : List Property, never : List Action }

let Expect = < pass : Pass | fail : Property | reach : Witness | inconclusive >

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

let row =
      \(name : Text) ->
      \(expect : Expect) ->
      \(comment : List Text) ->
      \(constants : Constants) ->
          { name, expect, comment, constants, spec = T.Specification.Spec, flags = None Text }
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

        in  if isTemporal property then liveRow r else r

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
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.SeatVerify, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick , Action.ProbeExchange ])
          [ "Refs (Q144: remote-tracking refs follow the source; local branches"
          , "and tags deleted at the source are kept and reported): every class,"
          , "the foo -> foo/bar rename both ways, o/x -> o/x/y, HEAD switches and"
          , "a crash at any step."
          ]
          refs
      , row
          "MC_ec_refs_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Refs under operator edits (Q144: a ref the operator changed is kept"
          , "and reported, ref-modified-at-destination): the operator moves,"
          , "creates or deletes a ref (a checked-out one too), or puts a landed"
          , "value back, between the source's moves, deletes and renames; a"
          , "crash. Source switches with a put-back ref are"
          , "MC_ec_finding_settle_branch_conflict's."
          ]
          refsOp
      , row
          "MC_ec_seats"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpStash, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcWorktree ])
          [ "The checkout (Q144: content, not stat): seat edits, additions,"
          , "removals and a directory turned into a file at the source; operator"
          , "edits, touches, an untracked file in a landed directory and"
          , "`git add`; a crash at any step, settled by content."
          ]
          seats
      , row
          "MC_ec_stash"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick , Action.ProbeExchange ])
          [ "The stash (findings 4, 8): pushes and drops at the source, every"
          , "store a compare-and-swap; an operator push while the step runs, and"
          , "a crash. Operator drops are MC_ec_finding_stash_settle_revert's."
          ]
          stash
      , row
          "MC_ec_linked"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.Tick , Action.ProbeExchange ])
          [ "A main and a linked worktree in one repository (findings 1, 11,"
          , "review 2 finding 1): commits, switches, worktree remove and add,"
          , "each item's ref step leaving the other's checked-out branch alone,"
          , "a crash at any step."
          ]
          linked
      , row
          "MC_ec_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "Operator edits of a seat and of the index while an apply is in"
          , "flight (findings 3, 10): the exchange's displaced check and the"
          , "index compare-and-swap keep them."
          ]
          concurrent
      , row
          "MC_ec_racy"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
          [ "A racy write (design 4.2): the operator writes a seat in the"
          , "timestamp tick of bulkload's write, keeping its stat; the proof"
          , "reads a racy seat's content, so the edit refuses."
          ]
          racy
      , row
          "MC_ec_first"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcSeat, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree , Action.ProbeExchange ])
          [ "First landings (#215): the main from an empty destination, then its"
          , "linked worktree, natively, with no carry/* branch; then a converge."
          , "No crash: a crashed linked first landing is #216's residual"
          , "(MC_ec_finding_first_linked_crash)."
          ]
          first
      , liveRow
          ( row
              "MC_ec_live"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpRestore, Action.OpSeat, Action.OpStash, Action.OpUntracked, Action.SrcCreate, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
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
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick , Action.ProbeExchange ])
              [ "Liveness, a main and its linked worktree: commits on both"
              , "checkouts' branches, a crash. Switches are the findings'"
              , "(MC_ec_finding_old_head_branch, MC_ec_finding_detached_reattach)."
              ]
              liveLinked
          )
      , liveRow
          ( row
              "MC_ec_refs_rename_op"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
              [ "Review 3 finding 1: o/x -> o/x/y with the operator moving o/x"
              , "while the ref intent is pending (a crash). finish keeps o/x (its"
              , "delete will not run), so the create of o/x/y is dropped"
              , "(ref-name-conflict), never retried: no wedge."
              ]
              renameOp
          )
      , row
          "MC_ec_rebase"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 3 finding 2: a rebase or bisect detaches the main's HEAD and"
          , "holds its branch; every ref step, the linked worktree's included,"
          , "leaves the held branch (rebase-merge/head-name, BISECT_START)."
          ]
          rebase
      , row
          "MC_ec_index_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.OpHead, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 3 finding 3: `git add` while an apply is in flight, over two"
          , "converges: the ledger records the index publish_index installed, so"
          , "an add after the publish fails the next proof (MODIFIED)."
          ]
          indexConcurrent
      , row
          "MC_ec_head_op"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 3 findings 4 and 10: the operator switches HEAD (to a branch"
          , "or an unborn one) and moves or deletes refs, a checked-out branch"
          , "too, around a crashed converge: the settle never moves a branch"
          , "some worktree has checked out, and an unborn HEAD refuses typed."
          ]
          headOp
      , liveRow
          ( row
              "MC_ec_seat_parent"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.RefFinish, Action.RefLedger, Action.RefStash, Action.RefTxnDeletes, Action.RefTxnWrites, Action.SrcCreate, Action.SrcDelete, Action.SrcMove, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
              [ "Review 3 finding 5: the operator removes the landed directory or"
              , "replaces it with a file while a seat under it is pending; the"
              , "settle refuses GIT_CONVERGE_INTERRUPTED (never a path code), and"
              , "converges once the operator puts the directory back."
              ]
              seatParent
          )
      , row
          "MC_ec_head_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 3 findings 10, 11 and 14: the operator switches HEAD while an"
          , "apply that moves and switches is in flight: the ref step's"
          , "symref-verify lines and set_head's compare-and-swap keep it."
          ]
          headConcurrent
      , row
          "MC_ec_refs_concurrent"
          (pass [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.Crash, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 3 finding 11: a ref edit while the ref step is in flight:"
          , "the transaction's compare-and-swap keeps it."
          ]
          refsConcurrent
      , row
          "MC_ec_child_crash"
          (pass [ Action.ChildCrashHead, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpHead, Action.OpIndex, Action.OpRef, Action.OpRestore, Action.OpStash, Action.ProbeExchange, Action.SeatVerify, Action.SrcCreate, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree, Action.Tick ])
          [ "Review 3 finding 13: every safety invariant with crashes inside"
          , "Git's ref transactions (any partial commit, lock files left), no"
          , "HEAD switch and no stash (MC_ec_finding_set_head_partial and"
          , "MC_ec_finding_stash_lock fail on those)."
          ]
          childCrashRefs
      , liveRow
          ( row
              "MC_ec_live_ops"
              (passLive [ Action.ChildCrashHead, Action.ChildCrashTxn, Action.FirstAddWt, Action.FirstLandMain, Action.FirstLedgerWt, Action.FirstPlanWt, Action.FirstStepWt, Action.IndexPublish, Action.OpIndex, Action.OpRacy, Action.OpRef, Action.OpStash, Action.OpTouch, Action.OpUntracked, Action.SrcCreate, Action.SrcDelete, Action.SrcRename, Action.SrcStage, Action.SrcStashDrop, Action.SrcStashPush, Action.SrcSwitch, Action.SrcTypeChange, Action.SrcWorktree ])
              [ "Review 3 finding 12: liveness with seat edits and HEAD switches"
              , "the operator puts back, and a crash."
              ]
              liveOps
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
      ]

{- Findings against the code on feat/estate-converge-20261008: each row is
   the current code at a bound that reaches the defect, expected to fail its
   named property. README.md, "Findings (EstateConverge)".
-}
let findings =
      [ liveRow
          ( row
              "MC_ec_finding_file_to_dir_settle"
              (Expect.fail Property.NoWedge)
              [ "CODE FINDING (expected to fail): the source turns the landed"
              , "directory into a file and back. A file the source turns into a"
              , "directory converges as Remove, MkDir, Add (converge::delta). A crash"
              , "after the MkDir leaves the new directory at the seat; the settle's"
              , "Remove (Engine::remove, settling) finds it neither absent nor the"
              , "old file (`holds` on a directory is false) and refuses"
              , "GIT_CONVERGE_INTERRUPTED on every rerun, with no operator edit."
              ]
              (     defaults
                //  { Paths = [ Path.d, Path.d_f ]
                    , SrcKinds = [ SrcKind.dirfile, SrcKind.filedir ]
                    , MaxSrc = 2
                    , MaxCrashes = 1
                    }
              )
          )
      ,     row
              "MC_ec_finding_settle_stale_branch"
              (Expect.fail Property.CrashAtomicity)
              [ "CODE FINDING (expected to fail; found by simulation, as BFS needs"
              , "depth 25 past millions of states): the main's intent moves HEAD to"
              , "a branch it also moves (foo, 2 -> 5); a crash, then an operator"
              , "`git add`, makes the main's settle refuse once (publish_index); the"
              , "linked worktree's ref step, which skips only checked-out branches,"
              , "moves foo to its newer capture's value. The settle replays"
              , "set_head with the intent's compare-and-swap (foo 2 -> 5), which can"
              , "never hold again: once the operator puts the index back, the main"
              , "refuses GIT_CONVERGE_INTERRUPTED on every rerun."
              ]
              (     defaults
                //  { Items = [ Item.main, Item.wt ]
                    , Refs = [ Ref.main, Ref.dev, Ref.foo ]
                    , IndexOn = True
                    , SrcKinds = [ SrcKind.move, SrcKind.switch ]
                    , OpKinds = [ OpKind.index ]
                    , MaxSrc = 3
                    , MaxOp = 1
                    , MaxCrashes = 1
                    }
              )
        //  { flags = Some "-simulate num=200000 -depth 40" }
      , liveRow
          ( row
              "MC_ec_finding_old_head_branch"
              (Expect.fail Property.ConvergesWhenQuiet)
              [ "CODE FINDING (expected to fail; review 2 finding 1's stated limit):"
              , "a commit on the checkout's branch, then a switch to another. The"
              , "step skips the old HEAD branch (it is checked out until set_head),"
              , "the journal makes every later apply a no-op, and the branch stays"
              , "at its old value while the source holds still."
              ]
              (     defaults
                //  { Refs = [ Ref.main, Ref.foo ]
                    , SrcKinds = [ SrcKind.move, SrcKind.switch ]
                    , MaxSrc = 2
                    }
              )
          )
      , liveRow
          ( row
              "MC_ec_finding_detached_reattach"
              (Expect.fail Property.ConvergesWhenQuiet)
              [ "CODE FINDING (expected to fail; finding 11's re-attach): the source"
              , "moves its linked worktree off dev and the main onto dev. The main,"
              , "applied first, detaches (dev is the destination worktree's); the"
              , "worktree then leaves dev, but the main's journal makes every later"
              , "apply a no-op, so it never re-attaches while the source holds still."
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
              "MC_ec_finding_child_crash"
              (Expect.fail Property.NoWedge)
              [ "CODE FINDING (expected to fail): a crash inside Git's update-ref"
              , "child (power loss, a killed cgroup) leaves its lock file; nothing"
              , "in the converge removes a stale ref lock, so the settle's"
              , "compare-and-swap fails on every rerun (GIT_CONVERGE_INTERRUPTED)."
              , "set_head's transaction can also leave HEAD on a branch whose line"
              , "did not commit. The fault harness's _exit points never reach this."
              ]
              (     defaults
                //  { Refs = [ Ref.main, Ref.foo ]
                    , SrcKinds = [ SrcKind.move ]
                    , MaxSrc = 1
                    , MaxCrashes = 1
                    , ChildCrash = True
                    }
              )
          )
      , row
          "MC_ec_finding_stash_settle_revert"
          (Expect.fail Property.NoLocalWorkLost)
          [ "CODE FINDING (expected to fail): stash_apply continues from any"
          , "suffix of the new order, the empty reflog included. An operator's"
          , "`git stash drop` or `clear` while a stash op is pending (a crash, or"
          , "between the decision and the stores) is undone by re-storing the"
          , "entries. No bytes are lost (every entry is at provenance); the"
          , "operator's edit is."
          ]
          (     defaults
            //  { SrcKinds = [ SrcKind.stash ]
                , OpKinds = [ OpKind.stashdrop ]
                , MaxSrc = 2
                , MaxOp = 1
                , MaxCrashes = 1
                }
          )
      , liveRow
          ( row
              "MC_ec_finding_first_linked_crash"
              (Expect.fail Property.NoWedge)
              [ "CODE FINDING (expected to fail; #216's stated residual): a crash"
              , "between `worktree add` and the linked worktree's ledger leaves a"
              , "worktree without a ledger, which every rerun refuses"
              , "GIT_DESTINATION_OCCUPIED."
              ]
              (first // { Paths = [] : List Path, SrcKinds = [] : List SrcKind, MaxSrc = 0, MaxCrashes = 1 })
          )
      , row
          "MC_ec_finding_stash_lock"
          (Expect.fail Property.TypedRefusal)
          [ "CODE FINDING (expected to fail; review 3 finding 8): a crash inside"
          , "stash_apply's update-ref leaves refs/stash.lock. Every later store"
          , "fails, stash_apply returns false, and finish clears the stash row"
          , "and reports stash-modified-at-destination with no operator edit;"
          , "the capture's stash never lands and the operator's own `git stash`"
          , "fails on the lock."
          ]
          (     defaults
            //  { SrcKinds = [ SrcKind.stash ]
                , MaxSrc = 1
                , MaxCrashes = 1
                , ChildCrash = True
                }
          )
      , row
          "MC_ec_finding_set_head_partial"
          (Expect.fail Property.CrashAtomicity)
          [ "CODE FINDING (expected to fail; review 3 finding 13, the earlier"
          , "report's finding 3): set_head's one transaction can commit HEAD"
          , "without its branch line when the Git child dies, leaving HEAD on a"
          , "branch that is neither the intent's old nor its new value."
          ]
          (     defaults
            //  { Refs = [ Ref.main, Ref.foo ]
                , SrcKinds = [ SrcKind.move, SrcKind.switch ]
                , MaxSrc = 2
                , MaxCrashes = 1
                , ChildCrash = True
                }
          )
      , liveRow
          ( row
              "MC_ec_finding_restored_ref"
              (Expect.fail Property.ConvergesWhenQuiet)
              [ "CODE FINDING (expected to fail; review 3 finding 12): the source"
              , "moves foo; the operator moves it too, so the converge keeps it"
              , "(ref-modified-at-destination) and journals the capture. The"
              , "operator then puts foo back to its landed value: every edit is"
              , "undone, but A4's journal no-op never plans foo again, so it stays"
              , "behind the source while the source holds still (Q144 counts"
              , "content: a ref put back is unchanged)."
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
          "MC_ec_finding_settle_branch_conflict"
          (Expect.fail Property.TypedRefusal)
          [ "CODE FINDING (expected to fail; found by review 3 finding 6's"
          , "tighter TypedRefusal; the class of the earlier report's finding 2):"
          , "the source renames foo to foo/bar and switches to it; the operator"
          , "deletes foo, so the converge plans HEAD onto a new foo/bar. A crash"
          , "before set_head, then the operator puts foo back. The settle replays"
          , "the intent's `create foo/bar`, which Git refuses beside foo, so the"
          , "main refuses GIT_CONVERGE_INTERRUPTED on every rerun with every"
          , "operator edit undone."
          ]
          (     defaults
            //  { Refs = [ Ref.main, Ref.foo, Ref.foo_bar ]
                , SrcKinds = [ SrcKind.rename, SrcKind.switch ]
                , OpKinds = [ OpKind.ref, OpKind.restore ]
                , MaxSrc = 2
                , MaxOp = 2
                , MaxCrashes = 1
                }
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
            [ "keeps its compare-and-swap line (review 2 finding 1, the blocker)." ]
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
          { head = "CODE TODAY (review 3 finding 1): finish removes every delete's"
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
          { head = "CODE TODAY (review 3 finding 2): the skip set is `worktree list"
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
          { head = "CODE TODAY (review 3 finding 3): next_ledger re-reads the index"
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
          { head = "CODE TODAY (review 3 finding 4): head_of's `rev-parse --verify"
          , tail = [ "HEAD` fails on an unborn HEAD: GIT_CHILD_FAILED, not MODIFIED." ]
          }
        }
      , seat_parent_untyped =
        { mutation = Mutation.seat_parent_untyped
        , constants = seatParent // { OpKinds = [ OpKind.dir ], MaxOp = 1 }
        , comment =
          { head = "CODE TODAY (review 3 finding 5): Engine::run passes Seat::at's"
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
          { head = "CODE TODAY (review 3 finding 7): no directory sync after"
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
          { head = "CODE TODAY (review 3 finding 9): probe_exchange's files a crash"
          , tail = [ "leaves in the worktree root are never removed." ]
          }
        }
      , settle_moves_checked_out =
        { mutation = Mutation.settle_moves_checked_out
        , constants = headOp // { OpKinds = [ OpKind.head ], MaxOp = 1 }
        , comment =
          { head = "CODE TODAY (review 3 finding 10, the blocker): finish redoes an"
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
          { head = "CODE TODAY (review 3 finding 10): the ref step's transaction has"
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
              , inconclusive = "WithinBudget"
              }
              r.expect
          , merge
              { pass = \(p : Pass) -> neverColumn p.never
              , fail = \(_ : Property) -> "*"
              , reach = \(_ : Witness) -> "*"
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
            , "#   expect          pass | fail | reach | inconclusive"
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
   code symbols it is about that origin/main already has (`definition`
   grounding), the convergence symbols on feat/estate-converge-20261008
   (pending until that branch merges: lane Converge), and the property
   tests the design names (P80-P85, proposed; section 13).
-}
let InvariantRow =
      { tla : Property
      , slo : List S
      , ruling : List Text
      , codeSymbol : List Text
      , pending : List T.PendingSymbol
      , ptest : List Text
      }

let pending = \(symbol : Text) -> { symbol, lands = Lane.Converge }

let invariants
    : List InvariantRow
    = [ { tla = Property.NoLocalWorkLost
        , slo = [ S.S4, S.Durability ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q18", "OI-1003-Q100", "OI-1003-Q102" ]
        , codeSymbol = [ "apply_item", "restore_staged", "restore_linked_staged" ]
        , pending =
          [ pending "converge::prove"
          , pending "converge::Engine (replace, remove, sweep)"
          , pending "converge::publish_index"
          , pending "converge::set_head"
          , pending "native_refs::stash_apply"
          , pending "native_refs::decide"
          , pending "converge::checked_out_by (rebase and bisect heads)"
          , pending "converge::next_ledger (the installed index)"
          , pending "native_refs::transact (symref-verify)"
          , pending "native_refs::finish (checked-out ops)"
          ]
        , ptest = [ "P82", "P84", "P85" ]
        }
      , { tla = Property.NoSourceWorkLost
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q2", "OI-1003-Q11" ]
        , codeSymbol = [ "import_verified", "import_staged", "capture_item" ]
        , pending =
          [ pending "native_refs::decide (R7, tombstones)"
          , pending "native_refs::plan (ref-name-conflict)"
          ]
        , ptest = [ "P80", "P81", "P84" ]
        }
      , { tla = Property.TypedRefusal
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q1", "OI-1003-Q74", "OI-1003-Q75" ]
        , codeSymbol = [ "apply_item" ]
        , pending =
          [ pending "GIT_DESTINATION_MODIFIED"
          , pending "GIT_CONVERGE_INTERRUPTED"
          , pending "GIT_CONVERGE_STALE_CAPTURE"
          , pending "converge::holds_unlanded"
          , pending "native_refs::finish"
          , pending "converge::head_of (unborn HEAD)"
          , pending "converge::Engine::run (Seat::at after the intent)"
          , pending "native_refs::update_refs (directory sync)"
          ]
        , ptest = [ "P82" ]
        }
      , { tla = Property.NoReRead
        , slo = [ S.S3 ]
        , ruling = [ "OI-1003-Q6", "OI-1003-Q10", "R25" ]
        , codeSymbol = [ "apply_item", "journal_path", "stage_bundle" ]
        , pending = [ pending "converge::pending", pending "converge::settle_refs" ]
        , ptest = [ "P83" ]
        }
      , { tla = Property.CrashAtomicity
        , slo = [ S.Durability, S.S4 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q102" ]
        , codeSymbol = [ "apply_item" ]
        , pending =
          [ pending "landed::WorkspaceIntent"
          , pending "landed::RefIntent"
          , pending "converge::settle"
          , pending "native_refs::settle"
          , pending "estate.converge.* fault points"
          , pending "converge::probe_exchange (the sweep)"
          ]
        , ptest = [ "P81", "the estate_converge fault-harness rows" ]
        }
      , { tla = Property.ConvergesWhenQuiet
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q144", "OI-1003-Q13" ]
        , codeSymbol = [ "apply_item", "capture_item" ]
        , pending = [ pending "converge::converge", pending "converge::head_plan" ]
        , ptest = [ "P81", "P83" ]
        }
      , { tla = Property.NoWedge
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q144" ]
        , codeSymbol = [ "apply_item" ]
        , pending =
          [ pending "native_refs::plan (name conflicts)"
          , pending "native_refs::transact (deletes first)"
          , pending "converge::settle"
          , pending "native_refs::finish (the names after the step)"
          ]
        , ptest = [ "P84", "the estate_converge fault-harness rows" ]
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
