//! Cosigned-STH verification wiring for the server: bridges
//! `crate::known_list`'s witness identities into the
//! `(witness_key_id, VerifyingKey)` shape
//! `avalon_protocol::cosigned_sth::verify_cosigned_tree_head` needs, and
//! the wire format for cosignatures accompanying a fetched Signed Tree
//! Head over `GET /ledger/sth/latest`/`GET /ledger/sth/{tree_size}`.

use avalon_protocol::cosigned_sth::{self, CosignedTreeHead};
use avalon_protocol::sth::SignedTreeHead;
use avalon_protocol::witness::WitnessCosignature;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::known_list::KnownListHandle;

/// How far back a witness cosignature's `observed_at` may be and still
/// count toward majority. Matches `known_list`'s own default freshness
/// window (a witness this stale is already dropped from the known list
/// itself) — this mostly guards against a cosignature relayed well after
/// the witness that produced it went stale, or a clock-skewed/forged
/// timestamp, exactly as `cosigned_sth::verify_cosigned_tree_head`'s own
/// doc comment describes.
pub const COSIGNATURE_FRESHNESS_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// Bridges [`KnownListHandle`]'s confirmed witness identities into the
/// `(witness_key_id, VerifyingKey)` pairs
/// `cosigned_sth::verify_cosigned_tree_head` needs.
///
/// A slot's `witness_key_id` is the hex verifying key its peer proved
/// possession of, so it decodes directly; an id that does not decode as a key
/// yields no pair. An empty or partially-undecoded known list falls into
/// `verify_cosigned_tree_head`'s `len() <= 1` degenerate case, i.e. the plain
/// single-author-signature check.
pub fn known_list_verifying_keys(handle: &KnownListHandle) -> Vec<(String, VerifyingKey)> {
    handle
        .confirmed_witness_key_ids()
        .into_iter()
        .filter_map(|witness_key_id| {
            let key = parse_hex_verifying_key(&witness_key_id)?;
            Some((witness_key_id, key))
        })
        .collect()
}

fn parse_hex_verifying_key(hex_value: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(hex_value).ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// Tries every candidate author key — a shard/network can have more than
/// one currently-valid resolved key (rotation, multiple sources agreeing)
/// — against the same cosigned head, returning the first that verifies.
/// The cosigned equivalent of the
/// `.any(|key| sth::verify_tree_head(key, &sth))` pattern used throughout
/// this crate's mirror/cross-shard verification before cosigning existed;
/// this replaces that pattern at every one of those call sites, it does not
/// sit alongside it — see this module's own callers. Returns the matched
/// key (not just `bool`) since a caller cross-checking two accepted heads
/// for equivocation needs to know which author key both were verified
/// against.
pub fn verify_cosigned_against_any_key(
    candidate_author_keys: impl IntoIterator<Item = VerifyingKey>,
    head: &CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
    now: OffsetDateTime,
) -> Option<VerifyingKey> {
    let freshness_cutoff = now - COSIGNATURE_FRESHNESS_WINDOW;
    candidate_author_keys.into_iter().find(|key| {
        cosigned_sth::verify_cosigned_tree_head(key, head, known_list, freshness_cutoff, now)
    })
}

/// Wire shape for one witness cosignature accompanying a fetched STH —
/// `GET /ledger/sth/latest`'s `cosignatures` field. Deliberately does not
/// repeat `tree_size`/`root_hash`/`network_id`/`author_created_at`: those
/// are already the enclosing STH response's own fields, and every
/// cosignature in the array is, by construction, over that exact head (see
/// [`Self::to_witness_cosignature`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessCosignatureDto {
    pub witness_key_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub signature: String,
}

impl WitnessCosignatureDto {
    /// Reconstructs the full [`WitnessCosignature`] this DTO describes,
    /// binding it to `sth`'s own signed fields — the same binding
    /// `avalon_protocol::witness::witness_signing_message` itself covers.
    pub fn to_witness_cosignature(&self, sth: &SignedTreeHead) -> WitnessCosignature {
        WitnessCosignature {
            tree_size: sth.tree_size,
            root_hash: sth.root_hash.clone(),
            network_id: sth.network_id.clone(),
            author_created_at: sth.created_at,
            witness_key_id: self.witness_key_id.clone(),
            observed_at: self.observed_at,
            signature: self.signature.clone(),
        }
    }

    pub fn from_witness_cosignature(cosig: &WitnessCosignature) -> Self {
        Self {
            witness_key_id: cosig.witness_key_id.clone(),
            observed_at: cosig.observed_at,
            signature: cosig.signature.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_list::{KnownListConfig, KnownListHandle};
    use avalon_protocol::witness::sign_witness_cosignature;
    use ed25519_dalek::SigningKey;
    use std::time::Duration;

    /// A config where a freshly-admitted, non-anchor witness is confirmed
    /// on the very next `tick` — real deployments wait out a real
    /// probation window, but these tests only care about membership
    /// changing live, not about how long that takes.
    fn instant_confirm_config() -> KnownListConfig {
        KnownListConfig {
            capacity: 10,
            anchor_capacity: 0,
            max_per_prefix: 10,
            freshness_window: Duration::from_secs(3600),
            probation_window: Duration::from_secs(0),
        }
    }

    fn witness() -> (SigningKey, String) {
        let key = SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        (key, id)
    }

    fn cosigned_head(
        author_key: &SigningKey,
        tree_size: i64,
        root_hash: &str,
        network_id: &str,
        created_at: OffsetDateTime,
        cosigners: &[(&SigningKey, &str, OffsetDateTime)],
    ) -> CosignedTreeHead {
        let sth = avalon_protocol::sth::sign_tree_head(
            author_key,
            "settlement-operator-1",
            tree_size,
            root_hash,
            network_id,
            created_at,
        );
        let cosignatures = cosigners
            .iter()
            .map(|(key, id, observed_at)| {
                sign_witness_cosignature(
                    key,
                    id,
                    tree_size,
                    root_hash,
                    network_id,
                    created_at,
                    *observed_at,
                )
            })
            .collect();
        CosignedTreeHead { sth, cosignatures }
    }

    fn root(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    #[test]
    fn a_slot_whose_id_is_not_hex_key_material_bridges_to_no_pair() {
        let cfg = instant_confirm_config();
        let handle = KnownListHandle::load_or_new(cfg, None);
        let now = OffsetDateTime::now_utc();
        assert!(handle.try_admit("http://avalon-peer:8080", "prefix-a", false, now));
        handle.tick(&[], now);

        assert!(known_list_verifying_keys(&handle).is_empty());
    }

    #[test]
    fn a_slot_whose_id_is_real_hex_key_material_bridges_correctly() {
        let cfg = instant_confirm_config();
        let handle = KnownListHandle::load_or_new(cfg, None);
        let now = OffsetDateTime::now_utc();
        let (key, id) = witness();
        assert!(handle.try_admit(&id, "prefix-a", false, now));
        handle.tick(&[], now);

        let pairs = known_list_verifying_keys(&handle);
        assert_eq!(pairs, vec![(id, key.verifying_key())]);
    }

    /// Two nodes advertise witness keys with proofs, are admitted to a third
    /// node's peer table, become keyed known-list slots, and their own
    /// cosignatures then meet the majority.
    #[tokio::test]
    async fn two_admitted_nodes_cosignatures_count_toward_a_majority() {
        use crate::nodes::{verified_advert, PeerInfo, PeerTable, WitnessSigner};
        let now = OffsetDateTime::now_utc();
        let table = PeerTable::new();
        let mut signers = Vec::new();
        for (i, url) in ["http://127.0.0.1:9701", "http://127.0.0.1:9702"]
            .into_iter()
            .enumerate()
        {
            let (key, id) = witness();
            let signer = WitnessSigner::new(key.clone(), id.clone()).unwrap();
            let advert = verified_advert(url, Some(signer.advert(url, now)), now).map(|mut a| {
                a.direct = true;
                a
            });
            assert!(advert.is_some());
            table.upsert(PeerInfo {
                base_url: url.to_string(),
                roles: vec!["combined".to_string()],
                protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
                network_id: "avalon-test".to_string(),
                last_announced_at: now,
                libp2p_peer_id: Some(format!("peer-{i}")),
                libp2p_listen_addrs: Vec::new(),
                witness: advert,
            });
            signers.push((key, id));
        }
        // A keyless peer participates in the table but is no candidate.
        table.upsert(PeerInfo {
            base_url: "http://127.0.0.1:9703".to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: "avalon-test".to_string(),
            last_announced_at: now,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
            witness: None,
        });

        let policy = crate::outbound_policy::OutboundPolicy::new(true);
        let candidates =
            crate::known_list::build_candidates(&policy, &table, "avalon-test", now).await;
        assert_eq!(candidates.len(), 2);

        let handle = KnownListHandle::load_or_new(instant_confirm_config(), None);
        handle.tick(&candidates, now);
        handle.tick(&candidates, now);
        let pairs = known_list_verifying_keys(&handle);
        assert_eq!(pairs.len(), 2);

        let author_key = SigningKey::generate(&mut rand::rng());
        let (k0, id0) = &signers[0];
        let (k1, id1) = &signers[1];
        let both = cosigned_head(
            &author_key,
            3,
            &root(2),
            "avalon-test",
            now,
            &[(k0, id0, now), (k1, id1, now)],
        );
        assert!(
            verify_cosigned_against_any_key([author_key.verifying_key()], &both, &pairs, now)
                .is_some()
        );
        let one = cosigned_head(
            &author_key,
            3,
            &root(2),
            "avalon-test",
            now,
            &[(k0, id0, now)],
        );
        assert!(
            verify_cosigned_against_any_key([author_key.verifying_key()], &one, &pairs, now)
                .is_none()
        );
    }

    /// An advert that only arrived by gossip or an inbound announce (an
    /// attacker binding an innocent URL to its own key) is never a candidate.
    #[tokio::test]
    async fn a_gossip_only_advert_never_becomes_a_candidate() {
        use crate::nodes::{verified_advert, PeerInfo, PeerTable, WitnessSigner};
        let now = OffsetDateTime::now_utc();
        let url = "http://127.0.0.1:9711";
        let (key, id) = witness();
        let advert = verified_advert(
            url,
            Some(WitnessSigner::new(key, id).unwrap().advert(url, now)),
            now,
        );
        assert!(advert.is_some());
        let table = PeerTable::new();
        table.upsert(PeerInfo {
            base_url: url.to_string(),
            roles: vec!["combined".to_string()],
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
            network_id: "avalon-test".to_string(),
            last_announced_at: now,
            libp2p_peer_id: None,
            libp2p_listen_addrs: Vec::new(),
            witness: advert,
        });
        let policy = crate::outbound_policy::OutboundPolicy::new(true);
        assert!(
            crate::known_list::build_candidates(&policy, &table, "avalon-test", now)
                .await
                .is_empty()
        );
    }

    /// The ticket's own acceptance scenario: a mirror keeps verifying
    /// correctly across a known-list membership change, with no restart —
    /// reading `handle` live (not a snapshot taken once) is what makes this
    /// work at all.
    #[test]
    fn verification_follows_a_known_list_membership_change_without_a_restart() {
        let cfg = instant_confirm_config();
        let handle = KnownListHandle::load_or_new(cfg, None);
        let now = OffsetDateTime::now_utc();

        let author_key = SigningKey::generate(&mut rand::rng());
        let (w_a, id_a) = witness();
        let (w_b, id_b) = witness();
        let (w_c, id_c) = witness();
        let (w_d, id_d) = witness();

        for id in [&id_a, &id_b, &id_c] {
            assert!(handle.try_admit(id, id, false, now));
        }
        handle.tick(&[], now);
        assert_eq!(known_list_verifying_keys(&handle).len(), 3);

        let network_id = "avalon-test";
        let tree_size = 7;
        let root_hash = root(1);

        // Cosigned by A and B — majority of 3 (threshold 2) at this point.
        let head = cosigned_head(
            &author_key,
            tree_size,
            &root_hash,
            network_id,
            now,
            &[(&w_a, &id_a, now), (&w_b, &id_b, now)],
        );
        assert!(verify_cosigned_against_any_key(
            [author_key.verifying_key()],
            &head,
            &known_list_verifying_keys(&handle),
            now,
        )
        .is_some());

        // Membership changes live: B drops out, D is admitted and
        // confirmed — no restart, same handle.
        assert!(handle.remove(&id_b));
        assert!(handle.try_admit(&id_d, &id_d, false, now));
        handle.tick(&[], now);
        let current_list = known_list_verifying_keys(&handle);
        assert_eq!(current_list.len(), 3);

        // The same head, re-verified against the *current* list: only A
        // (of its two original cosigners) is still known, which is below
        // the majority threshold of 2 — correctly no longer accepted.
        assert!(verify_cosigned_against_any_key(
            [author_key.verifying_key()],
            &head,
            &current_list,
            now,
        )
        .is_none());

        // A fresh head cosigned by the *current* majority (A and D)
        // verifies correctly against the same, live-updated list.
        let head_after_change = cosigned_head(
            &author_key,
            tree_size + 1,
            &root(2),
            network_id,
            now,
            &[(&w_a, &id_a, now), (&w_d, &id_d, now)],
        );
        assert!(verify_cosigned_against_any_key(
            [author_key.verifying_key()],
            &head_after_change,
            &current_list,
            now,
        )
        .is_some());
        let _ = w_c; // kept in the list throughout, never re-cosigns above
    }

    #[test]
    fn a_head_with_an_insufficient_or_forged_cosignature_set_is_rejected() {
        let cfg = instant_confirm_config();
        let handle = KnownListHandle::load_or_new(cfg, None);
        let now = OffsetDateTime::now_utc();
        let author_key = SigningKey::generate(&mut rand::rng());
        let (w_a, id_a) = witness();
        let (w_b, id_b) = witness();
        let (w_c, id_c) = witness();
        for id in [&id_a, &id_b, &id_c] {
            assert!(handle.try_admit(id, id, false, now));
        }
        handle.tick(&[], now);
        let known_list = known_list_verifying_keys(&handle);

        // Insufficient: only one of three known witnesses cosigned
        // (threshold is 2).
        let under_threshold = cosigned_head(
            &author_key,
            1,
            &root(9),
            "avalon-test",
            now,
            &[(&w_a, &id_a, now)],
        );
        assert!(verify_cosigned_against_any_key(
            [author_key.verifying_key()],
            &under_threshold,
            &known_list,
            now,
        )
        .is_none());

        // Forged: a cosignature from a key outside the known list entirely,
        // padded up to a majority-looking count.
        let stranger = SigningKey::generate(&mut rand::rng());
        let forged = cosigned_head(
            &author_key,
            1,
            &root(9),
            "avalon-test",
            now,
            &[(&w_a, &id_a, now), (&stranger, &id_b, now)],
        );
        assert!(verify_cosigned_against_any_key(
            [author_key.verifying_key()],
            &forged,
            &known_list,
            now,
        )
        .is_none());
        let _ = (w_b, w_c, id_c);
    }

    #[test]
    fn dto_round_trips_into_a_witness_cosignature_bound_to_the_enclosing_sth() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::now_utc();
        let sth = avalon_protocol::sth::sign_tree_head(
            &author_key,
            "settlement-operator-1",
            3,
            &root(4),
            "avalon-test",
            now,
        );
        let (witness_key, id) = witness();
        let cosig =
            sign_witness_cosignature(&witness_key, &id, 3, &root(4), "avalon-test", now, now);

        let dto = WitnessCosignatureDto::from_witness_cosignature(&cosig);
        let rebuilt = dto.to_witness_cosignature(&sth);
        assert_eq!(rebuilt, cosig);
    }
}
