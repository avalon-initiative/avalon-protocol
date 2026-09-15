//! Direct/small-group conversations — capability-gated reads/writes on
//! [`crate::Session`] (issue #104), wired to `crates/server/src/conversations.rs`
//! (#102). See `docs/architecture/sdk.md`'s "Today in the repo" for the
//! `messages.read`/`messages.send` capability split and why `dm()` is
//! gated on `messages.send`; `docs/architecture/synchronization.md` for
//! how the deferred submission engine (#111) shares this same request path.

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
    /// `ConversationHandle::send_with_client_entry_id`, which the
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
/// to provide. Every other status falls back to the same status-bucket
/// mapping `crate::http::map_error_response` uses (kept a synchronous,
/// body-free function here rather than that shared helper specifically so
/// it stays unit-testable with a bare `StatusCode`, no response/server
/// needed — this module's own established pattern).
fn conversation_error(status: reqwest::StatusCode) -> SdkError {
    use reqwest::StatusCode;
    match status {
        StatusCode::FORBIDDEN => SdkError::NotConversationParticipant,
        StatusCode::UNAUTHORIZED => SdkError::Unauthorized,
        StatusCode::NOT_FOUND => SdkError::NotFound(status.to_string()),
        StatusCode::CONFLICT => SdkError::Conflict(status.to_string()),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => SdkError::Rejected {
            reason: status.to_string(),
        },
        _ => SdkError::Protocol(status.to_string()),
    }
}

impl Session {
    /// `GET /conversations` — requires `messages.read`. The caller's own
    /// conversation list; a conversation with a block anywhere in its
    /// participant set is already excluded server-side (#102).
    pub async fn conversations(&self) -> Result<Vec<Conversation>, SdkError> {
        self.require(Capability::MessagesRead)?;

        let response = crate::http::send(&self.http, &self.retry, true, |c| {
            c.get(format!("{}/conversations", self.server_url))
                .bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let conversations: Vec<ConversationResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(conversations.into_iter().map(Conversation::from).collect())
    }

    /// A handle scoped to a conversation id already known to the caller
    /// (e.g. from [`Session::conversations`] or [`Session::dm`]) — an
    /// ungated local constructor, same as [`crate::Session::guild`]:
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

        // No idempotency key on this write — `POST /conversations` is
        // itself naturally idempotent server-side on the participant set
        // (see `crates/server/src/conversations.rs`'s module doc comment,
        // "creates, or returns the existing" above), so a plain retry
        // would still be safe in practice, but this SDK doesn't special-
        // case that; it follows the same "no key, no automatic retry"
        // rule as every other write here.
        let response = crate::http::send(&self.http, &self.retry, false, |c| {
            c.post(format!("{}/conversations", self.server_url))
                .bearer_auth(&self.token)
                .json(&CreateConversationRequest {
                    participants: vec![other_identity_id.0],
                })
        })
        .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let body: ConversationResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
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

        let response = crate::http::send(&self.session.http, &self.session.retry, true, |c| {
            c.get(format!(
                "{}/conversations/{}/messages",
                self.session.server_url, self.conversation_id
            ))
            .query(&query)
            .bearer_auth(&self.session.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let messages: Vec<MessageResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
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

        // A caller-supplied `client_entry_id` is this endpoint's own
        // idempotency key (`crates/server/src/conversations.rs`'s
        // `find_message_by_client_entry_id` dedup) — when present, a retry
        // is provably safe, so it opts into the same automatic retry a
        // real `Idempotency-Key` header would (#47); a direct `send()`
        // call (no `client_entry_id`) gets exactly one attempt, same as
        // every other unkeyed write.
        let idempotent = client_entry_id.is_some();
        let response =
            crate::http::send(&self.session.http, &self.session.retry, idempotent, |c| {
                c.post(format!(
                    "{}/conversations/{}/messages",
                    self.session.server_url, self.conversation_id
                ))
                .bearer_auth(&self.session.token)
                .json(&SendMessageRequest {
                    body,
                    client_entry_id,
                })
            })
            .await?;
        if !response.status().is_success() {
            return Err(conversation_error(response.status()));
        }
        let message: MessageResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
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
            integrator_slug: None,
            signing_key: None,
            retry: crate::RetryConfig::default(),
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
            SdkError::NotFound(_)
        ));
    }
}
