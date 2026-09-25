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

#[cfg(test)]
pub(crate) mod test_env {
    //! Serializes unit tests that mutate process-global environment variables.
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Hold the returned guard for the whole test body.
    pub(crate) fn guard() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub mod achievements;
pub mod continuation;
pub mod cosigned_sth;
pub mod cross_node_login;
pub mod domain_proof;
pub mod event_payloads;
pub mod events;
pub mod guilds;
pub mod identity;
pub mod identity_chain;
pub mod identity_chain_wire;
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
