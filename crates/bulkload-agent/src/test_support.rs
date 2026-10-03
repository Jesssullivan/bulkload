//! Shared test configuration (OI-1003-Q7; property-test plan PR0 convention).
//!
//! Every new property runs through [`prop_config`]: CI runs a fixed seed and a
//! bounded case count, so each run checks the same corpus. Locally,
//! `BULKLOAD_PROPTEST_DEEP=1` switches to random seeds and twenty times the
//! cases. A failing deep run prints its seed; pin that case as an explicit row
//! beside the property rather than relying on a persistence file.

use proptest::test_runner::{Config, RngSeed};

/// The CI seed. Fixed, so the CI corpus is bounded and reproducible.
const CI_SEED: u64 = 0x6275_6c6b_6c6f_6164;

/// A proptest configuration of `cases` fixed-seed cases, or twenty times as
/// many random-seed cases under `BULKLOAD_PROPTEST_DEEP`.
pub fn prop_config(cases: u32) -> Config {
    let deep = std::env::var_os("BULKLOAD_PROPTEST_DEEP").is_some_and(|value| value == "1");
    Config {
        cases: if deep {
            cases.saturating_mul(20)
        } else {
            cases
        },
        rng_seed: if deep {
            RngSeed::Random
        } else {
            RngSeed::Fixed(CI_SEED)
        },
        failure_persistence: None,
        ..Config::default()
    }
}
