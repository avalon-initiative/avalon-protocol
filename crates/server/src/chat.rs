//! Live push for guild channel messages and DM/conversation messages
//! (issue #438) — the same transport shape `crate::presence` established
//! for presence (issue #136), extended to the two other realtime surfaces
//! #119's original decision always intended it to cover. See
//! `docs/architecture/communication.md`'s "Today in the repo" and
//! [ADR #437](https://github.com/LunarVagabond/avalon-protocol/issues/437)'s
//! freshness-tier policy for why chat/DMs are push, not poll.

use std::collections::HashSet;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::conversations;
use crate::error::AppError;
use crate::guild_messages;
use crate::handlers::authenticate_token;
use crate::state::AppState;

/// Same rationale as `presence::UPDATE_CHANNEL_CAPACITY`: bounded so a burst
/// of sends can't grow this unboundedly if a subscriber is slow to drain —
/// a lagging receiver drops the oldest messages instead of the channel
/// growing without limit.
const UPDATE_CHANNEL_CAPACITY: usize = 256;

/// One update pushed to every connected client, regardless of what it's
/// subscribed to — the per-connection handler filters by
/// `channel_id`/`conversation_id` before ever serializing/sending, the same
/// "single broadcast, filtered client-side" shape `presence::PresenceStore`
/// uses. Adjacently tagged (`type` + `data`) since a couple of these
/// variants carry a bare struct rather than a newtype the client can
/// destructure directly.
#[derive(Serialize, Clone)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum ChatUpdate {
    ChannelMessage(guild_messages::MessageResponse),
    ChannelMessageDeleted { channel_id: Uuid, message_id: Uuid },
    ConversationMessage(conversations::MessageResponse),
}

/// Fan-out for new/deleted guild channel messages and new conversation
/// messages. Deliberately not durable and not the source of truth — Postgres
/// (`guild_messages`/`conversation_messages`) is; this only tells an already-
/// subscribed client to go look, the same way `presence::PresenceStore`'s
/// broadcast is additive to `GET /presence` rather than replacing it. A
/// lossy broadcast (a slow/absent subscriber just misses a tick) is an
/// accepted tradeoff here for the same reason it is for presence: a client
/// that misses a push still has the cursor-paginated `GET` to fall back to
/// and reconcile against, so a dropped push is a delay, never a lost
/// message.
#[derive(Clone)]
pub struct ChatBus {
    updates: tokio::sync::broadcast::Sender<ChatUpdate>,
}

impl ChatBus {
    pub fn new() -> Self {
        let (updates, _) = tokio::sync::broadcast::channel(UPDATE_CHANNEL_CAPACITY);
        Self { updates }
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ChatUpdate> {
        self.updates.subscribe()
    }

    /// Called by `guild_messages::send_message` right after a successful
    /// insert.
    pub fn publish_channel_message(&self, message: guild_messages::MessageResponse) {
        let _ = self.updates.send(ChatUpdate::ChannelMessage(message));
    }

    /// Called by `guild_messages::delete_message` right after a successful
    /// delete (live or archive tier).
    pub fn publish_channel_message_deleted(&self, channel_id: Uuid, message_id: Uuid) {
        let _ = self.updates.send(ChatUpdate::ChannelMessageDeleted {
            channel_id,
            message_id,
        });
    }

    /// Called by `conversations::send_message` right after a successful
    /// insert (not on the idempotent-retry path, which already published
    /// once on the original send).
    pub fn publish_conversation_message(&self, message: conversations::MessageResponse) {
        let _ = self.updates.send(ChatUpdate::ConversationMessage(message));
    }
}

impl Default for ChatBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Same `?token=` tradeoff `presence::PresenceWsQuery` documents: a browser
/// `WebSocket` handshake can't set an `Authorization` header.
#[derive(Deserialize)]
pub struct ChatWsQuery {
    pub token: String,
}

/// `GET /ws/messages?token=…` — additive to the existing paginated
/// `GET .../messages` endpoints, not a replacement. Authenticates before
/// upgrading, same as `presence::presence_ws`.
pub async fn chat_ws(
    State(state): State<AppState>,
    Query(query): Query<ChatWsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let caller = authenticate_token(&state, &query.token).await?;
    Ok(ws.on_upgrade(move |socket| handle_chat_socket(socket, state, caller)))
}

/// What a subscribed client can send. Additive, same as
/// `presence::ClientMessage::Subscribe` — subscribing to more
/// channels/conversations grows the connection's subscription sets rather
/// than replacing them.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatClientMessage {
    SubscribeChannel { guild_id: Uuid, channel_id: Uuid },
    SubscribeConversation { conversation_id: Uuid },
}

async fn handle_chat_socket(mut socket: WebSocket, state: AppState, caller: Uuid) {
    let mut subscribed_channels: HashSet<Uuid> = HashSet::new();
    let mut subscribed_conversations: HashSet<Uuid> = HashSet::new();
    let mut updates = state.chat.subscribe();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(msg) = serde_json::from_str::<ChatClientMessage>(&text) else {
                            continue;
                        };
                        match msg {
                            ChatClientMessage::SubscribeChannel { guild_id, channel_id } => {
                                // Same membership/existence check
                                // `guild_messages::list_messages` runs — a
                                // subscribe attempt for a channel the caller
                                // can't read is silently dropped, never a
                                // distinguishable error, matching
                                // `presence`'s posture for an unauthorized id.
                                let authorized = crate::channels::require_member(&state, guild_id, caller).await.is_ok()
                                    && crate::channels::fetch_channel(&state, guild_id, channel_id).await.is_ok();
                                if authorized {
                                    subscribed_channels.insert(channel_id);
                                }
                            }
                            ChatClientMessage::SubscribeConversation { conversation_id } => {
                                // Same gate `conversations::list_messages`
                                // runs — not currently a participant (or a
                                // block exists among participants) silently
                                // drops the subscribe attempt.
                                if conversations::require_unblocked_participant(&state, conversation_id, caller).await.is_ok() {
                                    subscribed_conversations.insert(conversation_id);
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => {}
                }
            }
            update = updates.recv() => {
                match update {
                    Ok(update) => {
                        let relevant = match &update {
                            ChatUpdate::ChannelMessage(m) => subscribed_channels.contains(&m.channel_id),
                            ChatUpdate::ChannelMessageDeleted { channel_id, .. } => subscribed_channels.contains(channel_id),
                            ChatUpdate::ConversationMessage(m) => subscribed_conversations.contains(&m.conversation_id),
                        };
                        if !relevant {
                            continue;
                        }
                        let payload = serde_json::to_string(&update)
                            .expect("ChatUpdate always serializes");
                        if socket.send(Message::Text(payload.into())).await.is_err() {
                            return;
                        }
                    }
                    // A slow consumer missed some ticks — the paginated GET
                    // is always there to reconcile against; dropping interim
                    // ticks is the documented tradeoff of a lossy broadcast
                    // channel, same as `presence::handle_presence_socket`.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}
