{- DirectoryRecords.tla's part of docs/formal's typed catalogue
   (OI-1003-Q143 item 2, #217 review): every TLC config of the directory
   record model, the verdict of every mutation and the traceability row of
   every property.

   Catalogue.dhall imports this file and renders it beside the other
   modules. Its run order is configs_dir.tsv, and its configs are
   MC_dir_*.cfg. The same checks hold as for GitCarry.dhall: every mutation
   has a verdict and one primary row (total merges, asserted positions),
   every safety invariant but TypeOK has a fail row, and every safety
   property but TypeOK has one traceability row, in table order. Its unions
   live here: no other module uses them.
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

let provenance =
      { cfg =
          "\\* Rendered from catalogue/Directories.dhall; expected outcome in configs_dir.tsv."
      , tsv =
          "# Rendered from catalogue/Directories.dhall by `just tla-render`; edit the catalogue, not this file."
      }

-- The module's unions ----------------------------------------------------------

{- Every rule break DirectoryRecords.tla's `Mutations` set knows, except
   "none"; tla-check requires the two to agree.
-}
let M =
      < record_before_seal
      | rename_before_record
      | child_before_dirseal
      | complete_before_seal
      | level_seals_one_parent
      | level_rename_seals_one_parent
      | adopt_unbound
      | discard_unlink_before_clear
      | admit_under_held
      >

let mutationIndex =
      \(m : M) ->
        merge
          { record_before_seal = 0
          , rename_before_record = 1
          , child_before_dirseal = 2
          , complete_before_seal = 3
          , level_seals_one_parent = 4
          , level_rename_seals_one_parent = 5
          , adopt_unbound = 6
          , discard_unlink_before_clear = 7
          , admit_under_held = 8
          }
          m

-- The properties: the safety invariants and the budget.
let P =
      < TypeOK
      | RecordNamesDirectory
      | NoStrandedDirectory
      | CompleteImpliesFinal
      | ChildImpliesNamed
      | NeverAdoptForeign
      | NoDirectoryUnderTemporary
      | WithinBudget
      >

let PropertyEntry = { property : P, index : Natural, class : T.PropertyClass }

let propertyTable =
      { TypeOK =
        { property = P.TypeOK, index = 0, class = T.PropertyClass.safety }
      , RecordNamesDirectory =
        { property = P.RecordNamesDirectory
        , index = 1
        , class = T.PropertyClass.safety
        }
      , NoStrandedDirectory =
        { property = P.NoStrandedDirectory
        , index = 2
        , class = T.PropertyClass.safety
        }
      , CompleteImpliesFinal =
        { property = P.CompleteImpliesFinal
        , index = 3
        , class = T.PropertyClass.safety
        }
      , ChildImpliesNamed =
        { property = P.ChildImpliesNamed
        , index = 4
        , class = T.PropertyClass.safety
        }
      , NeverAdoptForeign =
        { property = P.NeverAdoptForeign
        , index = 5
        , class = T.PropertyClass.safety
        }
      , NoDirectoryUnderTemporary =
        { property = P.NoDirectoryUnderTemporary
        , index = 6
        , class = T.PropertyClass.safety
        }
      , WithinBudget =
        { property = P.WithinBudget, index = 7, class = T.PropertyClass.budget }
      }

let propertyIndex = \(p : P) -> (merge propertyTable p).index

let isSafety =
      \(p : P) ->
        merge
          { safety = True, budget = False, finding = False, temporal = False }
          (merge propertyTable p).class

-- The reachability witnesses.
let W = < Witness_PartialLevel | Witness_AdoptBatched >

let witnessTable =
      { Witness_PartialLevel = W.Witness_PartialLevel
      , Witness_AdoptBatched = W.Witness_AdoptBatched
      }

let witnessSelf = \(w : W) -> merge witnessTable w

-- The 22 actions of Next, in the sorted order of the never column.
let A =
      < Bind
      | ChildCommit
      | Chmod
      | Complete
      | DecideExisting
      | DiscardClear
      | DiscardUnlink
      | EnvSeal
      | Exit
      | FallbackMkdir
      | Foreign
      | ForeignReplace
      | Intent
      | Mkdir
      | PowerLoss
      | Record
      | Rename
      | SealDir
      | SealParent
      | StartRun
      | Stop
      | Terminated
      >

let actionTable =
      { Bind = { action = A.Bind, index = 0 }
      , ChildCommit = { action = A.ChildCommit, index = 1 }
      , Chmod = { action = A.Chmod, index = 2 }
      , Complete = { action = A.Complete, index = 3 }
      , DecideExisting = { action = A.DecideExisting, index = 4 }
      , DiscardClear = { action = A.DiscardClear, index = 5 }
      , DiscardUnlink = { action = A.DiscardUnlink, index = 6 }
      , EnvSeal = { action = A.EnvSeal, index = 7 }
      , Exit = { action = A.Exit, index = 8 }
      , FallbackMkdir = { action = A.FallbackMkdir, index = 9 }
      , Foreign = { action = A.Foreign, index = 10 }
      , ForeignReplace = { action = A.ForeignReplace, index = 11 }
      , Intent = { action = A.Intent, index = 12 }
      , Mkdir = { action = A.Mkdir, index = 13 }
      , PowerLoss = { action = A.PowerLoss, index = 14 }
      , Record = { action = A.Record, index = 15 }
      , Rename = { action = A.Rename, index = 16 }
      , SealDir = { action = A.SealDir, index = 17 }
      , SealParent = { action = A.SealParent, index = 18 }
      , StartRun = { action = A.StartRun, index = 19 }
      , Stop = { action = A.Stop, index = 20 }
      , Terminated = { action = A.Terminated, index = 21 }
      }

let actionIndex = \(a : A) -> (merge actionTable a).index

-- Inode numbers: model values, under SYMMETRY InoSymmetry.
let Ino = < i1 | i2 | i3 | i4 >

-- One value per CONSTANT of DirectoryRecords.tla, rendered in this order.
let Constants =
      { Inos : List Ino
      , Deep : Bool
      , BatchCreate : Bool
      , BatchFinish : Bool
      , NoReplaceRename : Bool
      , MaxRuns : Natural
      , MaxCrashes : Natural
      , MaxForeign : Natural
      , Mutation : Optional M
      , BudgetSeconds : Natural
      }

let Pass = { invariants : List P, never : List A }

let Expect = < pass : Pass | fail : P | reach : W | inconclusive >

let Row = { name : Text, expect : Expect, comment : List Text, constants : Constants }

let showP = \(p : P) -> showConstructor p

let showA = \(a : A) -> showConstructor a

let showM = \(m : M) -> showConstructor m

let showW = \(w : W) -> showConstructor w

let propertyEq = \(p : P) -> \(q : P) -> natEq (propertyIndex p) (propertyIndex q)

let actionEq = \(a : A) -> \(b : A) -> natEq (actionIndex a) (actionIndex b)

-- Every value of each union, from its table ------------------------------------

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

let ActionEntry = { action : A, index : Natural }

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

let allWitnesses = map WitnessRow W (\(e : WitnessRow) -> e.mapValue) witnessRows

let safety = filter P isSafety allProperties

-- Constants ------------------------------------------------------------------

let constantLines =
      \(c : Constants) ->
        let mutation = merge { None = "none", Some = \(m : M) -> showM m } c.Mutation

        in  [ { name = "Inos"
              , pad = "           "
              , value =
                  "{${join ", " (map Ino Text (\(i : Ino) -> showConstructor i) c.Inos)}}"
              }
            , { name = "Deep", pad = "           ", value = showBool c.Deep }
            , { name = "BatchCreate"
              , pad = "    "
              , value = showBool c.BatchCreate
              }
            , { name = "BatchFinish"
              , pad = "    "
              , value = showBool c.BatchFinish
              }
            , { name = "NoReplaceRename"
              , pad = ""
              , value = showBool c.NoReplaceRename
              }
            , { name = "MaxRuns", pad = "        ", value = Natural/show c.MaxRuns }
            , { name = "MaxCrashes"
              , pad = "     "
              , value = Natural/show c.MaxCrashes
              }
            , { name = "MaxForeign"
              , pad = "     "
              , value = Natural/show c.MaxForeign
              }
            , { name = "Mutation", pad = "       ", value = "\"${mutation}\"" }
            , { name = "BudgetSeconds"
              , pad = "  "
              , value = Natural/show c.BudgetSeconds
              }
            ]

{- The code today with batching off (BULKLOAD_FAULT_DIR_BATCH=1, or a
   descriptor budget under 32): one directory per commit, each finished
   alone; two levels of two siblings, two runs, one crash, one third-party
   directory, four inode numbers.
-}
let perDir
    : Constants
    = { Inos = [ Ino.i1, Ino.i2, Ino.i3, Ino.i4 ]
      , Deep = True
      , BatchCreate = False
      , BatchFinish = False
      , NoReplaceRename = True
      , MaxRuns = 2
      , MaxCrashes = 1
      , MaxForeign = 1
      , Mutation = None M
      , BudgetSeconds = 3600
      }

-- The code: a level bound in one commit, the finish in one commit.
let batched = perDir // { BatchCreate = True, BatchFinish = True }

-- One level of two siblings: the base of most reach and mutation rows.
let shallow =
          batched
      //  { Deep = False
          , Inos = [ Ino.i1, Ino.i2, Ino.i3 ]
          , MaxForeign = 0
          , BudgetSeconds = 600
          }

-- Row constructors -----------------------------------------------------------

let row =
      \(name : Text) ->
      \(expect : Expect) ->
      \(comment : List Text) ->
      \(constants : Constants) ->
        { name, expect, comment, constants } : Row

{- The verdict of every mutation: the one property its MC_dir_neg_<mutation>
   config must violate. A total merge.
-}
let verdict
    : M -> P
    = \(m : M) ->
        merge
          { record_before_seal = P.RecordNamesDirectory
          , rename_before_record = P.NoStrandedDirectory
          , child_before_dirseal = P.ChildImpliesNamed
          , complete_before_seal = P.CompleteImpliesFinal
          , level_seals_one_parent = P.RecordNamesDirectory
          , level_rename_seals_one_parent = P.ChildImpliesNamed
          , adopt_unbound = P.NeverAdoptForeign
          , discard_unlink_before_clear = P.NeverAdoptForeign
          , admit_under_held = P.NoDirectoryUnderTemporary
          }
          m

let pass = \(never : List A) -> Expect.pass { invariants = safety, never }

-- Never enabled with an exclusive rename: R-N119's fallback.
let noFallback = [ A.Bind, A.FallbackMkdir, A.Intent ]

let budgetSelftest =
      row
        "MC_dir_budget_selftest"
        Expect.inconclusive
        [ "BUDGET SELF-TEST (expected INCONCLUSIVE). MC_dir_batched's constants"
        , "with a 5 s budget: the search needs far longer, so WithinBudget, and"
        , "nothing else, must trip, which proves DirectoryRecords.tla's budget"
        , "is evaluated per state. The bound is MC_dir_batched's, so a broken"
        , "budget still ends (as a PASS, which tla-check rejects)."
        ]
        (batched // { BudgetSeconds = 5 })

let positives =
      [ row
          "MC_dir_per_dir"
          (pass (noFallback # [ A.ForeignReplace ]))
          [ "Directory records one at a time (R-N102), the code with batching off:"
          , "two levels of two siblings, two runs, one Stop or power loss (each"
          , "unsealed name kept or lost on its own), EnvSeal, one third-party"
          , "directory, inode numbers reused. The baseline the batched rows extend."
          ]
          perDir
      , row
          "MC_dir_batched"
          (pass (noFallback # [ A.ForeignReplace ]))
          [ "OI-1003-Q143 item 2, the code: a level's records bound in one commit,"
          , "one seal per distinct parent, every finished directory completed in"
          , "one commit; MC_dir_per_dir's bound."
          ]
          batched
      , row
          "MC_dir_batched_crashes"
          (pass (noFallback # [ A.ForeignReplace ]))
          [ "Batching over three runs and two crashes (a Stop then a power loss,"
          , "or two power losses), one level of two siblings: what a crashed"
          , "level leaves is swept, adopted or completed by the runs after it."
          ]
          (     shallow
            //  { MaxRuns = 3
                , MaxCrashes = 2
                , MaxForeign = 1
                , BudgetSeconds = 1800
                }
          )
      , row
          "MC_dir_foreign_replace"
          (pass noFallback)
          [ "A third party makes its directory at a free name and then replaces"
          , "it with a new inode, any free number (#74 N3): batching, one level of"
          , "two siblings, two runs, one crash, four inode numbers, so"
          , "ForeignReplace is enabled and no record adopts what it leaves (S1"
          , "throughput review, 2026-10-09, finding 5)."
          ]
          (     shallow
            //  { Inos = [ Ino.i1, Ino.i2, Ino.i3, Ino.i4 ]
                , MaxForeign = 2
                , BudgetSeconds = 1800
                }
          )
      , row
          "MC_dir_fallback"
          ( pass
              [ A.DiscardClear
              , A.DiscardUnlink
              , A.ForeignReplace
              , A.Mkdir
              , A.Record
              , A.Rename
              ]
          )
          [ "R-N119's fallback, batched: no exclusive rename, so each directory's"
          , "intent commits before a plain mkdirat at its final name, and a resume"
          , "adopts a fresh 0700 directory its intent owns. The temporary the code"
          , "makes first and discards is the EEXIST discard's (MC_dir_batched)."
          ]
          (batched // { NoReplaceRename = False })
      ]

let witnesses =
      [ row
          "MC_dir_reach_partial_level"
          (Expect.reach W.Witness_PartialLevel)
          [ "REACH (expected REACHED): one power loss keeps one sibling's rename"
          , "and loses the other's, under the same parent: the crash model is per"
          , "name, so a shared seal of the parent is the only thing that persists"
          , "both together (#217 review, finding 2)."
          ]
          (shallow // { MaxRuns = 1, Inos = [ Ino.i1, Ino.i2 ] })
      , row
          "MC_dir_reach_adopt_batched"
          (Expect.reach W.Witness_AdoptBatched)
          [ "REACH (expected REACHED): a directory whose record was bound in a"
          , "level's shared commit is adopted by the next run after a crash."
          ]
          (shallow // { MaxRuns = 2, Inos = [ Ino.i1, Ino.i2 ] })
      ]

{- The primary row of every mutation, keyed by its label. A total merge over
   M, as GitCarry.dhall's `primary`.
-}
let Primary = { mutation : M, constants : Constants, comment : List Text }

let primary =
      { record_before_seal =
        { mutation = M.record_before_seal
        , constants = shallow // { BatchCreate = False, BatchFinish = False }
        , comment =
          [ "a directory's record is committed before its temporary's entry is"
          , "sealed: a power loss keeps the record and loses the inode it names."
          ]
        }
      , rename_before_record =
        { mutation = M.rename_before_record
        , constants = shallow // { BatchCreate = False, BatchFinish = False }
        , comment =
          [ "the rename into place comes before the record: a power loss (or an"
          , "EnvSeal) keeps the final name with no record to adopt it by."
          ]
        }
      , child_before_dirseal =
        { mutation = M.child_before_dirseal
        , constants = shallow // { BatchCreate = False, BatchFinish = False }
        , comment =
          [ "an output commits inside a directory whose rename is not yet sealed"
          , "(#74 round 2, N1)."
          ]
        }
      , complete_before_seal =
        { mutation = M.complete_before_seal
        , constants = shallow // { BatchCreate = False, BatchFinish = False }
        , comment =
          [ "a directory's completion commits before its final mode is sealed." ]
        }
      , level_seals_one_parent =
        { mutation = M.level_seals_one_parent
        , constants =
            batched // { MaxRuns = 1, MaxForeign = 0, BudgetSeconds = 600 }
        , comment =
          [ "a level with two parents seals only one before its shared commit:"
          , "the other parent's temporaries may be lost with their records kept."
          ]
        }
      , level_rename_seals_one_parent =
        { mutation = M.level_rename_seals_one_parent
        , constants =
            batched // { MaxRuns = 1, MaxForeign = 0, BudgetSeconds = 600 }
        , comment =
          [ "after a level's renames only one parent is sealed before the"
          , "level's directories are decided: an output inside the other"
          , "parent's renamed directory can outlive its name (#217 review,"
          , "finding 2)."
          ]
        }
      , adopt_unbound =
        { mutation = M.adopt_unbound
        , constants = shallow // { BatchCreate = False, BatchFinish = False, MaxForeign = 1 }
        , comment =
          [ "an existing directory is adopted by its path alone, whatever its"
          , "record says."
          ]
        }
      , discard_unlink_before_clear =
        { mutation = M.discard_unlink_before_clear
        , constants =
                shallow
            //  { BatchCreate = False
                , BatchFinish = False
                , MaxForeign = 2
                , MaxRuns = 2
                }
        , comment =
          [ "a refused rename's temporary is removed before its record is"
          , "cleared: a power loss keeps the record bound to a freed inode"
          , "number, which a third party's directory then takes at the leaf,"
          , "and the next run adopts it (#74 N3; #217 review, finding 2)."
          ]
        }
      , admit_under_held =
        { mutation = M.admit_under_held
        , constants =
                perDir
            //  { MaxForeign = 0
                , MaxCrashes = 1
                , BudgetSeconds = 600
                }
        , comment =
          [ "a batch admits a new directory under an existing one still held,"
          , "undecided, whose crashed rename was never sealed: the child is made"
          , "inside a directory a power loss can turn back into a temporary"
          , "(#217 review, finding 1)."
          ]
        }
      }

let primaryOf =
      \(m : M) ->
        let p = merge primary m

        let property = verdict p.mutation

        in  row
              "MC_dir_neg_${showM p.mutation}"
              (Expect.fail property)
              (   [ "NEGATIVE: ${Lib.join " " p.comment}" ]
                # [ "Mutation ${showM p.mutation} must violate ${showP property}." ]
              )
              (p.constants // { Mutation = Some p.mutation })

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
      map PrimaryRow M (\(e : PrimaryRow) -> e.mapValue.mutation) primaryRowsByLabel

let negOrder =
      [ M.record_before_seal
      , M.rename_before_record
      , M.child_before_dirseal
      , M.complete_before_seal
      , M.level_seals_one_parent
      , M.level_rename_seals_one_parent
      , M.adopt_unbound
      , M.discard_unlink_before_clear
      , M.admit_under_held
      ]

let _ =
        assert
      :     map M Natural mutationIndex negOrder
        ===  range (List/length PrimaryRow primaryRowsByLabel)

let rows =
        [ budgetSelftest ]
      # positives
      # witnesses
      # map M Row primaryOf negOrder

-- Checks over the rows ----------------------------------------------------------

let failProperty =
      \(r : Row) ->
        merge
          { pass = \(_ : Pass) -> [] : List P
          , fail = \(p : P) -> [ p ]
          , reach = \(_ : W) -> [] : List P
          , inconclusive = [] : List P
          }
          r.expect

let failing = concatMap Row P failProperty rows

-- Every safety invariant but TypeOK is shown falsifiable by some fail row.
let _ =
        assert
      :     all
              P
              ( \(p : P) ->
                      propertyEq p P.TypeOK
                  ||  any P (\(q : P) -> propertyEq p q) failing
              )
              safety
        ===  True

-- Rendering -------------------------------------------------------------------

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
            # [ "SYMMETRY InoSymmetry" ]
            # map Text Text (\(i : Text) -> "INVARIANT ${i}") (checks r)
            # [ "INVARIANT WithinBudget" ]
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
        (   [ "# DirectoryRecords.tla's TLC configs, checked by `just tla-check` in this order (README.md, \"Directory records\")."
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

-- Traceability (README.md, "Directory records") ---------------------------------

{- One row per safety property but TypeOK, in table order: the SLOs it
   serves, its rulings, the code symbols it is about (grounded by tla-check:
   each must be on a non-comment line of crates/' Rust code) and the tests
   that check the same rule on the code.
-}
let InvariantRow =
      { tla : P
      , slo : List T.Slo
      , ruling : List Text
      , codeSymbol : List Text
      , ptest : List Text
      }

let invariants
    : List InvariantRow
    = [ { tla = P.RecordNamesDirectory
        , slo = [ S.Durability, S.S4 ]
        , ruling = [ "R-N102", "OI-1003-Q143" ]
        , codeSymbol =
          [ "seal_entry"
          , "create_directories"
          , "record_directories_created"
          , "PendingDirectory"
          ]
        , ptest =
          [ "power_loss::the_batched_directory_proofs_have_teeth"
          , "adoption_power_loss::a_level_crashed_after_its_commit_is_swept_and_its_records_cleared"
          ]
        }
      , { tla = P.NoStrandedDirectory
        , slo = [ S.Durability, S.S3 ]
        , ruling = [ "R-N102", "R-N119", "OI-1003-Q143" ]
        , codeSymbol =
          [ "create_directories"
          , "rename_exclusive"
          , "existing_directory"
          , "fallback_directory"
          , "INTENT"
          ]
        , ptest =
          [ "power_loss::every_power_loss_state_of_a_copy_is_consistent"
          , "power_loss::every_power_loss_state_of_a_directory_batch_is_explored"
          ]
        }
      , { tla = P.CompleteImpliesFinal
        , slo = [ S.Durability ]
        , ruling = [ "R-N102", "OI-1003-Q143" ]
        , codeSymbol = [ "finish_directories", "complete_directories", "fchmod" ]
        , ptest =
          [ "adoption_power_loss::a_finish_crashed_after_some_seals_completes_every_directory_in_one_commit"
          ]
        }
      , { tla = P.ChildImpliesNamed
        , slo = [ S.Durability, S.S3 ]
        , ruling = [ "R-N102", "OI-1003-Q143" ]
        , codeSymbol =
          [ "create_directories", "seal_dir", "existing_directory", "close_batch" ]
        , ptest =
          [ "adoption_power_loss::a_level_crashed_after_some_renames_adopts_the_renamed_and_makes_the_rest"
          ]
        }
      , { tla = P.NeverAdoptForeign
        , slo = [ S.S4 ]
        , ruling = [ "R-N102", "OI-1003-Q143" ]
        , codeSymbol =
          [ "existing_directory"
          , "discard_directory"
          , "clear_directories_bound_to"
          , "clear_directory"
          , "DirectoryCleared"
          ]
        , ptest =
          [ "transfer::tests::throughput::a_member_refused_at_its_rename_leaves_its_children_to_the_file_system"
          ]
        }
      , { tla = P.NoDirectoryUnderTemporary
        , slo = [ S.Durability, S.S3 ]
        , ruling = [ "R-N102", "OI-1003-Q143" ]
        , codeSymbol =
          [ "DirectoryBatch", "close_batch", "directory_is_new", "parent_of" ]
        , ptest =
          [ "adoption_power_loss::a_batch_waits_for_a_held_adoptable_directory_before_making_inside_it"
          ]
        }
      ]

let traced = \(p : P) -> isSafety p && propertyEq p P.TypeOK == False

let _ =
        assert
      :     map InvariantRow Natural (\(r : InvariantRow) -> propertyIndex r.tla) invariants
        ===  map P Natural propertyIndex (filter P traced allProperties)

-- Outputs ----------------------------------------------------------------------

let constantNames =
      map
        { name : Text, pad : Text, value : Text }
        Text
        (\(k : { name : Text, pad : Text, value : Text }) -> k.name)
        (constantLines perDir)

let module = T.moduleEntry T.Module.DirectoryRecords

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
          # [ "Spec", "Init", "Next", "InoSymmetry" ]
      , constants = constantNames
      , mutations = map M Text showM allMutations
      , codeSymbols =
          concatMap InvariantRow Text (\(r : InvariantRow) -> r.codeSymbol) invariants
      , symbolMatch = showConstructor module.symbols
      , pendingSymbols = [] : List Text
      , labelSets = [] : List { name : Text, labels : List Text }
      }
    , invariants =
        map
          InvariantRow
          { tla : Text, slo : List Text, ruling : List Text, codeSymbol : List Text, ptest : List Text }
          ( \(r : InvariantRow) ->
              { tla = showP r.tla
              , slo = map T.Slo Text (\(s : T.Slo) -> showConstructor s) r.slo
              , ruling = r.ruling
              , codeSymbol = r.codeSymbol
              , ptest = r.ptest
              }
          )
          invariants
    }
