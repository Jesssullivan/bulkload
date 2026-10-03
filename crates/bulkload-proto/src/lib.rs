//! Shared wire types for bulkload.
//!
//! This crate is the contract between the two ends of a bulkload session
//! (`bulkload-agent` running as source and as destination). It holds three
//! things and deliberately nothing else:
//!
//! * [`frame`] -- the wire v5 frame codec (R-N118).
//! * [`row`] -- the scanned-row schema the agent emits.
//! * [`refusal`] -- the [`BulkloadRefusal`] taxonomy.
//!
//! Everything here is `no_std`-friendly in spirit and panic-free in practice:
//! the crate carries the R33 deny-panics wall, so every fallible path returns
//! a `Result` carrying a [`BulkloadRefusal`].

pub mod frame;
pub mod refusal;
pub mod row;

pub use frame::{Control, Decision, Frame, PROTO_VERSION};
pub use refusal::{BulkloadRefusal, StderrClass};
pub use row::{FileKind, RowSchema};

/// Convenience alias: every fallible bulkload operation refuses with a code.
pub type Result<T> = core::result::Result<T, BulkloadRefusal>;
