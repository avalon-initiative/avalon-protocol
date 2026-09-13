//! Direct/small-group conversations — capability-gated reads/writes on
//! [`crate::Session`] (issue #104), wired to the real `avalon-server`
//! conversation endpoints from #102 (`crates/server/src/conversations.rs`).
//!
//! Every method here calls [`crate::Session::require`] with its exact
//! capability *before* making any request — same convention `social.rs`
//! (#17) and `guilds.rs` (#23) already established. The server enforces the
//! same capabilities again once #26–#28 land; this check is a convenience
//! for integrator developers, not the security boundary.
//!
//! ## Two capabilities, split the same way `achievements()`/
//! `issue_achievement()` split theirs
//!
//! `messages.read` covers discovering and reading conversations an identity is
//! already in: [`Session::conversations`] (`GET /conversations`) and
//! [`ConversationHandle::messages`] (`GET /conversations/{id}/messages`).
//! `messages.send` covers *starting* a conversation and posting into one:
//! [`Session::dm`] (`POST /conversations`) and [`ConversationHandle::send`]
//! (`POST /conversations/{id}/messages`).
//!
//! `dm(other_identity_id)` is gated on `messages.send`, not `messages.read`,
//! even though the server's `POST /conversations` also returns the full
//! participant list: the only thing a caller can *do* with a brand-new
//! conversation is start it, and an integrator that only has `messages.send`
//! (e.g. "let an identity reply to a support conversation" without being
//! able to browse the identity's whole DM list) should still be able to
//! open one. A `messages.read`-only integrator isn't left without a way to
//! reach a specific conversation's messages either — it can still learn
//! conversation ids from
//! [`Session::conversations`] and open a handle to any of them via
//! [`Session::conversation`], which — like [`crate::guilds::GuildHandle`] —
//! is a plain, ungated local constructor that makes no request of its own;
//! only the methods called through it touch the network.
//!
//! ## Shared with the deferred submission engine (#111)
//!
//! `crate::submission::HttpTransport` submits queued `chat.message` journal
//! entries through [`Session::conversation`]/[`ConversationHandle::send_with_client_entry_id`]
//! rather than building its own request — one request-building path, so a
//! capability check (or any other classification) behaves the same whether
//! an integrator calls [`ConversationHandle::send`] directly or drains a
//! [`crate::sync_journal::SyncJournal`] through the submission engine.
//!
//! ## No conversation content is cached
//!
//! Every method here makes a fresh request; nothing is stored on
//! [`crate::Session`] beyond the bearer token and server URL it already
//! carries. Matches this issue's own invariant and the same posture
//! `social.rs`/`guilds.rs` already take.

use avalon_protocol::ids::IdentityId;
use avalon_protocol::permissions::Capability;
use avalon_protocol::social::{Conversation, ConversationMessage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{SdkError, Session};

#[derive(Deserialize)]
struct ConversationResponse {
    id: Uuid,
    participants: Vec<Uuid>,
}

impl From<ConversationResponse> for Conversation {
    fn from(response: ConversationResponse) -> Self {
        Conversation {
            id: response.id,
            participants: response.participants.into_iter().map(IdentityId).collect(),
        }
    }
}

#[derive(Serialize)]
struct CreateConversationRequest {
    participants: Vec<Uuid>,
}

#[derive(Deserialize)]
struct MessageResponse {
    id: Uuid,
    conversation_id: Uuid,
    author: Uuid,
    body: String,
    #[serde(with = "time::serde::rfc3339")]
    sent_at: time::OffsetDateTime,
}

impl From<MessageResponse> for ConversationMessage {
    fn from(response: MessageResponse) -> Self {
        ConversationMessage {
            id: response.id,
            conversation_id: response.conversation_id,
            author: IdentityId(response.author),
            body: response.body,
            sent_at: response.sent_at,
        }
    }
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    body: &'a str,
    /// See `crates/server/src/conversations.rs::SendMessageRequest`'s own
    /// doc comment — set only by
    /// [`ConversationHandle::send_with_client_entry_id`], which the
    /// submission engine (`crate::submission::HttpTransport`, issue #111)
    /// uses so a retried submission dedupes server-side instead of posting
    /// twice. A direct [`ConversationHandle::send`] call always sends
    /// `None` here.
    client_entry_id: Option<Uuid>,
}

/// Translates a non-success response from any `/conversations` endpoint.
/// The server collapses "never a participant" and "blocked" into the exact
/// same `403 Forbidden` (issue #97) — this maps that one status to
/// [`SdkError::NotConversationParticipant`] without inspecting the response
/// body, so the SDK never has more to leak than the server already refused
/// to provide. Every other status still becomes the generic
/// [`SdkError::ServerError`].
fn conversation_error(status: reqwest::StatusCode) -> SdkError {
    if status == reqwest::StatusCode::FORBIDDEN {
        SdkError::NotConversationParticipant
    } else {
        SdkError::ServerError(status)
    }
}

impl Session {
    /// `GET /conversations` — requires `messages.read`. The caller's own
    /// conversation list; a conversation with a block anywhere in its
    /// participant set is already excluded server-side (#102).
    pub async fn conversations(&self) -> Result<Vec<Conversation>, SdkError> {
        self.require(Capability::MessagesRead)?;

        let response = self
            .http
            .get(format!("{}/conversations", self.server_url))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let conversations: Vec<ConversationResponse> = response.json().await?;
        Ok(conversations.into_iter().map(Conversation::from).collect())
    }

    /// A handle scoped to a conversation id already known to the caller
    /// (e.g. from [`Session::conversations`] or [`Session::dm`]) — an
    /// ungated local constructor, same as [`crate::guilds::Session::guild`]:
    /// it makes no request of its own, only the methods called through it
    /// do.
    pub fn conversation(&self, id: Uuid) -> ConversationHandle<'_> {
        ConversationHandle {
            session: self,
            conversation_id: id,
        }
    }

    /// `POST /conversations` — requires `messages.send`. Creates, or
    /// returns the existing, 1:1 conversation between the caller and
    /// `other_identity_id` (idempotent on the participant set — see
    /// `crates/server/src/conversations.rs`'s module doc comment). See the
    /// module doc comment for why this is gated on `messages.send` rather
    /// than `messages.read`.
    pub async fn dm(
        &self,
        other_identity_id: IdentityId,
    ) -> Result<ConversationHandle<'_>, SdkError> {
        self.require(Capability::MessagesSend)?;

        let response = self
            .http
            .post(format!("{}/conversations", self.server_url))
            .bearer_auth(&self.token)
            .json(&CreateConversationRequest {
                participants: vec![other_identity_id.0],
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let body: ConversationResponse = response.json().await?;
        Ok(self.conversation(body.id))
    }
}

/// See [`Session::conversation`] / [`Session::dm`].
pub struct ConversationHandle<'a> {
    session: &'a Session,
    conversation_id: Uuid,
}

impl ConversationHandle<'_> {
    /// The conversation id this handle is scoped to.
    pub fn conversation_id(&self) -> Uuid {
        self.conversation_id
    }

    /// `GET /conversations/{id}/messages?before=&limit=` — requires
    /// `messages.read`. Newest first, cursor-paginated exactly as the
    /// server paginates it (see
    /// `crates/server/src/conversations.rs::list_messages`); `before` is a
    /// message id already seen by the caller, `limit` is clamped
    /// server-side. Fails with [`SdkError::NotConversationParticipant`] if
    /// the caller isn't (or is no longer, due to a block) a participant —
    /// see that variant's doc comment.
    pub async fn messages(
        &self,
        before: Option<Uuid>,
        limit: Option<i64>,
    ) -> Result<Vec<ConversationMessage>, SdkError> {
        self.session.require(Capability::MessagesRead)?;

        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }

        let response = self
            .session
            .http
            .get(format!(
                "{}/conversations/{}/messages",
                self.session.server_url, self.conversation_id
            ))
            .query(&query)
            .bearer_auth(&self.session.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let messages: Vec<MessageResponse> = response.json().await?;
        Ok(messages
            .into_iter()
            .map(ConversationMessage::from)
            .collect())
    }

    /// `POST /conversations/{id}/messages` — requires `messages.send`.
    /// Posts *as the identity* under their own session token; there is no
    /// path for an integrator to post as itself. Fails with
    /// [`SdkError::NotConversationParticipant`] if the caller isn't (or is
    /// no longer, due to a block) a participant — see that variant's doc
    /// comment for why a blocked send is indistinguishable from a
    /// never-was-a-participant one.
    pub async fn send(&self, body: &str) -> Result<ConversationMessage, SdkError> {
        self.send_with_client_entry_id(body, None).await
    }

    /// Same request as [`Self::send`], with an optional idempotency key —
    /// the one thing `crate::submission::HttpTransport` (issue #111) needs
    /// beyond what a direct online send does. This is the single place that
    /// builds a `POST /conversations/{id}/messages` request; both `send`
    /// and the submission engine's transport route through it so a
    /// capability check failure (or any other classification) behaves
    /// identically regardless of which path an integrator used.
    pub(crate) async fn send_with_client_entry_id(
        &self,
        body: &str,
        client_entry_id: Option<Uuid>,
    ) -> Result<ConversationMessage, SdkError> {
        self.session.require(Capability::MessagesSend)?;

        let response = self
            .session
            .http
            .post(format!(
                "{}/conversations/{}/messages",
                self.session.server_url, self.conversation_id
            ))
            .bearer_auth(&self.session.token)
            .json(&SendMessageRequest {
                body,
                client_entry_id,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let message: MessageResponse = response.json().await?;
        Ok(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::identity::{Identity, Profile};
    use time::OffsetDateTime;

    /// Builds a `Session` with no live server behind it — every test here
    /// must be rejected by the capability check before any request is
    /// attempted. Same pattern `social.rs`/`guilds.rs` use for their own
    /// capability-check tests.
    fn test_session(granted: Vec<&str>) -> Session {
        let self_id = IdentityId(Uuid::new_v4());
        Session {
            identity: Identity {
                id: self_id,
                created_at: OffsetDateTime::now_utc(),
            },
            profile: Profile {
                identity_id: self_id,
                display_name: "test".to_string(),
                avatar_url: None,
                bio: None,
                favorite_genres: Vec::new(),
                pronouns: None,
                banner_url: None,
                status: None,
                links: Vec::new(),
                timezone: None,
                theme_color: None,
                location: None,
                main_guild: None,
            },
            granted: granted.into_iter().map(Capability::from).collect(),
            http: reqwest::Client::new(),
            // Deliberately unroutable — these tests must never actually
            // reach the network; an attempted connection here would hang or
            // error in a way that's obviously not `CapabilityNotGranted`.
            server_url: "http://127.0.0.1:1".to_string(),
            token: "test-token".to_string(),
            integrator_key_id: "test-key".to_string(),
            game_slug: None,
            signing_key: None,
        }
    }

    #[tokio::test]
    async fn conversations_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.conversations().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn dm_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.dm(IdentityId(Uuid::new_v4())).await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn messages_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session
            .conversation(Uuid::new_v4())
            .messages(None, None)
            .await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn send_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.conversation(Uuid::new_v4()).send("hello").await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    /// `messages.read` alone must not satisfy `dm()`/`send()` — there is no
    /// `messages.*` blanket check, mirroring `guilds.read` not satisfying
    /// `guilds.chat` methods.
    #[tokio::test]
    async fn messages_read_does_not_satisfy_messages_send_methods() {
        let session = test_session(vec!["messages.read"]);
        let result = session.dm(IdentityId(Uuid::new_v4())).await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));

        let result = session.conversation(Uuid::new_v4()).send("hello").await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    /// `messages.send` alone must not satisfy `conversations()`/`messages()`.
    #[tokio::test]
    async fn messages_send_does_not_satisfy_messages_read_methods() {
        let session = test_session(vec!["messages.send"]);
        let result = session.conversations().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));

        let result = session
            .conversation(Uuid::new_v4())
            .messages(None, None)
            .await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[test]
    fn conversation_error_maps_forbidden_to_the_uninformative_variant() {
        assert!(matches!(
            conversation_error(reqwest::StatusCode::FORBIDDEN),
            SdkError::NotConversationParticipant
        ));
    }

    #[test]
    fn conversation_error_leaves_other_statuses_generic() {
        assert!(matches!(
            conversation_error(reqwest::StatusCode::NOT_FOUND),
            SdkError::ServerError(status) if status == reqwest::StatusCode::NOT_FOUND
        ));
    }
}
