//! Guild membership, rosters, channels, and chat — capability-gated
//! reads/writes on [`crate::Session`] (issue #23). See
//! `docs/architecture/sdk.md`'s "Today in the repo" for the
//! `GuildRosterMember` view-type rationale, why the SDK never exposes
//! guild-authority actions, and known gaps (#87 visibility scoping,
//! dropped `archived` field).

use std::collections::HashMap;

use avalon_protocol::guilds::{
    Guild, GuildChannel, GuildEvent, GuildLink, GuildMember, GuildMessage, GuildRole, JoinPolicy,
};
use avalon_protocol::ids::{GuildId, IdentityId};
use avalon_protocol::permissions::Capability;
use avalon_protocol::social::Presence;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use crate::http::websocket_url;
use crate::{SdkError, Session};

/// The calling user's own membership in a guild — `guild` is the full
/// guild record (fetched from `GET /guilds/{id}`, since `GET /me/guilds`
/// only returns the guild id, role, and join timestamp per membership, not
/// the guild itself).
#[derive(Debug, Clone)]
pub struct GuildMembership {
    /// The full guild record.
    pub guild: Guild,
    /// The caller's own role in this guild.
    pub role: GuildRole,
    /// When the caller joined.
    pub joined_at: OffsetDateTime,
}

/// A roster entry, from this integrator's point of view.
///
/// See the module doc comment for why this exists instead of `roster()`
/// returning `Vec<GuildMember>` directly.
#[derive(Debug, Clone)]
pub struct GuildRosterMember {
    /// The protocol member record.
    pub member: GuildMember,
    /// Populated only if `presence.read` is also granted alongside
    /// `guilds.read` — otherwise always `None`, mirroring `Friend::presence`
    /// exactly.
    pub presence: Option<Presence>,
}

/// Builds a [`GuildRosterMember`] from a raw [`GuildMember`] plus whatever
/// presence data is available for that member. Kept as a free function,
/// testable without any HTTP call: an empty `presence_by_id` (exactly what
/// callers pass when `presence.read` isn't granted) always yields
/// `presence: None`.
fn merge_roster_member(
    member: GuildMember,
    presence_by_id: &HashMap<IdentityId, Presence>,
) -> GuildRosterMember {
    let presence = presence_by_id.get(&member.identity_id).cloned();
    GuildRosterMember { member, presence }
}

#[derive(Deserialize)]
struct GuildResponse {
    id: Uuid,
    name: String,
    tag: String,
    description: String,
    owner: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    join_policy: String,
    /// Issue #153.
    motd: Option<String>,
    /// Issue #153.
    banner: Option<String>,
    /// Issue #246.
    icon: Option<String>,
    /// Issue #153.
    #[serde(default)]
    links: Vec<GuildLink>,
    /// Issue #153.
    #[serde(default)]
    recruiting: bool,
    /// Issue #449.
    #[serde(default)]
    public: bool,
}

impl From<GuildResponse> for Guild {
    fn from(response: GuildResponse) -> Self {
        Guild {
            id: GuildId(response.id),
            name: response.name,
            tag: response.tag,
            description: response.description,
            owner: IdentityId(response.owner),
            created_at: response.created_at,
            // Falls back to the type's own default on an unparseable
            // string, same posture `crates/server/src/guilds.rs::guild_row`
            // already takes on this exact field — never a hard failure on
            // a read path.
            join_policy: JoinPolicy::parse(&response.join_policy).unwrap_or_default(),
            motd: response.motd,
            banner: response.banner,
            icon: response.icon,
            links: response.links,
            recruiting: response.recruiting,
            public: response.public,
        }
    }
}

#[derive(Deserialize)]
struct MyGuildMembershipResponse {
    guild_id: Uuid,
    role_index: i32,
    #[serde(with = "time::serde::rfc3339")]
    joined_at: OffsetDateTime,
}

#[derive(Deserialize)]
struct GuildMemberResponse {
    guild_id: Uuid,
    identity_id: Uuid,
    role_index: i32,
    #[serde(with = "time::serde::rfc3339")]
    joined_at: OffsetDateTime,
}

impl From<GuildMemberResponse> for GuildMember {
    fn from(response: GuildMemberResponse) -> Self {
        GuildMember {
            guild_id: GuildId(response.guild_id),
            identity_id: IdentityId(response.identity_id),
            role: GuildRole {
                guild_id: GuildId(response.guild_id),
                // The server stores role_index as a Postgres `int4`
                // (`i32`) — never negative in practice (0 is the starter
                // `owner` role, counting up from there) — while the
                // protocol type models it as `u32`. Clamped rather than
                // panicking on the wire value; a negative index would be a
                // server-side bug, not something an integrator's read should
                // crash on.
                name_index: response.role_index.max(0) as u32,
            },
            joined_at: response.joined_at,
        }
    }
}

#[derive(Deserialize)]
struct ChannelResponse {
    id: Uuid,
    guild_id: Uuid,
    name: String,
    // `archived` intentionally unused — see module doc comment.
    #[allow(dead_code)]
    archived: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[allow(dead_code)]
    created_at: OffsetDateTime,
    // Issue #250. Defaults to `false` for servers predating the field
    // (`#[serde(default)]`) rather than failing to deserialize.
    #[serde(default)]
    announcement_only: bool,
    // Issue #276. Defaults to `None` for servers predating the field.
    #[serde(default)]
    topic: Option<String>,
}

impl From<ChannelResponse> for GuildChannel {
    fn from(response: ChannelResponse) -> Self {
        GuildChannel {
            id: response.id,
            guild_id: GuildId(response.guild_id),
            name: response.name,
            announcement_only: response.announcement_only,
            topic: response.topic,
        }
    }
}

#[derive(Deserialize)]
struct MessageResponse {
    id: Uuid,
    channel_id: Uuid,
    author: Uuid,
    body: String,
    #[serde(with = "time::serde::rfc3339")]
    sent_at: OffsetDateTime,
}

impl From<MessageResponse> for GuildMessage {
    fn from(response: MessageResponse) -> Self {
        GuildMessage {
            id: response.id,
            channel_id: response.channel_id,
            author: IdentityId(response.author),
            body: response.body,
            sent_at: response.sent_at,
        }
    }
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    body: &'a str,
}

/// A pushed update for a channel subscribed via
/// [`ChannelHandle::subscribe_messages`] (issue #438) — a new message, or a
/// moderator deleting one. Not a replacement for [`ChannelHandle::messages`]:
/// a client should still reconcile against that paginated read after a
/// reconnect, the same way `Session::subscribe_presence`'s push is additive
/// to `presence_of`.
#[derive(Debug, Clone)]
pub enum GuildChatEvent {
    /// A new message was posted to the subscribed channel.
    New(GuildMessage),
    /// A moderator deleted a message from the subscribed channel.
    Deleted {
        /// The deleted message's id.
        message_id: Uuid,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SubscribeChannelMessage {
    SubscribeChannel { guild_id: Uuid, channel_id: Uuid },
}

/// Mirrors `crates/server/src/chat.rs`'s `ChatUpdate` wire shape — only the
/// two variants a channel subscription can ever receive (a connection that
/// only ever sends `subscribe_channel` never gets a `conversation_message`
/// back).
#[derive(Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum ChannelWireUpdate {
    ChannelMessage(MessageResponse),
    ChannelMessageDeleted { message_id: Uuid },
}

/// Mirrors `crates/server/src/guild_events.rs::EventResponse`.
///
/// `rsvp_counts` is intentionally dropped on the way to [`GuildEvent`] —
/// same "protocol type has nowhere to put it" posture `ChannelResponse`'s
/// dropped `archived` field documents above. A caller that needs RSVP
/// counts reads them off the raw JSON today; there is no SDK-side RSVP
/// surface yet (issue #169 ships server + protocol + Hub; the ticket's
/// "sdk" scope is limited to this read-only `events()` list, mirroring
/// `channels()`, not create/update/delete/rsvp — those stay user-authority
/// Hub-only actions, same posture channel management already has).
#[derive(Deserialize)]
struct EventResponse {
    id: Uuid,
    guild_id: Uuid,
    channel_id: Option<Uuid>,
    title: String,
    description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    starts_at: OffsetDateTime,
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    ends_at: Option<OffsetDateTime>,
    created_by: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(default)]
    public: bool,
}

impl From<EventResponse> for GuildEvent {
    fn from(response: EventResponse) -> Self {
        GuildEvent {
            id: response.id,
            guild_id: GuildId(response.guild_id),
            channel_id: response.channel_id,
            title: response.title,
            description: response.description,
            starts_at: response.starts_at,
            ends_at: response.ends_at,
            created_by: IdentityId(response.created_by),
            created_at: response.created_at,
            public: response.public,
        }
    }
}

impl Session {
    /// `GET /me/guilds` — requires `guilds.read`. Each membership's full
    /// `Guild` is fetched with one follow-up `GET /guilds/{id}` per
    /// membership, since `GET /me/guilds` itself returns only the guild id,
    /// role, and join timestamp — see the module doc comment.
    pub async fn guilds(&self) -> Result<Vec<GuildMembership>, SdkError> {
        self.require(Capability::GuildsRead)?;

        let response = crate::http::send(&self.http, &self.retry, true, |c| {
            c.get(format!("{}/me/guilds", self.server_url))
                .bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let memberships: Vec<MyGuildMembershipResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;

        let mut result = Vec::with_capacity(memberships.len());
        for membership in memberships {
            let guild = self.fetch_guild(membership.guild_id).await?;
            result.push(GuildMembership {
                role: GuildRole {
                    guild_id: guild.id,
                    name_index: membership.role_index.max(0) as u32,
                },
                guild,
                joined_at: membership.joined_at,
            });
        }
        Ok(result)
    }

    /// `GET /guilds/{id}` for a single guild. Not capability-gated on its
    /// own here since it's only ever called internally by [`Session::guilds`]
    /// (which already required `guilds.read`) and [`GuildHandle`] methods
    /// (which require their own capability before calling this).
    async fn fetch_guild(&self, id: Uuid) -> Result<Guild, SdkError> {
        let response = crate::http::send(&self.http, &self.retry, true, |c| {
            c.get(format!("{}/guilds/{}", self.server_url, id))
                .bearer_auth(&self.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let body: GuildResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(body.into())
    }

    /// A handle scoped to one guild, for the `guild(id).roster()` /
    /// `guild(id).channels()` / `guild(id).channel(cid)` fluent surface.
    /// Not capability-gated itself — capabilities are checked by the
    /// methods called through it.
    pub fn guild(&self, id: GuildId) -> GuildHandle<'_> {
        GuildHandle {
            session: self,
            guild_id: id,
        }
    }
}

/// See [`Session::guild`].
pub struct GuildHandle<'a> {
    session: &'a Session,
    guild_id: GuildId,
}

impl<'a> GuildHandle<'a> {
    /// `GET /guilds/{id}/members` — requires `guilds.read`. Full roster,
    /// no visibility scoping (#87 — see module doc comment). Embeds each
    /// member's [`Presence`] when `presence.read` is granted too, via a
    /// single batched [`Session::presence_of`] call, same pattern
    /// `social.rs::friends` uses.
    pub async fn roster(&self) -> Result<Vec<GuildRosterMember>, SdkError> {
        self.session.require(Capability::GuildsRead)?;

        let response = crate::http::send(&self.session.http, &self.session.retry, true, |c| {
            c.get(format!(
                "{}/guilds/{}/members",
                self.session.server_url, self.guild_id.0
            ))
            .bearer_auth(&self.session.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let members: Vec<GuildMemberResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        let members: Vec<GuildMember> = members.into_iter().map(GuildMember::from).collect();

        let presence_by_id =
            if self.session.require(Capability::PresenceRead).is_ok() && !members.is_empty() {
                let ids: Vec<IdentityId> = members.iter().map(|m| m.identity_id).collect();
                self.session
                    .presence_of(&ids)
                    .await?
                    .into_iter()
                    .map(|p| (p.identity_id, p))
                    .collect()
            } else {
                HashMap::new()
            };

        Ok(members
            .into_iter()
            .map(|member| merge_roster_member(member, &presence_by_id))
            .collect())
    }

    /// `GET /guilds/{id}/channels` — requires `guilds.chat`. Lists both
    /// active and archived channels; see the module doc comment for why
    /// `archived` doesn't survive the mapping to [`GuildChannel`].
    pub async fn channels(&self) -> Result<Vec<GuildChannel>, SdkError> {
        self.session.require(Capability::GuildsChat)?;

        let response = crate::http::send(&self.session.http, &self.session.retry, true, |c| {
            c.get(format!(
                "{}/guilds/{}/channels",
                self.session.server_url, self.guild_id.0
            ))
            .bearer_auth(&self.session.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let channels: Vec<ChannelResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(channels.into_iter().map(GuildChannel::from).collect())
    }

    /// `GET /guilds/{id}/events` — requires `guilds.read`. Lists all
    /// scheduled events for the guild, unfiltered (the server also accepts
    /// `from`/`to` date-range query params — not exposed through this
    /// method yet, matching #169's read-only SDK scope). See
    /// `EventResponse`'s doc comment for why `rsvp_counts` doesn't survive
    /// the mapping to [`GuildEvent`].
    pub async fn events(&self) -> Result<Vec<GuildEvent>, SdkError> {
        self.session.require(Capability::GuildsRead)?;

        let response = crate::http::send(&self.session.http, &self.session.retry, true, |c| {
            c.get(format!(
                "{}/guilds/{}/events",
                self.session.server_url, self.guild_id.0
            ))
            .bearer_auth(&self.session.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let events: Vec<EventResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(events.into_iter().map(GuildEvent::from).collect())
    }

    /// A handle scoped to one channel within this guild, for
    /// `guild(id).channel(cid).messages(...)` / `.send(...)`.
    pub fn channel(&self, id: Uuid) -> ChannelHandle<'a> {
        ChannelHandle {
            session: self.session,
            guild_id: self.guild_id,
            channel_id: id,
        }
    }
}

/// See [`GuildHandle::channel`].
pub struct ChannelHandle<'a> {
    session: &'a Session,
    guild_id: GuildId,
    channel_id: Uuid,
}

impl ChannelHandle<'_> {
    /// `GET /guilds/{id}/channels/{cid}/messages?before=&limit=` — requires
    /// `guilds.chat`. Newest first, cursor-paginated exactly as the server
    /// paginates it (see `crates/server/src/guild_messages.rs::list_messages`);
    /// `before` is a message id already seen by the caller, `limit` is
    /// clamped server-side.
    pub async fn messages(
        &self,
        before: Option<Uuid>,
        limit: Option<i64>,
    ) -> Result<Vec<GuildMessage>, SdkError> {
        self.session.require(Capability::GuildsChat)?;

        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }

        let response = crate::http::send(&self.session.http, &self.session.retry, true, |c| {
            c.get(format!(
                "{}/guilds/{}/channels/{}/messages",
                self.session.server_url, self.guild_id.0, self.channel_id
            ))
            .query(&query)
            .bearer_auth(&self.session.token)
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let messages: Vec<MessageResponse> = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(messages.into_iter().map(GuildMessage::from).collect())
    }

    /// `POST /guilds/{id}/channels/{cid}/messages` — requires `guilds.chat`.
    /// Posts *as the identity* under their own session token; there is no
    /// path for an integrator to post as itself.
    pub async fn send(&self, body: &str) -> Result<GuildMessage, SdkError> {
        self.session.require(Capability::GuildsChat)?;

        // No idempotency key on this write — a direct `send()` call always
        // gets exactly one attempt, same posture as
        // `conversations.rs::ConversationHandle::send`.
        let response = crate::http::send(&self.session.http, &self.session.retry, false, |c| {
            c.post(format!(
                "{}/guilds/{}/channels/{}/messages",
                self.session.server_url, self.guild_id.0, self.channel_id
            ))
            .bearer_auth(&self.session.token)
            .json(&SendMessageRequest { body })
        })
        .await?;
        if !response.status().is_success() {
            return Err(crate::http::map_error_response(response).await);
        }
        let message: MessageResponse = response
            .json()
            .await
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        Ok(message.into())
    }

    /// `GET /ws/messages?token=…`, subscribed to this channel — requires
    /// `guilds.chat`, same as [`Self::messages`]/[`Self::send`]. Sends one
    /// `subscribe_channel` message, then spawns a background task
    /// forwarding every pushed [`GuildChatEvent`] into the returned
    /// channel — same "drop the receiver to end the task" shape
    /// `Session::subscribe_presence` uses (issue #438). Additive to
    /// [`Self::messages`], not a replacement: reconcile against that
    /// paginated read after a reconnect rather than trusting this stream
    /// alone to never have missed anything.
    pub async fn subscribe_messages(
        &self,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<GuildChatEvent>, SdkError> {
        self.session.require(Capability::GuildsChat)?;

        let url = websocket_url(
            &self.session.server_url,
            &format!("/ws/messages?token={}", self.session.token),
        );
        let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| SdkError::WebSocket(e.to_string()))?;
        let (mut write, mut read) = ws_stream.split();

        let subscribe = serde_json::to_string(&SubscribeChannelMessage::SubscribeChannel {
            guild_id: self.guild_id.0,
            channel_id: self.channel_id,
        })
        .expect("SubscribeChannelMessage always serializes");
        write
            .send(WsMessage::Text(subscribe.into()))
            .await
            .map_err(|e| SdkError::WebSocket(e.to_string()))?;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(Ok(message)) = read.next().await {
                let WsMessage::Text(text) = message else {
                    continue;
                };
                let Ok(wire) = serde_json::from_str::<ChannelWireUpdate>(&text) else {
                    continue;
                };
                let event = match wire {
                    ChannelWireUpdate::ChannelMessage(m) => GuildChatEvent::New(m.into()),
                    ChannelWireUpdate::ChannelMessageDeleted { message_id } => {
                        GuildChatEvent::Deleted { message_id }
                    }
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::identity::{Identity, Profile};
    use avalon_protocol::social::PresenceStatus;

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

    fn make_member(identity_id: IdentityId) -> GuildMember {
        GuildMember {
            guild_id: GuildId(Uuid::new_v4()),
            identity_id,
            role: GuildRole {
                guild_id: GuildId(Uuid::new_v4()),
                name_index: 1,
            },
            joined_at: OffsetDateTime::now_utc(),
        }
    }

    fn make_presence(identity_id: IdentityId) -> Presence {
        Presence {
            identity_id,
            status: PresenceStatus::Online,
            active_in: None,
            updated_at: OffsetDateTime::now_utc(),
        }
    }

    #[tokio::test]
    async fn guilds_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.guilds().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn roster_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.guild(GuildId(Uuid::new_v4())).roster().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn channels_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.guild(GuildId(Uuid::new_v4())).channels().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn messages_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session
            .guild(GuildId(Uuid::new_v4()))
            .channel(Uuid::new_v4())
            .messages(None, None)
            .await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn send_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session
            .guild(GuildId(Uuid::new_v4()))
            .channel(Uuid::new_v4())
            .send("hello")
            .await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    /// `guilds.read` alone (no `guilds.chat`) must not satisfy
    /// `channels()`/`messages()`/`send()` — there is no `guilds.*` blanket
    /// check.
    #[tokio::test]
    async fn guilds_read_does_not_satisfy_guilds_chat_methods() {
        let session = test_session(vec!["guilds.read"]);
        let result = session.guild(GuildId(Uuid::new_v4())).channels().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[test]
    fn merge_roster_member_has_no_presence_when_map_is_empty() {
        let identity_id = IdentityId(Uuid::new_v4());
        let member = make_member(identity_id);

        let entry = merge_roster_member(member, &HashMap::new());

        assert_eq!(entry.member.identity_id, identity_id);
        assert!(entry.presence.is_none());
    }

    #[test]
    fn merge_roster_member_picks_up_presence_when_present_in_map() {
        let identity_id = IdentityId(Uuid::new_v4());
        let member = make_member(identity_id);
        let mut presence_by_id = HashMap::new();
        presence_by_id.insert(identity_id, make_presence(identity_id));

        let entry = merge_roster_member(member, &presence_by_id);

        assert_eq!(entry.member.identity_id, identity_id);
        assert!(entry.presence.is_some());
    }
}
