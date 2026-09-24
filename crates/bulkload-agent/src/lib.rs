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

pub mod counters;
pub mod estate;
#[cfg(feature = "fault-injection")]
pub mod fault;
pub mod freshness;
pub mod git_carry;
pub mod handoff;
pub mod hash;
pub mod io;
pub mod materialize;
pub mod provider_sqlite;
pub mod transfer;
pub mod transfer_store;
pub mod walk;

pub use bulkload_proto::{BulkloadRefusal, Frame, FrameKind, Result, RowSchema};
