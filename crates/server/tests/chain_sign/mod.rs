//! Client-side signing for the live tests: seeded identities get an Ed25519 signing key, and each
//! helper builds the `chain_event` object an endpoint needs, signed at the identity's current head.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use avalon_protocol::event_payloads::{
    FriendAcceptedPayload, FriendRemovedPayload, FriendRequestedPayload, GuildMemberAddedPayload,
    GuildMemberRemovedPayload, IdentityPasskeyRevokedPayload, IdentityRecoveryConfiguredPayload,
};
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::{author_sign, event_hash, truncate_to_micros};
use avalon_protocol::identity_id::TestIdentity;
use avalon_protocol::ids::{GlobalId, IdentityId};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

static KEYS: LazyLock<Mutex<HashMap<IdentityId, (ed25519_dalek::SigningKey, Uuid)>>> =
    LazyLock::new(Default::default);

pub fn network_id() -> String {
    std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string())
}

fn server_url() -> String {
    std::env::var("AVALON_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
}

/// Gives a seeded identity an active signing key (server and projection tables) and remembers it.
pub async fn register(pool: &PgPool, who: &TestIdentity) -> Uuid {
    let key_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identity_signing_keys (id, identity_id, public_key) VALUES ($1, $2, $3)",
    )
    .bind(key_id)
    .bind(who.id)
    .bind(who.public_key().to_vec())
    .execute(pool)
    .await
    .expect("failed to seed signing key");
    sqlx::query(
        "INSERT INTO indexer_identity_signing_keys (signing_key_id, identity_id, public_key, added_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(key_id)
    .bind(who.id)
    .bind(who.public_key().to_vec())
    .execute(pool)
    .await
    .expect("failed to seed projected signing key");
    KEYS.lock()
        .unwrap()
        .insert(who.id, (who.signing_key.clone(), key_id));
    key_id
}

pub fn key_of(id: IdentityId) -> (ed25519_dalek::SigningKey, Uuid) {
    KEYS.lock()
        .unwrap()
        .get(&id)
        .cloned()
        .expect("identity was not registered with chain_sign::register")
}

/// The chain head `(seq, hash)` of `id` as the server reports it.
pub async fn head(id: IdentityId) -> (u64, Option<String>) {
    let body: serde_json::Value =
        reqwest::get(format!("{}/identities/{id}/chain-head", server_url()))
            .await
            .expect("chain-head request failed")
            .json()
            .await
            .unwrap();
    (
        body["seq"].as_u64().unwrap(),
        body["head_hash"].as_str().map(str::to_string),
    )
}

pub fn identity_ref(id: IdentityId, verb: &str) -> GlobalId {
    GlobalId::new("identity", &id.to_string(), "self", verb)
}

pub fn guild_ref(id: Uuid, verb: &str) -> GlobalId {
    GlobalId::new("guild", &id.to_string(), "self", verb)
}

/// Signs `kind` as `author` at their current head and returns the `chain_event` request object.
pub async fn sign(
    author: IdentityId,
    kind: &str,
    issuer: GlobalId,
    subject: GlobalId,
    payload: serde_json::Value,
) -> serde_json::Value {
    let (key, key_id) = key_of(author);
    let (seq, prev) = head(author).await;
    sign_at(&key, key_id, kind, issuer, subject, payload, seq + 1, prev).await
}

#[allow(clippy::too_many_arguments)]
pub async fn sign_at(
    key: &ed25519_dalek::SigningKey,
    key_id: Uuid,
    kind: &str,
    issuer: GlobalId,
    subject: GlobalId,
    payload: serde_json::Value,
    seq: u64,
    prev: Option<String>,
) -> serde_json::Value {
    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer,
        subject,
        payload,
        timestamp: truncate_to_micros(OffsetDateTime::now_utc()),
        version: 1,
        identity_chain: Some(IdentityChainPosition::current(seq, prev.clone())),
    };
    author_sign(&mut event, &network_id(), key_id, key).unwrap();
    let _ = event_hash(&event);
    serde_json::json!({
        "event_id": event.id,
        "timestamp": event.timestamp.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "seq": seq,
        "prev_hash": prev,
        "signing_key_id": key_id,
        "signature": event.identity_chain.unwrap().signature,
    })
}

fn ordered_pair(x: IdentityId, y: IdentityId) -> (IdentityId, IdentityId) {
    if x < y {
        (x, y)
    } else {
        (y, x)
    }
}

pub async fn friend_requested(from: IdentityId, to: IdentityId) -> serde_json::Value {
    sign(
        from,
        "friend.requested",
        identity_ref(from, "friend_requested"),
        identity_ref(to, "friend_requested"),
        serde_json::to_value(FriendRequestedPayload {
            from,
            to,
            actor: from,
        })
        .unwrap(),
    )
    .await
}

/// `actor` accepts the request `from` -> `to` (actor is `to`).
pub async fn friend_accepted(
    actor: IdentityId,
    from: IdentityId,
    to: IdentityId,
) -> serde_json::Value {
    sign(
        actor,
        "friend.accepted",
        identity_ref(actor, "friend_accepted"),
        identity_ref(from, "friend_accepted"),
        serde_json::to_value(FriendAcceptedPayload { from, to, actor }).unwrap(),
    )
    .await
}

pub async fn friend_removed(actor: IdentityId, other: IdentityId) -> serde_json::Value {
    let (a, b) = ordered_pair(actor, other);
    sign(
        actor,
        "friend.removed",
        identity_ref(actor, "friend_removed"),
        identity_ref(other, "friend_removed"),
        serde_json::to_value(FriendRemovedPayload { a, b, actor }).unwrap(),
    )
    .await
}

/// `actor` adds `member` to the guild (`via`: "join", "invite" or "join_request").
pub async fn guild_member_added(
    actor: IdentityId,
    guild_id: Uuid,
    member: IdentityId,
    role_index: i32,
    via: &str,
) -> serde_json::Value {
    sign(
        actor,
        "guild.member_added",
        identity_ref(actor, "guild_member_added"),
        guild_ref(guild_id, "guild_member_added"),
        serde_json::to_value(GuildMemberAddedPayload {
            guild_id,
            identity_id: member,
            role_index,
            via: via.to_string(),
            actor,
        })
        .unwrap(),
    )
    .await
}

/// The default member role index a plain join takes.
pub const MEMBER_ROLE_INDEX: i32 = 2;

pub async fn joined(identity: IdentityId, guild_id: Uuid, via: &str) -> serde_json::Value {
    guild_member_added(identity, guild_id, identity, MEMBER_ROLE_INDEX, via).await
}

pub async fn guild_member_removed(
    actor: IdentityId,
    guild_id: Uuid,
    member: IdentityId,
    reason: &str,
) -> serde_json::Value {
    sign(
        actor,
        "guild.member_removed",
        identity_ref(actor, "guild_member_removed"),
        guild_ref(guild_id, "guild_member_removed"),
        serde_json::to_value(GuildMemberRemovedPayload {
            guild_id,
            identity_id: member,
            reason: reason.to_string(),
            actor,
        })
        .unwrap(),
    )
    .await
}

pub async fn passkey_revoked(identity: IdentityId, passkey_id: Uuid) -> serde_json::Value {
    sign(
        identity,
        "identity.passkey_revoked",
        identity_ref(identity, "passkey_revoked"),
        identity_ref(identity, "passkey_revoked"),
        serde_json::to_value(IdentityPasskeyRevokedPayload {
            passkey_id,
            identity_id: identity,
        })
        .unwrap(),
    )
    .await
}

pub async fn profile_updated(
    identity: IdentityId,
    changed: serde_json::Value,
) -> serde_json::Value {
    sign(
        identity,
        "profile.updated",
        identity_ref(identity, "profile_updated"),
        identity_ref(identity, "profile_updated"),
        changed,
    )
    .await
}

pub async fn recovery_configured(
    identity: IdentityId,
    mut guardian_ids: Vec<IdentityId>,
    threshold: i32,
) -> serde_json::Value {
    guardian_ids.sort();
    guardian_ids.dedup();
    sign(
        identity,
        "identity.recovery_configured",
        identity_ref(identity, "recovery_configured"),
        identity_ref(identity, "recovery_configured"),
        serde_json::to_value(IdentityRecoveryConfiguredPayload {
            guardian_ids,
            threshold,
        })
        .unwrap(),
    )
    .await
}

/// The `PATCH /me` body `body` plus the `chain_event` for the `profile.updated` it produces.
pub async fn profile_patch(identity: IdentityId, mut body: serde_json::Value) -> serde_json::Value {
    const NULLABLE: [&str; 9] = [
        "avatar_url",
        "bio",
        "pronouns",
        "banner_url",
        "status",
        "timezone",
        "theme_color",
        "location",
        "main_guild",
    ];
    let mut payload = serde_json::Map::new();
    for (key, value) in body.as_object().unwrap() {
        match key.as_str() {
            "display_name" | "favorite_genres" | "links" => {
                payload.insert(key.clone(), value.clone());
            }
            k if NULLABLE.contains(&k) => {
                let cleared = value.as_str() == Some("");
                payload.insert(
                    key.clone(),
                    if cleared {
                        serde_json::Value::Null
                    } else {
                        value.clone()
                    },
                );
            }
            _ => {}
        }
    }
    if !payload.is_empty() {
        body["chain_event"] = profile_updated(identity, payload.into()).await;
    }
    body
}

/// The `POST /guilds/{id}/leave` body; `clears_main_guild` adds the signed main-guild clear first.
pub async fn leave_body(
    identity: IdentityId,
    guild_id: Uuid,
    clears_main_guild: bool,
) -> serde_json::Value {
    let (key, key_id) = key_of(identity);
    let (seq, prev) = head(identity).await;
    let mut body = serde_json::Map::new();
    let (mut next_seq, mut next_prev) = (seq + 1, prev);
    if clears_main_guild {
        let (clear, hash) = sign_hashed(
            &key,
            key_id,
            "profile.updated",
            identity_ref(identity, "profile_updated"),
            identity_ref(identity, "profile_updated"),
            serde_json::json!({ "main_guild": null }),
            next_seq,
            next_prev,
        );
        body.insert("clear_main_guild_chain_event".into(), clear);
        next_seq += 1;
        next_prev = Some(hash);
    }
    let removed = sign_hashed(
        &key,
        key_id,
        "guild.member_removed",
        identity_ref(identity, "guild_member_removed"),
        guild_ref(guild_id, "guild_member_removed"),
        serde_json::to_value(GuildMemberRemovedPayload {
            guild_id,
            identity_id: identity,
            reason: "left".to_string(),
            actor: identity,
        })
        .unwrap(),
        next_seq,
        next_prev,
    )
    .0;
    body.insert("chain_event".into(), removed);
    body.into()
}

/// The `DELETE /guilds/{id}/members/{who}` body.
pub async fn remove_member_body(
    actor: IdentityId,
    guild_id: Uuid,
    member: IdentityId,
) -> serde_json::Value {
    serde_json::json!({ "chain_event": guild_member_removed(actor, guild_id, member, "removed").await })
}

/// Like [`sign_at`] but also returns the event hash, so a follow-up event can chain onto it.
#[allow(clippy::too_many_arguments)]
fn sign_hashed(
    key: &ed25519_dalek::SigningKey,
    key_id: Uuid,
    kind: &str,
    issuer: GlobalId,
    subject: GlobalId,
    payload: serde_json::Value,
    seq: u64,
    prev: Option<String>,
) -> (serde_json::Value, String) {
    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer,
        subject,
        payload,
        timestamp: truncate_to_micros(OffsetDateTime::now_utc()),
        version: 1,
        identity_chain: Some(IdentityChainPosition::current(seq, prev.clone())),
    };
    author_sign(&mut event, &network_id(), key_id, key).unwrap();
    let hash = hex::encode(event_hash(&event).unwrap());
    let json = serde_json::json!({
        "event_id": event.id,
        "timestamp": event.timestamp.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "seq": seq,
        "prev_hash": prev,
        "signing_key_id": key_id,
        "signature": event.identity_chain.unwrap().signature,
    });
    (json, hash)
}

/// Removes a seeded identity's signing keys, for tests of the "no registered key" paths.
pub async fn unregister(pool: &PgPool, id: IdentityId) {
    sqlx::query("DELETE FROM identity_signing_keys WHERE identity_id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("failed to remove signing key");
    sqlx::query("DELETE FROM indexer_identity_signing_keys WHERE identity_id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("failed to remove projected signing key");
    KEYS.lock().unwrap().remove(&id);
}
