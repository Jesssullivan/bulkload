{- docs/formal's typed catalogue (OI-1003-Q32): every TLC config of
   BulkloadTransfer.tla, the verdict of every mutation, and the
   traceability row of every model property. GitCarry.dhall holds the same
   for GitCarry.tla (OI-1003-Q43), and this file renders both modules
   (T.Module): `files` holds each module's run order and configs, and
   `grounding` one entry per module.

   Edit this file, never its outputs. `just tla-render` renders configs.tsv
   and every MC_*.cfg from it (dhall-to-json, then one file per entry of
   `files`). `just tla-check` renders it again into scratch before it starts
   TLC and stops unless:
   - the rendering equals the committed files byte for byte (staleness);
   - every name in `grounding` exists in BulkloadTransfer.tla, and every
     code symbol is found by `git grep -w` under crates/ (grounding).

   `just formal-nv` evaluates it too, and runs every row of `nversion` (the
   mutation rows inside the Haskell explorer's domain) on the explorer and,
   for the every-invariant check, on TLC.

   The `assert`s below are checked whenever the catalogue is evaluated, so a
   catalogue that breaks one of them renders nothing. No Prelude import: the
   catalogue evaluates offline.
-}
let T = ./Types.dhall

let P = T.Property

let W = T.Witness

let A = T.Action

let M = T.Mutation

let Seat = T.Seat

let Mode = T.SupersedeMode

-- Provenance lines written into every rendered file.
let provenance =
      { cfg =
          "\\* Rendered from catalogue/Catalogue.dhall; expected outcome in configs.tsv."
      , tsv =
          "# Rendered from catalogue/Catalogue.dhall by `just tla-render`; edit the catalogue, not this file."
      }

-- List and text helpers (Lib.dhall, shared with GitCarry.dhall) ---------------

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

let showP = \(p : P) -> showConstructor p

let showW = \(w : W) -> showConstructor w

let showA = \(a : A) -> showConstructor a

let showM = \(m : M) -> showConstructor m

let propertyEq =
      \(p : P) -> \(q : P) -> natEq (T.propertyIndex p) (T.propertyIndex q)

let actionEq = \(a : A) -> \(b : A) -> natEq (T.actionIndex a) (T.actionIndex b)

-- Every value of each union, from its table in Types.dhall -------------------

{- Dhall cannot list a union's labels, so each list below comes from a
   record with one field per label, which a total `merge` keeps exact (a
   label without a field, or a field without a label, does not type-check).
   Nothing here lists a union by hand. For each table: every field holds its
   own label's value, and the positions are 0, 1, 2, ... with no gap or
   repeat, counted from the table itself.
-}
let range = Lib.range

-- The values of a table, in the order of their positions.
let ordered = Lib.ordered

let PropertyRow = { mapKey : Text, mapValue : T.PropertyEntry }

let propertyRows = toMap T.propertyTable

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
        T.propertyIndex
        ( map
            PropertyRow
            P
            (\(e : PropertyRow) -> e.mapValue.property)
            propertyRows
        )

let _ =
        assert
      :     map P Natural T.propertyIndex allProperties
        ===  range (List/length PropertyRow propertyRows)

let WitnessRow = { mapKey : Text, mapValue : W }

let witnessRows = toMap T.witnessTable

let _ =
        assert
      :     map WitnessRow Text (\(e : WitnessRow) -> e.mapKey) witnessRows
        ===  map WitnessRow Text (\(e : WitnessRow) -> showW e.mapValue) witnessRows

let allWitnesses = map WitnessRow W (\(e : WitnessRow) -> e.mapValue) witnessRows

let ActionRow = { mapKey : Text, mapValue : T.ActionEntry }

let actionRows = toMap T.actionTable

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
        T.actionIndex
        (map ActionRow A (\(e : ActionRow) -> e.mapValue.action) actionRows)

let _ =
        assert
      :     map A Natural T.actionIndex allActions
        ===  range (List/length ActionRow actionRows)

-- Mutations come from the primary-row table, `primary`, below.

-- Constants ------------------------------------------------------------------

-- Every CONSTANT, its padding in the rendered block, and its value.
let constantLines =
      \(c : T.Constants) ->
        let mutation =
              merge { None = "none", Some = \(m : M) -> showM m } c.Mutation

        in  [ { name = "Seats"
              , pad = "              "
              , value =
                  "{${join ", " (map Seat Text (\(s : Seat) -> showConstructor s) c.Seats)}}"
              }
            , { name = "MaxRuns"
              , pad = "            "
              , value = Natural/show c.MaxRuns
              }
            , { name = "MaxCrashes"
              , pad = "         "
              , value = Natural/show c.MaxCrashes
              }
            , { name = "MaxEdits"
              , pad = "           "
              , value = Natural/show c.MaxEdits
              }
            , { name = "MaxForeign"
              , pad = "         "
              , value = Natural/show c.MaxForeign
              }
            , { name = "MaxCommitFails"
              , pad = "     "
              , value = Natural/show c.MaxCommitFails
              }
            , { name = "SpaceRefusals"
              , pad = "      "
              , value = showBool c.SpaceRefusals
              }
            , { name = "RelaxedSourceLedger"
              , pad = ""
              , value = showBool c.RelaxedSourceLedger
              }
            , { name = "RelaxedAuthority"
              , pad = "   "
              , value = showBool c.RelaxedAuthority
              }
            , { name = "SupersedeMode"
              , pad = "      "
              , value = "\"${showConstructor c.SupersedeMode}\""
              }
            , { name = "EstateReads"
              , pad = "        "
              , value = showBool c.EstateReads
              }
            , { name = "MaxBackupSteps"
              , pad = "     "
              , value = Natural/show c.MaxBackupSteps
              }
            , { name = "Mutation", pad = "           ", value = "\"${mutation}\"" }
            , { name = "BudgetSeconds"
              , pad = "      "
              , value = Natural/show c.BudgetSeconds
              }
            , { name = "StoreRootSealed"
              , pad = "    "
              , value = showBool c.StoreRootSealed
              }
            , { name = "TrackStrictHeld"
              , pad = "    "
              , value = showBool c.TrackStrictHeld
              }
            , { name = "AdoptUnrowed"
              , pad = "       "
              , value = showBool c.AdoptUnrowed
              }
            , { name = "ExchangeSupported"
              , pad = "  "
              , value = showBool c.ExchangeSupported
              }
            ]

let defaults
    : T.Constants
    = { Seats = [ Seat.a, Seat.b ]
      , MaxRuns = 2
      , MaxCrashes = 1
      , MaxEdits = 1
      , MaxForeign = 0
      , MaxCommitFails = 0
      , SpaceRefusals = False
      , RelaxedSourceLedger = False
      , RelaxedAuthority = False
      , SupersedeMode = Mode.off
      , EstateReads = False
      , MaxBackupSteps = 2
      , Mutation = None M
      , BudgetSeconds = 600
      , StoreRootSealed =
          {- An assumption the code does not meet yet (README, "Code and
             design disagreements"): the source state root's entry is sealed.
          -}
          True
      , TrackStrictHeld = False
      , AdoptUnrowed =
          {- The code since #169: each non-racy staged file carries its
             capture record, and a resume adopts a durable unrowed output
             the record proves, without a source read.
          -}
          True
      , ExchangeSupported =
          {- OI-1003-Q100: the destination file system has the atomic
             exchange of two names. Read only under SupersedeMode exchange;
             MC_supersede_noexchange and its rows turn it off.
          -}
          True
      }

{- Every fault on: a third-party write or delete, a failed group commit
   (full disk), and the space refusal.
-}
let faults = { MaxForeign = 1, MaxCommitFails = 1, SpaceRefusals = True }

{- MC_main: two seats, two runs, one crash of either host or both, one source
   edit per seat, destination faults off.
-}
let main =
      defaults
      // { Seats = [ Seat.a, Seat.b ], MaxRuns = 2, MaxCrashes = 1, MaxEdits = 1 }

{- One seat, one run, no faults: the base of most mutation configs. It
   models the code since #169 (AdoptUnrowed on, from `defaults`), so every
   mutation row built on it must be caught with the capture record's
   adoption in place. The rows that keep the transfer before #169 say so
   with `before169`.
-}
let one =
      defaults
      // { Seats = [ Seat.a ]
         , MaxRuns = 1
         , MaxCrashes = 0
         , MaxEdits = 0
         , MaxForeign = 0
         , MaxCommitFails = 0
         , SpaceRefusals = False
         , BudgetSeconds = 300
         }

{- The transfer before #169: no capture record, no adoption. Only the rows
   that exist to show that transfer set it: the frozen N-version core and
   its second row (with the two reach rows at that bound), and three finding
   rows (README.md, "Rows that model the transfer before #169").
-}
let before169 = { AdoptUnrowed = False }

{- The constants first drafted for MC_main: two seats, three runs, every
   fault. Far too large to model-check (README.md, "The budget is
   state-level"); used only for the seeded simulation.
-}
let drafted = main // { MaxRuns = 3 } // faults

{- The N-version core (OI-1003-Q32): the shared bound for TLC and the
   Haskell explorer (hs/Explorer.hs). One seat, three runs, two crashes, one
   source edit; third-party writes, commit failures, the space refusal,
   superseding publish and estate reads off.
-}
let nvCore =
      defaults
      // { Seats = [ Seat.a ]
         , MaxRuns = 3
         , MaxCrashes = 2
         , MaxEdits = 1
         , MaxForeign = 0
         , MaxCommitFails = 0
         , SpaceRefusals = False
         , SupersedeMode = Mode.off
         , EstateReads = False
         }
      // before169

{- The N-version core's second row: the same bound plus one third-party write
   or delete, so that the source ledger's read paths (a ledger manifest, and
   its chunks re-read to fill an absent output) are part of the cross-check.
-}
let nvLedger = nvCore // { MaxForeign = 1 }

{- The same two bounds for the code since #169 (AdoptUnrowed on): the rows
   the explorer cross-checks for the shipped transfer. MC_nv_core is a
   frozen name with a count of record, so it keeps the transfer before #169
   until a ruling moves it (README.md, "N-version core").
-}
let nvCoreAdopt = nvCore // { AdoptUnrowed = True }

let nvLedgerAdopt = nvLedger // { AdoptUnrowed = True }

{- A one-seat mutation config on the N-version core's bound, ONE's budget,
   the code since #169.
-}
let nvMutation = nvCoreAdopt // { BudgetSeconds = one.BudgetSeconds }

{- WP0(g) breadth: two seats, three runs, one crash, one third-party write or
   delete, relaxed ledger rows. The third-party write is what makes a later
   run consult the ledger for a row a crash dropped (MC_reach_wp0g_lost_row);
   with MC_main's bound the relaxed ledger is never read.
-}
let wp0g =
      defaults
      // { Seats = [ Seat.a, Seat.b ]
         , MaxRuns = 3
         , MaxCrashes = 1
         , MaxEdits = 0
         , MaxForeign = 1
         , RelaxedSourceLedger = True
         }

-- Properties -----------------------------------------------------------------

-- Every safety invariant, in table order. The names are frozen (README.md,
-- "Frozen names").
let safety = filter P T.isSafety allProperties

{- The verdict of every mutation: the one property its MC_neg_<mutation>
   config must violate. A total merge: a mutation added to T.Mutation
   without a verdict here does not type-check.
-}
let verdict
    : M -> P
    = \(m : M) ->
        merge
          { held_before_commit = P.HeldAfterCommit
          , commit_before_fsync = P.RecordImpliesBytes
          , commit_before_dirseal = P.RecordImpliesBytes
          , adopt_without_seal = P.RecordImpliesBytes
          , ledger_before_held = P.LedgerAfterHeld
          , done_before_sync = P.DoneAfterLedger
          , reread_durable = P.R25_NoDurableReread
          , reread_ignore_ledger = P.R25_NoCommittedCaptureReread
          , skip_output_row = P.S3_ClosedPassIsHeld
          , double_read = P.ReadOnce
          , src_ledger_carries_r25 = P.R25_NoDurableReread
          , record_racy = P.ReuseSound
          , untyped_space = P.ClosureAccounted
          , source_write = P.S2_TypedSourceAccess
          , pause_writer = P.S2_TypedSourceAccess
          , git_optional_locks = P.S2_TypedSourceAccess
          , unbounded_backup = P.S2_BackupLockBounded
          , supersede_unchecked = P.NoClobber
          , sweep_displaced = P.NoClobber
          , adopt_unkeyed = P.ReuseSound
          , adopt_unverified = P.RecordImpliesBytes
          , reuse_ignores_row = P.AdoptOnlyUnrowed
          , adopt_unrecorded = P.R25_StrictNoDurableReread
          , owned_ignores_identity = P.NoClobber
          , sweep_drops_ownership = P.SupersedeAtomic
          , exchange_before_intent = P.SupersedeAtomic
          , own_is_reuse = P.OwnershipNeverReuse
          , refusal_unbound = P.RememberedRefusalSound
          , late_exchange_refusal = P.ExchangeRefusedUpFront
          }
          m

-- Coverage (README.md, "Coverage"): the actions a pass row's report must show
-- never enabled. tla-check fails a pass row on any difference.
let wp0dOff =
      [ A.BeginSupersede, A.CheckOwn, A.Exchange, A.RenameReplace, A.VerifyDisp ]

-- OI-1003-Q100: with no atomic exchange nothing is ever superseded.
let noExchange = [ A.BeginSupersede, A.Exchange, A.VerifyDisp ]

let checkRename = [ A.CheckOwn, A.RenameReplace ]

let estateOff =
      [ A.BackupBegin, A.BackupEnd, A.BackupStepLock, A.BackupStepUnlock, A.GitRead ]

let noForeign = [ A.ForeignDelete, A.ForeignWrite ]

let noCrash = [ A.CrashBoth, A.CrashDst, A.CrashSrc ]

let noEdit = [ A.Edit, A.SilentRewrite, A.RecvRefused ]

-- Row constructors -----------------------------------------------------------

let row =
      \(name : Text) ->
      \(expect : T.Expect) ->
      \(comment : List Text) ->
      \(constants : T.Constants) ->
          { name
          , expect
          , comment
          , constants
          , spec = T.Specification.Spec
          , symmetry = False
          , flags = None Text
          }
        : T.Row

let pass =
      \(never : List A) ->
        T.Expect.pass
          { invariants = safety, properties = [] : List P, never }

{- A pass row of the code since #169: every safety invariant, and
   AdoptOnlyUnrowed (a rowed output is never adopted again from its record).
-}
let passAdopt =
      \(never : List A) ->
        T.Expect.pass
          { invariants = safety # [ P.AdoptOnlyUnrowed ]
          , properties = [] : List P
          , never
          }

-- A pass row that also checks R25's strict reading (#169).
let passStrict =
      \(never : List A) ->
        T.Expect.pass
          { invariants =
              safety # [ P.R25_StrictNoDurableReread, P.AdoptOnlyUnrowed ]
          , properties = [] : List P
          , never
          }

{- #187's four properties (OI-1003-Q100..Q102), beside the safety
   invariants: checked by every pass row with the superseding publish on.
-}
let superseding =
      [ P.SupersedeAtomic
      , P.OwnershipNeverReuse
      , P.RememberedRefusalSound
      , P.ExchangeRefusedUpFront
      ]

-- A pass row of the code since #187: SupersedeMode exchange.
let passSupersede =
      \(never : List A) ->
        T.Expect.pass
          { invariants = safety # [ P.AdoptOnlyUnrowed ] # superseding
          , properties = [] : List P
          , never
          }

-- The same with R25's strict reading (TrackStrictHeld).
let passSupersedeStrict =
      \(never : List A) ->
        T.Expect.pass
          { invariants =
                safety
              # [ P.R25_StrictNoDurableReread, P.AdoptOnlyUnrowed ]
              # superseding
          , properties = [] : List P
          , never
          }

{- The superseding publish's one-seat bound (MC_wp0d_exchange): three runs,
   one crash, one edit, one third-party write.
-}
let exchangeOne =
          defaults
      //  { Seats = [ Seat.a ]
          , MaxRuns = 3
          , MaxForeign = 1
          , SupersedeMode = Mode.exchange
          }

-- The same on a destination with no atomic exchange (OI-1003-Q100).
let noExchangeOne = exchangeOne // { ExchangeSupported = False }

-- #169's bound for R25's strict reading: one crash between runs.
let strictOne =
      one // { MaxRuns = 2, MaxCrashes = 1, TrackStrictHeld = True }

{- The invariants beside the safety ones that a row's constants make
   meaningful, in table order: formal-nv's every-invariant configs check
   them too, and hs/Explorer.hs's `all` is the same list.
-}
let beside =
      \(c : T.Constants) ->
          (if c.TrackStrictHeld then [ P.R25_StrictNoDurableReread ] else [] : List P)
        # (if c.AdoptUnrowed then [ P.AdoptOnlyUnrowed ] else [] : List P)

let core = "On the N-version core's bound, with #169's adoption (OI-1003-Q32)."

let line = \(head : Text) -> { head, tail = [] : List Text }

-- The property a mutation row must violate, and its config's name.
let negProperty =
      \(n : T.NegRow) ->
        merge
          { verdict = verdict n.mutation
          , also = \(a : { suffix : Text, property : P }) -> a.property
          }
          n.against

let negName =
      \(n : T.NegRow) ->
        let suffix =
              merge
                { verdict = showM n.mutation
                , also = \(a : { suffix : Text, property : P }) -> a.suffix
                }
                n.against

        in  "MC_neg_${suffix}"

let neg =
      \(n : T.NegRow) ->
        let property = negProperty n

        in  row
              (negName n)
              (T.Expect.fail property)
              (   [ "NEGATIVE: ${n.comment.head}" ]
                # n.comment.tail
                # [ "Mutation ${showM n.mutation} must violate ${showP property}."
                  ]
              )
              (n.constants // { Mutation = Some n.mutation })

let also =
      \(suffix : Text) ->
      \(property : P) ->
        T.Against.also { suffix, property }

-- The configs, in run order ---------------------------------------------------

-- The budget's own proof: always the first row.
let budgetSelftest =
          row
            "MC_budget_selftest"
            T.Expect.inconclusive
            [ "BUDGET SELF-TEST (expected INCONCLUSIVE). MC_main's constants with"
            , "a 5 s budget: the search needs far longer, so WithinBudget, and"
            , "nothing else, must trip, which proves the budget is evaluated per"
            , "state. The bound is MC_main's, so a broken budget still ends (as a"
            , "PASS, which tla-check rejects before it runs any other config)."
            ]
            (main // { BudgetSeconds = 5 })
      //  { symmetry = True }

let positives =
      [ -- The transfer without superseding publish: the code before #187
        -- (README.md, "Rows that model the transfer before #187").
            row
              "MC_main"
              (passAdopt (noForeign # [ A.CommitFail ] # wp0dOff # estateOff))
              [ "Main config, breadth: a strict source ledger and no superseding"
              , "publish (the transfer before #187; MC_supersede_main is this bound"
              , "with it), with the state root assumed sealed. Two seats, two runs,"
              , "one crash of either host or both, one source edit per seat;"
              , "destination faults off."
              ]
              main
        //  { symmetry = True }
      , row
          "MC_main_deep"
          (passAdopt (wp0dOff # estateOff))
          [ "Main config, depth: one seat through three runs and two crashes,"
          , "with a source edit and every destination fault: a third-party"
          , "write or delete, a failed group commit and the space refusal."
          ]
          (defaults // { Seats = [ Seat.a ], MaxRuns = 3, MaxCrashes = 2 } // faults)
      ,     row
              "MC_dest_faults"
              (passAdopt (noCrash # noEdit # wp0dOff # estateOff))
              [ "Destination faults on two seats: a third-party write or delete, a"
              , "failed group commit (full disk, #100) and the space refusal. No"
              , "crash or source edit here; MC_main_deep combines them on one seat."
              ]
              (defaults // { MaxCrashes = 0, MaxEdits = 0 } // faults)
        //  { symmetry = True }
      , row
          "MC_nv_core"
          (pass (noForeign # [ A.CommitFail ] # wp0dOff # estateOff))
          [ "N-version core (OI-1003-Q32): the shared bound for TLC and sprint"
          , "2's Haskell BFS explorer. One seat, three runs, two crashes, one"
          , "source edit; third-party writes, commit failures, the space"
          , "refusal, superseding publish and estate reads off. No SYMMETRY, so"
          , "its distinct-state count (README.md) is the plain state graph's."
          ]
          nvCore
      , row
          "MC_nv_ledger"
          (pass ([ A.CommitFail ] # wp0dOff # estateOff))
          [ "N-version core, second row (OI-1003-Q32): MC_nv_core plus one"
          , "third-party write or delete, so the source ledger's read paths (a"
          , "ledger manifest; its chunks re-read for an absent output) are part"
          , "of the cross-check (MC_reach_ledger_manifest, MC_reach_ledger_chunks"
          , "prove both reachable at this bound). No SYMMETRY."
          ]
          nvLedger
      , row
          "MC_nv_core_adopt"
          (passAdopt (noForeign # [ A.CommitFail ] # wp0dOff # estateOff))
          [ "N-version cross-check of the code since #169 (OI-1003-Q32):"
          , "MC_nv_core's bound with the capture record and its adoption"
          , "(AdoptUnrowed on). hs/Explorer.hs must reach this row's distinct"
          , "count too. MC_nv_core itself is frozen with the transfer before"
          , "#169. No SYMMETRY."
          ]
          nvCoreAdopt
      , row
          "MC_nv_ledger_adopt"
          (passAdopt ([ A.CommitFail ] # wp0dOff # estateOff))
          [ "N-version cross-check of the code since #169, second row:"
          , "MC_nv_ledger's bound with AdoptUnrowed on, so the manifest"
          , "adoption's record, the in-place third-party rewrite and both ledger"
          , "read paths are cross-checked. No SYMMETRY."
          ]
          nvLedgerAdopt
      , -- WP0(g) / OI-1003-Q20.
            row
              "MC_wp0g"
              ( passAdopt
                  (   [ A.Edit, A.SilentRewrite, A.RecvRefused, A.CommitFail ]
                    # wp0dOff
                    # estateOff
                  )
              )
              [ "WP0(g) / OI-1003-Q20, breadth: the source ledger's row commits run"
              , "synchronous=NORMAL, fullfsync=OFF, so a source power loss may drop"
              , "any subset of them. The store-creation commit (schema and"
              , "authority) stays durable. Two seats, three runs, one crash, one"
              , "third-party write or delete: a later run consults the ledger for a"
              , "dropped row (MC_reach_wp0g_lost_row). Every safety invariant must"
              , "still hold."
              ]
              wp0g
        //  { symmetry = True }
      , row
          "MC_wp0g_deep"
          (passAdopt (wp0dOff # estateOff))
          [ "WP0(g), depth: one seat, three runs, two crashes, every destination"
          , "fault, relaxed ledger rows, durable store creation."
          ]
          (     defaults
            //  { Seats = [ Seat.a ]
                , MaxRuns = 3
                , MaxCrashes = 2
                , RelaxedSourceLedger = True
                }
            //  faults
          )
      , row
          "MC_wp0g_strict"
          (passStrict (wp0dOff # estateOff))
          [ "WP0(g) with R25's strict reading (#169, OI-1003-Q40): MC_wp0g_deep's"
          , "bound with TrackStrictHeld. A relaxed ledger that loses rows, to a"
          , "power loss or to a failed commit, never costs a read of bytes the"
          , "destination holds durably, with a row or (by the capture record)"
          , "without one."
          ]
          (     defaults
            //  { Seats = [ Seat.a ]
                , MaxRuns = 3
                , MaxCrashes = 2
                , RelaxedSourceLedger = True
                , TrackStrictHeld = True
                }
            //  faults
          )
      , -- WP0(d) superseding publish, the exchange design: the code since #187.
        row
          "MC_wp0d_exchange"
          (passSupersede ([ A.CommitFail ] # checkRename # estateOff))
          [ "WP0(d) superseding publish, exchange design (the code since #187):"
          , "an intent that takes the path's rows, RENAME_EXCHANGE, then the"
          , "displaced identity is checked against the intent and a foreign file"
          , "is swapped back; the next session's sweep settles an intent a crash"
          , "left (rows back, an ownership row, or a displaced foreign file"
          , "restored). With the ownership row of a racy publish and the"
          , "remembered refusal (OI-1003-Q101, Q102). One seat, three runs, one"
          , "crash, one edit, one third-party write."
          ]
          exchangeOne
      ,     row
              "MC_supersede_main"
              ( passSupersede
                  (noForeign # [ A.CommitFail ] # checkRename # estateOff)
              )
              [ "The code since #187, breadth: MC_main's bound with the superseding"
              , "publish (exchange design). Two seats, two runs, one crash of either"
              , "host or both, one source edit per seat; destination faults off."
              ]
              (main // { SupersedeMode = Mode.exchange })
        //  { symmetry = True }
      , row
          "MC_supersede_deep"
          (passSupersede (checkRename # estateOff))
          [ "The code since #187, depth: MC_main_deep's bound with the superseding"
          , "publish. One seat through three runs and two crashes, with a source"
          , "edit and every destination fault: a third-party write or delete, a"
          , "failed group commit and the space refusal."
          ]
          (     defaults
            //  { Seats = [ Seat.a ]
                , MaxRuns = 3
                , MaxCrashes = 2
                , SupersedeMode = Mode.exchange
                }
            //  faults
          )
      ,     row
              "MC_supersede_strict_main"
              ( passSupersedeStrict
                  (noForeign # [ A.CommitFail ] # checkRename # estateOff)
              )
              [ "OI-1003-Q102: R25 with the superseding publish on, breadth."
              , "MC_supersede_main's bound with TrackStrictHeld, so both"
              , "R25_NoDurableReread and R25_StrictNoDurableReread are checked"
              , "across the intent, the exchange and the sweep."
              ]
              (main // { SupersedeMode = Mode.exchange, TrackStrictHeld = True })
        //  { symmetry = True }
      , row
          "MC_supersede_strict_deep"
          (passSupersedeStrict (checkRename # estateOff))
          [ "OI-1003-Q102: R25 with the superseding publish on, depth."
          , "MC_supersede_deep's bound with TrackStrictHeld."
          ]
          (     defaults
            //  { Seats = [ Seat.a ]
                , MaxRuns = 3
                , MaxCrashes = 2
                , SupersedeMode = Mode.exchange
                , TrackStrictHeld = True
                }
            //  faults
          )
      , row
          "MC_supersede_noexchange"
          (passSupersede ([ A.CommitFail ] # noExchange # checkRename # estateOff))
          [ "OI-1003-Q100: the code since #187 on a destination with no atomic"
          , "exchange. A seat whose output would be superseded is refused"
          , "DESTINATION_EXCHANGE_UNSUPPORTED before anything is staged; there is"
          , "no fallback, so nothing is ever replaced (ExchangeRefusedUpFront)."
          , "The refusal is remembered. MC_wp0d_exchange's bound."
          ]
          noExchangeOne
      , -- S2.
        row
          "MC_s2"
          (passAdopt (noForeign # [ A.CommitFail ] # wp0dOff))
          [ "S2 / WP0(b): typed source access, with estate capture's git read and"
          , "the SQLite backup's per-step shared read lock interleaved with a"
          , "transfer that crashes and reruns. One seat, two runs."
          ]
          (defaults // { Seats = [ Seat.a ], EstateReads = True })
      , -- Liveness: never under SYMMETRY (unsound for liveness).
            row
              "MC_live"
              ( T.Expect.pass
                  { invariants = [ P.TypeOK, P.ClosureAccounted ]
                  , properties = [ P.RunsClose, P.AllRunsFinish ]
                  , never =
                        noCrash
                      # noEdit
                      # noForeign
                      # [ A.CommitFail ]
                      # wp0dOff
                      # estateOff
                  }
              )
              [ "Liveness under WF_vars(Protocol): no crash, a stable source. Every"
              , "started run reaches closure (every seat applied or typed-refused),"
              , "and every run is made. The space refusal stays on (a typed refusal"
              , "closes an item). No SYMMETRY: it is unsound for liveness."
              ]
              (defaults // { MaxCrashes = 0, MaxEdits = 0, SpaceRefusals = True })
        //  { spec = T.Specification.LiveSpec }
      , -- R25's strict reading (#169, OI-1003-Q40).
        row
          "MC_r25_unrowed_bytes"
          (passStrict ([ A.CommitFail ] # noEdit # noForeign # wp0dOff # estateOff))
          [ "#169 (OI-1003-Q40): R25 under the strict reading of \"held durably\""
          , "(OI-1002-Q33). A crash after the file seal and the rename but before"
          , "commit_outputs leaves bulkload's own bytes durable at the final path"
          , "with no row. Each non-racy staged file now carries its capture record,"
          , "and the resume adopts such an output from it (RecvEntry, Reuse) without"
          , "a source read: R25_StrictNoDurableReread holds with every safety"
          , "invariant. MC_r25_unrowed_no_adopt is the same bound without it."
          , "Holds for non-racy captures whose record could be written (extended"
          , "attributes, an output its owner can write); one seat."
          ]
          strictOne
      , row
          "MC_r25_strict_deep"
          (passStrict (wp0dOff # estateOff))
          [ "#169, depth: R25's strict reading with the capture record's adoption"
          , "through three runs, two crashes, a source edit and every destination"
          , "fault: a third-party write (in place, keeping the record, or new), a"
          , "failed group commit (published, no row: adopted next run) and the"
          , "space refusal. MC_main_deep's bound with TrackStrictHeld."
          ]
          (     defaults
            //  { Seats = [ Seat.a ]
                , MaxRuns = 3
                , MaxCrashes = 2
                , TrackStrictHeld = True
                }
            //  faults
          )
      ,     row
              "MC_r25_strict_main"
              (passStrict (noForeign # [ A.CommitFail ] # wp0dOff # estateOff))
              [ "#169, breadth: R25's strict reading at MC_main's bound (two seats,"
              , "two runs, one crash of either host or both, one source edit per"
              , "seat), with TrackStrictHeld."
              ]
              (main // { TrackStrictHeld = True })
        //  { symmetry = True }
      , row
          "MC_r25_strict_unsealed"
          (passStrict ([ A.CommitFail ] # noEdit # noForeign # wp0dOff # estateOff))
          [ "#169 across a lost source authority: MC_r25_unrowed_bytes's bound"
          , "with the source state root's entry unsealed, so a source power loss"
          , "may lose the store and the next run keys every row anew. The capture"
          , "record's key holds no authority (the walked row alone), so outputs"
          , "with a record, rowed or not, are adopted without a read: the strict"
          , "reading and R25_NoDurableReread both hold. MC_store_root_unsealed is"
          , "this bound before #169, and fails."
          ]
          (strictOne // { StoreRootSealed = False })
      , row
          "MC_r25_strict_authority"
          (passStrict ([ A.CommitFail ] # noEdit # noForeign # wp0dOff # estateOff))
          [ "#169 across a lost source authority, relaxed: MC_wp0g_authority's"
          , "bound (the whole source store relaxed, creation commit included) with"
          , "TrackStrictHeld and the capture record. Passes for the same reason"
          , "as MC_r25_strict_unsealed. It does not lift WP0(g)'s condition: an"
          , "output without a record (no extended attributes) is still re-read."
          ]
          (     strictOne
            //  { RelaxedSourceLedger = True, RelaxedAuthority = True }
          )
      , -- The drafted constants: simulation only, never model-checked.
            row
              "MC_main_sim"
              (T.Expect.simulate safety)
              [ "SIMULATION ONLY, never a model-checking result: the constants first"
              , "drafted for MC_main (two seats, three runs, one crash, every fault)."
              , "Random behaviours of bounded depth from a fixed seed, every"
              , "invariant. No SYMMETRY (simulation does not use it)."
              ]
              drafted
        //  { flags = Some "-simulate num=3000 -depth 120 -seed 20261003" }
      ]

-- Reachability witnesses: expected REACHED.
let witnesses =
      [ row
          "MC_reach_ledger_manifest"
          (T.Expect.reach W.Witness_LedgerManifest)
          [ "REACH (expected REACHED): at MC_nv_ledger's bound the source serves"
          , "a manifest from its ledger without reading (RecvDecide's ledger"
          , "branch), so that pass row explores it."
          ]
          nvLedger
      , row
          "MC_reach_ledger_chunks"
          (T.Expect.reach W.Witness_LedgerChunkRead)
          [ "REACH (expected REACHED): at MC_nv_ledger's bound the source re-reads"
          , "a ledger manifest's chunks to fill an absent output (RecvNeed's"
          , "pread branch), so that pass row explores it."
          ]
          nvLedger
      ,     row
              "MC_reach_wp0g_lost_row"
              (T.Expect.reach W.Witness_LostRowRead)
              [ "REACH (expected REACHED): at MC_wp0g's bound a relaxed ledger loses"
              , "a committed row, and a later run consults the ledger for that seat,"
              , "misses, and reads it to build the manifest. A strict ledger never"
              , "loses a row, so this is the relaxed-only behaviour MC_wp0g must"
              , "explore for its PASS to say anything about OI-1003-Q20."
              ]
              wp0g
        //  { symmetry = True }
      , row
          "MC_reach_exchange_refused"
          (T.Expect.reach W.Witness_ExchangeRefused)
          [ "REACH (expected REACHED): at MC_supersede_noexchange's bound a"
          , "changed seat's own output is refused"
          , "DESTINATION_EXCHANGE_UNSUPPORTED (OI-1003-Q100), so that pass row"
          , "explores the refusal."
          ]
          noExchangeOne
      , row
          "MC_reach_remembered_refusal"
          (T.Expect.reach W.Witness_RememberedRefusal)
          [ "REACH (expected REACHED): at MC_wp0d_exchange's bound an entry is"
          , "refused from its remembered refusal when it is offered (RecvEntry),"
          , "with nothing read at the source."
          ]
          exchangeOne
      , row
          "MC_reach_ownership_superseded"
          (T.Expect.reach W.Witness_OwnershipSuperseded)
          [ "REACH (expected REACHED): at MC_wp0d_exchange's bound a superseding"
          , "publish begins on an output this store owns by its ownership row"
          , "alone: a racy publish whose seat changed again (OI-1003-Q101)."
          ]
          exchangeOne
      , row
          "MC_reach_sweep_ownership"
          (T.Expect.reach W.Witness_SweepOwnership)
          [ "REACH (expected REACHED): at MC_wp0d_exchange's bound the sweep"
          , "settles an interrupted supersede whose staged file is at the leaf"
          , "with the path's ownership row (StartRun)."
          ]
          exchangeOne
      , row
          "MC_reach_sweep_restore"
          (T.Expect.reach W.Witness_SweepRestore)
          [ "REACH (expected REACHED): at MC_wp0d_exchange's bound the sweep"
          , "settles an interrupted supersede whose exchange did not take effect"
          , "by putting the old output's rows back (StartRun)."
          ]
          exchangeOne
      ,     row
              "MC_reach_wp0g_failed_commit"
              (T.Expect.reach W.Witness_FailedRowRead)
              [ "REACH (expected REACHED): at MC_wp0g's bound a relaxed ledger's row"
              , "commit fails and is counted, not fatal (#163; LedgerSync::Relaxed),"
              , "and with no crash at all a later run consults the ledger for that"
              , "seat, misses, and reads it to build the manifest. So MC_wp0g's PASS"
              , "covers the counted failure as well as the power loss."
              ]
              wp0g
        //  { symmetry = True }
      ]

-- Design and code findings: expected to fail.
let findings =
      [ row
          "MC_wp0g_authority"
          (T.Expect.fail P.R25_NoDurableReread)
          [ "WP0(g) FINDING (expected to fail): relax the whole source store,"
          , "including the commit that creates its authority. A source power loss"
          , "before that commit reaches disk loses the authority; the next run keys"
          , "every row anew, finds no destination row, and re-reads bytes the"
          , "destination holds durably. R25 then fails. The transfer before #169"
          , "(AdoptUnrowed off), and any output without a capture record;"
          , "MC_r25_strict_authority is this bound with the record."
          ]
          (     one
            //  { MaxRuns = 2
                , MaxCrashes = 1
                , RelaxedSourceLedger = True
                , RelaxedAuthority = True
                }
            //  before169
          )
      , row
          "MC_store_root_unsealed"
          (T.Expect.fail P.R25_NoDurableReread)
          [ "CODE FINDING (expected to fail): the code today, strict settings,"
          , "but the source state root's directory entry is never sealed"
          , "(private_dir), so a source power loss may lose the whole store and"
          , "its authority even after synchronous=FULL commits. The next run"
          , "re-keys every row and re-reads bytes the destination holds durably:"
          , "MC_wp0g_authority's counterexample without any relaxed setting. The"
          , "transfer before #169 (AdoptUnrowed off), and any output without a"
          , "capture record; MC_r25_strict_unsealed is this bound with the record."
          ]
          (     one
            //  { MaxRuns = 2, MaxCrashes = 1, StoreRootSealed = False }
            //  before169
          )
      , row
          "MC_r25_unrowed_no_adopt"
          (T.Expect.fail P.R25_StrictNoDurableReread)
          [ "FINDING (expected to fail): the transfer before #169, without the"
          , "capture record. A crash after the file seal and the rename but before"
          , "commit_outputs leaves bulkload's own bytes durable at the final path"
          , "with no row; the next run reads the seat again. R25_NoDurableReread"
          , "does not see it (no row), and OI-1003-Q40 keeps that committed-row"
          , "reading as the SLO's obligation. MC_r25_unrowed_bytes is the same"
          , "bound with the adoption, and passes."
          ]
          (strictOne // before169)
      , row
          "MC_wp0d_check_rename"
          (T.Expect.fail P.NoClobber)
          [ "WP0(d) FINDING (expected to fail): the naive design, check the"
          , "output's identity and then rename over it, has a window in which a"
          , "third-party write lands and is clobbered."
          ]
          (     one
            //  { MaxRuns = 2
                , MaxEdits = 1
                , MaxForeign = 1
                , SupersedeMode = Mode.check_rename
                }
          )
      , row
          "MC_neg_live_unfair"
          (T.Expect.fail P.RunsClose)
          [ "NEGATIVE: the same run without fairness may stop short of closure,"
          , "so RunsClose depends on the fairness assumption. No SYMMETRY."
          ]
          one
      ]

{- The primary row of every mutation, keyed by its label: MC_neg_<mutation>,
   which must violate the mutation's verdict. `primaryOf` merges this record
   over T.Mutation, so it has exactly one field per label: a label without a
   field is a "Missing handler" error, and a field without a label an
   "Unused handler" error. Each field holds its own label's mutation
   (asserted below), and grounding.mutations is the list of its fields.
-}
let Primary =
      { mutation : M
      , constants : T.Constants
      , comment : { head : Text, tail : List Text }
      }

let primary =
      { held_before_commit =
        { mutation = M.held_before_commit
        , constants = nvMutation
        , comment =
          { head = "Held{true} is answered before the output's group commit."
          , tail = [ core ]
          }
        }
      , commit_before_fsync =
        { mutation = M.commit_before_fsync
        , constants = nvMutation
        , comment =
          { head =
              "the temporary is renamed and its row committed without the file seal."
          , tail = [ core ]
          }
        }
      , commit_before_dirseal =
        { mutation = M.commit_before_dirseal
        , constants = one
        , comment =
            line
              "the row commits before the directory seal makes the rename durable."
        }
      , adopt_without_seal =
        { mutation = M.adopt_without_seal
        , constants = one // { MaxForeign = 1 }
        , comment =
            line "an adopted existing output is recorded without sealing it."
        }
      , ledger_before_held =
        { mutation = M.ledger_before_held
        , constants = one
        , comment = line "the source ledger records a capture before Held{true}."
        }
      , done_before_sync =
        { mutation = M.done_before_sync
        , constants = one
        , comment =
            line "SourceDone is sent before the ledger's last commit returned."
        }
      , reread_durable =
        { mutation = M.reread_durable
        , constants = one // { MaxRuns = 2, MaxCrashes = 1 }
        , comment =
          { head =
              "the destination never answers Reuse, by its row or by a capture"
          , tail = [ "record (#169), so a rerun re-reads held bytes." ]
          }
        }
      , reread_ignore_ledger =
        { mutation = M.reread_ignore_ledger
        , constants = one // { MaxRuns = 2 }
        , comment =
          { head =
              "the destination never answers Reuse (by row or by record) AND"
          , tail =
            [ "manifest_capture ignores the source ledger, so a rerun re-reads a"
            , "committed capture. In code shape (SupersedeMode \"off\") slo.md's"
            , "R25 wording fails only when both protections are gone;"
            , "reread_durable alone never violates it."
            ]
          }
        }
      , skip_output_row =
        { mutation = M.skip_output_row
        , constants = one
        , comment =
            line
              "the store commit records no output row, so a closed pass holds nothing."
        }
      , double_read =
        { mutation = M.double_read
        , constants = one
        , comment =
            line "a fresh manifest's chunks are read again to serve them (#77 F1)."
        }
      , src_ledger_carries_r25 =
        { mutation = M.src_ledger_carries_r25
        , constants = nvMutation
        , comment =
          { head =
              "WP0(g) counterfactual: Reuse, by row or by capture record, also needs"
          , tail =
            [ "the SOURCE ledger's row. Fails even with a strict ledger: the source"
            , "row always trails the destination commit by the Held round trip, so"
            , "R25 must be carried by the destination."
            , core
            ]
          }
        }
      , record_racy =
        { mutation = M.record_racy
        , constants = one // { MaxEdits = 1 }
        , comment = line "a racy capture is recorded as a reuse key (#86)."
        }
      , untyped_space =
        { mutation = M.untyped_space
        , constants = one // { MaxCommitFails = 1 }
        , comment =
            line
              "a full-disk group surfaces as a bare IO, which closes nothing (#100)."
        }
      , source_write =
        { mutation = M.source_write
        , constants = one
        , comment = line "a capture also writes the source."
        }
      , pause_writer =
        { mutation = M.pause_writer
        , constants = one
        , comment = line "a capture interrupts the source's writer to pause it."
        }
      , git_optional_locks =
        { mutation = M.git_optional_locks
        , constants = one // { EstateReads = True }
        , comment = line "a git read runs without the optional-locks guard."
        }
      , unbounded_backup =
        { mutation = M.unbounded_backup
        , constants = one // { EstateReads = True }
        , comment = line "the SQLite backup takes its lock past max_steps."
        }
      , supersede_unchecked =
        { mutation = M.supersede_unchecked
        , constants = one // { MaxForeign = 1, SupersedeMode = Mode.exchange }
        , comment = line "WP0(d) exchange without the identity check."
        }
      , sweep_displaced =
        { mutation = M.sweep_displaced
        , constants =
                one
            //  { MaxRuns = 3
                , MaxCrashes = 1
                , MaxEdits = 1
                , MaxForeign = 1
                , SupersedeMode = Mode.exchange
                }
        , comment =
          { head =
              "WP0(d) exchange whose recovery sweeps a displaced foreign file like a"
          , tail = [ "temporary." ]
          }
        }
      , adopt_unkeyed =
        { mutation = M.adopt_unkeyed
        , constants = one // { MaxRuns = 2, MaxCrashes = 1, MaxEdits = 1 }
        , comment =
          { head =
              "#169: an unrowed output is adopted without checking that its capture"
          , tail =
            [ "record names this entry's row, so a seat edited since that"
            , "capture is answered from stale bytes."
            ]
          }
        }
      , adopt_unverified =
        { mutation = M.adopt_unverified
        , constants = one // { MaxRuns = 2, MaxCrashes = 1, MaxForeign = 1 }
        , comment =
          { head =
              "#169: an unrowed output is adopted without hashing its bytes against"
          , tail =
            [ "its capture record, so a third party's in-place rewrite of an"
            , "output a crash left unrowed is recorded as the capture."
            ]
          }
        }
      , reuse_ignores_row =
        { mutation = M.reuse_ignores_row
        , constants = one // { MaxRuns = 2 }
        , comment =
          { head =
              "#169: Reuse ignores the output's row (Store::output_matches always"
          , tail =
            [ "false), so a rerun adopts every rowed output again from its capture"
            , "record: hashed, sealed and re-rowed, with 0 source bytes read. No"
            , "R25 or S3 property can see that; AdoptOnlyUnrowed does."
            ]
          }
        }
      , adopt_unrecorded =
        { mutation = M.adopt_unrecorded
        , constants =
            one // { MaxRuns = 3, MaxCrashes = 1, TrackStrictHeld = True }
        , comment =
          { head =
              "#169: an output adopted against a manifest gets no capture record"
          , tail =
            [ "(only a staged publish writes one). A racy capture's output is"
            , "adopted in a later, non-racy run and sealed; a crash before its row"
            , "commits leaves durable bytes with no row and no record, and the next"
            , "run reads the seat again."
            ]
          }
        }
      , owned_ignores_identity =
        { mutation = M.owned_ignores_identity
        , constants =
            one // { MaxRuns = 2, MaxForeign = 1, SupersedeMode = Mode.exchange }
        , comment =
          { head =
              "#187: any file at a path that has an ownership row is superseded,"
          , tail =
            [ "whatever its identity (owned_output without the identity"
            , "comparison): a third party's file that replaced a racy publish is"
            , "exchanged away and removed."
            ]
          }
        }
      , sweep_drops_ownership =
        { mutation = M.sweep_drops_ownership
        , constants =
                one
            //  { MaxRuns = 3
                , MaxCrashes = 1
                , MaxEdits = 1
                , SupersedeMode = Mode.exchange
                }
        , comment =
          { head =
              "#187: the sweep deletes the intent of an interrupted supersede"
          , tail =
            [ "whose staged file is at the leaf without recording its ownership"
            , "(settle_supersede with no owned identity): the new output is left"
            , "with no row of any kind."
            ]
          }
        }
      , exchange_before_intent =
        { mutation = M.exchange_before_intent
        , constants =
                one
            //  { MaxRuns = 2
                , MaxCrashes = 1
                , MaxEdits = 1
                , SupersedeMode = Mode.exchange
                }
        , comment =
          { head =
              "#187: the exchange runs ahead of the intent's commit, which"
          , tail =
            [ "follows it (begin_supersedes after io::exchange). A crash between"
            , "the two leaves the new output beside the old output's rows."
            ]
          }
        }
      , own_is_reuse =
        { mutation = M.own_is_reuse
        , constants = one // { MaxRuns = 2, SupersedeMode = Mode.exchange }
        , comment =
          { head =
              "OI-1003-Q101: an output the path's ownership row names is answered"
          , tail =
            [ "Reuse (output_matches reading owned_outputs): a racy publish"
            , "becomes a reuse source (#86)."
            ]
          }
        }
      , refusal_unbound =
        { mutation = M.refusal_unbound
        , constants =
            one // { MaxRuns = 2, MaxForeign = 2, SupersedeMode = Mode.exchange }
        , comment =
          { head =
              "#187 review: a remembered refusal is answered by its row key alone,"
          , tail =
            [ "without comparing the identity of the file now at the path"
            , "(refused_output), so it outlives the file it was about."
            ]
          }
        }
      , late_exchange_refusal =
        { mutation = M.late_exchange_refusal
        , constants =
                one
            //  { MaxRuns = 2
                , MaxEdits = 1
                , SupersedeMode = Mode.exchange
                , ExchangeSupported = False
                }
        , comment =
          { head =
              "OI-1003-Q100: no exchange probe at the plan, so on a destination"
          , tail =
            [ "with no atomic exchange a changed seat is staged and its chunks"
            , "asked of the source before it is refused (the code before the #187"
            , "review)."
            ]
          }
        }
      }

let primaryOf
    : M -> T.NegRow
    = \(m : M) ->
        let p = merge primary m

        in  { mutation = p.mutation
            , against = T.Against.verdict
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

-- Every mutation label, from the table (each field holds its own label).
let allMutations =
      map PrimaryRow M (\(e : PrimaryRow) -> e.mapValue.mutation) primaryRowsByLabel

{- Mutation configs, in run order: every primary row, and the second rows
   (`also`) that hold the same mutation against another property. Each must
   violate its verdict, or, for an `also` row, the named property; TypeOK is
   checked alongside.
-}
let negRows
    : List T.NegRow
    = [ primaryOf M.held_before_commit
      , primaryOf M.commit_before_fsync
      , primaryOf M.commit_before_dirseal
      , primaryOf M.adopt_without_seal
      , primaryOf M.ledger_before_held
      , primaryOf M.done_before_sync
      , primaryOf M.reread_durable
      , { mutation = M.reread_durable
        , against = also "reread_unchanged" P.S3_UnchangedReadsZero
        , constants = one // { MaxRuns = 2, MaxCrashes = 1 }
        , comment =
          { head =
              "as reread_durable, against S3's unchanged-source clause. It needs"
          , tail =
            [ "the crash: with the source row present, the ledger manifest and an"
            , "adopt still read nothing."
            ]
          }
        }
      , { mutation = M.reread_durable
        , against = also "reread_changed_only" P.S3_ReadsOnlyChanged
        , constants = one // { MaxRuns = 2, MaxCrashes = 1 }
        , comment =
            line "as reread_durable, against S3's changed-seats-only inequality."
        }
      , primaryOf M.reread_ignore_ledger
      , { mutation = M.reread_durable
        , against = also "reread_exchange" P.R25_NoCommittedCaptureReread
        , constants = one // { MaxRuns = 2, SupersedeMode = Mode.exchange }
        , comment =
          { head =
              "as reread_durable, under WP0(d)'s exchange design: the source serves"
          , tail =
            [ "its ledger manifest and re-reads its chunks for the superseding"
            , "publish, a committed capture re-read."
            ]
          }
        }
      , primaryOf M.skip_output_row
      , primaryOf M.double_read
      , primaryOf M.src_ledger_carries_r25
      , primaryOf M.record_racy
      , { mutation = M.record_racy
        , against = also "record_racy_ledger" P.LedgerSound
        , constants = one // { MaxEdits = 1 }
        , comment = line "as record_racy, against the source ledger's manifest."
        }
      , primaryOf M.untyped_space
      , primaryOf M.source_write
      , primaryOf M.pause_writer
      , primaryOf M.git_optional_locks
      , primaryOf M.unbounded_backup
      , primaryOf M.supersede_unchecked
      , primaryOf M.sweep_displaced
      , primaryOf M.adopt_unkeyed
      , primaryOf M.adopt_unverified
      , primaryOf M.reuse_ignores_row
      , primaryOf M.adopt_unrecorded
      , primaryOf M.owned_ignores_identity
      , primaryOf M.sweep_drops_ownership
      , primaryOf M.exchange_before_intent
      , primaryOf M.own_is_reuse
      , primaryOf M.refusal_unbound
      , primaryOf M.late_exchange_refusal
      ]

{- Every mutation label has exactly one primary row in negRows, and the
   primary rows run in mutationIndex order. The expected positions are 0, 1,
   2, ... up to the number of fields of `primary`, which is the number of
   labels of T.Mutation. So a label whose row is left out of negRows, or
   listed twice, fails here, however many labels the union has.
-}
let primaryRows =
      List/fold
        T.NegRow
        negRows
        (List Natural)
        ( \(n : T.NegRow) ->
          \(acc : List Natural) ->
            merge
              { verdict = [ T.mutationIndex n.mutation ] # acc
              , also = \(_ : { suffix : Text, property : P }) -> acc
              }
              n.against
        )
        ([] : List Natural)

let _ =
        assert
      :     primaryRows
        ===  range (List/length PrimaryRow primaryRowsByLabel)

let rows =
        [ budgetSelftest ]
      # positives
      # witnesses
      # findings
      # map T.NegRow T.Row neg negRows

-- Checks over the rows (gen_cfgs.py's assertions, now evaluated by Dhall) -----

let failProperty =
      \(r : T.Row) ->
        merge
          { pass = \(_ : T.Pass) -> [] : List P
          , fail = \(p : P) -> [ p ]
          , reach = \(_ : W) -> [] : List P
          , simulate = \(_ : List P) -> [] : List P
          , inconclusive = [] : List P
          }
          r.expect

let temporal =
      \(r : T.Row) ->
        merge
          { pass = \(p : T.Pass) -> [] : List P
          , fail = \(p : P) -> if T.isTemporal p then [ p ] else [] : List P
          , reach = \(_ : W) -> [] : List P
          , simulate = \(_ : List P) -> [] : List P
          , inconclusive = [] : List P
          }
          r.expect
        # merge
            { pass = \(p : T.Pass) -> p.properties
            , fail = \(_ : P) -> [] : List P
            , reach = \(_ : W) -> [] : List P
            , simulate = \(_ : List P) -> [] : List P
            , inconclusive = [] : List P
            }
            r.expect

let failing = concatMap T.Row P failProperty rows

{- Every safety invariant must be shown falsifiable: a property no fail row
   violates may hold by construction (README.md, "Mutations").
-}
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

-- SYMMETRY is unsound for liveness: no row with a temporal property uses it.
let _ =
        assert
      :     all
              T.Row
              (\(r : T.Row) -> isEmpty P (temporal r) || r.symmetry == False)
              rows
        ===  True

-- Rendering -------------------------------------------------------------------

let checks =
      \(r : T.Row) ->
        merge
          { pass =
              \(p : T.Pass) ->
                { invariants = map P Text showP p.invariants
                , properties = map P Text showP p.properties
                }
          , fail =
              \(p : P) ->
                if    T.isTemporal p
                then  { invariants = [ "TypeOK" ], properties = [ showP p ] }
                else  { invariants = [ "TypeOK", showP p ]
                      , properties = [] : List Text
                      }
          , reach =
              \(w : W) ->
                { invariants = [ "TypeOK", showW w ], properties = [] : List Text }
          , simulate =
              \(ps : List P) ->
                { invariants = map P Text showP ps, properties = [] : List Text }
          , inconclusive =
            { invariants = [ "TypeOK" ], properties = [] : List Text }
          }
          r.expect

let cfgText =
      \(r : T.Row) ->
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
                # (if r.symmetry then [ "SYMMETRY SeatSymmetry" ] else [] : List Text)
                # map Text Text (\(i : Text) -> "INVARIANT ${i}") c.invariants
                # [ "INVARIANT WithinBudget" ]
                # map Text Text (\(p : Text) -> "PROPERTY ${p}") c.properties
              )

let neverColumn =
      \(never : List A) ->
        let sorted =
              filter A (\(a : A) -> any A (actionEq a) never) allActions

        in  if isEmpty A sorted then "-" else join "," (map A Text showA sorted)

let tsvRow =
      \(r : T.Row) ->
        join
          "\t"
          [ r.name
          , showConstructor r.expect
          , merge
              { pass = \(_ : T.Pass) -> "all"
              , fail = showP
              , reach = showW
              , simulate = \(_ : List P) -> "all"
              , inconclusive = "WithinBudget"
              }
              r.expect
          , merge
              { pass = \(p : T.Pass) -> neverColumn p.never
              , fail = \(_ : P) -> "*"
              , reach = \(_ : W) -> "*"
              , simulate = \(_ : List P) -> "*"
              , inconclusive = "*"
              }
              r.expect
          , merge { None = "-", Some = \(f : Text) -> f } r.flags
          ]

let tsv =
      unlines
        (   [ "# TLC configs checked by `just tla-check`, in this order (README.md)."
            , provenance.tsv
            , "# Tab-separated columns:"
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
          # map T.Row Text tsvRow rows
        )

-- Traceability (README.md, "Properties, SLOs, rulings and tests") -------------

let S = T.Slo

let invariants
    : List T.InvariantRow
    = [ { tla = P.R25_NoDurableReread
        , slo = [ S.S3 ]
        , ruling = [ "R25", "R-N58", "OI-1003-Q7", "OI-1003-Q20", "OI-1003-Q37" ]
        , codeSymbol =
          [ "output_matches", "source_bytes_read", "relax_ledger_rows" ]
        , ptest = [ "P23", "P21", "P19", "P24", "P33", "P79" ]
        }
      , { tla = P.R25_NoCommittedCaptureReread
        , slo = [ S.S3 ]
        , ruling = [ "R-N58", "OI-1003-Q7" ]
        , codeSymbol = [ "manifest_capture" ]
        , ptest = [ "P23", "P33" ]
        }
      , { tla = P.ReadOnce
        , slo = [ S.S1, S.S3 ]
        , ruling = [ "R-N58" ]
        , codeSymbol = [ "serve_chunks" ]
        , ptest = [ "P23" ]
        }
      , { tla = P.S3_ReadsOnlyChanged
        , slo = [ S.S3 ]
        , ruling = [ "OI-1003-Q18" ]
        , codeSymbol = [ "capture_file", "source_bytes_read" ]
        , ptest = [ "P21", "P23" ]
        }
      , { tla = P.S3_UnchangedReadsZero
        , slo = [ S.S3 ]
        , ruling = [ "OI-1003-Q6", "R-N58" ]
        , codeSymbol = [ "source_bytes_read" ]
        , ptest = [ "P21", "P32" ]
        }
      , { tla = P.S3_ClosedPassIsHeld
        , slo = [ S.S3 ]
        , ruling = [ "R-N58" ]
        , codeSymbol = [ "commit_outputs" ]
        , ptest = [ "P23" ]
        }
      , { tla = P.RecordImpliesBytes
        , slo = [ S.Durability, S.S4 ]
        , ruling = [ "R-N86", "R-N88" ]
        , codeSymbol = [ "seal_file", "seal_dir", "commit_outputs" ]
        , ptest = [ "P13", "P14", "P16", "P33" ]
        }
      , { tla = P.HeldAfterCommit
        , slo = [ S.S3, S.Durability ]
        , ruling = [ "OI-1001-Q15" ]
        , codeSymbol = [ "answer_held", "settle_held" ]
        , ptest = [ "P23", "P33" ]
        }
      , { tla = P.LedgerAfterHeld
        , slo = [ S.S3 ]
        , ruling = [ "R-N58", "R-N86" ]
        , codeSymbol = [ "commit_captures", "LedgerSync" ]
        , ptest = [ "P17", "P29" ]
        }
      , { tla = P.DoneAfterLedger
        , slo = [ S.Durability ]
        , ruling = [ "wire v5 (design.md)" ]
        , codeSymbol = [ "SourceDone" ]
        , ptest = [ "P4", "P23" ]
        }
      , { tla = P.ReuseSound
        , slo = [ S.S3, S.S5 ]
        , ruling = [ "#86", "R-N76" ]
        , codeSymbol = [ "racy", "output_matches" ]
        , ptest = [ "P19" ]
        }
      , { tla = P.LedgerSound
        , slo = [ S.S3, S.S5 ]
        , ruling = [ "#86", "R-N58" ]
        , codeSymbol = [ "manifest_capture", "row_key" ]
        , ptest = [ "P17", "P19" ]
        }
      , { tla = P.NoClobber
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q18", "R-N119" ]
        , codeSymbol =
          [ "publish_noreplace", "owned_output", "settle_supersedes" ]
        , ptest = [ "P7", "P8", "P26", "P78" ]
        }
      , { tla = P.S2_TypedSourceAccess
        , slo = [ S.S2 ]
        , ruling = [ "OI-1003-Q5", "OI-1003-Q16" ]
        , codeSymbol = [ "git_env" ]
        , ptest = [ "P34" ]
        }
      , { tla = P.S2_BackupLockBounded
        , slo = [ S.S2 ]
        , ruling = [ "OI-1003-Q16" ]
        , codeSymbol = [ "max_steps" ]
        , ptest = [ "P34" ]
        }
      , { tla = P.ClosureAccounted
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q1", "#100" ]
        , codeSymbol = [ "space_refusal" ]
        , ptest = [ "P61" ]
        }
      , { tla = P.RunsClose
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q2" ]
        , codeSymbol = [ "finish_receive" ]
        , ptest = [ "P28", "P23" ]
        }
      , { tla = P.AllRunsFinish
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q2" ]
        , codeSymbol = [ "finish_receive" ]
        , ptest = [ "P28", "P23" ]
        }
      ]

{- One row per frozen safety invariant (TypeOK is a sanity check) and per
   temporal property, in table order. Both lists come from the property
   table, so a property added with either class needs a row here.
-}
let traced =
      \(p : P) ->
        (T.isSafety p && propertyEq p P.TypeOK == False) || T.isTemporal p

let _ =
        assert
      :     map
              T.InvariantRow
              Natural
              (\(r : T.InvariantRow) -> T.propertyIndex r.tla)
              invariants
        ===  map P Natural T.propertyIndex (filter P traced allProperties)

{- #187's four properties (class finding, not frozen): the same traceability
   as a frozen invariant's row, kept apart from `invariants`, whose rows are
   the frozen ones. Their code symbols are grounded with the others.
-}
let supersedeInvariants
    : List T.InvariantRow
    = [ { tla = P.SupersedeAtomic
        , slo = [ S.S4, S.Durability ]
        , ruling = [ "OI-1003-Q18", "OI-1003-Q102", "R-N88" ]
        , codeSymbol =
          [ "begin_supersedes"
          , "settle_supersede"
          , "settle_supersedes"
          , "settle_in"
          ]
        , ptest = [ "P78", "P33" ]
        }
      , { tla = P.OwnershipNeverReuse
        , slo = [ S.S3, S.S5 ]
        , ruling = [ "OI-1003-Q101", "#86", "R-N58" ]
        , codeSymbol = [ "owner_key", "own_in", "output_matches" ]
        , ptest = [ "P19", "P78" ]
        }
      , { tla = P.RememberedRefusalSound
        , slo = [ S.S3, S.S4 ]
        , ruling = [ "OI-1003-Q81", "R-N58" ]
        , codeSymbol =
          [ "refused_output", "remember_refused_output", "verify_settled" ]
        , ptest = [ "P21", "P74" ]
        }
      , { tla = P.ExchangeRefusedUpFront
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q100" ]
        , codeSymbol = [ "exchange_supported", "NoExchange" ]
        , ptest = [ "P78" ]
        }
      ]

-- The N-version explorer's rows (`just formal-nv`) ------------------------------

{- The constants hs/Explorer.hs models: no failed group commit, no space
   refusal, a strict source ledger and store, no superseding publish, no
   estate reads and a sealed state root. #187's destination records (the
   intent, the ownership row, the remembered refusal) and OI-1003-Q100's
   refusal exist only under SupersedeMode exchange, so every row that
   checks them is outside this domain; ExchangeSupported is pinned too. Seats, runs, crashes, edits and
   third-party writes are its bound flags; #169's capture record
   (AdoptUnrowed) and the strict-held ghost (TrackStrictHeld) are its
   --adopt and --strict-held switches (`switches` below).
-}
let explorerModels =
      \(c : T.Constants) ->
            Natural/isZero c.MaxCommitFails
        &&  c.SpaceRefusals == False
        &&  c.RelaxedSourceLedger == False
        &&  c.RelaxedAuthority == False
        &&  merge { off = True, check_rename = False, exchange = False } c.SupersedeMode
        &&  c.EstateReads == False
        &&  c.StoreRootSealed
        &&  c.ExchangeSupported

{- Every mutation row inside the explorer's domain, with its bound as the
   explorer's flags. formal-nv runs each one on the explorer, which must
   violate `property`, as TLC must. For a primary row it also runs the
   mutation once with every safety invariant checked, on the explorer and
   on TLC (`everyInvariant`, a config for scratch, never committed), and
   the two must stop at the same invariant after the same number of states.
-}
let NvRow =
      { name : Text
      , mutation : Text
      , property : Text
      , primary : Bool
      , seats : Text
      , runs : Natural
      , crashes : Natural
      , edits : Natural
      , foreign : Natural
      , switches : Text
      , everyInvariant : Text
      }

let nversion =
      concatMap
        T.NegRow
        NvRow
        ( \(n : T.NegRow) ->
            let c = n.constants

            let r = neg n

            in  if    explorerModels c
                then  [ { name = r.name
                        , mutation = showM n.mutation
                        , property = showP (negProperty n)
                        , primary =
                            merge
                              { verdict = True
                              , also =
                                  \(_ : { suffix : Text, property : P }) -> False
                              }
                              n.against
                        , seats =
                            join
                              ""
                              ( map
                                  Seat
                                  Text
                                  (\(s : Seat) -> showConstructor s)
                                  c.Seats
                              )
                        , runs = c.MaxRuns
                        , crashes = c.MaxCrashes
                        , edits = c.MaxEdits
                        , foreign = c.MaxForeign
                        , switches =
                                (if c.AdoptUnrowed then " --adopt" else "")
                            ++  (if c.TrackStrictHeld then " --strict-held" else "")
                        , everyInvariant =
                            cfgText
                              (     r
                                //  { comment =
                                      [ "formal-nv scratch config, not a row of configs.tsv: ${r.name}"
                                      , "with every safety invariant checked, and #169's where the row's"
                                      , "constants set them, for TLC's first violation."
                                      ]
                                    , expect = T.Expect.simulate (safety # beside c)
                                    }
                              )
                        }
                      ]
                else  [] : List NvRow
        )
        negRows

-- Outputs ----------------------------------------------------------------------

let constantNames =
      map
        { name : Text, pad : Text, value : Text }
        Text
        (\(k : { name : Text, pad : Text, value : Text }) -> k.name)
        (constantLines defaults)

-- GitCarry.tla's part (OI-1003-Q43).
let GitCarry = ./GitCarry.dhall

let module = T.moduleEntry T.Module.BulkloadTransfer

in  { files =
            [ { name = module.tsv, text = tsv } ]
          # map
              T.Row
              { name : Text, text : Text }
              (\(r : T.Row) -> { name = "${r.name}.cfg", text = cfgText r })
              rows
        # GitCarry.files
    , grounding =
      [ { module = showConstructor module.module
        , spec = module.spec
        , tsv = module.tsv
        , operators =
              map P Text showP allProperties
            # map W Text showW allWitnesses
            # map A Text showA allActions
            # [ showConstructor T.Specification.Spec
              , showConstructor T.Specification.LiveSpec
              , "SeatSymmetry"
              , "Init"
              , "Next"
              ]
        , constants = constantNames
        , mutations = map M Text showM allMutations
        , codeSymbols =
            concatMap
              T.InvariantRow
              Text
              (\(r : T.InvariantRow) -> r.codeSymbol)
              (invariants # supersedeInvariants)
        , symbolMatch = showConstructor module.symbols
        , pendingSymbols = [] : List Text
        , labelSets = [] : List { name : Text, labels : List Text }
        }
      , GitCarry.grounding
      ]
    , invariants
    , supersedeInvariants
    , nversion
    , gitCarryInvariants = GitCarry.invariants
    , gitCarryNversion = GitCarry.nversion
    , decide = GitCarry.decide
    }
