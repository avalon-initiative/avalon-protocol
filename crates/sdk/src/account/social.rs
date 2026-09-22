//! Friends, blocks, presence, and discovery (issues #15/#97/#136/#204/#205)
//! on [`super::AccountSession`] — the first-party counterpart to
//! `crate::social`'s capability-gated integrator methods. See
//! `crates/server/src/friends.rs`/`blocks.rs`/`presence.rs`/`discovery.rs`.

use crate::types::social::PresenceStatus;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::SdkError;

use super::AccountSession;

/// A confirmed friendship — `a`/`b` are the two identities, in no
/// particular order (the server never distinguishes "who sent the
/// request" past acceptance).
#[derive(Debug, Clone)]
pub struct Friendship {
    /// One side of the friendship.
    pub a: Uuid,
    /// The other side.
    pub b: Uuid,
    /// When the friendship was established.
    pub since: OffsetDateTime,
}

impl TryFrom<crate::generated::FriendshipResponse> for Friendship {
    type Error = SdkError;

    fn try_from(body: crate::generated::FriendshipResponse) -> Result<Self, SdkError> {
        Ok(Friendship {
            a: body.a,
            b: body.b,
            since: super::parse_rfc3339(&body.since)?,
        })
    }
}

/// A pending friend request.
#[derive(Debug, Clone)]
pub struct FriendRequest {
    /// This request's own id.
    pub id: Uuid,
    /// The requester.
    pub from: Uuid,
    /// The recipient.
    pub to: Uuid,
    /// When the request was sent.
    pub requested_at: OffsetDateTime,
}

impl TryFrom<crate::generated::FriendRequestResponse> for FriendRequest {
    type Error = SdkError;

    fn try_from(body: crate::generated::FriendRequestResponse) -> Result<Self, SdkError> {
        Ok(FriendRequest {
            id: body.id,
            from: body.from,
            to: body.to,
            requested_at: super::parse_rfc3339(&body.requested_at)?,
        })
    }
}

/// One outgoing block — see `crates/server/src/blocks.rs`'s own invariant:
/// there is no endpoint anywhere that reveals who has blocked *you*.
#[derive(Debug, Clone)]
pub struct Block {
    /// The blocked identity.
    pub blocked: Uuid,
    /// When the block was created.
    pub created_at: OffsetDateTime,
}

impl TryFrom<crate::generated::BlockListEntry> for Block {
    type Error = SdkError;

    fn try_from(body: crate::generated::BlockListEntry) -> Result<Self, SdkError> {
        Ok(Block {
            blocked: body.blocked,
            created_at: super::parse_rfc3339(&body.created_at)?,
        })
    }
}

impl TryFrom<crate::generated::BlockResponse> for Block {
    type Error = SdkError;

    fn try_from(body: crate::generated::BlockResponse) -> Result<Self, SdkError> {
        Ok(Block {
            blocked: body.blocked,
            created_at: super::parse_rfc3339(&body.created_at)?,
        })
    }
}

/// A public-face profile — the least-sensitive batch-resolvable fields
/// only (`GET /identities/profiles`).
#[derive(Debug, Clone)]
pub struct PublicProfile {
    /// The identity these fields describe.
    pub identity_id: Uuid,
    /// Display name / globally-unique handle.
    pub display_name: String,
    /// Avatar image URL, if set.
    pub avatar_url: Option<String>,
}

impl From<crate::generated::PublicProfileResponse> for PublicProfile {
    fn from(body: crate::generated::PublicProfileResponse) -> Self {
        PublicProfile {
            identity_id: body.identity_id,
            display_name: body.display_name,
            avatar_url: body.avatar_url,
        }
    }
}

/// A "people you may know" candidate (`GET /people/discover`).
///
/// Only ever carries `identity_id` — matching
/// `crates/server/src/discovery.rs::DiscoveryCandidate`'s real (narrower)
/// shape. A prior version of this struct declared `display_name`/
/// `avatar_url`/`mutual_friends`/`mutual_guilds` fields the server has
/// never actually sent (`display_name` wasn't even optional, so every real
/// `discover_people()` call would have failed to deserialize) — found and
/// fixed migrating onto the generated type, which is exactly the real
/// server response shape.
#[derive(Debug, Clone)]
pub struct DiscoveryCandidate {
    /// The candidate identity.
    pub identity_id: Uuid,
}

impl From<crate::generated::DiscoveryCandidate> for DiscoveryCandidate {
    fn from(body: crate::generated::DiscoveryCandidate) -> Self {
        DiscoveryCandidate {
            identity_id: body.identity_id,
        }
    }
}

/// A single global search result (`GET /identities/search`).
#[derive(Debug, Clone)]
pub struct SearchResultIdentity {
    /// The matched identity.
    pub identity_id: Uuid,
    /// Display name.
    pub display_name: String,
    /// Avatar image URL, if set.
    pub avatar_url: Option<String>,
}

impl From<crate::generated::SearchResultIdentity> for SearchResultIdentity {
    fn from(body: crate::generated::SearchResultIdentity) -> Self {
        SearchResultIdentity {
            identity_id: body.identity_id,
            display_name: body.display_name,
            avatar_url: body.avatar_url,
        }
    }
}

/// This identity's own presence, as currently published.
#[derive(Debug, Clone)]
pub struct Presence {
    /// The identity this presence describes.
    pub identity_id: Uuid,
    /// Current status.
    pub status: PresenceStatus,
    /// Which guild the identity is currently active in, if visible.
    pub active_in: Option<Uuid>,
    /// When this status was last published.
    pub updated_at: OffsetDateTime,
}

impl TryFrom<crate::generated::PresenceResponse> for Presence {
    type Error = SdkError;

    fn try_from(body: crate::generated::PresenceResponse) -> Result<Self, SdkError> {
        Ok(Presence {
            identity_id: body.identity_id,
            status: body.status,
            active_in: body.active_in,
            updated_at: super::parse_rfc3339(&body.updated_at)?,
        })
    }
}

/// One entry of the caller's own recent protocol history (`GET /me/history`).
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// The underlying durable event's id.
    pub event_id: Uuid,
    /// The event's kind (e.g. `"friend.requested"`).
    pub kind: String,
    /// The event's subject `GlobalId` string.
    pub subject: String,
    /// The event's own payload, or `None` if pruned locally.
    pub payload: Option<serde_json::Value>,
    /// When the event occurred.
    pub timestamp: OffsetDateTime,
}

impl TryFrom<crate::generated::HistoryEntryResponse> for HistoryEntry {
    type Error = SdkError;

    fn try_from(body: crate::generated::HistoryEntryResponse) -> Result<Self, SdkError> {
        Ok(HistoryEntry {
            event_id: body.event_id,
            kind: body.kind,
            subject: body.subject,
            payload: body.payload.map(serde_json::Value::Object),
            timestamp: super::parse_rfc3339(&body.timestamp)?,
        })
    }
}

/// Another identity's full self-description profile (`GET
/// /identities/{id}/profile`, issue #403) — same fields `GET /me` exposes
/// for the caller's own profile.
#[derive(Debug, Clone)]
pub struct PublicIdentityProfile {
    /// The identity these fields describe.
    pub identity_id: Uuid,
    /// When this identity was created.
    pub identity_created_at: OffsetDateTime,
    /// Display name / globally-unique handle.
    pub display_name: String,
    /// Avatar image URL, if set.
    pub avatar_url: Option<String>,
    /// Free-text bio, if set.
    pub bio: Option<String>,
    /// Self-described favorite genres.
    pub favorite_genres: Vec<crate::types::identity::Genre>,
    /// Free-text pronouns, if set.
    pub pronouns: Option<String>,
    /// Banner image URL, if set.
    pub banner_url: Option<String>,
    /// Free-text status line, if set.
    pub status: Option<String>,
    /// External links.
    pub links: Vec<String>,
    /// Self-described timezone, if set.
    pub timezone: Option<String>,
    /// Self-described theme color, if set.
    pub theme_color: Option<String>,
    /// Self-described free-text location, if set.
    pub location: Option<String>,
}

impl TryFrom<crate::generated::PublicIdentityProfileResponse> for PublicIdentityProfile {
    type Error = SdkError;

    fn try_from(body: crate::generated::PublicIdentityProfileResponse) -> Result<Self, SdkError> {
        Ok(PublicIdentityProfile {
            identity_id: body.identity_id,
            identity_created_at: super::parse_rfc3339(&body.identity_created_at)?,
            display_name: body.display_name,
            avatar_url: body.avatar_url,
            bio: body.bio,
            favorite_genres: body.favorite_genres,
            pronouns: body.pronouns,
            banner_url: body.banner_url,
            status: body.status,
            links: body.links,
            timezone: body.timezone,
            theme_color: body.theme_color,
            location: body.location,
        })
    }
}

/// A recent announcement-only channel post (`GET /me/guild-announcements`,
/// issue #280).
#[derive(Debug, Clone)]
pub struct GuildAnnouncementAlert {
    /// The underlying message's id.
    pub message_id: Uuid,
    /// The channel it was posted in.
    pub channel_id: Uuid,
    /// That channel's name.
    pub channel_name: String,
    /// The guild the channel belongs to.
    pub guild_id: Uuid,
    /// The message's author.
    pub author: Uuid,
    /// The message body.
    pub body: String,
    /// When it was sent.
    pub sent_at: OffsetDateTime,
}

impl TryFrom<crate::generated::GuildAnnouncementAlert> for GuildAnnouncementAlert {
    type Error = SdkError;

    fn try_from(body: crate::generated::GuildAnnouncementAlert) -> Result<Self, SdkError> {
        Ok(GuildAnnouncementAlert {
            message_id: body.message_id,
            channel_id: body.channel_id,
            channel_name: body.channel_name,
            guild_id: body.guild_id,
            author: body.author,
            body: body.body,
            sent_at: super::parse_rfc3339(&body.sent_at)?,
        })
    }
}

impl AccountSession {
    /// `GET /friends`.
    pub async fn friends(&self) -> Result<Vec<Friendship>, SdkError> {
        let raw: Vec<crate::generated::FriendshipResponse> = self
            .get(crate::generated::paths::friends::LIST_FRIENDS)
            .await?;
        raw.into_iter().map(Friendship::try_from).collect()
    }

    /// `GET /friends/requests` — both incoming and outgoing.
    pub async fn friend_requests(&self) -> Result<Vec<FriendRequest>, SdkError> {
        let raw: Vec<crate::generated::FriendRequestResponse> = self
            .get(crate::generated::paths::friends::LIST_FRIEND_REQUESTS)
            .await?;
        raw.into_iter().map(FriendRequest::try_from).collect()
    }

    /// `POST /friends/requests`.
    pub async fn create_friend_request(&self, to: Uuid) -> Result<FriendRequest, SdkError> {
        let raw: crate::generated::FriendRequestResponse = self
            .post(
                crate::generated::paths::friends::CREATE_FRIEND_REQUEST,
                &crate::generated::CreateFriendRequestRequest { to },
            )
            .await?;
        raw.try_into()
    }

    /// `POST /friends/requests/{id}/accept`.
    pub async fn accept_friend_request(&self, request_id: Uuid) -> Result<Friendship, SdkError> {
        let raw: crate::generated::FriendshipResponse = self
            .post_empty(&super::path(
                crate::generated::paths::friends::ACCEPT_FRIEND_REQUEST,
                &[("id", &request_id.to_string())],
            ))
            .await?;
        raw.try_into()
    }

    /// `DELETE /friends/requests/{id}` — declines an incoming request or
    /// withdraws an outgoing one (the server infers which).
    pub async fn decline_or_withdraw_friend_request(
        &self,
        request_id: Uuid,
    ) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::friends::DECLINE_OR_WITHDRAW_FRIEND_REQUEST,
            &[("id", &request_id.to_string())],
        ))
        .await
    }

    /// `DELETE /friends/{identity_id}`.
    pub async fn remove_friend(&self, identity_id: Uuid) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::friends::REMOVE_FRIEND,
            &[("identity_id", &identity_id.to_string())],
        ))
        .await
    }

    /// `GET /friends/handle/{handle}` — exact-match handle resolution for
    /// the "add friend" flow.
    pub async fn resolve_handle(&self, handle: &str) -> Result<Uuid, SdkError> {
        let response: crate::generated::ResolveHandleResponse = self
            .get(&super::path(
                crate::generated::paths::friends::RESOLVE_HANDLE,
                &[("handle", &urlencoding_path(handle))],
            ))
            .await?;
        Ok(response.identity_id)
    }

    /// `GET /blocks` — the caller's own outgoing blocks only.
    pub async fn blocks(&self) -> Result<Vec<Block>, SdkError> {
        let raw: Vec<crate::generated::BlockListEntry> = self
            .get(crate::generated::paths::blocks::LIST_BLOCKS)
            .await?;
        raw.into_iter().map(Block::try_from).collect()
    }

    /// `POST /blocks`.
    pub async fn block(&self, identity_id: Uuid) -> Result<Block, SdkError> {
        let raw: crate::generated::BlockResponse = self
            .post(
                crate::generated::paths::blocks::CREATE_BLOCK,
                &crate::generated::CreateBlockRequest { identity_id },
            )
            .await?;
        raw.try_into()
    }

    /// `DELETE /blocks/{identity_id}`.
    pub async fn unblock(&self, identity_id: Uuid) -> Result<(), SdkError> {
        self.delete(&super::path(
            crate::generated::paths::blocks::REMOVE_BLOCK,
            &[("identity_id", &identity_id.to_string())],
        ))
        .await
    }

    /// `GET /people/discover` — no query parameters; the caller's own
    /// session is the only input.
    pub async fn discover_people(&self) -> Result<Vec<DiscoveryCandidate>, SdkError> {
        let response: crate::generated::DiscoverPeopleResponse = self
            .get(crate::generated::paths::discovery::DISCOVER_PEOPLE)
            .await?;
        Ok(response.candidates.into_iter().map(Into::into).collect())
    }

    /// `GET /identities/search?q=` — only matches identities that opted
    /// into `discoverable`. An empty/blank `q` returns no results without
    /// a round trip.
    pub async fn search_identities(&self, q: &str) -> Result<Vec<SearchResultIdentity>, SdkError> {
        if q.trim().is_empty() {
            return Ok(Vec::new());
        }
        let response: crate::generated::SearchIdentitiesResponse = self
            .get_query(
                crate::generated::paths::discovery::SEARCH_IDENTITIES,
                &[("q", q)],
            )
            .await?;
        Ok(response.results.into_iter().map(Into::into).collect())
    }

    /// `GET /identities/profiles?ids=` — batched, public-fields-only.
    pub async fn profiles(&self, ids: &[Uuid]) -> Result<Vec<PublicProfile>, SdkError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let joined = ids
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let raw: Vec<crate::generated::PublicProfileResponse> = self
            .get_query(
                crate::generated::paths::identity::LIST_PROFILES,
                &[("ids", &joined)],
            )
            .await?;
        Ok(raw.into_iter().map(Into::into).collect())
    }

    /// `GET /me/history`.
    pub async fn history(&self) -> Result<Vec<HistoryEntry>, SdkError> {
        let raw: Vec<crate::generated::HistoryEntryResponse> = self
            .get(crate::generated::paths::identity::MY_HISTORY)
            .await?;
        raw.into_iter().map(HistoryEntry::try_from).collect()
    }

    /// `GET /identities/{id}/profile` — another identity's full
    /// self-description profile, same exposure level as `GET /me`.
    pub async fn identity_profile(
        &self,
        identity_id: Uuid,
    ) -> Result<PublicIdentityProfile, SdkError> {
        let raw: crate::generated::PublicIdentityProfileResponse = self
            .get(&super::path(
                crate::generated::paths::identity::GET_IDENTITY_PROFILE,
                &[("id", &identity_id.to_string())],
            ))
            .await?;
        raw.try_into()
    }

    /// `GET /me/guild-announcements` (issue #280) — recent
    /// announcement-only channel posts across every guild the caller
    /// currently belongs to.
    pub async fn guild_announcements(&self) -> Result<Vec<GuildAnnouncementAlert>, SdkError> {
        let raw: Vec<crate::generated::GuildAnnouncementAlert> = self
            .get(crate::generated::paths::guilds::LIST_MY_GUILD_ANNOUNCEMENTS)
            .await?;
        raw.into_iter()
            .map(GuildAnnouncementAlert::try_from)
            .collect()
    }

    /// `PUT /me/presence` — publishes the caller's own status. Not
    /// signature-required (high-frequency, self-correcting).
    pub async fn update_presence(
        &self,
        status: PresenceStatus,
        hide_active_in: Option<bool>,
    ) -> Result<Presence, SdkError> {
        let raw: crate::generated::PresenceResponse = self
            .put(
                crate::generated::paths::presence::UPDATE_MY_PRESENCE,
                &crate::generated::UpdatePresenceRequest {
                    status,
                    hide_active_in,
                },
            )
            .await?;
        raw.try_into()
    }

    /// `GET /presence?ids=` — no visibility filtering server-side (#87
    /// tracks adding it); returns exactly what the server returns.
    pub async fn presence_of(&self, ids: &[Uuid]) -> Result<Vec<Presence>, SdkError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let joined = ids
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let raw: Vec<crate::generated::PresenceResponse> = self
            .get_query(
                crate::generated::paths::presence::GET_PRESENCE,
                &[("ids", &joined)],
            )
            .await?;
        raw.into_iter().map(Presence::try_from).collect()
    }
}

/// Minimal path-segment percent-encoding for a display-name handle, which
/// can contain characters unsafe in a URL path segment (spaces, etc.) —
/// mirrors `packages/api-client/src/client.ts::resolveHandle`'s
/// `encodeURIComponent` call without pulling in a full URL-encoding
/// dependency for this one call site.
fn urlencoding_path(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_path_leaves_safe_characters_alone() {
        assert_eq!(urlencoding_path("abc-123_.~"), "abc-123_.~");
    }

    #[test]
    fn urlencoding_path_percent_encodes_a_space() {
        assert_eq!(urlencoding_path("dragon slayer"), "dragon%20slayer");
    }
}
