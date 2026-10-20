//! Consolidating the agent homes (operator ruling 2026-10-20).
//!
//! The source's `~/.claude`, `~/.claude.json` and `~/.codex` become one
//! with the destination's live homes, never a second copy beside them. "No
//! dupes, no duplicated besides, no stage dupes": SPEC RUL-10, a lossless
//! union into DEST (C10; LIVE-15, LIVE-18, CONF-05, KEEP-36).
//!
//! Each file class has its own union. This module holds the structured
//! config one ([`config`]: JSON and TOML, merged key by key), the values
//! both parse into ([`value`]), and the parsers and writers ([`json`],
//! [`toml`]).

pub mod config;
pub mod json;
pub mod toml;
pub mod value;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines
)]
mod tests;

/// Why a file is not consolidated: it is held typed
/// (`HOMES_CONSOLIDATE_UNSUPPORTED`), nothing written, and listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);
