//! Glue between [`crate::events::ProtocolEvent`] and the pure rule in
//! [`crate::identity_chain`]: which identity's chain an event belongs to,
//! how its chain position rides inside a ledger payload, its content hash,
//! and the recovery-aware replay every node runs over an identity's events.
//!
//! No I/O. Every node derives the same values from the same event bytes.

use std::collections::BTreeSet;

use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::events::{IdentityChainPosition, ProtocolEvent, ProtocolEventKind};
use crate::identity_chain::{
    apply_chain, clamp_timestamp, compute_event_hash, ActionClass, ChainOutcome, ChainedEvent,
    ClockSkewBounds, EventAuthority, EventHash, TimestampError,
};

/// Reserved top-level payload key a chain position is stored under inside a
/// ledger entry, so the position is covered by the entry hash and travels
/// with the entry through every mirror without a wire-format change.
pub const PAYLOAD_KEY: &str = "_identity_chain";

/// Why an event could not be turned into a [`ChainedEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainEventError {
    NotChained,
    Timestamp(TimestampError),
    MalformedPrevHash,
}

/// Returns `payload` with `position` embedded under [`PAYLOAD_KEY`]. A
/// non-object payload is returned unchanged.
pub fn embed_position(payload: &Value, position: &IdentityChainPosition) -> Value {
    let mut out = payload.clone();
    if let Value::Object(map) = &mut out {
        map.insert(
            PAYLOAD_KEY.to_string(),
            serde_json::to_value(position).expect("IdentityChainPosition should serialize"),
        );
    }
    out
}

/// Splits an embedded position back out of a stored payload.
pub fn split_position(mut payload: Value) -> (Value, Option<IdentityChainPosition>) {
    let position = match &mut payload {
        Value::Object(map) => map
            .remove(PAYLOAD_KEY)
            .and_then(|v| serde_json::from_value(v).ok()),
        _ => None,
    };
    (payload, position)
}

/// The identity whose chain `event` belongs to, or `None` when the kind is
/// unchained or the relevant `GlobalId` is not in the `identity` namespace.
pub fn chain_owner(event: &ProtocolEvent) -> Option<Uuid> {
    ActionClass::classify(&ProtocolEventKind::from(event.kind.as_str()))?;
    let id = match event.kind.as_str() {
        "identity.recovery_approved" | "identity.recovery_cancelled" => &event.subject,
        _ => &event.issuer,
    };
    let mut parts = id.as_str().splitn(4, ':');
    if parts.next()? != "identity" {
        return None;
    }
    parts.next()?.parse().ok()
}

/// How the event's authority is classified for the conflict rule.
pub fn authority_of(kind: &str) -> EventAuthority {
    match kind {
        "identity.signing_key_added" | "identity.signing_key_revoked" => {
            EventAuthority::OwnerSigned
        }
        _ => EventAuthority::AuthenticatedSession,
    }
}

/// Timestamps are hashed at microsecond precision, the resolution the
/// ledger's timestamp column preserves.
pub fn truncate_to_micros(ts: OffsetDateTime) -> OffsetDateTime {
    let nanos = ts.unix_timestamp_nanos();
    OffsetDateTime::from_unix_timestamp_nanos(nanos - nanos.rem_euclid(1000)).unwrap_or(ts)
}

/// Serializes `value` with object keys sorted, recursively.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

pub fn parse_hash(hex_str: &str) -> Option<EventHash> {
    hex::decode(hex_str).ok()?.try_into().ok()
}

/// The chain hash of `event` given its own `identity_chain` position.
pub fn event_hash(event: &ProtocolEvent) -> Result<EventHash, ChainEventError> {
    let position = event
        .identity_chain
        .as_ref()
        .ok_or(ChainEventError::NotChained)?;
    let prev = match &position.prev_hash {
        Some(h) => Some(parse_hash(h).ok_or(ChainEventError::MalformedPrevHash)?),
        None => None,
    };
    Ok(compute_event_hash(
        &event.kind,
        event.issuer.as_str(),
        event.subject.as_str(),
        &canonical_json(&event.payload),
        truncate_to_micros(event.timestamp),
        position.seq,
        prev.as_ref(),
    ))
}

/// Builds the rule's input for `event`, validating its timestamp against
/// the receiving node's clock.
pub fn to_chained_event(
    event: &ProtocolEvent,
    node_now: OffsetDateTime,
    bounds: &ClockSkewBounds,
) -> Result<ChainedEvent, ChainEventError> {
    let class = ActionClass::classify(&ProtocolEventKind::from(event.kind.as_str()))
        .ok_or(ChainEventError::NotChained)?;
    let position = event
        .identity_chain
        .as_ref()
        .ok_or(ChainEventError::NotChained)?;
    let prev_hash = match &position.prev_hash {
        Some(h) => Some(parse_hash(h).ok_or(ChainEventError::MalformedPrevHash)?),
        None => None,
    };
    let timestamp = clamp_timestamp(truncate_to_micros(event.timestamp), node_now, bounds)
        .map_err(ChainEventError::Timestamp)?;
    Ok(ChainedEvent {
        seq: position.seq,
        prev_hash,
        event_hash: event_hash(event)?,
        timestamp,
        class,
        authority: authority_of(&event.kind),
    })
}

/// Replays an identity's events like [`apply_chain`], except that a fork is
/// resolved when exactly one `identity.recovered` event (guardian-approved,
/// its hash listed in `recovery_hashes`) extends the last unforked head at
/// the fork position: it is then the only candidate there.
pub fn resolve_identity_chain(
    events: Vec<ChainedEvent>,
    recovery_hashes: &BTreeSet<EventHash>,
) -> ChainOutcome {
    let mut events = events;
    loop {
        let outcome = apply_chain(events.clone());
        let Some(fork_seq) = outcome.forked_at else {
            return outcome;
        };
        let head = outcome.head().map(|e| e.event_hash);
        let resolvers: Vec<EventHash> = events
            .iter()
            .filter(|e| {
                e.seq == fork_seq && e.prev_hash == head && recovery_hashes.contains(&e.event_hash)
            })
            .map(|e| e.event_hash)
            .collect();
        let [winner] = resolvers.as_slice() else {
            return outcome;
        };
        events.retain(|e| !(e.seq == fork_seq && e.prev_hash == head && e.event_hash != *winner));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::GlobalId;
    use serde_json::json;

    fn event(kind: &str, issuer_id: Uuid, seq: u64, prev: Option<String>) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            issuer: GlobalId::new("identity", &issuer_id.to_string(), "self", "x"),
            subject: GlobalId::new("identity", &issuer_id.to_string(), "self", "x"),
            payload: json!({"b": 1, "a": {"z": 1, "y": 2}}),
            timestamp: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(100),
            version: 1,
            identity_chain: Some(IdentityChainPosition {
                seq,
                prev_hash: prev,
            }),
        }
    }

    #[test]
    fn position_round_trips_through_payload() {
        let pos = IdentityChainPosition {
            seq: 3,
            prev_hash: Some("ab".repeat(32)),
        };
        let embedded = embed_position(&json!({"k": 1}), &pos);
        let (payload, back) = split_position(embedded);
        assert_eq!(payload, json!({"k": 1}));
        assert_eq!(back, Some(pos));
    }

    #[test]
    fn hash_ignores_key_order_and_sub_microsecond_time() {
        let id = Uuid::new_v4();
        let a = event("profile.updated", id, 1, None);
        let mut b = a.clone();
        b.payload = json!({"a": {"y": 2, "z": 1}, "b": 1});
        b.timestamp += time::Duration::nanoseconds(500);
        assert_eq!(event_hash(&a).unwrap(), event_hash(&b).unwrap());
    }

    #[test]
    fn owner_is_subject_for_guardian_recovery_events_else_issuer() {
        let id = Uuid::new_v4();
        let guardian = Uuid::new_v4();
        let mut e = event("identity.recovery_approved", guardian, 1, None);
        e.subject = GlobalId::new("identity", &id.to_string(), "self", "x");
        assert_eq!(chain_owner(&e), Some(id));
        let e = event("profile.updated", id, 1, None);
        assert_eq!(chain_owner(&e), Some(id));
        let e = event("achievement.issued", id, 1, None);
        assert_eq!(chain_owner(&e), None);
    }

    #[test]
    fn far_future_timestamp_is_rejected() {
        let e = event("profile.updated", Uuid::new_v4(), 1, None);
        let now = e.timestamp - time::Duration::hours(1);
        assert_eq!(
            to_chained_event(&e, now, &ClockSkewBounds::default()),
            Err(ChainEventError::Timestamp(TimestampError::TooFarInFuture))
        );
    }

    #[test]
    fn recovered_event_resolves_a_fork() {
        let id = Uuid::new_v4();
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let b = ClockSkewBounds::default();
        let a = event("identity.signing_key_added", id, 1, None);
        let mut a2 = event("identity.signing_key_revoked", id, 1, None);
        a2.payload = json!({"other": 1});
        let mut r = event("identity.recovered", id, 1, None);
        r.payload = json!({"r": 1});
        let ca = to_chained_event(&a, now, &b).unwrap();
        let ca2 = to_chained_event(&a2, now, &b).unwrap();
        let cr = to_chained_event(&r, now, &b).unwrap();
        let forked = resolve_identity_chain(vec![ca.clone(), ca2.clone()], &BTreeSet::new());
        assert!(forked.is_forked());
        let hashes = BTreeSet::from([cr.event_hash]);
        let resolved = resolve_identity_chain(vec![ca, ca2, cr.clone()], &hashes);
        assert!(!resolved.is_forked());
        assert_eq!(resolved.head().unwrap().event_hash, cr.event_hash);
    }
}
