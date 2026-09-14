//! Friends and presence — capability-gated reads/writes on [`crate::Session`]
//! (issue #17).
//!
//! Every method here calls [`crate::Session::require`] with its exact
//! capability string *before* making any request — a `Session` with no
//! grants (which is every `Session` today; see `crate::AvalonClient::authenticate`)
//! correctly rejects without ever touching the network. The server enforces
//! the same capabilities again once #26–#28 land; this check is a
//! convenience for integrator developers, not the security boundary.
//!
//! ## `update_presence` deviates from issue #17's original design
//!
//! The issue describes presence *publishing* as `AvalonClient::publish_presence(identity_id,
//! status)`, gated on an integrator credential and an active `GameBinding` (#83).
//! None of that exists in this repo yet — there is no `GameCredential`, no
//! `GameBinding`, no capability-grant system. What #16 actually built is
//! `PUT /me/presence`: an *identity*, under their own session, publishing their
//! own status. It has no `playing` field (no integrator can attribute that claim
//! to itself yet) and cannot target another identity. So this module exposes
//! `Session::update_presence` instead — matching what the server actually
//! does — rather than an integrator-authority method the server has no endpoint
//! for. Revisit once #26/#28/#83 land and a real integrator-side publish path
//! exists.
//!
//! ## `presence_of` has no visibility filtering yet
//!
//! The server performs no friends/guild/private scoping on `GET /presence`
//! (deferred to #87) — any valid session can look up presence for any ids it
//! names. This method returns exactly what the server returns; it does not
//! (and cannot) narrow that further on the client side, since narrowing
//! client-side would just be security theater on top of a server that
//! doesn't enforce it.

use std::collections::HashMap;

use avalon_protocol::ids::IdentityId;
use avalon_protocol::permissions::Capability;
use avalon_protocol::social::{Friendship, Presence, PresenceStatus};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::{SdkError, Session};

/// A friend, from this game's point of view.
///
/// `display_name` is always `None` today: `GET /friends` returns only the
/// two identity ids and the friendship's `since` timestamp (see
/// `crates/server/src/friends.rs`) — no endpoint resolves another identity's
/// profile yet. Filling this in needs a profile-lookup endpoint, which
/// doesn't exist; tracked as a documented gap here rather than guessed at.
#[derive(Debug, Clone)]
pub struct Friend {
    pub identity_id: IdentityId,
    pub display_name: Option<String>,
    /// Populated only if `presence.read` is also granted alongside
    /// `friends.read` — otherwise always `None`, so an integrator with only
    /// `friends.read` gets names (once resolvable) and nothing else.
    pub presence: Option<Presence>,
}

/// Builds a [`Friend`] view from a raw [`Friendship`] plus whatever presence
/// data is available for the *other* party. Kept as a free function, testable
/// without any HTTP call: an empty `presence_by_id` (which is exactly what
/// callers pass when `presence.read` isn't granted) always yields
/// `presence: None`.
fn merge_friend(
    friendship: &Friendship,
    self_id: IdentityId,
    presence_by_id: &HashMap<IdentityId, Presence>,
) -> Friend {
    let other = if friendship.a == self_id {
        friendship.b
    } else {
        friendship.a
    };
    Friend {
        identity_id: other,
        display_name: None,
        presence: presence_by_id.get(&other).cloned(),
    }
}

#[derive(Serialize)]
struct UpdatePresenceRequest {
    status: PresenceStatus,
}

/// What `presence::presence_ws` (`crates/server/src/presence.rs`, issue
/// #136) accepts from a connected client. Only one variant exists today;
/// kept as a tagged enum (`{"type":"subscribe","ids":[...]}`) rather than a
/// bare struct so a second client-to-server message kind can be added later
/// without a wire-format break.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PresenceSubscribeMessage {
    Subscribe { ids: Vec<IdentityId> },
}

/// `server_url` is `http(s)://…`; the websocket endpoint needs `ws(s)://…`.
/// A plain scheme swap, not a full URL library round-trip — kept as a free
/// function, testable without any network call.
fn websocket_url(server_url: &str, path: &str) -> String {
    if let Some(rest) = server_url.strip_prefix("https://") {
        format!("wss://{rest}{path}")
    } else if let Some(rest) = server_url.strip_prefix("http://") {
        format!("ws://{rest}{path}")
    } else {
        format!("{server_url}{path}")
    }
}

impl Session {
    /// `GET /friends` — requires `friends.read`. Also embeds each friend's
    /// [`Presence`] when `presence.read` is granted too, via a single batched
    /// `presence_of` call rather than one request per friend.
    pub async fn friends(&self) -> Result<Vec<Friend>, SdkError> {
        self.require(Capability::FriendsRead)?;

        let response = self
            .http
            .get(format!("{}/friends", self.server_url))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(SdkError::ServerError(response.status()));
        }
        let friendships: Vec<Friendship> = response.json().await?;

        let presence_by_id =
            if self.require(Capability::PresenceRead).is_ok() && !friendships.is_empty() {
                let other_ids: Vec<IdentityId> = friendships
                    .iter()
                    .map(|f| if f.a == self.identity().id { f.b } else { f.a })
                    .collect();
                self.presence_of(&other_ids)
                    .await?
                    .into_iter()
                    .map(|p| (p.identity_id, p))
                    .collect()
            } else {
                HashMap::new()
            };

        Ok(friendships
            .iter()
            .map(|f| merge_friend(f, self.identity().id, &presence_by_id))
            .collect())
    }

    /// The calling user's own presence, as the server currently has it.
    /// Requires `presence.read`.
    pub async fn presence(&self) -> Result<Presence, SdkError> {
        self.require(Capability::PresenceRead)?;
        let mine = self.presence_of(&[self.identity().id]).await?;
        // The store always answers for any id (missing/stale reads as
        // Offline — see crates/server/src/presence.rs), so this is always
        // populated; `expect` documents that invariant rather than masking
        // a real failure behind a made-up default.
        Ok(mine
            .into_iter()
            .next()
            .expect("GET /presence?ids=<one id> always returns exactly one entry"))
    }

    /// `GET /presence?ids=…` for the given identities. Requires
    /// `presence.read`. No visibility filtering — see module docs.
    pub async fn presence_of(&self, ids: &[IdentityId]) -> Result<Vec<Presence>, SdkError> {
        self.require(Capability::PresenceRead)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids_param = ids
            .iter()
            .map(|id| id.0.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let response = self
            .http
            .get(format!("{}/presence", self.server_url))
            .query(&[("ids", ids_param)])
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(SdkError::ServerError(response.status()));
        }
        Ok(response.json().await?)
    }

    /// `PUT /me/presence` — an identity publishing their own status. See the
    /// module-level docs for why this lives here rather than as
    /// `AvalonClient::publish_presence`. Not capability-gated: the server
    /// requires only a valid identity session for this, matching what's
    /// implemented, not the integrator-credential design #17 originally
    /// described.
    pub async fn update_presence(&self, status: PresenceStatus) -> Result<(), SdkError> {
        let response = self
            .http
            .put(format!("{}/me/presence", self.server_url))
            .bearer_auth(&self.token)
            .json(&UpdatePresenceRequest { status })
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(SdkError::ServerError(response.status()));
        }
        Ok(())
    }

    /// Subscribes to live presence updates for `ids` — issue #136, additive
    /// to `presence_of`'s point-in-time reads, not a replacement for them.
    /// Requires `presence.read`, same as every other presence method here.
    ///
    /// Connects to `GET /ws/presence` (auth via a `?token=` query parameter
    /// — a websocket handshake can't carry a bearer header, see
    /// `crates/server/src/handlers.rs::authenticate_token`'s own doc
    /// comment), sends one `subscribe` message for `ids`, then spawns a
    /// background task forwarding every [`Presence`] the server pushes into
    /// the returned channel. Dropping the receiver drops the sender on the
    /// task's next send attempt, which ends the task — no separate
    /// `unsubscribe` call is needed.
    pub async fn subscribe_presence(
        &self,
        ids: &[IdentityId],
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Presence>, SdkError> {
        self.require(Capability::PresenceRead)?;

        let url = websocket_url(
            &self.server_url,
            &format!("/ws/presence?token={}", self.token),
        );
        let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| SdkError::WebSocket(e.to_string()))?;
        let (mut write, mut read) = ws_stream.split();

        let subscribe =
            serde_json::to_string(&PresenceSubscribeMessage::Subscribe { ids: ids.to_vec() })
                .expect("PresenceSubscribeMessage always serializes");
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
                let Ok(presence) = serde_json::from_str::<Presence>(&text) else {
                    continue;
                };
                if tx.send(presence).is_err() {
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
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn make_friendship(a: Uuid, b: Uuid) -> Friendship {
        Friendship {
            a: IdentityId(a),
            b: IdentityId(b),
            since: OffsetDateTime::now_utc(),
        }
    }

    fn make_presence(identity_id: IdentityId) -> Presence {
        Presence {
            identity_id,
            status: PresenceStatus::Online,
            playing: None,
            updated_at: OffsetDateTime::now_utc(),
        }
    }

    /// Builds a `Session` with no live server behind it — every test here
    /// either never makes a request (capability check fails first) or
    /// exercises pure logic (`merge_friend`) directly.
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
    async fn friends_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.friends().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn presence_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.presence().await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn presence_of_without_grant_is_rejected_before_any_request() {
        let session = test_session(vec![]);
        let result = session.presence_of(&[IdentityId(Uuid::new_v4())]).await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }

    #[tokio::test]
    async fn presence_of_with_no_ids_short_circuits_before_any_request() {
        let session = test_session(vec!["presence.read"]);
        let result = session.presence_of(&[]).await;
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn merge_friend_has_no_presence_when_map_is_empty() {
        let self_id = IdentityId(Uuid::new_v4());
        let other_id = IdentityId(Uuid::new_v4());
        let friendship = make_friendship(self_id.0, other_id.0);

        let friend = merge_friend(&friendship, self_id, &HashMap::new());

        assert_eq!(friend.identity_id, other_id);
        assert!(friend.presence.is_none());
    }

    #[test]
    fn merge_friend_picks_up_presence_when_present_in_map() {
        let self_id = IdentityId(Uuid::new_v4());
        let other_id = IdentityId(Uuid::new_v4());
        let friendship = make_friendship(self_id.0, other_id.0);
        let mut presence_by_id = HashMap::new();
        presence_by_id.insert(other_id, make_presence(other_id));

        let friend = merge_friend(&friendship, self_id, &presence_by_id);

        assert_eq!(friend.identity_id, other_id);
        assert!(friend.presence.is_some());
    }

    #[test]
    fn merge_friend_resolves_the_other_party_regardless_of_a_b_order() {
        let self_id = IdentityId(Uuid::new_v4());
        let other_id = IdentityId(Uuid::new_v4());

        // self as `a`
        let friendship_1 = make_friendship(self_id.0, other_id.0);
        assert_eq!(
            merge_friend(&friendship_1, self_id, &HashMap::new()).identity_id,
            other_id
        );

        // self as `b`
        let friendship_2 = make_friendship(other_id.0, self_id.0);
        assert_eq!(
            merge_friend(&friendship_2, self_id, &HashMap::new()).identity_id,
            other_id
        );
    }

    #[test]
    fn websocket_url_swaps_http_scheme_for_ws() {
        assert_eq!(
            websocket_url("http://127.0.0.1:8080", "/ws/presence?token=abc"),
            "ws://127.0.0.1:8080/ws/presence?token=abc"
        );
    }

    #[test]
    fn websocket_url_swaps_https_scheme_for_wss() {
        assert_eq!(
            websocket_url("https://avalon.example", "/ws/presence?token=abc"),
            "wss://avalon.example/ws/presence?token=abc"
        );
    }

    #[tokio::test]
    async fn subscribe_presence_without_grant_is_rejected_before_any_connection() {
        let session = test_session(vec![]);
        let result = session
            .subscribe_presence(&[IdentityId(Uuid::new_v4())])
            .await;
        assert!(matches!(result, Err(SdkError::CapabilityNotGranted(_))));
    }
}
