//! Confirms a [`crate::nodes::HeadConflict`] gossip surfaced — the second
//! half of head-summary gossip's fork detection: a conflicting
//! pair of summaries is only a signal (two peers claim different roots at
//! the same `shard_id`/`tree_size`); this module fetches full cosignature
//! detail for both sides directly from the peers that reported them
//! (`GET /ledger/sth/{tree_size}?witnesses=1`, `crate::settlement`) and runs
//! `avalon_protocol::cosigned_sth::find_equivocating_witnesses` to turn that
//! signal into a proof.
//!
//! **Equivocation proof is verifiable by anyone from signatures alone.**
//! `witness_key_id` is a witness's own hex-encoded Ed25519 public key
//! (`avalon_protocol::witness::WitnessCosignature`'s own field doc
//! comment) — so the "known list" this confirmation checks majority
//! against is built directly from the keys present in the two fetched
//! heads' cosignatures, not from any node's own trusted witness list. This
//! is deliberate: the proof this module produces must stand on its own,
//! reproducible by any third party from just the two heads' signed bytes.
//!
//! No equivocation-evidence table existed in this schema before this
//! ticket; [`EquivocationEvidence`]/`store_equivocation_evidence` is this
//! own reasonable storage shape. Reconcile with any separately-landed
//! mirror-sync change that independently adds its own.

use avalon_chain::{EquivocationEvidence, PostgresSettlementProvider};
use avalon_protocol::cosigned_sth::{find_equivocating_witnesses, CosignedTreeHead};
use avalon_protocol::witness::WitnessCosignature;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::cross_shard::{pinned_core_verify_key, resolve_shard_verify_keys_from_db};
use crate::nodes::{HeadConflict, HeadGossipTracker};

/// Same freshness window the design doc gives as the default
/// (`docs/projects/backend-server/architecture/witness-cosigning.md`) — how
/// far back a cosignature's `observed_at` may be and still count toward
/// this confirmation's majority check.
const CONFIRMATION_FRESHNESS_WINDOW: time::Duration = time::Duration::minutes(10);

/// Mirrors `crate::settlement::WitnessCosignatureResponse`'s wire shape —
/// duplicated rather than shared, matching this codebase's established
/// "small per-module DTO, not a shared internal type" convention
/// (`crate::cross_shard`'s own `FetchedSth`).
#[derive(Deserialize)]
struct FetchedCosignature {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    witness_key_id: String,
    #[serde(with = "time::serde::rfc3339")]
    author_created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
    signature: String,
}

impl From<FetchedCosignature> for WitnessCosignature {
    fn from(c: FetchedCosignature) -> Self {
        WitnessCosignature {
            tree_size: c.tree_size,
            root_hash: c.root_hash,
            network_id: c.network_id,
            author_created_at: c.author_created_at,
            witness_key_id: c.witness_key_id,
            observed_at: c.observed_at,
            signature: c.signature,
        }
    }
}

#[derive(Deserialize)]
struct FetchedSthWithWitnesses {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(default)]
    cosignatures: Vec<FetchedCosignature>,
}

impl From<FetchedSthWithWitnesses> for CosignedTreeHead {
    fn from(dto: FetchedSthWithWitnesses) -> Self {
        CosignedTreeHead {
            sth: avalon_protocol::sth::SignedTreeHead {
                tree_size: dto.tree_size,
                root_hash: dto.root_hash,
                network_id: dto.network_id,
                signing_key_id: dto.signing_key_id,
                signature: dto.signature,
                created_at: dto.created_at,
            },
            cosignatures: dto.cosignatures.into_iter().map(Into::into).collect(),
        }
    }
}

/// Fetches full cosignature detail for `shard_id` at `tree_size` directly
/// from `base_url` — the fetch-on-demand half of head-summary gossip.
async fn fetch_cosigned_head(
    client: &reqwest::Client,
    base_url: &str,
    shard_id: &str,
    tree_size: i64,
) -> Result<CosignedTreeHead, String> {
    let dto: FetchedSthWithWitnesses = client
        .get(format!("{base_url}/ledger/sth/{tree_size}"))
        .query(&[("shard_id", shard_id), ("witnesses", "1")])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{base_url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("{base_url}: {e}"))?;
    Ok(dto.into())
}

/// The union of witness keys present (as valid hex-encoded Ed25519 public
/// keys — an entry whose `witness_key_id` doesn't parse as one is simply
/// excluded, never panics) in either head's cosignature set — see this
/// module's own doc comment for why this, rather than any node's own known
/// list, is what confirmation checks majority against.
fn known_list_from_cosignatures(
    head_a: &CosignedTreeHead,
    head_b: &CosignedTreeHead,
) -> Vec<(String, VerifyingKey)> {
    let mut list = Vec::new();
    for cosig in head_a.cosignatures.iter().chain(head_b.cosignatures.iter()) {
        if list.iter().any(|(id, _)| *id == cosig.witness_key_id) {
            continue;
        }
        if let Some(key) = parse_witness_verify_key(&cosig.witness_key_id) {
            list.push((cosig.witness_key_id.clone(), key));
        }
    }
    list
}

fn parse_witness_verify_key(witness_key_id: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(witness_key_id).ok()?;
    let array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// Tries each of `candidate_author_keys` (a shard can have more than one
/// currently-authorized `shard_settlement` key on record) until one
/// confirms both heads as validly cosigned and produces a non-empty
/// equivocation proof. `None` if no candidate key confirms anything —
/// either the heads don't actually reach majority under any resolvable
/// author key, or this node simply can't resolve the right key yet.
fn confirm_equivocation(
    candidate_author_keys: &[VerifyingKey],
    head_a: &CosignedTreeHead,
    head_b: &CosignedTreeHead,
    now: OffsetDateTime,
) -> Option<(VerifyingKey, Vec<String>)> {
    let known_list = known_list_from_cosignatures(head_a, head_b);
    let freshness_cutoff = now - CONFIRMATION_FRESHNESS_WINDOW;
    candidate_author_keys.iter().find_map(|key| {
        let equivocators =
            find_equivocating_witnesses(key, &known_list, freshness_cutoff, now, head_a, head_b);
        (!equivocators.is_empty()).then_some((*key, equivocators))
    })
}

fn cosignatures_to_json(cosignatures: &[WitnessCosignature]) -> serde_json::Value {
    serde_json::Value::Array(
        cosignatures
            .iter()
            .map(|c| {
                serde_json::json!({
                    "tree_size": c.tree_size,
                    "root_hash": c.root_hash,
                    "network_id": c.network_id,
                    "witness_key_id": c.witness_key_id,
                    "author_created_at": c.author_created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                    "observed_at": c.observed_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                    "signature": c.signature,
                })
            })
            .collect(),
    )
}

/// Fetches, confirms, and (on success) durably records the equivocation
/// `conflict` signals — spawned from `crate::nodes::announce` and
/// `crate::nodes::run_worker` whenever [`crate::nodes::HeadGossipTracker::merge`]
/// surfaces a new conflict. A no-op if this shard's equivocation is already
/// on record, if either peer can't be reached, or if no resolvable author
/// key confirms an actual majority-vs-majority conflict (a false-positive
/// gossip signal — e.g. a peer that has since moved past the size in
/// question — is not an error, just nothing to prove).
pub async fn confirm_and_record(
    chain: &PostgresSettlementProvider,
    head_gossip: &HeadGossipTracker,
    conflict: HeadConflict,
) {
    if head_gossip.is_equivocating(&conflict.shard_id) {
        return;
    }

    let client = reqwest::Client::new();
    let (head_a, head_b) = tokio::join!(
        fetch_cosigned_head(
            &client,
            &conflict.source_a,
            &conflict.shard_id,
            conflict.tree_size
        ),
        fetch_cosigned_head(
            &client,
            &conflict.source_b,
            &conflict.shard_id,
            conflict.tree_size
        ),
    );
    let (head_a, head_b) = match (head_a, head_b) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            tracing::warn!(
                event = "equivocation_confirmation_fetch_failed",
                shard_id = %conflict.shard_id,
                tree_size = conflict.tree_size,
                error_a = ?a.err(),
                error_b = ?b.err(),
                "could not fetch full cosignature detail for both sides of a gossiped head \
                 conflict",
            );
            return;
        }
    };

    if head_a.sth.root_hash != conflict.root_hash_a || head_b.sth.root_hash != conflict.root_hash_b
    {
        // The reporting peer has since moved on (or the gossip was stale) —
        // nothing left to confirm at this exact pair of roots.
        return;
    }

    let network_id = chain.network_id();
    let candidate_author_keys: Vec<VerifyingKey> =
        pinned_core_verify_key(network_id, &conflict.shard_id)
            .into_iter()
            .chain(
                resolve_shard_verify_keys_from_db(chain.pool(), network_id, &conflict.shard_id)
                    .await,
            )
            .collect();

    let now = OffsetDateTime::now_utc();
    let Some((author_key, equivocators)) =
        confirm_equivocation(&candidate_author_keys, &head_a, &head_b, now)
    else {
        return;
    };

    tracing::error!(
        event = "equivocation_confirmed",
        shard_id = %conflict.shard_id,
        network_id = %network_id,
        tree_size = conflict.tree_size,
        root_hash_a = %head_a.sth.root_hash,
        root_hash_b = %head_b.sth.root_hash,
        equivocating_witnesses = ?equivocators,
        "confirmed a witness equivocation: this shard's log showed two different roots at the \
         same tree_size to different witness groups — no longer trusting either head",
    );

    head_gossip.mark_equivocating(&conflict.shard_id);

    let evidence = EquivocationEvidence {
        network_id: network_id.to_string(),
        shard_id: conflict.shard_id.clone(),
        tree_size: conflict.tree_size,
        root_hash_a: head_a.sth.root_hash.clone(),
        root_hash_b: head_b.sth.root_hash.clone(),
        author_verify_key: hex::encode(author_key.to_bytes()),
        cosignatures_a: cosignatures_to_json(&head_a.cosignatures),
        cosignatures_b: cosignatures_to_json(&head_b.cosignatures),
        equivocating_witness_key_ids: serde_json::Value::Array(
            equivocators
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
        detected_at: now,
    };
    if let Err(e) = chain.store_equivocation_evidence(&evidence).await {
        tracing::error!(
            event = "equivocation_evidence_storage_failed",
            shard_id = %conflict.shard_id,
            error = %e,
            "confirmed an equivocation but failed to durably record it",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::sth;
    use ed25519_dalek::SigningKey;

    fn root_hash_fixture(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    fn head(
        author_key: &SigningKey,
        tree_size: i64,
        root_hash: &str,
        network_id: &str,
        now: OffsetDateTime,
        cosigners: &[(&SigningKey, String)],
    ) -> CosignedTreeHead {
        let author_sth = sth::sign_tree_head(
            author_key,
            "settlement-operator-1",
            tree_size,
            root_hash,
            network_id,
            now,
        );
        let cosignatures = cosigners
            .iter()
            .map(|(key, key_id)| {
                avalon_protocol::witness::sign_witness_cosignature(
                    key, key_id, tree_size, root_hash, network_id, now, now,
                )
            })
            .collect();
        CosignedTreeHead {
            sth: author_sth,
            cosignatures,
        }
    }

    fn witness_fixture() -> (SigningKey, String) {
        let key = SigningKey::generate(&mut rand::rng());
        let key_id = hex::encode(key.verifying_key().to_bytes());
        (key, key_id)
    }

    /// The end-to-end scenario the ticket asks for: a log shows two
    /// different roots at the same `tree_size`, each independently
    /// majority-cosigned by a different (but overlapping) witness group —
    /// mirrors `cosigned_sth::conflicting_majority_cosigned_heads_share_a_witness`,
    /// exercised through this module's fetch-independent confirmation path
    /// instead of calling `find_equivocating_witnesses` directly.
    #[test]
    fn confirms_an_equivocation_shown_to_disjoint_witness_groups() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let w1 = witness_fixture();
        let w2 = witness_fixture();
        let w3 = witness_fixture();

        // A 3-witness majority is 2. head_a: w1 + w2. head_b: w2 + w3 — w2
        // is the equivocator, cosigning both conflicting roots.
        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone()), (&w2.0, w2.1.clone())],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[(&w2.0, w2.1.clone()), (&w3.0, w3.1.clone())],
        );

        let result =
            confirm_equivocation(&[author_key.verifying_key()], &head_a, &head_b, now).unwrap();
        assert_eq!(result.0, author_key.verifying_key());
        assert_eq!(result.1, vec![w2.1]);
    }

    #[test]
    fn no_confirmation_when_the_wrong_author_key_is_tried() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let wrong_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let w1 = witness_fixture();
        let w2 = witness_fixture();
        let w3 = witness_fixture();

        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone()), (&w2.0, w2.1.clone())],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[(&w2.0, w2.1.clone()), (&w3.0, w3.1.clone())],
        );

        assert!(
            confirm_equivocation(&[wrong_key.verifying_key()], &head_a, &head_b, now).is_none()
        );
    }

    #[test]
    fn no_confirmation_when_neither_head_reaches_majority() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        let w1 = witness_fixture();
        let w3 = witness_fixture();

        // Each head has exactly one cosigner, but the two heads' union known
        // list has size 2 (w1, w3 — disjoint sets), so `majority_threshold(2)
        // == 2` and neither head's single cosignature reaches it. Nothing to
        // prove: no shared witness, no majority on either side.
        let head_a = head(
            &author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone())],
        );
        let head_b = head(
            &author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            now,
            &[(&w3.0, w3.1.clone())],
        );

        assert!(
            confirm_equivocation(&[author_key.verifying_key()], &head_a, &head_b, now).is_none()
        );
    }

    #[test]
    fn known_list_ignores_a_malformed_witness_key_id() {
        let author_key = SigningKey::generate(&mut rand::rng());
        let now = OffsetDateTime::UNIX_EPOCH;
        let w1 = witness_fixture();

        let mut head_a = head(
            &author_key,
            1,
            &root_hash_fixture(1),
            "avalon-test",
            now,
            &[(&w1.0, w1.1.clone())],
        );
        head_a
            .cosignatures
            .push(avalon_protocol::witness::sign_witness_cosignature(
                &w1.0,
                "not-hex-at-all",
                1,
                &root_hash_fixture(1),
                "avalon-test",
                now,
                now,
            ));
        let head_b = head_a.clone();

        let list = known_list_from_cosignatures(&head_a, &head_b);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, w1.1);
    }
}
