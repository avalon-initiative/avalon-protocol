//! Exercises `avalon_chain::migration` (issue #484) against real Postgres —
//! gated `--ignored`, see `make test-live`. Each test gets its own pair of
//! throwaway schemas (source/target), mirroring `settlement.rs`'s
//! `isolated_genesis_pool` pattern, extended with the extra tables a
//! migration actually touches (`signed_tree_heads`,
//! `issuer_network_registrations`, `network_migration_checkpoints`) so two
//! independent "networks" can exist side by side in one real database
//! without a second `DATABASE_URL`.

use avalon_chain::migration::{migrate_network, MigrationError};
use avalon_chain::PostgresSettlementProvider;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    avalon_devenv::load();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres — is it reachable?")
}

/// A throwaway schema carrying every table `avalon_chain::migration` reads
/// or writes, on either side of a migration. Not the real migrations
/// (`crates/server/db/migrations/`) — a hand-rolled minimal mirror, same
/// approach `settlement.rs::isolated_genesis_pool` already takes for
/// `chain_genesis` alone.
async fn isolated_network_pool(schema: &str) -> PgPool {
    let pool = test_pool().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE"
    )))
    .execute(&pool)
    .await
    .expect("failed to drop any stale test schema");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&pool)
        .await
        .expect("failed to create test schema");

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {schema}.chain_genesis (
            id BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
            network_id TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )"
    )))
    .execute(&pool)
    .await
    .expect("failed to create test chain_genesis table");

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {schema}.signed_tree_heads (
            tree_size BIGINT PRIMARY KEY,
            root_hash TEXT NOT NULL,
            network_id TEXT NOT NULL,
            signing_key_id TEXT NOT NULL,
            signature TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL
        )"
    )))
    .execute(&pool)
    .await
    .expect("failed to create test signed_tree_heads table");

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {schema}.issuer_network_registrations (
            issuer_pubkey BYTEA PRIMARY KEY,
            issuer_ref TEXT NOT NULL,
            registered_at TIMESTAMPTZ NOT NULL,
            auto_registered BOOLEAN NOT NULL DEFAULT false
        )"
    )))
    .execute(&pool)
    .await
    .expect("failed to create test issuer_network_registrations table");

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {schema}.network_migration_checkpoints (
            id UUID PRIMARY KEY,
            source_network_id TEXT NOT NULL,
            source_tree_size BIGINT NOT NULL,
            source_root_hash TEXT,
            source_signing_key_id TEXT,
            source_signature TEXT,
            source_sth_created_at TIMESTAMPTZ,
            migrated_at TIMESTAMPTZ NOT NULL
        )"
    )))
    .execute(&pool)
    .await
    .expect("failed to create test network_migration_checkpoints table");

    let owned_schema = schema.to_string();
    PgPoolOptions::new()
        .after_connect(move |conn, _meta| {
            let schema = owned_schema.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "SET search_path = {schema}, public"
                )))
                .execute(conn)
                .await?;
                Ok(())
            })
        })
        .connect_lazy_with((*pool.connect_options()).clone())
}

async fn insert_sth(pool: &PgPool, tree_size: i64, network_id: &str) {
    sqlx::query(
        "INSERT INTO signed_tree_heads (tree_size, root_hash, network_id, signing_key_id, signature, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tree_size)
    .bind(format!("root-hash-{tree_size}"))
    .bind(network_id)
    .bind("settlement-key-1")
    .bind(format!("sig-{tree_size}"))
    .bind(OffsetDateTime::now_utc())
    .execute(pool)
    .await
    .expect("failed to insert test signed_tree_heads row");
}

async fn insert_issuer_registration(pool: &PgPool, issuer_pubkey: &[u8], issuer_ref: &str) {
    sqlx::query(
        "INSERT INTO issuer_network_registrations (issuer_pubkey, issuer_ref, registered_at, auto_registered) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(issuer_pubkey)
    .bind(issuer_ref)
    .bind(OffsetDateTime::now_utc())
    .bind(false)
    .execute(pool)
    .await
    .expect("failed to insert test issuer registration");
}

#[tokio::test]
#[ignore]
async fn migrates_checkpoint_and_issuer_registrations_to_a_fresh_target() {
    let source = isolated_network_pool("test_migrate_source_a").await;
    let target = isolated_network_pool("test_migrate_target_a").await;

    PostgresSettlementProvider::connect(source.clone(), "avalon-mainnet-1")
        .await
        .expect("source connect should succeed");
    insert_sth(&source, 5, "avalon-mainnet-1").await;
    let issuer_pubkey = Uuid::new_v4().as_bytes().to_vec();
    insert_issuer_registration(&source, &issuer_pubkey, "studio:alpha").await;

    let report = migrate_network(&source, &target, "avalon-mainnet-2")
        .await
        .expect("migration should succeed");

    assert_eq!(report.source_network_id, "avalon-mainnet-1");
    assert_eq!(report.target_network_id, "avalon-mainnet-2");
    assert_eq!(report.source_tree_size, 5);
    assert!(!report.checkpoint_already_recorded);
    assert_eq!(report.issuers_carried_over, 1);

    let target_network_id = PostgresSettlementProvider::read_genesis_network_id(&target)
        .await
        .expect("read should succeed")
        .expect("target genesis should now exist");
    assert_eq!(target_network_id, "avalon-mainnet-2");

    let checkpoint_row = sqlx::query(
        "SELECT source_network_id, source_tree_size, source_root_hash \
         FROM network_migration_checkpoints",
    )
    .fetch_one(&target)
    .await
    .expect("checkpoint row should exist on target");
    let stored_source: String = checkpoint_row.try_get("source_network_id").unwrap();
    let stored_tree_size: i64 = checkpoint_row.try_get("source_tree_size").unwrap();
    let stored_root_hash: String = checkpoint_row.try_get("source_root_hash").unwrap();
    assert_eq!(stored_source, "avalon-mainnet-1");
    assert_eq!(stored_tree_size, 5);
    assert_eq!(stored_root_hash, "root-hash-5");

    let carried_ref: String = sqlx::query_scalar(
        "SELECT issuer_ref FROM issuer_network_registrations WHERE issuer_pubkey = $1",
    )
    .bind(&issuer_pubkey)
    .fetch_one(&target)
    .await
    .expect("issuer registration should have carried over");
    assert_eq!(carried_ref, "studio:alpha");
}

#[tokio::test]
#[ignore]
async fn migration_is_idempotent_on_a_retried_run() {
    let source = isolated_network_pool("test_migrate_source_b").await;
    let target = isolated_network_pool("test_migrate_target_b").await;

    PostgresSettlementProvider::connect(source.clone(), "avalon-mainnet-1")
        .await
        .expect("source connect should succeed");
    insert_sth(&source, 3, "avalon-mainnet-1").await;
    let issuer_pubkey = Uuid::new_v4().as_bytes().to_vec();
    insert_issuer_registration(&source, &issuer_pubkey, "studio:beta").await;

    let first = migrate_network(&source, &target, "avalon-mainnet-2")
        .await
        .expect("first migration should succeed");
    assert!(!first.checkpoint_already_recorded);
    assert_eq!(first.issuers_carried_over, 1);

    let second = migrate_network(&source, &target, "avalon-mainnet-2")
        .await
        .expect("retried migration should succeed, not error");
    assert!(second.checkpoint_already_recorded);
    assert_eq!(
        second.issuers_carried_over, 0,
        "the issuer was already carried over by the first call"
    );

    let checkpoint_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM network_migration_checkpoints")
            .fetch_one(&target)
            .await
            .expect("count should succeed");
    assert_eq!(
        checkpoint_count, 1,
        "a retried migration must not record a duplicate checkpoint"
    );
}

#[tokio::test]
#[ignore]
async fn migration_refuses_a_target_that_already_belongs_to_a_different_network() {
    let source = isolated_network_pool("test_migrate_source_c").await;
    let target = isolated_network_pool("test_migrate_target_c").await;

    PostgresSettlementProvider::connect(source.clone(), "avalon-mainnet-1")
        .await
        .expect("source connect should succeed");
    insert_sth(&source, 1, "avalon-mainnet-1").await;

    PostgresSettlementProvider::connect(target.clone(), "avalon-int-unrelated")
        .await
        .expect("target's own unrelated genesis should establish fine");

    let result = migrate_network(&source, &target, "avalon-mainnet-2").await;
    assert!(matches!(result, Err(MigrationError::Genesis(_))));
}

#[tokio::test]
#[ignore]
async fn migration_handles_a_source_with_genesis_but_no_committed_entries() {
    let source = isolated_network_pool("test_migrate_source_d").await;
    let target = isolated_network_pool("test_migrate_target_d").await;

    PostgresSettlementProvider::connect(source.clone(), "avalon-mainnet-1")
        .await
        .expect("source connect should succeed");
    // Deliberately no `insert_sth` call — a genesis with zero commits.

    let report = migrate_network(&source, &target, "avalon-mainnet-2")
        .await
        .expect("migration of an empty-but-genesis'd source should succeed");

    assert_eq!(report.source_tree_size, 0);

    let stored_root_hash: Option<String> =
        sqlx::query_scalar("SELECT source_root_hash FROM network_migration_checkpoints")
            .fetch_one(&target)
            .await
            .expect("checkpoint row should exist");
    assert_eq!(
        stored_root_hash, None,
        "no STH ever existed on the source, so there is nothing to reference"
    );
}
