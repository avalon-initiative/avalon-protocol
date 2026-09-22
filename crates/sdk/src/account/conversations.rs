//! Direct/small-group conversations (issue #102/#105) on
//! [`super::AccountSession`] — see `crates/server/src/conversations.rs`.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::AccountSession;

/// A conversation this identity participates in.
#[derive(Debug, Clone)]
pub struct Conversation {
    /// This conversation's own id.
    pub id: Uuid,
    /// Every current participant, including the caller.
    pub participants: Vec<Uuid>,
}

impl From<crate::generated::ConversationResponse> for Conversation {
    fn from(body: crate::generated::ConversationResponse) -> Self {
        Conversation {
            id: body.id,
            participants: body.participants,
        }
    }
}

/// One message within a conversation.
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    /// This message's own id.
    pub id: Uuid,
    /// The conversation it belongs to.
    pub conversation_id: Uuid,
    /// The sender.
    pub author: Uuid,
    /// The message body.
    pub body: String,
    /// When it was sent.
    pub sent_at: OffsetDateTime,
}

// `ConversationMessageResponse`, not `MessageResponse` — the two used to
// collide in the published schema (both registered as bare
// `MessageResponse`; see `crates/server/src/conversations.rs`'s own doc
// comment on the `#[schema(as = ...)]` fix, #726) and this crate originally
// migrated onto the wrong (silently-overwritten) one, reading a
// `channel_id` field the real `/conversations/{id}/messages` response never
// actually sends. `SendMessageRequest`/`ConversationSendMessageRequest` had
// the same collision (issue #742) — fixed the same way.
impl TryFrom<crate::generated::ConversationMessageResponse> for ConversationMessage {
    type Error = SdkError;

    fn try_from(body: crate::generated::ConversationMessageResponse) -> Result<Self, SdkError> {
        Ok(ConversationMessage {
            id: body.id,
            conversation_id: body.conversation_id,
            author: body.author,
            body: body.body,
            sent_at: super::parse_rfc3339(&body.sent_at)?,
        })
    }
}

impl AccountSession {
    /// `GET /conversations`.
    pub async fn list_conversations(&self) -> Result<Vec<Conversation>, SdkError> {
        let raw: Vec<crate::generated::ConversationResponse> = self
            .get(crate::generated::paths::chat::LIST_MY_CONVERSATIONS)
            .await?;
        Ok(raw.into_iter().map(Conversation::from).collect())
    }

    /// `POST /conversations` — idempotent on the final participant set
    /// (the caller is always added, then deduplicated); returns the
    /// existing conversation rather than creating a duplicate.
    pub async fn create_conversation(
        &self,
        participants: &[Uuid],
    ) -> Result<Conversation, SdkError> {
        let raw: crate::generated::ConversationResponse = self
            .post(
                crate::generated::paths::chat::CREATE_CONVERSATION,
                &crate::generated::CreateConversationRequest {
                    participants: participants.to_vec(),
                },
            )
            .await?;
        Ok(raw.into())
    }

    /// `GET /conversations/{id}/messages`, cursor-paginated with `before`.
    pub async fn conversation_messages(
        &self,
        conversation_id: Uuid,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<ConversationMessage>, SdkError> {
        let mut query = Vec::new();
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let query_refs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let raw: Vec<crate::generated::ConversationMessageResponse> = self
            .get_query(
                &super::path(
                    crate::generated::paths::chat::LIST_MESSAGES,
                    &[("id", &conversation_id.to_string())],
                ),
                &query_refs,
            )
            .await?;
        raw.into_iter().map(ConversationMessage::try_from).collect()
    }

    /// `POST /conversations/{id}/messages`. Not signature-required (chat is
    /// high-frequency and reversible by deletion, per #697's own
    /// invariants) — though conversations have no moderation-delete
    /// endpoint themselves, unlike guild channel messages.
    pub async fn send_conversation_message(
        &self,
        conversation_id: Uuid,
        body: &str,
    ) -> Result<ConversationMessage, SdkError> {
        let raw: crate::generated::ConversationMessageResponse = self
            .post(
                &super::path(
                    crate::generated::paths::chat::SEND_MESSAGE,
                    &[("id", &conversation_id.to_string())],
                ),
                &crate::generated::ConversationSendMessageRequest {
                    body: body.to_string(),
                    client_entry_id: None,
                },
            )
            .await?;
        raw.try_into()
    }
}
