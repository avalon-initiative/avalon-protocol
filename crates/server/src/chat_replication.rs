//! Async at-rest replication of guild chat / conversation history to at
//! least one additional node, meeting a "replicated to ≥1 additional node"
//! standard. Separate concern from
//! `crate::realtime_relay`: that module is about *live* delivery
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
//! `avalon-docs/architecture/nodes/README.md`'s capability table — distinct from
//! `realtime_relay`'s `realtime`/`gateway` eligibility, a different
//! concern) — deterministic, so which peer is "the" replica target is
//! never ambiguous or flapping between ticks.
//!
//! **Observable, never silently swallowed.** Every attempt logs its
//! outcome and elapsed time — a `tracing::warn` on failure/unreachable
//! peer, `tracing::debug` on success — so replication lag or a stuck
//! target is something an operator can actually notice, not a design
//! promise with no way to check it's holding.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::conversations;
use crate::guild_messages;
use crate::node_auth::{AuthenticatedNode, Budget};
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
    replication_target_from(state.peers.list_all())
}

/// A `p2p://` entry's self-reported roles prove nothing, so it is never a target.
fn replication_target_from(peers: Vec<crate::nodes::PeerInfo>) -> Option<String> {
    peers
        .into_iter()
        .filter(|peer| !crate::nodes::is_p2p_url(&peer.base_url))
        .filter(|peer| advertises_storage_role(&peer.roles))
        .min_by(|a, b| a.base_url.cmp(&b.base_url))
        .map(|peer| crate::node_http::NodeClient::url_for(&peer))
}

fn replication_client() -> &'static crate::node_http::NodeClient {
    static CLIENT: OnceLock<crate::node_http::NodeClient> = OnceLock::new();
    CLIENT.get_or_init(crate::node_http::NodeClient::new)
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

/// Replica requests one signer may make a minute, unless
/// `AVALON_NODE_AUTH_REPLICATE_PER_MINUTE` says otherwise.
pub const DEFAULT_REPLICATE_PER_MINUTE: u32 = 600;

/// How far past this node's clock a replicated message's sent time may be.
const MAX_SENT_AT_FUTURE_SECS: i64 = 300;

/// Earliest sent time accepted (2024-01-01T00:00:00Z); nothing older can be a real message.
const MIN_SENT_AT_UNIX: i64 = 1_704_067_200;

/// Most signers tracked by the replica rate limit; only peers with standing reach it.
const MAX_RATE_KEYS: usize = 16_384;

/// Whether this node stores replicas, and the per-signer rate limit on writing them.
#[derive(Clone)]
pub struct ReplicaIntake {
    stores_chat: bool,
    budget: Arc<Mutex<Budget<PeerId>>>,
}

impl ReplicaIntake {
    /// Accepts replicas only when `roles` include a storage role.
    pub fn new(roles: &[String]) -> Self {
        let per_minute = std::env::var("AVALON_NODE_AUTH_REPLICATE_PER_MINUTE")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_REPLICATE_PER_MINUTE);
        Self::with_rate(roles, per_minute)
    }

    pub fn with_rate(roles: &[String], per_minute: u32) -> Self {
        Self {
            stores_chat: crate::nodes::indexer_role_is_local(roles),
            budget: Arc::new(Mutex::new(Budget::new(per_minute, MAX_RATE_KEYS, false))),
        }
    }

    fn admit(&self, signer: PeerId, now: i64) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .charge(signer, now)
    }
}

/// Why a replicated event was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaRefusal {
    BodyTooLong,
    SentAtImplausible,
}

fn body_fits(body: &str) -> bool {
    body.chars().count() <= guild_messages::MESSAGE_BODY_MAX_CHARS
}

fn sent_at_plausible(sent_at: OffsetDateTime, now: OffsetDateTime) -> bool {
    let at = sent_at.unix_timestamp();
    at >= MIN_SENT_AT_UNIX && at <= now.unix_timestamp().saturating_add(MAX_SENT_AT_FUTURE_SECS)
}

/// Checks the size and sent time of a replicated message; deletes carry neither.
pub fn validate_event(event: &ReplicationEvent, now: OffsetDateTime) -> Result<(), ReplicaRefusal> {
    let (body, sent_at) = match event {
        ReplicationEvent::ChannelMessage(m) => (&m.body, m.sent_at),
        ReplicationEvent::ConversationMessage(m) => (&m.body, m.sent_at),
        ReplicationEvent::ChannelMessageDeleted { .. } => return Ok(()),
    };
    if !body_fits(body) {
        return Err(ReplicaRefusal::BodyTooLong);
    }
    if !sent_at_plausible(sent_at, now) {
        return Err(ReplicaRefusal::SentAtImplausible);
    }
    Ok(())
}

/// What storing an event did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    Stored,
    /// A delete matched no row inserted by this signer.
    NotOwned,
}

/// Writes `event` to the replica tables on behalf of `signer`. An insert never overwrites an
/// existing row, and a delete applies only to a row `signer` inserted.
pub async fn store_event(
    pool: &sqlx::PgPool,
    signer: &str,
    event: &ReplicationEvent,
) -> Result<StoreOutcome, sqlx::Error> {
    match event {
        ReplicationEvent::ChannelMessage(message) => {
            sqlx::query(
                "INSERT INTO guild_messages_replica \
                 (id, channel_id, author, body, sent_at, replicated_by) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(message.id)
            .bind(message.channel_id)
            .bind(message.author)
            .bind(&message.body)
            .bind(message.sent_at)
            .bind(signer)
            .execute(pool)
            .await?;
        }
        ReplicationEvent::ChannelMessageDeleted {
            channel_id,
            message_id,
        } => {
            let done = sqlx::query(
                "UPDATE guild_messages_replica SET deleted_at = COALESCE(deleted_at, $4) \
                 WHERE id = $1 AND channel_id = $2 AND replicated_by = $3",
            )
            .bind(message_id)
            .bind(channel_id)
            .bind(signer)
            .bind(OffsetDateTime::now_utc())
            .execute(pool)
            .await?;
            if done.rows_affected() == 0 {
                return Ok(StoreOutcome::NotOwned);
            }
        }
        ReplicationEvent::ConversationMessage(message) => {
            sqlx::query(
                "INSERT INTO conversation_messages_replica \
                 (id, conversation_id, author, body, sent_at, replicated_by) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(message.id)
            .bind(message.conversation_id)
            .bind(message.author)
            .bind(&message.body)
            .bind(message.sent_at)
            .bind(signer)
            .execute(pool)
            .await?;
        }
    }
    Ok(StoreOutcome::Stored)
}

/// `POST /nodes/replicate-chat` — the receiving end. Accepted only on a node with a storage
/// role, within the signer's replica rate, for a message of bounded size and a sane sent time.
/// Rows go to the foreign-key-free replica tables (never the live ones), tagged with the
/// authenticated signer: a repeated insert is a no-op (first writer wins) and a delete applies
/// only to a row that signer inserted (404 otherwise). Rows from before the signer column, and
/// rows of a signer that has since changed its key, can therefore never be deleted.
pub async fn replicate_chat_handler(
    State(state): State<AppState>,
    AuthenticatedNode(signer): AuthenticatedNode,
    Json(event): Json<ReplicationEvent>,
) -> Result<StatusCode, StatusCode> {
    let now = OffsetDateTime::now_utc();
    if !state.replica_intake.stores_chat {
        return Err(StatusCode::FORBIDDEN);
    }
    if !state.replica_intake.admit(signer, now.unix_timestamp()) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    validate_event(&event, now).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    match store_event(&state.pool, &signer.to_string(), &event).await {
        Ok(StoreOutcome::Stored) => Ok(StatusCode::NO_CONTENT),
        Ok(StoreOutcome::NotOwned) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
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
    #[test]
    fn a_p2p_entry_claiming_a_storage_role_is_never_the_replication_target() {
        let id = libp2p::PeerId::random();
        let entry = |url: String| crate::nodes::PeerInfo {
            base_url: url,
            roles: vec!["combined".into()],
            protocol_version: "0.1.0".into(),
            network_id: "n".into(),
            last_announced_at: time::OffsetDateTime::now_utc(),
            libp2p_peer_id: None,
            libp2p_listen_addrs: vec![],
            witness: None,
            connectivity: None,
            identity_bound: false,
        };
        // "http://z" sorts after "p2p://": the p2p entry would win without the filter.
        let p2p = entry(crate::node_http::p2p_base_url(&id));
        assert_eq!(replication_target_from(vec![p2p.clone()]), None);
        assert_eq!(
            replication_target_from(vec![p2p, entry("http://z.test".into())]),
            Some("http://z.test".to_string())
        );
    }

    fn channel_message(body: &str, sent_at: OffsetDateTime) -> ReplicationEvent {
        ReplicationEvent::ChannelMessage(guild_messages::MessageResponse {
            id: Uuid::new_v4(),
            channel_id: Uuid::new_v4(),
            author: Uuid::new_v4(),
            body: body.to_string(),
            sent_at,
        })
    }

    #[test]
    fn a_message_must_fit_the_send_limit_and_carry_a_sane_sent_time() {
        let now = OffsetDateTime::now_utc();
        let at = |secs: i64| now + time::Duration::seconds(secs);
        assert_eq!(validate_event(&channel_message("hi", at(0)), now), Ok(()));
        assert_eq!(
            validate_event(&channel_message("hi", at(MAX_SENT_AT_FUTURE_SECS)), now),
            Ok(())
        );
        let long = "a".repeat(guild_messages::MESSAGE_BODY_MAX_CHARS + 1);
        assert_eq!(
            validate_event(&channel_message(&long, at(0)), now),
            Err(ReplicaRefusal::BodyTooLong)
        );
        assert_eq!(
            validate_event(&channel_message("hi", at(MAX_SENT_AT_FUTURE_SECS + 5)), now),
            Err(ReplicaRefusal::SentAtImplausible)
        );
        let ancient = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            validate_event(&channel_message("hi", ancient), now),
            Err(ReplicaRefusal::SentAtImplausible)
        );
        let delete = ReplicationEvent::ChannelMessageDeleted {
            channel_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
        };
        assert_eq!(validate_event(&delete, now), Ok(()));
    }

    #[test]
    fn only_storage_roles_store_replicas_and_each_signer_has_its_own_rate() {
        for (roles, stores) in [
            ("indexer", true),
            ("combined", true),
            ("realtime", false),
            ("gateway", false),
        ] {
            assert_eq!(
                ReplicaIntake::with_rate(&[roles.to_string()], 1).stores_chat,
                stores,
                "{roles}"
            );
        }
        let intake = ReplicaIntake::with_rate(&["combined".to_string()], 2);
        let (a, b) = (PeerId::random(), PeerId::random());
        assert!(intake.admit(a, 60) && intake.admit(a, 61));
        assert!(!intake.admit(a, 62));
        assert!(intake.admit(b, 62));
        assert!(intake.admit(a, 120));
    }
}
