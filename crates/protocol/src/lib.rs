//! Avalon Protocol domain model.
//!
//! Pure types and traits only — no I/O, no storage, no network calls. Every
//! other crate in this workspace, and every non-Rust SDK, treats these
//! definitions as the source of truth for what Avalon *is*, independent of how
//! any particular deployment implements it.
//!
//! See `docs/stakeholders/Proposal.md` for the narrative version of this model. Real
//! architecture decisions are recorded as closed GitHub issues labeled
//! `architecture-decision-record`, not as files in this repo.

pub mod achievements;
pub mod continuation;
pub mod cross_node_login;
pub mod event_payloads;
pub mod events;
pub mod guilds;
pub mod identity;
pub mod ids;
pub mod integrator_schema_mappings;
pub mod integrator_schemas;
pub mod integrators;
pub mod interest_claim;
pub mod known_list;
pub mod network_trust;
pub mod permissions;
pub mod revocation;
pub mod shard;
pub mod shard_identity;
pub mod social;
pub mod sth;
pub mod witness;
