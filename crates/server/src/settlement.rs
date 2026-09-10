//! Mirror-facing, public transparency-log reads — issue #211, the read-side
//! API surface for the RFC 6962 Merkle tree and Signed Tree Heads issue
//! #210 already computes and stores (`crates/chain/src/merkle.rs`,
//! `crates/chain/src/sth.rs`). See `docs/architecture/settlement.md`'s
//! "What is decided (continued)" section for the design this implements —
//! #40/#39's decision that mirror sync stays minimal: expose the latest
//! STH, a historical STH by `tree_size`, RFC 6962 consistency proofs, and
//! inclusion proofs.
//!
//! **All four endpoints are public, unauthenticated reads.** A
//! transparency log's whole point is independent verifiability by anyone
//! holding only the operator's public key (`AVALON_SETTLEMENT_VERIFY_KEY`)
//! — gating these behind a session or game credential would defeat that.
//! Nothing here exposes entry *content*: responses carry `tree_size`,
//! hex-encoded hashes, and signatures, the same shape `avalon
//! inspect-ledger` already prints and no more sensitive than the
//! `entry_hash` values `list_entries` already computes over.
//!
//! **Never a fabricated proof.** Every proof this module returns is
//! independently re-verified with [`avalon_chain::merkle::verify_inclusion_proof`]/
//! [`avalon_chain::merkle::verify_consistency_proof`] against the exact
//! root(s) the response claims, before the response is ever built — a
//! mismatch is an [`AppError::ProofVerificationFailed`] (mapped to 500),
//! never a silently-wrong 200. This is the ticket's own invariant: "every
//! proof returned must independently verify ... never return a proof that
//! doesn't actually check out against stored data."
//!
//! A `seq`/`tree_size` combination beyond what's actually been committed
//! yet is a clear [`AppError::LedgerRangeNotCommitted`] (404), never an
//! empty or fabricated proof — the negative case the ticket calls out
//! explicitly.

use avalon_chain::merkle;
use avalon_chain::sth::SignedTreeHead;
use avalon_chain::LedgerEntryView;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SignedTreeHeadResponse {
    pub tree_size: i64,
    /// Lowercase hex-encoded RFC 6962 Merkle Tree Hash.
    pub root_hash: String,
    pub network_id: String,
    pub signing_key_id: String,
    /// Lowercase hex-encoded Ed25519 signature over
    /// `(tree_size, root_hash, network_id, created_at)` — verifiable with
    /// only `AVALON_SETTLEMENT_VERIFY_KEY`, per `crate::sth`'s doc comment.
    pub signature: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<SignedTreeHead> for SignedTreeHeadResponse {
    fn from(sth: SignedTreeHead) -> Self {
        Self {
            tree_size: sth.tree_size,
            root_hash: sth.root_hash,
            network_id: sth.network_id,
            signing_key_id: sth.signing_key_id,
            signature: sth.signature,
            created_at: sth.created_at,
        }
    }
}

/// `GET /ledger/sth/latest` — the current Signed Tree Head. Public,
/// unauthenticated (see module docs).
pub async fn latest_sth(
    State(state): State<AppState>,
) -> Result<Json<SignedTreeHeadResponse>, AppError> {
    let sth = state
        .chain
        .latest_signed_tree_head()
        .await?
        .ok_or(AppError::SignedTreeHeadNotFound)?;
    Ok(Json(sth.into()))
}

/// `GET /ledger/sth/{tree_size}` — a historical Signed Tree Head at exactly
/// this `tree_size`. Needed to chain consistency proofs (#40's mirror-sync
/// design: a mirror stores every STH it has independently observed and
/// compares against another mirror's history for the same `tree_size`).
/// 404s (not an empty/fabricated STH) if no batch ever closed at exactly
/// this size — `tree_size` values between batch boundaries have no STH by
/// construction, since one is only ever produced per batch commit.
pub async fn sth_at_tree_size(
    State(state): State<AppState>,
    Path(tree_size): Path<i64>,
) -> Result<Json<SignedTreeHeadResponse>, AppError> {
    let sth = state
        .chain
        .signed_tree_head_at(tree_size)
        .await?
        .ok_or(AppError::SignedTreeHeadNotFound)?;
    Ok(Json(sth.into()))
}

#[derive(Deserialize)]
pub struct ConsistencyProofQuery {
    pub first: i64,
    pub second: i64,
}

#[derive(Serialize)]
pub struct ConsistencyProofResponse {
    pub first: i64,
    pub second: i64,
    /// The RFC 6962 Merkle Tree Hash at `first` leaves, recomputed here for
    /// convenience — a real verifier's actual trust anchor should still be
    /// an independently-fetched, signature-checked STH
    /// (`GET /ledger/sth/{tree_size}`) for this size, not this field alone.
    pub first_root_hash: String,
    pub second_root_hash: String,
    /// Hex-encoded RFC 6962 consistency-proof nodes, in the order
    /// [`avalon_chain::merkle::verify_consistency_proof`] expects.
    pub proof: Vec<String>,
}

/// `GET /ledger/proof/consistency?first={a}&second={b}` — an RFC 6962
/// consistency proof that the tree at `second` leaves is a strict
/// append-only extension of the tree at `first` leaves. See module docs for
/// the self-verification invariant and the not-yet-committed negative case.
pub async fn consistency_proof(
    State(state): State<AppState>,
    Query(query): Query<ConsistencyProofQuery>,
) -> Result<Json<ConsistencyProofResponse>, AppError> {
    let (first, second) = (query.first, query.second);
    if first < 0 || second < 0 || first > second {
        return Err(AppError::InvalidProofQuery);
    }

    let entry_count = state.chain.entry_count().await?;
    if second > entry_count {
        return Err(AppError::LedgerRangeNotCommitted);
    }

    let leaves = state.chain.entry_hashes_up_to(second).await?;
    let second_usize = second as usize;
    let first_usize = first as usize;

    let first_root = if first == 0 {
        merkle::empty_root()
    } else {
        merkle::mth_of_hex_hashes(&leaves[..first_usize])
            .map_err(avalon_chain::SettlementError::Storage)?
    };
    let second_root =
        merkle::mth_of_hex_hashes(&leaves).map_err(avalon_chain::SettlementError::Storage)?;

    let proof = merkle::consistency_proof_of_hex_hashes(first_usize, &leaves)
        .map_err(avalon_chain::SettlementError::Storage)?;

    // Never return a proof that doesn't actually check out (this module's
    // own headline invariant) — re-verify with the standalone RFC 6962
    // verifier before this response is ever built, exactly as a remote
    // mirror would, rather than trusting proof generation blindly.
    if !merkle::verify_consistency_proof(
        first_usize,
        second_usize,
        &proof,
        &first_root,
        &second_root,
    ) {
        return Err(AppError::ProofVerificationFailed);
    }

    Ok(Json(ConsistencyProofResponse {
        first,
        second,
        first_root_hash: hex::encode(first_root),
        second_root_hash: hex::encode(second_root),
        proof: proof.into_iter().map(hex::encode).collect(),
    }))
}

#[derive(Deserialize)]
pub struct InclusionProofQuery {
    pub seq: i64,
    pub tree_size: i64,
}

#[derive(Serialize)]
pub struct InclusionProofResponse {
    pub seq: i64,
    pub tree_size: i64,
    /// The entry's `entry_hash` — the exact leaf input RFC 6962 leaf-hashes
    /// under the hood, same value already exposed by `avalon
    /// inspect-ledger` / `list_entries`. Not entry payload content.
    pub leaf_hash: String,
    pub root_hash: String,
    /// Hex-encoded RFC 6962 audit-path nodes, leaf-to-root order — what
    /// [`avalon_chain::merkle::verify_inclusion_proof`] expects.
    pub proof: Vec<String>,
}

/// `GET /ledger/proof/inclusion?seq={n}&tree_size={s}` — an RFC 6962
/// inclusion proof that the ledger entry at `seq` is included in the tree
/// at `tree_size`. `seq` is `ledger_entries.seq`, a real row identifier —
/// **not** a dense 1-based position. `seq` can have gaps (a batch commit
/// that fails partway through permanently burns whatever `seq` values it
/// already allocated, since Postgres identity/sequence advancement isn't
/// transactional), so the Merkle leaf index is never `seq - 1`; it's the
/// entry's *rank* among all committed entries, resolved via
/// `leaf_index_for_seq`.
pub async fn inclusion_proof(
    State(state): State<AppState>,
    Query(query): Query<InclusionProofQuery>,
) -> Result<Json<InclusionProofResponse>, AppError> {
    let (seq, tree_size) = (query.seq, query.tree_size);
    if seq < 1 || tree_size < 1 {
        return Err(AppError::InvalidProofQuery);
    }

    let entry_count = state.chain.entry_count().await?;
    if tree_size > entry_count {
        return Err(AppError::LedgerRangeNotCommitted);
    }

    let leaf_index = state
        .chain
        .leaf_index_for_seq(seq)
        .await?
        .ok_or(AppError::InvalidProofQuery)?;
    if leaf_index >= tree_size {
        // A real entry, just not (yet) included in a tree this small —
        // same "well-formed but unanswerable" shape as the bounds check
        // above, not a fabricated/truncated proof.
        return Err(AppError::InvalidProofQuery);
    }
    let leaf_index = leaf_index as usize;
    let tree_size_usize = tree_size as usize;

    let leaves = state.chain.entry_hashes_up_to(tree_size).await?;
    let leaf_hash_hex = leaves[leaf_index].clone();
    let leaf_bytes = hex::decode(&leaf_hash_hex)
        .map_err(|e: hex::FromHexError| avalon_chain::SettlementError::Storage(e.to_string()))?;

    let root =
        merkle::mth_of_hex_hashes(&leaves).map_err(avalon_chain::SettlementError::Storage)?;
    let proof = merkle::inclusion_proof_of_hex_hashes(leaf_index, &leaves)
        .map_err(avalon_chain::SettlementError::Storage)?;

    // Same self-verification invariant as `consistency_proof` above.
    if !merkle::verify_inclusion_proof(&leaf_bytes, leaf_index, tree_size_usize, &proof, &root) {
        return Err(AppError::ProofVerificationFailed);
    }

    Ok(Json(InclusionProofResponse {
        seq,
        tree_size,
        leaf_hash: leaf_hash_hex,
        root_hash: hex::encode(root),
        proof: proof.into_iter().map(hex::encode).collect(),
    }))
}

/// A public maximum on `limit` for `GET /ledger/entries` — an
/// unauthenticated bulk-read endpoint accepting an arbitrary `limit`
/// straight from the query string would let any caller demand an
/// unbounded response.
const MAX_ENTRIES_LIMIT: i64 = 1000;
const DEFAULT_ENTRIES_LIMIT: i64 = 500;

#[derive(Deserialize)]
pub struct EntriesQuery {
    #[serde(default)]
    pub since_seq: i64,
    #[serde(default = "default_entries_limit")]
    pub limit: i64,
}

fn default_entries_limit() -> i64 {
    DEFAULT_ENTRIES_LIMIT
}

#[derive(Serialize)]
pub struct LedgerEntryResponse {
    pub seq: i64,
    pub event_id: Uuid,
    pub kind: String,
    pub issuer: String,
    pub subject: String,
    /// `None` if this row's payload has been pruned (issue #208) — same
    /// meaning as `avalon_chain::LedgerEntryView::payload_pruned`.
    pub payload: Option<serde_json::Value>,
    pub payload_pruned: bool,
    pub version: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub event_timestamp: OffsetDateTime,
    pub prev_hash: String,
    pub entry_hash: String,
    pub batch_id: Uuid,
}

impl From<LedgerEntryView> for LedgerEntryResponse {
    fn from(entry: LedgerEntryView) -> Self {
        Self {
            seq: entry.seq,
            event_id: entry.event_id,
            kind: entry.kind,
            issuer: entry.issuer,
            subject: entry.subject,
            payload: entry.payload,
            payload_pruned: entry.payload_pruned,
            version: entry.version,
            event_timestamp: entry.event_timestamp,
            prev_hash: entry.prev_hash,
            entry_hash: entry.entry_hash,
            batch_id: entry.batch_id,
        }
    }
}

/// `GET /ledger/entries?since_seq={n}&limit={m}` — issue #299's bulk
/// entries endpoint, the read path a mirror needs to hold real ledger
/// content rather than only verify STHs. Public, unauthenticated, same
/// rationale as every other endpoint in this module (see module docs).
///
/// Returns entries with `seq` strictly greater than `since_seq`, oldest
/// first, capped at [`MAX_ENTRIES_LIMIT`] rows regardless of what `limit`
/// asks for. **Not itself a verified read** — see
/// [`avalon_chain::PostgresSettlementProvider::list_entries_since`]'s doc
/// comment: a caller that needs to trust this content (a mirror
/// backfilling) must independently verify each entry against a
/// signature-checked STH via `GET /ledger/proof/inclusion` before
/// accepting it.
pub async fn list_entries(
    State(state): State<AppState>,
    Query(query): Query<EntriesQuery>,
) -> Result<Json<Vec<LedgerEntryResponse>>, AppError> {
    if query.since_seq < 0 || query.limit <= 0 {
        return Err(AppError::InvalidEntriesQuery);
    }
    let limit = query.limit.min(MAX_ENTRIES_LIMIT);
    let entries = state
        .chain
        .list_entries_since(query.since_seq, limit)
        .await?;
    Ok(Json(entries.into_iter().map(Into::into).collect()))
}
