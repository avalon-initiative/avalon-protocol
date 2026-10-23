//! Test doubles shared by the live indexer tests.
#![allow(dead_code)]

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use avalon_indexer::head_attestor::{EntryAttestation, HeadAttestor};
use avalon_indexer::IndexError;
use time::OffsetDateTime;
use uuid::Uuid;

/// The ledger entries and cosigned-head times the recovery tests plug in; entries are keyed by hash,
/// so tests running side by side never meet.
#[derive(Default)]
pub struct FakeHeads {
    entries: Mutex<Vec<(String, EntryAttestation)>>,
}

impl FakeHeads {
    pub fn put(&self, entry_hash: String, entry: EntryAttestation) {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|(hash, _)| *hash != entry_hash);
        entries.push((entry_hash, entry));
    }
}

#[async_trait]
impl HeadAttestor for FakeHeads {
    async fn by_entry_hash(
        &self,
        _conn: &mut sqlx::PgConnection,
        entry_hash: &str,
    ) -> Result<Option<EntryAttestation>, IndexError> {
        let entries = self.entries.lock().unwrap();
        Ok(entries
            .iter()
            .find(|(hash, _)| hash == entry_hash)
            .map(|(_, e)| e.clone()))
    }

    async fn by_event_id(
        &self,
        _conn: &mut sqlx::PgConnection,
        event_id: Uuid,
    ) -> Result<Vec<EntryAttestation>, IndexError> {
        let entries = self.entries.lock().unwrap();
        Ok(entries
            .iter()
            .filter(|(_, e)| e.event_id == event_id)
            .map(|(_, e)| e.clone())
            .collect())
    }
}

pub fn heads() -> Arc<FakeHeads> {
    static HEADS: OnceLock<Arc<FakeHeads>> = OnceLock::new();
    HEADS.get_or_init(Default::default).clone()
}

/// A cosigned-head view where the latest head is `reference_hours_after` hours after the first
/// covering one.
pub fn attestation(
    kind: &str,
    event_id: Uuid,
    subject: String,
    stored_payload_hash: [u8; 32],
    reference_hours_after: i64,
) -> EntryAttestation {
    let covered = OffsetDateTime::now_utc() - time::Duration::hours(200);
    EntryAttestation {
        kind: kind.to_string(),
        event_id,
        subject,
        payload_hash: hex::encode(stored_payload_hash),
        anchor_at: Some(covered),
        reference_at: Some(covered + time::Duration::hours(reference_hours_after)),
    }
}
