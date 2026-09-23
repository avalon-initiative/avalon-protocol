//! Derives an integrator's `shard_settlement` keys from mirrored core-shard
//! ledger events (`game.registered`, `issuer.key_added`, `issuer.key_revoked`).
//!
//! Every mirrored entry was inclusion-verified against a signature-checked
//! tree head from the pinned network key before it was stored, so keys derived
//! here are rooted at that pinned key. Entries whose payload has been pruned
//! cannot contribute; a pruned `game.registered` yields no keys at all because
//! the owner's category and id are unknown.

use avalon_chain::mirror::MirroredEntry;
use avalon_protocol::event_payloads::{
    GameRegisteredPayload, IssuerKeyAddedPayload, IssuerKeyRevokedPayload,
};
use avalon_protocol::integrators::KeyPurpose;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ed25519_dalek::VerifyingKey;
use sqlx::PgPool;
use uuid::Uuid;

const KIND_REGISTERED: &str = "game.registered";
const KIND_KEY_ADDED: &str = "issuer.key_added";
const KIND_KEY_REVOKED: &str = "issuer.key_revoked";

/// One core-shard ledger entry, reduced to what derivation reads.
#[derive(Debug, Clone)]
pub struct CoreEvent {
    pub seq: i64,
    pub kind: String,
    /// `None` when the payload has been pruned.
    pub payload: Option<serde_json::Value>,
}

impl From<&MirroredEntry> for CoreEvent {
    fn from(entry: &MirroredEntry) -> Self {
        CoreEvent {
            seq: entry.seq,
            kind: entry.kind.clone(),
            payload: entry.payload.clone(),
        }
    }
}

/// What the core ledger says about one integrator's shard authority.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MirroredShardAuthority {
    /// Registered display name; `None` when no registration is visible.
    pub display_name: Option<String>,
    /// Currently valid `shard_settlement` keys, in ledger order, without duplicates.
    pub keys: Vec<VerifyingKey>,
}

/// Derives the authority for integrator `owner` of category `namespace` from
/// `events`, which are processed in `seq` order regardless of input order.
///
/// A key is valid when an `issuer.key_added` with purpose `shard_settlement`
/// for the registered integrator exists and no `issuer.key_revoked` names the
/// same key id; key ids are unique, so a revocation applies whether it sorts
/// before or after the add. `valid_until` is not applied, matching the local
/// `issuer_keys` resolver. Keys of any other purpose are never returned.
pub fn derive_shard_authority(
    events: &[CoreEvent],
    namespace: &str,
    owner: &str,
) -> MirroredShardAuthority {
    let mut ordered: Vec<&CoreEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.seq);

    let registration = ordered.iter().find_map(|e| {
        if e.kind != KIND_REGISTERED {
            return None;
        }
        let p: GameRegisteredPayload = serde_json::from_value(e.payload.clone()?).ok()?;
        (p.slug == owner && p.category == namespace).then_some(p)
    });
    let Some(registration) = registration else {
        return MirroredShardAuthority::default();
    };

    let mut added: Vec<(Uuid, VerifyingKey)> = Vec::new();
    let mut revoked: Vec<Uuid> = Vec::new();
    for event in &ordered {
        let Some(payload) = event.payload.clone() else {
            continue;
        };
        match event.kind.as_str() {
            KIND_KEY_ADDED => {
                let Ok(p) = serde_json::from_value::<IssuerKeyAddedPayload>(payload) else {
                    continue;
                };
                if p.game_id != registration.game_id
                    || p.slug != owner
                    || p.purpose != KeyPurpose::ShardSettlement.as_str()
                {
                    continue;
                }
                let Some(key) = decode_key(&p.public_key) else {
                    continue;
                };
                added.push((p.key_id, key));
            }
            KIND_KEY_REVOKED => {
                let Ok(p) = serde_json::from_value::<IssuerKeyRevokedPayload>(payload) else {
                    continue;
                };
                if p.game_id == registration.game_id && p.slug == owner {
                    revoked.push(p.key_id);
                }
            }
            _ => {}
        }
    }

    let mut keys: Vec<VerifyingKey> = Vec::new();
    for (key_id, key) in added {
        if !revoked.contains(&key_id) && !keys.contains(&key) {
            keys.push(key);
        }
    }
    MirroredShardAuthority {
        display_name: Some(registration.name),
        keys,
    }
}

fn decode_key(b64: &str) -> Option<VerifyingKey> {
    let bytes = BASE64.decode(b64).ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// Reads the mirrored core-shard events for `shard_id`'s owner on
/// `network_id` and derives the authority. Storage errors yield an empty
/// authority.
pub async fn resolve_mirrored_shard_authority(
    pool: &PgPool,
    network_id: &str,
    shard_id: &str,
) -> MirroredShardAuthority {
    let Some((namespace, owner)) = avalon_protocol::shard::shard_authority(shard_id) else {
        return MirroredShardAuthority::default();
    };
    let entries = avalon_chain::mirror::mirrored_entries_by_kinds_and_slug(
        pool,
        network_id,
        avalon_protocol::shard::CORE_SHARD_ID,
        &[KIND_REGISTERED, KIND_KEY_ADDED, KIND_KEY_REVOKED],
        owner,
    )
    .await
    .unwrap_or_default();
    let events: Vec<CoreEvent> = entries.iter().map(CoreEvent::from).collect();
    derive_shard_authority(&events, namespace, owner)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SS: &str = "shard_settlement";

    fn pk(seed: u8) -> (String, VerifyingKey) {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (BASE64.encode(vk.to_bytes()), vk)
    }

    fn registered(seq: i64, id: Uuid, slug: &str, category: &str) -> CoreEvent {
        CoreEvent {
            seq,
            kind: KIND_REGISTERED.to_string(),
            payload: Some(json!({
                "game_id": id, "slug": slug, "name": "Demo", "developer": "d",
                "category": category, "requested_capabilities": [],
                "initial_key": {"key_id": Uuid::new_v4(), "algorithm": "ed25519",
                                "public_key": pk(200).0},
            })),
        }
    }

    fn added(seq: i64, id: Uuid, slug: &str, key_id: Uuid, key: &str, purpose: &str) -> CoreEvent {
        CoreEvent {
            seq,
            kind: KIND_KEY_ADDED.to_string(),
            payload: Some(json!({
                "game_id": id, "slug": slug, "key_id": key_id, "algorithm": "ed25519",
                "public_key": key, "role": "operational", "purpose": purpose,
            })),
        }
    }

    fn revoked(seq: i64, id: Uuid, slug: &str, key_id: Uuid) -> CoreEvent {
        CoreEvent {
            seq,
            kind: KIND_KEY_REVOKED.to_string(),
            payload: Some(json!({
                "game_id": id, "slug": slug, "key_id": key_id,
                "revoked_at": "2026-01-01T00:00:00Z",
            })),
        }
    }

    #[test]
    fn add_then_resolve() {
        let id = Uuid::new_v4();
        let (b, k) = pk(1);
        let events = vec![
            registered(1, id, "wow", "game"),
            added(2, id, "wow", Uuid::new_v4(), &b, SS),
        ];
        let a = derive_shard_authority(&events, "game", "wow");
        assert_eq!(a.keys, vec![k]);
        assert_eq!(a.display_name.as_deref(), Some("Demo"));
    }

    #[test]
    fn add_then_revoke_yields_nothing() {
        let id = Uuid::new_v4();
        let kid = Uuid::new_v4();
        let (b, _) = pk(1);
        let events = vec![
            registered(1, id, "wow", "game"),
            added(2, id, "wow", kid, &b, SS),
            revoked(3, id, "wow", kid),
        ];
        assert!(derive_shard_authority(&events, "game", "wow")
            .keys
            .is_empty());
    }

    #[test]
    fn two_keys_and_duplicate_key_material() {
        let id = Uuid::new_v4();
        let (b1, k1) = pk(1);
        let (b2, k2) = pk(2);
        let events = vec![
            registered(1, id, "wow", "game"),
            added(2, id, "wow", Uuid::new_v4(), &b1, SS),
            added(3, id, "wow", Uuid::new_v4(), &b2, SS),
            added(4, id, "wow", Uuid::new_v4(), &b1, SS),
        ];
        assert_eq!(
            derive_shard_authority(&events, "game", "wow").keys,
            vec![k1, k2]
        );
    }

    #[test]
    fn other_integrator_purpose_and_category_are_excluded() {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let (b1, _) = pk(1);
        let (b2, k2) = pk(2);
        let (b3, _) = pk(3);
        let events = vec![
            registered(1, id, "wow", "game"),
            registered(2, other, "zzz", "game"),
            added(3, other, "zzz", Uuid::new_v4(), &b1, SS),
            added(4, id, "wow", Uuid::new_v4(), &b3, "attestation"),
            added(5, id, "wow", Uuid::new_v4(), &b2, SS),
        ];
        assert_eq!(
            derive_shard_authority(&events, "game", "wow").keys,
            vec![k2]
        );
        assert!(derive_shard_authority(&events, "app", "wow")
            .keys
            .is_empty());
        assert_eq!(derive_shard_authority(&events, "game", "zzz").keys.len(), 1);
    }

    #[test]
    fn pruned_payloads_are_skipped() {
        let id = Uuid::new_v4();
        let (b1, _) = pk(1);
        let (b2, k2) = pk(2);
        let mut pruned = added(2, id, "wow", Uuid::new_v4(), &b1, SS);
        pruned.payload = None;
        let events = vec![
            registered(1, id, "wow", "game"),
            pruned,
            added(3, id, "wow", Uuid::new_v4(), &b2, SS),
        ];
        assert_eq!(
            derive_shard_authority(&events, "game", "wow").keys,
            vec![k2]
        );

        let mut no_reg = registered(1, id, "wow", "game");
        no_reg.payload = None;
        let events = vec![no_reg, added(3, id, "wow", Uuid::new_v4(), &b2, SS)];
        assert_eq!(
            derive_shard_authority(&events, "game", "wow"),
            MirroredShardAuthority::default()
        );
    }

    #[test]
    fn revocation_only_affects_its_own_key_id() {
        let id = Uuid::new_v4();
        let old = Uuid::new_v4();
        let (b1, _) = pk(1);
        let (b2, k2) = pk(2);
        let events = vec![
            registered(1, id, "wow", "game"),
            revoked(2, id, "wow", old),
            added(3, id, "wow", Uuid::new_v4(), &b2, SS),
            added(4, id, "wow", old, &b1, SS),
        ];
        assert_eq!(
            derive_shard_authority(&events, "game", "wow").keys,
            vec![k2]
        );
    }

    #[test]
    fn unordered_input_is_sorted_by_seq() {
        let id = Uuid::new_v4();
        let kid = Uuid::new_v4();
        let (b, _) = pk(1);
        let events = vec![
            revoked(3, id, "wow", kid),
            added(2, id, "wow", kid, &b, SS),
            registered(1, id, "wow", "game"),
        ];
        assert!(derive_shard_authority(&events, "game", "wow")
            .keys
            .is_empty());
    }

    #[test]
    fn sibling_ids_resolve_via_owner() {
        let id = Uuid::new_v4();
        let (b, k) = pk(1);
        let events = vec![
            registered(1, id, "wow", "game"),
            added(2, id, "wow", Uuid::new_v4(), &b, SS),
        ];
        let (ns, owner) = avalon_protocol::shard::shard_authority("game:wow/2").unwrap();
        assert_eq!(derive_shard_authority(&events, ns, owner).keys, vec![k]);
    }
}
