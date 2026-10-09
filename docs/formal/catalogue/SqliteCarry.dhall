{- SqliteCarry.tla's part of docs/formal's typed catalogue (#218,
   OI-1003-Q102's model first; OI-1003-Q146's superseding publish of a
   changed store's snapshot): every TLC config of the SQLite snapshot seat's
   model, the verdict of every mutation, and the traceability row of every
   property.

   Catalogue.dhall imports this file and renders it beside the other
   modules. Its run order is configs_sq.tsv, and its configs are
   MC_sq_*.cfg. Its types live here, not in Types.dhall: only its Module
   entry is shared. The same checks hold as for GitCarry: every mutation has
   a verdict and one primary row (total merges), and every safety invariant
   but TypeOK has a fail row and one traceability row.
-}
let T = ./Types.dhall

let Lib = ./Lib.dhall

let map = Lib.map

let concatMap = Lib.concatMap

let filter = Lib.filter

let any = Lib.any

let natEq = Lib.natEq

let isEmpty = Lib.isEmpty

let join = Lib.join

let unlines = Lib.unlines

let showBool = Lib.showBool

let range = Lib.range

let ordered = Lib.ordered

let S = T.Slo

-- Types -------------------------------------------------------------------------

{- Every rule break SqliteCarry.tla's `Mutations` set knows, except "none";
   tla-check requires the two sets to be equal.
-}
let M =
      < sqlite_raw_send
      | sqlite_key_main_only
      | sqlite_record_unsettled
      | sqlite_capture_record_main_only
      | wal_lock_exclusive
      | sqlite_unbounded_pinned
      | sqlite_as_root
      | sqlite_not_owner_opened
      | sqlite_publish_beside_sidecar
      | sqlite_supersede_no_recheck
      | sqlite_supersede_unowned
      | sqlite_supersede_unlink_rename
      | sidecar_covered_by_name
      | sqlite_header_refusal_in_snapshot_mode
      | sqlite_reuse_ignored
      >

-- SqliteCarry.tla's properties: its safety invariants and its budget.
let P =
      < TypeOK
      | SqliteNeverTorn
      | SqliteReuseSound
      | WalWriterNeverBlocked
      | S2_BackupLockBounded
      | SqliteRootRefusedUpFront
      | SqliteOwnerRefused
      | SqliteNoForeignSidecar
      | SqliteNoClobber
      | SqliteOldOrNewWhole
      | SidecarCoverageSound
      | SnapshotModeIgnoresV5Record
      | S3_UnchangedSqliteZero
      | WithinBudget
      >

-- Reachability witnesses: each holds until the bound explores its state.
let W = < Witness_Reuse | Witness_Restart | Witness_Superseded >

-- The 21 actions of SqliteCarry.tla's Next, in the sorted order of the never column.
let A =
      < BackupBegin
      | BackupEnd
      | BackupRefused
      | BackupStep
      | CommitRow
      | Crash
      | Decide
      | DestTouch
      | Exchange
      | Publish
      | RenameAfterUnlink
      | RunEnd
      | SidecarAppear
      | SidecarRemoved
      | SqlCheckpoint
      | SqlCommitRollback
      | SqlCommitWal
      | SqlWalReset
      | StepUnlock
      | WalkMain
      | WalkWal
      >

-- One value per CONSTANT of SqliteCarry.tla, rendered in this order.
let Constants =
      { Wal : Bool
      , SnapshotMode : Bool
      , AsRoot : Bool
      , OwnedByReader : Bool
      , BaseIsDb : Bool
      , V5Record : Bool
      , MaxCommits : Natural
      , MaxCheckpoints : Natural
      , MaxRuns : Natural
      , MaxCrashes : Natural
      , Pages : Natural
      , StepBudget : Natural
      , DestSidecars : Bool
      , DestTouches : Bool
      , Mutation : Optional M
      , BudgetSeconds : Natural
      }

let Pass = { invariants : List P, never : List A }

let Expect = < pass : Pass | fail : P | reach : W | inconclusive >

let Row =
      { name : Text, expect : Expect, comment : List Text, constants : Constants }

let PropertyClass = < safety | budget >

let propertyTable =
      { TypeOK = { property = P.TypeOK, index = 0, class = PropertyClass.safety }
      , SqliteNeverTorn =
        { property = P.SqliteNeverTorn, index = 1, class = PropertyClass.safety }
      , SqliteReuseSound =
        { property = P.SqliteReuseSound, index = 2, class = PropertyClass.safety }
      , WalWriterNeverBlocked =
        { property = P.WalWriterNeverBlocked
        , index = 3
        , class = PropertyClass.safety
        }
      , S2_BackupLockBounded =
        { property = P.S2_BackupLockBounded
        , index = 4
        , class = PropertyClass.safety
        }
      , SqliteRootRefusedUpFront =
        { property = P.SqliteRootRefusedUpFront
        , index = 5
        , class = PropertyClass.safety
        }
      , SqliteOwnerRefused =
        { property = P.SqliteOwnerRefused
        , index = 6
        , class = PropertyClass.safety
        }
      , SqliteNoForeignSidecar =
        { property = P.SqliteNoForeignSidecar
        , index = 7
        , class = PropertyClass.safety
        }
      , SqliteNoClobber =
        { property = P.SqliteNoClobber, index = 8, class = PropertyClass.safety }
      , SqliteOldOrNewWhole =
        { property = P.SqliteOldOrNewWhole
        , index = 9
        , class = PropertyClass.safety
        }
      , SidecarCoverageSound =
        { property = P.SidecarCoverageSound
        , index = 10
        , class = PropertyClass.safety
        }
      , SnapshotModeIgnoresV5Record =
        { property = P.SnapshotModeIgnoresV5Record
        , index = 11
        , class = PropertyClass.safety
        }
      , S3_UnchangedSqliteZero =
        { property = P.S3_UnchangedSqliteZero
        , index = 12
        , class = PropertyClass.safety
        }
      , WithinBudget =
        { property = P.WithinBudget, index = 13, class = PropertyClass.budget }
      }

let PropertyEntry = { property : P, index : Natural, class : PropertyClass }

let propertyIndex = \(p : P) -> (merge propertyTable p).index

let isSafety =
      \(p : P) ->
        merge { safety = True, budget = False } (merge propertyTable p).class

let actionTable =
      { BackupBegin = { action = A.BackupBegin, index = 0 }
      , BackupEnd = { action = A.BackupEnd, index = 1 }
      , BackupRefused = { action = A.BackupRefused, index = 2 }
      , BackupStep = { action = A.BackupStep, index = 3 }
      , CommitRow = { action = A.CommitRow, index = 4 }
      , Crash = { action = A.Crash, index = 5 }
      , Decide = { action = A.Decide, index = 6 }
      , DestTouch = { action = A.DestTouch, index = 7 }
      , Exchange = { action = A.Exchange, index = 8 }
      , Publish = { action = A.Publish, index = 9 }
      , RenameAfterUnlink = { action = A.RenameAfterUnlink, index = 10 }
      , RunEnd = { action = A.RunEnd, index = 11 }
      , SidecarAppear = { action = A.SidecarAppear, index = 12 }
      , SidecarRemoved = { action = A.SidecarRemoved, index = 13 }
      , SqlCheckpoint = { action = A.SqlCheckpoint, index = 14 }
      , SqlCommitRollback = { action = A.SqlCommitRollback, index = 15 }
      , SqlCommitWal = { action = A.SqlCommitWal, index = 16 }
      , SqlWalReset = { action = A.SqlWalReset, index = 17 }
      , StepUnlock = { action = A.StepUnlock, index = 18 }
      , WalkMain = { action = A.WalkMain, index = 19 }
      , WalkWal = { action = A.WalkWal, index = 20 }
      }

let ActionEntry = { action : A, index : Natural }

let actionIndex = \(a : A) -> (merge actionTable a).index

let witnessTable =
      { Witness_Reuse = W.Witness_Reuse
      , Witness_Restart = W.Witness_Restart
      , Witness_Superseded = W.Witness_Superseded
      }

let witnessSelf = \(w : W) -> merge witnessTable w

let mutationIndex =
      \(m : M) ->
        merge
          { sqlite_raw_send = 0
          , sqlite_key_main_only = 1
          , sqlite_record_unsettled = 2
          , sqlite_capture_record_main_only = 3
          , wal_lock_exclusive = 4
          , sqlite_unbounded_pinned = 5
          , sqlite_as_root = 6
          , sqlite_not_owner_opened = 7
          , sqlite_publish_beside_sidecar = 8
          , sqlite_supersede_no_recheck = 9
          , sqlite_supersede_unowned = 10
          , sqlite_supersede_unlink_rename = 11
          , sidecar_covered_by_name = 12
          , sqlite_header_refusal_in_snapshot_mode = 13
          , sqlite_reuse_ignored = 14
          }
          m

let showP = \(p : P) -> showConstructor p

let showA = \(a : A) -> showConstructor a

let showM = \(m : M) -> showConstructor m

let showW = \(w : W) -> showConstructor w

let propertyEq = \(p : P) -> \(q : P) -> natEq (propertyIndex p) (propertyIndex q)

let actionEq = \(a : A) -> \(b : A) -> natEq (actionIndex a) (actionIndex b)

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
        P
        propertyIndex
        ( map
            PropertyRow
            P
            (\(e : PropertyRow) -> e.mapValue.property)
            propertyRows
        )

let _ =
        assert
      :     map P Natural propertyIndex allProperties
        ===  range (List/length PropertyRow propertyRows)

let ActionRow = { mapKey : Text, mapValue : ActionEntry }

let actionRows = toMap actionTable

let _ =
        assert
      :     map ActionRow Text (\(e : ActionRow) -> e.mapKey) actionRows
        ===  map
               ActionRow
               Text
               (\(e : ActionRow) -> showA e.mapValue.action)
               actionRows

let allActions =
      ordered
        A
        actionIndex
        (map ActionRow A (\(e : ActionRow) -> e.mapValue.action) actionRows)

let _ =
        assert
      :     map A Natural actionIndex allActions
        ===  range (List/length ActionRow actionRows)

let WitnessRow = { mapKey : Text, mapValue : W }

let witnessRows = toMap witnessTable

let _ =
        assert
      :     map WitnessRow Text (\(e : WitnessRow) -> e.mapKey) witnessRows
        ===  map WitnessRow Text (\(e : WitnessRow) -> showW e.mapValue) witnessRows

let allWitnesses =
      map WitnessRow W (\(e : WitnessRow) -> witnessSelf e.mapValue) witnessRows

let allMutations =
      ordered
        M
        mutationIndex
        [ M.sqlite_raw_send
        , M.sqlite_key_main_only
        , M.sqlite_record_unsettled
        , M.sqlite_capture_record_main_only
        , M.wal_lock_exclusive
        , M.sqlite_unbounded_pinned
        , M.sqlite_as_root
        , M.sqlite_not_owner_opened
        , M.sqlite_publish_beside_sidecar
        , M.sqlite_supersede_no_recheck
        , M.sqlite_supersede_unowned
        , M.sqlite_supersede_unlink_rename
        , M.sidecar_covered_by_name
        , M.sqlite_header_refusal_in_snapshot_mode
        , M.sqlite_reuse_ignored
        ]

let _ = assert : map M Natural mutationIndex allMutations === range 15

-- Constants -----------------------------------------------------------------------

let constantLines =
      \(c : Constants) ->
        let mutation = merge { None = "none", Some = \(m : M) -> showM m } c.Mutation

        in  [ { name = "Wal", pad = "           ", value = showBool c.Wal }
            , { name = "SnapshotMode", pad = "  ", value = showBool c.SnapshotMode }
            , { name = "AsRoot", pad = "        ", value = showBool c.AsRoot }
            , { name = "OwnedByReader"
              , pad = " "
              , value = showBool c.OwnedByReader
              }
            , { name = "BaseIsDb", pad = "      ", value = showBool c.BaseIsDb }
            , { name = "V5Record", pad = "      ", value = showBool c.V5Record }
            , { name = "MaxCommits"
              , pad = "    "
              , value = Natural/show c.MaxCommits
              }
            , { name = "MaxCheckpoints"
              , pad = ""
              , value = Natural/show c.MaxCheckpoints
              }
            , { name = "MaxRuns", pad = "       ", value = Natural/show c.MaxRuns }
            , { name = "MaxCrashes"
              , pad = "    "
              , value = Natural/show c.MaxCrashes
              }
            , { name = "Pages", pad = "         ", value = Natural/show c.Pages }
            , { name = "StepBudget"
              , pad = "    "
              , value = Natural/show c.StepBudget
              }
            , { name = "DestSidecars", pad = "  ", value = showBool c.DestSidecars }
            , { name = "DestTouches", pad = "   ", value = showBool c.DestTouches }
            , { name = "Mutation", pad = "      ", value = "\"${mutation}\"" }
            , { name = "BudgetSeconds"
              , pad = " "
              , value = Natural/show c.BudgetSeconds
              }
            ]

{- WAL mode, the code today (OI-1003-Q146): a live writer (two commits), a
   checkpoint and its WAL reset, three runs and a crash anywhere, a third
   party that may drop a sidecar beside the published output or write the
   output itself. One page run per step, two steps a copy, a budget of four
   (one restart).
-}
let defaults
    : Constants
    = { Wal = True
      , SnapshotMode = True
      , AsRoot = False
      , OwnedByReader = True
      , BaseIsDb = True
      , V5Record = False
      , MaxCommits = 2
      , MaxCheckpoints = 1
      , MaxRuns = 3
      , MaxCrashes = 1
      , Pages = 2
      , StepBudget = 4
      , DestSidecars = True
      , DestTouches = True
      , Mutation = None M
      , BudgetSeconds = 1800
      }

-- The smaller bound most rows use: no third-party write of the output.
let untouched = defaults // { DestTouches = False }

let rollback = defaults // { Wal = False, MaxCheckpoints = 0 }

-- Properties -----------------------------------------------------------------------

let safety = filter P isSafety allProperties

{- The verdict of every mutation: the one property its MC_sq_neg_<mutation>
   config must violate. A total merge.
-}
let verdict
    : M -> P
    = \(m : M) ->
        merge
          { sqlite_raw_send = P.SqliteNeverTorn
          , sqlite_key_main_only = P.SqliteReuseSound
          , sqlite_record_unsettled = P.SqliteReuseSound
          , sqlite_capture_record_main_only = P.SqliteReuseSound
          , wal_lock_exclusive = P.WalWriterNeverBlocked
          , sqlite_unbounded_pinned = P.S2_BackupLockBounded
          , sqlite_as_root = P.SqliteRootRefusedUpFront
          , sqlite_not_owner_opened = P.SqliteOwnerRefused
          , sqlite_publish_beside_sidecar = P.SqliteNoForeignSidecar
          , sqlite_supersede_no_recheck = P.SqliteNoForeignSidecar
          , sqlite_supersede_unowned = P.SqliteNoClobber
          , sqlite_supersede_unlink_rename = P.SqliteOldOrNewWhole
          , sidecar_covered_by_name = P.SidecarCoverageSound
          , sqlite_header_refusal_in_snapshot_mode = P.SnapshotModeIgnoresV5Record
          , sqlite_reuse_ignored = P.S3_UnchangedSqliteZero
          }
          m

-- Row constructors -----------------------------------------------------------------

let row =
      \(name : Text) ->
      \(expect : Expect) ->
      \(comment : List Text) ->
      \(constants : Constants) ->
        { name, expect, comment, constants } : Row

-- A pass row checks every safety invariant; `never` is its never column.
let pass = \(never : List A) -> Expect.pass { invariants = safety, never }

{- Never enabled in every pass row: the unlink-then-rename mutation's second
   step.
-}
let noMutation = [ A.RenameAfterUnlink ]

let neg =
      \(m : M) ->
      \(head : Text) ->
      \(constants : Constants) ->
        let property = verdict m

        in  row
              "MC_sq_neg_${showM m}"
              (Expect.fail property)
              [ "NEGATIVE: ${head}"
              , "Mutation ${showM m} must violate ${showP property}."
              ]
              (constants // { Mutation = Some m })

-- The configs, in run order ---------------------------------------------------------

let budgetSelftest =
      row
        "MC_sq_budget_selftest"
        Expect.inconclusive
        [ "BUDGET SELF-TEST (expected INCONCLUSIVE). MC_sq_wal's constants with"
        , "a 5 s budget: the search needs far longer, so WithinBudget, and"
        , "nothing else, must trip, which proves SqliteCarry.tla's budget is"
        , "evaluated per state. The bound is MC_sq_wal's, so a broken budget"
        , "still ends (as a PASS, which tla-check rejects)."
        ]
        (defaults // { BudgetSeconds = 5 })

let positives =
      [ row
          "MC_sq_wal"
          (pass (noMutation # [ A.BackupRefused, A.SqlCommitRollback ]))
          [ "WAL mode, the code today: a writer, a checkpointer, crashes, a"
          , "destination sidecar and a third-party write of the output at any"
          , "step. A changed store's snapshot supersedes its own untouched"
          , "output through the exchange (OI-1003-Q146)."
          ]
          defaults
      , row
          "MC_sq_rollback"
          ( pass
              (   noMutation
                # [ A.BackupRefused, A.SqlCheckpoint, A.SqlCommitWal, A.SqlWalReset ]
              )
          )
          [ "Rollback (DELETE) mode: commits wait for a step's SHARED, never"
          , "longer; supersede, sidecars and third-party writes as MC_sq_wal."
          ]
          rollback
      , row
          "MC_sq_root"
          ( pass
              (   noMutation
                # [ A.BackupBegin
                  , A.BackupEnd
                  , A.BackupRefused
                  , A.BackupStep
                  , A.CommitRow
                  , A.Crash
                  , A.Decide
                  , A.DestTouch
                  , A.Exchange
                  , A.Publish
                  , A.SidecarAppear
                  , A.SidecarRemoved
                  , A.SqlCommitRollback
                  , A.StepUnlock
                  , A.WalkWal
                  ]
              )
          )
          [ "As root: the snapshot-mode session opens nothing (OI-1003-Q76)." ]
          (untouched // { AsRoot = True })
      , row
          "MC_sq_not_owner"
          ( pass
              (   noMutation
                # [ A.BackupBegin
                  , A.BackupEnd
                  , A.BackupRefused
                  , A.BackupStep
                  , A.CommitRow
                  , A.DestTouch
                  , A.Exchange
                  , A.Publish
                  , A.SidecarAppear
                  , A.SidecarRemoved
                  , A.SqlCommitRollback
                  , A.StepUnlock
                  ]
              )
          )
          [ "Another user's database: never opened (R4)." ]
          (untouched // { OwnedByReader = False })
      , row
          "MC_sq_raw_base"
          ( pass
              (   noMutation
                # [ A.BackupBegin
                  , A.BackupEnd
                  , A.BackupRefused
                  , A.BackupStep
                  , A.CommitRow
                  , A.DestTouch
                  , A.Exchange
                  , A.Publish
                  , A.SidecarAppear
                  , A.SidecarRemoved
                  , A.SqlCommitRollback
                  , A.StepUnlock
                  ]
              )
          )
          [ "A base without the magic: its sidecar is never covered (R2)." ]
          (untouched // { BaseIsDb = False })
      , row
          "MC_sq_v5_record"
          ( pass
              (noMutation # [ A.BackupRefused, A.DestTouch, A.SqlCommitRollback ])
          )
          [ "A refuse-mode record: ignored in snapshot mode (R8)." ]
          (untouched // { V5Record = True })
      , row
          "MC_sq_reach_reuse"
          (Expect.reach W.Witness_Reuse)
          [ "Reached: a WAL store reused from its row." ]
          untouched
      , row
          "MC_sq_reach_restart"
          (Expect.reach W.Witness_Restart)
          [ "Reached: a backup restarted by a commit between steps." ]
          untouched
      , row
          "MC_sq_reach_superseded"
          (Expect.reach W.Witness_Superseded)
          [ "Reached (OI-1003-Q146): a changed store's snapshot superseded its"
          , "own older output through the exchange, and the row committed."
          ]
          untouched
      ]

let NegRow = { mutation : M, head : Text, constants : Constants }

let negRows
    : List NegRow
    = [ { mutation = M.sqlite_raw_send
        , head = "the live file is sent."
        , constants = untouched
        }
      , { mutation = M.sqlite_key_main_only
        , head = "the key ignores the -wal."
        , constants = untouched
        }
      , { mutation = M.sqlite_record_unsettled
        , head = "an unsettled capture keeps a reuse row."
        , constants = untouched
        }
      , { mutation = M.sqlite_capture_record_main_only
        , head = "the capture record ignores the -wal (R1)."
        , constants = untouched
        }
      , { mutation = M.wal_lock_exclusive
        , head = "a WAL backup blocks the writer."
        , constants = untouched
        }
      , { mutation = M.sqlite_unbounded_pinned
        , head = "the lock is held across steps."
        , constants = untouched
        }
      , { mutation = M.sqlite_as_root
        , head = "no root refusal."
        , constants = untouched // { AsRoot = True }
        }
      , { mutation = M.sqlite_not_owner_opened
        , head = "no owner refusal (R4)."
        , constants = untouched // { OwnedByReader = False }
        }
      , { mutation = M.sqlite_publish_beside_sidecar
        , head = "no destination sidecar check at all (R5)."
        , constants = untouched
        }
      , { mutation = M.sqlite_supersede_no_recheck
        , head = "no sidecar look just before the exchange (OI-1003-Q146)."
        , constants = untouched
        }
      , { mutation = M.sqlite_supersede_unowned
        , head = "a supersede without the ownership proof (OI-1003-Q146)."
        , constants = defaults
        }
      , { mutation = M.sqlite_supersede_unlink_rename
        , head = "remove the old output, then rename the new (OI-1003-Q146)."
        , constants = untouched
        }
      , { mutation = M.sidecar_covered_by_name
        , head = "a sidecar is covered by its name (R2)."
        , constants = untouched // { BaseIsDb = False }
        }
      , { mutation = M.sqlite_header_refusal_in_snapshot_mode
        , head = "the v5 record is honoured (R8)."
        , constants = untouched // { V5Record = True }
        }
      , { mutation = M.sqlite_reuse_ignored
        , head = "an unchanged store is read again."
        , constants = untouched
        }
      ]

-- One primary row per mutation, in the Mutations order.
let _ =
        assert
      :     map NegRow Natural (\(n : NegRow) -> mutationIndex n.mutation) negRows
        ===  range 15

-- Every safety invariant but TypeOK is some mutation's verdict.
let _ =
        assert
      :     map
              P
              Bool
              ( \(p : P) ->
                      propertyEq p P.TypeOK
                  ||  any NegRow (\(n : NegRow) -> propertyEq (verdict n.mutation) p) negRows
              )
              safety
        ===  map P Bool (\(_ : P) -> True) safety

let rows =
        [ budgetSelftest ]
      # positives
      # map
          NegRow
          Row
          (\(n : NegRow) -> neg n.mutation n.head n.constants)
          negRows

-- Rendering -------------------------------------------------------------------------

let provenance =
      { cfg =
          "\\* Rendered from catalogue/SqliteCarry.dhall; expected outcome in configs_sq.tsv."
      , tsv =
          "# Rendered from catalogue/SqliteCarry.dhall by `just tla-render`; edit the catalogue, not this file."
      }

let checks =
      \(r : Row) ->
        merge
          { pass = \(p : Pass) -> map P Text showP p.invariants
          , fail = \(p : P) -> [ "TypeOK", showP p ]
          , reach = \(w : W) -> [ "TypeOK", showW w ]
          , inconclusive = [ "TypeOK" ]
          }
          r.expect

let cfgText =
      \(r : Row) ->
        unlines
          (   map Text Text (\(l : Text) -> "\\* ${l}") r.comment
            # [ provenance.cfg, "SPECIFICATION Spec", "CONSTANTS" ]
            # map
                { name : Text, pad : Text, value : Text }
                Text
                ( \(k : { name : Text, pad : Text, value : Text }) ->
                    "    ${k.name}${k.pad} = ${k.value}"
                )
                (constantLines r.constants)
            # map Text Text (\(i : Text) -> "INVARIANT ${i}") (checks r)
            # [ "INVARIANT WithinBudget", "CHECK_DEADLOCK FALSE" ]
          )

let neverColumn =
      \(never : List A) ->
        let sorted = filter A (\(a : A) -> any A (actionEq a) never) allActions

        in  if isEmpty A sorted then "-" else join "," (map A Text showA sorted)

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
              , fail = \(_ : P) -> "*"
              , reach = \(_ : W) -> "*"
              , inconclusive = "*"
              }
              r.expect
          , "-"
          ]

let tsv =
      unlines
        (   [ "# SqliteCarry.tla's TLC configs, checked by `just tla-check` in this order (README.md, \"SqliteCarry\")."
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
            , "#   flags           extra TLC flags (- for none)"
            , "name\texpect\tnamed-property\tnever\tflags"
            ]
          # map Row Text tsvRow rows
        )

-- Traceability (README.md, "SqliteCarry") ----------------------------------------------

let InvariantRow =
      { tla : P, slo : List T.Slo, ruling : List Text, codeSymbol : List Text }

let invariants
    : List InvariantRow
    = [ { tla = P.SqliteNeverTorn
        , slo = [ S.S2 ]
        , ruling = [ "#218 G1", "OI-1003-Q16" ]
        , codeSymbol = [ "snapshot_for_carry", "verify_received" ]
        }
      , { tla = P.SqliteReuseSound
        , slo = [ S.S3 ]
        , ruling = [ "R25", "#218 R1" ]
        , codeSymbol = [ "sqlite_key", "sqlite_record_key", "wal_settled" ]
        }
      , { tla = P.WalWriterNeverBlocked
        , slo = [ S.S2 ]
        , ruling = [ "OI-1003-Q16", "#218 R3" ]
        , codeSymbol = [ "backup_into", "step_all" ]
        }
      , { tla = P.S2_BackupLockBounded
        , slo = [ S.S2 ]
        , ruling = [ "OI-1003-Q16", "#218 D2" ]
        , codeSymbol = [ "step_all", "CARRY_WALL" ]
        }
      , { tla = P.SqliteRootRefusedUpFront
        , slo = [ S.S2 ]
        , ruling = [ "OI-1003-Q76" ]
        , codeSymbol = [ "read_open", "SqliteSourceAsRoot" ]
        }
      , { tla = P.SqliteOwnerRefused
        , slo = [ S.S2 ]
        , ruling = [ "#218 R4" ]
        , codeSymbol = [ "SqliteSourceNotOwner" ]
        }
      , { tla = P.SqliteNoForeignSidecar
        , slo = [ S.Durability ]
        , ruling = [ "#218 R5", "OI-1003-Q146" ]
        , codeSymbol = [ "sqlite_sidecars_present", "sqlite_sidecar_appeared" ]
        }
      , { tla = P.SqliteNoClobber
        , slo = [ S.Durability ]
        , ruling = [ "OI-1003-Q18", "OI-1003-Q146" ]
        , codeSymbol = [ "owned_output", "prepare_supersede" ]
        }
      , { tla = P.SqliteOldOrNewWhole
        , slo = [ S.Durability ]
        , ruling = [ "OI-1003-Q18", "OI-1003-Q146" ]
        , codeSymbol = [ "SupersedeIntent", "Exchanged" ]
        }
      , { tla = P.SidecarCoverageSound
        , slo = [ S.S4 ]
        , ruling = [ "#218 R2" ]
        , codeSymbol = [ "resolve_sidecars" ]
        }
      , { tla = P.SnapshotModeIgnoresV5Record
        , slo = [ S.S4 ]
        , ruling = [ "#218 R8" ]
        , codeSymbol = [ "remembered_refusal", "SqliteWalHeader" ]
        }
      , { tla = P.S3_UnchangedSqliteZero
        , slo = [ S.S3 ]
        , ruling = [ "R25", "OI-1003-Q40" ]
        , codeSymbol = [ "output_matches", "sqlite_key" ]
        }
      ]

-- One traceability row per safety invariant but TypeOK, in table order.
let _ =
        assert
      :     map InvariantRow Natural (\(r : InvariantRow) -> propertyIndex r.tla) invariants
        ===  map
               P
               Natural
               propertyIndex
               (filter P (\(p : P) -> propertyEq p P.TypeOK == False) safety)

-- Outputs -----------------------------------------------------------------------------

let constantNames =
      map
        { name : Text, pad : Text, value : Text }
        Text
        (\(k : { name : Text, pad : Text, value : Text }) -> k.name)
        (constantLines defaults)

let module = T.moduleEntry T.Module.SqliteCarry

in  { files =
          [ { name = module.tsv, text = tsv } ]
        # map
            Row
            { name : Text, text : Text }
            (\(r : Row) -> { name = "${r.name}.cfg", text = cfgText r })
            rows
    , grounding =
      { module = showConstructor module.module
      , spec = module.spec
      , tsv = module.tsv
      , operators =
            map P Text showP allProperties
          # map W Text showW allWitnesses
          # map A Text showA allActions
          # [ "Spec", "Init", "Next" ]
      , constants = constantNames
      , mutations = map M Text showM allMutations
      , codeSymbols =
          concatMap
            InvariantRow
            Text
            (\(r : InvariantRow) -> r.codeSymbol)
            invariants
      , symbolMatch = showConstructor module.symbols
      , pendingSymbols = [] : List Text
      , labelSets = [] : List { name : Text, labels : List Text }
      }
    , invariants =
        map
          InvariantRow
          { tla : Text, slo : List Text, ruling : List Text, codeSymbol : List Text }
          ( \(r : InvariantRow) ->
              { tla = showP r.tla
              , slo = map T.Slo Text (\(s : T.Slo) -> showConstructor s) r.slo
              , ruling = r.ruling
              , codeSymbol = r.codeSymbol
              }
          )
          invariants
    }
