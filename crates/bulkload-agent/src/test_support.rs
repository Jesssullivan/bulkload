//! Shared property-test configuration (OI-1003-Q7, OI-1003-Q78;
//! property-test plan PR0).
//!
//! Every property that runs through [`prop_config`] draws one bounded,
//! fixed-seed corpus, and nothing is persisted between runs. The helper
//! names one seed, in every tier (OI-1003-Q78): the deep tier
//! (`BULKLOAD_PROPTEST_DEEP=1`, `just props-deep`) keeps [`CI_SEED`] and
//! only multiplies the case count by twenty. A deep run is therefore the PR
//! gate's own stream drawn further, and a deep failure reproduces with the
//! same command. A shape worth keeping is pinned as an explicit test row
//! rather than through a persistence file.
//!
//! Two properties do not run through this helper yet; they are the `EXEMPT`
//! entries of `tests/prop_seed_guard.rs`. `random_dags_equal_upload_pack`
//! (`tests/git_carry_v2.rs`) still draws a random seed in both tiers, until
//! Q42 L5 (#189) deletes its file. `tests/refusal_taxonomy.rs` has its own
//! fixed seed and does not multiply its cases in the deep tier.

use proptest::test_runner::{Config, RngSeed};

/// The fixed seed: every run, PR gate or deep tier, draws from it.
pub const CI_SEED: u64 = 0x0B01_C0AD_2026_1003;

/// The environment switch for the deep tier. It also gates the heavy fixed
/// rows that OI-1003-Q81 moved out of the PR gate.
pub const DEEP: &str = "BULKLOAD_PROPTEST_DEEP";

/// Whether this process runs the deep tier: [`DEEP`] is set to `1`.
#[must_use]
pub fn deep() -> bool {
    std::env::var_os(DEEP).is_some_and(|value| value == "1")
}

/// The configuration for a property with `cases` PR-gate cases, in the tier
/// this process runs: [`prop_config_for`] with [`deep`].
#[must_use]
pub fn prop_config(cases: u32) -> Config {
    prop_config_for(cases, deep())
}

/// The configuration for a property with `cases` PR-gate cases in a named
/// tier: the fixed seed in both, and twenty times the cases when `deep`.
/// The tier is a parameter so that `tests/prop_seed_guard.rs` holds both
/// tiers to the seed in the PR gate, where [`DEEP`] is never set. Properties
/// call [`prop_config`]; the guard refuses a call of this one.
#[must_use]
pub fn prop_config_for(cases: u32, deep: bool) -> Config {
    Config {
        cases: if deep {
            cases.saturating_mul(20)
        } else {
            cases
        },
        rng_seed: RngSeed::Fixed(CI_SEED),
        failure_persistence: None,
        ..Config::default()
    }
}
