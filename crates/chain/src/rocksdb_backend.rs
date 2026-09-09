//! RocksDB-backed `SettlementProvider` (issue #178) — a second, parallel
//! implementation of the same trait `postgres.rs` implements, per ADR #177
//! (RocksDB is the decided embedded engine) and #40 (each node/mirror holds
//! its own local copy, not a shared Postgres). **Not wired into
//! `avalon-server` as the default anywhere** — this proves the
//! `SettlementProvider` boundary holds across a genuinely different storage
//! engine and gives a real, tested implementation to build the eventual
//! migration/sync path on, per `docs/architecture/settlement.md`'s own
//! statement that `PostgresSettlementProvider` stays the implementation
//! until that design is real.
//!
//! Behind the `rocksdb-backend` Cargo feature, off by default — nothing
//! consumes this yet, and building RocksDB's C++ core is real build time.
//!
//! Layout: three column families in one `rocksdb::DB`.
//! - `meta`: a single `genesis` key holding this ledger's `network_id`,
//!   written once by [`RocksDbSettlementProvider::connect`] and never
//!   updated after — the same genesis contract issue #173 gives
//!   `PostgresSettlementProvider`, including `network_id` being hashed into
//!   every entry ahead of its own content (`crate::hashing::hash_entry`),
//!   so this backend and the Postgres one produce byte-identical hashes for
//!   identical input (see the cross-backend test in
//!   `tests/rocksdb_settlement.rs`). `GenesisError` here is still this
//!   module's own type rather than shared with Postgres's — unifying the
//!   two into one genesis module is a natural, small follow-up, not a
//!   design disagreement.
//! - `entries`: big-endian `u64` seq → JSON-encoded `StoredEntry`, so a
//!   forward iteration over the CF visits entries in seq order and the last
//!   key is the ledger's tip.
//! - `batches`: batch `Uuid` (16 raw bytes) → JSON-encoded `StoredBatch`.
//!
//! Every `rocksdb` call is synchronous (it's a C++ library under a Rust
//! binding, not an async one) — every `SettlementProvider` method wraps its
//! actual work in `tokio::task::spawn_blocking` so this stays a
//! well-behaved async provider rather than stalling the runtime.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use avalon_protocol::events::{Commitment, EventBatch, ProtocolEvent};
use rocksdb::{IteratorMode, Options, DB};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hashing::{hash_event, recompute_batch_root, EntryContent, GENESIS_HASH};
use crate::{SettlementError, SettlementProvider};

const CF_META: &str = "meta";
const CF_ENTRIES: &str = "entries";
const CF_BATCHES: &str = "batches";
const GENESIS_KEY: &[u8] = b"genesis";

#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    #[error(
        "ledger genesis network_id `{stored}` does not match configured `{configured}` — refusing to open the wrong network"
    )]
    Mismatch { stored: String, configured: String },
    #[error("storage error: {0}")]
    Storage(String),
}

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    event: ProtocolEvent,
    batch_id: Uuid,
    prev_hash: String,
    entry_hash: String,
}

#[derive(Serialize, Deserialize)]
struct StoredBatch {
    first_seq: u64,
    last_seq: u64,
    batch_root: String,
    committed_at: time::OffsetDateTime,
}

fn seq_key(seq: u64) -> [u8; 8] {
    seq.to_be_bytes()
}

fn seq_from_key(key: &[u8]) -> u64 {
    u64::from_be_bytes(key.try_into().expect("entries CF keys are always 8 bytes"))
}

fn storage_err(e: impl std::fmt::Display) -> SettlementError {
    SettlementError::Storage(e.to_string())
}

#[derive(Clone)]
pub struct RocksDbSettlementProvider {
    db: Arc<DB>,
    network_id: String,
}

impl RocksDbSettlementProvider {
    /// The real entry point: creates this ledger's genesis on an empty
    /// database (first-ever open at `path`), or verifies `expected_network_id`
    /// against what's already there — a mismatch is a hard error, never a
    /// silently-accepted open. Mirrors `PostgresSettlementProvider::connect`
    /// (issue #173) exactly in intent.
    pub fn connect(path: &Path, expected_network_id: &str) -> Result<Self, GenesisError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let db = DB::open_cf(&opts, path, [CF_META, CF_ENTRIES, CF_BATCHES])
            .map_err(|e| GenesisError::Storage(e.to_string()))?;
        let meta_cf = db
            .cf_handle(CF_META)
            .expect("meta column family was just opened/created above");

        let existing = db
            .get_cf(meta_cf, GENESIS_KEY)
            .map_err(|e| GenesisError::Storage(e.to_string()))?;

        let network_id = match existing {
            None => {
                db.put_cf(meta_cf, GENESIS_KEY, expected_network_id.as_bytes())
                    .map_err(|e| GenesisError::Storage(e.to_string()))?;
                expected_network_id.to_string()
            }
            Some(bytes) => {
                let stored =
                    String::from_utf8(bytes).map_err(|e| GenesisError::Storage(e.to_string()))?;
                if stored != expected_network_id {
                    return Err(GenesisError::Mismatch {
                        stored,
                        configured: expected_network_id.to_string(),
                    });
                }
                stored
            }
        };

        Ok(Self {
            db: Arc::new(db),
            network_id,
        })
    }

    /// The network identity this provider is bound to (mirrors
    /// `PostgresSettlementProvider::network_id`, issue #173).
    pub fn network_id(&self) -> &str {
        &self.network_id
    }

    fn entries_cf(&self) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(CF_ENTRIES)
            .expect("entries column family always exists after connect")
    }

    fn batches_cf(&self) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(CF_BATCHES)
            .expect("batches column family always exists after connect")
    }

    /// The current chain tip: the last entry's `entry_hash`, or
    /// `GENESIS_HASH` if the ledger is empty. Also returns the next `seq` to
    /// use, so callers don't need a second scan.
    fn tip(&self) -> Result<(String, u64), SettlementError> {
        let cf = self.entries_cf();
        let mut iter = self.db.iterator_cf(cf, IteratorMode::End);
        match iter.next() {
            None => Ok((GENESIS_HASH.to_string(), 0)),
            Some(result) => {
                let (key, value) = result.map_err(storage_err)?;
                let stored: StoredEntry = serde_json::from_slice(&value).map_err(storage_err)?;
                Ok((stored.entry_hash, seq_from_key(&key) + 1))
            }
        }
    }

    fn commit_blocking(&self, batch: &EventBatch) -> Result<Commitment, SettlementError> {
        if batch.events.is_empty() {
            return Err(SettlementError::Storage(
                "cannot commit an empty batch".to_string(),
            ));
        }

        let entries_cf = self.entries_cf();
        let batches_cf = self.batches_cf();
        let (mut prev_hash, mut seq) = self.tip()?;
        let first_seq = seq;
        let mut last_hash = prev_hash.clone();

        let mut write_batch = rocksdb::WriteBatch::default();
        for event in &batch.events {
            let entry_hash = hash_event(&self.network_id, &prev_hash, event);
            let stored = StoredEntry {
                event: event.clone(),
                batch_id: batch.id,
                prev_hash: prev_hash.clone(),
                entry_hash: entry_hash.clone(),
            };
            let value = serde_json::to_vec(&stored).map_err(storage_err)?;
            write_batch.put_cf(entries_cf, seq_key(seq), value);

            last_hash = entry_hash.clone();
            prev_hash = entry_hash;
            seq += 1;
        }
        let last_seq = seq - 1;

        let committed_at = time::OffsetDateTime::now_utc();
        let stored_batch = StoredBatch {
            first_seq,
            last_seq,
            batch_root: last_hash.clone(),
            committed_at,
        };
        write_batch.put_cf(
            batches_cf,
            batch.id.as_bytes(),
            serde_json::to_vec(&stored_batch).map_err(storage_err)?,
        );

        self.db.write(write_batch).map_err(storage_err)?;

        Ok(Commitment {
            batch_id: batch.id,
            proof: last_hash.into_bytes(),
            committed_at,
        })
    }

    fn verify_blocking(&self, commitment: &Commitment) -> Result<bool, SettlementError> {
        let batches_cf = self.batches_cf();
        let Some(stored_bytes) = self
            .db
            .get_cf(batches_cf, commitment.batch_id.as_bytes())
            .map_err(storage_err)?
        else {
            return Ok(false);
        };
        let stored_batch: StoredBatch =
            serde_json::from_slice(&stored_bytes).map_err(storage_err)?;

        let entries_cf = self.entries_cf();
        let mut stored_entries = Vec::new();
        for seq in stored_batch.first_seq..=stored_batch.last_seq {
            let Some(bytes) = self
                .db
                .get_cf(entries_cf, seq_key(seq))
                .map_err(storage_err)?
            else {
                return Ok(false);
            };
            let stored: StoredEntry = serde_json::from_slice(&bytes).map_err(storage_err)?;
            stored_entries.push(stored);
        }
        let Some(first) = stored_entries.first() else {
            return Ok(false);
        };
        let entering_prev_hash = first.prev_hash.clone();

        let contents: Vec<EntryContent<'_>> = stored_entries
            .iter()
            .map(|stored| EntryContent {
                event_id: stored.event.id,
                kind: &stored.event.kind,
                issuer: stored.event.issuer.as_str(),
                subject: stored.event.subject.as_str(),
                payload: &stored.event.payload,
                timestamp: stored.event.timestamp,
                version: stored.event.version as i32,
            })
            .collect();

        let recomputed_root =
            recompute_batch_root(&self.network_id, &entering_prev_hash, &contents);
        let claimed_root = String::from_utf8_lossy(&commitment.proof).to_string();
        Ok(recomputed_root == claimed_root)
    }

    /// Every entry in the ledger, oldest first — inspection parity with
    /// `PostgresSettlementProvider::list_entries` (not part of the
    /// `SettlementProvider` trait; CLI wiring is separate, later work).
    /// Each entry is independently re-verified, same as Postgres's version:
    /// its content is rehashed and compared against its stored
    /// `entry_hash`, and that hash is compared against the next entry's
    /// `prev_hash`.
    pub fn list_entries(&self) -> Result<Vec<crate::LedgerEntryView>, SettlementError> {
        let cf = self.entries_cf();
        let mut entries = Vec::new();
        let mut expected_prev = GENESIS_HASH.to_string();

        for result in self.db.iterator_cf(cf, IteratorMode::Start) {
            let (key, value) = result.map_err(storage_err)?;
            let stored: StoredEntry = serde_json::from_slice(&value).map_err(storage_err)?;

            let recomputed = hash_event(&self.network_id, &stored.prev_hash, &stored.event);
            let content_intact = recomputed == stored.entry_hash;
            let link_intact = stored.prev_hash == expected_prev;
            expected_prev = stored.entry_hash.clone();

            entries.push(crate::LedgerEntryView {
                seq: seq_from_key(&key) as i64,
                event_id: stored.event.id,
                kind: stored.event.kind,
                issuer: stored.event.issuer.as_str().to_string(),
                subject: stored.event.subject.as_str().to_string(),
                payload: stored.event.payload,
                version: stored.event.version as i32,
                event_timestamp: stored.event.timestamp,
                prev_hash: stored.prev_hash,
                entry_hash: stored.entry_hash,
                batch_id: stored.batch_id,
                chain_intact: content_intact && link_intact,
            });
        }
        Ok(entries)
    }

    /// Every committed batch, oldest first — inspection parity with
    /// `PostgresSettlementProvider::list_batches`.
    pub fn list_batches(&self) -> Result<Vec<crate::LedgerBatchView>, SettlementError> {
        let cf = self.batches_cf();
        let mut batches = Vec::new();
        for result in self.db.iterator_cf(cf, IteratorMode::Start) {
            let (key, value) = result.map_err(storage_err)?;
            let stored: StoredBatch = serde_json::from_slice(&value).map_err(storage_err)?;
            let batch_id = Uuid::from_slice(&key).map_err(storage_err)?;
            batches.push(crate::LedgerBatchView {
                batch_id,
                first_seq: stored.first_seq as i64,
                last_seq: stored.last_seq as i64,
                batch_root: stored.batch_root,
                committed_at: stored.committed_at,
            });
        }
        batches.sort_by_key(|b| b.first_seq);
        Ok(batches)
    }

    fn get_commitment_blocking(&self, batch_id: Uuid) -> Result<Commitment, SettlementError> {
        let batches_cf = self.batches_cf();
        let Some(bytes) = self
            .db
            .get_cf(batches_cf, batch_id.as_bytes())
            .map_err(storage_err)?
        else {
            return Err(SettlementError::BatchNotFound);
        };
        let stored: StoredBatch = serde_json::from_slice(&bytes).map_err(storage_err)?;
        Ok(Commitment {
            batch_id,
            proof: stored.batch_root.into_bytes(),
            committed_at: stored.committed_at,
        })
    }
}

#[async_trait]
impl SettlementProvider for RocksDbSettlementProvider {
    async fn commit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError> {
        let this = self.clone();
        let batch = batch.clone();
        tokio::task::spawn_blocking(move || this.commit_blocking(&batch))
            .await
            .map_err(storage_err)?
    }

    async fn verify(&self, commitment: &Commitment) -> Result<bool, SettlementError> {
        let this = self.clone();
        let commitment = commitment.clone();
        tokio::task::spawn_blocking(move || this.verify_blocking(&commitment))
            .await
            .map_err(storage_err)?
    }

    async fn get_commitment(&self, batch_id: Uuid) -> Result<Commitment, SettlementError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.get_commitment_blocking(batch_id))
            .await
            .map_err(storage_err)?
    }
}
