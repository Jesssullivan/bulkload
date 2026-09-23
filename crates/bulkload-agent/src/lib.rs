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

pub mod counters;
pub mod estate;
pub mod freshness;
pub mod git_carry;
pub mod handoff;
pub mod hash;
pub mod materialize;
pub mod provider_sqlite;
pub mod transfer;
pub mod transfer_store;
pub mod walk;

pub use bulkload_proto::{BulkloadRefusal, Frame, FrameKind, Result, RowSchema};
