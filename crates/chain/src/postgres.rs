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
//! (batch_id, signatures, Merkle fields all still open per #38/#39/#40), and
//! the macro's compile-time schema check would mean every one of those
//! changes breaks the build until a live, migrated database is available —
//! a real cost for a table that isn't stable yet.

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

fn hash_entry(prev_hash: &str, content: &EntryContent<'_>) -> String {
    let mut hasher = Sha256::new();
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

fn hash_event(prev_hash: &str, event: &ProtocolEvent) -> String {
    hash_entry(
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
            SELECT seq, event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash
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
                chain_intact: content_intact && link_intact,
            });
        }
        Ok(entries)
    }
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
    pub chain_intact: bool,
}

#[async_trait]
impl SettlementProvider for PostgresSettlementProvider {
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        let mut prev_hash = self.tip_hash().await?;
        let mut last_hash = prev_hash.clone();

        for event in &batch.events {
            let entry_hash = hash_event(&prev_hash, event);
            sqlx::query(
                r#"
                INSERT INTO ledger_entries
                    (event_id, kind, issuer, subject, payload, event_timestamp, version, prev_hash, entry_hash)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
            .execute(&mut *tx)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

            last_hash = entry_hash.clone();
            prev_hash = entry_hash;
        }

        tx.commit()
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;

        Ok(Commitment {
            batch_id: batch.id,
            proof: last_hash.into_bytes(),
            committed_at: time::OffsetDateTime::now_utc(),
        })
    }

    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError> {
        let claimed_hash = String::from_utf8_lossy(&commitment.proof).to_string();
        let row = sqlx::query("SELECT entry_hash FROM ledger_entries WHERE entry_hash = $1")
            .bind(claimed_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| SettlementError::Storage(e.to_string()))?;
        Ok(row.is_some())
    }

    async fn get_commitment(&self, _batch_id: Uuid) -> Result<Commitment, SettlementError> {
        // Batch-to-commitment lookup needs a batch_id column on ledger_entries
        // (currently each event is committed individually, one row per
        // event, not grouped by the batch it arrived in) — real event
        // batching is issue #38, still open. Not needed for
        // `avalon inspect-ledger`, which reads via `list_entries` instead.
        Err(SettlementError::BatchNotFound)
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
}
