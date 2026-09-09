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
use avalon_protocol::events::{Commitment, EventBatch};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::hashing::{hash_entry, hash_event, recompute_batch_root, EntryContent, GENESIS_HASH};
use crate::{LedgerBatchView, LedgerEntryView, SettlementError, SettlementProvider};

#[derive(Clone)]
pub struct PostgresSettlementProvider {
    pool: PgPool,
}

impl PostgresSettlementProvider {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
            let entry_hash = hash_event(&prev_hash, event);
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

        let recomputed_root = recompute_batch_root(&entering_prev_hash, &contents);
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
