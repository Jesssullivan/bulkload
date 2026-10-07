{- GitCarry.tla's part of docs/formal's typed catalogue (Q42 lane L4,
   OI-1003-Q43): every TLC config of the custody model, the verdict of every
   mutation, the traceability row of every property, the explorer's rows,
   and the labels of the decision core's closed unions.

   Catalogue.dhall imports this file and renders it beside
   BulkloadTransfer's configs. Its run order is configs_gc.tsv, and its
   configs are MC_gc_*.cfg. The same checks hold as for BulkloadTransfer:
   every mutation has a verdict and one primary row (total merges), every
   safety invariant but TypeOK has a fail row, and every traced property
   has one traceability row, in table order.
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

let P = T.GcProperty

let A = T.GcAction

let W = T.GcWitness

let M = T.GcMutation

let Item = T.Item

let S = T.Slo

let Id = T.PId

let Lane = T.Lane

let provenance =
      { cfg =
          "\\* Rendered from catalogue/GitCarry.dhall; expected outcome in configs_gc.tsv."
      , tsv =
          "# Rendered from catalogue/GitCarry.dhall by `just tla-render`; edit the catalogue, not this file."
      }

let showP = \(p : P) -> showConstructor p

let showA = \(a : A) -> showConstructor a

let showM = \(m : M) -> showConstructor m

let showW = \(w : W) -> showConstructor w

let propertyEq =
      \(p : P) -> \(q : P) -> natEq (T.gcPropertyIndex p) (T.gcPropertyIndex q)

let actionEq =
      \(a : A) -> \(b : A) -> natEq (T.gcActionIndex a) (T.gcActionIndex b)

-- Every value of each union, from its table in Types.dhall --------------------

let PropertyRow = { mapKey : Text, mapValue : T.GcPropertyEntry }

let propertyRows = toMap T.gcPropertyTable

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
        T.gcPropertyIndex
        ( map
            PropertyRow
            P
            (\(e : PropertyRow) -> e.mapValue.property)
            propertyRows
        )

let _ =
        assert
      :     map P Natural T.gcPropertyIndex allProperties
        ===  range (List/length PropertyRow propertyRows)

let ActionRow = { mapKey : Text, mapValue : T.GcActionEntry }

let actionRows = toMap T.gcActionTable

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
        T.gcActionIndex
        (map ActionRow A (\(e : ActionRow) -> e.mapValue.action) actionRows)

let _ =
        assert
      :     map A Natural T.gcActionIndex allActions
        ===  range (List/length ActionRow actionRows)

let WitnessRow = { mapKey : Text, mapValue : W }

let witnessRows = toMap T.gcWitnessTable

let _ =
        assert
      :     map WitnessRow Text (\(e : WitnessRow) -> e.mapKey) witnessRows
        ===  map WitnessRow Text (\(e : WitnessRow) -> showW e.mapValue) witnessRows

let allWitnesses = map WitnessRow W (\(e : WitnessRow) -> e.mapValue) witnessRows

{- The decision core's labels, each union from its table, each field its own
   label. Types.dhall merges each table over its union (basisSelf, ...,
   decisionSelf), so a table holds exactly its union's labels; the asserts
   below check that each field holds its own label, and for Decision, that
   decisionSelf returns each field's own entry.
-}
let labels =
      \(a : Type) ->
      \(rows : List { mapKey : Text, mapValue : a }) ->
        map { mapKey : Text, mapValue : a } Text (\(e : { mapKey : Text, mapValue : a }) -> e.mapKey) rows

let shown =
      \(a : Type) ->
      \(show : a -> Text) ->
      \(rows : List { mapKey : Text, mapValue : a }) ->
        map
          { mapKey : Text, mapValue : a }
          Text
          (\(e : { mapKey : Text, mapValue : a }) -> show e.mapValue)
          rows

let basisRows = toMap T.basisTable

let rebaseRows = toMap T.rebaseTable

let reuseRows = toMap T.reuseTable

let refusalRows = toMap T.refusalTable

let decisionRows = toMap T.decisionTable

let _ =
        assert
      :     labels T.Basis basisRows
        ===  shown T.Basis (\(x : T.Basis) -> showConstructor x) basisRows

let _ =
        assert
      :     labels T.Rebase rebaseRows
        ===  shown T.Rebase (\(x : T.Rebase) -> showConstructor x) rebaseRows

let _ =
        assert
      :     labels T.ReuseEligibility reuseRows
        ===  shown
               T.ReuseEligibility
               (\(x : T.ReuseEligibility) -> showConstructor x)
               reuseRows

let _ =
        assert
      :     labels T.Refusal refusalRows
        ===  shown T.Refusal (\(x : T.Refusal) -> showConstructor x) refusalRows

let _ =
        assert
      :     labels T.Decision decisionRows
        ===  shown T.Decision (\(x : T.Decision) -> showConstructor x) decisionRows

let _ =
        assert
      :     labels T.Decision decisionRows
        ===  shown
               T.Decision
               (\(x : T.Decision) -> showConstructor (T.decisionSelf x))
               decisionRows

{- Each closed union's labels, in table order, with the name of the
   GitCarry.tla set that must hold exactly them. hs/GitCarryCore.hs's
   `schema` prints the same lines; formal-nv requires them equal.
-}
let decide =
      [ { name = "decision", set = "Decisions", labels = labels T.Decision decisionRows }
      , { name = "basis", set = "Bases", labels = labels T.Basis basisRows }
      , { name = "rebase", set = "Rebases", labels = labels T.Rebase rebaseRows }
      , { name = "reuse", set = "Reuses", labels = labels T.ReuseEligibility reuseRows }
      , { name = "refusal", set = "Refusals", labels = labels T.Refusal refusalRows }
      ]

-- Constants ------------------------------------------------------------------

let constantLines =
      \(c : T.GcConstants) ->
        let mutation =
              merge { None = "none", Some = \(m : M) -> showM m } c.Mutation

        in  [ { name = "Items"
              , pad = "           "
              , value =
                  "{${join ", " (map Item Text (\(i : Item) -> showConstructor i) c.Items)}}"
              }
            , { name = "DepthLimit"
              , pad = "      "
              , value = Natural/show c.DepthLimit
              }
            , { name = "RootWindow"
              , pad = "      "
              , value = Natural/show c.RootWindow
              }
            , { name = "ChainUnderBase"
              , pad = "  "
              , value = showBool c.ChainUnderBase
              }
            , { name = "GCOn", pad = "            ", value = showBool c.GCOn }
            , { name = "MaxCommits"
              , pad = "      "
              , value = Natural/show c.MaxCommits
              }
            , { name = "MaxRewrites"
              , pad = "     "
              , value = Natural/show c.MaxRewrites
              }
            , { name = "MaxCrashes"
              , pad = "      "
              , value = Natural/show c.MaxCrashes
              }
            , { name = "MaxDamage"
              , pad = "       "
              , value = Natural/show c.MaxDamage
              }
            , { name = "DamageBase"
              , pad = "      "
              , value = showBool c.DamageBase
              }
            , { name = "DamageRewrites"
              , pad = "  "
              , value = showBool c.DamageRewrites
              }
            , { name = "BaseMissingTyped"
              , pad = ""
              , value = showBool c.BaseMissingTyped
              }
            , { name = "ReuseManifest"
              , pad = "   "
              , value = showBool c.ReuseManifest
              }
            , { name = "Mutation", pad = "        ", value = "\"${mutation}\"" }
            , { name = "BudgetSeconds"
              , pad = "   "
              , value = Natural/show c.BudgetSeconds
              }
            ]

{- The code today, on one item: v1's depth limit (CHAIN_DEPTH_LIMIT, here
   2 so a chain grows two links in a few commits), no re-root, no chain
   under a base, no GC; every fault on once.
-}
let defaults
    : T.GcConstants
    = { Items = [ Item.i1 ]
      , DepthLimit = 2
      , RootWindow = 0
      , ChainUnderBase = False
      , GCOn = False
      , MaxCommits = 3
      , MaxRewrites = 1
      , MaxCrashes = 1
      , MaxDamage = 1
      , DamageBase = False
      , DamageRewrites =
          {- A third party may rewrite a bundle in place as well as delete
             it. MC_gc_live turns it off: a bundle rewritten at its content
             name blocks recapture at the same tip (MC_gc_live_rewritten).
          -}
          True
      , BaseMissingTyped =
          {- False is the code before lane L6b, which refused a missing
             plan base with a bare IO (#181). The code now refuses it
             SEALED_OBJECT_MISSING (estate::stage_base, chain::flatten), so
             every config with a base sets True; configs without a base
             cannot reach it.
          -}
          False
      , ReuseManifest =
          {- Lane L7's manifest step (the code since L7: every capture
             publishes {bundle}.reuse before its record). Off here: no
             custody definition reads the manifest, so the step is checked
             in its own rows (MC_gc_reuse and reuse_after_record's), and
             every other row keeps its state count of record, which the
             explorer's presets pin.
          -}
          False
      , Mutation = None M
      , BudgetSeconds = 600
      }

{- The custody core (OI-1003-Q43): the shared bound for TLC and
   hs/GitCarryCore.hs's explorer (preset gc_core).
-}
let core = defaults

{- Q46's design on one item (preset gc_q46): a 4-capture root window and
   CORPUS GC. Four commits reach a re-root (depth 2 at age 2) and the
   window's new root (age 3).
-}
let q46 = defaults // { RootWindow = 4, GCOn = True, MaxCommits = 4 }

{- Two items on one plan base, v1's policy (preset gc_grouped): damage may
   reach the base, and apply refuses a missing base by name.
-}
let grouped =
          defaults
      //  { Items = [ Item.i1, Item.i2 ]
          , MaxCommits = 2
          , MaxRewrites = 0
          , DamageBase = True
          , BaseMissingTyped = True
          }

{- L6b's fix 2 (the code's policy) with Q46's re-root and GC (lane L8), on
   two items: a chain kept under the plan base, re-rooted on a based root.
-}
let fix2 =
          grouped
      //  { DepthLimit = 1
          , RootWindow = 3
          , ChainUnderBase = True
          , GCOn = True
          }

{- Q46's re-root twice in one window (preset gc_reroot): depth limit 1, so
   every capture past the first link re-roots, and a 4-capture window. Four
   commits re-root twice (ages 2 and 3), so GC can collect the re-root
   bundle the second re-root abandons, and the window then starts a new
   root (an age of 4 would reach it).
-}
let reroot = q46 // { DepthLimit = 1 }

{- Q46's re-rooted chain extended (preset gc_reroot_extended): RootWindow =
   DepthLimit + 3, so the capture after the re-root (age 3, depth 1) chains
   on it (age 4, depth 2) instead of ending the window. At RootWindow =
   DepthLimit + 2, as in MC_gc_q46, the window ends first.
-}
let rerootExtended = q46 // { RootWindow = 5 }

{- Fix 2 over two links (preset gc_fix2_deep): depth limit 2 and a
   3-capture window, so a chain of two links stays under the plan base and
   a restore imports the base, then flattens both links.
-}
let fix2Deep = fix2 // { DepthLimit = 2 }

{- Lane L7's manifest step on fix 2's bound (the code's policy with Q46's
   GC): every pass publishes its bundle's reuse manifest before its record,
   under crashes, and GC removes a collected bundle's manifest with it.
-}
let reuse = fix2 // { ReuseManifest = True }

-- One item, one commit, no faults: the base of most mutation configs.
let one =
          defaults
      //  { MaxCommits = 1
          , MaxRewrites = 0
          , MaxCrashes = 0
          , MaxDamage = 0
          , BudgetSeconds = 300
          }

-- Properties -----------------------------------------------------------------

let safety = filter P T.gcIsSafety allProperties

{- The verdict of every mutation: the one property its MC_gc_neg_<mutation>
   config must violate. A total merge.
-}
let verdict
    : M -> P
    = \(m : M) ->
        merge
          { chain_ignores_depth = P.ChainDepthBounded
          , gc_deletes_depended = P.GCNeverDeletesDepended
          , base_replaced_live = P.BaseNotReplacedWhileDepended
          , sidecar_after_record = P.SidecarsBeforeRecord
          , skip_flatten_verify = P.PrereqsSatisfiedByEarlierLinks
          , hit_ignores_chain = P.BrokenLinkNeverReuseHit
          , reroot_pre_mismatch = P.PrereqsSatisfiedByEarlierLinks
          , reuse_after_record = P.ReuseManifestBeforeRecord
          }
          m

-- Coverage: the actions a pass row's report must show never enabled.
let noBase = [ A.BaseRecord, A.StartBase ]

-- Never enabled where ReuseManifest is off: lane L7's manifest step.
let noManifest = [ A.ReuseSidecar ]

-- Row constructors -----------------------------------------------------------

let row =
      \(name : Text) ->
      \(expect : T.GcExpect) ->
      \(comment : List Text) ->
      \(constants : T.GcConstants) ->
          { name
          , expect
          , comment
          , constants
          , spec = T.Specification.Spec
          , flags = None Text
          }
        : T.GcRow

-- A pass row with the manifest step on: `never` is its whole never column.
let passWithManifest =
      \(never : List A) ->
        T.GcExpect.pass { invariants = safety, properties = [] : List P, never }

-- A pass row at ReuseManifest = FALSE: ReuseSidecar is never enabled too.
let pass = \(never : List A) -> passWithManifest (never # noManifest)

let line = \(head : Text) -> { head, tail = [] : List Text }

let negProperty =
      \(n : T.GcNegRow) ->
        merge
          { verdict = verdict n.mutation
          , also = \(a : { suffix : Text, property : P }) -> a.property
          }
          n.against

let negName =
      \(n : T.GcNegRow) ->
        let suffix =
              merge
                { verdict = showM n.mutation
                , also = \(a : { suffix : Text, property : P }) -> a.suffix
                }
                n.against

        in  "MC_gc_neg_${suffix}"

let neg =
      \(n : T.GcNegRow) ->
        let property = negProperty n

        in  row
              (negName n)
              (T.GcExpect.fail property)
              (   [ "NEGATIVE: ${n.comment.head}" ]
                # n.comment.tail
                # [ "Mutation ${showM n.mutation} must violate ${showP property}."
                  ]
              )
              (n.constants // { Mutation = Some n.mutation })

-- The configs, in run order ---------------------------------------------------

let budgetSelftest =
      row
        "MC_gc_budget_selftest"
        T.GcExpect.inconclusive
        [ "BUDGET SELF-TEST (expected INCONCLUSIVE). MC_gc_q46's constants with"
        , "a 5 s budget: the search needs far longer, so WithinBudget, and"
        , "nothing else, must trip, which proves GitCarry.tla's budget is"
        , "evaluated per state. The bound is MC_gc_q46's, so a broken budget"
        , "still ends (as a PASS, which tla-check rejects)."
        ]
        (q46 // { BudgetSeconds = 5 })

let positives =
      [ row
          "MC_gc_core"
          (pass (noBase # [ A.GC ]))
          [ "Custody core (OI-1003-Q43), the code today: one item, depth limit"
          , "2, no re-root, no GC; three commits, a history rewrite, a crash and"
          , "a third-party delete or rewrite of a CORPUS bundle. The shared bound"
          , "for TLC and hs/GitCarryCore.hs's explorer (preset gc_core)."
          ]
          core
      , row
          "MC_gc_q46"
          (pass noBase)
          [ "OI-1003-Q46's design on one item: a 4-capture root window (re-root"
          , "at the depth limit, a new root when the window ends) and CORPUS GC"
          , "between passes, with every fault. Explorer preset gc_q46."
          ]
          q46
      , row
          "MC_gc_grouped"
          (pass [ A.GC, A.Rewrite ])
          [ "Two items on one plan base, v1's policy (no chain under the base):"
          , "damage may reach the base, and prepare_base must refuse rather than"
          , "replace it. Apply refuses a missing base by name"
          , "(MC_gc_base_missing_untyped). Explorer preset gc_grouped."
          ]
          grouped
      , row
          "MC_gc_fix2"
          (pass [ A.Rewrite ])
          [ "Lane L6b's fix 2 (the code's policy) with Q46 (lane L8, no code"
          , "yet): two items whose chains stay under the plan base"
          , "(BaseAndChain), re-rooted on a based root at depth limit 1, a"
          , "3-capture window and CORPUS GC; damage may reach the base."
          , "Explorer preset gc_fix2."
          ]
          fix2
      , row
          "MC_gc_reroot"
          (pass noBase)
          [ "OI-1003-Q46's re-root twice in one window, on one item: depth limit"
          , "1 and a 4-capture window, so GC can collect the re-root bundle a"
          , "second re-root abandons; every fault. Explorer preset gc_reroot."
          , "MC_gc_reach_second_reroot shows the second re-root is reached."
          ]
          reroot
      , row
          "MC_gc_reroot_extended"
          (pass noBase)
          [ "OI-1003-Q46's re-rooted chain extended, on one item: depth limit 2"
          , "and a 5-capture window (RootWindow = DepthLimit + 3), so a capture"
          , "chains on a re-root bundle and RootOf, GC and restore run through"
          , "it; every fault. Explorer preset gc_reroot_extended."
          , "MC_gc_reach_reroot_extended shows the extension is reached."
          ]
          rerootExtended
      , row
          "MC_gc_fix2_deep"
          (pass [ A.Rewrite ])
          [ "Lane L6b's fix 2 (the code's policy) over two links: two items,"
          , "depth limit 2 and a 3-capture window (Q46, lane L8, no code yet),"
          , "so a restore imports the plan base and flattens a chain of two"
          , "links under it; CORPUS GC and every fault, damage reaching the"
          , "base. Explorer preset gc_fix2_deep."
          , "MC_gc_reach_based_chain shows such a restore is reached."
          ]
          fix2Deep
      , row
          "MC_gc_reuse"
          (passWithManifest [ A.Rewrite ])
          [ "Lane L7's manifest step (the code since L7) on MC_gc_fix2's bound:"
          , "every pass publishes its bundle's {bundle}.reuse manifest after the"
          , "dependency sidecars and before the record, with crashes between any"
          , "two steps, and GC removes a collected bundle's manifest with it. A"
          , "record never lacks its manifest (ReuseManifestBeforeRecord), and"
          , "every other invariant holds as in MC_gc_fix2: nothing reads it."
          ]
          reuse
      ,     row
              "MC_gc_live"
              ( T.GcExpect.pass
                  { invariants = [ P.TypeOK, P.RestoreOrRecapture ]
                  , properties = [ P.ChainRecovery ]
                  , never = noBase # noManifest
                  }
              )
              [ "Liveness under WF_vars(Protocol): once the environment stops, every"
              , "item whose record does not restore gets one that does (Q46's"
              , "re-root and GC on). Damage deletes bundles only: an in-place"
              , "rewrite blocks recovery while the source holds still"
              , "(MC_gc_live_rewritten), and damage may not reach a base, whose"
              , "loss v1 keeps as visible custody by design."
              ]
              (     q46
                //  { MaxCommits = 3, DamageRewrites = False }
              )
        //  { spec = T.Specification.LiveSpec }
      ]

-- Reachability witnesses: expected REACHED.
let witnesses =
      [ row
          "MC_gc_reach_reroot_extended"
          (T.GcExpect.reach W.Witness_RerootExtended)
          [ "REACH (expected REACHED): at MC_gc_reroot_extended's bound a capture"
          , "chains on a link whose root age exceeds its depth, a re-root bundle,"
          , "so that pass row explores a chain extended past a re-root."
          ]
          rerootExtended
      , row
          "MC_gc_reach_second_reroot"
          (T.GcExpect.reach W.Witness_SecondReroot)
          [ "REACH (expected REACHED): at MC_gc_reroot's bound a chain is"
          , "re-rooted twice (its root age exceeds its depth by two depth"
          , "limits), so that pass row explores the second re-root."
          ]
          reroot
      , row
          "MC_gc_reach_based_chain"
          (T.GcExpect.reach W.Witness_BasedChainRestored)
          [ "REACH (expected REACHED): at MC_gc_fix2_deep's bound a record whose"
          , "chain has two links under a plan base restores, so that pass row"
          , "explores fix 2's flatten after a base import over several links."
          ]
          fix2Deep
      ]

let findings =
      [ row
          "MC_gc_base_missing_untyped"
          (pass [ A.Advance, A.Crash, A.GC, A.Rewrite ])
          [ "CODE FINDING, FIXED by lane L6b (#181; expected to pass): two items"
          , "on one plan base, the base deleted. Before L6b estate::import_base"
          , "staged it with stage_bundle, whose canonicalize fails ENOENT: a bare"
          , "IO, which never counts (S4), and this row failed RestoreOrRecapture."
          , "estate::stage_base and chain::flatten now refuse a missing base"
          , "SEALED_OBJECT_MISSING, as apply_item does for the head bundle. The"
          , "row keeps its name and its bound."
          ]
          (     grouped
            //  { MaxCommits = 0
                , MaxCrashes = 0
                , BaseMissingTyped = True
                , BudgetSeconds = 300
                }
          )
      ,     row
              "MC_gc_live_rewritten"
              (T.GcExpect.fail P.ChainRecovery)
              [ "CODE FINDING (expected to fail): one item whose bundle a third party"
              , "rewrites in place, the source holding still. The record no longer"
              , "restores (DIGEST_MISMATCH), and retained_capture recaptures, but the"
              , "re-export at the same tip has the same bytes, so the same content"
              , "name, and publish_bundle refuses DIGEST_MISMATCH on the rewritten"
              , "file, on every pass, until the source moves. prepare_base does the"
              , "same for a plan base. RestoreOrRecapture holds (each refusal is"
              , "typed); ChainRecovery does not."
              ]
              (one // { MaxCommits = 0, MaxDamage = 1 })
        //  { spec = T.Specification.LiveSpec }
      , row
          "MC_gc_neg_live_unfair"
          (T.GcExpect.fail P.ChainRecovery)
          [ "NEGATIVE: without fairness a behaviour may stop before any capture,"
          , "so ChainRecovery depends on the fairness assumption."
          ]
          (one // { MaxCommits = 0 })
      ]

{- The primary row of every mutation, keyed by its label. A total merge over
   T.GcMutation, as Catalogue.dhall's `primary`.
-}
let Primary =
      { mutation : M
      , constants : T.GcConstants
      , comment : { head : Text, tail : List Text }
      }

let primary =
      { chain_ignores_depth =
        { mutation = M.chain_ignores_depth
        , constants = one // { DepthLimit = 1, MaxCommits = 2 }
        , comment =
            line
              "chainable ignores CHAIN_DEPTH_LIMIT, so a chain outgrows the limit."
        }
      , gc_deletes_depended =
        { mutation = M.gc_deletes_depended
        , constants = one // { GCOn = True }
        , comment =
            line
              "GC keeps only what the records name, forgetting their chain links."
        }
      , base_replaced_live =
        { mutation = M.base_replaced_live
        , constants =
                grouped
            //  { MaxCommits = 1, MaxCrashes = 0, BudgetSeconds = 300 }
        , comment =
          { head =
              "prepare_base replaces a missing base that older deltas depend on."
          , tail =
            [ "One commit: a base re-exported at the same tip has the same content"
            , "name, so only a later tip can replace it."
            ]
          }
        }
      , sidecar_after_record =
        { mutation = M.sidecar_after_record
        , constants = one
        , comment =
            line
              "the {item}.capture record is written before the bundle's .prior."
        }
      , skip_flatten_verify =
        { mutation = M.skip_flatten_verify
        , constants = one // { MaxDamage = 1 }
        , comment =
            line
              "chain::flatten restores without checking digests or prerequisites."
        }
      , hit_ignores_chain =
        { mutation = M.hit_ignores_chain
        , constants = one // { MaxDamage = 1 }
        , comment =
            line
              "retained_capture reuses a record without checking its chain."
        }
      , reroot_pre_mismatch =
        { mutation = M.reroot_pre_mismatch
        , constants = one // { DepthLimit = 1, RootWindow = 3, MaxCommits = 2 }
        , comment =
          { head =
              "a re-root's header declares the head's tips while its .prior names"
          , tail =
            [ "the root: the two are derived separately (ExportOptions.chain and"
            , "the link Prior), so a restore cannot satisfy the header."
            ]
          }
        }
      , reuse_after_record =
        { mutation = M.reuse_after_record
        , constants = one // { ReuseManifest = True }
        , comment =
          { head =
              "the {item}.capture record is written before the bundle's .reuse"
          , tail =
            [ "manifest (lane L7): a crash between the two leaves a record whose"
            , "capture every later pass must fetch whole to reuse."
            ]
          }
        }
      }

let primaryOf
    : M -> T.GcNegRow
    = \(m : M) ->
        let p = merge primary m

        in  { mutation = p.mutation
            , against = T.GcAgainst.verdict
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
      map PrimaryRow M (\(e : PrimaryRow) -> e.mapValue.mutation) primaryRowsByLabel

let negRows
    : List T.GcNegRow
    = [ primaryOf M.chain_ignores_depth
      , primaryOf M.gc_deletes_depended
      , primaryOf M.base_replaced_live
      , primaryOf M.sidecar_after_record
      , primaryOf M.skip_flatten_verify
      , primaryOf M.hit_ignores_chain
      , { mutation = M.hit_ignores_chain
        , against =
            T.GcAgainst.also
              { suffix = "hit_ignores_chain_restore"
              , property = P.RestoreOrRecapture
              }
        , constants = one // { MaxDamage = 1 }
        , comment =
            line
              "as hit_ignores_chain, against RestoreOrRecapture's recapture half."
        }
      , primaryOf M.reroot_pre_mismatch
      , primaryOf M.reuse_after_record
      ]

let primaryPositions =
      List/fold
        T.GcNegRow
        negRows
        (List Natural)
        ( \(n : T.GcNegRow) ->
          \(acc : List Natural) ->
            merge
              { verdict = [ T.gcMutationIndex n.mutation ] # acc
              , also = \(_ : { suffix : Text, property : P }) -> acc
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
      # map T.GcNegRow T.GcRow neg negRows

-- Checks over the rows ----------------------------------------------------------

let failProperty =
      \(r : T.GcRow) ->
        merge
          { pass = \(_ : T.GcPass) -> [] : List P
          , fail = \(p : P) -> [ p ]
          , reach = \(_ : W) -> [] : List P
          , inconclusive = [] : List P
          }
          r.expect

let failing = concatMap T.GcRow P failProperty rows

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
      \(r : T.GcRow) ->
        merge
          { pass =
              \(p : T.GcPass) ->
                { invariants = map P Text showP p.invariants
                , properties = map P Text showP p.properties
                }
          , fail =
              \(p : P) ->
                if    T.gcIsTemporal p
                then  { invariants = [ "TypeOK" ], properties = [ showP p ] }
                else  { invariants = [ "TypeOK", showP p ]
                      , properties = [] : List Text
                      }
          , reach =
              \(w : W) ->
                { invariants = [ "TypeOK", showW w ], properties = [] : List Text }
          , inconclusive =
            { invariants = [ "TypeOK" ], properties = [] : List Text }
          }
          r.expect

let cfgText =
      \(r : T.GcRow) ->
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
      \(never : List A) ->
        let sorted = filter A (\(a : A) -> any A (actionEq a) never) allActions

        in  if isEmpty A sorted then "-" else join "," (map A Text showA sorted)

let tsvRow =
      \(r : T.GcRow) ->
        join
          "\t"
          [ r.name
          , showConstructor r.expect
          , merge
              { pass = \(_ : T.GcPass) -> "all"
              , fail = showP
              , reach = showW
              , inconclusive = "WithinBudget"
              }
              r.expect
          , merge
              { pass = \(p : T.GcPass) -> neverColumn p.never
              , fail = \(_ : P) -> "*"
              , reach = \(_ : W) -> "*"
              , inconclusive = "*"
              }
              r.expect
          , merge { None = "-", Some = \(f : Text) -> f } r.flags
          ]

let tsv =
      unlines
        (   [ "# GitCarry.tla's TLC configs, checked by `just tla-check` in this order (README.md, \"GitCarry\")."
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
          # map T.GcRow Text tsvRow rows
        )

-- Traceability (README.md, "GitCarry") --------------------------------------------

let pending = \(symbol : Text) -> \(lands : Lane) -> { symbol, lands }

let invariants
    : List T.GcInvariantRow
    = [ { tla = P.ChainDepthBounded
        , slo = [ S.S3, S.S4 ]
        , ruling = [ "OI-1003-Q15", "OI-1003-Q46" ]
        , codeSymbol =
          [ "CHAIN_DEPTH_LIMIT"
          , "chainable"
          , "chain_offer"
          , "chain_links"
          , "ExportOptions"
          , "export_pass"
          , "write_capture"
          , "decide"
          , "Rebase"
          ]
        , pending = [ pending "the Q46 re-root window" Lane.L8 ]
        , ptest = [ Id.P67, Id.P71 ]
        }
      , { tla = P.PrereqsSatisfiedByEarlierLinks
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q15", "R-N72" ]
        , codeSymbol =
          [ "flatten"
          , "prerequisites"
          , "verify_bundle"
          , "source_held_tips"
          , "write_bundle"
          , "write_capture"
          , "chain_links"
          , "bind_base"
          ]
        , pending =
          [ pending
              "a re-root's header prerequisites and .prior, from one chain path"
              Lane.L8
          ]
        , ptest = [ Id.P68, Id.P67 ]
        }
      , { tla = P.BrokenLinkNeverReuseHit
        , slo = [ S.S3, S.S4 ]
        , ruling = [ "OI-1003-Q15", "R-N72" ]
        , codeSymbol =
          [ "retained_capture"
          , "chain_links"
          , "LinkBinding"
          , "decide"
          , "Inputs"
          ]
        , pending = [] : List T.PendingSymbol
        , ptest = [ Id.P42, Id.P67 ]
        }
      , { tla = P.BaseNotReplacedWhileDepended
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q15", "R-N72" ]
        , codeSymbol =
          [ "prepare_base", "retained_base", "requires_base", "write_new" ]
        , pending = [] : List T.PendingSymbol
        , ptest = [ Id.P68 ]
        }
      , { tla = P.GCNeverDeletesDepended
        , slo = [ S.S4 ]
        , ruling = [ "OI-1003-Q46" ]
        , codeSymbol = [ "chain_links" ]
        , pending =
          [ pending "STATE and CORPUS GC" Lane.L8
          , pending
              "GC's CORPUS-level exclusive lock, also taken by every capture and apply"
              Lane.L8
          ]
        , ptest = [ Id.P71 ]
        }
      , { tla = P.SidecarsBeforeRecord
        , slo = [ S.Durability, S.S4 ]
        , ruling = [ "OI-1003-Q15", "R-N86" ]
        , codeSymbol =
          [ "capture_item", "publish_bundle", "publish_prior", "publish_sidecars" ]
        , pending = [] : List T.PendingSymbol
        , ptest = [ Id.P70 ]
        }
      , { tla = P.ReuseManifestBeforeRecord
        , slo = [ S.Durability, S.S3 ]
        , ruling = [ "OI-1003-Q42", "OI-1003-Q45", "OI-1003-Q94" ]
        , codeSymbol =
          [ "capture_item"
          , "publish_reuse"
          , "ReuseManifest"
          , "reuse_sidecar"
          , "retained_manifest"
          , "reuse_offer"
          , "manifest_blobs"
          , "ManifestSeat"
          ]
        , pending = [] : List T.PendingSymbol
        , ptest = [ Id.P70, Id.P69 ]
        }
      , { tla = P.RestoreOrRecapture
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q1", "OI-1003-Q46", "R-N72" ]
        , codeSymbol =
          [ "apply_item"
          , "import_base"
          , "stage_base"
          , "stage_bundle"
          , "retained_capture"
          , "bound_base"
          ]
        , pending = [ pending "STATE and CORPUS GC" Lane.L8 ]
        , ptest = [ Id.P68, Id.P69, Id.P71 ]
        }
      , { tla = P.ChainRecovery
        , slo = [ S.S4, S.S5 ]
        , ruling = [ "OI-1003-Q46" ]
        , codeSymbol =
          [ "chainable", "retained_capture", "publish_bundle", "prepare_base" ]
        , pending = [ pending "the Q46 re-root window" Lane.L8 ]
        , ptest = [ Id.P71, Id.P68 ]
        }
      ]

let traced =
      \(p : P) ->
        (T.gcIsSafety p && propertyEq p P.TypeOK == False) || T.gcIsTemporal p

let _ =
        assert
      :     map
              T.GcInvariantRow
              Natural
              (\(r : T.GcInvariantRow) -> T.gcPropertyIndex r.tla)
              invariants
        ===  map P Natural T.gcPropertyIndex (filter P traced allProperties)

-- The explorer's rows (`just formal-nv`) --------------------------------------------

{- Every mutation row: hs/GitCarryCore.hs models all of GitCarry.tla's
   constants and mutations, so each MC_gc_neg_ row runs on the explorer at
   its own bound (its constants as the explorer's flags) and must violate
   its named property. A primary row also runs with every safety invariant,
   on the explorer and on TLC with one worker (`everyInvariant`, a scratch
   config), and both must stop at the same invariant after the same number
   of states.
-}
let NvRow =
      { name : Text
      , mutation : Text
      , property : Text
      , primary : Bool
      , flags : Text
      , everyInvariant : Text
      }

let flag = \(b : Bool) -> if b then "true" else "false"

let explorerFlags =
      \(c : T.GcConstants) ->
        join
          " "
          [ "--items"
          , Natural/show (List/length Item c.Items)
          , "--depth-limit"
          , Natural/show c.DepthLimit
          , "--root-window"
          , Natural/show c.RootWindow
          , "--chain-under-base"
          , flag c.ChainUnderBase
          , "--gc"
          , flag c.GCOn
          , "--commits"
          , Natural/show c.MaxCommits
          , "--rewrites"
          , Natural/show c.MaxRewrites
          , "--crashes"
          , Natural/show c.MaxCrashes
          , "--damage"
          , Natural/show c.MaxDamage
          , "--damage-base"
          , flag c.DamageBase
          , "--damage-rewrites"
          , flag c.DamageRewrites
          , "--base-missing-typed"
          , flag c.BaseMissingTyped
          , "--reuse-manifest"
          , flag c.ReuseManifest
          ]

let nversion =
      map
        T.GcNegRow
        NvRow
        ( \(n : T.GcNegRow) ->
            let r = neg n

            in  { name = r.name
                , mutation = showM n.mutation
                , property = showP (negProperty n)
                , primary =
                    merge
                      { verdict = True
                      , also = \(_ : { suffix : Text, property : P }) -> False
                      }
                      n.against
                , flags = explorerFlags r.constants
                , everyInvariant =
                    cfgText
                      (     r
                        //  { comment =
                              [ "formal-nv scratch config, not a row of configs_gc.tsv: ${r.name}"
                              , "with every safety invariant checked, for TLC's first violation."
                              ]
                            , expect =
                                T.GcExpect.pass
                                  { invariants = safety
                                  , properties = [] : List P
                                  , never = [] : List A
                                  }
                            }
                      )
                }
        )
        negRows

-- Outputs ----------------------------------------------------------------------

let constantNames =
      map
        { name : Text, pad : Text, value : Text }
        Text
        (\(k : { name : Text, pad : Text, value : Text }) -> k.name)
        (constantLines defaults)

let module = T.moduleEntry T.Module.GitCarry

in  { files =
          [ { name = module.tsv, text = tsv } ]
        # map
            T.GcRow
            { name : Text, text : Text }
            (\(r : T.GcRow) -> { name = "${r.name}.cfg", text = cfgText r })
            rows
    , grounding =
      { module = showConstructor module.module
      , spec = module.spec
      , tsv = module.tsv
      , operators =
            map P Text showP allProperties
          # map W Text showW allWitnesses
          # map A Text showA allActions
          # [ showConstructor T.Specification.Spec
            , showConstructor T.Specification.LiveSpec
            , "Init"
            , "Next"
            , "DecideCore"
            , "CaptureOutcome"
            , "ApplyOutcome"
            ]
      , constants = constantNames
      , mutations = map M Text showM allMutations
      , codeSymbols =
          concatMap
            T.GcInvariantRow
            Text
            (\(r : T.GcInvariantRow) -> r.codeSymbol)
            invariants
      , symbolMatch = showConstructor module.symbols
      , pendingSymbols =
          concatMap
            T.GcInvariantRow
            Text
            ( \(r : T.GcInvariantRow) ->
                map
                  T.PendingSymbol
                  Text
                  ( \(s : T.PendingSymbol) ->
                      "${s.symbol} (${showConstructor s.lands})"
                  )
                  r.pending
            )
            invariants
      , labelSets =
          map
            { name : Text, set : Text, labels : List Text }
            { name : Text, labels : List Text }
            ( \(d : { name : Text, set : Text, labels : List Text }) ->
                { name = d.set, labels = d.labels }
            )
            decide
      }
    , invariants
    , nversion
    , decide
    }
