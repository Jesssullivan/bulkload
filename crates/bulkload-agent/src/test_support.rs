//! Shared property-test configuration (OI-1003-Q7, OI-1003-Q78;
//! property-test plan PR0).
//!
//! Every property runs through [`prop_config`], so every run draws one
//! bounded, fixed-seed corpus and nothing is persisted between runs. The
//! seed is fixed everywhere (OI-1003-Q78): the deep tier
//! (`BULKLOAD_PROPTEST_DEEP=1`, `just props-deep`) keeps [`CI_SEED`] and
//! only multiplies the case count by twenty. A deep run is therefore the PR
//! gate's own stream drawn further, and a deep failure reproduces with the
//! same command. A shape worth keeping is pinned as an explicit test row
//! rather than through a persistence file.

use proptest::test_runner::{Config, RngSeed};

/// The fixed seed: every run, PR gate or deep tier, draws from it.
pub const CI_SEED: u64 = 0x0B01_C0AD_2026_1003;

/// The environment switch for the deep tier. It also gates the heavy fixed
/// rows that OI-1003-Q81 moved out of the PR gate.
pub const DEEP: &str = "BULKLOAD_PROPTEST_DEEP";

/// The configuration for a property with `cases` PR-gate cases: the fixed
/// seed always, and twenty times the cases in the deep tier.
#[must_use]
pub fn prop_config(cases: u32) -> Config {
    let deep = std::env::var_os(DEEP).is_some_and(|value| value == "1");
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
