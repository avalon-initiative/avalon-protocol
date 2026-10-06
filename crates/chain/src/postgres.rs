//! Postgres-backed `SettlementProvider` — milestone 1's implementation:
//! a sequential hash
//! chain plus a real RFC 6962 Merkle tree with signed tree heads. See
//! `avalon-docs/architecture/settlement.md` and `settlement-implementation-notes.md`
//! for the two tamper-evidence structures, why `tree_size` is a derived
//! leaf count rather than raw `seq`, batching, and node-tiered
//! payload retention.

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch, ProtocolEvent};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use avalon_protocol::canonical_payload::CanonicalPayloadError;
use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::ledger_entry::{self, EntryHashError, EntryHashInput};
use avalon_protocol::witness::WitnessCosignature;

use avalon_protocol::shard::CORE_SHARD_ID;

use crate::incremental_merkle::IncrementalMerkleTree;
use crate::retention::PruneReport;
use crate::sth::SignedTreeHead;
use crate::{merkle, sth, SettlementError, SettlementProvider};

/// All-zero hash, the `prev_hash` of the very first entry in the chain.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The fields that make up an entry's content hash, so recomputing one (at insert time from a
/// `ProtocolEvent`, or at verify time from a stored row) takes one argument. `pub` so a
/// cross-shard verifier can recompute a fetched entry's hash. The entry commits to
/// `payload_hash`, not the payload, so a row with a pruned payload still verifies.
pub struct EntryContent<'a> {
    pub seq: i64,
    pub event_id: Uuid,
    pub kind: &'a str,
    pub issuer: &'a str,
    pub subject: &'a str,
    /// Lowercase hex SHA-256 of the canonical payload ([`payload_hash_hex`]).
    pub payload_hash: &'a str,
    pub timestamp: time::OffsetDateTime,
    pub version: i32,
}

/// Lowercase hex SHA-256 of the canonical payload, the value an entry commits to.
pub fn payload_hash_hex(payload: &serde_json::Value) -> Result<String, CanonicalPayloadError> {
    Ok(hex::encode(ledger_entry::payload_hash(payload)?))
}

/// Whether a surviving payload hashes to the entry's `payload_hash`; a pruned payload (`None`) has
/// nothing to contradict the hash and passes.
pub fn payload_matches(payload: Option<&serde_json::Value>, payload_hash: &str) -> bool {
    payload.is_none_or(|p| payload_hash_hex(p).is_ok_and(|h| h == payload_hash))
}

/// The entry hash (hex) for `content` under `network_id`, `shard_id` and `prev_hash`; see
/// [`avalon_protocol::ledger_entry`] for the layout.
pub fn hash_entry(
    network_id: &str,
    shard_id: &str,
    prev_hash: &str,
    content: &EntryContent<'_>,
) -> Result<String, EntryHashError> {
    let prev = ledger_entry::parse_hash("prev_hash", prev_hash)?;
    let payload_hash = ledger_entry::parse_hash("payload_hash", content.payload_hash)?;
    let hash = ledger_entry::entry_hash(&EntryHashInput {
        network_id,
        shard_id,
        seq: u64::try_from(content.seq).map_err(|_| EntryHashError::OutOfRange("seq"))?,
        prev_hash: &prev,
        event_id: content.event_id,
        kind: content.kind,
        issuer: content.issuer,
        subject: content.subject,
        payload_hash: &payload_hash,
        timestamp_micros: ledger_entry::timestamp_micros(content.timestamp)?,
        version: u16::try_from(content.version)
            .map_err(|_| EntryHashError::OutOfRange("version"))?,
    })?;
    Ok(hex::encode(hash))
}

/// Everything checkable about one stored entry: its payload (when it survives) matches
/// `payload_hash`, and the content hashes to `entry_hash`.
pub fn entry_content_intact(
    network_id: &str,
    shard_id: &str,
    prev_hash: &str,
    entry_hash: &str,
    payload: Option<&serde_json::Value>,
    content: &EntryContent<'_>,
) -> bool {
    payload_matches(payload, content.payload_hash)
        && hash_entry(network_id, shard_id, prev_hash, content).is_ok_and(|h| h == entry_hash)
}

/// The payload as stored in the ledger: the event's own payload with its
/// identity-chain position (when it has one) embedded under a reserved key.
fn stored_payload(event: &ProtocolEvent) -> serde_json::Value {
    match &event.identity_chain {
        Some(position) => {
            avalon_protocol::identity_chain_wire::embed_position(&event.payload, position)
        }
        None => event.payload.clone(),
    }
}

/// Rejects the first event in `batch` the entry layout cannot carry, before anything is written.
fn check_batch(batch: &EventBatch) -> Result<(), SettlementError> {
    for event in &batch.events {
        ledger_entry::layout_version(event.version).map_err(|_| {
            SettlementError::UnsupportedEntryVersion {
                version: event.version,
            }
        })?;
    }
    Ok(())
}

/// The event time as stored: whole microseconds, so what is hashed equals what is bound.
fn stored_timestamp(event: &ProtocolEvent) -> Result<time::OffsetDateTime, SettlementError> {
    ledger_entry::floor_to_micros(event.timestamp)
        .map_err(|e| SettlementError::InvalidEntry(e.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn hash_event(
    network_id: &str,
    shard_id: &str,
    seq: i64,
    prev_hash: &str,
    event: &ProtocolEvent,
    payload_hash: &str,
) -> Result<String, SettlementError> {
    let timestamp = stored_timestamp(event)?;
    hash_entry(
        network_id,
        shard_id,
        prev_hash,
        &EntryContent {
            seq,
            event_id: event.id,
            kind: &event.kind,
            issuer: event.issuer.as_str(),
            subject: event.subject.as_str(),
            payload_hash,
            timestamp,
            version: i32::try_from(event.version).unwrap_or(i32::MAX),
        },
    )
    .map_err(|e| SettlementError::InvalidEntry(e.to_string()))
}

/// Recomputes a batch's *chain tip* (not its Merkle `batch_root` — see
/// [`crate::merkle`] for that) the same way `commit` produced it in the
/// first place: replay the sequential hash chain across the batch's own
/// entries, in order, starting from the hash the batch extended (its first
/// entry's stored `prev_hash` — the ledger's tip immediately before this
/// batch was committed, not verified here since that's a cross-batch
/// concern, not this batch's own integrity).
///
/// A pure, DB-free function on purpose, directly unit-testable without
/// Postgres. `PostgresSettlementProvider::verify` no longer calls this
/// directly — it needs a *per-entry* link/content check, not an
/// all-or-nothing batch replay, so a batch with one pruned entry doesn't
/// lose tamper detection for its other entries — see `verify`'s own doc
/// comment) but it stays here as a from-scratch reference implementation
/// these tests check `verify`'s per-entry logic against, since both are
/// meant to agree exactly when every entry's payload is present.
#[cfg(test)]
fn recompute_batch_root(
    network_id: &str,
    entering_prev_hash: &str,
    entries: &[EntryContent<'_>],
) -> String {
    let mut prev = entering_prev_hash.to_string();
    for content in entries {
        prev = hash_entry(network_id, CORE_SHARD_ID, &prev, content).unwrap();
    }
    prev
}

/// Shared row-mapping for `signed_tree_heads` — used by both
/// `latest_signed_tree_head` and `signed_tree_head_at` so there's exactly
/// one place that knows the column layout.
fn sth_from_row(row: sqlx::postgres::PgRow) -> Result<SignedTreeHead, SettlementError> {
    let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
    Ok(SignedTreeHead {
        tree_size: row.try_get("tree_size").map_err(get)?,
        root_hash: row.try_get("root_hash").map_err(get)?,
        network_id: row.try_get("network_id").map_err(get)?,
        signing_key_id: row.try_get("signing_key_id").map_err(get)?,
        signature: row.try_get("signature").map_err(get)?,
        created_at: row.try_get("created_at").map_err(get)?,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    /// The ledger already has a genesis row, and it doesn't match what this
    /// process is configured to expect. Refuse to construct a provider at
    /// all rather than let a mismatched process touch this ledger.
    #[error(
        "ledger genesis network_id `{stored}` does not match configured AVALON_NETWORK_ID `{configured}` — refusing to start against the wrong network"
    )]
    Mismatch { stored: String, configured: String },
    #[error("storage error: {0}")]
    Storage(String),
}

/// The in-memory leaf-hash prefix plus its incrementally-maintained Merkle
/// tree, kept together so they can never drift apart. Rebuilt
/// from Postgres (`ledger_entries.entry_hash`) whenever a process starts, or
/// whenever `commit`/`entry_hashes_up_to` find it isn't caught up to what's
/// actually been committed — see [`PostgresSettlementProvider::leaf_cache`]'s
/// doc comment for why that fallback exists and when it triggers.
#[derive(Default)]
struct LedgerCache {
    leaves: Vec<String>,
    tree: IncrementalMerkleTree,
}

impl LedgerCache {
    /// Appends already-known hashes to both `leaves` and `tree` — O(log n)
    /// per hash, used on the fast (sole-writer) path.
    fn extend(&mut self, new_hashes: &[String]) -> Result<(), String> {
        for hash in new_hashes {
            let bytes = hex::decode(hash).map_err(|e| e.to_string())?;
            self.tree.append(&bytes);
            self.leaves.push(hash.clone());
        }
        Ok(())
    }

    /// Replaces both `leaves` and `tree` from a freshly-fetched full prefix
    /// — the fallback path when the cache can't be trusted to be caught up.
    fn rebuild(&mut self, all_hashes: Vec<String>) -> Result<(), String> {
        let mut tree = IncrementalMerkleTree::new();
        for hash in &all_hashes {
            let bytes = hex::decode(hash).map_err(|e| e.to_string())?;
            tree.append(&bytes);
        }
        self.tree = tree;
        self.leaves = all_hashes;
        Ok(())
    }
}

#[derive(Clone)]
pub struct PostgresSettlementProvider {
    pool: PgPool,
    network_id: String,
    /// The shard this ledger is the log of; part of every entry hash.
    shard_id: String,
    /// In-memory cache of the committed leaf-hash prefix plus its Merkle
    /// tree, oldest-first — backs [`Self::entry_hashes_up_to`],
    /// [`Self::root_at`], [`Self::inclusion_proof`], and
    /// [`Self::consistency_proof`]. Safe to cache indefinitely because
    /// `ledger_entries` (the *authority's* table this struct reads, distinct
    /// from a mirror's separate `mirrored_entries` — see `crate::mirror`) is
    /// genuinely append-only: nothing in this codebase ever updates or
    /// deletes a committed row, so a cached prefix never goes stale, only
    /// grows. `Arc<RwLock<_>>` rather than a plain field because `Self` is
    /// `Clone`d per request (see `AppState`) and every clone must observe
    /// the same cache.
    ///
    /// This assumes the current process is the ledger's sole writer —
    /// `commit`/`entry_hashes_up_to` verify that assumption against the real
    /// row count on every use and fall back to a correct-but-O(n) rebuild
    /// from Postgres whenever it doesn't hold (e.g. another node wrote
    /// concurrently, or right after process start). Rebuilt in memory, not
    /// persisted in Postgres — see `docs/projects/backend-server/architecture/settlement.md` for
    /// that tradeoff.
    leaf_cache: std::sync::Arc<tokio::sync::RwLock<LedgerCache>>,
}

impl PostgresSettlementProvider {
    /// Low-level constructor for callers that already know (or don't care
    /// about) the ledger's genesis network_id — tests against a throwaway
    /// database, and read-only CLI inspection paired with
    /// [`Self::read_genesis_network_id`]. Does **not** create or verify a
    /// `chain_genesis` row; use [`Self::connect_core_shard`] at real process startup,
    /// where that enforcement actually matters.
    /// Binds the ledger to the `core` shard; a node authoring another shard must follow with
    /// [`Self::with_shard_id`].
    pub fn new_core_shard(pool: PgPool, network_id: impl Into<String>) -> Self {
        Self {
            pool,
            network_id: network_id.into(),
            shard_id: CORE_SHARD_ID.to_string(),
            leaf_cache: std::sync::Arc::new(tokio::sync::RwLock::new(LedgerCache::default())),
        }
    }

    /// Reads the ledger's genesis `network_id` without creating one —
    /// `None` if this database has never been booted against by
    /// [`Self::connect_core_shard`]. For read-only diagnostics (`avalon inspect-ledger`)
    /// that want to display which network they're pointed at without
    /// asserting anything about it.
    pub async fn read_genesis_network_id(pool: &PgPool) -> Result<Option<String>, SettlementError> {
        sqlx::query_scalar("SELECT network_id FROM chain_genesis LIMIT 1")
            .fetch_optional(pool)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))
    }

    /// The real boot-time entry point. If this database has no
    /// `chain_genesis` row yet, this is genesis: `expected_network_id` is
    /// written once and never touched again. If a row already exists, it
    /// must match `expected_network_id` exactly, or this returns
    /// `GenesisError::Mismatch` instead of a provider — the caller (see
    /// `avalon-server`'s `main.rs`) is expected to treat that as fatal and
    /// exit before binding a listener, never as a warning to log past.
    /// Like [`Self::new_core_shard`] for the shard, plus the genesis check.
    pub async fn connect_core_shard(
        pool: PgPool,
        expected_network_id: &str,
    ) -> Result<Self, GenesisError> {
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| GenesisError::Storage(e.to_string()))?;

        let existing: Option<String> =
            sqlx::query_scalar("SELECT network_id FROM chain_genesis LIMIT 1 FOR UPDATE")
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| GenesisError::Storage(e.to_string()))?;

        match existing {
            None => {
                sqlx::query("INSERT INTO chain_genesis (network_id) VALUES ($1)")
                    .bind(expected_network_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| GenesisError::Storage(e.to_string()))?;
                tx.commit()
                    .await
                    .map_err(|e| GenesisError::Storage(e.to_string()))?;
                Ok(Self {
                    pool,
                    network_id: expected_network_id.to_string(),
                    shard_id: CORE_SHARD_ID.to_string(),
                    leaf_cache: std::sync::Arc::new(tokio::sync::RwLock::new(
                        LedgerCache::default(),
                    )),
                })
            }
            Some(stored) if stored == expected_network_id => {
                tx.commit()
                    .await
                    .map_err(|e| GenesisError::Storage(e.to_string()))?;
                Ok(Self {
                    pool,
                    network_id: stored,
                    shard_id: CORE_SHARD_ID.to_string(),
                    leaf_cache: std::sync::Arc::new(tokio::sync::RwLock::new(
                        LedgerCache::default(),
                    )),
                })
            }
            Some(stored) => Err(GenesisError::Mismatch {
                stored,
                configured: expected_network_id.to_string(),
            }),
        }
    }

    /// Binds this ledger to the shard it logs (default `core`); callers set it right after
    /// construction, before any entry is hashed or verified.
    pub fn with_shard_id(mut self, shard_id: impl Into<String>) -> Self {
        self.shard_id = shard_id.into();
        self
    }

    /// The shard this ledger logs; every entry hash is rooted in it.
    pub fn shard_id(&self) -> &str {
        &self.shard_id
    }

    /// The network identity this provider is bound to — every
    /// hash it computes or verifies is rooted in this value.
    pub fn network_id(&self) -> &str {
        &self.network_id
    }

    /// This provider's underlying pool — for a caller that needs a raw
    /// query this trait doesn't expose (e.g. `avalon-server`'s
    /// `equivocation` module resolving a shard's `issuer_keys` rows),
    /// rather than threading a second, separately-cloned `PgPool` alongside
    /// `Self` everywhere.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The highest committed `seq`, 0 for an empty ledger.
    async fn max_seq<'e>(
        &self,
        executor: impl sqlx::PgExecutor<'e>,
    ) -> Result<i64, SettlementError> {
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM ledger_entries")
            .fetch_one(executor)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))
    }

    /// Takes an executor rather than always using `self.pool` — a caller
    /// already holding an open transaction (e.g.
    /// `insert_batch_and_compute_root`) must pass that transaction's own
    /// connection, never acquire a second one from the pool while the
    /// first is still checked out.
    async fn tip_hash<'e>(
        &self,
        executor: impl sqlx::PgExecutor<'e>,
    ) -> Result<String, SettlementError> {
        let row = sqlx::query("SELECT entry_hash FROM ledger_entries ORDER BY seq DESC LIMIT 1")
            .fetch_optional(executor)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        Ok(match row {
            Some(row) => row
                .try_get("entry_hash")
                .map_err(|e| SettlementError::Storage(e.to_string()))?,
            None => GENESIS_HASH.to_string(),
        })
    }

    /// Every entry in the ledger, oldest first, for local inspection —
    /// `avalon inspect-ledger` uses this. Deliberately not part of the
    /// `SettlementProvider` trait: that trait stays minimal (commit/verify/
    /// get_commitment), and "dump everything" is a debug affordance, not a
    /// protocol-level operation other crates should depend on.
    ///
    /// Each entry is independently re-verified here — its content is
    /// rehashed and compared against its stored `entry_hash`, and that hash
    /// is compared against the next entry's `prev_hash` — so tampering with
    /// a row's content (not just its links) is actually caught, not just
    /// tampering with the chain pointers themselves.
    pub async fn list_entries(&self) -> Result<Vec<LedgerEntryView>, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT seq, event_id, kind, issuer, subject, payload, payload_hash, payload_pruned_at, event_timestamp, version, prev_hash, entry_hash, batch_id
            FROM ledger_entries
            ORDER BY seq ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut entries = Vec::with_capacity(rows.len());
        let mut expected_prev = GENESIS_HASH.to_string();
        for row in rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            let event_id: Uuid = row.try_get("event_id").map_err(get)?;
            let kind: String = row.try_get("kind").map_err(get)?;
            let issuer: String = row.try_get("issuer").map_err(get)?;
            let subject: String = row.try_get("subject").map_err(get)?;
            let payload: Option<serde_json::Value> = row.try_get("payload").map_err(get)?;
            let payload_hash: String = row.try_get("payload_hash").map_err(get)?;
            let seq: i64 = row.try_get("seq").map_err(get)?;
            let payload_pruned_at: Option<time::OffsetDateTime> =
                row.try_get("payload_pruned_at").map_err(get)?;
            let event_timestamp: time::OffsetDateTime =
                row.try_get("event_timestamp").map_err(get)?;
            let version: i32 = row.try_get("version").map_err(get)?;
            let prev_hash: String = row.try_get("prev_hash").map_err(get)?;
            let entry_hash: String = row.try_get("entry_hash").map_err(get)?;
            let batch_id: Uuid = row.try_get("batch_id").map_err(get)?;

            let link_intact = prev_hash == expected_prev;
            // The entry hash is recomputed from `payload_hash`, so it verifies even for a pruned
            // row; a surviving payload must also hash to `payload_hash`.
            let content_intact = entry_content_intact(
                &self.network_id,
                &self.shard_id,
                &prev_hash,
                &entry_hash,
                payload.as_ref(),
                &EntryContent {
                    seq,
                    event_id,
                    kind: &kind,
                    issuer: &issuer,
                    subject: &subject,
                    payload_hash: &payload_hash,
                    timestamp: event_timestamp,
                    version,
                },
            );
            expected_prev = entry_hash.clone();

            entries.push(LedgerEntryView {
                seq,
                event_id,
                kind,
                issuer,
                subject,
                payload,
                payload_hash,
                payload_pruned: payload_pruned_at.is_some(),
                version,
                event_timestamp,
                prev_hash,
                entry_hash,
                batch_id,
                chain_intact: content_intact && link_intact,
            });
        }
        Ok(entries)
    }

    /// Entries with `seq` strictly greater than `since_seq`, oldest first,
    /// up to `limit` rows — issue #299's bulk entries endpoint (`GET
    /// /ledger/entries?since_seq={n}&limit={m}`), the read path a mirror
    /// needs to hold real ledger content, not just verify STHs (#211 only
    /// ever exposed bare `entry_hash` values via `entry_hashes_up_to`, not
    /// full entry content).
    ///
    /// Unlike [`Self::list_entries`], this does **not** recompute or verify
    /// the hash-chain link or content hash for each row — `chain_intact` is
    /// always `true` here and must never be trusted as a real check: a
    /// windowed query has no way to validate a page's first row's
    /// `prev_hash` against a predecessor it wasn't asked to fetch. A caller
    /// that needs verified content (a mirror backfilling, per #40's
    /// design) must independently verify each entry against a
    /// signature-checked STH via `GET /ledger/proof/inclusion` before
    /// accepting it — this endpoint alone is not sufficient trust.
    pub async fn list_entries_since(
        &self,
        since_seq: i64,
        limit: i64,
    ) -> Result<Vec<LedgerEntryView>, SettlementError> {
        self.list_entries_since_for_subject(since_seq, limit, None)
            .await
    }

    /// Same pagination/ordering semantics as [`Self::list_entries_since`],
    /// pre-filtered to one `subject` when `Some` (making the common
    /// one-subject-at-a-time case cheap).
    /// Filtering by subject is purely a read-side convenience — it never
    /// changes an entry's hash-chain position, and `chain_intact` carries
    /// the exact same "not verified here" caveat
    /// [`Self::list_entries_since`]'s own doc comment describes; inclusion
    /// proofs for a filtered row still verify against the same global tree
    /// regardless of this filter.
    pub async fn list_entries_since_for_subject(
        &self,
        since_seq: i64,
        limit: i64,
        subject: Option<&str>,
    ) -> Result<Vec<LedgerEntryView>, SettlementError> {
        let rows = match subject {
            Some(subject) => {
                sqlx::query(
                    r#"
                    SELECT seq, event_id, kind, issuer, subject, payload, payload_hash, payload_pruned_at, event_timestamp, version, prev_hash, entry_hash, batch_id
                    FROM ledger_entries
                    WHERE seq > $1 AND subject = $3
                    ORDER BY seq ASC
                    LIMIT $2
                    "#,
                )
                .bind(since_seq)
                .bind(limit)
                .bind(subject)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"
                    SELECT seq, event_id, kind, issuer, subject, payload, payload_hash, payload_pruned_at, event_timestamp, version, prev_hash, entry_hash, batch_id
                    FROM ledger_entries
                    WHERE seq > $1
                    ORDER BY seq ASC
                    LIMIT $2
                    "#,
                )
                .bind(since_seq)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            let payload_pruned_at: Option<time::OffsetDateTime> =
                row.try_get("payload_pruned_at").map_err(get)?;
            entries.push(LedgerEntryView {
                seq: row.try_get("seq").map_err(get)?,
                event_id: row.try_get("event_id").map_err(get)?,
                kind: row.try_get("kind").map_err(get)?,
                issuer: row.try_get("issuer").map_err(get)?,
                subject: row.try_get("subject").map_err(get)?,
                payload: row.try_get("payload").map_err(get)?,
                payload_hash: row.try_get("payload_hash").map_err(get)?,
                payload_pruned: payload_pruned_at.is_some(),
                version: row.try_get("version").map_err(get)?,
                event_timestamp: row.try_get("event_timestamp").map_err(get)?,
                prev_hash: row.try_get("prev_hash").map_err(get)?,
                entry_hash: row.try_get("entry_hash").map_err(get)?,
                batch_id: row.try_get("batch_id").map_err(get)?,
                chain_intact: true,
            });
        }
        Ok(entries)
    }

    /// Every committed batch, oldest first — `avalon inspect-ledger` uses
    /// this alongside [`Self::list_entries`] to print batch boundaries and
    /// each batch's root. Like `list_entries`, deliberately not part of the
    /// `SettlementProvider` trait: it's a debug/inspection affordance over
    /// the whole ledger, not a per-batch protocol operation.
    pub async fn list_batches(&self) -> Result<Vec<LedgerBatchView>, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT batch_id, first_seq, last_seq, batch_root, committed_at
            FROM ledger_batches
            ORDER BY first_seq ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut batches = Vec::with_capacity(rows.len());
        for row in rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            batches.push(LedgerBatchView {
                batch_id: row.try_get("batch_id").map_err(get)?,
                first_seq: row.try_get("first_seq").map_err(get)?,
                last_seq: row.try_get("last_seq").map_err(get)?,
                batch_root: row.try_get("batch_root").map_err(get)?,
                committed_at: row.try_get("committed_at").map_err(get)?,
            });
        }
        Ok(batches)
    }

    /// Every Signed Tree Head, oldest (`tree_size`) first —
    /// `avalon inspect-ledger` uses the latest one to report STH
    /// verification (Merkle recompute + Ed25519 signature check) alongside
    /// the hash-chain check `list_entries` already reports. Like
    /// `list_entries`/`list_batches`, deliberately not part of the
    /// `SettlementProvider` trait: a debug/inspection affordance, not a
    /// protocol-level operation other crates should depend on.
    pub async fn list_signed_tree_heads(&self) -> Result<Vec<SignedTreeHead>, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT tree_size, root_hash, network_id, signing_key_id, signature, created_at
            FROM signed_tree_heads
            WHERE network_id = $1
            ORDER BY tree_size ASC
            "#,
        )
        .bind(&self.network_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut heads = Vec::with_capacity(rows.len());
        for row in rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            heads.push(SignedTreeHead {
                tree_size: row.try_get("tree_size").map_err(get)?,
                root_hash: row.try_get("root_hash").map_err(get)?,
                network_id: row.try_get("network_id").map_err(get)?,
                signing_key_id: row.try_get("signing_key_id").map_err(get)?,
                signature: row.try_get("signature").map_err(get)?,
                created_at: row.try_get("created_at").map_err(get)?,
            });
        }
        Ok(heads)
    }

    /// The settlement-state checkpoint: a thin, purely-naming
    /// wrapper over [`Self::latest_signed_tree_head`], satisfying the
    /// "periodic durable-state checkpoint" ask for the commitment layer
    /// only — not the indexer/projection read-model snapshot half (still
    /// open). See `docs/projects/backend-server/architecture/nodes.md`'s
    /// "Settlement-state checkpoint" section for why the latest
    /// `SignedTreeHead` already satisfies this with no new storage.
    pub async fn checkpoint(&self) -> Result<Option<SignedTreeHead>, SettlementError> {
        self.latest_signed_tree_head().await
    }

    /// The most recent Signed Tree Head (highest `tree_size`) — issue #211's
    /// `GET /ledger/sth/latest`. `None` only before the very first batch has
    /// ever been committed.
    pub async fn latest_signed_tree_head(&self) -> Result<Option<SignedTreeHead>, SettlementError> {
        let row = sqlx::query(
            r#"
            SELECT tree_size, root_hash, network_id, signing_key_id, signature, created_at
            FROM signed_tree_heads
            WHERE network_id = $1
            ORDER BY tree_size DESC
            LIMIT 1
            "#,
        )
        .bind(&self.network_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.map(sth_from_row).transpose()
    }

    /// The Signed Tree Head at exactly `tree_size` — issue #211's
    /// `GET /ledger/sth/{tree_size}`, needed to chain consistency proofs
    /// (a mirror verifying an STH history holds one of these per batch it
    /// has observed). `None` if no batch ever closed at exactly that size —
    /// callers must not fabricate one; `tree_size` is a PRIMARY KEY, so
    /// intermediate ledger sizes between batches simply have no STH, which
    /// is a real "not found," not a storage error.
    pub async fn signed_tree_head_at(
        &self,
        tree_size: i64,
    ) -> Result<Option<SignedTreeHead>, SettlementError> {
        let row = sqlx::query(
            r#"
            SELECT tree_size, root_hash, network_id, signing_key_id, signature, created_at
            FROM signed_tree_heads
            WHERE tree_size = $1 AND network_id = $2
            "#,
        )
        .bind(tree_size)
        .bind(&self.network_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.map(sth_from_row).transpose()
    }

    /// Stores one witness's cosignature over an already-known STH for
    /// `shard_id` — the storage half of `avalon_protocol::witness`/
    /// `cosigned_sth`'s primitives; a node deciding to cosign others' heads,
    /// and gossiping cosignatures around, are separate concerns from this
    /// storage layer. Idempotent on a replayed
    /// `(network_id, shard_id, tree_size, witness_key_id)` — re-receiving
    /// the same witness's cosignature for a head this node already has
    /// (e.g. via gossip from more than one peer) is a no-op, not a
    /// conflict; two *different* cosignatures for the same key at the same
    /// tree_size would violate that witness's own no-double-cosign rule and
    /// are rejected outright rather than silently overwritten.
    ///
    /// `shard_id` scopes this independently of `network_id`/`tree_size` —
    /// every shard under a network has its own tree_size numbering, applied
    /// here the same way `observed_sths`/`mirrored_entries` already are, so
    /// two unrelated shards legitimately reaching the same `tree_size` must
    /// never share cosignature rows.
    pub async fn store_witness_cosignature(
        &self,
        shard_id: &str,
        cosig: &WitnessCosignature,
    ) -> Result<(), SettlementError> {
        let outcome = sqlx::query(
            r#"
            INSERT INTO witness_cosignatures
                (network_id, shard_id, tree_size, witness_key_id, root_hash, author_created_at, observed_at, signature)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (network_id, shard_id, tree_size, witness_key_id) DO NOTHING
            "#,
        )
        .bind(&cosig.network_id)
        .bind(shard_id)
        .bind(cosig.tree_size)
        .bind(&cosig.witness_key_id)
        .bind(&cosig.root_hash)
        .bind(cosig.author_created_at)
        .bind(cosig.observed_at)
        .bind(&cosig.signature)
        .execute(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        if outcome.rows_affected() == 1 {
            return Ok(());
        }

        // A row already exists: a replay, a relayed re-attestation refresh,
        // or a genuine equivocation — tell those apart below.
        let row = sqlx::query(
            r#"
            SELECT root_hash, signature, observed_at FROM witness_cosignatures
            WHERE network_id = $1 AND shard_id = $2 AND tree_size = $3 AND witness_key_id = $4
            "#,
        )
        .bind(&cosig.network_id)
        .bind(shard_id)
        .bind(cosig.tree_size)
        .bind(&cosig.witness_key_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let existing_root_hash: String = row
            .try_get("root_hash")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let existing_signature: String = row
            .try_get("signature")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let existing_observed_at: time::OffsetDateTime = row
            .try_get("observed_at")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        if existing_signature == cosig.signature {
            return Ok(());
        }
        if existing_root_hash == cosig.root_hash && existing_observed_at < cosig.observed_at {
            // Same witness, same head, a fresher observation — a relayed
            // re-attestation, not an equivocation. Update in place.
            self.refresh_witness_cosignature(shard_id, cosig).await?;
            return Ok(());
        }
        Err(SettlementError::Storage(format!(
            "witness {} already cosigned tree_size {} for network {} shard {} with a \
             different signature — refusing to overwrite a possible equivocation",
            cosig.witness_key_id, cosig.tree_size, cosig.network_id, shard_id
        )))
    }

    /// Replaces this witness's stored cosignature for a head with a newer one over the same
    /// root and author timestamp. Returns whether a row was updated; a different root, author
    /// timestamp or an older `observed_at` never overwrites, so a refresh cannot become a
    /// second, conflicting cosignature.
    pub async fn refresh_witness_cosignature(
        &self,
        shard_id: &str,
        cosig: &WitnessCosignature,
    ) -> Result<bool, SettlementError> {
        let outcome = sqlx::query(
            r#"
            UPDATE witness_cosignatures
            SET observed_at = $6, signature = $7
            WHERE network_id = $1 AND shard_id = $2 AND tree_size = $3 AND witness_key_id = $4
              AND root_hash = $5 AND author_created_at = $8 AND observed_at < $6
            "#,
        )
        .bind(&cosig.network_id)
        .bind(shard_id)
        .bind(cosig.tree_size)
        .bind(&cosig.witness_key_id)
        .bind(&cosig.root_hash)
        .bind(cosig.observed_at)
        .bind(&cosig.signature)
        .bind(cosig.author_created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        Ok(outcome.rows_affected() == 1)
    }

    /// Every stored cosignature for one shard's tree head at `tree_size`,
    /// in no particular order — the read half of [`Self::store_witness_cosignature`].
    pub async fn list_witness_cosignatures(
        &self,
        network_id: &str,
        shard_id: &str,
        tree_size: i64,
    ) -> Result<Vec<WitnessCosignature>, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT tree_size, root_hash, network_id, author_created_at, witness_key_id, observed_at, signature
            FROM witness_cosignatures
            WHERE network_id = $1 AND shard_id = $2 AND tree_size = $3
            "#,
        )
        .bind(network_id)
        .bind(shard_id)
        .bind(tree_size)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut cosigs = Vec::with_capacity(rows.len());
        for row in rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            cosigs.push(WitnessCosignature {
                tree_size: row.try_get("tree_size").map_err(get)?,
                root_hash: row.try_get("root_hash").map_err(get)?,
                network_id: row.try_get("network_id").map_err(get)?,
                author_created_at: row.try_get("author_created_at").map_err(get)?,
                witness_key_id: row.try_get("witness_key_id").map_err(get)?,
                observed_at: row.try_get("observed_at").map_err(get)?,
                signature: row.try_get("signature").map_err(get)?,
            });
        }
        Ok(cosigs)
    }

    /// Assembles a [`CosignedTreeHead`] for `shard_id`/`tree_size` from
    /// stored state: the author STH ([`Self::signed_tree_head_at`]) plus
    /// every stored cosignature for it. `None` if this node has no STH at
    /// that `tree_size` at all — the same "not found, not an error"
    /// contract `signed_tree_head_at` already has. Read-only assembly only;
    /// callers still run the result through
    /// `avalon_protocol::cosigned_sth::verify_cosigned_tree_head` against
    /// their own known list, exactly as they would for a head learned via
    /// gossip instead of storage.
    pub async fn cosigned_tree_head_at(
        &self,
        shard_id: &str,
        tree_size: i64,
    ) -> Result<Option<CosignedTreeHead>, SettlementError> {
        let Some(sth) = self.signed_tree_head_at(tree_size).await? else {
            return Ok(None);
        };
        let cosignatures = self
            .list_witness_cosignatures(&sth.network_id, shard_id, tree_size)
            .await?;
        Ok(Some(CosignedTreeHead { sth, cosignatures }))
    }

    /// The first `tree_size` entries' `entry_hash`, oldest first — the
    /// exact leaf set [`crate::merkle`]'s proof functions need, for issue
    /// #211's inclusion/consistency proof endpoints. Deliberately
    /// count-based (`ORDER BY seq ASC LIMIT`), never `WHERE seq <= tree_size`
    /// — `seq` can have gaps (see module doc comment), so a raw seq-value
    /// bound would return *fewer* than `tree_size` rows once one exists,
    /// silently desynchronizing this leaf list from the `tree_size` a
    /// caller already validated against a real, signed STH. `LIMIT`
    /// guarantees exactly `tree_size` rows whenever that many exist,
    /// regardless of what the underlying `seq` values happen to be.
    pub async fn entry_hashes_up_to(&self, tree_size: i64) -> Result<Vec<String>, SettlementError> {
        self.ensure_cache_covers(tree_size).await?;
        let cached = self.leaf_cache.read().await;
        Ok(cached.leaves[..tree_size as usize].to_vec())
    }

    /// Drops the cached leaves and tree; the next read rebuilds them from
    /// `ledger_entries`. Called when a transaction that had already extended
    /// the cache does not commit.
    async fn reset_leaf_cache(&self) {
        *self.leaf_cache.write().await = LedgerCache::default();
    }

    /// Grows `leaf_cache` (leaves + tree together) to cover `tree_size` if
    /// it doesn't already — the fast/slow path every cache-backed read
    /// below shares. No-op if the cache already covers `tree_size`.
    async fn ensure_cache_covers(&self, tree_size: i64) -> Result<(), SettlementError> {
        {
            let cached = self.leaf_cache.read().await;
            if cached.leaves.len() as i64 >= tree_size {
                return Ok(());
            }
        }
        // Holds the write lock across the query so concurrent callers past
        // the fast path above don't all fetch redundantly.
        let mut cached = self.leaf_cache.write().await;
        if cached.leaves.len() as i64 >= tree_size {
            return Ok(());
        }
        let rows = sqlx::query("SELECT entry_hash FROM ledger_entries ORDER BY seq ASC LIMIT $1")
            .bind(tree_size)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let hashes = rows
            .into_iter()
            .map(|row| {
                row.try_get::<String, _>("entry_hash")
                    .map_err(|e| SettlementError::Storage(e.to_string()))
            })
            .collect::<Result<Vec<String>, SettlementError>>()?;
        if hashes.len() > cached.leaves.len() {
            cached.rebuild(hashes).map_err(SettlementError::Storage)?;
        }
        Ok(())
    }

    /// The RFC 6962 root at `tree_size` — O(log n) via the incremental tree,
    /// `None` if `tree_size` exceeds what's been committed.
    pub async fn root_at(&self, tree_size: i64) -> Result<Option<[u8; 32]>, SettlementError> {
        self.ensure_cache_covers(tree_size).await?;
        let cached = self.leaf_cache.read().await;
        Ok(cached.tree.root(tree_size as u64))
    }

    /// The leaf's hex hash plus its O(log n) RFC 6962 inclusion (audit)
    /// path at `tree_size` — `entry_hashes_up_to` +
    /// `merkle::inclusion_proof_of_hex_hashes`'s O(n) equivalent.
    pub async fn inclusion_proof(
        &self,
        leaf_index: i64,
        tree_size: i64,
    ) -> Result<(String, Vec<[u8; 32]>), SettlementError> {
        self.ensure_cache_covers(tree_size).await?;
        let cached = self.leaf_cache.read().await;
        let leaf_hash_hex = cached.leaves[leaf_index as usize].clone();
        let proof = cached
            .tree
            .inclusion_proof(leaf_index as u64, tree_size as u64)
            .map_err(SettlementError::Storage)?;
        Ok((leaf_hash_hex, proof))
    }

    /// The O(log n) RFC 6962 consistency proof from `first` to `second`
    /// leaves — `entry_hashes_up_to` +
    /// `merkle::consistency_proof_of_hex_hashes`'s O(n) equivalent.
    pub async fn consistency_proof(
        &self,
        first: i64,
        second: i64,
    ) -> Result<Vec<[u8; 32]>, SettlementError> {
        self.ensure_cache_covers(second).await?;
        let cached = self.leaf_cache.read().await;
        cached
            .tree
            .consistency_proof(first as u64, second as u64)
            .map_err(SettlementError::Storage)
    }

    /// How many entries have been committed so far — the true `tree_size`
    /// upper bound. Issue #211 uses this to give a clear "doesn't exist yet"
    /// error for a `tree_size`/`seq` request beyond what's actually been
    /// committed, rather than a confusing empty-proof or panic. Deliberately
    /// `COUNT(*)`, not `MAX(seq)` — those diverge once `seq` has a gap (see
    /// module doc comment), and `tree_size` is always a leaf count.
    pub async fn entry_count(&self) -> Result<i64, SettlementError> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM ledger_entries")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.try_get("count")
            .map_err(|e| SettlementError::Storage(e.to_string()))
    }

    /// The 0-indexed Merkle leaf position of the entry at `seq` — its rank
    /// among all committed entries ordered by `seq`, **not** `seq - 1`.
    /// `seq` is a real, stable row identifier (`GENERATED ALWAYS AS
    /// IDENTITY`), but it is not a dense, gap-free position: a batch commit
    /// that fails partway through and rolls back still permanently burns
    /// whatever `seq` values it had already allocated (Postgres identity/
    /// sequence advancement is not transactional), so treating `seq - 1` as
    /// a leaf index silently desyncs from the real tree the moment any gap
    /// exists. `None` if no entry has this exact `seq`.
    pub async fn leaf_index_for_seq(&self, seq: i64) -> Result<Option<i64>, SettlementError> {
        let row = sqlx::query(
            r#"
            SELECT (SELECT COUNT(*) FROM ledger_entries e2 WHERE e2.seq <= e1.seq) - 1 AS leaf_index
            FROM ledger_entries e1
            WHERE e1.seq = $1
            "#,
        )
        .bind(seq)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.map(|row| {
            row.try_get::<i64, _>("leaf_index")
                .map_err(|e| SettlementError::Storage(e.to_string()))
        })
        .transpose()
    }

    /// Issue #208 — how many entries currently still have a payload and
    /// were committed strictly before `cutoff`: exactly what
    /// [`Self::prune_payloads_older_than`] would act on if called right
    /// now. A dry-run count, used by `avalon prune-ledger` to report what
    /// a pruning pass *would* do before (and independent of) actually
    /// doing it.
    pub async fn prunable_entry_count(
        &self,
        cutoff: time::OffsetDateTime,
    ) -> Result<i64, SettlementError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM ledger_entries \
             WHERE committed_at < $1 AND payload_pruned_at IS NULL",
        )
        .bind(cutoff)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.try_get("count")
            .map_err(|e| SettlementError::Storage(e.to_string()))
    }

    /// Issue #569's archive-confirmation gating (see
    /// `avalon_server::retention`): the highest `seq` among entries that
    /// [`Self::prune_payloads_older_than`] would act on for this exact
    /// `cutoff` — the boundary a would-be archive peer must have already
    /// mirrored up through before a hot node prunes past it. `None` when
    /// nothing is prunable at this cutoff at all (nothing to confirm
    /// coverage for).
    pub async fn max_seq_before(
        &self,
        cutoff: time::OffsetDateTime,
    ) -> Result<Option<i64>, SettlementError> {
        let row = sqlx::query(
            "SELECT MAX(seq) AS max_seq FROM ledger_entries \
             WHERE committed_at < $1 AND payload_pruned_at IS NULL",
        )
        .bind(cutoff)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.try_get::<Option<i64>, _>("max_seq")
            .map_err(|e| SettlementError::Storage(e.to_string()))
    }

    /// Issue #208's actual pruning operation: discards (`NULL`s out) the
    /// `payload` of every entry committed strictly before `cutoff` that
    /// hasn't already been pruned, and stamps `payload_pruned_at`.
    ///
    /// **Only ever touches `payload`/`payload_pruned_at`.** Every column
    /// the hash chain and Merkle tree depend on — `seq`, `entry_hash`,
    /// `prev_hash`, `kind`, `issuer`, `subject`, `event_timestamp`,
    /// `version`, `batch_id` — is untouched, by construction: this is a
    /// single-column `UPDATE`, not a `DELETE`, so there is no code path
    /// here that could remove a row or a commitment-relevant column even
    /// by accident. See `crate::retention`'s module doc comment for why
    /// this is what makes payload pruning safe against the settlement
    /// commitment specifically (as opposed to safe against network-wide
    /// data loss, which milestone 1 cannot yet guarantee — that's a
    /// caller-level gate, not this function's job; see
    /// [`crate::retention::RetentionConfig::should_prune`]).
    ///
    /// Callers are expected to have already checked
    /// [`crate::retention::RetentionConfig::should_prune`] — this method
    /// itself does not re-check any config; it does exactly what it's
    /// asked, unconditionally, so it stays simple and directly testable.
    /// The gating (tier, opt-in flag) lives one layer up, in
    /// `avalon-server`'s retention worker and `avalon prune-ledger`.
    pub async fn prune_payloads_older_than(
        &self,
        cutoff: time::OffsetDateTime,
    ) -> Result<PruneReport, SettlementError> {
        let result = sqlx::query(
            "UPDATE ledger_entries SET payload = NULL, payload_pruned_at = now() \
             WHERE committed_at < $1 AND payload_pruned_at IS NULL",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        Ok(PruneReport {
            cutoff,
            pruned_count: result.rows_affected() as i64,
        })
    }

    /// Every entry issued by `issuer_prefix` (a `GlobalId` prefix, e.g.
    /// `identity:<id>:self:` — every verb an identity signs itself under
    /// shares that prefix, see `crates/server/src/friends.rs`'s
    /// `identity_ref`), newest first — the read path behind issue #121's
    /// "my activity" view.
    ///
    /// Deliberately a narrower, unverified read than [`Self::list_entries`]:
    /// no hash/chain-link recomputation, since that's only meaningful
    /// against the *full*, sequential ledger — a per-issuer slice is a
    /// convenience projection for a user looking at their own history,
    /// not a tamper-evidence check. `issuer_prefix` is caller-controlled
    /// but always server-constructed from an authenticated identity id, not
    /// arbitrary user input — see `handlers::my_history`.
    pub async fn list_entries_for_issuer_prefix(
        &self,
        issuer_prefix: &str,
    ) -> Result<Vec<IssuerHistoryEntry>, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT event_id, kind, subject, payload, event_timestamp
            FROM ledger_entries
            WHERE issuer LIKE $1
            ORDER BY seq DESC
            "#,
        )
        .bind(format!("{issuer_prefix}%"))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            entries.push(IssuerHistoryEntry {
                event_id: row.try_get("event_id").map_err(get)?,
                kind: row.try_get("kind").map_err(get)?,
                subject: row.try_get("subject").map_err(get)?,
                payload: row.try_get("payload").map_err(get)?,
                event_timestamp: row.try_get("event_timestamp").map_err(get)?,
            });
        }
        Ok(entries)
    }
}

/// One event from a single issuer's own history — a plain
/// projection, not a verified ledger entry; see
/// [`PostgresSettlementProvider::list_entries_for_issuer_prefix`].
pub struct IssuerHistoryEntry {
    pub event_id: Uuid,
    pub kind: String,
    pub subject: String,
    /// `None` if this row's payload has been pruned — a
    /// hot-tier node's "my activity" view degrades to showing that an
    /// event of this `kind` happened, without its content, rather than
    /// erroring or fabricating one.
    pub payload: Option<serde_json::Value>,
    pub event_timestamp: time::OffsetDateTime,
}

/// One ledger entry plus whether it's actually intact — computed by
/// `list_entries` — both that its own content still matches its claimed
/// hash, and that it correctly links to the entry before it. `payload` is
/// carried through mainly for `avalon inspect-ledger-full`; the concise
/// `avalon inspect-ledger` view doesn't print it.
pub struct LedgerEntryView {
    pub seq: i64,
    pub event_id: Uuid,
    pub kind: String,
    pub issuer: String,
    pub subject: String,
    /// `None` once this row's payload has been pruned (a
    /// hot-tier node with pruning enabled) — the entry itself, its hash,
    /// and its position in the chain/Merkle tree all survive regardless;
    /// only the content is gone. See [`Self::payload_pruned`].
    pub payload: Option<serde_json::Value>,
    /// Hex SHA-256 of the canonical payload; the entry hash commits to it, so it survives pruning.
    pub payload_hash: String,
    /// Whether this row's payload has been pruned. Redundant with
    /// `payload.is_none()` today, but kept as its own field — a payload
    /// legitimately being absent for some other reason in the future
    /// shouldn't silently be read as "pruned" by every caller of this
    /// struct.
    pub payload_pruned: bool,
    pub version: i32,
    pub event_timestamp: time::OffsetDateTime,
    pub prev_hash: String,
    pub entry_hash: String,
    pub batch_id: Uuid,
    /// Everything *checkable* about this entry checks out: the hash-chain
    /// link always, and content re-verification whenever the payload is
    /// still present. A pruned entry with an intact link still reports
    /// `true` here — the absence of its payload is not itself evidence of
    /// tampering (see `list_entries`'s doc comment).
    pub chain_intact: bool,
}

impl LedgerEntryView {
    /// Decodes this entry back into the same [`ProtocolEvent`] shape
    /// `outbox::drain_once` built it from — the read half of the
    /// settlement/indexer boundary, used by `avalon rebuild-index`
    /// to replay ledger history back through an `Indexer`. `None` for
    /// a pruned payload (the entry survives, but its content
    /// doesn't) or an `issuer`/`subject` that isn't a well-formed
    /// `GlobalId`, which should never happen for a genuine ledger entry but
    /// is handled as a skip, not a panic. Same conversion
    /// `mirror_watcher::protocol_event_from_mirrored` does for a
    /// peer-mirrored entry.
    pub fn to_protocol_event(&self) -> Option<ProtocolEvent> {
        let (payload, identity_chain) =
            avalon_protocol::identity_chain_wire::split_position(self.payload.clone()?);
        Some(ProtocolEvent {
            id: self.event_id,
            kind: self.kind.clone(),
            issuer: global_id_from_str(&self.issuer)?,
            subject: global_id_from_str(&self.subject)?,
            payload,
            timestamp: self.event_timestamp,
            version: u32::try_from(self.version).ok()?,
            identity_chain,
        })
    }
}

/// `GlobalId` derives `Deserialize` as a transparent newtype over `String`,
/// so this is the same round trip a `ProtocolEvent`'s `issuer`/`subject`
/// field already goes through at every other JSON boundary — there is no
/// public raw-string constructor on `GlobalId` itself.
fn global_id_from_str(raw: &str) -> Option<avalon_protocol::ids::GlobalId> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

/// One committed batch — the unit of settlement: entries are
/// hash-chained individually, but a batch is what `get_commitment` looks up
/// and what `avalon inspect-ledger` prints boundaries for. `batch_root` is
/// the real RFC 6962 Merkle Tree Hash of the whole ledger (not just this
/// batch's own entries), covering every entry committed so far
/// — not a per-batch sub-tree, and not a placeholder chain-tip
/// value. Its corresponding `tree_size` (the leaf *count*, not
/// `last_seq`) is recorded separately in `signed_tree_heads`, not on this
/// row — `seq` can have gaps (see module doc comment), so `last_seq` here
/// is a row-identity bookmark, never a leaf count.
pub struct LedgerBatchView {
    pub batch_id: Uuid,
    pub first_seq: i64,
    pub last_seq: i64,
    pub batch_root: String,
    pub committed_at: time::OffsetDateTime,
}

impl PostgresSettlementProvider {
    /// `commit`/`finalize`'s shared idempotency check —
    /// `Ok(None)` if `batch_id` has never been committed on this node,
    /// `Ok(Some(commitment))` if it has (a replayed retry). A thin
    /// `Option`-returning wrapper around [`SettlementProvider::get_commitment`],
    /// which itself returns `Err(BatchNotFound)` for "no such batch" — a
    /// perfectly normal outcome here, not an error to propagate.
    async fn existing_commitment(
        &self,
        batch_id: Uuid,
    ) -> Result<Option<Commitment>, SettlementError> {
        match self.get_commitment(batch_id).await {
            Ok(commitment) => Ok(Some(commitment)),
            Err(SettlementError::BatchNotFound) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Inserts `batch`'s events as `ledger_entries`, computes the
    /// resulting Merkle tree size/root, and inserts the `ledger_batches`
    /// row — everything `commit`/`finalize` share, up to but
    /// not including who signs the resulting Signed Tree Head (a local
    /// key for `commit`, a caller-provided already-verified signature for
    /// `finalize`). Callers commit or roll back `tx` themselves; nothing
    /// here is durable until the caller commits it.
    async fn insert_batch_and_compute_root(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        batch: &EventBatch,
    ) -> Result<(i64, String, time::OffsetDateTime), SettlementError> {
        if batch.events.is_empty() {
            return Err(SettlementError::Storage(
                "cannot commit an empty batch".to_string(),
            ));
        }

        check_batch(batch)?;
        // One appender at a time: `seq` and `prev_hash` are read, hashed and written together.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('avalon.ledger.append'))")
            .execute(&mut **tx)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let mut prev_hash = self.tip_hash(&mut **tx).await?;
        // `seq` is part of the entry hash, so the next value is chosen here, not by the sequence.
        let mut next_seq = self.max_seq(&mut **tx).await? + 1;
        let mut first_seq: Option<i64> = None;
        let mut last_seq: i64 = 0;

        // Entries stay hash-chained across batch boundaries
        // — `prev_hash` continues from the ledger's global tip,
        // not reset per batch. `batch_id` is what groups these rows as one
        // settlement unit; `ledger_entries_batch_id_fkey` is deferred to the
        // end of this transaction, so it's fine that `ledger_batches` doesn't
        // have this row yet.
        let mut batch_hashes: Vec<String> = Vec::with_capacity(batch.events.len());
        for event in &batch.events {
            let seq = next_seq;
            next_seq += 1;
            let payload = stored_payload(event);
            let payload_hash =
                payload_hash_hex(&payload).map_err(SettlementError::InvalidPayload)?;
            let entry_hash = hash_event(
                &self.network_id,
                &self.shard_id,
                seq,
                &prev_hash,
                event,
                &payload_hash,
            )?;
            sqlx::query(
                r#"
                INSERT INTO ledger_entries
                    (seq, event_id, kind, issuer, subject, payload, payload_hash, event_timestamp, version, prev_hash, entry_hash, batch_id)
                OVERRIDING SYSTEM VALUE
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                "#,
            )
            .bind(seq)
            .bind(event.id)
            .bind(&event.kind)
            .bind(event.issuer.as_str())
            .bind(event.subject.as_str())
            .bind(&payload)
            .bind(&payload_hash)
            .bind(stored_timestamp(event)?)
            .bind(event.version as i32)
            .bind(&prev_hash)
            .bind(&entry_hash)
            .bind(batch.id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

            first_seq.get_or_insert(seq);
            last_seq = seq;

            batch_hashes.push(entry_hash.clone());
            prev_hash = entry_hash;
        }
        let first_seq = first_seq.expect("checked batch.events is non-empty above");
        // Keep the identity sequence ahead of the explicit values.
        sqlx::query("SELECT setval(pg_get_serial_sequence('ledger_entries', 'seq'), $1)")
            .bind(last_seq)
            .execute(&mut **tx)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        // batch_root is the real RFC 6962 Merkle Tree Hash of the whole
        // ledger, computed via the incremental tree: fast path
        // extends `leaf_cache`'s tree by this batch's own already-computed
        // hashes — O(log n) per leaf, not O(n) — assuming this process is
        // the ledger's sole writer; falls back to a full re-fetch and
        // O(n) rebuild (self-healing the cache) whenever that assumption
        // doesn't hold, e.g. right after process start.
        let mut cached = self.leaf_cache.write().await;
        let previous_committed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ledger_entries")
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let previous_committed = previous_committed - batch_hashes.len() as i64;
        // The *count* of real rows, never `last_seq` itself — `seq` can
        // have gaps (module doc comment), so `tree_size` must always match
        // the true leaf count.
        let (tree_size, tree_root): (i64, [u8; 32]) =
            if cached.leaves.len() as i64 == previous_committed {
                cached
                    .extend(&batch_hashes)
                    .map_err(SettlementError::Storage)?;
                let size = cached.leaves.len() as i64;
                let root = cached
                    .tree
                    .root(size as u64)
                    .expect("cache was just extended to exactly this size");
                (size, root)
            } else {
                let leaf_rows = sqlx::query(
                    "SELECT entry_hash FROM ledger_entries WHERE seq <= $1 ORDER BY seq ASC",
                )
                .bind(last_seq)
                .fetch_all(&mut **tx)
                .await
                .map_err(|e| SettlementError::Storage(e.to_string()))?;
                let fetched: Vec<String> = leaf_rows
                    .into_iter()
                    .map(|row| row.try_get::<String, _>("entry_hash"))
                    .collect::<Result<_, _>>()
                    .map_err(|e| SettlementError::Storage(e.to_string()))?;
                if fetched.len() > cached.leaves.len() {
                    cached
                        .rebuild(fetched.clone())
                        .map_err(SettlementError::Storage)?;
                }
                let size = fetched.len() as i64;
                let root = merkle::mth_of_hex_hashes(&fetched).map_err(SettlementError::Storage)?;
                (size, root)
            };
        drop(cached);
        let batch_root = hex::encode(tree_root);

        let batch_row = sqlx::query(
            r#"
            INSERT INTO ledger_batches (batch_id, first_seq, last_seq, batch_root)
            VALUES ($1, $2, $3, $4)
            RETURNING committed_at
            "#,
        )
        .bind(batch.id)
        .bind(first_seq)
        .bind(last_seq)
        .bind(&batch_root)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let committed_at: time::OffsetDateTime = batch_row
            .try_get("committed_at")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        Ok((tree_size, batch_root, committed_at))
    }

    /// Inserts a Signed Tree Head row using an already-built
    /// [`SignedTreeHead`] — shared by `commit` (locally signed) and
    /// `finalize` (caller-signed and independently verified
    /// before this is ever called).
    async fn insert_signed_tree_head(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tree_head: &SignedTreeHead,
    ) -> Result<(), SettlementError> {
        sqlx::query(
            r#"
            INSERT INTO signed_tree_heads (tree_size, root_hash, network_id, signing_key_id, signature, created_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(tree_head.tree_size)
        .bind(&tree_head.root_hash)
        .bind(&tree_head.network_id)
        .bind(&tree_head.signing_key_id)
        .bind(&tree_head.signature)
        .bind(tree_head.created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Issue #531 (managed hosting): the first half of the two-phase
    /// remote-signing flow — `POST /ledger/prepare-batch`
    /// (`crate::settlement`, `avalon-server`) calls this to give an
    /// integrator using a managed host the exact tree head they need to
    /// sign locally with their own settlement key, without that key ever
    /// touching this node.
    ///
    /// **Read-only — never touches `ledger_entries`, `ledger_batches`, or
    /// the shared `leaf_cache`.** Deliberately not the fast incremental
    /// path `commit`/`finalize` use: a preview that mutated shared state
    /// or burned real `seq` values (Postgres identity columns advance
    /// even inside a rolled-back transaction) every time it was called,
    /// whether or not the caller ever actually finalizes, would be a real
    /// cost with no corresponding commit. A fresh full fetch-and-recompute
    /// is the correct tradeoff here — previews are infrequent relative to
    /// `commit`'s own hot path.
    ///
    /// Because [`Self::finalize`] independently recomputes everything
    /// fresh rather than trusting this preview back, nothing returned
    /// here needs to be persisted or tracked as "pending" — if the
    /// ledger's tip moves between `prepare` and `finalize`, the caller's
    /// signature (over this preview's now-stale values) simply fails to
    /// verify against `finalize`'s freshly-recomputed ones, and the
    /// caller re-prepares and re-signs.
    pub async fn prepare(
        &self,
        batch: &EventBatch,
    ) -> Result<sth::PreparedTreeHead, SettlementError> {
        if batch.events.is_empty() {
            return Err(SettlementError::Storage(
                "cannot prepare an empty batch".to_string(),
            ));
        }

        check_batch(batch)?;
        let mut prev_hash = self.tip_hash(&self.pool).await?;
        let mut seq = self.max_seq(&self.pool).await?;
        let mut entry_hashes = Vec::with_capacity(batch.events.len());
        for event in &batch.events {
            seq += 1;
            let payload_hash = payload_hash_hex(&stored_payload(event))
                .map_err(SettlementError::InvalidPayload)?;
            let entry_hash = hash_event(
                &self.network_id,
                &self.shard_id,
                seq,
                &prev_hash,
                event,
                &payload_hash,
            )?;
            entry_hashes.push(entry_hash.clone());
            prev_hash = entry_hash;
        }

        let existing_rows = sqlx::query("SELECT entry_hash FROM ledger_entries ORDER BY seq ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let mut all_hashes: Vec<String> = existing_rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("entry_hash"))
            .collect::<Result<_, _>>()
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        all_hashes.extend(entry_hashes);

        let tree_size = all_hashes.len() as i64;
        let root = merkle::mth_of_hex_hashes(&all_hashes).map_err(SettlementError::Storage)?;

        Ok(sth::PreparedTreeHead {
            batch_id: batch.id,
            tree_size,
            root_hash: hex::encode(root),
            network_id: self.network_id.clone(),
            created_at: time::OffsetDateTime::now_utc(),
        })
    }

    /// Issue #531 (managed hosting): the second half of the two-phase
    /// remote-signing flow — `POST /ledger/finalize-batch`
    /// (`crate::settlement`, `avalon-server`) calls this after a caller
    /// (an integrator using a managed host) has independently signed the
    /// tree head [`Self::commit`] would otherwise have signed with a
    /// locally-held key.
    ///
    /// **Does not trust anything from a prior `prepare` call as
    /// authoritative** — it recomputes `batch`'s insertion and resulting
    /// tree size/root fresh, inside a real transaction, from this node's
    /// *current* tip. It then builds the candidate `SignedTreeHead` from
    /// those freshly-computed values (never from whatever a caller might
    /// claim) plus the caller-supplied `signing_key_id`/`signature`/
    /// `created_at`, and verifies that against `verify_key` *before*
    /// committing the transaction. A stale finalize (the ledger's tip
    /// moved since the caller last previewed it) or a forged/wrong
    /// signature both fail signature verification here — one check
    /// closes both failure modes, and the transaction rolls back on
    /// either, so a rejected finalize never burns real `seq`/tree state.
    ///
    /// **Idempotent on a replayed `batch_id`**: if `batch.id`
    /// already has a `ledger_batches` row — this exact finalize call was
    /// already applied, e.g. the caller retried after a timeout without
    /// knowing whether its first attempt landed — this returns that
    /// existing [`Commitment`] directly rather than re-inserting (which
    /// would otherwise fail on `ledger_batches`' `batch_id` primary key)
    /// or re-verifying the signature against what may now be a stale tip.
    /// This only ever de-duplicates retries against *this same node*; it
    /// cannot and does not prevent two independent managed-hosting nodes
    /// from each finalizing this `batch_id` into their own separate
    /// ledgers — avoiding that is the caller's responsibility (finalize a
    /// given batch against at most one host at a time; see
    /// `avalon_sdk::managed_hosting`'s prepare-race/finalize-once client).
    pub async fn finalize(
        &self,
        batch: &EventBatch,
        created_at: time::OffsetDateTime,
        signing_key_id: &str,
        signature_hex: &str,
        verify_key: &ed25519_dalek::VerifyingKey,
    ) -> Result<Commitment, SettlementError> {
        if let Some(existing) = self.existing_commitment(batch.id).await? {
            // A normal (non-replayed) `finalize` reports `committed_at` as
            // the caller-signed `created_at`, not `ledger_batches`' own DB
            // timestamp (see this method's return below) — a legitimate
            // replay resends that exact same `created_at`, so echo it back
            // here too rather than the DB row's insert time, keeping a
            // replayed call's return value indistinguishable from the
            // original's.
            return Ok(Commitment {
                committed_at: created_at,
                ..existing
            });
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        // Everything past this point can fail after the shared leaf cache was
        // already extended, so any error drops the cache.
        let outcome = async {
            let (tree_size, batch_root, _real_committed_at) =
                self.insert_batch_and_compute_root(&mut tx, batch).await?;

            let candidate = SignedTreeHead {
                tree_size,
                root_hash: batch_root.clone(),
                network_id: self.network_id.clone(),
                signing_key_id: signing_key_id.to_string(),
                signature: signature_hex.to_string(),
                created_at,
            };
            if !sth::verify_tree_head(verify_key, &candidate) {
                // Transaction is dropped without `commit()`, rolling back
                // everything `insert_batch_and_compute_root` just did — a
                // rejected finalize (stale tip, or a genuinely invalid
                // signature) never leaves partial state or burns `seq`.
                return Err(SettlementError::Storage(
                    "finalize: signature does not verify against the freshly-computed tree head \
                     (stale prepare, or an invalid signature) — re-prepare and re-sign"
                        .to_string(),
                ));
            }

            self.insert_signed_tree_head(&mut tx, &candidate).await?;

            tx.commit()
                .await
                .map_err(|e| SettlementError::Storage(e.to_string()))?;

            Ok(Commitment {
                batch_id: batch.id,
                proof: batch_root.into_bytes(),
                committed_at: candidate.created_at,
            })
        }
        .await;
        if outcome.is_err() {
            self.reset_leaf_cache().await;
        }
        outcome
    }
}

#[async_trait]
impl SettlementProvider for PostgresSettlementProvider {
    /// Issue #564: same replayed-`batch_id` idempotency as [`Self::finalize`]
    /// — see its doc comment.
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError> {
        if let Some(existing) = self.existing_commitment(batch.id).await? {
            return Ok(existing);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        // A failure after the shared leaf cache was extended drops the cache.
        let outcome = async {
            let (tree_size, batch_root, committed_at) =
                self.insert_batch_and_compute_root(&mut tx, batch).await?;

            // Signed Tree Head: one per batch commit, in this
            // same transaction, STH-only signing — no per-entry signature is
            // ever produced. The private key is loaded from the environment
            // fresh here (never persisted) — see `crate::sth`'s doc comment.
            let (signing_key, signing_key_id) = sth::load_signing_key_from_env()
                .map_err(|e| SettlementError::Storage(e.to_string()))?;
            let tree_head = sth::sign_tree_head(
                &signing_key,
                &signing_key_id,
                tree_size,
                &batch_root,
                &self.network_id,
                committed_at,
            );
            self.insert_signed_tree_head(&mut tx, &tree_head).await?;

            tx.commit()
                .await
                .map_err(|e| SettlementError::Storage(e.to_string()))?;

            Ok(Commitment {
                batch_id: batch.id,
                proof: batch_root.into_bytes(),
                committed_at,
            })
        }
        .await;
        if outcome.is_err() {
            self.reset_leaf_cache().await;
        }
        outcome
    }

    /// Two independent checks, both must pass: a per-entry hash-chain
    /// replay, and a from-scratch RFC 6962 Merkle recompute against
    /// `commitment.proof`. See `docs/projects/backend-server/architecture/settlement.md`'s
    /// "`verify`'s two independent checks" bullet for why both exist and
    /// how pruned payloads interact with each.
    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT seq, event_id, kind, issuer, subject, payload, payload_hash, event_timestamp, version, prev_hash, entry_hash
            FROM ledger_entries
            WHERE batch_id = $1
            ORDER BY seq ASC
            "#,
        )
        .bind(commitment.batch_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let Some(first_row) = rows.first() else {
            return Ok(false);
        };
        let entering_prev_hash: String = first_row
            .try_get("prev_hash")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut owned = Vec::with_capacity(rows.len());
        for row in &rows {
            let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
            owned.push((
                row.try_get::<i64, _>("seq").map_err(get)?,
                row.try_get::<Uuid, _>("event_id").map_err(get)?,
                row.try_get::<String, _>("kind").map_err(get)?,
                row.try_get::<String, _>("issuer").map_err(get)?,
                row.try_get::<String, _>("subject").map_err(get)?,
                row.try_get::<Option<serde_json::Value>, _>("payload")
                    .map_err(get)?,
                row.try_get::<String, _>("payload_hash").map_err(get)?,
                row.try_get::<time::OffsetDateTime, _>("event_timestamp")
                    .map_err(get)?,
                row.try_get::<i32, _>("version").map_err(get)?,
                row.try_get::<String, _>("prev_hash").map_err(get)?,
                row.try_get::<String, _>("entry_hash").map_err(get)?,
            ));
        }

        // Per entry: the link and the hash (recomputed from `payload_hash`) are checked even for
        // a pruned row, and a surviving payload must also match `payload_hash`.
        let mut expected_prev = entering_prev_hash;
        let mut chain_intact = true;
        #[allow(clippy::type_complexity)]
        for (
            seq,
            event_id,
            kind,
            issuer,
            subject,
            payload,
            payload_hash,
            timestamp,
            version,
            prev_hash,
            entry_hash,
        ) in &owned
        {
            let link_intact = *prev_hash == expected_prev;
            let content_intact = entry_content_intact(
                &self.network_id,
                &self.shard_id,
                prev_hash,
                entry_hash,
                payload.as_ref(),
                &EntryContent {
                    seq: *seq,
                    event_id: *event_id,
                    kind,
                    issuer,
                    subject,
                    payload_hash,
                    timestamp: *timestamp,
                    version: *version,
                },
            );
            if !(link_intact && content_intact) {
                chain_intact = false;
            }
            expected_prev = entry_hash.clone();
        }

        let Some(batch_row) =
            sqlx::query("SELECT last_seq FROM ledger_batches WHERE batch_id = $1")
                .bind(commitment.batch_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| SettlementError::Storage(e.to_string()))?
        else {
            return Ok(false);
        };
        let last_seq: i64 = batch_row
            .try_get("last_seq")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let leaf_rows =
            sqlx::query("SELECT entry_hash FROM ledger_entries WHERE seq <= $1 ORDER BY seq ASC")
                .bind(last_seq)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| SettlementError::Storage(e.to_string()))?;
        let leaf_hashes: Vec<String> = leaf_rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("entry_hash"))
            .collect::<Result<_, _>>()
            .map_err(|e: sqlx::Error| SettlementError::Storage(e.to_string()))?;
        let recomputed_tree_root =
            merkle::mth_of_hex_hashes(&leaf_hashes).map_err(SettlementError::Storage)?;
        let claimed_root = String::from_utf8_lossy(&commitment.proof).to_string();
        let merkle_intact = hex::encode(recomputed_tree_root) == claimed_root;

        Ok(chain_intact && merkle_intact)
    }

    async fn get_commitment(&self, batch_id: Uuid) -> Result<Commitment, SettlementError> {
        let row =
            sqlx::query("SELECT batch_root, committed_at FROM ledger_batches WHERE batch_id = $1")
                .bind(batch_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let Some(row) = row else {
            return Err(SettlementError::BatchNotFound);
        };
        let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
        let batch_root: String = row.try_get("batch_root").map_err(get)?;
        let committed_at: time::OffsetDateTime = row.try_get("committed_at").map_err(get)?;

        Ok(Commitment {
            batch_id,
            proof: batch_root.into_bytes(),
            committed_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avalon_protocol::canonical_payload::canonicalize;
    use serde_json::json;

    const NET: &str = "avalon-test";
    const SHARD: &str = CORE_SHARD_ID;

    fn ph(payload: &serde_json::Value) -> String {
        payload_hash_hex(payload).unwrap()
    }

    fn entry<'a>(seq: i64, event_id: Uuid, payload_hash: &'a str) -> EntryContent<'a> {
        EntryContent {
            seq,
            event_id,
            kind: "guild.created",
            issuer: "identity:x:self:guild_created",
            subject: "guild:y:self:guild_created",
            payload_hash,
            timestamp: time::OffsetDateTime::UNIX_EPOCH,
            version: 1,
        }
    }

    fn hash(content: &EntryContent<'_>) -> String {
        hash_entry(NET, SHARD, GENESIS_HASH, content).unwrap()
    }

    #[test]
    fn canonical_payload_is_stable_across_key_order() {
        let a = json!({ "from": "x", "to": "y", "actor": "x" });
        let b = json!({ "actor": "x", "to": "y", "from": "x" });
        assert_eq!(canonicalize(&a).unwrap(), canonicalize(&b).unwrap());
        assert_eq!(ph(&a), ph(&b));
        assert_ne!(ph(&a), ph(&json!({ "from": "x", "to": "z", "actor": "x" })));
    }

    #[test]
    fn payload_hash_rejects_payloads_without_a_canonical_encoding() {
        for payload in [
            json!({"n": 0.1234567890123456}),
            json!({"n": 18446744073709551615u64}),
        ] {
            assert!(matches!(
                payload_hash_hex(&payload),
                Err(CanonicalPayloadError::InvalidNumber { .. })
            ));
        }
    }

    #[test]
    fn every_entry_field_is_covered_by_the_hash() {
        let id = Uuid::from_u128(7);
        let p = ph(&json!({"a": 1}));
        let base = hash(&entry(1, id, &p));
        let other_payload = ph(&json!({"a": 2}));
        let variants = [
            EntryContent {
                seq: 2,
                ..entry(1, id, &p)
            },
            EntryContent {
                event_id: Uuid::from_u128(8),
                ..entry(1, id, &p)
            },
            EntryContent {
                kind: "k2",
                ..entry(1, id, &p)
            },
            EntryContent {
                issuer: "i2",
                ..entry(1, id, &p)
            },
            EntryContent {
                subject: "s2",
                ..entry(1, id, &p)
            },
            EntryContent {
                payload_hash: &other_payload,
                ..entry(1, id, &p)
            },
            EntryContent {
                timestamp: time::OffsetDateTime::UNIX_EPOCH + time::Duration::microseconds(1),
                ..entry(1, id, &p)
            },
            EntryContent {
                version: 2,
                ..entry(1, id, &p)
            },
        ];
        for v in &variants {
            assert_ne!(hash(v), base);
        }
        let c = entry(1, id, &p);
        assert_ne!(
            hash_entry("other-net", SHARD, GENESIS_HASH, &c).unwrap(),
            base
        );
        assert_ne!(hash_entry(NET, "game:x", GENESIS_HASH, &c).unwrap(), base);
        assert_ne!(hash_entry(NET, SHARD, &"1".repeat(64), &c).unwrap(), base);
    }

    /// Issue #173: two networks never share a hash space, even for identical content.
    #[test]
    fn hash_entry_differs_across_network_ids() {
        let p = ph(&json!({ "same": "content" }));
        let c = entry(1, Uuid::new_v4(), &p);
        let mainnet = hash_entry("avalon-mainnet-1", SHARD, GENESIS_HASH, &c).unwrap();
        let devnet = hash_entry("avalon-dev-chris", SHARD, GENESIS_HASH, &c).unwrap();
        assert_ne!(mainnet, devnet);
    }

    #[test]
    fn field_boundaries_cannot_shift() {
        let p = ph(&json!(null));
        let id = Uuid::nil();
        let a = EntryContent {
            kind: "ab",
            issuer: "c",
            ..entry(1, id, &p)
        };
        let b = EntryContent {
            kind: "a",
            issuer: "bc",
            ..entry(1, id, &p)
        };
        assert_ne!(hash(&a), hash(&b));
        let net_a = hash_entry("ab", "c", GENESIS_HASH, &entry(1, id, &p)).unwrap();
        let net_b = hash_entry("a", "bc", GENESIS_HASH, &entry(1, id, &p)).unwrap();
        assert_ne!(net_a, net_b);
    }

    #[test]
    fn malformed_inputs_are_rejected_not_hashed() {
        let p = ph(&json!(1));
        let c = entry(1, Uuid::nil(), &p);
        for prev in ["", "zz", &"A".repeat(64), &"0".repeat(63)] {
            assert!(hash_entry(NET, SHARD, prev, &c).is_err(), "{prev:?}");
        }
        assert!(hash_entry(
            NET,
            SHARD,
            GENESIS_HASH,
            &EntryContent {
                payload_hash: "ab",
                ..entry(1, Uuid::nil(), &p)
            }
        )
        .is_err());
        assert!(hash_entry(
            NET,
            SHARD,
            GENESIS_HASH,
            &EntryContent {
                seq: -1,
                ..entry(1, Uuid::nil(), &p)
            }
        )
        .is_err());
        assert!(hash_entry(
            NET,
            SHARD,
            GENESIS_HASH,
            &EntryContent {
                version: -1,
                ..entry(1, Uuid::nil(), &p)
            }
        )
        .is_err());
        assert!(hash_entry(
            NET,
            SHARD,
            GENESIS_HASH,
            &EntryContent {
                version: 65536,
                ..entry(1, Uuid::nil(), &p)
            }
        )
        .is_err());
    }

    #[test]
    fn a_pruned_row_verifies_and_a_tampered_payload_does_not() {
        let payload = json!({"a": 1});
        let p = ph(&payload);
        let c = entry(1, Uuid::from_u128(1), &p);
        let h = hash(&c);
        let check = |payload: Option<&serde_json::Value>, claimed: &str| {
            entry_content_intact(NET, SHARD, GENESIS_HASH, claimed, payload, &c)
        };
        assert!(check(Some(&payload), &h));
        assert!(check(None, &h), "skeleton row verifies without its payload");
        assert!(!check(Some(&json!({"a": 2})), &h), "tampered payload");
        assert!(!check(None, &"f".repeat(64)), "tampered hash");
    }

    /// The pre-#1226 hash (no length prefixes, hex text, unix seconds) is not accepted.
    #[test]
    fn an_old_format_hash_is_rejected() {
        use sha2::{Digest, Sha256};
        let payload = json!({"a": 1});
        let p = ph(&payload);
        let c = entry(1, Uuid::from_u128(1), &p);
        let mut hasher = Sha256::new();
        hasher.update(NET.as_bytes());
        hasher.update(GENESIS_HASH.as_bytes());
        hasher.update(c.event_id.as_bytes());
        hasher.update(c.kind.as_bytes());
        hasher.update(c.issuer.as_bytes());
        hasher.update(c.subject.as_bytes());
        hasher.update(canonicalize(&payload).unwrap().as_bytes());
        hasher.update(c.timestamp.unix_timestamp().to_le_bytes());
        hasher.update(c.version.to_le_bytes());
        let old = hex::encode(hasher.finalize());
        assert!(!entry_content_intact(
            NET,
            SHARD,
            GENESIS_HASH,
            &old,
            Some(&payload),
            &c
        ));
    }

    #[test]
    fn chain_position_is_stored_in_the_payload_and_covered_by_the_entry_hash() {
        use avalon_protocol::events::IdentityChainPosition;
        let mut event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "profile.updated".to_string(),
            issuer: global_id_from_str("identity:x:self:profile_updated").unwrap(),
            subject: global_id_from_str("identity:x:self:profile_updated").unwrap(),
            payload: serde_json::json!({"bio": "hi"}),
            timestamp: time::OffsetDateTime::UNIX_EPOCH,
            version: 1,
            identity_chain: None,
        };
        let hash_of = |event: &ProtocolEvent| {
            let p = ph(&stored_payload(event));
            hash_event(NET, SHARD, 1, GENESIS_HASH, event, &p).unwrap()
        };
        let unchained = hash_of(&event);
        assert_eq!(stored_payload(&event), event.payload);
        event.identity_chain = Some(IdentityChainPosition {
            seq: 1,
            prev_hash: None,
        });
        assert_ne!(hash_of(&event), unchained);
        let (payload, position) =
            avalon_protocol::identity_chain_wire::split_position(stored_payload(&event));
        assert_eq!(payload, event.payload);
        assert_eq!(position, event.identity_chain);
    }

    fn event_at(nanos: i128, version: u32) -> ProtocolEvent {
        ProtocolEvent {
            id: Uuid::nil(),
            kind: "k".to_string(),
            issuer: global_id_from_str("identity:x:self:k").unwrap(),
            subject: global_id_from_str("identity:x:self:k").unwrap(),
            payload: json!({}),
            timestamp: time::OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap(),
            version,
            identity_chain: None,
        }
    }

    #[test]
    fn stored_timestamp_is_whole_microseconds_even_before_2000() {
        for nanos in [-1_500i128, -946_684_800_000_000_500, 1_500] {
            let t = stored_timestamp(&event_at(nanos, 1)).unwrap();
            assert_eq!(t.unix_timestamp_nanos() % 1000, 0, "{nanos}");
        }
    }

    #[test]
    fn a_batch_with_an_unrepresentable_version_is_rejected_up_front() {
        let batch = EventBatch {
            id: Uuid::nil(),
            events: vec![event_at(0, 1), event_at(0, 65536)],
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        assert!(matches!(
            check_batch(&batch),
            Err(SettlementError::UnsupportedEntryVersion { version: 65536 })
        ));
    }

    fn chain_of(payloads: &[serde_json::Value]) -> Vec<String> {
        let mut prev = GENESIS_HASH.to_string();
        let mut out = Vec::new();
        for (i, payload) in payloads.iter().enumerate() {
            let p = ph(payload);
            prev = hash_entry(
                NET,
                SHARD,
                &prev,
                &entry(i as i64 + 1, Uuid::from_u128(i as u128), &p),
            )
            .unwrap();
            out.push(prev.clone());
        }
        out
    }

    #[test]
    fn recompute_batch_root_matches_sequential_hash_entry_calls() {
        let payloads = [json!({"a": 1}), json!({"b": 2}), json!({"c": 3})];
        let hashes: Vec<String> = payloads.iter().map(ph).collect();
        let contents: Vec<EntryContent<'_>> = hashes
            .iter()
            .enumerate()
            .map(|(i, h)| entry(i as i64 + 1, Uuid::from_u128(i as u128), h))
            .collect();
        assert_eq!(
            recompute_batch_root(NET, GENESIS_HASH, &contents),
            *chain_of(&payloads).last().unwrap()
        );
    }

    #[test]
    fn tampering_with_the_first_entry_changes_the_chain_tip() {
        let original = chain_of(&[json!({"a": 1}), json!({"b": 2}), json!({"c": 3})]);
        let tampered = chain_of(&[json!({"a": 999}), json!({"b": 2}), json!({"c": 3})]);
        assert_ne!(original.last(), tampered.last());
    }

    #[test]
    fn merkle_root_is_not_the_chain_tip_and_detects_any_tampered_leaf() {
        let hashes = chain_of(&[json!(0), json!(1), json!(2), json!(3), json!(4)]);
        let root = crate::merkle::mth_of_hex_hashes(&hashes).unwrap();
        assert_ne!(hex::encode(root), *hashes.last().unwrap());
        for i in 0..hashes.len() {
            let mut tampered = hashes.clone();
            tampered[i] = GENESIS_HASH.to_string();
            assert_ne!(
                root,
                crate::merkle::mth_of_hex_hashes(&tampered).unwrap(),
                "leaf {i}"
            );
        }
    }
}
