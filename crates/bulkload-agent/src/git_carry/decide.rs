//! Git carry's decision core (Q42 lane L6a, OI-1003-Q43): a pure, total
//! [`decide`], with no IO.
//!
//! What a v1 capture decides before it exports, from what the code reads:
//! the group's plan base (`estate::prepare_base`), the item's capture record
//! (`estate::retained_capture`), the retained bundle's custody
//! (`estate::chainable`: `chain_links` under `LinkBinding::Custody`, or the
//! bundle's own prerequisites), and what the export pass observes (a shallow
//! source; the source holding a tip of the prior, [`super::chain`]). The
//! decision is a reuse hit, a typed refusal (R33: refusals are values), or
//! an export with a basis, a chain depth, a rebase and a reuse offer. The
//! variant names are the labels of the closed unions in
//! `docs/formal/catalogue/Types.dhall`.
//!
//! The reference copy is `decide` in `docs/formal/hs/GitCarryCore.hs`, and
//! its pinned rows (`tests/data/decide_rows.tsv`) pin this function: P67
//! checks every row of every lane (`v1`, `L6b`, `L8`), drawing each "-"
//! input at random. The policy is a column of each row, so every policy the
//! reference has is checked here (L6b's chain under a plan base, Q46's root
//! window). The code runs only [`Policy::CODE`], the L6b rows' policy (fix
//! 2, OI-1003-Q63 D4: no flag, and a reader from before it fails closed):
//! acting on Q46's root window waits for lane L8, which lands the custody
//! it needs. [`Policy::V1`] is what the code ran before L6b; the v1 rows
//! pin it, and the estate's back-compat tests capture under it.
//!
//! **Staged inputs.** The code reads its inputs in stages and decides at
//! each: the decision never rests on an input not read yet, because it is
//! decided again once that input is read, and each read happens only when
//! the decision depends on it, exactly where v1 read it.
//!
//! - `prepare_base` reads only the group's base. A lost base refuses before
//!   any item's record is read, as the reference's first rule reads nothing
//!   else.
//! - `capture_item` reads the record ([`Inputs::new`], then
//!   `retained_capture`) with the write-time inputs at the values under
//!   which a writer honours every offer, and decides through
//!   [`decide_recorded`]. It acts on a hit or a refusal, and for an export
//!   offers the writer the plan base or the chain link the basis names. A
//!   retained bundle's bound base is read only when the decision rests on
//!   it ([`reads_prev_base`]: a hit on a based bundle).
//! - The writer (`shared::write_capture`) decides again on that offer
//!   through [`decide_offered`] ([`Inputs::offered`]) once it has read
//!   whether the source is shallow, and reads whether the source holds a
//!   tip of the link only when the decision rests on it
//!   ([`reads_tips_held`]).
//!
//! The two stage functions take the lazy reads as callbacks, so the code
//! and P67 run the same staging: P67 checks that these stages, composed,
//! decide what one call on every input decides, under [`Policy::CODE`] and
//! under [`Policy::V1`].

use super::chain::CHAIN_DEPTH_LIMIT;
use crate::{BulkloadRefusal, Result};

/// The policy a capture runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The deepest a chain grows before a capture re-bases (or re-roots).
    pub depth_limit: u32,
    /// Q46's captures per root (lane L8); 0 never re-roots, as in v1.
    pub root_window: u32,
    /// L6b's fix 2: a capture under a plan base still chains on its prior.
    pub chain_under_base: bool,
}

impl Policy {
    /// The code's only policy (L6b's fix 2, OI-1003-Q63 D4): depth limit
    /// [`CHAIN_DEPTH_LIMIT`], no root window, and a grouped item's capture
    /// chains on its prior under the plan base. It is the L6b rows' policy.
    /// With no root window the chain re-bases on its ninth changed capture
    /// (OI-1003-Q62); only L8's re-root removes that re-pack.
    pub const CODE: Self = Self {
        depth_limit: CHAIN_DEPTH_LIMIT,
        root_window: 0,
        chain_under_base: true,
    };

    /// The code before L6b: depth limit [`CHAIN_DEPTH_LIMIT`], no root
    /// window, and no chain under a plan base. No caller runs it; the v1
    /// rows pin it and the back-compat tests write v1 corpora under it.
    pub const V1: Self = Self {
        depth_limit: CHAIN_DEPTH_LIMIT,
        root_window: 0,
        chain_under_base: false,
    };
}

/// The group's plan base record (`prepare_base`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseState {
    /// The item shares its common repository with no other item.
    NoGroup,
    /// No base record yet: `prepare_base` exports one.
    Absent,
    /// The base record's bundle is at its recorded identity.
    Retained,
    /// The base record's bundle is missing or not at its recorded identity.
    Lost,
}

/// The item's capture record (`retained_capture`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedState {
    /// No record.
    NoRecord,
    /// A record whose bundle is missing or not at its recorded identity.
    BundleGone,
    /// A record whose bundle is at its recorded identity.
    Held,
}

/// The retained bundle's dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Self-contained: no prerequisites and no `.prior`.
    Unchained,
    /// A plan base's delta: it declares prerequisites and has no `.prior`.
    Based,
    /// A chain link: it has a `.prior` sidecar.
    Chained,
}

/// The base the retained bundle's `{bundle}.base` sidecar names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrevBase {
    /// It names none.
    None,
    /// Its bundle is at its recorded identity.
    Retained,
    /// Its bundle is missing or not at its recorded identity.
    Lost,
}

/// What [`decide`] reads: the reference's `Inputs`, one field per input
/// column of the pinned rows.
#[allow(clippy::struct_excessive_bools)] // The reference's flat inputs, one per row column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    /// The item shares its common repository (`capture_groups`).
    pub grouped: bool,
    /// The group's plan base.
    pub base: BaseState,
    /// The item's capture record.
    pub retained: RetainedState,
    /// The record's key equals this pass's key.
    pub key_equal: bool,
    /// The retained `{bundle}.drift` is not empty.
    pub drifted: bool,
    /// A recorded pass start, with no seat racy since.
    pub settled: bool,
    /// `{bundle}.parts` records the pass start.
    pub pass_start: bool,
    /// The retained bundle's dependencies.
    pub shape: Shape,
    /// `chain_links` under `LinkBinding::Custody` succeeds.
    pub chain_intact: bool,
    /// The base the retained bundle's `.base` names.
    pub prev_base: PrevBase,
    /// Links in the retained bundle's chain: 0 for a self-contained bundle.
    pub depth: u32,
    /// Captures since the chain's root (Q46). Without re-roots, as in v1, it
    /// equals the depth.
    pub age: u32,
    /// The source holds a tip of the retained bundle (`source_held_tips`).
    pub tips_held: bool,
    /// The source holds a tip of the chain's root (Q46).
    pub root_held: bool,
    /// The source is shallow.
    pub shallow: bool,
    /// The policy.
    pub policy: Policy,
}

/// What a capture's bundle depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// Nothing: no prerequisite.
    SelfContained,
    /// The group's plan base.
    Base,
    /// The retained bundle, a chain link.
    Chain,
    /// Both (L6b's fix 2).
    BaseAndChain,
}

impl Basis {
    /// Whether the bundle declares a plan base's tips as prerequisites.
    #[must_use]
    pub const fn based(self) -> bool {
        matches!(self, Self::Base | Self::BaseAndChain)
    }

    /// Whether the bundle declares a chain link's tips as prerequisites.
    #[must_use]
    pub const fn chained(self) -> bool {
        matches!(self, Self::Chain | Self::BaseAndChain)
    }
}

/// Whether a capture ends its chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebase {
    /// It does not.
    NoRebase,
    /// The depth limit (or Q46's window) ends the chain, and the capture
    /// re-packs a fresh root.
    NewRoot,
    /// Q46: at the depth limit the capture chains on its chain's root.
    Reroot,
}

/// The retained capture's blobs, as the capture is offered them (R25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseEligibility {
    /// No retained capture.
    NoRetained,
    /// Every seat at an unchanged, non-racy identity is reused by name.
    BlobReuse,
    /// The retained capture records no pass start, so no seat is reused.
    PassStartUnrecorded,
}

/// The typed refusals the core returns (R33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A base that deltas depend on is lost: never replaced, never reused.
    ReceiptBindingInvalid,
}

impl From<Refusal> for BulkloadRefusal {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ReceiptBindingInvalid => Self::ReceiptBindingInvalid,
        }
    }
}

/// An export's plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// What its bundle depends on.
    pub basis: Basis,
    /// Its chain depth: 0 unless its basis is chained.
    pub depth: u32,
    /// Whether it ends its chain.
    pub rebase: Rebase,
    /// The reuse offer.
    pub reuse: ReuseEligibility,
}

/// What a capture does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The retained bundle is the capture.
    Hit,
    /// Export a new bundle.
    Export(Plan),
    /// Refuse, by type.
    Refuse(Refusal),
}

impl Inputs {
    /// An item's inputs before its record is read: no record, and the
    /// write-time inputs (`shallow`, `tips_held`, `root_held`) at the values
    /// under which a writer honours every offer. `retained_capture` fills in
    /// the record. For a group whose base is lost the decision reads nothing
    /// else, so `prepare_base` decides on these.
    #[must_use]
    pub const fn new(grouped: bool, base: BaseState, policy: Policy) -> Self {
        Self {
            grouped,
            base,
            retained: RetainedState::NoRecord,
            key_equal: false,
            drifted: false,
            settled: false,
            pass_start: false,
            shape: Shape::Unchained,
            chain_intact: true,
            prev_base: PrevBase::None,
            depth: 0,
            age: 0,
            tips_held: true,
            root_held: true,
            shallow: false,
            policy,
        }
    }

    /// A writer's inputs (`shared::write_capture`): the caller's offer, a
    /// plan base (`base`) and a retained capture to chain on (`link`), as
    /// `ExportOptions` carries them, and whether the pass found the source
    /// `shallow`. A caller that offers a link has decided the capture
    /// extends a chainable retained bundle below the depth limit (it owns
    /// the link's custody and depth, `ExportOptions::chain`). So the writer
    /// reads the link as a retained, unchained bundle at depth 0: it acts on
    /// the basis, and the caller on the depth. `tips_held` is true until the
    /// writer reads the source ([`reads_tips_held`]).
    #[must_use]
    pub const fn offered(base: bool, link: bool, shallow: bool, policy: Policy) -> Self {
        let mut inputs = Self::new(
            base,
            if base {
                BaseState::Retained
            } else {
                BaseState::NoGroup
            },
            policy,
        );
        if link {
            inputs.retained = RetainedState::Held;
        }
        inputs.root_held = false;
        inputs.shallow = shallow;
        inputs
    }
}

/// The decision a capture makes before it exports. Pure and total: it reads
/// only `inputs` and returns a value for every one.
#[must_use]
pub const fn decide(inputs: &Inputs) -> Decision {
    if matches!(inputs.base, BaseState::Lost) {
        // prepare_base: never replace a base older deltas depend on.
        return Decision::Refuse(Refusal::ReceiptBindingInvalid);
    }
    if !matches!(inputs.retained, RetainedState::Held) {
        // retained_capture: nothing retained, a fresh capture with no reuse.
        return Decision::Export(Plan {
            basis: root_basis(inputs),
            depth: 0,
            rebase: Rebase::NoRebase,
            reuse: ReuseEligibility::NoRetained,
        });
    }
    if hit_path(inputs) {
        // A hit on a bundle bound to a base needs that base retained.
        return if needs_bound_base(inputs) && matches!(inputs.prev_base, PrevBase::Lost) {
            Decision::Refuse(Refusal::ReceiptBindingInvalid)
        } else {
            Decision::Hit
        };
    }
    Decision::Export(extend(inputs))
}

/// Whether [`decide`] on `inputs` rests on the retained bundle's bound base.
///
/// That is, whether reading it can change the decision. The code reads the
/// `{bundle}.base` sidecars only then: a hit on a based bundle, as v1 did,
/// and under fix 2 a hit on a chained one, whose links may each bind a base.
#[must_use]
pub fn reads_prev_base(inputs: &Inputs) -> bool {
    let retained = Inputs {
        prev_base: PrevBase::Retained,
        ..*inputs
    };
    let lost = Inputs {
        prev_base: PrevBase::Lost,
        ..*inputs
    };
    decide(&retained) != decide(&lost)
}

/// Whether [`decide`] on `inputs` rests on the source holding a link's tip.
///
/// That is, whether reading it can change the decision. The writer queries
/// the source (`chain::source_held_tips`) only then: a chain link offered
/// and a source that is not shallow (and, under [`Policy::V1`], no plan
/// base, which wins there).
#[must_use]
pub fn reads_tips_held(inputs: &Inputs) -> bool {
    let held = Inputs {
        tips_held: true,
        ..*inputs
    };
    let none = Inputs {
        tips_held: false,
        ..*inputs
    };
    decide(&held) != decide(&none)
}

/// The estate's stage (`estate::decide_capture`).
///
/// The decision on `recorded`, an item's inputs once its capture record is
/// read ([`Inputs::new`], then `retained_capture`), reading the retained
/// bundle's bound base through `bound_base` only when the decision rests on
/// it ([`reads_prev_base`]). Returns the decision and the inputs it read.
///
/// # Errors
/// What `bound_base` refuses.
pub fn decide_recorded(
    recorded: Inputs,
    bound_base: impl FnOnce() -> Result<PrevBase>,
) -> Result<(Decision, Inputs)> {
    let mut inputs = recorded;
    if reads_prev_base(&inputs) {
        inputs.prev_base = bound_base()?;
    }
    Ok((decide(&inputs), inputs))
}

/// The writer's stage (`shared::write_capture`).
///
/// The plan for the caller's offer ([`Inputs::offered`]: a plan base, a
/// `link` to chain on, and whether the source is `shallow`), reading whether
/// the source holds a tip of the link through `tips_held` only when the
/// decision rests on it ([`reads_tips_held`]).
///
/// # Errors
/// What `tips_held` refuses, and `CONTRACT_SELF_INCONSISTENT` for a decision
/// that is not an export (no offer decides one).
pub fn decide_offered<L>(
    base: bool,
    link: Option<L>,
    shallow: bool,
    policy: Policy,
    tips_held: impl FnOnce(L) -> Result<bool>,
) -> Result<Plan> {
    let mut inputs = Inputs::offered(base, link.is_some(), shallow, policy);
    if let Some(link) = link {
        if reads_tips_held(&inputs) {
            inputs.tips_held = tips_held(link)?;
        }
    }
    match decide(&inputs) {
        Decision::Export(plan) => Ok(plan),
        Decision::Hit | Decision::Refuse(_) => Err(BulkloadRefusal::ContractSelfInconsistent),
    }
}

/// A fresh root: a plan base's delta, else self-contained. A shallow source
/// is always self-contained (its envelope declares no prerequisite).
const fn root_basis(inputs: &Inputs) -> Basis {
    if inputs.grouped && !inputs.shallow {
        Basis::Base
    } else {
        Basis::SelfContained
    }
}

/// `retained_capture`'s `restorable`: a chained bundle's chain is intact.
const fn chain_ok(inputs: &Inputs) -> bool {
    !matches!(inputs.shape, Shape::Chained) || inputs.chain_intact
}

/// A based bundle; under fix 2, a chained one too.
const fn needs_bound_base(inputs: &Inputs) -> bool {
    match inputs.shape {
        Shape::Based => true,
        Shape::Chained => inputs.policy.chain_under_base,
        Shape::Unchained => false,
    }
}

/// The same key, no drift, a settled pass start and a restorable chain.
const fn hit_path(inputs: &Inputs) -> bool {
    matches!(inputs.retained, RetainedState::Held)
        && inputs.key_equal
        && !inputs.drifted
        && inputs.settled
        && chain_ok(inputs)
}

/// `chainable`, `chain_offer` and the writer's choice, with Q46's re-root
/// policy and fix 2.
const fn extend(inputs: &Inputs) -> Plan {
    let policy = inputs.policy;
    let reuse = if inputs.pass_start {
        ReuseEligibility::BlobReuse
    } else {
        ReuseEligibility::PassStartUnrecorded
    };
    // A broken chain is never extended; a based bundle only under fix 2.
    let linkable = match inputs.shape {
        Shape::Chained => inputs.chain_intact,
        Shape::Based => policy.chain_under_base,
        Shape::Unchained => true,
    };
    // A plan base wins unless fix 2 keeps the chain under it.
    let offered = linkable && (!inputs.grouped || policy.chain_under_base);
    let chain_basis = if inputs.grouped {
        Basis::BaseAndChain
    } else {
        Basis::Chain
    };
    let window_ends = policy.root_window > 0 && inputs.age.saturating_add(1) >= policy.root_window;
    let (basis, depth, rebase) = if inputs.shallow || !offered {
        (root_basis(inputs), 0, Rebase::NoRebase)
    } else if window_ends {
        (root_basis(inputs), 0, Rebase::NewRoot)
    } else if inputs.depth < policy.depth_limit {
        if inputs.tips_held {
            // Below the limit, so the sum is at most the limit.
            (
                chain_basis,
                inputs.depth.saturating_add(1),
                Rebase::NoRebase,
            )
        } else {
            // The source holds none of its tips: nothing to exclude.
            (root_basis(inputs), 0, Rebase::NoRebase)
        }
    } else if policy.root_window > 0 && inputs.root_held {
        (chain_basis, 1, Rebase::Reroot)
    } else {
        (root_basis(inputs), 0, Rebase::NewRoot)
    };
    Plan {
        basis,
        depth,
        rebase,
        reuse,
    }
}
