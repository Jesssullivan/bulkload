{- |
N-version explorer for docs/formal (OI-1003-Q32).

An explicit-state breadth-first search of bulkload's wire v5 transfer,
destination group commit and source ledger, across crashes and reruns, on
the N-version core: one seat, three runs, two crashes, one source edit;
third-party writes, commit failures, the space refusal, superseding publish
and estate reads off (TLC's MC_nv_core). The preset nv_ledger adds one
third-party write or delete (MC_nv_ledger).

TLA+ with TLC is the checker of record. This program is the second
version: an independent implementation of the same transition relation, in
another language and another state representation (one record per seat
instead of one function per variable). It reads no TLA+, no .cfg and no
generated file, and shares no code with the spec. The same actions and the
same invariants are implemented from the model's description
(docs/formal/README.md, "Abstraction map") and the code it cites, where
A = crates/bulkload-agent/src. Its distinct-state count must equal TLC's,
and each core mutation must violate the same named invariant
(docs/formal/README.md, "Hybrid roles").

Only the core's actions are here. Out of the core, and absent: the failed
group commit (CommitFail), the space refusal, a relaxed source ledger or
store, an unsealed state root, the strict-held ghost, WP0(d)'s superseding
publish (CheckOwn, RenameReplace, Exchange, VerifyDisp, the displaced
file) and estate capture's typed reads (GitRead, Backup*). The mutations
that need them are refused at the command line.

Base and containers only:

  runghc docs/formal/hs/Explorer.hs [OPTIONS]
  ghc -O1 -outputdir DIR -o DIR/explorer docs/formal/hs/Explorer.hs

Options:

  --preset nv_core|nv_ledger   the bound (default nv_core)
  --mutation NAME              one deliberate rule break (default none)
  --check all|NAME[,NAME...]   the invariants to check (default all)
  --name NAME                  the row name reported (default from the above)
  --json DIR                   write a counterexample to DIR/NAME.json (DIR
                               must exist)

Output: one line of key=value pairs. Exit 0 when the search completed with
no violation, 1 when it stopped at a violated invariant or a deadlock, 2 on
a usage error.
-}
module Main (main) where

import Data.Char (isAlphaNum)
import Data.List (intercalate, subsequences)
import qualified Data.Map.Strict as M
import qualified Data.Set as S
import System.Environment (getArgs)
import System.Exit (ExitCode (..), exitWith)
import System.IO (hPutStrLn, stderr)

-- ---------------------------------------------------------------------------
-- Bounds and mutations

data Mutation
  = HeldBeforeCommit
  | CommitBeforeFsync
  | CommitBeforeDirseal
  | AdoptWithoutSeal
  | LedgerBeforeHeld
  | DoneBeforeSync
  | RereadDurable
  | RereadIgnoreLedger
  | SkipOutputRow
  | DoubleRead
  | SrcLedgerCarriesR25
  | RecordRacy
  | SourceWrite
  | PauseWriter
  deriving (Eq, Show, Enum, Bounded)

mutationName :: Mutation -> String
mutationName m = case m of
  HeldBeforeCommit -> "held_before_commit"
  CommitBeforeFsync -> "commit_before_fsync"
  CommitBeforeDirseal -> "commit_before_dirseal"
  AdoptWithoutSeal -> "adopt_without_seal"
  LedgerBeforeHeld -> "ledger_before_held"
  DoneBeforeSync -> "done_before_sync"
  RereadDurable -> "reread_durable"
  RereadIgnoreLedger -> "reread_ignore_ledger"
  SkipOutputRow -> "skip_output_row"
  DoubleRead -> "double_read"
  SrcLedgerCarriesR25 -> "src_ledger_carries_r25"
  RecordRacy -> "record_racy"
  SourceWrite -> "source_write"
  PauseWriter -> "pause_writer"

data Config = Config
  { cPreset :: String
  , cSeats :: [Seat]
  , cRuns :: Int -- transfer sessions in one behaviour
  , cCrashes :: Int -- crash budget: either host, or both
  , cEdits :: Int -- source edits per seat
  , cForeign :: Int -- third-party writes or deletes at the destination
  , cMutation :: Maybe Mutation
  }

preset :: String -> Maybe Config
preset "nv_core" = Just (Config "nv_core" "a" 3 2 1 0 Nothing)
preset "nv_ledger" = Just (Config "nv_ledger" "a" 3 2 1 1 Nothing)
preset _ = Nothing

mut :: Config -> Mutation -> Bool
mut cfg m = cMutation cfg == Just m

-- Content is a version number. These sentinels stay far above any version.
garbage, foreignBytes, firstForeignId, keyBase :: Int
garbage = 98 -- what a power loss leaves in data that was never sealed
foreignBytes = 99 -- content a third party wrote
firstForeignId = 50 -- bulkload's own output identities are run numbers
keyBase = 10 -- a row key is authority epoch * keyBase + stat version

-- ---------------------------------------------------------------------------
-- State

type Seat = Char

-- Source entry phase (A/transfer.rs Outbound).
data SEnt = SIdle | SUnwalked | SOffered | SManifested | SAwaitHeld | SDone
  deriving (Eq, Ord, Show)

-- Destination entry phase (A/transfer.rs Inbound).
data DEnt = DIdle | DNone | DStreaming | DAwaitManifest | DFilling | DQueued | DDone
  deriving (Eq, Ord, Show)

data Plan = PlanNone | PlanWrite | PlanAdopt
  deriving (Eq, Ord, Show)

-- Group-commit pipeline stage of an output (A/materialize.rs PublishSink).
data Pub
  = PubNone
  | PubStaged
  | PubSealed
  | PubRenamed
  | PubReady
  | PubAdoptWait
  | PubCommitted
  | PubFailedOccupied
  deriving (Eq, Ord, Show)

data Kind = KindNone | KindWrite | KindAdopt
  deriving (Eq, Ord, Show)

data TmpSt = TmpNone | TmpWritten | TmpSealed
  deriving (Eq, Ord, Show)

data Decision = Reuse | Send | WantManifest
  deriving (Eq, Ord, Show)

data Refusal = SourceChanged | GitOccupied
  deriving (Eq, Ord, Show)

-- The one control frame in flight for an entry (P/frame.rs Control).
data MsgT = MNone | MEntry | MDecide | MManifest | MNeed | MEnd | MRefused | MHeld
  deriving (Eq, Ord, Show)

data Code = CNone | CDecision Decision | CRefusal Refusal
  deriving (Eq, Ord, Show)

data Msg = Msg {mT :: !MsgT, mV :: !Int, mB :: !Bool, mC :: !Code}
  deriving (Eq, Ord, Show)

-- An entry's closure outcome this run (S4).
data Outc = ONone | OPending | OApplied | OAppliedRacy | ORefused Refusal
  deriving (Eq, Ord, Show)

-- A file at a destination path: present, identity, content, data durable,
-- name durable.
data File = File {fPres :: !Bool, fId :: !Int, fData :: !Int, fDD :: !Bool, fND :: !Bool}
  deriving (Eq, Ord, Show)

data Tmp = Tmp {tSt :: !TmpSt, tData :: !Int}
  deriving (Eq, Ord, Show)

-- The capture the source offered: content, racy, recordable, from the ledger.
data Cap = Cap {capData :: !Int, capRacy :: !Bool, capRec :: !Bool, capLed :: !Bool}
  deriving (Eq, Ord, Show)

-- The output record the destination will commit.
data Rec = Rec {rKey :: !Int, rData :: !Int, rRacy :: !Bool, rId :: !Int, rKind :: !Kind}
  deriving (Eq, Ord, Show)

-- A source ledger row (digest-only capture) and a destination output row.
data LRow = LRow {lSeat :: !Seat, lKey :: !Int, lData :: !Int}
  deriving (Eq, Ord, Show)

data DRow = DRow {drSeat :: !Seat, drKey :: !Int, drId :: !Int, drData :: !Int}
  deriving (Eq, Ord, Show)

data ReadKind = Full | Chunks
  deriving (Eq, Ord, Show)

-- One content read of the source, with what was held when it happened.
data ReadRec = ReadRec
  { rdRun :: !Int
  , rdSeat :: !Seat
  , rdKey :: !Int
  , rdKind :: !ReadKind
  , rdHeld :: !Bool
  , rdCommitted :: !Bool
  }
  deriving (Eq, Ord, Show)

-- Kinds of source access (S2): typed reads, and the two a mutation adds.
data Op = OpStat | OpRead | OpWrite | OpProcCtl
  deriving (Eq, Ord, Show)

data Sess = Idle | On | Broken | Done
  deriving (Eq, Ord, Show)

-- The source store. A sealed state root and a durable creation commit mean
-- it is durable as soon as a session creates it.
data Store = StoreNone | StoreDurable
  deriving (Eq, Ord, Show)

-- Everything about one seat: its live source, both processes' view of its
-- entry, its destination path, and its per-run ghosts.
data SeatSt = SeatSt
  { srcStat :: !Int -- stat identity version (only grows)
  , edits :: !Int -- content version
  , fresh :: !Bool -- inside the racy window of "now"
  , sEnt :: !SEnt
  , sRow :: !Int -- the stat identity the walk offered
  , sCap :: !Cap
  , msg :: !Msg
  , dEnt :: !DEnt
  , dKey :: !Int -- row key of the entry
  , dPlan :: !Plan
  , dPub :: !Pub
  , dRec :: !Rec
  , tmp :: !Tmp -- the staged temporary beside the output
  , out :: !File -- the file at the final path
  , outPrev :: !File -- the durable view of the path before a pending rename
  , rc :: !Int -- content reads this run
  , committedRun :: !Bool -- the output's group commit returned this run
  , outc :: !Outc
  }
  deriving (Eq, Ord, Show)

data St = St
  { seats :: !(M.Map Seat SeatSt)
  , sPend :: !(S.Set LRow) -- Held{true}, submitted to the ledger, not committed
  , srcLedger :: !(S.Set LRow)
  , srcDone :: !Bool
  , srcAuth :: !Int -- the source store's authority epoch (0: no store)
  , srcStore :: !Store
  , dstRows :: !(S.Set DRow)
  , run :: !Int
  , sess :: !Sess
  , crashes :: !Int
  , foreignN :: !Int
  , readLog :: !(S.Set ReadRec)
  , srcOps :: !(S.Set Op)
  , runReads :: !(S.Set Seat)
  , heldAtStart :: !(S.Set Seat)
  , changedRun :: !(S.Set Seat)
  , everCommitted :: !(S.Set (Seat, Int))
  }
  deriving (Eq, Ord, Show)

noFile :: File
noFile = File False 0 0 True True

noTmp :: Tmp
noTmp = Tmp TmpNone 0

noCap :: Cap
noCap = Cap 0 False False False

noRec :: Rec
noRec = Rec 0 0 False 0 KindNone

noMsg :: Msg
noMsg = Msg MNone 0 False CNone

durable :: File -> File
durable f = if fPres f then f {fDD = True, fND = True} else f

-- Every seat may start inside the racy window or outside it.
initial :: Config -> [St]
initial cfg =
  [ St
      { seats = M.fromList (zip (cSeats cfg) (map seat0 window))
      , sPend = S.empty
      , srcLedger = S.empty
      , srcDone = False
      , srcAuth = 0
      , srcStore = StoreNone
      , dstRows = S.empty
      , run = 0
      , sess = Idle
      , crashes = 0
      , foreignN = 0
      , readLog = S.empty
      , srcOps = S.empty
      , runReads = S.empty
      , heldAtStart = S.empty
      , changedRun = S.empty
      , everCommitted = S.empty
      }
  | window <- mapM (const [False, True]) (cSeats cfg)
  ]
  where
    seat0 w =
      SeatSt
        { srcStat = 0
        , edits = 0
        , fresh = w
        , sEnt = SIdle
        , sRow = 0
        , sCap = noCap
        , msg = noMsg
        , dEnt = DIdle
        , dKey = 0
        , dPlan = PlanNone
        , dPub = PubNone
        , dRec = noRec
        , tmp = noTmp
        , out = noFile
        , outPrev = noFile
        , rc = 0
        , committedRun = False
        , outc = ONone
        }

-- ---------------------------------------------------------------------------
-- Derived facts

at :: St -> Seat -> SeatSt
at st s = seats st M.! s

put :: Seat -> SeatSt -> St -> St
put s x st = st {seats = M.insert s x (seats st)}

each :: (SeatSt -> SeatSt) -> St -> St
each f st = st {seats = M.map f (seats st)}

seatsOf :: St -> [Seat]
seatsOf = M.keys . seats

-- The key the walk offers (the live stat identity) and the walked row's key
-- (A/transfer_store.rs row_key over the session's authority).
wireKey, walkedKey :: St -> Seat -> Int
wireKey st s = srcAuth st * keyBase + srcStat (at st s)
walkedKey st s = srcAuth st * keyBase + sRow (at st s)

ledgerRows :: St -> Seat -> Int -> [LRow]
ledgerRows st s k = [r | r <- S.toList (srcLedger st), lSeat r == s, lKey r == k]

-- Store::output_matches: a committed row under key k names the identity of
-- the file now at the path.
rowMatches :: St -> Seat -> Int -> Bool
rowMatches st s k =
  let o = out (at st s)
   in fPres o && any (\r -> drSeat r == s && drKey r == k && drId r == fId o) (S.toList (dstRows st))

sound :: File -> Bool
sound o = fDD o && fND o && fData o /= garbage

destHolds :: St -> Seat -> Int -> Bool
destHolds st s k = rowMatches st s k && sound (out (at st s))

-- What R25 is about: a committed row, recorded from stat version v under any
-- authority, vouches for the durable file at the path.
heldPhys :: St -> Seat -> Int -> Bool
heldPhys st s v =
  let o = out (at st s)
   in fPres o
        && sound o
        && any
          (\r -> drSeat r == s && drKey r `mod` keyBase == v && drId r == fId o)
          (S.toList (dstRows st))

-- Nothing in either pipeline: a new session may open the publisher.
quiescent :: St -> Bool
quiescent st =
  all ((`elem` [PubNone, PubCommitted, PubFailedOccupied]) . dPub) (M.elems (seats st))
    && S.null (sPend st)

-- capture_file and serve_chunks check the stat identity before any byte.
captureOK :: SeatSt -> Bool
captureOK x = srcStat x == sRow x

mutationOps :: Config -> S.Set Op
mutationOps cfg =
  S.fromList ([OpWrite | mut cfg SourceWrite] ++ [OpProcCtl | mut cfg PauseWriter])

-- One content read of seat s, judged in the pre-state st and recorded in
-- the successor st'.
readSeat :: Config -> St -> Seat -> ReadKind -> St -> St
readSeat cfg st s kind st' =
  let k = walkedKey st s
      entry =
        ReadRec
          { rdRun = run st
          , rdSeat = s
          , rdKey = k
          , rdKind = kind
          , rdHeld = heldPhys st s (sRow (at st s))
          , rdCommitted = not (null (ledgerRows st s k)) && destHolds st s k
          }
      x' = at st' s
   in put
        s
        x' {rc = rc x' + 1}
        st'
          { readLog = S.insert entry (readLog st')
          , runReads = S.insert s (runReads st')
          , srcOps = S.unions [srcOps st', S.singleton OpRead, mutationOps cfg]
          }

-- ---------------------------------------------------------------------------
-- Actions. Each returns its successors, labelled; none when disabled.

type Step = (String, St)

label :: String -> Seat -> String -> String
label name s detail = name ++ "(" ++ [s] ++ ")" ++ (if null detail then "" else " " ++ detail)

successors :: Config -> St -> [Step]
successors cfg st =
  concat
    [ startRun cfg st
    , commit cfg st
    , ledgerCommit st
    , sendSourceDone cfg st
    , finish st
    , concat [act cfg st s | s <- seatsOf st, act <- seatActions]
    , concat [act cfg st s | s <- seatsOf st, act <- [edit, silentRewrite, foreignWrite, foreignDelete]]
    , tick st
    , crashSrc cfg st
    , crashDst cfg st
    , crashBoth cfg st
    , terminated cfg st
    ]
  where
    seatActions =
      [ walk
      , recvEntry
      , recvDecide
      , recvManifest
      , recvNeed
      , recvEnd
      , recvRefused
      , answerHeld
      , recvHeld
      , sealTemp
      , publish
      , dirSeal
      , sealAdopted
      ]

-- A/transfer.rs receive and serve: Store::open (creating the source store and
-- its authority when there is none), the exclusive publisher, sweep_root.
startRun :: Config -> St -> [Step]
startRun cfg st
  | run st < cRuns cfg && sess st `elem` [Idle, Done, Broken] && quiescent st =
      let (auth, store) = case srcStore st of
            StoreNone -> (srcAuth st + 1, StoreDurable)
            known -> (srcAuth st, known)
          held = S.fromList [s | s <- seatsOf st, heldPhys st s (srcStat (at st s))]
          begin x =
            x
              { sEnt = SUnwalked
              , sCap = noCap
              , msg = noMsg
              , dEnt = DNone
              , dPlan = PlanNone
              , dPub = PubNone
              , dRec = noRec
              , tmp = noTmp
              , rc = 0
              , committedRun = False
              , outc = OPending
              }
       in [ ( "StartRun"
            , (each begin st)
                { srcAuth = auth
                , srcStore = store
                , run = run st + 1
                , sess = On
                , srcDone = False
                , heldAtStart = held
                , runReads = S.empty
                , changedRun = S.empty
                }
            )
          ]
  | otherwise = []

-- A/transfer.rs walk_source, Outbound::offer: metadata only. The Entry says
-- whether the source ledger holds the row.
walk :: Config -> St -> Seat -> [Step]
walk _ st s
  | sess st == On && sEnt x == SUnwalked && msg x == noMsg =
      let k = wireKey st s
       in [ ( label "Walk" s ""
            , put
                s
                x {sEnt = SOffered, sRow = srcStat x, msg = Msg MEntry k (not (null (ledgerRows st s k))) CNone}
                st {srcOps = S.insert OpStat (srcOps st)}
            )
          ]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs Inbound::entry: Reuse when the output's identity is the one
-- recorded under the key; otherwise WantManifest for an existing output, and
-- Send or WantManifest (salvage may fill chunks) for an absent one.
recvEntry :: Config -> St -> Seat -> [Step]
recvEntry cfg st s
  | sess st == On && dEnt x == DNone && mT (msg x) == MEntry =
      let k = mV (msg x)
          reuse =
            rowMatches st s k
              && not (mut cfg RereadDurable || mut cfg RereadIgnoreLedger)
              && (not (mut cfg SrcLedgerCarriesR25) || mB (msg x))
          choices
            | reuse = [Reuse]
            | fPres (out x) = [WantManifest]
            | otherwise = [Send, WantManifest]
          phase d = case d of
            Reuse -> DDone
            Send -> DStreaming
            WantManifest -> DAwaitManifest
          decided d =
            put
              s
              x
                { msg = Msg MDecide 0 False (CDecision d)
                , dKey = k
                , dEnt = phase d
                , outc = if d == Reuse then OApplied else OPending
                }
              st
       in [(label "RecvEntry" s (show d), decided d) | d <- choices]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs Outbound::decide, send_capture, manifest_capture,
-- capture_file: a racy capture is sent but never recorded; a manifest comes
-- from the ledger without reading when the ledger has the row.
recvDecide :: Config -> St -> Seat -> [Step]
recvDecide cfg st s
  | sess st == On && sEnt x == SOffered && mT (msg x) == MDecide =
      let racy = fresh x
          recordable = not racy || mut cfg RecordRacy
          k = walkedKey st s
          hits = ledgerRows st s k
          fromLedger = not (null hits) && not (mut cfg RereadIgnoreLedger)
          early p
            | mut cfg LedgerBeforeHeld && recordable = p {sPend = S.insert (LRow s k (edits x)) (sPend p)}
            | otherwise = p
          captured = Cap (edits x) racy recordable False
          refused = Msg MRefused 0 False (CRefusal SourceChanged)
       in case mC (msg x) of
            CDecision Reuse -> [(label "RecvDecide" s "retire", put s x {sEnt = SDone, msg = noMsg} st)]
            CDecision Send
              | captureOK x ->
                  [ ( label "RecvDecide" s "send"
                    , readSeat cfg st s Full (early (put s x {msg = Msg MEnd (edits x) racy CNone, sEnt = SAwaitHeld, sCap = captured} st))
                    )
                  ]
            CDecision WantManifest
              | fromLedger -> case hits of
                  [r] ->
                    [ ( label "RecvDecide" s "ledger manifest"
                      , put s x {msg = Msg MManifest (lData r) False CNone, sEnt = SManifested, sCap = Cap (lData r) False False True} st
                      )
                    ]
                  _ -> error "two source ledger rows under one key"
              | captureOK x ->
                  [ ( label "RecvDecide" s "manifest"
                    , readSeat cfg st s Full (early (put s x {msg = Msg MManifest (edits x) racy CNone, sEnt = SManifested, sCap = captured} st))
                    )
                  ]
            _ -> [(label "RecvDecide" s "refused", put s x {msg = refused, sEnt = SDone} st)]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs Inbound::manifest, plan_file: adopt an existing output
-- (verified at End), stage an absent one; ask for chunks only to write.
recvManifest :: Config -> St -> Seat -> [Step]
recvManifest _ st s
  | sess st == On && dEnt x == DAwaitManifest && mT (msg x) == MManifest =
      let plan = if fPres (out x) then PlanAdopt else PlanWrite
       in [ ( label "RecvManifest" s (if plan == PlanAdopt then "adopt" else "write")
            , put s x {dPlan = plan, msg = Msg MNeed 0 (plan /= PlanAdopt) CNone, dEnt = DFilling} st
            )
          ]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs serve_chunks: a fresh manifest's chunks are retained; a
-- ledger manifest's chunks are re-read (pread) under an unchanged identity
-- and re-verified.
recvNeed :: Config -> St -> Seat -> [Step]
recvNeed cfg st s
  | sess st == On && sEnt x == SManifested && mT (msg x) == MNeed =
      let c = sCap x
          ended = Msg MEnd (capData c) (capRacy c) CNone
          refused = Msg MRefused 0 False (CRefusal SourceChanged)
          reread = mB (msg x) && (capLed c || mut cfg DoubleRead)
       in if not reread
            then [(label "RecvNeed" s "retained", put s x {msg = ended, sEnt = SAwaitHeld} st)]
            else
              if captureOK x && edits x == capData c
                then [(label "RecvNeed" s "pread", readSeat cfg st s Chunks (put s x {msg = ended, sEnt = SAwaitHeld} st))]
                else
                  if captureOK x
                    then [(label "RecvNeed" s "digest mismatch", readSeat cfg st s Chunks (put s x {msg = refused, sEnt = SDone} st))]
                    else [(label "RecvNeed" s "refused", put s x {msg = refused, sEnt = SDone} st)]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs Inbound::end: stage a written output for its group commit;
-- verify an adopted one byte for byte, or refuse it occupied.
recvEnd :: Config -> St -> Seat -> [Step]
recvEnd _ st s
  | sess st == On && mT (msg x) == MEnd && dEnt x `elem` [DStreaming, DFilling] =
      let d = mV (msg x)
          racy = mB (msg x)
          o = out x
          plan = if dEnt x == DStreaming then PlanWrite else dPlan x
       in case plan of
            PlanWrite ->
              [ ( label "RecvEnd" s "stage"
                , put s x {tmp = Tmp TmpWritten d, dPub = PubStaged, dRec = Rec (dKey x) d racy 0 KindWrite, dEnt = DQueued, msg = noMsg} st
                )
              ]
            PlanAdopt
              | fPres o && fData o == d ->
                  [ ( label "RecvEnd" s "adopt"
                    , put s x {dPub = PubAdoptWait, dRec = Rec (dKey x) d racy (fId o) KindAdopt, dEnt = DQueued, msg = noMsg} st
                    )
                  ]
            _ ->
              [ ( label "RecvEnd" s "occupied"
                , put s x {msg = Msg MHeld 0 False CNone, dEnt = DDone, outc = ORefused GitOccupied} st
                )
              ]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs Inbound::refused.
recvRefused :: Config -> St -> Seat -> [Step]
recvRefused _ st s
  | sess st == On && mT (msg x) == MRefused && dEnt x `elem` [DStreaming, DAwaitManifest, DFilling] =
      case mC (msg x) of
        CRefusal r -> [(label "RecvRefused" s "", put s x {dEnt = DDone, outc = ORefused r, msg = noMsg} st)]
        _ -> error "a refused frame without its code"
  | otherwise = []
  where
    x = at st s

-- The pipeline below also runs after a session broke: the dropped Committer
-- still commits what is pending (A/io/durable.rs Committer).

-- A/materialize.rs StagedFile::seal.
sealTemp :: Config -> St -> Seat -> [Step]
sealTemp _ st s
  | dPub x == PubStaged = [(label "SealTemp" s "", put s x {tmp = (tmp x) {tSt = TmpSealed}, dPub = PubSealed} st)]
  | otherwise = []
  where
    x = at st s

-- A/materialize.rs StagedFile::publish (publish_noreplace): an occupied leaf
-- refuses; the temporary is gone either way.
publish :: Config -> St -> Seat -> [Step]
publish cfg st s
  | rKind (dRec x) == KindWrite && (dPub x == PubSealed || (mut cfg CommitBeforeFsync && dPub x == PubStaged)) =
      if fPres (out x)
        then [(label "Publish" s "occupied", put s x {dPub = PubFailedOccupied, tmp = noTmp} st)]
        else
          let t = tmp x
              placed = File True (run st) (tData t) (tSt t == TmpSealed) False
           in [ ( label "Publish" s ""
                , put s x {out = placed, outPrev = out x, dRec = (dRec x) {rId = run st}, dPub = PubRenamed, tmp = noTmp} st
                )
              ]
  | otherwise = []
  where
    x = at st s

-- A/materialize.rs TouchedDevices::seal: the rename becomes durable.
dirSeal :: Config -> St -> Seat -> [Step]
dirSeal _ st s
  | dPub x == PubRenamed =
      let o = out x
          o' = if fPres o && fId o == rId (dRec x) then o {fND = True} else o
       in [(label "DirSeal" s "", put s x {out = o', dPub = PubReady} st)]
  | otherwise = []
  where
    x = at st s

-- A/materialize.rs PublishSink::commit, Publication::Adopted.
sealAdopted :: Config -> St -> Seat -> [Step]
sealAdopted cfg st s
  | dPub x == PubAdoptWait =
      let o = out x
          o' = if fPres o && fId o == rId (dRec x) && not (mut cfg AdoptWithoutSeal) then durable o else o
       in [(label "SealAdopted" s "", put s x {out = o', dPub = PubReady} st)]
  | otherwise = []
  where
    x = at st s

-- A/transfer_store.rs StorePublisher::commit_outputs: one transaction for
-- any non-empty group of sealed outputs. A racy capture keeps no row, and
-- replaces any row under its key.
commit :: Config -> St -> [Step]
commit cfg st =
  [ ("Commit {" ++ g ++ "}", commitGroup g)
  | g <- filter (not . null) (subsequences ready)
  ]
  where
    ready =
      [ s
      | (s, x) <- M.toList (seats st)
      , dPub x == PubReady || (mut cfg CommitBeforeDirseal && dPub x == PubRenamed)
      ]
    commitGroup g =
      let keyOf s = rKey (dRec (at st s))
          recorded
            | mut cfg SkipOutputRow = []
            | otherwise = [s | s <- g, not (rRacy (dRec (at st s))) || mut cfg RecordRacy]
          kept = S.filter (\r -> not (any (\s -> drSeat r == s && drKey r == keyOf s) g)) (dstRows st)
          added = S.fromList [DRow s (keyOf s) (rId (dRec (at st s))) (rData (dRec (at st s))) | s <- recorded]
          done x = x {dPub = PubCommitted, committedRun = True}
       in st
            { seats = foldr (M.adjust done) (seats st) g
            , dstRows = S.union kept added
            , everCommitted = S.union (everCommitted st) (S.fromList [(s, keyOf s) | s <- recorded])
            }

-- A/transfer.rs Inbound::answer_held, settle_held: Held{true} only after
-- the group commit returned; Held{false} for an occupied path.
answerHeld :: Config -> St -> Seat -> [Step]
answerHeld cfg st s
  | sess st == On && dEnt x == DQueued && msg x == noMsg =
      let held b o = put s x {msg = Msg MHeld 0 b CNone, outc = o, dEnt = DDone} st
       in [ (label "AnswerHeld" s "true", held True (if rRacy (dRec x) then OAppliedRacy else OApplied))
          | dPub x == PubCommitted
          ]
            ++ [(label "AnswerHeld" s "false", held False (ORefused GitOccupied)) | dPub x == PubFailedOccupied]
            ++ [ (label "AnswerHeld" s "early", held True OApplied)
               | mut cfg HeldBeforeCommit
               , dPub x `elem` [PubStaged, PubSealed, PubRenamed, PubReady, PubAdoptWait]
               ]
  | otherwise = []
  where
    x = at st s

-- A/transfer.rs Outbound::handle (Event::Held): only a recordable capture
-- that did not come from the ledger is submitted, and only on Held{true}.
recvHeld :: Config -> St -> Seat -> [Step]
recvHeld cfg st s
  | sess st == On && sEnt x == SAwaitHeld && mT (msg x) == MHeld =
      let c = sCap x
          submit = mB (msg x) && capRec c && not (mut cfg LedgerBeforeHeld)
          pend = if submit then S.insert (LRow s (walkedKey st s) (capData c)) (sPend st) else sPend st
       in [(label "RecvHeld" s "", put s x {sEnt = SDone, msg = noMsg} st {sPend = pend})]
  | otherwise = []
  where
    x = at st s

-- A/transfer_store.rs LedgerSink::publish, commit_captures: one transaction.
ledgerCommit :: St -> [Step]
ledgerCommit st
  | not (S.null pend) =
      let replaced r = any (\p -> lSeat p == lSeat r && lKey p == lKey r) (S.toList pend)
       in [("LedgerCommit", st {srcLedger = S.union (S.filter (not . replaced) (srcLedger st)) pend, sPend = S.empty})]
  | otherwise = []
  where
    pend = sPend st

-- A/transfer.rs serve: committer.sync() returns before SourceDone.
sendSourceDone :: Config -> St -> [Step]
sendSourceDone cfg st
  | sess st == On
      && not (srcDone st)
      && all ((== SDone) . sEnt) (M.elems (seats st))
      && (S.null (sPend st) || mut cfg DoneBeforeSync) =
      [("SendSourceDone", st {srcDone = True})]
  | otherwise = []

-- A/transfer.rs Inbound::run, finish_receive.
finish :: St -> [Step]
finish st
  | sess st == On && srcDone st && all settled (M.elems (seats st)) =
      [("Finish", (each (\x -> x {tmp = noTmp}) st) {sess = Done})]
  | otherwise = []
  where
    settled x =
      dEnt x == DDone && msg x == noMsg && dPub x `elem` [PubNone, PubCommitted, PubFailedOccupied]

-- The environment: live source writers, the racy window, third parties.

edit :: Config -> St -> Seat -> [Step]
edit cfg st s
  | edits x < cEdits cfg =
      [ ( label "Edit" s ""
        , put s x {srcStat = srcStat x + 1, edits = edits x + 1, fresh = True} st {changedRun = S.insert s (changedRun st)}
        )
      ]
  | otherwise = []
  where
    x = at st s

-- A same-size rewrite inside the racy window keeps the stat identity.
silentRewrite :: Config -> St -> Seat -> [Step]
silentRewrite cfg st s
  | fresh x && edits x < cEdits cfg =
      [(label "SilentRewrite" s "", put s x {edits = edits x + 1} st {changedRun = S.insert s (changedRun st)})]
  | otherwise = []
  where
    x = at st s

-- Time passes beyond the racy allowance.
tick :: St -> [Step]
tick st
  | any fresh (M.elems (seats st)) = [("Tick", each (\x -> x {fresh = False}) st)]
  | otherwise = []

-- A third party writes the path (a copy of the source's bytes or other bytes)
-- under a new identity; its name is durable, its data not yet.
foreignWrite :: Config -> St -> Seat -> [Step]
foreignWrite cfg st s
  | foreignN st < cForeign cfg =
      [ ( label "ForeignWrite" s (show d)
        , put
            s
            x {out = File True (firstForeignId + foreignN st + 1) d False True, outPrev = noFile}
            st {foreignN = foreignN st + 1, changedRun = S.insert s (changedRun st)}
        )
      | d <- S.toList (S.fromList [foreignBytes, edits x])
      ]
  | otherwise = []
  where
    x = at st s

foreignDelete :: Config -> St -> Seat -> [Step]
foreignDelete cfg st s
  | foreignN st < cForeign cfg && fPres (out x) =
      [ ( label "ForeignDelete" s ""
        , put s x {out = noFile, outPrev = noFile} st {foreignN = foreignN st + 1, changedRun = S.insert s (changedRun st)}
        )
      ]
  | otherwise = []
  where
    x = at st s

-- Crashes (A/io/crash_check.rs persistence model). A destination power loss
-- may keep or lose a rename its directory seal has not made durable, and
-- leaves garbage in data never sealed; the receiver's pipeline is lost, its
-- store rows are not. A source crash loses the pending ledger items; with
-- the strict ledger and a sealed state root, nothing committed.

canCrash :: Config -> St -> Bool
canCrash cfg st = crashes st < cCrashes cfg && run st > 0

data Fate = Persist | Garbage | Revert
  deriving (Eq, Show)

fates :: SeatSt -> [Fate]
fates x = [Persist] ++ [Garbage | fPres o && not (fDD o)] ++ [Revert | fPres o && not (fND o)]
  where
    o = out x

afterLoss :: Fate -> SeatSt -> SeatSt
afterLoss f x =
  x
    { out = case f of
        Revert -> durable (outPrev x)
        Garbage -> (out x) {fData = garbage, fDD = True, fND = True}
        Persist -> durable (out x)
    , outPrev = noFile
    , tmp = noTmp
    , dPub = PubNone
    , dEnt = DIdle
    , dPlan = PlanNone
    , dRec = noRec
    }

-- Every combination of per-seat fates.
dstLosses :: St -> [([(Seat, Fate)], St)]
dstLosses st =
  [ (combo, st {seats = M.fromList [(s, afterLoss f (at st s)) | (s, f) <- combo]})
  | combo <- mapM (\(s, x) -> [(s, f) | f <- fates x]) (M.toList (seats st))
  ]

-- The source session is gone: entries idle, captures dropped, no frames.
sourceGone :: St -> St
sourceGone st = (each (\x -> x {sEnt = SIdle, sCap = noCap, msg = noMsg}) st) {srcDone = False}

broken :: St -> Sess
broken st = if sess st == On then Broken else sess st

crashSrc :: Config -> St -> [Step]
crashSrc cfg st
  | canCrash cfg st =
      [ ( "CrashSrc"
        , (each (\x -> x {dEnt = DIdle}) (sourceGone st))
            {sPend = S.empty, sess = broken st, crashes = crashes st + 1}
        )
      ]
  | otherwise = []

showFates :: [(Seat, Fate)] -> String
showFates combo = unwords [s : '=' : show f | (s, f) <- combo]

crashDst :: Config -> St -> [Step]
crashDst cfg st
  | canCrash cfg st =
      [ ("CrashDst " ++ showFates combo, (sourceGone lost) {sess = broken st, crashes = crashes st + 1})
      | (combo, lost) <- dstLosses st
      ]
  | otherwise = []

-- One host runs both ends (a loopback copy) and loses power.
crashBoth :: Config -> St -> [Step]
crashBoth cfg st
  | canCrash cfg st =
      [ ("CrashBoth " ++ showFates combo, (sourceGone lost) {sPend = S.empty, sess = broken st, crashes = crashes st + 1})
      | (combo, lost) <- dstLosses st
      ]
  | otherwise = []

-- Every run has been made: the behaviour may stop (a stuttering step).
terminated :: Config -> St -> [Step]
terminated cfg st
  | run st == cRuns cfg && sess st `elem` [Done, Broken] && quiescent st = [("Terminated", st)]
  | otherwise = []

-- ---------------------------------------------------------------------------
-- Invariants, under the model's frozen names

type Invariant = (String, Config -> St -> Bool)

invariants :: [Invariant]
invariants =
  [ ("TypeOK", typeOK)
  , ("R25_NoDurableReread", \_ st -> not (any rdHeld (S.toList (readLog st))))
  , ("R25_NoCommittedCaptureReread", \_ st -> not (any rdCommitted (S.toList (readLog st))))
  , ("ReadOnce", \_ st -> all ((<= 1) . rc) (M.elems (seats st)))
  , ("S3_ReadsOnlyChanged", \_ st -> runReads st `S.isSubsetOf` S.union (allSeats st S.\\ heldAtStart st) (changedRun st))
  , ("S3_UnchangedReadsZero", \_ st -> not (heldAtStart st == allSeats st && S.null (changedRun st)) || S.null (runReads st))
  , ("S3_ClosedPassIsHeld", closedPassIsHeld)
  , ("RecordImpliesBytes", recordImpliesBytes)
  , ("HeldAfterCommit", \_ st -> all (\x -> not (mT (msg x) == MHeld && mB (msg x)) || committedRun x) (M.elems (seats st)))
  , ("LedgerAfterHeld", \_ st -> all (\r -> (lSeat r, lKey r) `S.member` everCommitted st) (S.toList (S.union (srcLedger st) (sPend st))))
  , ("DoneAfterLedger", \_ st -> not (srcDone st) || S.null (sPend st))
  , ("ReuseSound", \_ st -> all (\s -> not (heldPhys st s (srcStat (at st s))) || fData (out (at st s)) == edits (at st s)) (seatsOf st))
  , ("LedgerSound", \_ st -> all (\r -> lKey r /= wireKey st (lSeat r) || lData r == edits (at st (lSeat r))) (S.toList (srcLedger st)))
  , -- Only WP0(d)'s superseding publish and its recovery replace or remove a
    -- destination file, and neither is in the core: nothing can clobber.
    ("NoClobber", \_ _ -> True)
  , ("S2_TypedSourceAccess", \_ st -> all (`elem` [OpStat, OpRead]) (S.toList (srcOps st)))
  , -- Estate reads are off in the core: no backup, so no backup lock.
    ("S2_BackupLockBounded", \_ _ -> True)
  , ("ClosureAccounted", closureAccounted)
  ]
  where
    allSeats = S.fromList . seatsOf
    typeOK cfg st =
      run st >= 0
        && run st <= cRuns cfg
        && crashes st >= 0
        && crashes st <= cCrashes cfg
        && srcAuth st >= 0
        && srcAuth st <= cCrashes cfg + 1
        && all (\x -> srcStat x <= edits x && edits x <= cEdits cfg) (M.elems (seats st))
    closedPassIsHeld _ st =
      not (sess st == Done && S.null (changedRun st) && all ((== OApplied) . outc) (M.elems (seats st)))
        || all (\s -> heldPhys st s (srcStat (at st s))) (seatsOf st)
    recordImpliesBytes _ st =
      all
        ( \r ->
            let o = out (at st (drSeat r))
             in not (fPres o && fId o == drId r) || (fDD o && fND o && fData o == drData r)
        )
        (S.toList (dstRows st))
    closureAccounted _ st =
      sess st /= Done || all (closed . outc) (M.elems (seats st))
    closed o = case o of
      OApplied -> True
      OAppliedRacy -> True
      ORefused _ -> True
      _ -> False

-- ---------------------------------------------------------------------------
-- Breadth-first search

data Failure = Failure
  { failWhat :: [String] -- the checked invariants violated, or ["deadlock"]
  , failTrace :: [Step] -- from an initial state to the failing state
  }

data Report = Report
  { repInitial :: Int
  , repDistinct :: Int
  , repGenerated :: Int -- initial states plus every successor computed
  , repDepth :: Int -- BFS levels, initial states at level 1
  , repFailure :: Maybe Failure
  }

explore :: Config -> [Invariant] -> Report
explore cfg checks =
  case [(s, b) | s <- inits, let b = violatedBy s, not (null b)] of
    (s, b) : _ -> Report (length inits) (M.size seen0) (length inits) 1 (Just (Failure b [("Init", s)]))
    [] -> level 1 inits seen0 (length inits)
  where
    inits = S.toList (S.fromList (initial cfg))
    seen0 = M.fromList [(s, Nothing) | s <- inits]
    violatedBy st = [name | (name, ok) <- checks, not (ok cfg st)]
    trace seen st = reverse (go st)
      where
        go u = case seen M.! u of
          Nothing -> [("Init", u)]
          Just (p, l) -> (l, u) : go p
    level depth frontier seen generated
      | null frontier = Report (length inits) (M.size seen) generated (depth - 1) Nothing
      | otherwise = scan frontier [] seen generated
      where
        scan [] next seen' gen = level (depth + 1) (reverse next) seen' gen
        scan (st : rest) next seen' gen =
          case successors cfg st of
            [] -> Report (length inits) (M.size seen') gen depth (Just (Failure ["deadlock"] (trace seen' st)))
            steps -> absorb st steps rest next seen' gen
        absorb _ [] rest next seen' gen = scan rest next seen' gen
        absorb from ((l, st) : more) rest next seen' gen
          | st `M.member` seen' = absorb from more rest next seen' (gen + 1)
          | otherwise =
              let seen'' = M.insert st (Just (from, l)) seen'
               in case violatedBy st of
                    [] -> absorb from more rest (st : next) seen'' (gen + 1)
                    b -> Report (length inits) (M.size seen'') (gen + 1) (depth + 1) (Just (Failure b (trace seen'' st)))

-- ---------------------------------------------------------------------------
-- JSON (counterexamples), with the model's variable and value spellings

data J = JO [(String, J)] | JA [J] | JS String | JN Int | JB Bool

render :: J -> String
render j = case j of
  JO kvs -> "{" ++ intercalate ", " [quote k ++ ": " ++ render v | (k, v) <- kvs] ++ "}"
  JA vs -> "[" ++ intercalate ", " (map render vs) ++ "]"
  JS s -> quote s
  JN n -> show n
  JB b -> if b then "true" else "false"

quote :: String -> String
quote s = "\"" ++ concatMap esc s ++ "\""
  where
    esc c
      | c == '"' = "\\\""
      | c == '\\' = "\\\\"
      | c < ' ' = "\\u" ++ pad4 (showHex4 (fromEnum c))
      | otherwise = [c]
    pad4 h = replicate (4 - length h) '0' ++ h
    showHex4 n = let (q, r) = n `divMod` 16 in (if q > 0 then showHex4 q else "") ++ ["0123456789abcdef" !! r]

sEntName :: SEnt -> String
sEntName e = case e of
  SIdle -> "idle"
  SUnwalked -> "unwalked"
  SOffered -> "offered"
  SManifested -> "manifested"
  SAwaitHeld -> "await_held"
  SDone -> "done"

dEntName :: DEnt -> String
dEntName e = case e of
  DIdle -> "idle"
  DNone -> "none"
  DStreaming -> "streaming"
  DAwaitManifest -> "await_manifest"
  DFilling -> "filling"
  DQueued -> "queued"
  DDone -> "done"

planName :: Plan -> String
planName p = case p of
  PlanNone -> "none"
  PlanWrite -> "write"
  PlanAdopt -> "adopt"

pubName :: Pub -> String
pubName p = case p of
  PubNone -> "none"
  PubStaged -> "staged"
  PubSealed -> "sealed"
  PubRenamed -> "renamed"
  PubReady -> "ready"
  PubAdoptWait -> "adopt_wait"
  PubCommitted -> "committed"
  PubFailedOccupied -> "failed_occupied"

kindName :: Kind -> String
kindName k = case k of
  KindNone -> "none"
  KindWrite -> "write"
  KindAdopt -> "adopt"

refusalName :: Refusal -> String
refusalName r = case r of
  SourceChanged -> "SOURCE_CHANGED_AFTER_SNAPSHOT"
  GitOccupied -> "GIT_DESTINATION_OCCUPIED"

codeName :: Code -> String
codeName c = case c of
  CNone -> "none"
  CDecision Reuse -> "reuse"
  CDecision Send -> "send"
  CDecision WantManifest -> "manifest"
  CRefusal r -> refusalName r

msgTName :: MsgT -> String
msgTName t = case t of
  MNone -> "none"
  MEntry -> "entry"
  MDecide -> "decide"
  MManifest -> "manifest"
  MNeed -> "need"
  MEnd -> "end"
  MRefused -> "refused"
  MHeld -> "held"

outcName :: Outc -> String
outcName o = case o of
  ONone -> "none"
  OPending -> "pending"
  OApplied -> "applied"
  OAppliedRacy -> "applied_racy"
  ORefused r -> refusalName r

opName :: Op -> String
opName o = case o of
  OpStat -> "stat"
  OpRead -> "read"
  OpWrite -> "write"
  OpProcCtl -> "procctl"

stateJ :: St -> J
stateJ st =
  JO
    [ ("srcStat", per (JN . srcStat))
    , ("edits", per (JN . edits))
    , ("fresh", per (JB . fresh))
    , ("sEnt", per (JS . sEntName . sEnt))
    , ("sRow", per (JN . sRow))
    , ("sCap", per (capJ . sCap))
    , ("sPend", JA (map lrowJ (S.toList (sPend st))))
    , ("srcLedger", JA (map lrowJ (S.toList (srcLedger st))))
    , ("srcDone", JB (srcDone st))
    , ("srcAuth", JN (srcAuth st))
    , ("srcStore", JS (if srcStore st == StoreNone then "none" else "durable"))
    , ("msg", per (msgJ . msg))
    , ("dEnt", per (JS . dEntName . dEnt))
    , ("dKey", per (JN . dKey))
    , ("dPlan", per (JS . planName . dPlan))
    , ("dPub", per (JS . pubName . dPub))
    , ("dRec", per (recJ . dRec))
    , ("tmp", per (\x -> JO [("st", JS (tmpName (tSt (tmp x)))), ("data", JN (tData (tmp x)))]))
    , ("out", per (fileJ . out))
    , ("outPrev", per (fileJ . outPrev))
    , ("dstRows", JA [JO [("seat", JS [drSeat r]), ("key", JN (drKey r)), ("id", JN (drId r)), ("data", JN (drData r))] | r <- S.toList (dstRows st)])
    , ("run", JN (run st))
    , ("sess", JS (sessName (sess st)))
    , ("crashes", JN (crashes st))
    , ("foreign", JN (foreignN st))
    , ("reads", JA (map readJ (S.toList (readLog st))))
    , ("srcOps", JA (map (JS . opName) (S.toList (srcOps st))))
    , ("rc", per (JN . rc))
    , ("runReads", seatSet (runReads st))
    , ("heldAtStart", seatSet (heldAtStart st))
    , ("changedRun", seatSet (changedRun st))
    , ("committedRun", per (JB . committedRun))
    , ("everCommitted", JA [JO [("seat", JS [s]), ("key", JN k)] | (s, k) <- S.toList (everCommitted st)])
    , ("outc", per (JS . outcName . outc))
    ]
  where
    per f = JO [([s], f x) | (s, x) <- M.toList (seats st)]
    seatSet = JA . map (\s -> JS [s]) . S.toList
    capJ c = JO [("data", JN (capData c)), ("racy", JB (capRacy c)), ("rec", JB (capRec c)), ("led", JB (capLed c))]
    lrowJ r = JO [("seat", JS [lSeat r]), ("key", JN (lKey r)), ("data", JN (lData r))]
    msgJ m = JO [("t", JS (msgTName (mT m))), ("v", JN (mV m)), ("b", JB (mB m)), ("c", JS (codeName (mC m)))]
    recJ r = JO [("key", JN (rKey r)), ("data", JN (rData r)), ("racy", JB (rRacy r)), ("id", JN (rId r)), ("kind", JS (kindName (rKind r)))]
    fileJ f = JO [("pres", JB (fPres f)), ("id", JN (fId f)), ("data", JN (fData f)), ("dd", JB (fDD f)), ("nd", JB (fND f))]
    readJ r =
      JO
        [ ("run", JN (rdRun r))
        , ("seat", JS [rdSeat r])
        , ("key", JN (rdKey r))
        , ("kind", JS (if rdKind r == Full then "full" else "chunks"))
        , ("held", JB (rdHeld r))
        , ("committed", JB (rdCommitted r))
        ]
    tmpName t = case t of
      TmpNone -> "none"
      TmpWritten -> "written"
      TmpSealed -> "sealed"
    sessName s = case s of
      Idle -> "idle"
      On -> "on"
      Broken -> "broken"
      Done -> "done"

counterexample :: Config -> String -> [String] -> Failure -> String
counterexample cfg name checked f =
  let final = snd (last (failTrace f))
      alsoFalse = [n | (n, ok) <- invariants, not (ok cfg final), n `notElem` failWhat f]
      header =
        [ ("explorer", JS "docs/formal/hs/Explorer.hs")
        , ("row", JS name)
        , ("preset", JS (cPreset cfg))
        , ("mutation", JS (maybe "none" mutationName (cMutation cfg)))
        , ("checked", JA (map JS checked))
        , ("violated", JA (map JS (failWhat f)))
        , ("alsoFalseInFinalState", JA (map JS alsoFalse))
        , ("states", JN (length (failTrace f)))
        ]
      steps = [render (JO [("index", JN i), ("action", JS l), ("state", stateJ s)]) | (i, (l, s)) <- zip [1 :: Int ..] (failTrace f)]
   in "{\n"
        ++ concatMap (\(k, v) -> "  " ++ quote k ++ ": " ++ render v ++ ",\n") header
        ++ "  \"trace\": [\n    "
        ++ intercalate ",\n    " steps
        ++ "\n  ]\n}\n"

-- ---------------------------------------------------------------------------
-- Command line

data Opts = Opts
  { oPreset :: String
  , oMutation :: Maybe String
  , oCheck :: String
  , oName :: Maybe String
  , oJson :: Maybe FilePath
  }

parseOpts :: [String] -> Either String Opts
parseOpts = go (Opts "nv_core" Nothing "all" Nothing Nothing)
  where
    go o [] = Right o
    go o ("--preset" : v : rest) = go o {oPreset = v} rest
    go o ("--mutation" : v : rest) = go o {oMutation = if v == "none" then Nothing else Just v} rest
    go o ("--check" : v : rest) = go o {oCheck = v} rest
    go o ("--name" : v : rest) = go o {oName = Just v} rest
    go o ("--json" : v : rest) = go o {oJson = Just v} rest
    go _ (a : _) = Left ("unknown or incomplete option: " ++ a)

splitOn :: Char -> String -> [String]
splitOn c s = case break (== c) s of
  (a, []) -> [a]
  (a, _ : b) -> a : splitOn c b

usage :: String -> IO a
usage err = do
  hPutStrLn stderr ("Explorer: " ++ err)
  hPutStrLn stderr "usage: Explorer [--preset nv_core|nv_ledger] [--mutation NAME] [--check all|NAME,...] [--name NAME] [--json DIR]"
  exitWith (ExitFailure 2)

main :: IO ()
main = do
  opts <- either usage pure . parseOpts =<< getArgs
  base <- maybe (usage ("unknown preset " ++ oPreset opts)) pure (preset (oPreset opts))
  mutation <- case oMutation opts of
    Nothing -> pure Nothing
    Just m -> case [x | x <- [minBound .. maxBound], mutationName x == m] of
      [x] -> pure (Just x)
      _ -> usage ("mutation " ++ m ++ " is not in the core (supported: " ++ unwords (map mutationName [minBound .. maxBound]) ++ ")")
  let cfg = base {cMutation = mutation}
      known = map fst invariants
      wanted = if oCheck opts == "all" then known else splitOn ',' (oCheck opts)
      name = case oName opts of
        Just n -> n
        Nothing -> maybe ("MC_" ++ cPreset cfg) (("MC_neg_" ++) . mutationName) mutation
  case filter (`notElem` known) wanted of
    [] -> pure ()
    bad -> usage ("unknown invariant(s): " ++ unwords bad)
  if all (\c -> isAlphaNum c || c == '_') name then pure () else usage "a row name is letters, digits and _"
  let checks = [inv | inv@(n, _) <- invariants, n `elem` wanted]
      rep = explore cfg checks
      common =
        [ ("row", name)
        , ("preset", cPreset cfg)
        , ("mutation", maybe "none" mutationName mutation)
        , ("initial", show (repInitial rep))
        , ("distinct", show (repDistinct rep))
        , ("generated", show (repGenerated rep))
        , ("depth", show (repDepth rep))
        ]
      line kvs = putStrLn (unwords [k ++ "=" ++ v | (k, v) <- kvs])
  case repFailure rep of
    Nothing -> do
      line (common ++ [("outcome", "pass"), ("violated", "-")])
      exitWith ExitSuccess
    Just f -> do
      written <- case oJson opts of
        Nothing -> pure "-"
        Just dir -> do
          let path = dir ++ "/" ++ name ++ ".json"
          writeFile path (counterexample cfg name wanted f)
          pure path
      line
        ( common
            ++ [ ("outcome", if failWhat f == ["deadlock"] then "deadlock" else "violation")
               , ("violated", intercalate "," (failWhat f))
               , ("states", show (length (failTrace f)))
               , ("json", written)
               ]
        )
      exitWith (ExitFailure 1)
