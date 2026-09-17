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
//! — gating these behind a session or integrator credential would defeat that.
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
//!
//! **Issue #520 — mirror-backed fallback.** Every handler in this module
//! first tries `state.chain` (this node's own authored `ledger_entries`/
//! `signed_tree_heads`), and only when that has nothing at all
//! (`state.chain.entry_count() == 0`) falls back to this node's own
//! mirrored data (`mirrored_entries`/`observed_sths`, populated by
//! `crate::mirror_watcher`). This is deliberately all-or-nothing per
//! request, never a partial blend of authored and mirrored rows for one
//! response: a node with any authored history of its own is always this
//! network's authority for `/ledger/*` and never even looks at its mirror
//! tables; a pure-mirror node (no authored history) serves entirely from
//! what it independently verified while mirroring. The mirror fallback
//! re-derives and self-verifies exactly the same way the authored path
//! does — see each `mirror_*` helper's own doc comment — so a caller gets
//! the same verifiable answer regardless of which kind of node answered.

use avalon_chain::merkle;
use avalon_chain::mirror;
use avalon_chain::sth::SignedTreeHead;
use avalon_chain::{LedgerEntryView, SettlementProvider};
use avalon_protocol::events::{Commitment, EventBatch};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
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
    /// Issue #368: this node's own build version — not part of the signed
    /// bytes (see `signature`'s doc comment above; adding a field here
    /// never changes what's cryptographically covered), purely a
    /// compatibility/availability signal a peer's mirror-watcher checks
    /// against its own `crate::version::MIN_SUPPORTED_PEER_VERSION` floor.
    pub protocol_version: String,
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
            protocol_version: crate::version::PROTOCOL_VERSION.to_string(),
        }
    }
}

/// `GET /ledger/sth/latest` — the current Signed Tree Head. Public,
/// unauthenticated (see module docs).
///
/// Issue #520: falls back to this node's own mirrored data
/// ([`mirror_latest_sth`]) when it has no locally-authored STH of its own —
/// a pure-mirror node's whole point, per the module-level "Mirrors, not
/// federation" design. An authority node with any local history always
/// answers from `state.chain` and never even looks at the mirror tables,
/// so authored and mirrored history are never blended for one response.
pub async fn latest_sth(
    State(state): State<AppState>,
) -> Result<Json<SignedTreeHeadResponse>, AppError> {
    if let Some(sth) = state.chain.latest_signed_tree_head().await? {
        return Ok(Json(sth.into()));
    }
    let sth = mirror_latest_sth(&state.pool, state.chain.network_id())
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
///
/// Issue #520: same mirror fallback as [`latest_sth`] — see its doc comment.
pub async fn sth_at_tree_size(
    State(state): State<AppState>,
    Path(tree_size): Path<i64>,
) -> Result<Json<SignedTreeHeadResponse>, AppError> {
    if let Some(sth) = state.chain.signed_tree_head_at(tree_size).await? {
        return Ok(Json(sth.into()));
    }
    let sth = mirror_sth_at(&state.pool, state.chain.network_id(), tree_size)
        .await?
        .ok_or(AppError::SignedTreeHeadNotFound)?;
    Ok(Json(sth.into()))
}

/// Issue #520: `GET /ledger/sth/latest`'s mirror-backed fallback, reached
/// only once this node has no locally-authored STH of its own to serve.
/// Recomputes the Merkle root from this node's own `mirrored_entries` at
/// the highest `tree_size` it has fully backfilled so far, then looks up
/// the one `observed_sths` row whose `root_hash` matches that recomputed
/// root exactly — the same self-verify-before-serving invariant every
/// proof endpoint in this module already follows, applied here to STHs
/// instead of proofs. `Ok(None)` if this node has never mirrored anything
/// for `network_id`.
async fn mirror_latest_sth(
    pool: &sqlx::PgPool,
    network_id: &str,
) -> Result<Option<SignedTreeHead>, AppError> {
    let progress = mirror::mirrored_progress(pool, network_id).await?;
    if progress.verified_count == 0 {
        return Ok(None);
    }
    mirror_sth_at(pool, network_id, progress.verified_count).await
}

/// Issue #520: `GET /ledger/sth/{tree_size}`'s mirror-backed fallback — see
/// [`mirror_latest_sth`]'s doc comment for the self-verification shape this
/// shares. `Ok(None)` if this node hasn't mirrored `tree_size` entries yet,
/// or (should never happen, given the mirror-watcher's own
/// verify-before-store invariant) no peer's STH ever matched the root this
/// node independently recomputes — treated as "not found" rather than
/// served unverifiable, per this module's headline rule.
async fn mirror_sth_at(
    pool: &sqlx::PgPool,
    network_id: &str,
    tree_size: i64,
) -> Result<Option<SignedTreeHead>, AppError> {
    let hashes = mirror::mirrored_entry_hashes_up_to(pool, network_id, tree_size).await?;
    if (hashes.len() as i64) < tree_size {
        return Ok(None);
    }
    let root =
        merkle::mth_of_hex_hashes(&hashes).map_err(avalon_chain::SettlementError::Storage)?;
    let observed =
        mirror::observed_sth_matching_root(pool, network_id, tree_size, &hex::encode(root)).await?;
    Ok(observed.map(SignedTreeHead::from))
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
///
/// Issue #520: falls back to [`mirror_consistency_proof`] when this node has
/// no locally-authored entries — see [`latest_sth`]'s doc comment for the
/// "never blended" rationale shared by every handler in this module.
pub async fn consistency_proof(
    State(state): State<AppState>,
    Query(query): Query<ConsistencyProofQuery>,
) -> Result<Json<ConsistencyProofResponse>, AppError> {
    let (first, second) = (query.first, query.second);
    if first < 0 || second < 0 || first > second {
        return Err(AppError::InvalidProofQuery);
    }

    let entry_count = state.chain.entry_count().await?;
    if entry_count == 0 {
        return mirror_consistency_proof(&state.pool, state.chain.network_id(), first, second)
            .await;
    }
    if second > entry_count {
        return Err(AppError::LedgerRangeNotCommitted);
    }

    let second_usize = second as usize;
    let first_usize = first as usize;

    let first_root = if first == 0 {
        merkle::empty_root()
    } else {
        state
            .chain
            .root_at(first)
            .await?
            .ok_or(AppError::LedgerRangeNotCommitted)?
    };
    let second_root = state
        .chain
        .root_at(second)
        .await?
        .ok_or(AppError::LedgerRangeNotCommitted)?;

    let proof = state.chain.consistency_proof(first, second).await?;

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

/// Issue #520: [`consistency_proof`]'s mirror-backed fallback. Rebuilds
/// both roots and the proof itself from this node's own `mirrored_entries`
/// (the exact same leaf ordering the mirror-watcher already verified each
/// entry's inclusion against while backfilling), then re-verifies before
/// ever responding — the same invariant [`consistency_proof`] itself
/// follows for the authored path.
async fn mirror_consistency_proof(
    pool: &sqlx::PgPool,
    network_id: &str,
    first: i64,
    second: i64,
) -> Result<Json<ConsistencyProofResponse>, AppError> {
    let progress = mirror::mirrored_progress(pool, network_id).await?;
    if second > progress.verified_count {
        return Err(AppError::LedgerRangeNotCommitted);
    }

    let hashes_to_second = mirror::mirrored_entry_hashes_up_to(pool, network_id, second).await?;
    let second_root = merkle::mth_of_hex_hashes(&hashes_to_second)
        .map_err(avalon_chain::SettlementError::Storage)?;
    let first_root = if first == 0 {
        merkle::empty_root()
    } else {
        merkle::mth_of_hex_hashes(&hashes_to_second[..first as usize])
            .map_err(avalon_chain::SettlementError::Storage)?
    };

    let proof = merkle::consistency_proof_of_hex_hashes(first as usize, &hashes_to_second)
        .map_err(avalon_chain::SettlementError::Storage)?;

    if !merkle::verify_consistency_proof(
        first as usize,
        second as usize,
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
///
/// Issue #520: falls back to [`mirror_inclusion_proof`] when this node has
/// no locally-authored entries — see [`latest_sth`]'s doc comment for the
/// "never blended" rationale shared by every handler in this module.
pub async fn inclusion_proof(
    State(state): State<AppState>,
    Query(query): Query<InclusionProofQuery>,
) -> Result<Json<InclusionProofResponse>, AppError> {
    let (seq, tree_size) = (query.seq, query.tree_size);
    if seq < 1 || tree_size < 1 {
        return Err(AppError::InvalidProofQuery);
    }

    let entry_count = state.chain.entry_count().await?;
    if entry_count == 0 {
        return mirror_inclusion_proof(&state.pool, state.chain.network_id(), seq, tree_size).await;
    }
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
    let tree_size_usize = tree_size as usize;

    let (leaf_hash_hex, proof) = state.chain.inclusion_proof(leaf_index, tree_size).await?;
    let leaf_bytes = hex::decode(&leaf_hash_hex)
        .map_err(|e: hex::FromHexError| avalon_chain::SettlementError::Storage(e.to_string()))?;
    let leaf_index = leaf_index as usize;

    let root = state
        .chain
        .root_at(tree_size)
        .await?
        .ok_or(AppError::LedgerRangeNotCommitted)?;

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

/// Issue #520: [`inclusion_proof`]'s mirror-backed fallback — same
/// self-verify-before-serving shape, over this node's own
/// `mirrored_entries` rather than `ledger_entries`.
async fn mirror_inclusion_proof(
    pool: &sqlx::PgPool,
    network_id: &str,
    seq: i64,
    tree_size: i64,
) -> Result<Json<InclusionProofResponse>, AppError> {
    let progress = mirror::mirrored_progress(pool, network_id).await?;
    if tree_size > progress.verified_count {
        return Err(AppError::LedgerRangeNotCommitted);
    }

    let leaf_index = mirror::mirrored_leaf_index_for_seq(pool, network_id, seq)
        .await?
        .ok_or(AppError::InvalidProofQuery)?;
    if leaf_index >= tree_size {
        return Err(AppError::InvalidProofQuery);
    }
    let leaf_index_usize = leaf_index as usize;
    let tree_size_usize = tree_size as usize;

    let hashes = mirror::mirrored_entry_hashes_up_to(pool, network_id, tree_size).await?;
    let leaf_hash_hex = hashes[leaf_index_usize].clone();
    let proof = merkle::inclusion_proof_of_hex_hashes(leaf_index_usize, &hashes)
        .map_err(avalon_chain::SettlementError::Storage)?;
    let leaf_bytes = hex::decode(&leaf_hash_hex)
        .map_err(|e: hex::FromHexError| avalon_chain::SettlementError::Storage(e.to_string()))?;
    let root =
        merkle::mth_of_hex_hashes(&hashes).map_err(avalon_chain::SettlementError::Storage)?;

    if !merkle::verify_inclusion_proof(
        &leaf_bytes,
        leaf_index_usize,
        tree_size_usize,
        &proof,
        &root,
    ) {
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
    /// #364: pre-filter to one subject's own entries, e.g.
    /// `identity:<uuid>` — the exact same `subject` string
    /// `LedgerEntryResponse::subject` echoes back. `None` (the default)
    /// keeps this endpoint's original unfiltered, mirror-facing behavior.
    #[serde(default)]
    pub subject: Option<String>,
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

/// Issue #520: a mirrored entry (`mirrored_entries`) never has a pruned
/// payload — the mirror-watcher only ever stores what it fetched and
/// independently verified, and this codebase's retention/pruning (#208)
/// only ever touches `ledger_entries`, this node's own authored rows.
impl From<mirror::MirroredEntry> for LedgerEntryResponse {
    fn from(entry: mirror::MirroredEntry) -> Self {
        Self {
            seq: entry.seq,
            event_id: entry.event_id,
            kind: entry.kind,
            issuer: entry.issuer,
            subject: entry.subject,
            payload: entry.payload,
            payload_pruned: false,
            version: entry.version,
            event_timestamp: entry.event_timestamp,
            prev_hash: entry.prev_hash,
            entry_hash: entry.entry_hash,
            batch_id: entry.batch_id,
        }
    }
}

/// `GET /ledger/entries?since_seq={n}&limit={m}&subject={id}` — issue
/// #299's bulk entries endpoint, the read path a mirror needs to hold real
/// ledger content rather than only verify STHs. Public, unauthenticated,
/// same rationale as every other endpoint in this module (see module
/// docs).
///
/// Returns entries with `seq` strictly greater than `since_seq`, oldest
/// first, capped at [`MAX_ENTRIES_LIMIT`] rows regardless of what `limit`
/// asks for. When `subject` is given, pre-filters to that subject's own
/// entries (#364) — same pagination/ordering semantics, just scoped, so an
/// integrator that only cares about one of its own users doesn't have to
/// replay the whole ledger to find their entries. A subject with no
/// entries yet returns an empty list, not an error. **Not itself a
/// verified read** — see
/// [`avalon_chain::PostgresSettlementProvider::list_entries_since`]'s doc
/// comment: a caller that needs to trust this content (a mirror
/// backfilling) must independently verify each entry against a
/// signature-checked STH via `GET /ledger/proof/inclusion` before
/// accepting it. Filtering by `subject` never changes an entry's
/// hash-chain position — inclusion proofs for a filtered row still verify
/// against the same global tree.
///
/// Issue #520: falls back to this node's own `mirrored_entries` when it has
/// no locally-authored entries — see [`latest_sth`]'s doc comment for the
/// "never blended" rationale shared by every handler in this module.
pub async fn list_entries(
    State(state): State<AppState>,
    Query(query): Query<EntriesQuery>,
) -> Result<Json<Vec<LedgerEntryResponse>>, AppError> {
    if query.since_seq < 0 || query.limit <= 0 {
        return Err(AppError::InvalidEntriesQuery);
    }
    let limit = query.limit.min(MAX_ENTRIES_LIMIT);

    let entry_count = state.chain.entry_count().await?;
    if entry_count == 0 {
        let entries = mirror::mirrored_entries_since(
            &state.pool,
            state.chain.network_id(),
            query.since_seq,
            limit,
            query.subject.as_deref(),
        )
        .await?;
        return Ok(Json(entries.into_iter().map(Into::into).collect()));
    }

    let entries = state
        .chain
        .list_entries_since_for_subject(query.since_seq, limit, query.subject.as_deref())
        .await?;
    Ok(Json(entries.into_iter().map(Into::into).collect()))
}

/// `POST /ledger/submit` — issue #313's node-to-node write endpoint: a
/// remote-settlement node's `outbox::run_worker` posts each drained batch
/// here instead of calling `chain.commit` against its own pool, and this
/// handler runs the exact same `chain.commit` call the local outbox worker
/// already runs for itself. This is privileged (unlike every other endpoint
/// in this module): it lets an authenticated caller get arbitrary events
/// committed to the authoritative log, so it is deliberately **not**
/// public/unauthenticated like the read endpoints above.
///
/// **Auth: a shared-secret bearer token (`AVALON_SETTLEMENT_SUBMIT_KEY`).**
/// Nothing more specific for trusted node-to-node calls already existed in
/// this codebase to reuse (every other authenticated route here checks a
/// user's own session or an integrator's own registered credential, neither of
/// which fits "one operator's own two nodes talking to each other"), and
/// the ticket left the exact mechanism open with this as its suggested
/// default. If this node has no `AVALON_SETTLEMENT_SUBMIT_KEY` configured
/// at all, every request is refused — an operator who never turns this on
/// gets an endpoint that exists but accepts nothing, never one that's
/// silently open.
pub async fn submit_ledger_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(batch): Json<EventBatch>,
) -> Result<Json<Commitment>, AppError> {
    require_settlement_submit_key(&state, &headers)?;
    let commitment = state.chain.commit(&batch).await?;
    Ok(Json(commitment))
}

/// `POST /ledger/prepare-batch` — issue #531's managed-hosting two-phase
/// remote-signing flow, phase one. An integrator using a managed host
/// (rather than self-hosting) submits its already-collected pending
/// events; this handler returns the unsigned candidate tree head
/// (`avalon_chain::sth::PreparedTreeHead`) the integrator needs to sign
/// *locally*, with its own settlement key — this node never holds, sees,
/// or asks for that key. See `PostgresSettlementProvider::prepare`'s own
/// doc comment for why the response is a stateless preview, not something
/// this node persists as "pending."
///
/// **Auth**: the same shared-secret bearer mechanism `submit_ledger_batch`
/// above uses (`AVALON_SETTLEMENT_SUBMIT_KEY`) — this is node-to-node/
/// integrator-to-managed-host traffic, not a public read. A managed host
/// with `AVALON_MANAGED_HOSTING_VERIFY_KEY` unset also refuses every
/// request here (see `finalize_batch`'s own doc comment for why gating
/// both endpoints on that one var, not just `finalize`, is the honest
/// choice) — preparing a batch there could never be finalized anyway.
pub async fn prepare_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(batch): Json<EventBatch>,
) -> Result<Json<avalon_chain::sth::PreparedTreeHead>, AppError> {
    require_settlement_submit_key(&state, &headers)?;
    if state.managed_hosting_verify_key.is_none() {
        return Err(AppError::Unauthorized);
    }

    let preview = state.chain.prepare(&batch).await?;
    Ok(Json(preview))
}

#[derive(Deserialize)]
pub struct FinalizeBatchRequest {
    pub batch: EventBatch,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub signing_key_id: String,
    /// Hex-encoded Ed25519 signature over
    /// `avalon_chain::sth::signing_message(tree_size, root_hash,
    /// network_id, created_at)` — the exact preview `prepare_batch`
    /// returned, computed and signed by the integrator's own settlement
    /// key.
    pub signature: String,
}

/// `POST /ledger/finalize-batch` — issue #531's managed-hosting two-phase
/// remote-signing flow, phase two. Verifies the caller's signature against
/// `AVALON_MANAGED_HOSTING_VERIFY_KEY` and, only if it checks out,
/// actually commits `body.batch` — see
/// `PostgresSettlementProvider::finalize`'s own doc comment for why this
/// recomputes everything fresh rather than trusting a prior `prepare`
/// call, and why that alone is what makes a stale or forged finalize fail
/// cleanly.
///
/// **Interim, single-key-per-node** (`AVALON_MANAGED_HOSTING_VERIFY_KEY`,
/// `AppState::managed_hosting_verify_key`): a managed-hosting node is
/// presumed dedicated to exactly one hosted integrator's shard for now.
/// #543 (per-shard trust anchors — resolving a shard's authorized
/// signing key via that integrator's own `issuer.key_added` event,
/// reusing issuer-key registration rather than a static config value) is
/// the real, designed replacement for this; not built yet, so this is the
/// honest interim. Unset, this endpoint refuses every request — an
/// operator who never turns on managed hosting gets an endpoint that
/// exists but accepts nothing, never one that's silently open.
pub async fn finalize_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FinalizeBatchRequest>,
) -> Result<Json<Commitment>, AppError> {
    require_settlement_submit_key(&state, &headers)?;
    let verify_key = state
        .managed_hosting_verify_key
        .as_ref()
        .ok_or(AppError::Unauthorized)?;

    let commitment = state
        .chain
        .finalize(
            &body.batch,
            body.created_at,
            &body.signing_key_id,
            &body.signature,
            verify_key,
        )
        .await?;
    Ok(Json(commitment))
}

#[derive(Serialize)]
pub struct FailingShard {
    pub shard_id: String,
    pub authority: String,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

#[derive(Serialize)]
pub struct RemoteSubmitStatusResponse {
    pub failing_shards: Vec<FailingShard>,
}

/// `GET /ledger/remote-submit-status` — issue #526. A forwarding node
/// (`AVALON_SETTLEMENT_REMOTE_URL(S)` configured) that can't reach or gets
/// rejected by a shard's configured Settlement authority previously only
/// logged that failure locally (`crate::outbox::drain_locked`) — an
/// integrator whose SDK is misconfigured against the wrong node had no way
/// to discover the correct authority from the error alone. This surfaces
/// it: `failing_shards` lists every shard currently failing to reach its
/// configured authority, each entry's `authority` being exactly the URL
/// this node was already configured with for that shard — never an
/// inferred or alternate one (exactly one Settlement authority is expected
/// per shard; this doesn't add or imply a second one).
///
/// Public, unauthenticated — same rationale as every other `/ledger/*`
/// diagnostic read in this module (see module docs): the URL a node is
/// configured to forward to isn't sensitive, and gating discovery of it
/// behind a credential would defeat the point for exactly the
/// misconfigured caller this exists to help.
///
/// **503** while any shard is failing (a signal worth a non-2xx, not just
/// a quiet 200 a caller has to know to inspect); **200** with an empty
/// list otherwise — including when this node has no remote authority
/// configured at all, since there is then nothing to ever report as
/// failing (`AppState::remote_submit_status` is `None`).
pub async fn remote_submit_status(
    State(state): State<AppState>,
) -> (axum::http::StatusCode, Json<RemoteSubmitStatusResponse>) {
    let failing_shards: Vec<FailingShard> = state
        .remote_submit_status
        .as_ref()
        .map(|status| {
            status
                .currently_failing()
                .into_iter()
                .map(|(shard_id, failure)| FailingShard {
                    shard_id,
                    authority: failure.authority_url,
                    reason: failure.reason,
                    since: failure.since,
                })
                .collect()
        })
        .unwrap_or_default();

    let http_status = if failing_shards.is_empty() {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };

    (
        http_status,
        Json(RemoteSubmitStatusResponse { failing_shards }),
    )
}

/// Shared bearer-token check `prepare_batch`/`finalize_batch` both use —
/// factored out of `submit_ledger_batch` rather than duplicated a third
/// time.
fn require_settlement_submit_key(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let expected = state
        .settlement_submit_key
        .as_deref()
        .ok_or(AppError::Unauthorized)?;
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    if provided != expected {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}
