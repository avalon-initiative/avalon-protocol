//! Issue #484 (per #476/#479's decided ADR): tooling for a deliberate
//! `avalon-mainnet-N` -> `avalon-mainnet-(N+1)` genesis reset. Since
//! attestation/event signatures deliberately never bind `network_id`
//! (`crates/protocol/src/achievements.rs`'s `attestation_signing_bytes`),
//! nothing about a signature needs to change across a migration — this
//! module only has to carry forward the two things that *are*
//! network-scoped: the outgoing network's final ledger checkpoint (so the
//! new network's history is auditable as a continuation, not "started from
//! nothing") and its issuer admission registry (`issuer_network_registrations`,
//! #481), so no issuer — including ones no longer reachable — has to
//! re-register, let alone re-sign anything.
//!
//! This is deliberately a rare, operator-run migration (`avalon
//! migrate-network` in `crates/cli`), not a routine sync mechanism —
//! consistent with `avalon-mainnet-N` only ever incrementing for a
//! genuine, maintainer-decided reset (see
//! `docs/architecture/network-trust-anchors.md`). It works across two
//! independent `PgPool`s (source and target may be, and in the real
//! mainnet-reset case always will be, entirely separate databases) rather
//! than assuming they share a connection.

use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{GenesisError, PostgresSettlementProvider, SettlementError};

/// What a source network's history looks like at the moment of cutover.
/// `root_hash`/`signing_key_id`/`signature`/`sth_created_at` are `None`
/// when the source has a genesis but has never actually committed
/// anything (`tree_size` 0, no `signed_tree_heads` row yet) — a legitimate
/// state for a freshly stood-up network nobody has written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCheckpoint {
    pub network_id: String,
    pub tree_size: i64,
    pub root_hash: Option<String>,
    pub signing_key_id: Option<String>,
    pub signature: Option<String>,
    pub sth_created_at: Option<OffsetDateTime>,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("source database has no genesis set — nothing to migrate")]
    SourceHasNoGenesis,
    #[error(transparent)]
    Genesis(#[from] GenesisError),
    #[error(transparent)]
    Settlement(#[from] SettlementError),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
}

/// The result of a [`migrate_network`] call — everything an operator needs
/// to confirm the cutover did what it should have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub source_network_id: String,
    pub target_network_id: String,
    pub source_tree_size: i64,
    /// `true` when this exact source checkpoint was already recorded on
    /// the target — a retried run after a partial earlier failure, not a
    /// fresh cutover. The target genesis and issuer carry-over still ran
    /// (idempotently) either way.
    pub checkpoint_already_recorded: bool,
    /// How many issuer registrations were newly inserted into the target
    /// on this call. Rows the target already had (a retry) don't count
    /// again.
    pub issuers_carried_over: usize,
}

/// Reads the checkpoint a target network's genesis would reference if it
/// migrated from `pool` right now. Read-only — never mutates `pool`.
pub async fn read_source_checkpoint(pool: &PgPool) -> Result<SourceCheckpoint, MigrationError> {
    let network_id = PostgresSettlementProvider::read_genesis_network_id(pool)
        .await?
        .ok_or(MigrationError::SourceHasNoGenesis)?;

    let latest = sqlx::query(
        "SELECT tree_size, root_hash, signing_key_id, signature, created_at \
         FROM signed_tree_heads ORDER BY tree_size DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    Ok(match latest {
        Some(row) => SourceCheckpoint {
            network_id,
            tree_size: row.try_get("tree_size")?,
            root_hash: Some(row.try_get("root_hash")?),
            signing_key_id: Some(row.try_get("signing_key_id")?),
            signature: Some(row.try_get("signature")?),
            sth_created_at: Some(row.try_get("created_at")?),
        },
        None => SourceCheckpoint {
            network_id,
            tree_size: 0,
            root_hash: None,
            signing_key_id: None,
            signature: None,
            sth_created_at: None,
        },
    })
}

/// Performs the cutover: establishes (or, on a retry, confirms)
/// `target_network_id`'s genesis on `target_pool`, records `source_pool`'s
/// current checkpoint there, and bulk-carries its issuer registry forward.
///
/// Idempotent on retry against the same source/target pair — a second call
/// after a partial earlier failure records no duplicate checkpoint row and
/// only inserts whatever issuer rows the first attempt didn't reach, so a
/// stalled or interrupted migration can always just be re-run rather than
/// needing manual cleanup first.
pub async fn migrate_network(
    source_pool: &PgPool,
    target_pool: &PgPool,
    target_network_id: &str,
) -> Result<MigrationReport, MigrationError> {
    let checkpoint = read_source_checkpoint(source_pool).await?;

    // Fails fast if the target database already belongs to some other
    // network_id — the same guarantee every other boot path against this
    // table gets (`PostgresSettlementProvider::connect`'s own doc comment).
    PostgresSettlementProvider::connect(target_pool.clone(), target_network_id).await?;

    let already_present = sqlx::query(
        "SELECT 1 AS present FROM network_migration_checkpoints \
         WHERE source_network_id = $1 AND source_tree_size = $2",
    )
    .bind(&checkpoint.network_id)
    .bind(checkpoint.tree_size)
    .fetch_optional(target_pool)
    .await?
    .is_some();

    if !already_present {
        sqlx::query(
            "INSERT INTO network_migration_checkpoints \
             (id, source_network_id, source_tree_size, source_root_hash, \
              source_signing_key_id, source_signature, source_sth_created_at, migrated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(Uuid::new_v4())
        .bind(&checkpoint.network_id)
        .bind(checkpoint.tree_size)
        .bind(&checkpoint.root_hash)
        .bind(&checkpoint.signing_key_id)
        .bind(&checkpoint.signature)
        .bind(checkpoint.sth_created_at)
        .bind(OffsetDateTime::now_utc())
        .execute(target_pool)
        .await?;
    }

    let issuers_carried_over = carry_over_issuer_registrations(source_pool, target_pool).await?;

    Ok(MigrationReport {
        source_network_id: checkpoint.network_id,
        target_network_id: target_network_id.to_string(),
        source_tree_size: checkpoint.tree_size,
        checkpoint_already_recorded: already_present,
        issuers_carried_over,
    })
}

/// Bulk-carries every issuer registration from `source_pool` onto
/// `target_pool` — no re-registration, no re-signing (#476's decision that
/// signatures stay network-agnostic; admission is the only network-scoped
/// concept). `ON CONFLICT DO NOTHING` on `issuer_pubkey` (its primary key
/// in `issuer_network_registrations`) makes repeated calls safe.
async fn carry_over_issuer_registrations(
    source_pool: &PgPool,
    target_pool: &PgPool,
) -> Result<usize, MigrationError> {
    let rows = sqlx::query(
        "SELECT issuer_pubkey, issuer_ref, registered_at, auto_registered \
         FROM issuer_network_registrations",
    )
    .fetch_all(source_pool)
    .await?;

    let mut carried = 0usize;
    for row in &rows {
        let issuer_pubkey: Vec<u8> = row.try_get("issuer_pubkey")?;
        let issuer_ref: String = row.try_get("issuer_ref")?;
        let registered_at: OffsetDateTime = row.try_get("registered_at")?;
        let auto_registered: bool = row.try_get("auto_registered")?;

        let result = sqlx::query(
            "INSERT INTO issuer_network_registrations \
             (issuer_pubkey, issuer_ref, registered_at, auto_registered) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (issuer_pubkey) DO NOTHING",
        )
        .bind(&issuer_pubkey)
        .bind(&issuer_ref)
        .bind(registered_at)
        .bind(auto_registered)
        .execute(target_pool)
        .await?;
        if result.rows_affected() > 0 {
            carried += 1;
        }
    }

    Ok(carried)
}
