//! Live tests for `promotion::promote_mirror`. A real ledger is produced by
//! `PostgresSettlementProvider::commit` in one scratch schema, copied into
//! another schema's mirror tables the way a mirror-watcher would have
//! stored it, then promoted into a third, empty schema. Each schema is
//! created by cloning the migrated `public` table shapes and is dropped when
//! the test finishes. Gated `--ignored`; run with `--test-threads=1`.

use avalon_chain::merkle;
use avalon_chain::mirror::{self, MirroredEntry, ObservedSth, CORE_SHARD_ID};
use avalon_chain::promotion::{promote_mirror, PromoteParams, PromotionError};
use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use avalon_protocol::ids::GlobalId;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

const TABLES: [&str; 7] = [
    "ledger_entries",
    "ledger_batches",
    "signed_tree_heads",
    "chain_genesis",
    "mirrored_entries",
    "observed_sths",
    "equivocation_findings",
];

const PEER: &str = "http://authority.invalid";

struct Scratch {
    schema: String,
    pool: PgPool,
    admin: PgPool,
}

impl Scratch {
    async fn new(label: &str) -> Self {
        avalon_devenv::load();
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let admin = PgPoolOptions::new().connect(&url).await.expect("connect");
        let schema = format!("promo_{label}_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .expect("create schema");
        for table in TABLES {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE {schema}.{table} (LIKE public.{table} INCLUDING ALL)"
            )))
            .execute(&admin)
            .await
            .expect("clone table");
        }
        let owned = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |conn, _meta| {
                let schema = owned.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "SET search_path = {schema}, public"
                    )))
                    .execute(conn)
                    .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("connect scratch");
        Self {
            schema,
            pool,
            admin,
        }
    }

    async fn drop(self) {
        self.pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .expect("drop schema");
    }
}

fn batch(events: usize) -> EventBatch {
    EventBatch {
        id: Uuid::new_v4(),
        events: (0..events)
            .map(|i| {
                let actor = Uuid::new_v4();
                ProtocolEvent {
                    id: Uuid::new_v4(),
                    kind: format!("test.event_{i}"),
                    issuer: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
                    subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
                    payload: serde_json::json!({ "note": "promotion", "i": i, "z": [1, 2] }),
                    timestamp: OffsetDateTime::now_utc(),
                    version: 1,
                }
            })
            .collect(),
        created_at: OffsetDateTime::now_utc(),
    }
}

/// Commits 3 batches (2, 3 and 1 events) with a burned seq value between
/// the first and second, returning the authority's provider.
async fn build_authority(scratch: &Scratch, network_id: &str) -> PostgresSettlementProvider {
    let chain = PostgresSettlementProvider::connect(scratch.pool.clone(), network_id)
        .await
        .expect("genesis");
    chain.commit(&batch(2)).await.expect("commit 1");
    sqlx::query("SELECT nextval(pg_get_serial_sequence('ledger_entries', 'seq'))")
        .execute(&scratch.pool)
        .await
        .expect("burn a seq");
    chain.commit(&batch(3)).await.expect("commit 2");
    chain.commit(&batch(1)).await.expect("commit 3");
    chain
}

/// Copies the authority's ledger and STH history into `mirror`'s mirror tables.
async fn mirror_authority(authority: &PostgresSettlementProvider, mirror_pool: &PgPool) {
    let entries = authority.list_entries().await.expect("entries");
    for e in entries {
        mirror::insert_mirrored_entry(
            mirror_pool,
            &MirroredEntry {
                source_url: PEER.to_string(),
                network_id: authority.network_id().to_string(),
                shard_id: CORE_SHARD_ID.to_string(),
                seq: e.seq,
                event_id: e.event_id,
                kind: e.kind,
                issuer: e.issuer,
                subject: e.subject,
                payload: e.payload,
                event_timestamp: e.event_timestamp,
                version: e.version,
                prev_hash: e.prev_hash,
                entry_hash: e.entry_hash,
                batch_id: e.batch_id,
                verified_tree_size: e.seq,
            },
        )
        .await
        .expect("mirror entry");
    }
    for sth in authority.list_signed_tree_heads().await.expect("sths") {
        mirror::insert_observation(
            mirror_pool,
            &ObservedSth::from_sth(PEER, CORE_SHARD_ID, &sth, OffsetDateTime::now_utc()),
        )
        .await
        .expect("observation");
    }
}

fn params<'a>(network_id: &'a str, dry_run: bool) -> PromoteParams<'a> {
    PromoteParams {
        network_id,
        shard_id: CORE_SHARD_ID,
        source_url: None,
        dry_run,
    }
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(pool)
        .await
        .expect("count")
}

#[tokio::test]
#[ignore]
async fn promoted_target_reproduces_the_authority_and_can_extend_it() {
    let (auth, mir, tgt) = (
        Scratch::new("auth").await,
        Scratch::new("mir").await,
        Scratch::new("tgt").await,
    );
    let net = format!("avalon-test-promo-{}", Uuid::new_v4());
    let authority = build_authority(&auth, &net).await;
    mirror_authority(&authority, &mir.pool).await;

    // Hot-tier pruning on the mirror: the hash and its links survive.
    sqlx::query("UPDATE mirrored_entries SET payload = NULL WHERE seq = (SELECT MIN(seq) FROM mirrored_entries)")
        .execute(&mir.pool)
        .await
        .unwrap();
    // A forged STH at a size the authority signed, with a wrong root.
    let real = authority.list_signed_tree_heads().await.unwrap();
    let mut forged = ObservedSth::from_sth(
        "http://forger.invalid",
        CORE_SHARD_ID,
        &real[0],
        OffsetDateTime::now_utc(),
    );
    forged.root_hash = "ee".repeat(32);
    mirror::insert_observation(&mir.pool, &forged)
        .await
        .unwrap();

    let report = promote_mirror(&mir.pool, &tgt.pool, &params(&net, false))
        .await
        .expect("promotion");
    assert_eq!(report.entries, 6);
    assert_eq!(report.batches, 3);
    assert_eq!(report.pruned_entries, 1);
    assert_eq!(report.sths_carried, 3);
    assert_eq!(report.sths_missing, 0);
    assert!(report.genesis_written);

    let promoted = PostgresSettlementProvider::connect(tgt.pool.clone(), &net)
        .await
        .expect("genesis matches");

    let original = authority.list_entries().await.unwrap();
    let copied = promoted.list_entries().await.unwrap();
    assert_eq!(original.len(), copied.len());
    for (a, b) in original.iter().zip(&copied) {
        assert_eq!(
            (a.seq, &a.event_id, &a.prev_hash, &a.entry_hash, a.batch_id),
            (b.seq, &b.event_id, &b.prev_hash, &b.entry_hash, b.batch_id)
        );
        assert!(b.chain_intact);
    }
    assert!(original.windows(2).any(|w| w[1].seq - w[0].seq > 1));
    assert!(copied[0].payload.is_none() && copied[0].payload_pruned);
    assert!(copied[1].payload.is_some() && !copied[1].payload_pruned);

    let original_batches = authority.list_batches().await.unwrap();
    let copied_batches = promoted.list_batches().await.unwrap();
    assert_eq!(original_batches.len(), copied_batches.len());
    for (a, b) in original_batches.iter().zip(&copied_batches) {
        assert_eq!(
            (a.batch_id, a.first_seq, a.last_seq, &a.batch_root),
            (b.batch_id, b.first_seq, b.last_seq, &b.batch_root)
        );
    }

    let copied_sths = promoted.list_signed_tree_heads().await.unwrap();
    assert_eq!(real, copied_sths);

    // The promoted authority continues the chain and tree.
    let old_size = promoted.entry_count().await.unwrap();
    let old_root = promoted.root_at(old_size).await.unwrap().unwrap();
    promoted
        .commit(&batch(2))
        .await
        .expect("commit on promoted");
    let after = promoted.list_entries().await.unwrap();
    assert_eq!(after.len() as i64, old_size + 2);
    assert_eq!(after[6].seq, report.highest_seq + 1);
    assert!(after.iter().all(|e| e.chain_intact));
    let new_size = promoted.entry_count().await.unwrap();
    let new_root = promoted.root_at(new_size).await.unwrap().unwrap();
    let proof = promoted
        .consistency_proof(old_size, new_size)
        .await
        .unwrap();
    assert!(merkle::verify_consistency_proof(
        old_size as usize,
        new_size as usize,
        &proof,
        &old_root,
        &new_root
    ));
    let latest = promoted.latest_signed_tree_head().await.unwrap().unwrap();
    assert_eq!(latest.tree_size, new_size);
    assert_eq!(latest.root_hash, hex::encode(new_root));

    for s in [auth, mir, tgt] {
        s.drop().await;
    }
}

#[tokio::test]
#[ignore]
async fn dry_run_checks_everything_and_writes_nothing() {
    let (auth, mir, tgt) = (
        Scratch::new("auth").await,
        Scratch::new("mir").await,
        Scratch::new("tgt").await,
    );
    let net = format!("avalon-test-promo-{}", Uuid::new_v4());
    let authority = build_authority(&auth, &net).await;
    mirror_authority(&authority, &mir.pool).await;

    let report = promote_mirror(&mir.pool, &tgt.pool, &params(&net, true))
        .await
        .expect("dry run");
    assert!(report.dry_run);
    assert_eq!(report.entries, 6);
    for table in [
        "ledger_entries",
        "ledger_batches",
        "signed_tree_heads",
        "chain_genesis",
    ] {
        assert_eq!(count(&tgt.pool, table).await, 0, "{table} must stay empty");
    }

    for s in [auth, mir, tgt] {
        s.drop().await;
    }
}

#[tokio::test]
#[ignore]
async fn refuses_a_mirror_that_is_not_converged() {
    let (auth, mir, tgt) = (
        Scratch::new("auth").await,
        Scratch::new("mir").await,
        Scratch::new("tgt").await,
    );
    let net = format!("avalon-test-promo-{}", Uuid::new_v4());
    let authority = build_authority(&auth, &net).await;
    mirror_authority(&authority, &mir.pool).await;
    sqlx::query("DELETE FROM mirrored_entries WHERE seq = (SELECT MAX(seq) FROM mirrored_entries)")
        .execute(&mir.pool)
        .await
        .unwrap();

    let err = promote_mirror(&mir.pool, &tgt.pool, &params(&net, false))
        .await
        .expect_err("must refuse");
    assert!(matches!(err, PromotionError::NotConverged { .. }), "{err}");
    assert_eq!(count(&tgt.pool, "ledger_entries").await, 0);

    for s in [auth, mir, tgt] {
        s.drop().await;
    }
}

#[tokio::test]
#[ignore]
async fn refuses_a_target_that_already_has_a_ledger() {
    let (auth, mir, tgt) = (
        Scratch::new("auth").await,
        Scratch::new("mir").await,
        Scratch::new("tgt").await,
    );
    let net = format!("avalon-test-promo-{}", Uuid::new_v4());
    let authority = build_authority(&auth, &net).await;
    mirror_authority(&authority, &mir.pool).await;

    promote_mirror(&mir.pool, &tgt.pool, &params(&net, false))
        .await
        .expect("first promotion");
    let err = promote_mirror(&mir.pool, &tgt.pool, &params(&net, false))
        .await
        .expect_err("second promotion must refuse");
    assert!(matches!(err, PromotionError::TargetNotFresh(_)), "{err}");

    for s in [auth, mir, tgt] {
        s.drop().await;
    }
}

#[tokio::test]
#[ignore]
async fn refuses_a_target_bound_to_another_network_and_accepts_the_same_one() {
    let (auth, mir, tgt) = (
        Scratch::new("auth").await,
        Scratch::new("mir").await,
        Scratch::new("tgt").await,
    );
    let net = format!("avalon-test-promo-{}", Uuid::new_v4());
    let authority = build_authority(&auth, &net).await;
    mirror_authority(&authority, &mir.pool).await;

    sqlx::query("INSERT INTO chain_genesis (network_id) VALUES ('some-other-network')")
        .execute(&tgt.pool)
        .await
        .unwrap();
    let err = promote_mirror(&mir.pool, &tgt.pool, &params(&net, false))
        .await
        .expect_err("must refuse");
    assert!(
        matches!(err, PromotionError::GenesisMismatch { .. }),
        "{err}"
    );
    assert_eq!(count(&tgt.pool, "ledger_entries").await, 0);

    sqlx::query("UPDATE chain_genesis SET network_id = $1")
        .bind(&net)
        .execute(&tgt.pool)
        .await
        .unwrap();
    let report = promote_mirror(&mir.pool, &tgt.pool, &params(&net, false))
        .await
        .expect("matching genesis is accepted");
    assert!(!report.genesis_written);
    let stored: String = sqlx::query("SELECT network_id FROM chain_genesis")
        .fetch_one(&tgt.pool)
        .await
        .unwrap()
        .get("network_id");
    assert_eq!(stored, net);

    for s in [auth, mir, tgt] {
        s.drop().await;
    }
}

#[tokio::test]
#[ignore]
async fn refuses_to_promote_a_database_into_itself() {
    let mir = Scratch::new("mir").await;
    let err = promote_mirror(
        &mir.pool,
        &mir.pool,
        &params("avalon-test-promo-self", false),
    )
    .await
    .expect_err("must refuse");
    assert!(matches!(err, PromotionError::SameDatabase(_)), "{err}");
    mir.drop().await;
}
