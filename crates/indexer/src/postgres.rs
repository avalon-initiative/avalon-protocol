//! `PostgresIndexer` — the first real [`Indexer`], closing issue #42.
//!
//! Dispatches by `event.kind` to the matching module under
//! [`crate::projections`], guarded by an `indexer_applied_events(event_id)`
//! dedup table so a redelivered or replayed (rebuild) event is a no-op
//! the second time — the two-layer idempotency design in the ticket:
//! per-event dedup here, natural-key upserts inside each projection.
//!
//! Two entry points, both doing the same dispatch:
//!
//! - [`Indexer::apply`] opens its own transaction — the shape a background
//!   consumer (a future outbox-style worker, or #43's rebuild-from-events
//!   pass) uses.
//! - [`PostgresIndexer::apply_in_tx`] takes a transaction the caller already
//!   owns, so a request handler's app-data write and its projection update
//!   commit or roll back together — see `crates/server/src/handlers.rs`
//!   (`register_finish`/`update_profile`), which call this instead of
//!   writing `profiles` themselves.

use std::sync::Arc;

use async_trait::async_trait;
use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Transaction};

use crate::attestation_proof;
use crate::head_attestor::HeadAttestor;
use crate::identity_chain_store::{self, Recorded};
use crate::identity_proof::{self, EventOrigin, Verified};
use crate::projections::{
    attestations, friendships, guild_rosters, identity_passkeys, identity_signing_keys,
    integrator_bindings, integrator_data_instances, integrator_recognitions,
    integrator_schema_mappings, integrator_schemas, issuer_keys, profiles,
};
use crate::{IndexError, Indexer};
use time::OffsetDateTime;

/// Every table a `PostgresIndexer` owns — closing issue #43: this is the
/// literal list `rebuild_from_scratch` truncates before replaying, and the
/// list the disaster-recovery test snapshots/diffs. Kept as one place so
/// adding a new projection (a new module under `projections/`) has one
/// obvious spot to also register its table here, instead of a rebuild
/// silently missing it. Deliberately does not include `indexer_applied_events`
/// alongside the read-model tables in doc comments elsewhere — it's a
/// dedup ledger, not a promised-durable projection itself, but it still has
/// to be truncated for a rebuild to actually re-apply anything.
pub const PROJECTION_TABLES: &[&str] = &[
    "indexer_applied_events",
    "profiles",
    "indexer_friendships",
    "indexer_guild_members",
    "indexer_attestations",
    "indexer_integrator_bindings",
    "indexer_integrator_data_instances",
    "indexer_integrator_recognitions",
    "indexer_integrator_schema_mappings",
    "indexer_integrator_schemas",
    "indexer_identity_passkeys",
    "indexer_identity_signing_keys",
    "indexer_identity_signing_key_revocations",
    "indexer_identity_passkey_revocations",
    "indexer_issuers",
    "indexer_issuer_keys",
    "indexer_issuer_key_revocations",
    "identity_chain_events",
    "identity_chain_state",
];

/// How a rebuild went: events applied (including deliberate no-ops) and events refused or whose
/// dependencies never arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildOutcome {
    pub applied: usize,
    pub refused: usize,
}

#[derive(Clone)]
pub struct PostgresIndexer {
    pool: PgPool,
    local_origin: Option<EventOrigin>,
    heads: Option<Arc<dyn HeadAttestor>>,
}

impl PostgresIndexer {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            local_origin: None,
            heads: None,
        }
    }

    /// The cosigned-head source set by [`Self::with_head_attestor`], if any.
    pub fn head_attestor(&self) -> Option<Arc<dyn HeadAttestor>> {
        self.heads.clone()
    }

    /// Sets where cosigned tree heads are read from, which `identity.recovered` needs to prove
    /// its delay. Without it every recovery is refused.
    pub fn with_head_attestor(mut self, heads: Arc<dyn HeadAttestor>) -> Self {
        self.heads = Some(heads);
        self
    }

    /// Sets the network and shard of events this node authors itself, which
    /// [`Self::apply_in_tx`] and [`Indexer::apply`] verify against. Without it
    /// those entry points refuse every event that needs an origin.
    pub fn with_local_origin(
        mut self,
        network_id: impl Into<String>,
        shard_id: impl Into<String>,
    ) -> Self {
        self.local_origin = Some(EventOrigin::local(network_id, shard_id));
        self
    }

    /// Drops and rebuilds every projection table from `events` alone —
    /// a disaster-recovery proof that Postgres really is just a
    /// projection, not a second source of truth. `events` must
    /// already be in ledger `seq` order (the caller — `avalon
    /// rebuild-index`, or a test driving this directly — is the one
    /// reading `ledger_entries`, so it owns that ordering).
    ///
    /// The truncate and every event's replay all happen inside **one**
    /// transaction: a rebuild is all-or-nothing. A partial rebuild would be
    /// worse than no rebuild at all — it would silently leave a live
    /// database missing whatever hadn't been replayed yet when a later
    /// event failed to decode/apply, rather than leaving the pre-rebuild
    /// state untouched for an operator to investigate. Returns how many
    /// events were applied or refused (an event whose kind no projection
    /// recognizes still counts — [`PostgresIndexer::apply_in_tx`] treats
    /// that as a deliberate no-op, not a skip worth distinguishing here).
    pub async fn rebuild_from_scratch(
        &self,
        events: &[ProtocolEvent],
    ) -> Result<RebuildOutcome, IndexError> {
        let mut tx = self.pool.begin().await?;
        for table in PROJECTION_TABLES {
            // `table` always comes from the fixed `PROJECTION_TABLES`
            // constant above, never external input — `AssertSqlSafe` is
            // sqlx 0.9's opt-in for a dynamic-but-not-attacker-controlled
            // query string.
            sqlx::query(sqlx::AssertSqlSafe(format!("TRUNCATE TABLE {table}")))
                .execute(&mut *tx)
                .await?;
        }

        // An event waiting on state later in the history is retried until a pass makes no progress.
        let mut pending: Vec<&ProtocolEvent> = events.iter().collect();
        let mut refused = 0usize;
        loop {
            let mut deferred = Vec::new();
            for event in &pending {
                // Each event applies in its own savepoint so a refused one rolls back completely.
                let mut savepoint = sqlx::Acquire::begin(&mut *tx).await?;
                match self
                    .apply_verified(&mut savepoint, event, self.local_origin.as_ref())
                    .await
                {
                    Ok(()) => savepoint.commit().await?,
                    Err(IndexError::Deferred(_) | IndexError::AwaitingKey(_)) => {
                        savepoint.rollback().await?;
                        deferred.push(*event);
                    }
                    // Refused by a validity rule, not a storage fault: the same event is refused
                    // on every replay, so it must not abort the whole rebuild.
                    Err(
                        IndexError::DisplayNameNotPermitted
                        | IndexError::DisplayNameTaken
                        | IndexError::Rejected(_),
                    ) => {
                        eprintln!(
                            "indexer: skipping refused event {} ({})",
                            event.id, event.kind
                        );
                        savepoint.rollback().await?;
                        refused += 1;
                    }
                    Err(other) => return Err(other),
                }
            }
            let progressed = deferred.len() < pending.len();
            pending = deferred;
            if pending.is_empty() || !progressed {
                break;
            }
        }
        for event in &pending {
            eprintln!(
                "indexer: skipping event {} ({}) whose dependencies never arrived",
                event.id, event.kind
            );
        }
        refused += pending.len();

        tx.commit().await?;
        Ok(RebuildOutcome {
            applied: events.len() - refused,
            refused,
        })
    }

    /// Applies an event this node authored itself, verified against the origin set by
    /// [`Self::with_local_origin`]. See [`Self::apply_in_tx_from`].
    pub async fn apply_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
    ) -> Result<(), IndexError> {
        self.apply_verified(tx, event, self.local_origin.as_ref())
            .await
    }

    /// Applies an event read from `origin`. The event is verified first
    /// ([`identity_proof::verify`]) and applied in a savepoint, so a refused or failed event
    /// leaves nothing behind: no dedup claim, identity row, profile, key or chain record.
    pub async fn apply_in_tx_from(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
        origin: &EventOrigin,
    ) -> Result<(), IndexError> {
        self.apply_verified(tx, event, Some(origin)).await
    }

    async fn apply_verified(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
        origin: Option<&EventOrigin>,
    ) -> Result<(), IndexError> {
        let mut savepoint = sqlx::Acquire::begin(&mut **tx).await?;
        match self.dispatch(&mut savepoint, event, origin).await {
            Ok(()) => {
                savepoint.commit().await?;
                Ok(())
            }
            Err(err) => {
                savepoint.rollback().await?;
                Err(err)
            }
        }
    }

    /// The real dispatch. Marks `event.id` as applied first (inside `tx`,
    /// so the claim and the projection write commit together); if it was
    /// already claimed — a redelivery, or a rebuild replaying history
    /// that's already reflected here — returns `Ok(())` without touching
    /// any projection table a second time.
    async fn dispatch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
        origin: Option<&EventOrigin>,
    ) -> Result<(), IndexError> {
        // A chained event is claimed by its chain hash, so a reused client-chosen id cannot pre-claim it;
        // any other event is claimed per (event id, shard).
        let shard = origin.map_or("", |o| o.shard_id.as_str());
        let claim_key = match event
            .identity_chain
            .as_ref()
            .and_then(|_| avalon_protocol::identity_chain_wire::event_hash(event).ok())
        {
            Some(hash) => format!("chain:{}", hex::encode(hash)),
            None => format!("{}:{shard}", event.id),
        };
        let claimed = sqlx::query(
            "INSERT INTO indexer_applied_events (event_id, shard_id, claim_key) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING RETURNING event_id",
        )
        .bind(event.id)
        .bind(shard)
        .bind(&claim_key)
        .fetch_optional(&mut **tx)
        .await?;

        if claimed.is_none() {
            return Ok(());
        }

        // Proof comes before the identity chain: a refused event must not reach it either.
        let verified = identity_proof::verify(tx, event, origin, self.heads.as_deref()).await?;

        // Chained events are recorded and resolved first; what they change is projected through the
        // resolved chain, so an event outside it changes nothing (see `reconcile`).
        match identity_chain_store::record(
            tx,
            self.heads.as_deref(),
            event,
            OffsetDateTime::now_utc(),
        )
        .await?
        {
            Recorded::Unchained => {}
            Recorded::Duplicate => return Ok(()),
            Recorded::Rejected(err) => {
                eprintln!("indexer: dropping chained event {}: {err:?}", event.id);
                return Ok(());
            }
            Recorded::Chained { .. } => {
                if let Some(owner) = avalon_protocol::identity_chain_wire::chain_owner(event) {
                    self.reconcile(tx, owner).await?;
                }
                return Ok(());
            }
        }

        match event.kind.as_str() {
            "identity.created" => {
                if let (Verified::Creation(created), Some(origin)) = (&verified, origin) {
                    profiles::apply_created(tx, event.timestamp, created, origin).await?;
                }
            }
            "profile.updated" => {
                if let Some(write) = profiles::decode(event) {
                    profiles::apply(tx, &write).await?;
                }
            }
            "identity.passkey_registered" | "identity.passkey_revoked" => {
                if let Some(write) = identity_passkeys::decode(event) {
                    identity_passkeys::apply(tx, &write).await?;
                }
            }
            "identity.recovered" => {
                if let Some(owner) = avalon_protocol::identity_chain_wire::chain_owner(event) {
                    identity_signing_keys::apply_recovered(tx, event, owner).await?;
                }
            }
            "identity.signing_key_added" | "identity.signing_key_revoked" => {
                if let Some(write) = identity_signing_keys::decode(event) {
                    identity_signing_keys::apply(tx, &write).await?;
                }
            }
            "friend.requested"
            | "friend.accepted"
            | "friend.removed"
            | "friend.relationship_reversed" => {
                if let Some(write) = friendships::decode(event) {
                    friendships::apply(tx, &write).await?;
                }
            }
            "guild.created"
            | "guild.member_added"
            | "guild.member_removed"
            | "guild.membership_reversed"
            | "guild.role_changed" => {
                if let Some(write) = guild_rosters::decode(event) {
                    guild_rosters::apply(tx, &write).await?;
                }
            }
            "game.registered" | "issuer.key_added" | "issuer.key_revoked" => {
                if let (Some(write), Some(origin)) = (issuer_keys::decode(event), origin) {
                    issuer_keys::apply(tx, origin, &write).await?;
                }
            }
            "achievement.issued" => {
                if let Some(write) = attestations::decode(event) {
                    attestations::apply(tx, &write, origin).await?;
                }
            }
            "achievement.revoked" => {
                attestation_proof::verify_revocation(tx, event, origin).await?;
                if let Some(write) = attestations::decode(event) {
                    attestations::apply(tx, &write, origin).await?;
                }
            }
            "milestone.revoked" => {
                attestation_proof::verify_revocation(tx, event, origin).await?;
            }
            "game_schema.published" => {
                if let Some(write) = integrator_schemas::decode(event) {
                    integrator_schemas::apply(tx, &write).await?;
                }
            }
            "game_schema_mapping.published" => {
                if let Some(write) = integrator_schema_mappings::decode(event) {
                    integrator_schema_mappings::apply(tx, &write).await?;
                }
            }
            "game.binding_established" | "game.binding_ended" => {
                if let Some(write) = integrator_bindings::decode(event) {
                    integrator_bindings::apply(tx, &write).await?;
                }
            }
            "game_data.published" => {
                if let Some(write) = integrator_data_instances::decode(event) {
                    integrator_data_instances::apply(tx, &write).await?;
                }
            }
            "game_data.deleted" => {
                if let Some(write) = integrator_data_instances::decode_deletion(event) {
                    integrator_data_instances::apply_deletion(tx, &write).await?;
                }
            }
            "integrator.recognition_published" | "integrator.recognition_revoked" => {
                if let Some(write) = integrator_recognitions::decode(event) {
                    integrator_recognitions::apply(tx, &write).await?;
                }
            }

            // #669: known, current event kinds whose data already lives in
            // its own direct source-of-truth table, written synchronously
            // by the handler that emits the event (in the same transaction
            // as its `outbox::enqueue` call) — never through the indexer.
            // Nothing to project here, so these are a deliberate no-op
            // rather than falling into the `other` branch below, which
            // would log them as unrecognized on every rebuild even though
            // nothing is actually missing. See
            // `avalon-docs/architecture/query-and-indexing.md`'s
            // "Events with their own source of truth" section for the full
            // table-by-kind mapping and why each one is scoped this way.
            "guild.channel_created"
            | "permission.granted"
            | "permission.revoked"
            | "achievement.defined"
            | "achievement.definition_updated"
            | "achievement.definition_retired"
            | "milestone.defined"
            | "milestone.definition_updated"
            | "milestone.definition_retired"
            | "milestone.issued"
            // #678: the other half of #669's sweep, verified the same way —
            // each writes its own direct table synchronously, never through
            // the indexer. See query-and-indexing.md's table for specifics.
            | "identity.recovery_configured"
            | "identity.recovery_requested"
            | "identity.recovery_approved"
            | "identity.recovery_cancelled"
            | "issuer.registered"
            | "guild.updated"
            | "guild.role_defined"
            | "guild.role_deleted"
            | "guild.owner_transferred"
            | "guild.game_associated"
            | "guild.favorite_games_updated"
            | "guild.channel_renamed"
            | "guild.channel_archived" => {}

            other => {
                // Never an error — an old indexer must survive a new event
                // kind being introduced elsewhere in the protocol. See
                // avalon-docs/architecture/query-and-indexing.md.
                eprintln!("indexer: skipping unrecognized event kind {other:?}");
            }
        }

        Ok(())
    }
}

/// The projections a chained event's effect is tracked in as a group: undoing one member undoes all of
/// them and the accepted ones are applied again in chain order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Keys,
    Profile,
    Social,
}

fn group_of(kind: &str) -> Option<Group> {
    match kind {
        "identity.recovered"
        | "identity.signing_key_added"
        | "identity.signing_key_revoked"
        | "identity.passkey_registered"
        | "identity.passkey_revoked" => Some(Group::Keys),
        "profile.updated" => Some(Group::Profile),
        "friend.requested"
        | "friend.accepted"
        | "friend.removed"
        | "friend.relationship_reversed"
        | "guild.member_added"
        | "guild.member_removed"
        | "guild.membership_reversed" => Some(Group::Social),
        _ => None,
    }
}

async fn project_chained(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
) -> Result<(), IndexError> {
    let owner = avalon_protocol::identity_chain_wire::chain_owner(event);
    match event.kind.as_str() {
        "profile.updated" => {
            if let Some(write) = profiles::decode(event) {
                profiles::apply(tx, &write).await?;
            }
        }
        "identity.passkey_registered" | "identity.passkey_revoked" => {
            if let Some(write) = identity_passkeys::decode(event) {
                identity_passkeys::apply(tx, &write).await?;
            }
        }
        "identity.recovered" => {
            if let Some(owner) = owner {
                identity_signing_keys::apply_recovered(tx, event, owner).await?;
            }
        }
        "identity.signing_key_added" | "identity.signing_key_revoked" => {
            if let Some(write) = identity_signing_keys::decode(event) {
                identity_signing_keys::apply(tx, &write).await?;
            }
        }
        "friend.requested"
        | "friend.accepted"
        | "friend.removed"
        | "friend.relationship_reversed" => {
            if let Some(write) = friendships::decode(event) {
                friendships::apply(tx, &write).await?;
            }
        }
        "guild.member_added" | "guild.member_removed" | "guild.membership_reversed" => {
            if let Some(write) = guild_rosters::decode(event) {
                guild_rosters::apply(tx, &write).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn revert_chained(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
) -> Result<(), IndexError> {
    let owner = avalon_protocol::identity_chain_wire::chain_owner(event);
    match event.kind.as_str() {
        "profile.updated" => {
            if let Some(write) = profiles::decode(event) {
                profiles::apply(tx, &profiles::revert_of(&write)).await?;
            }
        }
        "identity.passkey_registered" | "identity.passkey_revoked" => {
            identity_passkeys::revert(tx, event).await?
        }
        "identity.recovered" => {
            if let Some(owner) = owner {
                identity_signing_keys::revert_recovered(tx, event, owner).await?;
            }
        }
        "identity.signing_key_added" | "identity.signing_key_revoked" => {
            if let Some(owner) = owner {
                identity_signing_keys::revert_key_event(tx, event, owner).await?;
            }
        }
        // Friendship and roster rows have no inverse of their own; they follow later events.
        _ => {}
    }
    Ok(())
}

fn seq_of(event: &ProtocolEvent) -> u64 {
    event.identity_chain.as_ref().map_or(0, |p| p.seq)
}

impl PostgresIndexer {
    /// Makes what is projected for the identity's chained events match the chain it resolves now:
    /// events the chain no longer holds are undone, events it holds are applied. A change among key,
    /// passkey or profile events undoes every projected one of that group and applies the accepted
    /// ones again in chain order, so the rows are a function of the accepted set alone.
    async fn reconcile(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        owner: avalon_protocol::identity_id::IdentityId,
    ) -> Result<(), IndexError> {
        let view = identity_chain_store::reconcile_view(tx, self.heads.as_deref(), owner).await?;
        let fresh_hashes: std::collections::BTreeSet<_> = view
            .fresh
            .iter()
            .filter_map(|e| avalon_protocol::identity_chain_wire::event_hash(e).ok())
            .collect();
        let is_fresh = |e: &ProtocolEvent| {
            avalon_protocol::identity_chain_wire::event_hash(e)
                .is_ok_and(|h| fresh_hashes.contains(&h))
        };
        for group in [Group::Keys, Group::Profile] {
            let in_group = |e: &&ProtocolEvent| group_of(&e.kind) == Some(group);
            let stale: Vec<&ProtocolEvent> = view.stale.iter().filter(in_group).collect();
            let fresh: Vec<&ProtocolEvent> = view.fresh.iter().filter(in_group).collect();
            if stale.is_empty() && fresh.is_empty() {
                continue;
            }
            let kept: Vec<&ProtocolEvent> = view
                .accepted
                .iter()
                .filter(in_group)
                .filter(|e| !is_fresh(e))
                .collect();
            let last_kept = kept.iter().map(|e| seq_of(e)).max().unwrap_or(0);
            let appended = stale.is_empty() && fresh.iter().all(|e| seq_of(e) > last_kept);
            if appended {
                for event in fresh {
                    project_chained(tx, event).await?;
                }
                continue;
            }
            let mut undo: Vec<&ProtocolEvent> =
                stale.iter().copied().chain(kept.iter().copied()).collect();
            undo.sort_by_key(|e| std::cmp::Reverse(seq_of(e)));
            for event in undo {
                revert_chained(tx, event).await?;
            }
            for event in view.accepted.iter().filter(in_group) {
                project_chained(tx, event).await?;
            }
        }
        for event in view
            .fresh
            .iter()
            .filter(|e| group_of(&e.kind) == Some(Group::Social))
        {
            project_chained(tx, event).await?;
        }
        identity_chain_store::mark_projected(tx, owner, &view.stale, false).await?;
        identity_chain_store::mark_projected(tx, owner, &view.accepted, true).await?;
        Ok(())
    }

    /// Re-resolves up to `limit` identities whose chain depends on cosigned head dates, so projections
    /// follow dates that arrive after the events themselves. Returns how many were examined.
    pub async fn reconcile_anchor_dependent(&self, limit: usize) -> Result<usize, IndexError> {
        let mut tx = self.pool.begin().await?;
        let owners =
            identity_chain_store::anchor_dependent_identities(&mut tx, limit as i64).await?;
        tx.commit().await?;
        for owner in &owners {
            let mut tx = self.pool.begin().await?;
            self.reconcile(&mut tx, *owner).await?;
            tx.commit().await?;
        }
        Ok(owners.len())
    }
}

#[async_trait]
impl Indexer for PostgresIndexer {
    async fn apply(&self, event: &ProtocolEvent) -> Result<(), IndexError> {
        let mut tx = self.pool.begin().await?;
        self.apply_in_tx(&mut tx, event).await?;
        tx.commit().await?;
        Ok(())
    }
}
