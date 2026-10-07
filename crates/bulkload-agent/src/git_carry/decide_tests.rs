//! P67 DECISION-CORE (Q42 lane L6a, OI-1003-Q43): git carry's decision core,
//! [`decide`], against its Haskell reference and its own laws.
//!
//! - **Pinned rows.** Every row of `tests/data/decide_rows.tsv`, which
//!   `docs/formal/hs/GitCarryCore.hs rows` renders from the reference
//!   `decide`, decides exactly the row's output under the row's policy, with
//!   each "-" input drawn at random (the fixed seed everywhere;
//!   `BULKLOAD_PROPTEST_DEEP=1` draws more from it, OI-1003-Q78). That is all
//!   three lanes: `v1`, whose policy is [`Policy::V1`] (the code before
//!   L6b), `L6b` (a chain under a plan base), whose policy is
//!   [`Policy::CODE`], the one the code runs, and `L8` (Q46's root window),
//!   whose policy `decide` implements before its lane lands the custody the
//!   callers need to act on it.
//! - **Labels.** The rows' output columns, over every lane, use exactly the
//!   labels of the Rust unions. The reference refuses to render rows that
//!   leave a label unreached, and `just formal-nv` requires its labels to be
//!   `catalogue/Types.dhall`'s, so the Rust variant names are the Dhall labels.
//! - **Totality.** For every input and policy, [`decide`] returns, and its
//!   decision is well-formed: a refusal only for a lost base, a hit never on
//!   a broken chain, a chained basis exactly when the depth is at least one
//!   and never past the limit, and so on (`well_formed`). Neither lazily read
//!   input ([`reads_prev_base`], [`reads_tips_held`]) changes a decision that
//!   is said not to rest on it.
//! - **Stages.** Under [`Policy::CODE`] and under [`Policy::V1`], the
//!   estate's decision (the record read,
//!   the bound base only when the decision rests on it, and the write-time
//!   inputs at their requested values), then the writer's decision on that
//!   offer (the tips read only when the decision rests on them), equal one
//!   decision on every input in kind, basis, depth and reuse offer. Each
//!   stage is the function the code calls ([`decide_recorded`] in
//!   `estate::decide_capture`, [`decide_offered`] in
//!   `shared::write_capture`), with its lazy read answered from the drawn
//!   inputs.

use std::fmt::Debug;

use proptest::prelude::*;
use proptest::test_runner::TestRunner;

use super::chain::CHAIN_DEPTH_LIMIT;
use super::decide::{
    decide, decide_offered, decide_recorded, reads_prev_base, reads_tips_held, BaseState, Basis,
    Decision, Inputs, Plan, Policy, PrevBase, Rebase, Refusal, RetainedState, ReuseEligibility,
    Shape,
};
use crate::test_support::prop_config;

/// The pinned rows, as the reference renders them.
const ROWS: &str = include_str!("../../tests/data/decide_rows.tsv");

const BASES: [BaseState; 4] = [
    BaseState::NoGroup,
    BaseState::Absent,
    BaseState::Retained,
    BaseState::Lost,
];
const RECORDS: [RetainedState; 3] = [
    RetainedState::NoRecord,
    RetainedState::BundleGone,
    RetainedState::Held,
];
const SHAPES: [Shape; 3] = [Shape::Unchained, Shape::Based, Shape::Chained];
const PREV_BASES: [PrevBase; 3] = [PrevBase::None, PrevBase::Retained, PrevBase::Lost];
const BASISES: [Basis; 4] = [
    Basis::SelfContained,
    Basis::Base,
    Basis::Chain,
    Basis::BaseAndChain,
];
const REBASES: [Rebase; 3] = [Rebase::NoRebase, Rebase::NewRoot, Rebase::Reroot];
const REUSES: [ReuseEligibility; 3] = [
    ReuseEligibility::NoRetained,
    ReuseEligibility::BlobReuse,
    ReuseEligibility::PassStartUnrecorded,
];
const REFUSALS: [Refusal; 1] = [Refusal::ReceiptBindingInvalid];

/// Draws per pinned row in CI.
const DRAWS_PER_ROW: u32 = 32;

/// The label of a decision's constructor.
const fn decision_label(decision: Decision) -> &'static str {
    match decision {
        Decision::Hit => "Hit",
        Decision::Export(_) => "Export",
        Decision::Refuse(_) => "Refuse",
    }
}

/// The value of `all` whose label (its variant name) is `text`.
fn label<T: Copy + Debug>(all: &[T], text: &str, line: usize) -> T {
    *all.iter()
        .find(|value| format!("{value:?}") == text)
        .unwrap_or_else(|| panic!("line {line}: {text} is not a label of the core"))
}

/// A label set: sorted, each label once.
fn labels<T: ToString>(values: impl IntoIterator<Item = T>) -> Vec<String> {
    let mut labels: Vec<String> = values.into_iter().map(|value| value.to_string()).collect();
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// A "-" input, or the label's value.
fn input<T>(text: &str, parse: impl Fn(&str) -> T) -> Option<T> {
    (text != "-").then(|| parse(text))
}

fn boolean(text: &str, line: usize) -> bool {
    match text {
        "true" => true,
        "false" => false,
        _ => panic!("line {line}: {text} is not a Bool"),
    }
}

fn number(text: &str, line: usize) -> u32 {
    text.parse()
        .unwrap_or_else(|_| panic!("line {line}: {text} is not a count"))
}

/// One pinned row: its lane, its inputs (`None` is "-") and its decision.
#[derive(Debug, Clone)]
struct Row {
    line: usize,
    lane: String,
    grouped: Option<bool>,
    base: Option<BaseState>,
    retained: Option<RetainedState>,
    key_equal: Option<bool>,
    drifted: Option<bool>,
    settled: Option<bool>,
    pass_start: Option<bool>,
    shape: Option<Shape>,
    chain_intact: Option<bool>,
    prev_base: Option<PrevBase>,
    depth: Option<u32>,
    age: Option<u32>,
    tips_held: Option<bool>,
    root_held: Option<bool>,
    shallow: Option<bool>,
    policy: Policy,
    decision: Decision,
}

const COLUMNS: [&str; 25] = [
    "lane",
    "grouped",
    "base",
    "retained",
    "key_equal",
    "drifted",
    "settled",
    "pass_start",
    "shape",
    "chain_intact",
    "prev_base",
    "depth",
    "age",
    "tips_held",
    "root_held",
    "shallow",
    "depth_limit",
    "root_window",
    "chain_under_base",
    "decision",
    "basis",
    "plan_depth",
    "rebase",
    "reuse",
    "refusal",
];

/// Every pinned row, in file order. The header must be the reference's
/// columns, and every label one of the core's.
fn rows() -> Vec<Row> {
    let mut lines = ROWS
        .lines()
        .enumerate()
        .map(|(index, text)| (index + 1, text))
        .filter(|(_, text)| !text.starts_with('#'));
    let (_, header) = lines.next().expect("a header line");
    assert_eq!(header.split('\t').collect::<Vec<_>>(), COLUMNS);
    lines
        .map(|(line, text)| {
            let cells: Vec<&str> = text.split('\t').collect();
            assert_eq!(cells.len(), COLUMNS.len(), "line {line}: {text}");
            let b = |text: &str| boolean(text, line);
            let n = |text: &str| number(text, line);
            let decision = match cells[19] {
                "Hit" => Decision::Hit,
                "Refuse" => Decision::Refuse(label(&REFUSALS, cells[24], line)),
                "Export" => Decision::Export(Plan {
                    basis: label(&BASISES, cells[20], line),
                    depth: n(cells[21]),
                    rebase: label(&REBASES, cells[22], line),
                    reuse: label(&REUSES, cells[23], line),
                }),
                other => panic!("line {line}: {other} is not a decision"),
            };
            Row {
                line,
                lane: cells[0].to_owned(),
                grouped: input(cells[1], b),
                base: input(cells[2], |text| label(&BASES, text, line)),
                retained: input(cells[3], |text| label(&RECORDS, text, line)),
                key_equal: input(cells[4], b),
                drifted: input(cells[5], b),
                settled: input(cells[6], b),
                pass_start: input(cells[7], b),
                shape: input(cells[8], |text| label(&SHAPES, text, line)),
                chain_intact: input(cells[9], b),
                prev_base: input(cells[10], |text| label(&PREV_BASES, text, line)),
                depth: input(cells[11], n),
                age: input(cells[12], n),
                tips_held: input(cells[13], b),
                root_held: input(cells[14], b),
                shallow: input(cells[15], b),
                policy: Policy {
                    depth_limit: n(cells[16]),
                    root_window: n(cells[17]),
                    chain_under_base: b(cells[18]),
                },
                decision,
            }
        })
        .collect()
}

/// A count around `limit` and the window, or any count at all.
fn count() -> BoxedStrategy<u32> {
    prop_oneof![0u32..=40, Just(u32::MAX), any::<u32>()].boxed()
}

/// The pinned value, or any value of the input's domain.
fn either<T: Clone + Debug + 'static>(
    pinned: Option<T>,
    any: BoxedStrategy<T>,
) -> BoxedStrategy<T> {
    pinned.map_or(any, |value| Just(value).boxed())
}

fn flag(pinned: Option<bool>) -> BoxedStrategy<bool> {
    either(pinned, any::<bool>().boxed())
}

fn of<T: Copy + Debug + 'static>(pinned: Option<T>, all: &'static [T]) -> BoxedStrategy<T> {
    either(pinned, proptest::sample::select(all).boxed())
}

/// The row's inputs, each "-" drawn from its whole domain.
fn completion(row: &Row) -> impl Strategy<Value = Inputs> {
    let policy = row.policy;
    (
        (
            flag(row.grouped),
            of(row.base, &BASES),
            of(row.retained, &RECORDS),
            flag(row.key_equal),
            flag(row.drifted),
            flag(row.settled),
            flag(row.pass_start),
            of(row.shape, &SHAPES),
        ),
        (
            flag(row.chain_intact),
            of(row.prev_base, &PREV_BASES),
            either(row.depth, count()),
            either(row.age, count()),
            flag(row.tips_held),
            flag(row.root_held),
            flag(row.shallow),
        ),
    )
        .prop_map(move |(first, second)| {
            let (grouped, base, retained, key_equal, drifted, settled, pass_start, shape) = first;
            let (chain_intact, prev_base, depth, age, tips_held, root_held, shallow) = second;
            Inputs {
                grouped,
                base,
                retained,
                key_equal,
                drifted,
                settled,
                pass_start,
                shape,
                chain_intact,
                prev_base,
                depth,
                age,
                tips_held,
                root_held,
                shallow,
                policy,
            }
        })
}

/// Any policy: the code's, small limits and windows, and any at all.
fn any_policy() -> impl Strategy<Value = Policy> {
    (
        prop_oneof![Just(CHAIN_DEPTH_LIMIT), 0u32..=12, any::<u32>()],
        prop_oneof![Just(0u32), 1u32..=40, any::<u32>()],
        any::<bool>(),
    )
        .prop_map(|(depth_limit, root_window, chain_under_base)| Policy {
            depth_limit,
            root_window,
            chain_under_base,
        })
}

/// Any inputs under a policy drawn from `policy`: a row of nothing but "-",
/// with the depth and the root age often at the edges of the policy's
/// limit and window, where the decision turns.
fn any_inputs(policy: impl Strategy<Value = Policy> + 'static) -> impl Strategy<Value = Inputs> {
    let blank = Row {
        line: 0,
        lane: String::new(),
        grouped: None,
        base: None,
        retained: None,
        key_equal: None,
        drifted: None,
        settled: None,
        pass_start: None,
        shape: None,
        chain_intact: None,
        prev_base: None,
        depth: None,
        age: None,
        tips_held: None,
        root_held: None,
        shallow: None,
        policy: Policy::V1,
        decision: Decision::Hit,
    };
    (completion(&blank), policy, 0u8..6, 0u8..6).prop_map(|(inputs, policy, depth, age)| {
        // One below, at and one past each edge, or the drawn value.
        let edge = |pick: u8, at: u32, drawn: u32| match pick {
            0 => at.saturating_sub(1),
            1 => at,
            2 => at.saturating_add(1),
            _ => drawn,
        };
        Inputs {
            depth: edge(depth, policy.depth_limit, inputs.depth),
            age: edge(age, policy.root_window.saturating_sub(1), inputs.age),
            policy,
            ..inputs
        }
    })
}

/// The laws every decision keeps. `Err` names the first one broken.
fn well_formed(inputs: &Inputs, decision: Decision) -> Result<(), String> {
    let law = |holds: bool, name: &str| if holds { Ok(()) } else { Err(name.to_owned()) };
    let held = inputs.retained == RetainedState::Held;
    let intact = inputs.shape != Shape::Chained || inputs.chain_intact;
    law(
        inputs.base != BaseState::Lost || matches!(decision, Decision::Refuse(_)),
        "a lost base refuses",
    )?;
    match decision {
        Decision::Refuse(Refusal::ReceiptBindingInvalid) => law(
            inputs.base == BaseState::Lost || (held && inputs.prev_base == PrevBase::Lost),
            "a refusal names a lost base",
        ),
        Decision::Hit => {
            let bound = match inputs.shape {
                Shape::Based => true,
                Shape::Chained => inputs.policy.chain_under_base,
                Shape::Unchained => false,
            };
            law(
                held && inputs.key_equal && !inputs.drifted && inputs.settled && intact,
                "a hit: the same key, no drift, settled, and an intact chain",
            )?;
            law(
                !bound || inputs.prev_base != PrevBase::Lost,
                "a hit on a bundle bound to a base needs that base retained",
            )
        }
        Decision::Export(plan) => {
            let policy = inputs.policy;
            law(
                (plan.reuse == ReuseEligibility::NoRetained) != held,
                "no reuse offer exactly when nothing is retained",
            )?;
            law(
                !held
                    || plan.reuse
                        == if inputs.pass_start {
                            ReuseEligibility::BlobReuse
                        } else {
                            ReuseEligibility::PassStartUnrecorded
                        },
                "a retained capture is offered for reuse only with a recorded pass start",
            )?;
            law(
                plan.basis.chained() == (plan.depth >= 1),
                "a chained basis exactly when the depth is at least one",
            )?;
            law(
                plan.depth <= policy.depth_limit.max(1),
                "never past the depth limit (ChainDepthBounded)",
            )?;
            law(
                plan.basis.based() == (inputs.grouped && !inputs.shallow),
                "a plan base exactly for a grouped item of a full source",
            )?;
            law(
                !inputs.shallow
                    || (plan.basis == Basis::SelfContained && plan.rebase == Rebase::NoRebase),
                "a shallow source is self-contained",
            )?;
            law(
                plan.basis != Basis::BaseAndChain || policy.chain_under_base,
                "a chain under a plan base only under fix 2",
            )?;
            law(
                !plan.basis.chained()
                    || (held
                        && intact
                        && (inputs.shape != Shape::Based || policy.chain_under_base)),
                "a chain only on a retained, intact, chainable bundle",
            )?;
            match plan.rebase {
                Rebase::NoRebase => law(
                    !plan.basis.chained()
                        || (inputs.tips_held && plan.depth == inputs.depth.saturating_add(1)),
                    "a chain link is one deeper than its prior, whose tips the source holds",
                ),
                Rebase::NewRoot => law(!plan.basis.chained(), "a new root is not chained"),
                Rebase::Reroot => law(
                    plan.basis.chained()
                        && plan.depth == 1
                        && policy.root_window > 0
                        && inputs.root_held,
                    "a re-root chains on the root at depth one, under a window",
                ),
            }
        }
    }
}

/// The estate's decision, then the writer's on its offer, as the code makes
/// them: [`decide_recorded`] (`estate::decide_capture`) under the inputs'
/// policy, and [`decide_offered`] (`shared::write_capture`), each lazy read
/// answered from `inputs`. The writer always runs [`Policy::CODE`]: an
/// estate under [`Policy::V1`] (the back-compat tests) never offers a link
/// beside a plan base, so the writer decides the same under either.
fn staged(inputs: &Inputs) -> Decision {
    // What `Inputs::new` leaves for later: the bound base and the write-time
    // inputs. `retained_capture` reads the rest, the record.
    let fresh = Inputs::new(inputs.grouped, inputs.base, inputs.policy);
    let recorded = Inputs {
        prev_base: fresh.prev_base,
        tips_held: fresh.tips_held,
        root_held: fresh.root_held,
        shallow: fresh.shallow,
        ..*inputs
    };
    let (decision, _) = decide_recorded(recorded, || Ok(inputs.prev_base))
        .unwrap_or_else(|refusal| panic!("the estate refused {refusal:?} on {inputs:?}"));
    let Decision::Export(plan) = decision else {
        return decision;
    };
    // `capture_item` offers the plan base when the basis is based, and
    // `chain_offer` the link when it is chained.
    let written = decide_offered(
        plan.basis.based(),
        plan.basis.chained().then_some(()),
        inputs.shallow,
        Policy::CODE,
        |()| Ok(inputs.tips_held),
    )
    .unwrap_or_else(|refusal| panic!("the writer refused {refusal:?} on {plan:?}, {inputs:?}"));
    Decision::Export(Plan {
        basis: written.basis,
        depth: if written.basis.chained() {
            plan.depth
        } else {
            0
        },
        // Informational without a root window: the estate's. A shallow source at the depth
        // limit reads NewRoot to the estate and NoRebase to one decision; the
        // bundle is self-contained either way.
        rebase: Rebase::NoRebase,
        reuse: plan.reuse,
    })
}

/// A decision without its rebase.
fn unrebased(decision: Decision) -> Decision {
    match decision {
        Decision::Export(plan) => Decision::Export(Plan {
            rebase: Rebase::NoRebase,
            ..plan
        }),
        other => other,
    }
}

#[test]
fn p67_the_pinned_rows_are_the_reference_s_lanes_and_the_code_s_policy() {
    let rows = rows();
    let lane = |name: &str| rows.iter().filter(|row| row.lane == name).count();
    assert_eq!(
        (lane("v1"), lane("L6b"), lane("L8"), rows.len()),
        (79, 85, 199, 363)
    );
    // The L6b rows pin the code's policy: CHAIN_DEPTH_LIMIT, no window, a
    // chain under a base. The v1 rows pin the policy before it.
    for row in rows.iter().filter(|row| row.lane == "L6b") {
        assert_eq!(row.policy, Policy::CODE, "line {}", row.line);
    }
    for row in rows.iter().filter(|row| row.lane == "v1") {
        assert_eq!(row.policy, Policy::V1, "line {}", row.line);
    }
    assert_eq!(
        Policy::CODE,
        Policy {
            chain_under_base: true,
            ..Policy::V1
        }
    );
}

#[test]
fn p67_the_rows_use_exactly_the_core_s_labels() {
    let rows = rows();
    let decisions: Vec<Decision> = rows.iter().map(|row| row.decision).collect();
    let plans: Vec<Plan> = decisions
        .iter()
        .filter_map(|decision| match decision {
            Decision::Export(plan) => Some(*plan),
            Decision::Hit | Decision::Refuse(_) => None,
        })
        .collect();
    assert_eq!(
        labels(decisions.iter().map(|decision| decision_label(*decision))),
        labels(["Hit", "Export", "Refuse"])
    );
    assert_eq!(
        labels(plans.iter().map(|plan| format!("{:?}", plan.basis))),
        labels(BASISES.iter().map(|basis| format!("{basis:?}")))
    );
    assert_eq!(
        labels(plans.iter().map(|plan| format!("{:?}", plan.rebase))),
        labels(REBASES.iter().map(|rebase| format!("{rebase:?}")))
    );
    assert_eq!(
        labels(plans.iter().map(|plan| format!("{:?}", plan.reuse))),
        labels(REUSES.iter().map(|reuse| format!("{reuse:?}")))
    );
    assert_eq!(
        labels(decisions.iter().filter_map(|decision| match decision {
            Decision::Refuse(refusal) => Some(format!("{refusal:?}")),
            Decision::Hit | Decision::Export(_) => None,
        })),
        labels(REFUSALS.iter().map(|refusal| format!("{refusal:?}")))
    );
}

/// P67, pinned: every row, of every lane, decides the reference's output
/// under its own policy, whatever its "-" inputs are. The policy is a column
/// of the row, so the L6b and L8 rows need no custody to check `decide`.
#[test]
fn p67_every_row_decides_what_the_reference_decides() {
    let rows = rows();
    assert_eq!(rows.len(), 363);
    for row in &rows {
        let mut runner = TestRunner::new(prop_config(DRAWS_PER_ROW));
        let expected = row.decision;
        let line = row.line;
        runner
            .run(&completion(row), |inputs| {
                prop_assert_eq!(decide(&inputs), expected, "line {}: {:?}", line, inputs);
                Ok(())
            })
            .unwrap_or_else(|failure| panic!("P67 {} line {line}: {failure}", row.lane));
    }
}

proptest! {
    #![proptest_config(prop_config(512))]

    /// P67, totality: decide returns on every input and policy, and its
    /// decision is well-formed. A decision said not to rest on the bound
    /// base or on the source's tips is the same whatever they are.
    #[test]
    fn p67_decide_is_total_and_well_formed(inputs in any_inputs(any_policy())) {
        let decision = decide(&inputs);
        if let Err(law) = well_formed(&inputs, decision) {
            return Err(TestCaseError::fail(format!("{law}: {inputs:?} -> {decision:?}")));
        }
        if !reads_prev_base(&inputs) {
            for prev_base in PREV_BASES {
                prop_assert_eq!(decide(&Inputs { prev_base, ..inputs }), decision);
            }
        }
        if !reads_tips_held(&inputs) {
            for tips_held in [false, true] {
                prop_assert_eq!(decide(&Inputs { tips_held, ..inputs }), decision);
            }
        }
    }

    /// P67, stages: under the code's policy, and under v1's, the estate's
    /// decision and then the writer's equal one decision on every input.
    #[test]
    fn p67_the_staged_decisions_are_one_decision(
        inputs in any_inputs(prop_oneof![Just(Policy::CODE), Just(Policy::V1)])
    ) {
        prop_assert_eq!(unrebased(staged(&inputs)), unrebased(decide(&inputs)), "{:?}", inputs);
    }
}
