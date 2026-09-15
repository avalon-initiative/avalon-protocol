//! Postgres-backed `SettlementProvider` — milestone 1's implementation
//! (issue #210, implementing #39/#40's decided design): a sequential hash
//! chain plus a real RFC 6962 Merkle tree with signed tree heads. See
//! `docs/architecture/settlement.md` and `settlement-implementation-notes.md`
//! for the two tamper-evidence structures, why `tree_size` is a derived
//! leaf count rather than raw `seq`, batching (#38), and node-tiered
//! payload retention (#208).

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch, ProtocolEvent};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::incremental_merkle::IncrementalMerkleTree;
use crate::retention::PruneReport;
use crate::sth::SignedTreeHead;
use crate::{merkle, sth, SettlementError, SettlementProvider};

/// All-zero hash, the `prev_hash` of the very first entry in the chain.
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The fields that make up an entry's content hash — grouped so recomputing
/// a hash (at insert time from a `ProtocolEvent`, or at verify time from a
/// stored row) takes one argument, not eight.
struct EntryContent<'a> {
    event_id: Uuid,
    kind: &'a str,
    issuer: &'a str,
    subject: &'a str,
    payload: &'a serde_json::Value,
    timestamp: time::OffsetDateTime,
    version: i32,
}

/// Serializes `value` with object keys sorted, recursively, so the result
/// is independent of the `Value`'s in-memory map ordering.
///
/// This matters because that ordering is *not* stable across this entry's
/// own lifecycle: a payload is built once in-process (order depends on
/// whether `serde_json`'s `preserve_order` feature is active in whichever
/// binary links this crate in — this crate doesn't request it itself, but
/// picks it up transitively when built into `avalon-server`/`avalon-cli`,
/// both of which pull it in via `webauthn-rs`/`passkey-types`), then
/// travels through the outbox's `JSONB` column and the ledger's own
/// `JSONB` column before `avalon inspect-ledger(-full)` ever reads it back
/// to verify — and Postgres's `jsonb` type does not preserve original key
/// order or formatting at all; it re-emits object keys in its own internal
/// canonical order. Hashing `Value::to_string()` directly, as this used to,
/// made the hash depend on which of those orderings happened to be current
/// at the moment of hashing rather than on the payload's actual content,
/// producing false "broken chain" reports for any multi-key payload
/// despite nothing being tampered with. Sorting keys ourselves removes the
/// dependency on any of those orderings agreeing with each other.
fn canonical_json(value: &serde_json::Value) -> String {
    fn write(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).expect("string always serializes"));
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            leaf => out.push_str(&leaf.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// `network_id` (issue #173) is hashed in ahead of everything else, so two
/// ledgers with different network identities produce disjoint hash spaces
/// by construction — an entry hashed under one `network_id` can never
/// collide with, or be mistaken for a valid link in, a chain rooted in a
/// different one. See `PostgresSettlementProvider::connect` for where that
/// identity is established and enforced.
fn hash_entry(network_id: &str, prev_hash: &str, content: &EntryContent<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(network_id.as_bytes());
    hasher.update(prev_hash.as_bytes());
    hasher.update(content.event_id.as_bytes());
    hasher.update(content.kind.as_bytes());
    hasher.update(content.issuer.as_bytes());
    hasher.update(content.subject.as_bytes());
    hasher.update(canonical_json(content.payload).as_bytes());
    hasher.update(content.timestamp.unix_timestamp().to_le_bytes());
    hasher.update(content.version.to_le_bytes());
    hex::encode(hasher.finalize())
}

fn hash_event(network_id: &str, prev_hash: &str, event: &ProtocolEvent) -> String {
    hash_entry(
        network_id,
        prev_hash,
        &EntryContent {
            event_id: event.id,
            kind: &event.kind,
            issuer: event.issuer.as_str(),
            subject: event.subject.as_str(),
            payload: &event.payload,
            timestamp: event.timestamp,
            version: event.version as i32,
        },
    )
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
/// directly (issue #208: it needs a *per-entry* link/content check, not an
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
        prev = hash_entry(network_id, &prev, content);
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
/// tree (issue #349), kept together so they can never drift apart. Rebuilt
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
    /// persisted in Postgres — see `docs/architecture/settlement.md` for
    /// that tradeoff.
    leaf_cache: std::sync::Arc<tokio::sync::RwLock<LedgerCache>>,
}

impl PostgresSettlementProvider {
    /// Low-level constructor for callers that already know (or don't care
    /// about) the ledger's genesis network_id — tests against a throwaway
    /// database, and read-only CLI inspection paired with
    /// [`Self::read_genesis_network_id`]. Does **not** create or verify a
    /// `chain_genesis` row; use [`Self::connect`] at real process startup,
    /// where that enforcement actually matters.
    pub fn new(pool: PgPool, network_id: impl Into<String>) -> Self {
        Self {
            pool,
            network_id: network_id.into(),
            leaf_cache: std::sync::Arc::new(tokio::sync::RwLock::new(LedgerCache::default())),
        }
    }

    /// Reads the ledger's genesis `network_id` without creating one —
    /// `None` if this database has never been booted against by
    /// [`Self::connect`]. For read-only diagnostics (`avalon inspect-ledger`)
    /// that want to display which network they're pointed at without
    /// asserting anything about it.
    pub async fn read_genesis_network_id(pool: &PgPool) -> Result<Option<String>, SettlementError> {
        sqlx::query_scalar("SELECT network_id FROM chain_genesis LIMIT 1")
            .fetch_optional(pool)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))
    }

    /// The real boot-time entry point (issue #173). If this database has no
    /// `chain_genesis` row yet, this is genesis: `expected_network_id` is
    /// written once and never touched again. If a row already exists, it
    /// must match `expected_network_id` exactly, or this returns
    /// `GenesisError::Mismatch` instead of a provider — the caller (see
    /// `avalon-server`'s `main.rs`) is expected to treat that as fatal and
    /// exit before binding a listener, never as a warning to log past.
    pub async fn connect(pool: PgPool, expected_network_id: &str) -> Result<Self, GenesisError> {
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

    /// The network identity this provider is bound to (issue #173) — every
    /// hash it computes or verifies is rooted in this value.
    pub fn network_id(&self) -> &str {
        &self.network_id
    }

    async fn tip_hash(&self) -> Result<String, SettlementError> {
        let row = sqlx::query("SELECT entry_hash FROM ledger_entries ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&self.pool)
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
            SELECT seq, event_id, kind, issuer, subject, payload, payload_pruned_at, event_timestamp, version, prev_hash, entry_hash, batch_id
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
            let payload_pruned_at: Option<time::OffsetDateTime> =
                row.try_get("payload_pruned_at").map_err(get)?;
            let event_timestamp: time::OffsetDateTime =
                row.try_get("event_timestamp").map_err(get)?;
            let version: i32 = row.try_get("version").map_err(get)?;
            let prev_hash: String = row.try_get("prev_hash").map_err(get)?;
            let entry_hash: String = row.try_get("entry_hash").map_err(get)?;
            let batch_id: Uuid = row.try_get("batch_id").map_err(get)?;

            let link_intact = prev_hash == expected_prev;
            // Content can only be independently re-verified when the
            // payload is still present — a pruned row (issue #208) has had
            // its payload deliberately discarded, so recomputing its
            // content hash is impossible by design, not a sign of
            // tampering. `chain_intact` therefore only asserts what's
            // actually checkable: the link always, and content whenever
            // the payload survives to check it against.
            let content_intact = match &payload {
                Some(payload) => {
                    let recomputed = hash_entry(
                        &self.network_id,
                        &prev_hash,
                        &EntryContent {
                            event_id,
                            kind: &kind,
                            issuer: &issuer,
                            subject: &subject,
                            payload,
                            timestamp: event_timestamp,
                            version,
                        },
                    );
                    recomputed == entry_hash
                }
                None => true,
            };
            expected_prev = entry_hash.clone();

            entries.push(LedgerEntryView {
                seq: row.try_get("seq").map_err(get)?,
                event_id,
                kind,
                issuer,
                subject,
                payload,
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
        let rows = sqlx::query(
            r#"
            SELECT seq, event_id, kind, issuer, subject, payload, payload_pruned_at, event_timestamp, version, prev_hash, entry_hash, batch_id
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

    /// Every Signed Tree Head, oldest (`tree_size`) first (issue #210) —
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
            ORDER BY tree_size ASC
            "#,
        )
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

    /// Issue #208's settlement-state checkpoint: a thin, purely-naming
    /// wrapper over [`Self::latest_signed_tree_head`], closing #180's
    /// "periodic durable-state checkpoint" ask for the commitment layer
    /// only — not the indexer/projection read-model snapshot half (still
    /// open, issue #43). See `docs/architecture/nodes.md`'s
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
            ORDER BY tree_size DESC
            LIMIT 1
            "#,
        )
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
            WHERE tree_size = $1
            "#,
        )
        .bind(tree_size)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
        row.map(sth_from_row).transpose()
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

    /// The RFC 6962 root at `tree_size` — O(log n) via the incremental tree
    /// (issue #349), `None` if `tree_size` exceeds what's been committed.
    pub async fn root_at(&self, tree_size: i64) -> Result<Option<[u8; 32]>, SettlementError> {
        self.ensure_cache_covers(tree_size).await?;
        let cached = self.leaf_cache.read().await;
        Ok(cached.tree.root(tree_size as u64))
    }

    /// The leaf's hex hash plus its O(log n) RFC 6962 inclusion (audit)
    /// path at `tree_size` (issue #349) — `entry_hashes_up_to` +
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
    /// leaves (issue #349) — `entry_hashes_up_to` +
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

/// One event from a single issuer's own history (issue #121) — a plain
/// projection, not a verified ledger entry; see
/// [`PostgresSettlementProvider::list_entries_for_issuer_prefix`].
pub struct IssuerHistoryEntry {
    pub event_id: Uuid,
    pub kind: String,
    pub subject: String,
    /// `None` if this row's payload has been pruned (issue #208) — a
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
    /// `None` once this row's payload has been pruned (issue #208, a
    /// hot-tier node with pruning enabled) — the entry itself, its hash,
    /// and its position in the chain/Merkle tree all survive regardless;
    /// only the content is gone. See [`Self::payload_pruned`].
    pub payload: Option<serde_json::Value>,
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

/// One committed batch — the unit of settlement (issue #38): entries are
/// hash-chained individually, but a batch is what `get_commitment` looks up
/// and what `avalon inspect-ledger` prints boundaries for. `batch_root` is
/// the real RFC 6962 Merkle Tree Hash of the whole ledger (not just this
/// batch's own entries), covering every entry committed so far (issue
/// #210) — not a per-batch sub-tree, and not the placeholder chain-tip
/// value #38 shipped. Its corresponding `tree_size` (the leaf *count*, not
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

#[async_trait]
impl SettlementProvider for PostgresSettlementProvider {
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError> {
        if batch.events.is_empty() {
            return Err(SettlementError::Storage(
                "cannot commit an empty batch".to_string(),
            ));
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut prev_hash = self.tip_hash().await?;
        let mut first_seq: Option<i64> = None;
        let mut last_seq: i64 = 0;

        // Entries stay hash-chained across batch boundaries (issue #38's
        // invariant) — `prev_hash` continues from the ledger's global tip,
        // not reset per batch. `batch_id` is what groups these rows as one
        // settlement unit; `ledger_entries_batch_id_fkey` is deferred to the
        // end of this transaction, so it's fine that `ledger_batches` doesn't
        // have this row yet.
        let mut batch_hashes: Vec<String> = Vec::with_capacity(batch.events.len());
        for event in &batch.events {
            let entry_hash = hash_event(&self.network_id, &prev_hash, event);
            let row = sqlx::query(
                r#"
                INSERT INTO ledger_entries
                    (event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                RETURNING seq
                "#,
            )
            .bind(event.id)
            .bind(&event.kind)
            .bind(event.issuer.as_str())
            .bind(event.subject.as_str())
            .bind(&event.payload)
            .bind(event.timestamp)
            .bind(event.version as i32)
            .bind(&prev_hash)
            .bind(&entry_hash)
            .bind(batch.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

            let seq: i64 = row
                .try_get("seq")
                .map_err(|e| SettlementError::Storage(e.to_string()))?;
            first_seq.get_or_insert(seq);
            last_seq = seq;

            batch_hashes.push(entry_hash.clone());
            prev_hash = entry_hash;
        }
        let first_seq = first_seq.expect("checked batch.events is non-empty above");

        // batch_root is the real RFC 6962 Merkle Tree Hash of the whole
        // ledger, computed via the incremental tree (issue #349): fast path
        // extends `leaf_cache`'s tree by this batch's own already-computed
        // hashes — O(log n) per leaf, not O(n) — assuming this process is
        // the ledger's sole writer; falls back to a full re-fetch and
        // O(n) rebuild (self-healing the cache) whenever that assumption
        // doesn't hold, e.g. right after process start.
        let mut cached = self.leaf_cache.write().await;
        let previous_committed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ledger_entries")
            .fetch_one(&mut *tx)
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
                .fetch_all(&mut *tx)
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
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let committed_at: time::OffsetDateTime = batch_row
            .try_get("committed_at")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        // Signed Tree Head (issue #210/#39): one per batch commit, in this
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
        .execute(&mut *tx)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        Ok(Commitment {
            batch_id: batch.id,
            proof: batch_root.into_bytes(),
            committed_at,
        })
    }

    /// Two independent checks, both must pass: a per-entry hash-chain
    /// replay, and a from-scratch RFC 6962 Merkle recompute against
    /// `commitment.proof`. See `docs/architecture/settlement-implementation-notes.md`'s
    /// "`verify`'s two independent checks" bullet for why both exist and
    /// how pruned payloads (#208) interact with each.
    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash
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
                row.try_get::<Uuid, _>("event_id").map_err(get)?,
                row.try_get::<String, _>("kind").map_err(get)?,
                row.try_get::<String, _>("issuer").map_err(get)?,
                row.try_get::<String, _>("subject").map_err(get)?,
                row.try_get::<Option<serde_json::Value>, _>("payload")
                    .map_err(get)?,
                row.try_get::<time::OffsetDateTime, _>("event_timestamp")
                    .map_err(get)?,
                row.try_get::<i32, _>("version").map_err(get)?,
                row.try_get::<String, _>("prev_hash").map_err(get)?,
                row.try_get::<String, _>("entry_hash").map_err(get)?,
            ));
        }

        // Issue #208: a hot-tier node may have pruned one or more of this
        // batch's entries' payloads. Per-entry, not batch-wide — mirrors
        // `list_entries`'s `link_intact`/`content_intact` split exactly.
        // Pruning one entry must never disable tamper detection for its
        // still-content-complete siblings in the same batch: the link
        // (`prev_hash == expected_prev`) is always checkable regardless of
        // pruning, and content is checked whenever the payload survives to
        // check it against. A missing payload only ever widens what's
        // *uncheckable* for that one entry — it never causes a batch-wide
        // skip, and never masks a genuine mismatch on an entry whose
        // payload is still present. `expected_prev` always advances to the
        // entry's *stored* `entry_hash`, since that's the one thing every
        // entry has regardless of pruning.
        let mut expected_prev = entering_prev_hash;
        let mut chain_intact = true;
        for (event_id, kind, issuer, subject, payload, timestamp, version, prev_hash, entry_hash) in
            &owned
        {
            let link_intact = *prev_hash == expected_prev;
            let content_intact = match payload {
                Some(payload) => {
                    let recomputed = hash_entry(
                        &self.network_id,
                        prev_hash,
                        &EntryContent {
                            event_id: *event_id,
                            kind,
                            issuer,
                            subject,
                            payload,
                            timestamp: *timestamp,
                            version: *version,
                        },
                    );
                    recomputed == *entry_hash
                }
                None => true,
            };
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
    use serde_json::json;

    #[test]
    fn canonical_json_is_stable_across_key_order() {
        let a = json!({ "from": "x", "to": "y", "actor": "x" });
        let b = json!({ "actor": "x", "to": "y", "from": "x" });
        let c = json!({ "to": "y", "from": "x", "actor": "x" });
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&b), canonical_json(&c));
    }

    #[test]
    fn canonical_json_sorts_nested_objects_too() {
        let a = json!({ "outer": { "z": 1, "a": 2 } });
        let b = json!({ "outer": { "a": 2, "z": 1 } });
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn canonical_json_still_distinguishes_different_content() {
        let a = json!({ "from": "x", "to": "y" });
        let b = json!({ "from": "x", "to": "z" });
        assert_ne!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn hash_entry_is_order_independent_for_the_full_entry() {
        let event_id = Uuid::new_v4();
        let timestamp = time::OffsetDateTime::now_utc();
        let a = json!({ "from": "x", "to": "y", "actor": "x" });
        let b = json!({ "actor": "x", "to": "y", "from": "x" });

        let hash_a = hash_entry(
            "avalon-test",
            GENESIS_HASH,
            &EntryContent {
                event_id,
                kind: "friend.requested",
                issuer: "identity:x:self:friend_requested",
                subject: "identity:y:self:friend_requested",
                payload: &a,
                timestamp,
                version: 1,
            },
        );
        let hash_b = hash_entry(
            "avalon-test",
            GENESIS_HASH,
            &EntryContent {
                event_id,
                kind: "friend.requested",
                issuer: "identity:x:self:friend_requested",
                subject: "identity:y:self:friend_requested",
                payload: &b,
                timestamp,
                version: 1,
            },
        );
        assert_eq!(hash_a, hash_b);
    }

    /// Issue #173's core guarantee: two networks never share a hash space,
    /// even for byte-identical entry content. This is what makes a dev
    /// ledger's history structurally incapable of being mistaken for, or
    /// spliced into, production's — not a policy, a hash input.
    #[test]
    fn hash_entry_differs_across_network_ids() {
        let event_id = Uuid::new_v4();
        let timestamp = time::OffsetDateTime::now_utc();
        let payload = json!({ "same": "content" });
        let content = EntryContent {
            event_id,
            kind: "friend.requested",
            issuer: "identity:x:self:friend_requested",
            subject: "identity:y:self:friend_requested",
            payload: &payload,
            timestamp,
            version: 1,
        };

        let mainnet = hash_entry("avalon-mainnet-1", GENESIS_HASH, &content);
        let devnet = hash_entry("avalon-dev-chris", GENESIS_HASH, &content);
        assert_ne!(mainnet, devnet);
    }

    fn sample_entry(event_id: Uuid, payload: &serde_json::Value) -> EntryContent<'_> {
        EntryContent {
            event_id,
            kind: "guild.created",
            issuer: "identity:x:self:guild_created",
            subject: "guild:y:self:guild_created",
            payload,
            timestamp: time::OffsetDateTime::UNIX_EPOCH,
            version: 1,
        }
    }

    #[test]
    fn recompute_batch_root_matches_sequential_hash_entry_calls() {
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let payloads = [json!({"a": 1}), json!({"b": 2}), json!({"c": 3})];
        let entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();

        let expected = {
            let mut prev = GENESIS_HASH.to_string();
            for entry in &entries {
                prev = hash_entry("avalon-test", &prev, entry);
            }
            prev
        };

        assert_eq!(
            recompute_batch_root("avalon-test", GENESIS_HASH, &entries),
            expected
        );
    }

    #[test]
    fn recompute_batch_root_detects_tampering_with_any_entry_in_the_batch() {
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let original_payloads = [json!({"a": 1}), json!({"b": 2}), json!({"c": 3})];
        let entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&original_payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();
        let root = recompute_batch_root("avalon-test", GENESIS_HASH, &entries);

        // Tamper with the *first* entry's payload — not the last — to prove
        // this isn't just re-hashing the tip; it must actually replay the
        // whole chain to notice.
        let mut tampered_payloads = original_payloads.clone();
        tampered_payloads[0] = json!({"a": 999});
        let tampered_entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&tampered_payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();
        let tampered_root = recompute_batch_root("avalon-test", GENESIS_HASH, &tampered_entries);

        assert_ne!(root, tampered_root);
    }

    #[test]
    fn recompute_batch_root_of_a_single_event_batch_is_legal() {
        let payload = json!({"solo": true});
        let entries = vec![sample_entry(Uuid::new_v4(), &payload)];
        let root = recompute_batch_root("avalon-test", GENESIS_HASH, &entries);
        assert_eq!(root, hash_entry("avalon-test", GENESIS_HASH, &entries[0]));
    }

    // --- Merkle root / STH (issue #210) ---
    //
    // `merkle.rs` owns MTH correctness against RFC 6962 reference vectors;
    // these tests are specifically about what `commit`/`verify` build on top
    // of it — that `batch_root` is now that real tree's root rather than the
    // old chain-tip placeholder, and that tampering with any `entry_hash` in
    // the tree's leaf set changes the recomputed root `verify` checks
    // against. Both are pure, DB-free — the actual `commit`/`verify`
    // integration is covered (live-Postgres, `#[ignore]`) in
    // `crates/chain/tests/settlement.rs`.

    #[test]
    fn merkle_root_of_multiple_entries_differs_from_the_old_chain_tip_placeholder() {
        // Three chained entry_hash values, exactly the shape `commit` reads
        // back to build its leaf set.
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let payloads = [json!({"a": 1}), json!({"b": 2}), json!({"c": 3})];
        let entries: Vec<EntryContent<'_>> = ids
            .iter()
            .zip(&payloads)
            .map(|(id, payload)| sample_entry(*id, payload))
            .collect();

        let mut entry_hashes = Vec::new();
        let mut prev = GENESIS_HASH.to_string();
        for entry in &entries {
            let hash = hash_entry("avalon-test", &prev, entry);
            entry_hashes.push(hash.clone());
            prev = hash;
        }
        let old_placeholder_root = entry_hashes.last().unwrap().clone();

        let merkle_root = hex::encode(
            crate::merkle::mth_of_hex_hashes(&entry_hashes)
                .expect("stored entry_hash values should always be valid hex"),
        );

        assert_ne!(
            merkle_root, old_placeholder_root,
            "batch_root must be a real Merkle root over the leaf set, not just the last entry's hash"
        );
    }

    #[test]
    fn merkle_root_detects_tampering_with_any_entry_hash_in_the_leaf_set() {
        let entry_hashes: Vec<String> = (0..5)
            .map(|i| {
                hash_entry(
                    "avalon-test",
                    GENESIS_HASH,
                    &sample_entry(Uuid::new_v4(), &json!({ "i": i })),
                )
            })
            .collect();
        let original_root = crate::merkle::mth_of_hex_hashes(&entry_hashes).unwrap();

        for i in 0..entry_hashes.len() {
            let mut tampered = entry_hashes.clone();
            tampered[i] = GENESIS_HASH.to_string();
            let tampered_root = crate::merkle::mth_of_hex_hashes(&tampered).unwrap();
            assert_ne!(
                original_root, tampered_root,
                "tampering with entry_hash at index {i} must change the recomputed Merkle root"
            );
        }
    }
}
