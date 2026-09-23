//! One-hop live realtime event relay across nodes, implementing
//! the decided design, built on the peer table
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
//! one hop already reaches everyone.
//!
//! **DHT-scoped delivery for guild-channel/conversation events,
//! implementing the decided design.** [`relay_targets`] looks up
//! `crate::interest`'s DHT-backed registry for exactly who has a local
//! subscriber for the event's own channel/conversation id, instead of
//! posting to every realtime-capable peer in the full HTTP peer table —
//! the actual behavior change this whole epic exists for. Two
//! things still take the original full-peer-loop path unconditionally:
//! `RelayEvent::Presence` (presence has no guild/conversation scope to
//! look anything up by — see `crate::interest`'s own module doc on this),
//! and *any* event on a node with no DHT identity at all
//! (`AVALON_DHT_ENABLED` unset) — every earlier deployment's behavior,
//! unchanged, exactly the same "unconfigured node behaves as it always
//! did" posture already established. A DHT lookup against a
//! small, fully-interconnected mesh trivially resolves back to "everyone
//! who's actually subscribed," so the small-mesh case stays correct as a
//! natural degenerate case rather than needing its own special path.
//!
//! **Cross-network reachability closed**: `crate::dht`'s Kademlia
//! protocol id is now `network_id`-scoped, so a differently-networked peer
//! can no longer negotiate a Kademlia RPC with this swarm at all — this
//! module's own DHT lookups are structurally bounded to this node's own
//! network now, not merely protected by the bootstrap incidentally
//! never reaching a foreign peer. **Known limitation, not solved here:**
//! nothing yet authorizes *which scope* an
//! already-admitted, same-network node can register interest in — a
//! same-network node with no real subscriber for a channel/conversation
//! can still register interest in it and receive this relay's content for
//! it. The concrete leak needs a proposed signed-membership fix.
//!
//! **Never touches Postgres.** A relayed chat message only feeds this
//! node's `ChatBus` (live push to already-connected clients) — it is
//! never written into `guild_messages`/`conversation_messages` here.
//! Async at-rest replication of chat history to additional nodes is a
//! separate, explicitly scoped-out concern. A relayed presence
//! update *does* update `PresenceStore`'s in-memory map (presence has no
//! Postgres-backed source of truth to defer to — so the
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
use crate::interest::{self, InterestScope};
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

/// Every realtime-capable peer's base URL from #362's peer table — #539's
/// original, unscoped behavior. Still used unconditionally for
/// [`RelayEvent::Presence`] and as the fallback for every event type on a
/// node with no DHT identity — see [`relay_targets`].
fn full_peer_loop_targets(state: &AppState) -> Vec<String> {
    state
        .peers
        .list_all()
        .into_iter()
        .filter(|peer| advertises_realtime_relay_role(&peer.roles))
        .map(|peer| peer.base_url)
        .collect()
}

/// Which [`InterestScope`] `event` should be looked up by, or `None` for
/// an event with no guild-channel/conversation scope at all (presence) —
/// pure and unit-testable independent of an actual DHT lookup, same "pure
/// function behind the real (here, async) work" split
/// `crate::nodes::resolve_bootstrap_peers`/`crate::dht::new_dht_peer`
/// already establish elsewhere in this crate.
fn interest_scope_for(event: &RelayEvent) -> Option<InterestScope> {
    match event {
        RelayEvent::Presence(_) => None,
        RelayEvent::ChannelMessage(message) => Some(InterestScope::Channel(message.channel_id)),
        RelayEvent::ChannelMessageDeleted { channel_id, .. } => {
            Some(InterestScope::Channel(*channel_id))
        }
        RelayEvent::ConversationMessage(message) => {
            Some(InterestScope::Conversation(message.conversation_id))
        }
    }
}

/// Resolves who `event` should actually be posted to — a DHT
/// interest lookup scoped to `event`'s own channel/conversation id when
/// this node has a DHT identity and `event` has such a scope to look up in
/// the first place, [`full_peer_loop_targets`] otherwise. This node's own
/// `base_url` is filtered out of a DHT lookup's results: a node with a
/// local subscriber for the same scope it's relaying for would otherwise
/// see itself come back from `interest::lookup` and relay-POST to itself
/// (`full_peer_loop_targets` never has this problem — #362's peer table
/// never contains an entry for this node's own `base_url`).
async fn relay_targets(state: &AppState, event: &RelayEvent) -> Vec<String> {
    let (Some(scope), Some(dht_commands)) = (interest_scope_for(event), &state.dht_commands) else {
        return full_peer_loop_targets(state);
    };

    // Issue #610: `Channel`/`Conversation` interest is now claim-verified —
    // see `interest::lookup_claimed`'s own doc comment for why this can no
    // longer reuse the Redis fast-path `interest::lookup` still does for
    // `crate::mirror_push`'s unrelated `Network`-scope use.
    interest::lookup_claimed(state, dht_commands, scope)
        .await
        .into_iter()
        .filter(|base_url| Some(base_url) != state.own_base_url.as_ref())
        .collect()
}

/// Posts `event` once to whichever peers [`relay_targets`] resolves — a
/// no-op (nothing resolves, nothing sent) when this node has no relay
/// targets at all, matching #539's original "a single-node deployment
/// behaves exactly as today" invariant. Takes `state` by value (a cheap
/// `Arc`-backed clone, same as every request handler already gets via
/// axum's `State` extractor) rather than by reference, so callers can
/// `tokio::spawn` this directly instead of awaiting it inline — see module
/// doc comment.
pub async fn relay_to_peers(state: AppState, event: RelayEvent) {
    let targets = relay_targets(&state, &event).await;
    if targets.is_empty() {
        return;
    }
    let client = relay_client();
    for base_url in targets {
        let url = format!("{base_url}/nodes/relay");
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
    use time::OffsetDateTime;

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

    fn sample_time() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    #[test]
    fn presence_has_no_interest_scope() {
        let event = RelayEvent::Presence(PresenceResponse {
            identity_id: Uuid::new_v4(),
            status: avalon_protocol::social::PresenceStatus::Online,
            active_in: None,
            updated_at: sample_time(),
        });
        assert!(interest_scope_for(&event).is_none());
    }

    #[test]
    fn channel_message_scopes_by_its_channel_id() {
        let channel_id = Uuid::new_v4();
        let event = RelayEvent::ChannelMessage(guild_messages::MessageResponse {
            id: Uuid::new_v4(),
            channel_id,
            author: Uuid::new_v4(),
            body: "hi".to_string(),
            sent_at: sample_time(),
        });
        assert_eq!(
            interest_scope_for(&event),
            Some(InterestScope::Channel(channel_id))
        );
    }

    #[test]
    fn channel_message_deleted_scopes_by_its_channel_id() {
        let channel_id = Uuid::new_v4();
        let event = RelayEvent::ChannelMessageDeleted {
            channel_id,
            message_id: Uuid::new_v4(),
        };
        assert_eq!(
            interest_scope_for(&event),
            Some(InterestScope::Channel(channel_id))
        );
    }

    #[test]
    fn conversation_message_scopes_by_its_conversation_id() {
        let conversation_id = Uuid::new_v4();
        let event = RelayEvent::ConversationMessage(conversations::MessageResponse {
            id: Uuid::new_v4(),
            conversation_id,
            author: Uuid::new_v4(),
            body: "hi".to_string(),
            sent_at: sample_time(),
        });
        assert_eq!(
            interest_scope_for(&event),
            Some(InterestScope::Conversation(conversation_id))
        );
    }
}
