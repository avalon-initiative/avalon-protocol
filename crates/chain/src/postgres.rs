//! Postgres-backed `SettlementProvider` — milestone 1's implementation.
//!
//! Hash-chained, not yet signed: each entry commits to the hash of the entry
//! before it (`prev_hash`) plus its own content (`entry_hash`), giving
//! tamper-evidence today. Cryptographic signing per entry is a stronger,
//! separate guarantee tracked as its own open decision — issue #39 — and can
//! be layered on without changing this shape. The Merkle-root/checkpoint/
//! external-anchor design (issue #40, issue #70) is a further layer on top
//! of this hash chain, not built yet.
//!
//! Uses the same `PgPool` as `avalon-server` rather than its own connection —
//! milestone 1 has exactly one shared database (see
//! `crates/server/db/migrations/0002_ledger`).
//!
//! Uses runtime-checked `sqlx::query` (not the `query!` macro `avalon-server`
//! uses elsewhere) on purpose: this table's shape is still actively evolving
//! (signatures and Merkle fields are still open per #39/#40), and the
//! macro's compile-time schema check would mean every one of those changes
//! breaks the build until a live, migrated database is available — a real
//! cost for a table that isn't stable yet.
//!
//! Batching (issue #38): one protocol event is never one settlement action.
//! `commit` groups every event in an `EventBatch` under one `batch_id`,
//! inserted in a single transaction; `ledger_entries` stay hash-chained
//! across batch boundaries (the chain never resets per batch), and
//! `ledger_batches` holds one row per batch (`first_seq`, `last_seq`,
//! `batch_root`, `committed_at`). `batch_root` is a placeholder deterministic
//! root — the batch's chain tip, i.e. its last entry's `entry_hash` — not a
//! Merkle root; that's issue #40's call.

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch, ProtocolEvent};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{SettlementError, SettlementProvider};

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

/// Recomputes a batch's root the same way [`PostgresSettlementProvider::commit`]
/// produced it in the first place: replay the hash chain across the batch's
/// own entries, in order, starting from the hash the batch extended (its
/// first entry's stored `prev_hash` — the ledger's tip immediately before
/// this batch was committed, not verified here since that's a cross-batch
/// concern, not this batch's own integrity).
///
/// This is a pure, DB-free function on purpose (issue #38's acceptance
/// criteria that `verify` "recomputes and compares the batch root, not just
/// row existence") — it recomputes from each entry's actual stored content,
/// the same way `list_entries` re-verifies individual entries, so tampering
/// with any entry's content in the batch (not just deleting a row) changes
/// the result. `PostgresSettlementProvider::verify` is the only caller; kept
/// free-standing so it's directly unit-testable without Postgres.
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

#[derive(Clone)]
pub struct PostgresSettlementProvider {
    pool: PgPool,
    network_id: String,
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
                })
            }
            Some(stored) if stored == expected_network_id => {
                tx.commit()
                    .await
                    .map_err(|e| GenesisError::Storage(e.to_string()))?;
                Ok(Self {
                    pool,
                    network_id: stored,
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
            SELECT seq, event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash, batch_id
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
            let payload: serde_json::Value = row.try_get("payload").map_err(get)?;
            let event_timestamp: time::OffsetDateTime =
                row.try_get("event_timestamp").map_err(get)?;
            let version: i32 = row.try_get("version").map_err(get)?;
            let prev_hash: String = row.try_get("prev_hash").map_err(get)?;
            let entry_hash: String = row.try_get("entry_hash").map_err(get)?;
            let batch_id: Uuid = row.try_get("batch_id").map_err(get)?;

            let recomputed = hash_entry(
                &self.network_id,
                &prev_hash,
                &EntryContent {
                    event_id,
                    kind: &kind,
                    issuer: &issuer,
                    subject: &subject,
                    payload: &payload,
                    timestamp: event_timestamp,
                    version,
                },
            );
            let content_intact = recomputed == entry_hash;
            let link_intact = prev_hash == expected_prev;
            expected_prev = entry_hash.clone();

            entries.push(LedgerEntryView {
                seq: row.try_get("seq").map_err(get)?,
                event_id,
                kind,
                issuer,
                subject,
                payload,
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

    /// Every entry issued by `issuer_prefix` (a `GlobalId` prefix, e.g.
    /// `identity:<id>:self:` — every verb an identity signs itself under
    /// shares that prefix, see `crates/server/src/friends.rs`'s
    /// `identity_ref`), newest first — the read path behind issue #121's
    /// "my activity" view.
    ///
    /// Deliberately a narrower, unverified read than [`Self::list_entries`]:
    /// no hash/chain-link recomputation, since that's only meaningful
    /// against the *full*, sequential ledger — a per-issuer slice is a
    /// convenience projection for a player looking at their own history,
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
    pub payload: serde_json::Value,
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
    pub payload: serde_json::Value,
    pub version: i32,
    pub event_timestamp: time::OffsetDateTime,
    pub prev_hash: String,
    pub entry_hash: String,
    pub batch_id: Uuid,
    pub chain_intact: bool,
}

/// One committed batch — the unit of settlement (issue #38): entries are
/// hash-chained individually, but a batch is what `get_commitment` looks up
/// and what `avalon inspect-ledger` prints boundaries for. `batch_root` is a
/// placeholder deterministic root (the batch's chain tip — its last entry's
/// `entry_hash`); a Merkle root over the batch is #40's call, not this
/// ticket's.
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
        let mut last_hash = prev_hash.clone();
        let mut first_seq: Option<i64> = None;
        let mut last_seq: i64 = 0;

        // Entries stay hash-chained across batch boundaries (issue #38's
        // invariant) — `prev_hash` continues from the ledger's global tip,
        // not reset per batch. `batch_id` is what groups these rows as one
        // settlement unit; `ledger_entries_batch_id_fkey` is deferred to the
        // end of this transaction, so it's fine that `ledger_batches` doesn't
        // have this row yet.
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

            last_hash = entry_hash.clone();
            prev_hash = entry_hash;
        }
        let first_seq = first_seq.expect("checked batch.events is non-empty above");

        // batch_root is a placeholder deterministic root (the batch's chain
        // tip, i.e. its last entry's hash) — recomputable from the stored
        // entries alone, same as `verify` does. Whether this becomes a real
        // Merkle root is issue #40's call.
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
        .bind(&last_hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let committed_at: time::OffsetDateTime = batch_row
            .try_get("committed_at")
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        Ok(Commitment {
            batch_id: batch.id,
            proof: last_hash.into_bytes(),
            committed_at,
        })
    }

    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError> {
        let rows = sqlx::query(
            r#"
            SELECT event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash
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
                row.try_get::<serde_json::Value, _>("payload")
                    .map_err(get)?,
                row.try_get::<time::OffsetDateTime, _>("event_timestamp")
                    .map_err(get)?,
                row.try_get::<i32, _>("version").map_err(get)?,
            ));
        }
        let contents: Vec<EntryContent<'_>> = owned
            .iter()
            .map(
                |(event_id, kind, issuer, subject, payload, timestamp, version)| EntryContent {
                    event_id: *event_id,
                    kind,
                    issuer,
                    subject,
                    payload,
                    timestamp: *timestamp,
                    version: *version,
                },
            )
            .collect();

        let recomputed_root =
            recompute_batch_root(&self.network_id, &entering_prev_hash, &contents);
        let claimed_root = String::from_utf8_lossy(&commitment.proof).to_string();
        Ok(recomputed_root == claimed_root)
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
}
