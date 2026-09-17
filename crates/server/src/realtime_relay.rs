//! One-hop live realtime event relay across nodes (issue #539, implementing
//! #535's decided design), built on the #362 peer table
//! (`crate::nodes::PeerTable`).
//!
//! Today's realtime fan-out (`crate::presence::PresenceStore`,
//! `crate::chat::ChatBus`) is purely in-process: a `tokio::broadcast`
//! channel only reaches websocket connections held open on *this* process.
//! Once more than one Gateway/Realtime node exists for a network, two
//! guildmates connected to two different nodes previously couldn't see
//! each other's presence/chat at all — nothing had failed, the event just
//! never left the node it originated on. This module closes that gap:
//! whichever handler locally originates a realtime event
//! (`presence::update_my_presence`/`update_integrator_presence`,
//! `guild_messages::send_message`/`delete_message`,
//! `conversations::send_message`) also calls [`relay_to_peers`], which
//! posts the event once to every same-`network_id` peer this node's own
//! `PeerTable` currently knows about that advertises a `realtime`,
//! `gateway`, or `combined` role.
//!
//! **Single-hop by construction, not by an origin-tracking field.**
//! [`relay_handler`] — the `POST /nodes/relay` receiver — applies an
//! incoming relayed event to this node's own local store/broadcast
//! (`PresenceStore::apply_relayed`, `ChatBus::publish_*`) and never itself
//! calls [`relay_to_peers`] again. There is nothing structurally capable of
//! producing a second hop, so no origin-node bookkeeping is needed to
//! prevent one — today's peer mesh is small and fully interconnected, so
//! one hop already reaches everyone (see #542 for what changes once that
//! stops being true at real scale).
//!
//! **Never touches Postgres.** A relayed chat message only feeds this
//! node's `ChatBus` (live push to already-connected clients) — it is
//! never written into `guild_messages`/`conversation_messages` here.
//! Async at-rest replication of chat history to additional nodes is a
//! separate, explicitly scoped-out concern (#540). A relayed presence
//! update *does* update `PresenceStore`'s in-memory map (presence has no
//! Postgres-backed source of truth to defer to — see ADR #78 — so the
//! in-memory map *is* the state a relayed update needs to reach).
//!
//! **Best-effort, fire-and-forget.** Relaying is a liveness/reach
//! improvement, never a durability guarantee — a peer that's down or
//! unreachable simply doesn't get this tick's update, the same "a slow or
//! absent subscriber just misses it" tradeoff `PresenceStore`/`ChatBus`
//! already accept locally. [`relay_to_peers`] is spawned rather than
//! awaited inline by its callers, so a slow/unreachable peer never delays
//! the HTTP response to whoever actually published the event.

use std::sync::OnceLock;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::conversations;
use crate::guild_messages;
use crate::presence::PresenceResponse;
use crate::state::AppState;

/// One realtime event, exactly as it needs to reach a peer's local
/// fan-out — no envelope beyond the tag, since single-hop relay needs no
/// origin/sequence bookkeeping (see module doc comment).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RelayEvent {
    Presence(PresenceResponse),
    ChannelMessage(guild_messages::MessageResponse),
    ChannelMessageDeleted { channel_id: Uuid, message_id: Uuid },
    ConversationMessage(conversations::MessageResponse),
}

/// Role strings (case-insensitive) that make a peer an eligible relay
/// target — matching `docs/architecture/nodes.md`'s `Realtime`/`Gateway`
/// capability names, plus `combined` (milestone 1's default —
/// `nodes::node_roles`'s own fallback), which implies every capability
/// including these two.
fn advertises_realtime_relay_role(roles: &[String]) -> bool {
    roles.iter().any(|role| {
        let role = role.to_ascii_lowercase();
        role == "realtime" || role == "gateway" || role == "combined"
    })
}

fn relay_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Posts `event` once to every same-`network_id` peer this node's own
/// peer table currently knows about that advertises a realtime-capable
/// role — a no-op (network name never resolves, nothing sent) when this
/// node has no peers, matching #539's "a single-node deployment behaves
/// exactly as today" invariant. Takes `state` by value (a cheap `Arc`-backed
/// clone, same as every request handler already gets via axum's `State`
/// extractor) rather than by reference, so callers can `tokio::spawn` this
/// directly instead of awaiting it inline — see module doc comment.
pub async fn relay_to_peers(state: AppState, event: RelayEvent) {
    let peers = state.peers.list_all();
    if peers.is_empty() {
        return;
    }
    let client = relay_client();
    for peer in peers {
        if !advertises_realtime_relay_role(&peer.roles) {
            continue;
        }
        let url = format!("{}/nodes/relay", peer.base_url);
        let event = event.clone();
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(err) = client.post(&url).json(&event).send().await {
                // Best-effort: an unreachable peer just misses this tick's
                // event, same tradeoff a lagging local broadcast receiver
                // already accepts. Logged, not retried — a dropped relay
                // is a liveness gap, never a correctness one (nothing here
                // is durable protocol history).
                tracing::debug!(peer = %url, error = %err, "realtime relay: peer unreachable");
            }
        });
    }
}

/// `POST /nodes/relay` — issue #539's receiving end. Applies `event` to
/// this node's own local store/broadcast only; never relays it onward
/// (see module doc comment for why that alone is sufficient to guarantee
/// single-hop delivery). No auth: same posture `GET /nodes/peers` already
/// takes for node-to-node discovery traffic — a relayed presence/chat
/// event is exactly as sensitive as the live broadcast it feeds (already
/// unauthenticated once inside `PresenceStore`/`ChatBus`), not a new
/// privileged write.
pub async fn relay_handler(
    State(state): State<AppState>,
    Json(event): Json<RelayEvent>,
) -> StatusCode {
    match event {
        RelayEvent::Presence(presence) => {
            state.presence.apply_relayed(
                presence.identity_id,
                presence.status,
                presence.active_in,
                presence.updated_at,
            );
        }
        RelayEvent::ChannelMessage(message) => {
            state.chat.publish_channel_message(message);
        }
        RelayEvent::ChannelMessageDeleted {
            channel_id,
            message_id,
        } => {
            state
                .chat
                .publish_channel_message_deleted(channel_id, message_id);
        }
        RelayEvent::ConversationMessage(message) => {
            state.chat.publish_conversation_message(message);
        }
    }
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_role_is_eligible() {
        assert!(advertises_realtime_relay_role(&["combined".to_string()]));
    }

    #[test]
    fn realtime_and_gateway_roles_are_eligible_case_insensitively() {
        assert!(advertises_realtime_relay_role(&["Realtime".to_string()]));
        assert!(advertises_realtime_relay_role(&["GATEWAY".to_string()]));
    }

    #[test]
    fn settlement_only_role_is_not_eligible() {
        assert!(!advertises_realtime_relay_role(&["settlement".to_string()]));
    }

    #[test]
    fn empty_roles_is_not_eligible() {
        assert!(!advertises_realtime_relay_role(&[]));
    }
}
