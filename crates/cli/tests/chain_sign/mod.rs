//! Client-side signing for the walkthrough tests: builds the `chain_event` object a chained
//! endpoint needs, signed at the identity's current chain head.
#![allow(dead_code)]

use avalon_protocol::event_payloads::{
    FriendAcceptedPayload, FriendRequestedPayload, GuildMemberAddedPayload,
};
use avalon_protocol::events::{IdentityChainPosition, ProtocolEvent};
use avalon_protocol::identity_chain_wire::{author_sign, truncate_to_micros};
use avalon_protocol::ids::{GlobalId, IdentityId};
use ed25519_dalek::SigningKey;
use time::OffsetDateTime;
use uuid::Uuid;

pub fn network_id() -> String {
    std::env::var("AVALON_NETWORK_ID").unwrap_or_else(|_| "avalon-dev-local".to_string())
}

fn identity_ref(id: IdentityId, verb: &str) -> GlobalId {
    GlobalId::new("identity", &id.to_string(), "self", verb)
}

async fn sign(
    http: &reqwest::Client,
    base: &str,
    network_id: &str,
    author: (IdentityId, &SigningKey, &str),
    kind: &str,
    subject: GlobalId,
    payload: serde_json::Value,
) -> serde_json::Value {
    let (id, key, key_id) = author;
    let key_id: Uuid = key_id.parse().expect("signing key id is a uuid");
    let head: serde_json::Value = http
        .get(format!("{base}/identities/{id}/chain-head"))
        .send()
        .await
        .expect("chain-head request failed")
        .json()
        .await
        .unwrap();
    let seq = head["seq"].as_u64().unwrap() + 1;
    let prev = head["head_hash"].as_str().map(str::to_string);
    let mut event = ProtocolEvent {
        id: Uuid::new_v4(),
        kind: kind.to_string(),
        issuer: identity_ref(id, &kind.replace('.', "_")),
        subject,
        payload,
        timestamp: truncate_to_micros(OffsetDateTime::now_utc()),
        version: 1,
        identity_chain: Some(IdentityChainPosition::current(seq, prev.clone())),
    };
    author_sign(&mut event, network_id, key_id, key).unwrap();
    serde_json::json!({
        "event_id": event.id,
        "timestamp": event.timestamp.format(&time::format_description::well_known::Rfc3339).unwrap(),
        "seq": seq,
        "prev_hash": prev,
        "signing_key_id": key_id,
        "signature": event.identity_chain.unwrap().signature,
    })
}

pub async fn friend_requested(
    http: &reqwest::Client,
    base: &str,
    network_id: &str,
    from: (IdentityId, &SigningKey, &str),
    to: IdentityId,
) -> serde_json::Value {
    let payload = FriendRequestedPayload {
        from: from.0,
        to,
        actor: from.0,
    };
    sign(
        http,
        base,
        network_id,
        from,
        "friend.requested",
        identity_ref(to, "friend_requested"),
        serde_json::to_value(payload).unwrap(),
    )
    .await
}

pub async fn friend_accepted(
    http: &reqwest::Client,
    base: &str,
    network_id: &str,
    actor: (IdentityId, &SigningKey, &str),
    from: IdentityId,
) -> serde_json::Value {
    let payload = FriendAcceptedPayload {
        from,
        to: actor.0,
        actor: actor.0,
    };
    sign(
        http,
        base,
        network_id,
        actor,
        "friend.accepted",
        identity_ref(from, "friend_accepted"),
        serde_json::to_value(payload).unwrap(),
    )
    .await
}

/// `member` joins `guild_id` by accepting an invite.
pub async fn invite_accepted(
    http: &reqwest::Client,
    base: &str,
    network_id: &str,
    member: (IdentityId, &SigningKey, &str),
    guild_id: Uuid,
) -> serde_json::Value {
    let payload = GuildMemberAddedPayload {
        guild_id,
        identity_id: member.0,
        role_index: 2,
        via: "invite".to_string(),
        actor: member.0,
    };
    sign(
        http,
        base,
        network_id,
        member,
        "guild.member_added",
        GlobalId::new("guild", &guild_id.to_string(), "self", "guild_member_added"),
        serde_json::to_value(payload).unwrap(),
    )
    .await
}
