//! When witnesses first had a ledger entry cosigned and the latest such time of its shard. Both are the
//! witness-signed time a quorum had seen a head, never a shard's own head time or a node's clock.

use std::sync::Arc;

use async_trait::async_trait;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::identity_chain_wire::embed_position;
use avalon_protocol::ledger_entry::payload_hash;
use sqlx::{PgConnection, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::IndexError;

/// A ledger entry and the cosigned heads around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryAttestation {
    pub kind: String,
    pub event_id: Uuid,
    pub subject: String,
    /// Lowercase hex of the entry's payload hash.
    pub payload_hash: String,
    /// Quorum time of the earliest cosigned head covering the entry; `None` until one does.
    pub anchor_at: Option<OffsetDateTime>,
    /// Latest quorum time of any cosigned head of the entry's shard.
    pub reference_at: Option<OffsetDateTime>,
}

impl EntryAttestation {
    /// One view of an entry held as several copies (an event id is not unique across shards): the
    /// earliest anchor of any copy and the latest reference.
    pub fn earliest(copies: impl IntoIterator<Item = Self>) -> Option<Self> {
        let mut copies = copies.into_iter();
        let mut merged = copies.next()?;
        for copy in copies {
            merged.anchor_at = match (merged.anchor_at, copy.anchor_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            merged.reference_at = merged.reference_at.max(copy.reference_at);
        }
        Some(merged)
    }
}

/// The time by which `quorum` witnesses had cosigned a head: the `quorum`-th earliest cosignature time.
/// With `quorum == 0` (a known list of at most one member) there are no witnesses to wait for and the
/// head's own time stands in; that is a single-node dev setting.
pub fn quorum_time(
    cosigned: &[OffsetDateTime],
    quorum: usize,
    head_created_at: OffsetDateTime,
) -> Option<OffsetDateTime> {
    if quorum == 0 {
        return Some(head_created_at);
    }
    let mut times = cosigned.to_vec();
    times.sort();
    times.get(quorum - 1).copied()
}

#[async_trait]
pub trait HeadAttestor: Send + Sync {
    /// The entry with this lowercase hex entry hash, if the node holds it.
    async fn by_entry_hash(
        &self,
        conn: &mut PgConnection,
        entry_hash: &str,
    ) -> Result<Option<EntryAttestation>, IndexError>;

    /// Every held entry of this event id (an id is not unique across shards).
    async fn by_event_id(
        &self,
        conn: &mut PgConnection,
        event_id: Uuid,
    ) -> Result<Vec<EntryAttestation>, IndexError>;
}

/// The ledger entry of a chained `event`: the copies of its id (an id is not unique across shards)
/// whose kind and payload hash are those of the event as stored (payload with its chain position
/// embedded), merged to the earliest anchor. `None` while no node held entry matches.
pub async fn chain_event_attestation(
    conn: &mut PgConnection,
    heads: &dyn HeadAttestor,
    event: &ProtocolEvent,
) -> Result<Option<EntryAttestation>, IndexError> {
    let Some(position) = event.identity_chain.as_ref() else {
        return Ok(None);
    };
    let stored = payload_hash(&embed_position(&event.payload, position))
        .map_err(|_| IndexError::Rejected("chained event payload is not canonical".into()))?;
    let wanted = hex::encode(stored);
    let copies = heads
        .by_event_id(conn, event.id)
        .await?
        .into_iter()
        .filter(|e| e.kind == event.kind && e.payload_hash == wanted);
    Ok(EntryAttestation::earliest(copies))
}

/// Witnesses that must cosign a head before it counts; 0 when the known list has at most one member.
pub type QuorumFn = Arc<dyn Fn() -> usize + Send + Sync>;

/// Reads heads from `signed_tree_heads` (this node's shard) and `observed_sths` (mirrored shards).
pub struct SqlHeadAttestor {
    network_id: String,
    own_shard_id: String,
    quorum: QuorumFn,
}

impl SqlHeadAttestor {
    pub fn new(
        network_id: impl Into<String>,
        own_shard_id: impl Into<String>,
        quorum: QuorumFn,
    ) -> Self {
        Self {
            network_id: network_id.into(),
            own_shard_id: own_shard_id.into(),
            quorum,
        }
    }
}

struct Head {
    tree_size: i64,
    root_hash: String,
    created_at: OffsetDateTime,
}

struct Located {
    shard_id: String,
    own: bool,
    seq: i64,
    kind: String,
    event_id: Uuid,
    subject: String,
    payload_hash: String,
}

impl SqlHeadAttestor {
    /// The quorum time of every head of the shard covering `from_size` that enough witnesses cosigned.
    async fn quorum_times(
        &self,
        conn: &mut PgConnection,
        shard_id: &str,
        own: bool,
        from_size: i64,
    ) -> Result<Vec<OffsetDateTime>, IndexError> {
        let rows = if own {
            sqlx::query(
                "SELECT tree_size, root_hash, created_at FROM signed_tree_heads \
                 WHERE network_id = $1 AND tree_size >= $2 ORDER BY tree_size",
            )
            .bind(&self.network_id)
            .bind(from_size)
            .fetch_all(&mut *conn)
            .await?
        } else {
            sqlx::query(
                "SELECT tree_size, root_hash, MIN(created_at) AS created_at FROM observed_sths \
                 WHERE network_id = $1 AND shard_id = $2 AND tree_size >= $3 \
                 GROUP BY tree_size, root_hash ORDER BY tree_size",
            )
            .bind(&self.network_id)
            .bind(shard_id)
            .bind(from_size)
            .fetch_all(&mut *conn)
            .await?
        };
        let quorum = (self.quorum)();
        let mut out = Vec::new();
        for row in rows {
            let head = Head {
                tree_size: row.try_get("tree_size")?,
                root_hash: row.try_get("root_hash")?,
                created_at: row.try_get("created_at")?,
            };
            let times: Vec<OffsetDateTime> = sqlx::query_scalar(
                "SELECT observed_at FROM witness_cosignatures \
                 WHERE network_id = $1 AND shard_id = $2 AND tree_size = $3 AND root_hash = $4",
            )
            .bind(&self.network_id)
            .bind(shard_id)
            .bind(head.tree_size)
            .bind(&head.root_hash)
            .fetch_all(&mut *conn)
            .await?;
            out.extend(quorum_time(&times, quorum, head.created_at));
        }
        Ok(out)
    }

    async fn attest(
        &self,
        conn: &mut PgConnection,
        entry: Located,
    ) -> Result<EntryAttestation, IndexError> {
        let times = self
            .quorum_times(conn, &entry.shard_id, entry.own, entry.seq)
            .await?;
        let (anchor_at, reference_at) = (times.iter().min().copied(), times.iter().max().copied());
        Ok(EntryAttestation {
            kind: entry.kind,
            event_id: entry.event_id,
            subject: entry.subject,
            payload_hash: entry.payload_hash,
            anchor_at,
            reference_at,
        })
    }

    fn located(
        row: &sqlx::postgres::PgRow,
        shard_id: String,
        own: bool,
    ) -> Result<Located, IndexError> {
        Ok(Located {
            shard_id,
            own,
            seq: row.try_get("seq")?,
            kind: row.try_get("kind")?,
            event_id: row.try_get("event_id")?,
            subject: row.try_get("subject")?,
            payload_hash: row.try_get("payload_hash")?,
        })
    }
}

#[async_trait]
impl HeadAttestor for SqlHeadAttestor {
    async fn by_entry_hash(
        &self,
        conn: &mut PgConnection,
        entry_hash: &str,
    ) -> Result<Option<EntryAttestation>, IndexError> {
        let own = sqlx::query(
            "SELECT seq, kind, event_id, subject, payload_hash FROM ledger_entries WHERE entry_hash = $1",
        )
        .bind(entry_hash)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = own {
            let located = Self::located(&row, self.own_shard_id.clone(), true)?;
            return self.attest(conn, located).await.map(Some);
        }
        let mirrored = sqlx::query(
            "SELECT shard_id, seq, kind, event_id, subject, payload_hash FROM mirrored_entries \
             WHERE network_id = $1 AND entry_hash = $2",
        )
        .bind(&self.network_id)
        .bind(entry_hash)
        .fetch_optional(&mut *conn)
        .await?;
        match mirrored {
            Some(row) => {
                let shard_id: String = row.try_get("shard_id")?;
                let located = Self::located(&row, shard_id, false)?;
                self.attest(conn, located).await.map(Some)
            }
            None => Ok(None),
        }
    }

    async fn by_event_id(
        &self,
        conn: &mut PgConnection,
        event_id: Uuid,
    ) -> Result<Vec<EntryAttestation>, IndexError> {
        let mut found = Vec::new();
        let own = sqlx::query(
            "SELECT seq, kind, event_id, subject, payload_hash FROM ledger_entries WHERE event_id = $1",
        )
        .bind(event_id)
        .fetch_all(&mut *conn)
        .await?;
        for row in own {
            found.push(Self::located(&row, self.own_shard_id.clone(), true)?);
        }
        let mirrored = sqlx::query(
            "SELECT shard_id, seq, kind, event_id, subject, payload_hash FROM mirrored_entries \
             WHERE network_id = $1 AND event_id = $2",
        )
        .bind(&self.network_id)
        .bind(event_id)
        .fetch_all(&mut *conn)
        .await?;
        for row in mirrored {
            let shard_id: String = row.try_get("shard_id")?;
            found.push(Self::located(&row, shard_id, false)?);
        }
        let mut out = Vec::new();
        for located in found {
            out.push(self.attest(conn, located).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;

    fn t(hours: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::hours(hours)
    }

    fn entry(anchor: Option<i64>, reference: Option<i64>) -> EntryAttestation {
        EntryAttestation {
            kind: "profile.updated".into(),
            event_id: Uuid::nil(),
            subject: String::new(),
            payload_hash: String::new(),
            anchor_at: anchor.map(t),
            reference_at: reference.map(t),
        }
    }

    #[test]
    fn the_quorum_time_is_the_quorum_th_earliest_cosignature_and_ignores_the_head_time() {
        let times = [t(9), t(3), t(5), t(4)];
        assert_eq!(quorum_time(&times, 3, t(-100)), Some(t(5)));
        assert_eq!(quorum_time(&times, 1, t(-100)), Some(t(3)));
        assert_eq!(quorum_time(&times, 5, t(-100)), None);
    }

    #[test]
    fn without_a_quorum_requirement_the_head_time_stands_in() {
        assert_eq!(quorum_time(&[], 0, t(7)), Some(t(7)));
    }

    #[test]
    fn copies_of_an_entry_merge_to_the_earliest_anchor_and_latest_reference() {
        let merged = EntryAttestation::earliest([
            entry(Some(50), Some(60)),
            entry(Some(10), Some(70)),
            entry(None, None),
        ])
        .unwrap();
        assert_eq!(
            (merged.anchor_at, merged.reference_at),
            (Some(t(10)), Some(t(70)))
        );
        let uncovered = EntryAttestation::earliest([entry(None, None), entry(None, None)]).unwrap();
        assert_eq!(uncovered.anchor_at, None);
        assert!(EntryAttestation::earliest([]).is_none());
    }
}
