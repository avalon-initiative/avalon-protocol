//! Direct/small-group conversations (issue #102/#105) on
//! [`super::AccountSession`] — see `crates/server/src/conversations.rs`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::AccountSession;

/// A conversation this identity participates in.
#[derive(Debug, Clone, Deserialize)]
pub struct Conversation {
    /// This conversation's own id.
    pub id: Uuid,
    /// Every current participant, including the caller.
    pub participants: Vec<Uuid>,
}

/// One message within a conversation.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}

#[derive(Serialize)]
struct CreateConversationRequest {
    participants: Vec<Uuid>,
}

#[derive(Serialize)]
struct SendConversationMessageRequest<'a> {
    body: &'a str,
}

impl AccountSession {
    /// `GET /conversations`.
    pub async fn list_conversations(&self) -> Result<Vec<Conversation>, SdkError> {
        self.get("/conversations").await
    }

    /// `POST /conversations` — idempotent on the final participant set
    /// (the caller is always added, then deduplicated); returns the
    /// existing conversation rather than creating a duplicate.
    pub async fn create_conversation(
        &self,
        participants: &[Uuid],
    ) -> Result<Conversation, SdkError> {
        self.post(
            "/conversations",
            &CreateConversationRequest {
                participants: participants.to_vec(),
            },
        )
        .await
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
        self.get_query(
            &format!("/conversations/{conversation_id}/messages"),
            &query_refs,
        )
        .await
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
        self.post(
            &format!("/conversations/{conversation_id}/messages"),
            &SendConversationMessageRequest { body },
        )
        .await
    }
}
