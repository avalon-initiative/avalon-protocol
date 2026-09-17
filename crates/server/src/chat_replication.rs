//! Async at-rest replication of guild chat / conversation history to at
//! least one additional node (issue #540, implementing #535's decided
//! "replicated to ≥1 additional node" standard). Separate concern from
//! `crate::realtime_relay` (#539): that module is about *live* delivery
//! to an already-connected subscriber and never touches Postgres; this
//! module is about *durability* — surviving the originating node's loss —
//! and never touches a live broadcast channel.
//!
//! **Why a separate, foreign-key-free replica table, not the real
//! `guild_messages`/`conversation_messages` tables.** Those tables'
//! `channel_id`/`author`/`conversation_id` columns are foreign keys into
//! `guild_channels`/`identities`/`conversations` — core protocol data a
//! node only holds a copy of if it authored it directly (today's
//! architecture has no mechanism replicating those specific tables to a
//! node that isn't also handling live writes for them — #506 retargeted
//! *membership rosters* through the indexer, not these). A disaster-
//! recovery copy that can fail to insert because of an unrelated
//! referential-integrity gap on the replica is useless, so
//! `guild_messages_replica`/`conversation_messages_replica` (migration
//! `0068`) intentionally carry no foreign keys — they exist purely to be
//! read back after the originating node is gone, never to be joined
//! against or served through the normal live-read path.
//!
//! **Exactly one designated target, not a fan-out.** #535's decision only
//! requires "≥1 additional node," not every peer — replicating to every
//! realtime/gateway peer (#539's relay targets) would be write
//! amplification with no correctness benefit here. The target is the
//! lexicographically-smallest `base_url` among same-`network_id` peers
//! advertising an `indexer`/`combined` role (storage-capable roles, per
//! `docs/architecture/nodes.md`'s capability table — distinct from
//! `realtime_relay`'s `realtime`/`gateway` eligibility, a different
//! concern) — deterministic, so which peer is "the" replica target is
//! never ambiguous or flapping between ticks.
//!
//! **Observable, never silently swallowed.** Every attempt logs its
//! outcome and elapsed time — a `tracing::warn` on failure/unreachable
//! peer, `tracing::debug` on success — so replication lag or a stuck
//! target is something an operator can actually notice, not a design
//! promise with no way to check it's holding.

use std::sync::OnceLock;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::conversations;
use crate::guild_messages;
use crate::state::AppState;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ReplicationEvent {
    ChannelMessage(guild_messages::MessageResponse),
    ChannelMessageDeleted { channel_id: Uuid, message_id: Uuid },
    ConversationMessage(conversations::MessageResponse),
}

fn advertises_storage_role(roles: &[String]) -> bool {
    roles.iter().any(|role| {
        let role = role.to_ascii_lowercase();
        role == "indexer" || role == "combined"
    })
}

/// The one deterministic replication target, or `None` if this node
/// currently knows of no eligible peer — a no-op, matching #539's own
/// single-node-deployment invariant (nothing to replicate to yet is not
/// an error).
fn replication_target(state: &AppState) -> Option<String> {
    state
        .peers
        .list_all()
        .into_iter()
        .filter(|peer| advertises_storage_role(&peer.roles))
        .map(|peer| peer.base_url)
        .min()
}

fn replication_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Posts `event` to this node's one designated replication target, if it
/// has one. Spawned by callers, not awaited inline — replication lag must
/// never delay the HTTP response to whoever actually sent the message.
pub async fn replicate_to_peers(state: AppState, event: ReplicationEvent) {
    let Some(target) = replication_target(&state) else {
        return;
    };
    let url = format!("{target}/nodes/replicate-chat");
    let started = Instant::now();
    match replication_client().post(&url).json(&event).send().await {
        Ok(response) if response.status().is_success() => {
            tracing::debug!(
                target = %url,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "chat replication: delivered"
            );
        }
        Ok(response) => {
            tracing::warn!(
                target = %url,
                status = %response.status(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "chat replication: target rejected the event"
            );
        }
        Err(err) => {
            tracing::warn!(
                target = %url,
                error = %err,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "chat replication: target unreachable"
            );
        }
    }
}

/// `POST /nodes/replicate-chat` — the receiving end. Writes directly into
/// this node's own `guild_messages_replica`/`conversation_messages_replica`
/// (never the live `guild_messages`/`conversation_messages` tables — see
/// module doc comment), `ON CONFLICT (id) DO NOTHING` so a retried or
/// duplicate delivery is a harmless no-op. No auth — same posture
/// `POST /nodes/relay` already takes for node-to-node realtime traffic.
pub async fn replicate_chat_handler(
    State(state): State<AppState>,
    Json(event): Json<ReplicationEvent>,
) -> Result<StatusCode, StatusCode> {
    match event {
        ReplicationEvent::ChannelMessage(message) => {
            sqlx::query(
                "INSERT INTO guild_messages_replica (id, channel_id, author, body, sent_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(message.id)
            .bind(message.channel_id)
            .bind(message.author)
            .bind(&message.body)
            .bind(message.sent_at)
            .execute(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        ReplicationEvent::ChannelMessageDeleted {
            channel_id: _,
            message_id,
        } => {
            sqlx::query("UPDATE guild_messages_replica SET deleted_at = $2 WHERE id = $1")
                .bind(message_id)
                .bind(OffsetDateTime::now_utc())
                .execute(&state.pool)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        ReplicationEvent::ConversationMessage(message) => {
            sqlx::query(
                "INSERT INTO conversation_messages_replica \
                 (id, conversation_id, author, body, sent_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(message.id)
            .bind(message.conversation_id)
            .bind(message.author)
            .bind(&message.body)
            .bind(message.sent_at)
            .execute(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexer_and_combined_roles_are_eligible_case_insensitively() {
        assert!(advertises_storage_role(&["Indexer".to_string()]));
        assert!(advertises_storage_role(&["COMBINED".to_string()]));
    }

    #[test]
    fn realtime_only_role_is_not_eligible() {
        // Deliberately distinct from `realtime_relay`'s own eligibility —
        // a realtime-only node has no reason to hold durable chat copies.
        assert!(!advertises_storage_role(&["realtime".to_string()]));
    }

    #[test]
    fn empty_roles_is_not_eligible() {
        assert!(!advertises_storage_role(&[]));
    }
}
