//! Seeds a settlement authority's ledger from a mirror's stored copy of a
//! shard, so a replacement node can continue that shard's hash chain after
//! the original authority and its data are lost.
//!
//! The source is a mirror database (`mirrored_entries`, `observed_sths`);
//! the target is a separate, migrated database whose ledger tables are
//! empty. Everything is written in one target transaction and verified
//! before it commits. Sessions, credentials and any entry the mirror never
//! saw are not carried over.

use std::collections::HashSet;

use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::hash_entry;
use crate::incremental_merkle::IncrementalMerkleTree;
use crate::mirror::{self, ConvergenceVerdict, MirroredEntry, ObservedSth};
use crate::postgres::GENESIS_HASH;
use crate::{EntryContent, PostgresSettlementProvider, SettlementError};

const PAGE_SIZE: i64 = 2000;

/// Inputs of a promotion.
#[derive(Debug, Clone)]
pub struct PromoteParams<'a> {
    pub network_id: &'a str,
    pub shard_id: &'a str,
    /// Scopes the mirrored entries and observed STHs to one peer.
    pub source_url: Option<&'a str>,
    /// Runs every check and computes the plan without writing to the target.
    pub dry_run: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PromotionError {
    #[error("the mirror is not converged, refusing to promote")]
    NotConverged { verdict: ConvergenceVerdict },
    #[error("target database is not a fresh ledger: {0}")]
    TargetNotFresh(String),
    #[error("target database genesis network_id `{stored}` does not match `{expected}`")]
    GenesisMismatch { stored: String, expected: String },
    #[error("source and target are the same database ({0})")]
    SameDatabase(String),
    #[error("first mirrored entry (seq {seq}) has prev_hash `{prev_hash}`, not the genesis hash; the mirror starts mid-chain and cannot seed a ledger")]
    FirstEntryNotGenesis { seq: i64, prev_hash: String },
    #[error("mirrored entry seq {seq} does not link to its predecessor")]
    BrokenLink { seq: i64 },
    #[error("mirrored entry seq {seq} has a payload whose recomputed entry_hash differs from the stored one")]
    HashMismatch { seq: i64 },
    #[error("mirrored entry seq {seq} has an invalid entry_hash: {reason}")]
    InvalidHash { seq: i64, reason: String },
    #[error("mirrored entries are not in strictly increasing seq order at seq {seq}")]
    SeqOrder { seq: i64 },
    #[error("batch {batch_id} is not contiguous in the mirrored entries (seq {seq})")]
    BatchNotContiguous { batch_id: Uuid, seq: i64 },
    #[error("no mirrored entries to promote")]
    NothingMirrored,
    #[error("recomputed root `{computed}` at tree_size {tree_size} differs from the converged root `{converged}`")]
    RootMismatch {
        tree_size: i64,
        computed: String,
        converged: String,
    },
    #[error("no observed STH with a matching root at the final tree_size {0}")]
    NoFinalSth(i64),
    #[error("post-commit verification failed (the target was committed): {0}")]
    PostCommitVerification(String),
    #[error(transparent)]
    Settlement(#[from] SettlementError),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
}

/// One `ledger_batches` row to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBatch {
    pub batch_id: Uuid,
    pub first_seq: i64,
    pub last_seq: i64,
    /// Leaf count through this batch's last entry.
    pub tree_size: i64,
    /// RFC 6962 root over every leaf through this batch's last entry.
    pub batch_root: String,
}

/// Validates mirrored entries in seq order as they are pushed and derives
/// the batch boundaries and Merkle tree the authority would have stored.
pub struct PlanBuilder<'a> {
    network_id: &'a str,
    tree: IncrementalMerkleTree,
    prev_hash: String,
    last_seq: Option<i64>,
    /// `(batch_id, first_seq, last_seq, tree_size)` in seq order.
    batches: Vec<(Uuid, i64, i64, i64)>,
    seen_batches: HashSet<Uuid>,
    pruned_entries: i64,
}

/// The validated result of feeding every mirrored entry to a [`PlanBuilder`].
pub struct PromotionPlan {
    pub batches: Vec<PlannedBatch>,
    pub tree_size: i64,
    pub highest_seq: i64,
    pub pruned_entries: i64,
    tree: IncrementalMerkleTree,
}

impl PromotionPlan {
    /// Hex RFC 6962 root over the first `tree_size` leaves.
    pub fn root_at(&self, tree_size: i64) -> Option<String> {
        if tree_size < 1 {
            return None;
        }
        self.tree.root(tree_size as u64).map(hex::encode)
    }

    pub fn final_root(&self) -> String {
        self.root_at(self.tree_size)
            .expect("plan holds at least one entry")
    }
}

impl<'a> PlanBuilder<'a> {
    pub fn new(network_id: &'a str) -> Self {
        Self {
            network_id,
            tree: IncrementalMerkleTree::new(),
            prev_hash: GENESIS_HASH.to_string(),
            last_seq: None,
            batches: Vec::new(),
            seen_batches: HashSet::new(),
            pruned_entries: 0,
        }
    }

    /// Checks `entry` against the chain so far and records its leaf and
    /// batch membership. Entries must arrive in ascending seq order.
    pub fn push(&mut self, entry: &MirroredEntry) -> Result<(), PromotionError> {
        if let Some(last) = self.last_seq {
            if entry.seq <= last {
                return Err(PromotionError::SeqOrder { seq: entry.seq });
            }
        }
        if entry.prev_hash != self.prev_hash {
            if self.last_seq.is_none() {
                return Err(PromotionError::FirstEntryNotGenesis {
                    seq: entry.seq,
                    prev_hash: entry.prev_hash.clone(),
                });
            }
            return Err(PromotionError::BrokenLink { seq: entry.seq });
        }
        match &entry.payload {
            Some(payload) => {
                let recomputed = hash_entry(
                    self.network_id,
                    &entry.prev_hash,
                    &EntryContent {
                        event_id: entry.event_id,
                        kind: &entry.kind,
                        issuer: &entry.issuer,
                        subject: &entry.subject,
                        payload,
                        timestamp: entry.event_timestamp,
                        version: entry.version,
                    },
                );
                if recomputed != entry.entry_hash {
                    return Err(PromotionError::HashMismatch { seq: entry.seq });
                }
            }
            None => self.pruned_entries += 1,
        }
        let leaf = hex::decode(&entry.entry_hash).map_err(|e| PromotionError::InvalidHash {
            seq: entry.seq,
            reason: e.to_string(),
        })?;
        if leaf.len() != 32 {
            return Err(PromotionError::InvalidHash {
                seq: entry.seq,
                reason: format!("expected 32 bytes, got {}", leaf.len()),
            });
        }
        self.tree.append(&leaf);
        let size = self.tree.len() as i64;

        match self.batches.last_mut() {
            Some(current) if current.0 == entry.batch_id => {
                current.2 = entry.seq;
                current.3 = size;
            }
            _ => {
                if !self.seen_batches.insert(entry.batch_id) {
                    return Err(PromotionError::BatchNotContiguous {
                        batch_id: entry.batch_id,
                        seq: entry.seq,
                    });
                }
                self.batches
                    .push((entry.batch_id, entry.seq, entry.seq, size));
            }
        }
        self.prev_hash = entry.entry_hash.clone();
        self.last_seq = Some(entry.seq);
        Ok(())
    }

    pub fn finish(self) -> Result<PromotionPlan, PromotionError> {
        let highest_seq = self.last_seq.ok_or(PromotionError::NothingMirrored)?;
        let batches = self
            .batches
            .iter()
            .map(|&(batch_id, first_seq, last_seq, tree_size)| PlannedBatch {
                batch_id,
                first_seq,
                last_seq,
                tree_size,
                batch_root: hex::encode(
                    self.tree
                        .root(tree_size as u64)
                        .expect("batch size never exceeds the tree"),
                ),
            })
            .collect();
        Ok(PromotionPlan {
            batches,
            tree_size: self.tree.len() as i64,
            highest_seq,
            pruned_entries: self.pruned_entries,
            tree: self.tree,
        })
    }
}

/// Picks, per `tree_size`, the first observed STH whose root equals the
/// plan's recomputed root at that size. Observations with any other root at
/// that size are never selected.
pub fn select_sths<'o>(plan: &PromotionPlan, observed: &'o [ObservedSth]) -> Vec<&'o ObservedSth> {
    let mut chosen: Vec<&ObservedSth> = Vec::new();
    let mut sizes: HashSet<i64> = HashSet::new();
    for sth in observed {
        if sth.tree_size < 1 || sth.tree_size > plan.tree_size || sizes.contains(&sth.tree_size) {
            continue;
        }
        if plan.root_at(sth.tree_size).as_deref() == Some(sth.root_hash.as_str()) {
            sizes.insert(sth.tree_size);
            chosen.push(sth);
        }
    }
    chosen.sort_by_key(|s| s.tree_size);
    chosen
}

/// What a promotion did (or, for a dry run, would do).
#[derive(Debug, Clone)]
pub struct PromotionReport {
    pub network_id: String,
    pub shard_id: String,
    pub dry_run: bool,
    pub entries: i64,
    pub batches: usize,
    pub highest_seq: i64,
    /// Entries carried with a NULL payload because the mirror had pruned it.
    pub pruned_entries: i64,
    pub root: String,
    pub sths_carried: usize,
    /// Batches whose tree size has no matching observed STH.
    pub sths_missing: usize,
    pub genesis_written: bool,
    /// The seq the target's next committed entry will receive.
    pub next_seq: i64,
}

async fn database_identity(pool: &PgPool) -> Result<String, sqlx::Error> {
    let row = sqlx::query(
        "SELECT current_database()::text AS db, current_schema()::text AS schema, \
         COALESCE(inet_server_addr()::text, '') AS addr, COALESCE(inet_server_port(), 0) AS port",
    )
    .fetch_one(pool)
    .await?;
    Ok(format!(
        "{}/{}@{}:{}",
        row.try_get::<String, _>("db")?,
        row.try_get::<String, _>("schema")?,
        row.try_get::<String, _>("addr")?,
        row.try_get::<i32, _>("port")?
    ))
}

async fn observed_sths_for(
    source: &PgPool,
    params: &PromoteParams<'_>,
) -> Result<Vec<ObservedSth>, SettlementError> {
    let mut builder: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT source_url, network_id, shard_id, tree_size, root_hash, signature, signing_key_id, \
         created_at, observed_at FROM observed_sths WHERE network_id = ",
    );
    builder.push_bind(params.network_id);
    builder.push(" AND shard_id = ").push_bind(params.shard_id);
    if let Some(source_url) = params.source_url {
        builder.push(" AND source_url = ").push_bind(source_url);
    }
    builder.push(" ORDER BY tree_size ASC, observed_at ASC, id ASC");
    let rows = builder
        .build()
        .fetch_all(source)
        .await
        .map_err(|e| SettlementError::Storage(e.to_string()))?;
    let get = |e: sqlx::Error| SettlementError::Storage(e.to_string());
    rows.into_iter()
        .map(|row| {
            Ok(ObservedSth {
                source_url: row.try_get("source_url").map_err(get)?,
                network_id: row.try_get("network_id").map_err(get)?,
                shard_id: row.try_get("shard_id").map_err(get)?,
                tree_size: row.try_get("tree_size").map_err(get)?,
                root_hash: row.try_get("root_hash").map_err(get)?,
                signature: row.try_get("signature").map_err(get)?,
                signing_key_id: row.try_get("signing_key_id").map_err(get)?,
                created_at: row.try_get("created_at").map_err(get)?,
                observed_at: row.try_get("observed_at").map_err(get)?,
            })
        })
        .collect()
}

/// Refuses unless every ledger table of the target is empty and its genesis
/// is absent or equal to `network_id`. Returns whether genesis must be
/// written.
async fn check_target_fresh(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    network_id: &str,
) -> Result<bool, PromotionError> {
    for table in ["ledger_entries", "ledger_batches", "signed_tree_heads"] {
        let occupied: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS(SELECT 1 FROM {table})"
        )))
        .fetch_one(&mut **tx)
        .await?;
        if occupied {
            return Err(PromotionError::TargetNotFresh(format!(
                "`{table}` already has rows"
            )));
        }
    }
    let genesis: Option<String> =
        sqlx::query_scalar("SELECT network_id FROM chain_genesis LIMIT 1 FOR UPDATE")
            .fetch_optional(&mut **tx)
            .await?;
    match genesis {
        None => Ok(true),
        Some(stored) if stored == network_id => Ok(false),
        Some(stored) => Err(PromotionError::GenesisMismatch {
            stored,
            expected: network_id.to_string(),
        }),
    }
}

async fn insert_entry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry: &MirroredEntry,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO ledger_entries
            (seq, event_id, kind, issuer, subject, payload, payload_pruned_at, event_timestamp, version, prev_hash, entry_hash, batch_id)
        OVERRIDING SYSTEM VALUE
        VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $6::jsonb IS NULL THEN now() END, $7, $8, $9, $10, $11)
        "#,
    )
    .bind(entry.seq)
    .bind(entry.event_id)
    .bind(&entry.kind)
    .bind(&entry.issuer)
    .bind(&entry.subject)
    .bind(&entry.payload)
    .bind(entry.event_timestamp)
    .bind(entry.version)
    .bind(&entry.prev_hash)
    .bind(&entry.entry_hash)
    .bind(entry.batch_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Copies `params.shard_id`'s mirrored history from `source` into the fresh
/// ledger in `target`. See the module docs; on error nothing is written
/// unless the error is [`PromotionError::PostCommitVerification`].
pub async fn promote_mirror(
    source: &PgPool,
    target: &PgPool,
    params: &PromoteParams<'_>,
) -> Result<PromotionReport, PromotionError> {
    let source_identity = database_identity(source).await?;
    if source_identity == database_identity(target).await? {
        return Err(PromotionError::SameDatabase(source_identity));
    }

    let convergence = mirror::check_convergence(
        source,
        params.network_id,
        params.shard_id,
        params.source_url,
    )
    .await?;
    let ConvergenceVerdict::Converged {
        tree_size: converged_size,
    } = convergence.verdict
    else {
        return Err(PromotionError::NotConverged {
            verdict: convergence.verdict,
        });
    };
    let converged_root = convergence
        .recomputed_root
        .clone()
        .expect("a converged mirror has a recomputed root");

    let mut tx = target.begin().await?;
    let write_genesis = check_target_fresh(&mut tx, params.network_id).await?;

    let mut builder = PlanBuilder::new(params.network_id);
    let mut since_seq = 0i64;
    loop {
        let page = mirror::mirrored_entries_since(
            source,
            params.network_id,
            params.shard_id,
            since_seq,
            PAGE_SIZE,
            None,
            params.source_url,
        )
        .await?;
        let Some(last) = page.last() else { break };
        since_seq = last.seq;
        for entry in &page {
            builder.push(entry)?;
            if !params.dry_run {
                insert_entry(&mut tx, entry).await?;
            }
        }
    }
    let plan = builder.finish()?;

    if plan.tree_size != converged_size || plan.final_root() != converged_root {
        return Err(PromotionError::RootMismatch {
            tree_size: plan.tree_size,
            computed: plan.final_root(),
            converged: converged_root,
        });
    }

    let observed = observed_sths_for(source, params).await?;
    let sths = select_sths(&plan, &observed);
    if sths.last().map(|s| s.tree_size) != Some(plan.tree_size) {
        return Err(PromotionError::NoFinalSth(plan.tree_size));
    }
    let sths_missing = plan
        .batches
        .iter()
        .filter(|b| !sths.iter().any(|s| s.tree_size == b.tree_size))
        .count();

    let report = PromotionReport {
        network_id: params.network_id.to_string(),
        shard_id: params.shard_id.to_string(),
        dry_run: params.dry_run,
        entries: plan.tree_size,
        batches: plan.batches.len(),
        highest_seq: plan.highest_seq,
        pruned_entries: plan.pruned_entries,
        root: plan.final_root(),
        sths_carried: sths.len(),
        sths_missing,
        genesis_written: write_genesis,
        next_seq: plan.highest_seq + 1,
    };

    if params.dry_run {
        tx.rollback().await?;
        return Ok(report);
    }

    if write_genesis {
        sqlx::query("INSERT INTO chain_genesis (network_id) VALUES ($1)")
            .bind(params.network_id)
            .execute(&mut *tx)
            .await?;
    }
    let now = OffsetDateTime::now_utc();
    for batch in &plan.batches {
        let committed_at = sths
            .iter()
            .find(|s| s.tree_size == batch.tree_size)
            .map(|s| s.created_at)
            .unwrap_or(now);
        sqlx::query(
            "INSERT INTO ledger_batches (batch_id, first_seq, last_seq, batch_root, committed_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(batch.batch_id)
        .bind(batch.first_seq)
        .bind(batch.last_seq)
        .bind(&batch.batch_root)
        .bind(committed_at)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE ledger_entries e SET committed_at = b.committed_at \
         FROM ledger_batches b WHERE e.batch_id = b.batch_id",
    )
    .execute(&mut *tx)
    .await?;
    for sth in &sths {
        sqlx::query(
            "INSERT INTO signed_tree_heads (tree_size, root_hash, network_id, signing_key_id, signature, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(sth.tree_size)
        .bind(&sth.root_hash)
        .bind(&sth.network_id)
        .bind(&sth.signing_key_id)
        .bind(&sth.signature)
        .bind(sth.created_at)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE ledger_entries ALTER COLUMN seq RESTART WITH {}",
        report.next_seq
    )))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    verify_target(target, &plan)
        .await
        .map_err(PromotionError::PostCommitVerification)?;
    Ok(report)
}

/// Re-opens the promoted ledger through the authority's own read paths.
async fn verify_target(target: &PgPool, plan: &PromotionPlan) -> Result<(), String> {
    let network_id = PostgresSettlementProvider::read_genesis_network_id(target)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("target has no genesis")?;
    let chain = PostgresSettlementProvider::new(target.clone(), network_id);
    let entries = chain.list_entries().await.map_err(|e| e.to_string())?;
    if let Some(bad) = entries.iter().find(|e| !e.chain_intact) {
        return Err(format!("entry seq {} is not chain-intact", bad.seq));
    }
    if entries.len() as i64 != plan.tree_size {
        return Err(format!(
            "target holds {} entries, expected {}",
            entries.len(),
            plan.tree_size
        ));
    }
    let root = chain
        .root_at(plan.tree_size)
        .await
        .map_err(|e| e.to_string())?
        .map(hex::encode);
    if root.as_deref() != Some(plan.final_root().as_str()) {
        return Err("target tree root differs from the converged root".to_string());
    }
    let batches = chain.list_batches().await.map_err(|e| e.to_string())?;
    if batches.len() != plan.batches.len()
        || batches
            .iter()
            .zip(&plan.batches)
            .any(|(stored, planned)| stored.batch_root != planned.batch_root)
    {
        return Err("target batch roots differ from the recomputed roots".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::CORE_SHARD_ID;

    const NET: &str = "avalon-test-promotion";

    /// A mirrored chain with the given `(seq, batch_index, pruned)` layout.
    fn chain(layout: &[(i64, usize, bool)], batches: &[Uuid]) -> Vec<MirroredEntry> {
        let mut prev = GENESIS_HASH.to_string();
        let mut out = Vec::new();
        for &(seq, batch, pruned) in layout {
            let event_id = Uuid::new_v4();
            let payload = serde_json::json!({ "n": seq });
            let entry_hash = hash_entry(
                NET,
                &prev,
                &EntryContent {
                    event_id,
                    kind: "k",
                    issuer: "i",
                    subject: "s",
                    payload: &payload,
                    timestamp: OffsetDateTime::UNIX_EPOCH,
                    version: 1,
                },
            );
            out.push(MirroredEntry {
                source_url: "peer".into(),
                network_id: NET.into(),
                shard_id: CORE_SHARD_ID.into(),
                seq,
                event_id,
                kind: "k".into(),
                issuer: "i".into(),
                subject: "s".into(),
                payload: if pruned { None } else { Some(payload) },
                event_timestamp: OffsetDateTime::UNIX_EPOCH,
                version: 1,
                prev_hash: prev.clone(),
                entry_hash: entry_hash.clone(),
                batch_id: batches[batch],
                verified_tree_size: seq,
            });
            prev = entry_hash;
        }
        out
    }

    fn plan_of(entries: &[MirroredEntry]) -> Result<PromotionPlan, PromotionError> {
        let mut builder = PlanBuilder::new(NET);
        for e in entries {
            builder.push(e)?;
        }
        builder.finish()
    }

    #[test]
    fn batches_use_leaf_counts_across_seq_gaps() {
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        let entries = chain(
            &[(1, 0, false), (2, 0, false), (5, 1, false), (6, 1, false)],
            &ids,
        );
        let plan = plan_of(&entries).unwrap();
        assert_eq!(plan.tree_size, 4);
        assert_eq!(plan.highest_seq, 6);
        assert_eq!(plan.batches.len(), 2);
        assert_eq!(
            (plan.batches[0].first_seq, plan.batches[0].last_seq),
            (1, 2)
        );
        assert_eq!(
            (plan.batches[1].first_seq, plan.batches[1].last_seq),
            (5, 6)
        );
        let hashes: Vec<String> = entries.iter().map(|e| e.entry_hash.clone()).collect();
        let expected_first = hex::encode(crate::merkle::mth_of_hex_hashes(&hashes[..2]).unwrap());
        let expected_last = hex::encode(crate::merkle::mth_of_hex_hashes(&hashes).unwrap());
        assert_eq!(plan.batches[0].batch_root, expected_first);
        assert_eq!(plan.batches[0].tree_size, 2);
        assert_eq!(plan.batches[1].batch_root, expected_last);
        assert_eq!(plan.final_root(), expected_last);
    }

    #[test]
    fn pruned_payloads_are_counted_and_not_content_checked() {
        let ids = [Uuid::new_v4()];
        let entries = chain(&[(1, 0, true), (2, 0, false)], &ids);
        let plan = plan_of(&entries).unwrap();
        assert_eq!(plan.pruned_entries, 1);
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let ids = [Uuid::new_v4()];
        let mut entries = chain(&[(1, 0, false), (2, 0, false)], &ids);
        entries[1].payload = Some(serde_json::json!({ "n": 999 }));
        assert!(matches!(
            plan_of(&entries),
            Err(PromotionError::HashMismatch { seq: 2 })
        ));
    }

    #[test]
    fn first_entry_must_link_to_genesis() {
        let ids = [Uuid::new_v4()];
        let mut entries = chain(&[(1, 0, true), (2, 0, true)], &ids);
        entries.remove(0);
        assert!(matches!(
            plan_of(&entries),
            Err(PromotionError::FirstEntryNotGenesis { seq: 2, .. })
        ));
    }

    #[test]
    fn broken_link_is_rejected() {
        let ids = [Uuid::new_v4()];
        let mut entries = chain(&[(1, 0, true), (2, 0, true), (3, 0, true)], &ids);
        entries[2].prev_hash = "ab".repeat(32);
        assert!(matches!(
            plan_of(&entries),
            Err(PromotionError::BrokenLink { seq: 3 })
        ));
    }

    #[test]
    fn interleaved_batches_are_rejected() {
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        let entries = chain(&[(1, 0, true), (2, 1, true), (3, 0, true)], &ids);
        assert!(matches!(
            plan_of(&entries),
            Err(PromotionError::BatchNotContiguous { seq: 3, .. })
        ));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(matches!(plan_of(&[]), Err(PromotionError::NothingMirrored)));
    }

    #[test]
    fn only_sths_with_a_matching_root_are_selected() {
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        let entries = chain(&[(1, 0, true), (2, 0, true), (3, 1, true)], &ids);
        let plan = plan_of(&entries).unwrap();
        let sth = |size: i64, root: String, source: &str| ObservedSth {
            source_url: source.into(),
            network_id: NET.into(),
            shard_id: CORE_SHARD_ID.into(),
            tree_size: size,
            root_hash: root,
            signature: "sig".into(),
            signing_key_id: "key".into(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            observed_at: OffsetDateTime::UNIX_EPOCH,
        };
        let observed = vec![
            sth(2, "ff".repeat(32), "forged"),
            sth(2, plan.root_at(2).unwrap(), "honest"),
            sth(3, plan.root_at(3).unwrap(), "honest"),
            sth(3, plan.root_at(3).unwrap(), "second-peer"),
            sth(9, plan.root_at(3).unwrap(), "beyond"),
        ];
        let chosen = select_sths(&plan, &observed);
        assert_eq!(chosen.len(), 2);
        assert_eq!(chosen[0].source_url, "honest");
        assert_eq!(chosen[1].tree_size, 3);
    }
}
