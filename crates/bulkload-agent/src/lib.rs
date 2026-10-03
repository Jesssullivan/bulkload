//! Native ordinary-file transport and offline provider composition on Unix.
//!
//! # Scope
//!
//! The agent walks a corpus, transfers verified chunks in bounded frames,
//! preserves divergent destinations, and composes retained `SQLite` candidates.
//! It still enumerates the source on each run; resumable content completion is
//! not a no-rewalk guarantee. Ordinary-file transfer does not provide Git-native
//! divergent union or raw live `SQLite` copying. It does not talk to object storage, does
//! not speak HTTP or gRPC, and has no async runtime -- see the dependency wall
//! in `Cargo.toml` and the `dep_graph` integration test that enforces it.
//!
//! # Freshness
//!
//! The walker is written against the [`freshness::FreshnessCache`] trait
//! rather than any concrete cache, so callers choose a persistent, in-memory
//! or null cache.

// Crash-consistency fault points. With the `fault-injection` feature each
// expands to a call into `fault`; without it each expands to an empty block,
// so a shipped build carries no fault code at all. See `fault` for the list.
#[cfg(feature = "fault-injection")]
macro_rules! fault_point {
    ($point:ident) => {{
        $crate::fault::hit($crate::fault::Point::$point);
    }};
}

#[cfg(not(feature = "fault-injection"))]
macro_rules! fault_point {
    ($point:ident) => {{}};
}

// A fault point whose crash receipt also names the store root being written.
#[cfg(feature = "fault-injection")]
macro_rules! fault_point_in {
    ($point:ident, $root:expr) => {{
        $crate::fault::hit_in($crate::fault::Point::$point, $root);
    }};
}

#[cfg(not(feature = "fault-injection"))]
macro_rules! fault_point_in {
    ($point:ident, $root:expr) => {{}};
}

// Live-writer hook: runs registered source mutations once per capture, after
// the first content chunk has been read and before the rest of the file is.
#[cfg(feature = "fault-injection")]
macro_rules! fault_mid_read {
    ($first:expr, $path:expr) => {{
        if $first {
            $crate::fault::mid_read($path);
        }
    }};
}

#[cfg(not(feature = "fault-injection"))]
macro_rules! fault_mid_read {
    ($first:expr, $path:expr) => {{}};
}

pub mod child;
pub mod closure;
pub mod counters;
pub mod estate;
#[cfg(feature = "fault-injection")]
pub mod fault;
pub mod freshness;
pub mod git_carry;
pub mod hash;
// The engine's io layer (R-N90, R-N54, R-N88). W3's group commit
// (`io::durable`), the destination's materializer and the counted syncs run on
// its `sys` calls. The pieces W4's later PRs wire in carry their own narrowly
// scoped `dead_code` allowances.
pub(crate) mod io;

/// Group-commit durability controls (M2 W3): the `--durability` mode and the
/// per-file and per-directory seals. See `io::durable`.
pub mod durable {
    pub use crate::io::durable::*;
}

/// Open-file limits and the destination's descriptor budget (M2 W3).
pub mod limits {
    pub use crate::io::limits::*;
}

/// The R-N88 syscall trace (`io::trace`). Public only with the `io-trace`
/// feature, so the fault harness can record a real `copy`; a default build
/// exports nothing here.
#[cfg(feature = "io-trace")]
pub mod trace {
    pub use crate::io::trace::*;
    pub use crate::io::NodeId;
}

/// The R-N88 crash-state checker (`io::crash_check`), public only with the
/// `io-trace` feature.
#[cfg(feature = "io-trace")]
pub mod crash_check {
    pub use crate::io::crash_check::*;
}
pub mod materialize;
pub mod provider_sqlite;
pub mod space;
#[cfg(test)]
pub(crate) mod test_support;
pub mod transfer;
pub mod transfer_store;
pub mod walk;

pub use bulkload_proto::{BulkloadRefusal, Control, Decision, Frame, Result, RowSchema};
