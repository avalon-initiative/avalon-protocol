//! Avalon Protocol domain model.
//!
//! Pure types and traits only — no I/O, no storage, no network calls. Every
//! other crate in this workspace, and every non-Rust SDK, treats these
//! definitions as the source of truth for what Avalon *is*, independent of how
//! any particular deployment implements it.
//!
//! See `docs/Proposal.md` for the narrative version of this model and
//! `docs/adr/` for the decisions behind specific boundaries (e.g. why identity
//! and game characters are modeled separately: ADR 0001).

pub mod achievements;
pub mod events;
pub mod games;
pub mod guilds;
pub mod identity;
pub mod ids;
pub mod permissions;
pub mod social;
