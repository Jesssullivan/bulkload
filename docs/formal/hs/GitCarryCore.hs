{- |
Git carry's decision core: the reference decide, its pinned rows, and an
explorer of GitCarry.tla's custody model (Q42 lane L4, OI-1003-Q43).

Roles (OI-1003-Q43, which widens OI-1003-Q32 for this layer):

  * `decide :: Inputs -> Decision` is the reference copy of the pure,
    total decision a v1 capture makes before it exports. Lane L6a moved
    the code's decision into `git_carry/decide.rs`, which a fixed-seed
    property test (P67) checks against this copy's v1 pinned rows.
    So here Haskell is a differential oracle for the code, not only a
    second encoding of the TLA+ spec.
  * `rows` renders those pinned rows, crates/bulkload-agent/tests/data/
    decide_rows.tsv; `rows --check FILE` requires FILE to be that rendering
    byte for byte.
  * `explore` is an explicit-state breadth-first search of GitCarry.tla
    whose capture step calls this decide. `just formal-nv` requires TLC's
    state counts of record for its presets: the TLA+ DecideCore and this
    decide then agree on every reachable input, and so do the two
    encodings of the custody model. Like Explorer.hs it is a second
    encoding (independent code, shared design): it was transliterated by
    hand from GitCarry.tla, and it cannot catch a misreading of the Rust
    code that the spec makes. The pinned rows are what tie decide to the
    code (P67).
  * `schema` prints the closed unions' labels; `formal-nv` requires them
    to equal catalogue/Types.dhall's.

decide is derived from the v1 rules at main, cited as A/... for
crates/bulkload-agent/src:

  * A/estate.rs prepare_base: a group whose base record names a bundle
    that is missing or not at its recorded identity refuses
    RECEIPT_BINDING_INVALID, rather than replace a base older deltas
    depend on.
  * A/estate.rs retained_capture: no record, or a record whose bundle is
    gone, is Retained::None (a fresh capture, no reuse). The same key, no
    drift, a settled pass start and a restorable chain (chain_links under
    LinkBinding::Custody) is a hit; a hit on a based bundle needs its bound
    base retained, else RECEIPT_BINDING_INVALID. Anything else extends.
  * A/estate.rs chainable: a broken chain or a based bundle is never
    chained on; otherwise the link carries the prior's depth, and only a
    prior below chain::CHAIN_DEPTH_LIMIT is chainable.
  * A/estate.rs chain_offer and A/git_carry.rs ExportOptions: `chain` is
    "Ignored when prerequisite is set" (a plan base wins).
  * A/git_carry.rs export_pass, the match on (prerequisite, chain): with
    no base and a link, shared::write_chained, which writes a
    self-contained bundle for a shallow source or when the source holds
    none of the prior's tips (chain::source_held_tips); otherwise
    shared::write_bundle with the base, if any.
  * OI-1003-Q46's re-root policy (lane L8, no code yet), under
    rootWindow > 0: a capture whose root would reach the window starts a
    new root; at the depth limit a capture chains on its chain's root
    (a re-root) instead of re-packing its whole history.
  * Lane L6b's fix 2 (no code yet), under chainUnderBase: a based capture
    still chains on its prior.

Base and containers only:

  runghc docs/formal/hs/GitCarryCore.hs rows [--check FILE]
  runghc docs/formal/hs/GitCarryCore.hs schema
  runghc docs/formal/hs/GitCarryCore.hs explore [OPTIONS]
  ghc -O1 -outputdir DIR -o DIR/gitcarry docs/formal/hs/GitCarryCore.hs

explore options (each overrides the preset's value; TLC's constant in
brackets):

  --preset NAME              the base bound (default gc_core): gc_core,
                             gc_q46, gc_grouped, gc_fix2, gc_reroot,
                             gc_reroot_extended or gc_fix2_deep
  --items N                  plan items [Items = {i1, ..., iN}]
  --depth-limit N            [DepthLimit]
  --root-window N            [RootWindow]
  --chain-under-base BOOL    [ChainUnderBase]
  --gc BOOL                  [GCOn]
  --commits N                [MaxCommits]
  --rewrites N               [MaxRewrites]
  --crashes N                [MaxCrashes]
  --damage N                 [MaxDamage]
  --damage-base BOOL         [DamageBase]
  --damage-rewrites BOOL     [DamageRewrites]
  --base-missing-typed BOOL  [BaseMissingTyped]
  --mutation NAME            one deliberate rule break (default none)
  --check all|NAME[,NAME...] the invariants to check (default all)
  --name NAME                the row name reported
  --json DIR                 write a counterexample to DIR/NAME.json

explore prints one line of key=value pairs and exits 0 when the search
completed with no violation, 1 when it stopped at a violated invariant or a
deadlock, 2 on a usage error. Its `decisions` are every capture decision a
step of the search took (Hit, Io, Refuse:<refusal>,
Export:<basis>:<rebase>): formal-nv requires each preset to reach the
branches it exists for, such as Q46's Reroot. With --check all, `violated` lists every
invariant the failing state violates, in the spec's order, so its first
name is the one TLC reports.
-}
module Main (main) where

import Data.Char (isAlphaNum, isDigit)
import Data.List (intercalate)
import qualified Data.Map.Strict as M
import qualified Data.Set as S
import System.Environment (getArgs)
import System.Exit (ExitCode (..), exitWith)
import System.IO (hPutStrLn, stderr)

-- ---------------------------------------------------------------------------
-- The decision core

-- | The policy a capture runs under. v1 at main: depth limit 8
-- (chain::CHAIN_DEPTH_LIMIT), no re-root window, no chain under a base.
data Policy = Policy
  { depthLimit :: Int
  , rootWindow :: Int -- ^ Q46: captures per root; 0 never re-roots (v1)
  , chainUnderBase :: Bool -- ^ L6b fix 2
  }
  deriving (Eq, Show)

-- | The group's plan base record (prepare_base).
data BaseState = NoGroup | BaseAbsent | BaseRetained | BaseLost
  deriving (Eq, Ord, Show, Enum, Bounded)

-- | The item's capture record (retained_capture).
data RetainedState = NoRecord | BundleGone | Held
  deriving (Eq, Ord, Show, Enum, Bounded)

-- | The retained bundle: self-contained, a plan base's delta (it declares
-- prerequisites and has no .prior), or a chain link (a .prior sidecar).
data Shape = Unchained | Based | Chained
  deriving (Eq, Ord, Show, Enum, Bounded)

-- | The base the retained bundle's {bundle}.base names, if any.
data PrevBase = PrevNone | PrevRetained | PrevLost
  deriving (Eq, Ord, Show, Enum, Bounded)

-- | What the code reads before it decides (the Rust Inputs is lane L6a's).
data Inputs = Inputs
  { grouped :: Bool -- ^ the item shares its common repository (capture_groups)
  , base :: BaseState
  , retained :: RetainedState
  , keyEqual :: Bool -- ^ previous.key == key
  , drifted :: Bool -- ^ the retained {bundle}.drift is not empty
  , settled :: Bool -- ^ a recorded pass start with no seat racy since
  , passStart :: Bool -- ^ {bundle}.parts records the pass start
  , shape :: Shape
  , chainIntact :: Bool -- ^ chain_links(Custody) succeeds
  , prevBase :: PrevBase
  , depth :: Int -- ^ links in the retained bundle's chain
  , age :: Int -- ^ captures since its root (Q46)
  , tipsHeld :: Bool -- ^ the source holds one of its tips (source_held_tips)
  , rootHeld :: Bool -- ^ the source holds one of its chain root's tips
  , shallow :: Bool -- ^ the source is shallow
  , policy :: Policy
  }
  deriving (Eq, Show)

-- The closed unions catalogue/Types.dhall types; the constructors are the
-- labels.
data Basis = SelfContained | Base | Chain | BaseAndChain
  deriving (Eq, Ord, Show, Enum, Bounded)

data Rebase = NoRebase | NewRoot | Reroot
  deriving (Eq, Ord, Show, Enum, Bounded)

data Reuse = NoRetained | BlobReuse | PassStartUnrecorded
  deriving (Eq, Ord, Show, Enum, Bounded)

data Refusal = ReceiptBindingInvalid
  deriving (Eq, Ord, Show, Enum, Bounded)

data Plan = Plan {basis :: Basis, planDepth :: Int, rebase :: Rebase, reuse :: Reuse}
  deriving (Eq, Show)

data Decision = Hit | Export Plan | Refuse Refusal
  deriving (Eq, Show)

decisionLabel :: Decision -> String
decisionLabel d = case d of
  Hit -> "Hit"
  Export _ -> "Export"
  Refuse _ -> "Refuse"

-- | One tag per Decision constructor. decisionTag and decisionExample are
-- total case analyses, so under -Wall -Werror (formal-nv's build) a
-- constructor added to Decision does not build until it has a tag and an
-- example; `schema` and the rows' coverage check then list its label.
data DecisionTag = TagHit | TagExport | TagRefuse
  deriving (Eq, Ord, Show, Enum, Bounded)

decisionTag :: Decision -> DecisionTag
decisionTag d = case d of
  Hit -> TagHit
  Export _ -> TagExport
  Refuse _ -> TagRefuse

decisionExample :: DecisionTag -> Decision
decisionExample t = case t of
  TagHit -> Hit
  TagExport -> Export (Plan minBound 0 minBound minBound)
  TagRefuse -> Refuse minBound

-- | One value of each Decision constructor, in constructor order.
allDecisions :: [Decision]
allDecisions = map decisionExample allOf

-- | The explorer's rule breaks of the core (GitCarry.tla's mutations
-- chain_ignores_depth and hit_ignores_chain). decide is Faithful.
data Tweak = Faithful | IgnoreDepth | HitIgnoresChain
  deriving (Eq)

-- | The reference decision.
decide :: Inputs -> Decision
decide = decideWith Faithful

decideWith :: Tweak -> Inputs -> Decision
decideWith tw i
  | base i == BaseLost = Refuse ReceiptBindingInvalid -- prepare_base
  | retained i /= Held = Export (Plan (rootBasis i) 0 NoRebase NoRetained)
  | hitPath tw i =
      if needsBoundBase i && prevBase i == PrevLost
        then Refuse ReceiptBindingInvalid
        else Hit
  | otherwise = extend tw i

-- | A fresh root: a plan base's delta, else self-contained. A shallow
-- source is always self-contained (shared::write_bundle ignores the base).
rootBasis :: Inputs -> Basis
rootBasis i = if grouped i && not (shallow i) then Base else SelfContained

-- | retained_capture's `restorable`.
chainOk :: Tweak -> Inputs -> Bool
chainOk tw i = shape i /= Chained || chainIntact i || tw == HitIgnoresChain

-- | `!chained && requires_base`; under fix 2 a chained capture's base too.
needsBoundBase :: Inputs -> Bool
needsBoundBase i = shape i == Based || (shape i == Chained && chainUnderBase (policy i))

hitPath :: Tweak -> Inputs -> Bool
hitPath tw i =
  retained i == Held && keyEqual i && not (drifted i) && settled i && chainOk tw i

-- | chainable, chain_offer and the export_pass match, with Q46's re-root
-- policy and fix 2.
extend :: Tweak -> Inputs -> Decision
extend tw i
  | shallow i || not offered = plan (rootBasis i) 0 NoRebase
  | windowEnds = plan (rootBasis i) 0 NewRoot
  | depth i < depthLimit p || tw == IgnoreDepth =
      if tipsHeld i then plan chainBasis (depth i + 1) NoRebase else plan (rootBasis i) 0 NoRebase
  | rootWindow p > 0 && rootHeld i = plan chainBasis 1 Reroot
  | otherwise = plan (rootBasis i) 0 NewRoot
  where
    p = policy i
    plan b d rb = Export (Plan b d rb (if passStart i then BlobReuse else PassStartUnrecorded))
    linkable = case shape i of
      Chained -> chainIntact i
      Based -> chainUnderBase p
      Unchained -> True
    offered = linkable && (not (grouped i) || chainUnderBase p)
    chainBasis = if grouped i then BaseAndChain else Chain
    windowEnds = rootWindow p > 0 && age i + 1 >= rootWindow p

-- ---------------------------------------------------------------------------
-- Pinned rows (P67)

-- | The lane whose code each row pins: main's behaviour (v1), L6b's fix 2,
-- and L8's re-root. The window 27 is a parameter of the rows, not a
-- ruling: Q46 sets the policy, L8 its value.
data Lane = V1 | L6b | L8
  deriving (Eq, Show, Enum, Bounded)

laneName :: Lane -> String
laneName l = case l of
  V1 -> "v1"
  L6b -> "L6b"
  L8 -> "L8"

lanePolicy :: Lane -> Policy
lanePolicy l = case l of
  V1 -> Policy 8 0 False
  L6b -> Policy 8 0 True
  L8 -> Policy 8 27 True

-- | A row's inputs; Nothing is "-": decide must not depend on that input
-- for the row, which `rows` checks over the input's whole domain below.
data PIn = PIn
  { pGrouped :: Maybe Bool
  , pBase :: Maybe BaseState
  , pRetained :: Maybe RetainedState
  , pKeyEqual :: Maybe Bool
  , pDrifted :: Maybe Bool
  , pSettled :: Maybe Bool
  , pPassStart :: Maybe Bool
  , pShape :: Maybe Shape
  , pChainIntact :: Maybe Bool
  , pPrevBase :: Maybe PrevBase
  , pDepth :: Maybe Int
  , pAge :: Maybe Int
  , pTipsHeld :: Maybe Bool
  , pRootHeld :: Maybe Bool
  , pShallow :: Maybe Bool
  , pPolicy :: Policy
  }

blank :: Policy -> PIn
blank = PIn Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing Nothing

-- | The domain a "-" input is checked over: every value of a union or a
-- Bool, and depths and ages around the limit and the window.
expand :: PIn -> [Inputs]
expand p =
  [ Inputs g b r ke dr se ps sh ci pb d a th rh sl (pPolicy p)
  | g <- dom bools pGrouped
  , b <- dom allOf pBase
  , r <- dom allOf pRetained
  , ke <- dom bools pKeyEqual
  , dr <- dom bools pDrifted
  , se <- dom bools pSettled
  , ps <- dom bools pPassStart
  , sh <- dom allOf pShape
  , ci <- dom bools pChainIntact
  , pb <- dom allOf pPrevBase
  , d <- dom [0, 1, 2, lim - 1, lim, lim + 1] pDepth
  , a <- dom [0, 1, lim, win - 2, win - 1, win, win + 3] pAge
  , th <- dom bools pTipsHeld
  , rh <- dom bools pRootHeld
  , sl <- dom bools pShallow
  ]
  where
    dom xs f = maybe xs pure (f p)
    bools = [False, True]
    lim = depthLimit (pPolicy p)
    win = max 3 (rootWindow (pPolicy p))

allOf :: (Enum a, Bounded a) => [a]
allOf = [minBound .. maxBound]

-- | The pinned rows of one lane. Families: a lost base; no record or a
-- gone bundle; the hit path on each custody shape; each reason to leave
-- it; the extend path over shape, depth, held tips, root and age; a
-- shallow source; an unrecorded pass start.
laneRows :: Lane -> [PIn]
laneRows lane =
  concatMap groupRows [(False, NoGroup), (True, BaseAbsent), (True, BaseRetained)]
    ++ [(blank pol) {pGrouped = Just True, pBase = Just BaseLost}]
  where
    pol = lanePolicy lane
    cub = chainUnderBase pol
    win = rootWindow pol
    ages = if win > 0 then map Just [lim, win - 2, win - 1] else [Nothing]
    lim = depthLimit pol
    roots = if win > 0 then map Just [True, False] else [Nothing]
    groupRows (g, b) =
      let b0 = (blank pol) {pGrouped = Just g, pBase = Just b}
          held = b0 {pRetained = Just Held}
          hit = held {pKeyEqual = Just True, pDrifted = Just False, pSettled = Just True, pPassStart = Just True}
          moved = held {pKeyEqual = Just False, pPassStart = Just True, pShallow = Just False}
          unchained = moved {pShape = Just Unchained, pDepth = Just 0}
       in [b0 {pRetained = Just r, pShallow = Just s} | r <- [NoRecord, BundleGone], s <- [False, True]]
            -- The hit path, on each shape the record can stand on.
            ++ [hit {pShape = Just Unchained}]
            ++ [hit {pShape = Just Based, pPrevBase = Just pb} | pb <- [PrevRetained, PrevLost]]
            ++ [ hit {pShape = Just Chained, pChainIntact = Just True, pPrevBase = pb}
               | pb <- if cub then map Just [PrevNone, PrevRetained, PrevLost] else [Nothing]
               ]
            -- A broken chain is never a hit (BrokenLinkNeverReuseHit).
            ++ [hit {pShape = Just Chained, pChainIntact = Just False, pShallow = Just False}]
            -- Each other reason the hit path is left.
            ++ [ held
                   { pKeyEqual = Just True
                   , pDrifted = Just dr
                   , pSettled = Just se
                   , pPassStart = Just ps
                   , pShape = Just Unchained
                   , pDepth = Just 0
                   , pTipsHeld = Just True
                   , pAge = head' ages
                   , pShallow = Just False
                   }
               | (dr, se, ps) <- [(True, True, True), (False, False, True), (False, False, False)]
               ]
            -- The extend path.
            ++ [unchained {pTipsHeld = Just t, pAge = a} | t <- [True, False], a <- ages]
            ++ [moved {pShape = Just Based, pDepth = Just 0, pTipsHeld = Just t, pAge = a} | t <- [True, False], a <- ages]
            ++ [moved {pShape = Just Chained, pChainIntact = Just False}]
            ++ [ moved
                   { pShape = Just Chained
                   , pChainIntact = Just True
                   , pDepth = Just d
                   , pTipsHeld = Just t
                   , pRootHeld = rh
                   , pAge = a
                   }
               | d <- [1, lim - 1, lim]
               , t <- [True, False]
               , rh <- roots
               , a <- ages
               ]
            -- A shallow source is always self-contained.
            ++ [ unchained {pTipsHeld = Just True, pAge = head' ages, pShallow = Just True}
               , moved {pShape = Just Chained, pChainIntact = Just True, pDepth = Just 1, pTipsHeld = Just True, pAge = head' ages, pShallow = Just True}
               ]
            -- No recorded pass start: no per-seat reuse offer.
            ++ [unchained {pTipsHeld = Just True, pAge = head' ages, pPassStart = Just False}]
    head' xs = case xs of
      x : _ -> x
      [] -> Nothing

-- | A row's decision, the same for every completion of its "-" inputs.
rowDecision :: PIn -> Either String Decision
rowDecision p = case map decide (expand p) of
  [] -> Left "a row with an empty input domain"
  d : ds
    | all (== d) ds -> Right d
    | otherwise -> Left "a \"-\" input changes the decision"

columns :: [String]
columns =
  [ "lane", "grouped", "base", "retained", "key_equal", "drifted", "settled", "pass_start"
  , "shape", "chain_intact", "prev_base", "depth", "age", "tips_held", "root_held", "shallow"
  , "depth_limit", "root_window", "chain_under_base"
  , "decision", "basis", "plan_depth", "rebase", "reuse", "refusal"
  ]

baseLabel :: BaseState -> String
baseLabel b = case b of
  NoGroup -> "NoGroup"
  BaseAbsent -> "Absent"
  BaseRetained -> "Retained"
  BaseLost -> "Lost"

prevBaseLabel :: PrevBase -> String
prevBaseLabel b = case b of
  PrevNone -> "None"
  PrevRetained -> "Retained"
  PrevLost -> "Lost"

showBool :: Bool -> String
showBool b = if b then "true" else "false"

renderRow :: Lane -> PIn -> Decision -> String
renderRow lane p d =
  intercalate "\t" $
    [ laneName lane
    , opt showBool (pGrouped p)
    , opt baseLabel (pBase p)
    , opt show (pRetained p)
    , opt showBool (pKeyEqual p)
    , opt showBool (pDrifted p)
    , opt showBool (pSettled p)
    , opt showBool (pPassStart p)
    , opt show (pShape p)
    , opt showBool (pChainIntact p)
    , opt prevBaseLabel (pPrevBase p)
    , opt show (pDepth p)
    , opt show (pAge p)
    , opt showBool (pTipsHeld p)
    , opt showBool (pRootHeld p)
    , opt showBool (pShallow p)
    , show (depthLimit pol)
    , show (rootWindow pol)
    , showBool (chainUnderBase pol)
    , decisionLabel d
    ]
      ++ case d of
        Hit -> ["-", "-", "-", "-", "-"]
        Export pl -> [show (basis pl), show (planDepth pl), show (rebase pl), show (reuse pl), "-"]
        Refuse r -> ["-", "-", "-", "-", show r]
  where
    pol = pPolicy p
    opt f = maybe "-" f

rowsHeader :: [String]
rowsHeader =
  [ "# decide_rows.tsv: pinned rows of git carry's decision core, decide(Inputs) -> Decision (P67)."
  , "# Rendered by docs/formal/hs/GitCarryCore.hs (`rows`); do not edit. `just formal-nv` checks it"
  , "# byte for byte (`rows --check`). Reference: GitCarryCore.hs's decide, derived from v1 at main"
  , "# (estate.rs prepare_base, retained_capture, chainable, chain_offer; git_carry.rs ExportOptions,"
  , "# export_pass; chain.rs CHAIN_DEPTH_LIMIT) and OI-1003-Q46's re-root policy (OI-1003-Q43)."
  , "# lane: v1 = main's behaviour (root_window 0, chain_under_base false); L6b = fix 2, a chain kept"
  , "# under a plan base; L8 = Q46's re-root (the window 27 is a row parameter, not a ruling)."
  , "# \"-\" in an input column: decide must not depend on that input in this row (GitCarryCore.hs"
  , "# checks every value of its domain); P67 draws it. \"-\" in an output column: not applicable."
  , "# Labels: basis, rebase, reuse, refusal and decision are catalogue/Types.dhall's closed unions."
  ]

-- | The rows file, or the first row whose "-" inputs are not ignored, or a
-- label of a closed union no row reaches.
renderRows :: Either String String
renderRows = do
  body <- mapM one [(lane, p) | lane <- allOf, p <- laneRows lane]
  let decisions = map snd body
      seen f = S.fromList (concatMap f decisions)
      plans = [pl | Export pl <- decisions]
      missing =
        [l | l <- map decisionLabel allDecisions, l `S.notMember` seen (pure . decisionLabel)]
          ++ [show b | b <- allOf :: [Basis], b `notElem` map basis plans]
          ++ [show r | r <- allOf :: [Rebase], r `notElem` map rebase plans]
          ++ [show r | r <- allOf :: [Reuse], r `notElem` map reuse plans]
          ++ [show r | r <- allOf :: [Refusal], Refuse r `notElem` decisions]
  if map (decisionTag . decisionExample) allOf /= allOf
    then Left "decisionExample does not give one example per DecisionTag"
    else
      if null missing
        then Right (unlines (rowsHeader ++ [intercalate "\t" columns] ++ map fst body))
        else Left ("no row reaches " ++ unwords missing)
  where
    one (lane, p) = case rowDecision p of
      Right d -> Right (renderRow lane p d, d)
      Left err -> Left (laneName lane ++ ": " ++ err ++ ": " ++ renderRow lane p Hit)

schemaLines :: [String]
schemaLines =
  [ unwords ("decision" : map decisionLabel allDecisions)
  , unwords ("basis" : map show (allOf :: [Basis]))
  , unwords ("rebase" : map show (allOf :: [Rebase]))
  , unwords ("reuse" : map show (allOf :: [Reuse]))
  , unwords ("refusal" : map show (allOf :: [Refusal]))
  ]

-- ---------------------------------------------------------------------------
-- The custody model (GitCarry.tla), for the explorer

data Mutation
  = ChainIgnoresDepth
  | GcDeletesDepended
  | BaseReplacedLive
  | SidecarAfterRecord
  | SkipFlattenVerify
  | HitIgnoresChainM
  | RerootPreMismatch
  deriving (Eq, Enum, Bounded)

mutationName :: Mutation -> String
mutationName m = case m of
  ChainIgnoresDepth -> "chain_ignores_depth"
  GcDeletesDepended -> "gc_deletes_depended"
  BaseReplacedLive -> "base_replaced_live"
  SidecarAfterRecord -> "sidecar_after_record"
  SkipFlattenVerify -> "skip_flatten_verify"
  HitIgnoresChainM -> "hit_ignores_chain"
  RerootPreMismatch -> "reroot_pre_mismatch"

data Config = Config
  { cPreset :: String
  , cItems :: Int
  , cPolicy :: Policy
  , cGC :: Bool
  , cCommits :: Int
  , cRewrites :: Int
  , cCrashes :: Int
  , cDamage :: Int
  , cDamageBase :: Bool
  , cDamageRewrites :: Bool
  , cBaseMissingTyped :: Bool
  , cMutation :: Maybe Mutation
  }

-- | The presets: GitCarry.tla's MC_gc_core (the code today), MC_gc_q46
-- (Q46's re-root and GC), MC_gc_grouped (two items on a plan base),
-- MC_gc_fix2 (L6b's fix 2 with Q46, on two items), MC_gc_reroot (Q46's
-- re-root twice in one window, at depth limit 1), MC_gc_reroot_extended
-- (a chain extended past a re-root: window = depth limit + 3) and
-- MC_gc_fix2_deep (fix 2 over two links under the plan base).
preset :: String -> Maybe Config
preset name = case name of
  "gc_core" -> Just (Config name 1 (Policy 2 0 False) False 3 1 1 1 False True False Nothing)
  "gc_q46" -> Just (Config name 1 (Policy 2 4 False) True 4 1 1 1 False True False Nothing)
  "gc_grouped" -> Just (Config name 2 (Policy 2 0 False) False 2 0 1 1 True True True Nothing)
  "gc_fix2" -> Just (Config name 2 (Policy 1 3 True) True 2 0 1 1 True True True Nothing)
  "gc_reroot" -> Just (Config name 1 (Policy 1 4 False) True 4 1 1 1 False True False Nothing)
  "gc_reroot_extended" -> Just (Config name 1 (Policy 2 5 False) True 4 1 1 1 False True False Nothing)
  "gc_fix2_deep" -> Just (Config name 2 (Policy 2 3 True) True 2 0 1 1 True True True Nothing)
  _ -> Nothing

presetNames :: [String]
presetNames = ["gc_core", "gc_q46", "gc_grouped", "gc_fix2", "gc_reroot", "gc_reroot_extended", "gc_fix2_deep"]

mut :: Config -> Mutation -> Bool
mut cfg m = cMutation cfg == Just m

tweak :: Config -> Tweak
tweak cfg
  | mut cfg ChainIgnoresDepth = IgnoreDepth
  | mut cfg HitIgnoresChainM = HitIgnoresChain
  | otherwise = Faithful

data Kind = KBase | KCapture
  deriving (Eq, Ord)

-- | A source tip, (ver, gen).
type Tip = (Int, Int)

-- | A bundle's CORPUS name, i.e. its bytes (GitCarry.tla's meta): kind,
-- item, the tip it captured, its basis and the prerequisite tips its header
-- declares (a set of at most one tip there, a Maybe here).
data Meta = Meta
  { mKind :: !Kind
  , mItem :: !Int
  , mVer :: !Int
  , mGen :: !Int
  , mBasis :: !Basis
  , mPre :: !(Maybe Tip)
  }
  deriving (Eq, Ord)

-- | A bundle's dependency sidecars (GitCarry.tla's sidecar): its .prior
-- (the link, and the link's depth as the .prior records it), its .base
-- and Q46's root age. noSide: no sidecar file.
data Side = Side {scPrior :: !Int, scPDepth :: !Int, scBase :: !Int, scAge :: !Int}
  deriving (Eq, Ord)

noSide :: Side
noSide = Side 0 0 0 0

data Phys = Staged | POk | PMissing | PReplaced | PCollected
  deriving (Eq, Ord)

data CapSt = CIdle | CBaseRec | CDecide | CPublish | CSidecars | CRecord
  deriving (Eq, Ord)

data Cap = Cap {capSt :: !CapSt, capItem :: !Int, capB :: !Int, capPlan :: !Side}
  deriving (Eq, Ord)

-- | GitCarry.tla's variables, one field each.
data St = St
  { sVer :: !Int
  , sGen :: !Int
  , sMeta :: !(M.Map Int Meta)
  , sCorpus :: !(M.Map Int Phys)
  , sSidecar :: !(M.Map Int Side)
  , sRec :: !(M.Map Int Int)
  , sBaseRec :: !Int
  , sCap :: !Cap
  , sCrashes :: !Int
  , sDamage :: !Int
  }
  deriving (Eq, Ord)

-- | GitCarry.tla's Group and Idle: TLC's CHOOSE picks one fixed item; any
-- fixed item will do.
groupItem :: Int
groupItem = 1

idleCap :: Cap
idleCap = Cap CIdle groupItem 0 noSide

items :: Config -> [Int]
items cfg = [1 .. cItems cfg]

isGrouped :: Config -> Bool
isGrouped cfg = cItems cfg > 1

initial :: Config -> St
initial cfg = St 0 0 M.empty M.empty M.empty (M.fromList [(i, 0) | i <- items cfg]) 0 idleCap 0 0

nIds :: St -> Int
nIds = M.size . sMeta

metaOf :: St -> Int -> Meta
metaOf st b = sMeta st M.! b

physOf :: St -> Int -> Phys
physOf st b = sCorpus st M.! b

sideOf :: St -> Int -> Side
sideOf st b = sSidecar st M.! b

recOf :: St -> Int -> Int
recOf st i = sRec st M.! i

-- | The bundle a CORPUS name already denotes, if any.
named :: St -> Meta -> Maybe Int
named st m = case [b | (b, x) <- M.toList (sMeta st), x == m] of
  b : _ -> Just b
  [] -> Nothing

present :: St -> Int -> Bool
present st b = physOf st b `elem` [POk, PReplaced]

hasSidecars :: St -> Int -> Bool
hasSidecars st b = sideOf st b /= noSide

linkOf :: St -> Int -> Int
linkOf st b = scPrior (sideOf st b)

boundOf :: St -> Int -> Int
boundOf st b = scBase (sideOf st b)

hasPrior :: St -> Int -> Bool
hasPrior st b = b /= 0 && linkOf st b /= 0

depthOf :: St -> Int -> Int
depthOf st b = if hasPrior st b then scPDepth (sideOf st b) + 1 else 0

tipOf :: St -> Int -> Tip
tipOf st b = let m = metaOf st b in (mVer m, mGen m)

declaresPrereqs :: St -> Int -> Bool
declaresPrereqs st b = mPre (metaOf st b) /= Nothing

closure :: St -> Int -> S.Set Int
closure st b
  | b == 0 = S.empty
  | otherwise = S.insert b (closure st (linkOf st b) `S.union` closure st (boundOf st b))

depended :: St -> S.Set Int
depended st = S.unions [closure st r | r <- M.elems (sRec st)]

-- | ChainWalk: the links of h's chain, oldest first, or Nothing (refused).
-- `ex` is the depth the current link's .prior must record once the walk
-- has left the head.
chainWalk :: Config -> St -> Int -> Bool -> Maybe [Int]
chainWalk cfg st h custody = go h [] 0
  where
    go cur acc ex
      | not (hasPrior st cur) = if null acc then Just [] else Nothing
      | d >= depthLimit (cPolicy cfg) || (not (null acc) && d /= ex) = Nothing
      | not (present st p) = Nothing
      | custody && physOf st p == PReplaced = Nothing
      | d == 0 = if hasPrior st p then Nothing else Just (p : acc)
      | otherwise = go p (p : acc) (d - 1)
      where
        p = linkOf st cur
        d = scPDepth (sideOf st cur)

rootOf :: Config -> St -> Int -> Int
rootOf cfg st b = case chainWalk cfg st b True of
  Just (r : _) -> r
  _ -> b

data Apply = Restored | Refused | Io
  deriving (Eq)

baseOutcome :: Config -> St -> Int -> Apply
baseOutcome cfg st b
  | not (present st b) = if cBaseMissingTyped cfg then Refused else Io
  | physOf st b == PReplaced = Refused
  | otherwise = Restored

flattenOutcome :: Config -> St -> Int -> Apply
flattenOutcome cfg st h = case chainWalk cfg st h False of
  Nothing -> Refused
  Just links
    | mut cfg SkipFlattenVerify -> Restored
    | any (\l -> physOf st l == PReplaced) links -> Refused
    | otherwise -> case links of
        oldest : _
          | not (declaresPrereqs st oldest) -> Restored
          | not (chainUnderBase (cPolicy cfg)) -> Refused
          | boundOf st oldest == 0 -> Io
          | otherwise -> baseOutcome cfg st (boundOf st oldest)
        [] -> Restored

applyOutcome :: Config -> St -> Int -> Apply
applyOutcome cfg st i
  | h == 0 = Refused
  | not (present st h) = Refused
  | physOf st h == PReplaced = Refused
  | hasPrior st h = flattenOutcome cfg st h
  | declaresPrereqs st h = if boundOf st h == 0 then Io else baseOutcome cfg st (boundOf st h)
  | otherwise = Restored
  where
    h = recOf st i

restorable :: Config -> St -> Int -> Bool
restorable cfg st i = applyOutcome cfg st i == Restored

applySeq :: Config -> St -> Int -> [Int]
applySeq cfg st i
  | hasPrior st h = case chainWalk cfg st h False of
      Just links@(oldest : _) ->
        let b0 = boundOf st oldest
         in [b0 | b0 /= 0, chainUnderBase (cPolicy cfg)] ++ links ++ [h]
      _ -> [h]
  | declaresPrereqs st h = [boundOf st h, h]
  | otherwise = [h]
  where
    h = recOf st i

-- | Every declared prerequisite tip is the tip of an intact bundle applied
-- before it.
satisfied :: St -> [Int] -> Bool
satisfied st sq =
  and
    [ or [j < k && tipOf st x == t && physOf st x == POk | (j, x) <- indexed]
    | (k, b) <- indexed
    , Just t <- [mPre (metaOf st b)]
    ]
  where
    indexed = zip [1 :: Int ..] sq

intact :: St -> Int -> Bool
intact st i =
  h /= 0
    && all (\b -> physOf st b == POk && (not (declaresPrereqs st b) || hasSidecars st b)) (S.toList (closure st h))
  where
    h = recOf st i

-- | Inputs(i), and whether a hit would have to read an absent .base.
inputsOf :: Config -> St -> Int -> (Inputs, Bool)
inputsOf cfg st i =
  ( Inputs
      { grouped = isGrouped cfg
      , base = baseState
      , retained = if h == 0 then NoRecord else if not held then BundleGone else Held
      , keyEqual = held && tipOf st h == (sVer st, sGen st)
      , drifted = False
      , settled = True
      , passStart = True
      , shape = shp
      , chainIntact = walkOk
      , prevBase = pb
      , depth = case walk of
          Just links | held && hasPrior st h -> length links
          _ -> 0
      , age = if held then scAge (sideOf st h) else 0
      , tipsHeld = held && mGen m == sGen st
      , rootHeld = held && mGen (metaOf st (rootOf cfg st h)) == sGen st
      , shallow = False
      , policy = cPolicy cfg
      }
  , unreadable
  )
  where
    h = recOf st i
    held = h /= 0 && physOf st h == POk
    walk = chainWalk cfg st h True
    walkOk = walk /= Nothing
    m = metaOf st h
    shp
      | not held = Unchained
      | hasPrior st h = Chained
      | declaresPrereqs st h = Based
      | otherwise = Unchained
    (pb, unreadable)
      | shp == Unchained = (PrevNone, False)
      | not (hasSidecars st h) = (PrevNone, True)
      | boundOf st h == 0 = (PrevNone, False)
      | physOf st (boundOf st h) == POk = (PrevRetained, False)
      | otherwise = (PrevLost, False)
    baseState
      | not (isGrouped cfg) = NoGroup
      | sBaseRec st == 0 = BaseAbsent
      | physOf st (sBaseRec st) == POk = BaseRetained
      | otherwise = BaseLost

data Outcome = Decided Decision | IoOutcome
  deriving (Eq)

-- | A capture step's decision, as its label shows it: Hit, Io,
-- Refuse:<refusal> or Export:<basis>:<rebase>.
outcomeTag :: Outcome -> String
outcomeTag o = case o of
  Decided Hit -> "Hit"
  Decided (Refuse r) -> "Refuse:" ++ show r
  Decided (Export pl) -> "Export:" ++ show (basis pl) ++ ":" ++ show (rebase pl)
  IoOutcome -> "Io"

captureOutcome :: Config -> St -> Int -> Outcome
captureOutcome cfg st i
  | base inp /= BaseLost && hitPath tw inp && needsBoundBase inp && unreadable = IoOutcome
  | otherwise = Decided (decideWith tw inp)
  where
    tw = tweak cfg
    (inp, unreadable) = inputsOf cfg st i

-- | LinkOf: the link an exported capture chains on (its .prior).
linkFor :: Config -> St -> Int -> Plan -> Int
linkFor cfg st i pl
  | basis pl `elem` [Chain, BaseAndChain] = if rebase pl == Reroot then rootOf cfg st h else h
  | otherwise = 0
  where
    h = recOf st i

-- | DeclaredTips: the prerequisite tips its header declares.
declaredTips :: Config -> St -> Int -> Plan -> Maybe Tip
declaredTips cfg st i pl
  | basis pl `elem` [Chain, BaseAndChain] =
      Just (tipOf st (if rebase pl == Reroot && mut cfg RerootPreMismatch then recOf st i else linkFor cfg st i pl))
  | basis pl == Base = Just (tipOf st (sBaseRec st))
  | otherwise = Nothing

newMeta :: Config -> St -> Int -> Plan -> Meta
newMeta cfg st i pl = Meta KCapture i (sVer st) (sGen st) (basis pl) (declaredTips cfg st i pl)

newPlan :: Config -> St -> Int -> Plan -> Side
newPlan cfg st i pl =
  Side
    l
    (if l == 0 then 0 else planDepth pl - 1)
    (if basis pl `elem` [Base, BaseAndChain] then sBaseRec st else 0)
    (if planDepth pl == 0 then 0 else scAge (sideOf st (recOf st i)) + 1)
  where
    l = linkFor cfg st i pl

-- ---------------------------------------------------------------------------
-- Actions, in Next's disjunct order (TLC's one-worker search order)

type Step = (String, St)

successors :: Config -> St -> [Step]
successors cfg st =
  concat [startBase cfg st i | i <- items cfg]
    ++ baseRecord st
    ++ concat [capture cfg st i | i <- items cfg]
    ++ publish cfg st
    ++ sidecars cfg st
    ++ record cfg st
    ++ gc cfg st
    ++ advance cfg st
    ++ rewrite cfg st
    ++ damage cfg st
    ++ crash cfg st

baseNeeded :: Config -> St -> Bool
baseNeeded cfg st =
  sBaseRec st == 0 || (mut cfg BaseReplacedLive && physOf st (sBaseRec st) /= POk)

itemName :: Int -> String
itemName i = "i" ++ show i

-- | StartBase: a name that holds other bytes refuses DIGEST_MISMATCH and
-- leaves the state as it was.
startBase :: Config -> St -> Int -> [Step]
startBase cfg st i
  | capSt (sCap st) == CIdle && isGrouped cfg && baseNeeded cfg st =
      [ ( "StartBase(" ++ itemName i ++ ")"
        , case named st bm of
            Just b
              | physOf st b == PReplaced -> st
              | otherwise -> st {sCorpus = M.insert b POk (sCorpus st), sCap = Cap CBaseRec i b noSide}
            Nothing ->
              let b = nIds st + 1
               in st
                    { sMeta = M.insert b bm (sMeta st)
                    , sCorpus = M.insert b POk (sCorpus st)
                    , sSidecar = M.insert b noSide (sSidecar st)
                    , sCap = Cap CBaseRec i b noSide
                    }
        )
      ]
  | otherwise = []
  where
    bm = Meta KBase groupItem (sVer st) (sGen st) SelfContained Nothing

baseRecord :: St -> [Step]
baseRecord st
  | capSt c == CBaseRec = [("BaseRecord", st {sBaseRec = capB c, sCap = c {capSt = CDecide, capB = 0}})]
  | otherwise = []
  where
    c = sCap st

capture :: Config -> St -> Int -> [Step]
capture cfg st i
  | enabled = case outcome of
      Decided (Export pl) ->
        let m = newMeta cfg st i pl
            plan = newPlan cfg st i pl
         in case named st m of
              Just b -> [(label, st {sCap = Cap CPublish i b plan})]
              Nothing ->
                let b = nIds st + 1
                 in [ ( label
                      , st
                          { sMeta = M.insert b m (sMeta st)
                          , sCorpus = M.insert b Staged (sCorpus st)
                          , sSidecar = M.insert b noSide (sSidecar st)
                          , sCap = Cap CPublish i b plan
                          }
                      )
                    ]
      _ -> [(label, st {sCap = idleCap})]
  | otherwise = []
  where
    c = sCap st
    outcome = captureOutcome cfg st i
    label = "Capture(" ++ itemName i ++ ") " ++ outcomeTag outcome
    enabled =
      (capSt c == CIdle && not (isGrouped cfg && baseNeeded cfg st))
        || (capSt c == CDecide && capItem c == i)

-- | Publish: a name rewritten in place refuses DIGEST_MISMATCH; the pass
-- ends with no record.
publish :: Config -> St -> [Step]
publish cfg st
  | capSt c == CPublish =
      [ ( "Publish"
        , if physOf st (capB c) == PReplaced
            then st {sCap = idleCap}
            else
              st
                { sCorpus = M.insert (capB c) POk (sCorpus st)
                , sCap =
                    c
                      { capSt =
                          if declaresPrereqs st (capB c) && not (mut cfg SidecarAfterRecord)
                            then CSidecars
                            else CRecord
                      }
                }
        )
      ]
  | otherwise = []
  where
    c = sCap st

-- | Sidecars: publish_prior keeps an intact chain already recorded for the
-- name; a pass that does not chain writes no .prior.
sidecars :: Config -> St -> [Step]
sidecars cfg st
  | capSt c == CSidecars =
      [ ( "Sidecars"
        , st
            { sSidecar = M.insert b written (sSidecar st)
            , sCap = if mut cfg SidecarAfterRecord then idleCap else c {capSt = CRecord}
            }
        )
      ]
  | otherwise = []
  where
    c = sCap st
    b = capB c
    plan = capPlan c
    keep = scPrior plan == 0 || (hasPrior st b && chainWalk cfg st b True /= Nothing)
    written = if keep then (sideOf st b) {scBase = scBase plan} else plan

record :: Config -> St -> [Step]
record cfg st
  | capSt c == CRecord =
      [ ( "Record"
        , st
            { sRec = M.insert (capItem c) (capB c) (sRec st)
            , sCap =
                if mut cfg SidecarAfterRecord && declaresPrereqs st (capB c)
                  then c {capSt = CSidecars}
                  else idleCap
            }
        )
      ]
  | otherwise = []
  where
    c = sCap st

gcLive :: Config -> St -> S.Set Int
gcLive cfg st
  | mut cfg GcDeletesDepended = S.fromList (sBaseRec st : M.elems (sRec st))
  | otherwise = depended st `S.union` closure st (sBaseRec st)

garbage :: Config -> St -> [Int]
garbage cfg st = [b | b <- [1 .. nIds st], present st b, b `S.notMember` live]
  where
    live = gcLive cfg st

-- | GC: one garbage bundle and its sidecars per step.
gc :: Config -> St -> [Step]
gc cfg st
  | cGC cfg && capSt (sCap st) == CIdle =
      [ ( "GC(" ++ show b ++ ")"
        , st
            { sCorpus = M.insert b PCollected (sCorpus st)
            , sSidecar = M.insert b noSide (sSidecar st)
            }
        )
      | b <- garbage cfg st
      ]
  | otherwise = []

advance :: Config -> St -> [Step]
advance cfg st
  | sVer st < cCommits cfg = [("Advance", st {sVer = sVer st + 1})]
  | otherwise = []

rewrite :: Config -> St -> [Step]
rewrite cfg st
  | sGen st < cRewrites cfg = [("Rewrite", st {sGen = sGen st + 1})]
  | otherwise = []

damage :: Config -> St -> [Step]
damage cfg st
  | sDamage st < cDamage cfg =
      [ ("Damage(" ++ show b ++ "," ++ physName how ++ ")", st {sCorpus = M.insert b how (sCorpus st), sDamage = sDamage st + 1})
      | b <- [1 .. nIds st]
      , physOf st b == POk
      , cDamageBase cfg || mKind (metaOf st b) == KCapture
      , how <- if cDamageRewrites cfg then [PMissing, PReplaced] else [PMissing]
      ]
  | otherwise = []

crash :: Config -> St -> [Step]
crash cfg st
  | sCrashes st < cCrashes cfg && capSt (sCap st) /= CIdle =
      [("Crash", st {sCap = idleCap, sCrashes = sCrashes st + 1})]
  | otherwise = []

-- ---------------------------------------------------------------------------
-- Invariants, in the configs' order

type Invariant = (String, Config -> St -> Bool)

invariants :: [Invariant]
invariants =
  [ ("TypeOK", typeOK)
  , ("ChainDepthBounded", chainDepthBounded)
  , ("PrereqsSatisfiedByEarlierLinks", prereqsSatisfied)
  , ("BrokenLinkNeverReuseHit", brokenLinkNeverReuseHit)
  , ("BaseNotReplacedWhileDepended", baseNotReplaced)
  , ("GCNeverDeletesDepended", gcNeverDeletesDepended)
  , ("SidecarsBeforeRecord", sidecarsBeforeRecord)
  , ("RestoreOrRecapture", restoreOrRecapture)
  ]

-- | The parts of TypeOK the Haskell types do not already guarantee.
typeOK :: Config -> St -> Bool
typeOK cfg st =
  sVer st <= cCommits cfg
    && sGen st <= cRewrites cfg
    && M.keys (sCorpus st) == M.keys (sMeta st)
    && M.keys (sSidecar st) == M.keys (sMeta st)
    && M.keys (sMeta st) == [1 .. nIds st]
    && and [mVer m <= cCommits cfg && mGen m <= cRewrites cfg && maybe True tipOk (mPre m) | m <- M.elems (sMeta st)]
    && and [inRange (scPrior s) && inRange (scBase s) && scPDepth s >= 0 && scAge s >= 0 | s <- M.elems (sSidecar st)]
    && S.size (S.fromList (M.elems (sMeta st))) == nIds st
    && all inRange (M.elems (sRec st))
    && inRange (sBaseRec st)
    && inRange (capB (sCap st))
    && sCrashes st <= cCrashes cfg
    && sDamage st <= cDamage cfg
  where
    inRange b = b >= 0 && b <= nIds st
    tipOk (v, g) = v >= 0 && v <= cCommits cfg && g >= 0 && g <= cRewrites cfg

chainDepthBounded :: Config -> St -> Bool
chainDepthBounded cfg st =
  all
    (\b -> depthOf st b <= depthLimit p && (rootWindow p == 0 || scAge (sideOf st b) < rootWindow p))
    [1 .. nIds st]
  where
    p = cPolicy cfg

prereqsSatisfied :: Config -> St -> Bool
prereqsSatisfied cfg st =
  and [satisfied st (applySeq cfg st i) | i <- items cfg, recOf st i /= 0, restorable cfg st i]

brokenLinkNeverReuseHit :: Config -> St -> Bool
brokenLinkNeverReuseHit cfg st =
  and [intact st i | i <- items cfg, captureOutcome cfg st i == Decided Hit]

baseNotReplaced :: Config -> St -> Bool
baseNotReplaced _ st =
  and
    [ b == sBaseRec st
    | r <- M.elems (sRec st)
    , b <- S.toList (closure st r)
    , mKind (metaOf st b) == KBase
    ]

gcNeverDeletesDepended :: Config -> St -> Bool
gcNeverDeletesDepended _ st =
  all (\b -> physOf st b /= PCollected) (S.toList (depended st `S.union` closure st (sBaseRec st)))

sidecarsBeforeRecord :: Config -> St -> Bool
sidecarsBeforeRecord _ st =
  and
    [ physOf st h /= Staged && (not (declaresPrereqs st h) || hasSidecars st h)
    | h <- M.elems (sRec st)
    , h /= 0
    ]

restoreOrRecapture :: Config -> St -> Bool
restoreOrRecapture cfg st =
  and
    [ a /= Io && (a == Restored || recaptures)
    | i <- items cfg
    , recOf st i /= 0
    , let a = applyOutcome cfg st i
    , let recaptures = case captureOutcome cfg st i of
            Decided (Export _) -> True
            Decided (Refuse _) -> True
            _ -> False
    ]

-- ---------------------------------------------------------------------------
-- Breadth-first search (as Explorer.hs)

data Failure = Failure
  { failWhat :: [String]
  , failTrace :: [Step]
  }

data Report = Report
  { repInitial :: Int
  , repDistinct :: Int
  , repGenerated :: Int -- initial states plus every successor computed
  , repDepth :: Int -- BFS levels, the initial state at level 1
  , repFailure :: Maybe Failure
  , repDecisions :: S.Set String -- every capture decision a step of the search took
  }

explore :: Config -> [Invariant] -> Report
explore cfg checks =
  case violatedBy s0 of
    [] -> level 1 [s0] seen0 1 S.empty
    b -> Report 1 1 1 1 (Just (Failure b [("Init", s0)])) S.empty
  where
    s0 = initial cfg
    seen0 = M.singleton s0 Nothing
    violatedBy st = [name | (name, ok) <- checks, not (ok cfg st)]
    trace seen st = reverse (go st)
      where
        go u = case M.lookup u seen of
          Just (Just (p, l)) -> (l, u) : go p
          _ -> [("Init", u)]
    tagOf l = case words l of
      [_, tag] -> S.singleton tag
      _ -> S.empty
    level depthN frontier seen generated decs
      | null frontier = Report 1 (M.size seen) generated (depthN - 1) Nothing decs
      | otherwise = scan frontier [] seen generated decs
      where
        scan [] next seen' gen ds = level (depthN + 1) (reverse next) seen' gen ds
        scan (st : rest) next seen' gen ds =
          case successors cfg st of
            [] -> Report 1 (M.size seen') gen depthN (Just (Failure ["deadlock"] (trace seen' st))) ds
            steps -> absorb st steps rest next seen' gen ds
        absorb _ [] rest next seen' gen ds = scan rest next seen' gen ds
        absorb from ((l, st) : more) rest next seen' gen ds
          | st `M.member` seen' = absorb from more rest next seen' (gen + 1) ds'
          | otherwise =
              let seen'' = M.insert st (Just (from, l)) seen'
               in case violatedBy st of
                    [] -> absorb from more rest (st : next) seen'' (gen + 1) ds'
                    b -> Report 1 (M.size seen'') (gen + 1) (depthN + 1) (Just (Failure b (trace seen'' st))) ds'
          where
            ds' = ds `S.union` tagOf l

-- ---------------------------------------------------------------------------
-- JSON counterexamples, with the spec's variable names and value spellings

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
      | otherwise = [c]

physName :: Phys -> String
physName p = case p of
  Staged -> "staged"
  POk -> "ok"
  PMissing -> "missing"
  PReplaced -> "replaced"
  PCollected -> "collected"

capStName :: CapSt -> String
capStName c = case c of
  CIdle -> "idle"
  CBaseRec -> "baserec"
  CDecide -> "decide"
  CPublish -> "publish"
  CSidecars -> "sidecars"
  CRecord -> "record"

stateJ :: St -> J
stateJ st =
  JO
    [ ("src", JO [("ver", JN (sVer st)), ("gen", JN (sGen st))])
    , ("meta", JA (map metaJ (M.elems (sMeta st))))
    , ("corpus", JA (map (JS . physName) (M.elems (sCorpus st))))
    , ("sidecar", JA (map sideJ (M.elems (sSidecar st))))
    , ("rec", JO [(itemName i, JN b) | (i, b) <- M.toList (sRec st)])
    , ("baseRec", JN (sBaseRec st))
    , ("cap", JO [("st", JS (capStName (capSt c))), ("item", JS (itemName (capItem c))), ("b", JN (capB c)), ("plan", sideJ (capPlan c))])
    , ("crashes", JN (sCrashes st))
    , ("damage", JN (sDamage st))
    ]
  where
    c = sCap st
    metaJ m =
      JO
        [ ("kind", JS (if mKind m == KBase then "base" else "capture"))
        , ("item", JS (itemName (mItem m)))
        , ("ver", JN (mVer m))
        , ("gen", JN (mGen m))
        , ("basis", JS (show (mBasis m)))
        , ("pre", JA [JO [("ver", JN v), ("gen", JN g)] | Just (v, g) <- [mPre m]])
        ]
    sideJ x =
      JO
        [ ("prior", JN (scPrior x))
        , ("pdepth", JN (scPDepth x))
        , ("base", JN (scBase x))
        , ("age", JN (scAge x))
        ]

boundName :: Config -> String
boundName cfg =
  intercalate
    ","
    [ "items=" ++ show (cItems cfg)
    , "L=" ++ show (depthLimit p)
    , "W=" ++ show (rootWindow p)
    , "cub=" ++ showBool (chainUnderBase p)
    , "gc=" ++ showBool (cGC cfg)
    , "C=" ++ show (cCommits cfg)
    , "R=" ++ show (cRewrites cfg)
    , "X=" ++ show (cCrashes cfg)
    , "D=" ++ show (cDamage cfg)
    , "db=" ++ showBool (cDamageBase cfg)
    , "dr=" ++ showBool (cDamageRewrites cfg)
    , "bmt=" ++ showBool (cBaseMissingTyped cfg)
    ]
  where
    p = cPolicy cfg

counterexample :: Config -> String -> [String] -> Failure -> String
counterexample cfg name checked f =
  "{\n"
    ++ concatMap (\(k, v) -> "  " ++ quote k ++ ": " ++ render v ++ ",\n") header
    ++ "  \"trace\": [\n    "
    ++ intercalate ",\n    " steps
    ++ "\n  ]\n}\n"
  where
    final = case reverse (failTrace f) of
      (_, s) : _ -> s
      [] -> initial cfg
    alsoFalse = [n | (n, ok) <- invariants, not (ok cfg final), n `notElem` failWhat f]
    header =
      [ ("explorer", JS "docs/formal/hs/GitCarryCore.hs")
      , ("row", JS name)
      , ("preset", JS (cPreset cfg))
      , ("bound", JS (boundName cfg))
      , ("mutation", JS (maybe "none" mutationName (cMutation cfg)))
      , ("checked", JA (map JS checked))
      , ("violated", JA (map JS (failWhat f)))
      , ("alsoFalseInFinalState", JA (map JS alsoFalse))
      , ("states", JN (length (failTrace f)))
      ]
    steps = [render (JO [("index", JN i), ("action", JS l), ("state", stateJ s)]) | (i, (l, s)) <- zip [1 :: Int ..] (failTrace f)]

-- ---------------------------------------------------------------------------
-- Command line

usage :: String -> IO a
usage err = do
  hPutStrLn stderr ("GitCarryCore: " ++ err)
  hPutStrLn stderr ("usage: GitCarryCore rows [--check FILE] | schema | explore [--preset " ++ intercalate "|" presetNames ++ "] [--items N] [--depth-limit N] [--root-window N] [--chain-under-base BOOL] [--gc BOOL] [--commits N] [--rewrites N] [--crashes N] [--damage N] [--damage-base BOOL] [--damage-rewrites BOOL] [--base-missing-typed BOOL] [--mutation NAME] [--check all|NAME,...] [--name NAME] [--json DIR]")
  exitWith (ExitFailure 2)

splitOn :: Char -> String -> [String]
splitOn c s = case break (== c) s of
  (a, []) -> [a]
  (a, _ : b) -> a : splitOn c b

count :: String -> Either String Int
count v
  | not (null v) && all isDigit v && length v <= 2 = Right (read v)
  | otherwise = Left ("a bound is a count from 0 to 99, not " ++ v)

bool :: String -> Either String Bool
bool v = case v of
  "true" -> Right True
  "false" -> Right False
  _ -> Left ("a flag is true or false, not " ++ v)

data Opts = Opts
  { oPreset :: String
  , oSet :: [Config -> Either String Config]
  , oMutation :: Maybe String
  , oCheck :: String
  , oName :: Maybe String
  , oJson :: Maybe FilePath
  }

parseExplore :: [String] -> Either String Opts
parseExplore = go (Opts "gc_core" [] Nothing "all" Nothing Nothing)
  where
    set o f = o {oSet = oSet o ++ [f]}
    pol f cfg = cfg {cPolicy = f (cPolicy cfg)}
    go o [] = Right o
    go o ("--preset" : v : rest) = go o {oPreset = v} rest
    go o ("--items" : v : rest) = go (set o (\c -> count v >>= \n -> if n >= 1 then Right c {cItems = n} else Left "--items is at least 1")) rest
    go o ("--depth-limit" : v : rest) = go (set o (\c -> count v >>= \n -> if n >= 1 then Right (pol (\p -> p {depthLimit = n}) c) else Left "--depth-limit is at least 1")) rest
    go o ("--root-window" : v : rest) = go (set o (\c -> count v >>= \n -> Right (pol (\p -> p {rootWindow = n}) c))) rest
    go o ("--chain-under-base" : v : rest) = go (set o (\c -> bool v >>= \b -> Right (pol (\p -> p {chainUnderBase = b}) c))) rest
    go o ("--gc" : v : rest) = go (set o (\c -> bool v >>= \b -> Right c {cGC = b})) rest
    go o ("--commits" : v : rest) = go (set o (\c -> count v >>= \n -> Right c {cCommits = n})) rest
    go o ("--rewrites" : v : rest) = go (set o (\c -> count v >>= \n -> Right c {cRewrites = n})) rest
    go o ("--crashes" : v : rest) = go (set o (\c -> count v >>= \n -> Right c {cCrashes = n})) rest
    go o ("--damage" : v : rest) = go (set o (\c -> count v >>= \n -> Right c {cDamage = n})) rest
    go o ("--damage-base" : v : rest) = go (set o (\c -> bool v >>= \b -> Right c {cDamageBase = b})) rest
    go o ("--damage-rewrites" : v : rest) = go (set o (\c -> bool v >>= \b -> Right c {cDamageRewrites = b})) rest
    go o ("--base-missing-typed" : v : rest) = go (set o (\c -> bool v >>= \b -> Right c {cBaseMissingTyped = b})) rest
    go o ("--mutation" : v : rest) = go o {oMutation = if v == "none" then Nothing else Just v} rest
    go o ("--check" : v : rest) = go o {oCheck = v} rest
    go o ("--name" : v : rest) = go o {oName = Just v} rest
    go o ("--json" : v : rest) = go o {oJson = Just v} rest
    go _ (a : _) = Left ("unknown or incomplete option: " ++ a)

runExplore :: [String] -> IO ()
runExplore args = do
  opts <- either usage pure (parseExplore args)
  base0 <- maybe (usage ("unknown preset " ++ oPreset opts)) pure (preset (oPreset opts))
  configured <- either usage pure (foldl (\acc f -> acc >>= f) (Right base0) (oSet opts))
  mutation <- case oMutation opts of
    Nothing -> pure Nothing
    Just m -> case [x | x <- allOf, mutationName x == m] of
      [x] -> pure (Just x)
      _ -> usage ("unknown mutation " ++ m ++ " (known: " ++ unwords (map mutationName allOf) ++ ")")
  let cfg = configured {cMutation = mutation}
      known = map fst invariants
      wanted = if oCheck opts == "all" then known else splitOn ',' (oCheck opts)
      name = case oName opts of
        Just n -> n
        Nothing -> case mutation of
          Just m -> "MC_gc_neg_" ++ mutationName m
          Nothing -> "MC_" ++ cPreset cfg
  case filter (`notElem` known) wanted of
    [] -> pure ()
    bad -> usage ("unknown invariant(s): " ++ unwords bad)
  if all (\ch -> isAlphaNum ch || ch == '_') name then pure () else usage "a row name is letters, digits and _"
  let checks = [inv | inv@(n, _) <- invariants, n `elem` wanted]
      rep = explore cfg checks
      common =
        [ ("row", name)
        , ("preset", cPreset cfg)
        , ("bound", boundName cfg)
        , ("mutation", maybe "none" mutationName mutation)
        , ("initial", show (repInitial rep))
        , ("distinct", show (repDistinct rep))
        , ("generated", show (repGenerated rep))
        , ("depth", show (repDepth rep))
        , ("decisions", intercalate "," (S.toList (repDecisions rep)))
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

main :: IO ()
main = do
  args <- getArgs
  case args of
    ["rows"] -> either (\e -> hPutStrLn stderr ("GitCarryCore rows: " ++ e) >> exitWith (ExitFailure 1)) putStr renderRows
    ["rows", "--check", file] -> do
      rendered <- either (\e -> hPutStrLn stderr ("GitCarryCore rows: " ++ e) >> exitWith (ExitFailure 1)) pure renderRows
      committed <- readFile file
      if committed == rendered
        then putStrLn ("rows: " ++ file ++ " is current (" ++ show (length (lines rendered) - length rowsHeader - 1) ++ " rows)")
        else do
          let width = max (length (lines committed)) (length (lines rendered)) + 1
              padded xs = take width (map Just xs ++ repeat Nothing)
              diffs = [n | (n, a, b) <- zip3 [1 :: Int ..] (padded (lines committed)) (padded (lines rendered)), a /= b]
          hPutStrLn stderr ("GitCarryCore rows: " ++ file ++ " is not the rendering" ++ concat [" (first difference at line " ++ show n ++ ")" | n : _ <- [diffs]])
          exitWith (ExitFailure 1)
    ["schema"] -> mapM_ putStrLn schemaLines
    "explore" : rest -> runExplore rest
    _ -> usage "expected rows, rows --check FILE, schema or explore"
