//! Cross-shard projection resolution with inclusion proof — epic #623,
//! issue #636. Given a specific `shard_id` + `base_url` (typically learned
//! from #635's identity locator), fetches every ledger entry for a
//! `subject` from that remote node and verifies each one end-to-end before
//! trusting its payload at all — no full shard mirroring required, the
//! same trust-minimization property a continuously-backfilling mirror
//! already gets (`crate::mirror_watcher`), applied here to a one-off
//! fetch instead.
//!
//! **The full trust chain, each link independently checked:**
//! 1. A signature-checked Signed Tree Head (`GET /ledger/sth/latest`),
//!    verified against a shard's *real* trust anchor —
//!    [`crate::cross_shard::resolve_shard_verify_keys_from_db`], the same
//!    #543 mechanism `crate::cross_shard`'s own cross-shard-root
//!    aggregation already uses, deliberately not a second trust mechanism.
//!    A shard whose key can't be resolved (neither a registered issuer key
//!    nor `AVALON_SHARD_VERIFY_KEYS`) is unverifiable here for exactly the
//!    same reason it's "missing" to that module's own aggregation.
//! 2. The entry itself (`GET /ledger/entries?subject=...`) — **not**
//!    trusted on its own; a remote node could return anything at this
//!    step, verified or not.
//! 3. An RFC 6962 inclusion proof for that entry
//!    (`GET /ledger/proof/inclusion`), checked against the STH's own
//!    `root_hash`/`tree_size` from step 1 — proves *some* `leaf_hash` at
//!    this `seq` was really committed under the trusted root.
//! 4. The entry's `entry_hash` independently **recomputed** from its
//!    fetched content (`avalon_chain::hash_entry`, exposed for exactly
//!    this — see that function's own doc comment) and compared against
//!    both the entry's own claimed `entry_hash` and the proof's
//!    `leaf_hash`. Without this link, a remote node could hand back a
//!    genuine inclusion proof for *some* real entry alongside a
//!    completely different, forged payload, and steps 1-3 alone would
//!    still pass — this is what actually binds the payload to the proof.
//!
//! **Generic on purpose**: returns every verified entry for `subject`,
//! never picks "the one" — #636's own scope is the fetch-and-verify
//! primitive, not identity-signing-key-specific business logic (which key
//! is still active, revocation, etc.) that a caller like #634's
//! verification path owns for itself. Reusable for any subject/kind this
//! epic or a later one needs (achievements, attestations), not just
//! `identity.signing_key_added`.

use std::collections::HashMap;

use avalon_chain::sth::SignedTreeHead;
use avalon_chain::{hash_entry, merkle, EntryContent};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cross_shard::resolve_shard_verify_keys_from_db;

/// Entries fetched per `subject` lookup — generous enough for any real
/// identity's signing-key history, capped so a malicious/misbehaving
/// remote node can't make this loop verify an unbounded number of entries
/// (each of which costs a full inclusion-proof round trip).
const MAX_ENTRIES: &str = "100";

#[derive(Debug, thiserror::Error)]
pub enum CrossShardFetchError {
    #[error("failed to fetch signed tree head from {0}: {1}")]
    SthFetchFailed(String, String),
    #[error("signed tree head's network_id did not match this node's own")]
    NetworkMismatch,
    #[error("signed tree head signature did not verify against any resolved key for shard {0}")]
    SthVerificationFailed(String),
    #[error("failed to fetch entries for subject {0} from {1}: {2}")]
    EntriesFetchFailed(String, String, String),
    #[error("failed to fetch inclusion proof for seq {0} from {1}: {2}")]
    ProofFetchFailed(i64, String, String),
    #[error(
        "inclusion proof's own root_hash/tree_size did not match the trusted signed tree head"
    )]
    ProofRootMismatch,
    #[error("fetched entry's recomputed hash did not match its claimed entry_hash or the inclusion proof's leaf_hash")]
    EntryHashMismatch,
    #[error("inclusion proof did not verify against the trusted signed tree head's root")]
    InclusionProofVerificationFailed,
}

/// A ledger entry whose content has been independently verified against a
/// signature-checked STH and a real RFC 6962 inclusion proof — safe for a
/// caller to trust the `payload` of.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedEntry {
    pub event_id: Uuid,
    pub kind: String,
    pub issuer: String,
    pub subject: String,
    pub payload: serde_json::Value,
    pub event_timestamp: OffsetDateTime,
    pub version: i32,
}

#[derive(Deserialize)]
struct SignedTreeHeadDto {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl From<SignedTreeHeadDto> for SignedTreeHead {
    fn from(dto: SignedTreeHeadDto) -> Self {
        Self {
            tree_size: dto.tree_size,
            root_hash: dto.root_hash,
            network_id: dto.network_id,
            signing_key_id: dto.signing_key_id,
            signature: dto.signature,
            created_at: dto.created_at,
        }
    }
}

#[derive(Deserialize)]
struct LedgerEntryDto {
    seq: i64,
    event_id: Uuid,
    kind: String,
    issuer: String,
    subject: String,
    payload: Option<serde_json::Value>,
    payload_pruned: bool,
    version: i32,
    #[serde(with = "time::serde::rfc3339")]
    event_timestamp: OffsetDateTime,
    prev_hash: String,
    entry_hash: String,
}

#[derive(Deserialize)]
struct InclusionProofDto {
    tree_size: i64,
    leaf_index: i64,
    leaf_hash: String,
    root_hash: String,
    proof: Vec<String>,
}

/// Fetches a signature-verified Signed Tree Head for `shard_id` from
/// `base_url` — step 1 of this module's own trust chain (see module doc
/// comment). Split out from [`fetch_verified_entries`] so a future caller
/// that only needs the STH itself (not a specific entry) doesn't have to
/// go through the whole entry-fetch path to get one.
async fn fetch_verified_sth(
    client: &reqwest::Client,
    pool: &PgPool,
    this_network_id: &str,
    shard_id: &str,
    base_url: &str,
    static_verify_keys: &HashMap<String, VerifyingKey>,
) -> Result<SignedTreeHead, CrossShardFetchError> {
    let sth_dto: SignedTreeHeadDto = client
        .get(format!("{base_url}/ledger/sth/latest"))
        .query(&[("shard_id", shard_id)])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| CrossShardFetchError::SthFetchFailed(base_url.to_string(), e.to_string()))?
        .json()
        .await
        .map_err(|e| CrossShardFetchError::SthFetchFailed(base_url.to_string(), e.to_string()))?;
    let sth: SignedTreeHead = sth_dto.into();

    if sth.network_id != this_network_id {
        return Err(CrossShardFetchError::NetworkMismatch);
    }

    let db_keys = resolve_shard_verify_keys_from_db(pool, shard_id).await;
    let static_key = static_verify_keys.get(shard_id);
    let verified = db_keys
        .iter()
        .chain(static_key)
        .any(|key| avalon_chain::sth::verify_tree_head(key, &sth));
    if !verified {
        return Err(CrossShardFetchError::SthVerificationFailed(
            shard_id.to_string(),
        ));
    }

    Ok(sth)
}

/// Fetches, and fully verifies (see module doc comment for the four-step
/// chain), every ledger entry for `subject` on `shard_id`, reached at
/// `base_url`. Entries whose payload has been pruned (issue #208) are
/// skipped — a pruned payload can never be hash-recomputed, so there is
/// nothing here to verify, not a failure of this fetch itself.
pub async fn fetch_verified_entries(
    pool: &PgPool,
    this_network_id: &str,
    shard_id: &str,
    base_url: &str,
    subject: &str,
    static_verify_keys: &HashMap<String, VerifyingKey>,
) -> Result<Vec<VerifiedEntry>, CrossShardFetchError> {
    let client = reqwest::Client::new();

    let sth = fetch_verified_sth(
        &client,
        pool,
        this_network_id,
        shard_id,
        base_url,
        static_verify_keys,
    )
    .await?;

    let entries: Vec<LedgerEntryDto> = client
        .get(format!("{base_url}/ledger/entries"))
        .query(&[
            ("subject", subject),
            ("shard_id", shard_id),
            ("limit", MAX_ENTRIES),
        ])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            CrossShardFetchError::EntriesFetchFailed(
                subject.to_string(),
                base_url.to_string(),
                e.to_string(),
            )
        })?
        .json()
        .await
        .map_err(|e| {
            CrossShardFetchError::EntriesFetchFailed(
                subject.to_string(),
                base_url.to_string(),
                e.to_string(),
            )
        })?;

    let mut verified = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.payload_pruned {
            continue;
        }
        let Some(payload) = entry.payload.clone() else {
            continue;
        };
        verified.push(verify_one_entry(&client, base_url, shard_id, &sth, entry, payload).await?);
    }
    Ok(verified)
}

/// Steps 3-4 of this module's own trust chain, for one already-fetched
/// (not yet trusted) entry: fetch its inclusion proof, check the proof's
/// own root against `sth`, recompute the entry's hash from content and
/// compare, then verify the proof itself.
async fn verify_one_entry(
    client: &reqwest::Client,
    base_url: &str,
    shard_id: &str,
    sth: &SignedTreeHead,
    entry: LedgerEntryDto,
    payload: serde_json::Value,
) -> Result<VerifiedEntry, CrossShardFetchError> {
    let proof_dto: InclusionProofDto = client
        .get(format!("{base_url}/ledger/proof/inclusion"))
        .query(&[
            ("seq", entry.seq.to_string()),
            ("tree_size", sth.tree_size.to_string()),
            ("shard_id", shard_id.to_string()),
        ])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            CrossShardFetchError::ProofFetchFailed(entry.seq, base_url.to_string(), e.to_string())
        })?
        .json()
        .await
        .map_err(|e| {
            CrossShardFetchError::ProofFetchFailed(entry.seq, base_url.to_string(), e.to_string())
        })?;

    if proof_dto.tree_size != sth.tree_size || proof_dto.root_hash != sth.root_hash {
        return Err(CrossShardFetchError::ProofRootMismatch);
    }

    let recomputed = hash_entry(
        &sth.network_id,
        &entry.prev_hash,
        &EntryContent {
            event_id: entry.event_id,
            kind: &entry.kind,
            issuer: &entry.issuer,
            subject: &entry.subject,
            payload: &payload,
            timestamp: entry.event_timestamp,
            version: entry.version,
        },
    );
    if recomputed != entry.entry_hash || recomputed != proof_dto.leaf_hash {
        return Err(CrossShardFetchError::EntryHashMismatch);
    }

    let leaf_bytes = hex::decode(&proof_dto.leaf_hash)
        .map_err(|_| CrossShardFetchError::InclusionProofVerificationFailed)?;
    let root_bytes = hex::decode(&sth.root_hash)
        .map_err(|_| CrossShardFetchError::InclusionProofVerificationFailed)?;
    let root: [u8; 32] = root_bytes
        .try_into()
        .map_err(|_| CrossShardFetchError::InclusionProofVerificationFailed)?;
    let proof: Vec<[u8; 32]> = proof_dto
        .proof
        .iter()
        .map(|hex_str| {
            hex::decode(hex_str)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(CrossShardFetchError::InclusionProofVerificationFailed)
        })
        .collect::<Result<_, _>>()?;

    if !merkle::verify_inclusion_proof(
        &leaf_bytes,
        proof_dto.leaf_index as usize,
        sth.tree_size as usize,
        &proof,
        &root,
    ) {
        return Err(CrossShardFetchError::InclusionProofVerificationFailed);
    }

    Ok(VerifiedEntry {
        event_id: entry.event_id,
        kind: entry.kind,
        issuer: entry.issuer,
        subject: entry.subject,
        payload,
        event_timestamp: entry.event_timestamp,
        version: entry.version,
    })
}
