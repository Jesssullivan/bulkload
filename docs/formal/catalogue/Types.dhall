{- Types of docs/formal's typed catalogue (OI-1003-Q32).

   Catalogue.dhall uses these to describe every TLC config of
   BulkloadTransfer.tla and the traceability row of every model property.
   `just tla-render` renders configs.tsv and every MC_*.cfg from it, and
   `just tla-check` refuses to run TLC unless the committed files equal that
   rendering and every name below is grounded (README.md, "Hybrid roles").

   Union labels are the TLA+ names themselves: `showConstructor` turns a
   label into the name TLC reads, so the catalogue cannot misspell one, and
   tla-check's grounding step finds every label in the .tla. The names are
   frozen (README.md, "Frozen names"); do not rename a label.
-}
let Seat = < a | b >

let SupersedeMode = < off | check_rename | exchange >

{- Every deliberate rule break the spec's `Mutations` set knows, except
   "none" (Constants.Mutation is Optional). Catalogue.dhall maps each one to
   the property it must violate with a total `merge`, so a mutation added
   here without a verdict is a type error.
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

let isTemporal =
      \(p : Property) ->
        merge
          { TypeOK = False
          , R25_NoDurableReread = False
          , R25_NoCommittedCaptureReread = False
          , ReadOnce = False
          , S3_ReadsOnlyChanged = False
          , S3_UnchangedReadsZero = False
          , S3_ClosedPassIsHeld = False
          , RecordImpliesBytes = False
          , HeldAfterCommit = False
          , LedgerAfterHeld = False
          , DoneAfterLedger = False
          , ReuseSound = False
          , LedgerSound = False
          , NoClobber = False
          , S2_TypedSourceAccess = False
          , S2_BackupLockBounded = False
          , ClosureAccounted = False
          , WithinBudget = False
          , R25_StrictNoDurableReread = False
          , RunsClose = True
          , AllRunsFinish = True
          }
          p

-- A position per property, for equality (Dhall has none on unions).
let propertyIndex =
      \(p : Property) ->
        merge
          { TypeOK = 0
          , R25_NoDurableReread = 1
          , R25_NoCommittedCaptureReread = 2
          , ReadOnce = 3
          , S3_ReadsOnlyChanged = 4
          , S3_UnchangedReadsZero = 5
          , S3_ClosedPassIsHeld = 6
          , RecordImpliesBytes = 7
          , HeldAfterCommit = 8
          , LedgerAfterHeld = 9
          , DoneAfterLedger = 10
          , ReuseSound = 11
          , LedgerSound = 12
          , NoClobber = 13
          , S2_TypedSourceAccess = 14
          , S2_BackupLockBounded = 15
          , ClosureAccounted = 16
          , WithinBudget = 17
          , R25_StrictNoDurableReread = 18
          , RunsClose = 19
          , AllRunsFinish = 20
          }
          p

-- A position per mutation: the order of the primary mutation rows.
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
          }
          m

-- A position per action: its place in the sorted never column.
let actionIndex =
      \(a : Action) ->
        merge
          { AnswerHeld = 0
          , BackupBegin = 1
          , BackupEnd = 2
          , BackupStepLock = 3
          , BackupStepUnlock = 4
          , CheckOwn = 5
          , Commit = 6
          , CommitFail = 7
          , CrashBoth = 8
          , CrashDst = 9
          , CrashSrc = 10
          , DirSeal = 11
          , Edit = 12
          , Exchange = 13
          , Finish = 14
          , ForeignDelete = 15
          , ForeignWrite = 16
          , GitRead = 17
          , LedgerCommit = 18
          , Publish = 19
          , RecvDecide = 20
          , RecvEnd = 21
          , RecvEntry = 22
          , RecvHeld = 23
          , RecvManifest = 24
          , RecvNeed = 25
          , RecvRefused = 26
          , RenameReplace = 27
          , SealAdopted = 28
          , SealTemp = 29
          , SendSourceDone = 30
          , SilentRewrite = 31
          , StartRun = 32
          , Terminated = 33
          , Tick = 34
          , VerifyDisp = 35
          , Walk = 36
          }
          a

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
    , isTemporal
    , propertyIndex
    , mutationIndex
    , actionIndex
    }
